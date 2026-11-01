//! Native c9p bridge. The webview never receives a socket or a universal pipe.
//! Three independent request connections avoid serializing history scans with
//! small reads. Stream connections are separate from that pool.
use crate::engine_proto::{
    compatibility::{core_action, core_usable, CoreAction, GUI_METHODS},
    home::{engine_home, sock_path},
    spawn::{ensure_engine, restart_engine, ConnectError, EngineConn, Frames},
    wire::{AgentListResult, EventFrame, NotificationEvent, SubscribeParams},
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{ipc::Channel, AppHandle, Emitter, Manager, WebviewWindow};

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct BridgeError(pub Value);
impl BridgeError {
    fn new(code: &str, msg: impl Into<String>) -> Self {
        Self(json!({ "code": code, "msg": msg.into(), "retryable": false, "hint": "" }))
    }
    pub fn msg(&self) -> String {
        self.0["msg"]
            .as_str()
            .unwrap_or("Core request failed")
            .into()
    }
}

// The GUI connection is stamped local-unverified by the Unix transport.
// Consult the generated authority declarations even in GUI-only builds.
fn gui_authorized(section: &str, name: &str) -> bool {
    static SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    let schema = SCHEMA.get_or_init(|| {
        serde_json::from_str(include_str!("../../docs/api/c9p-1.json"))
            .expect("committed c9p schema")
    });
    schema[section][name]["x-authz"]["principals"]
        .as_array()
        .is_some_and(|principals| principals.iter().any(|p| p == "local-unverified"))
}

pub fn allowed(method: &str, popover: bool) -> bool {
    GUI_METHODS.contains(&method)
        && gui_authorized("methods", method)
        && !matches!(method, "events.subscribe" | "events.unsubscribe")
        && (!popover
            || !matches!(
                method,
                "session.stop" | "session.rename" | "detect.recheck" | "provider.opencode.connect"
            ))
}

struct Pool {
    conns: [Mutex<Option<EngineConn>>; 3],
    free: Mutex<Vec<usize>>,
    available: Condvar,
}
impl Default for Pool {
    fn default() -> Self {
        Self {
            conns: std::array::from_fn(|_| Mutex::new(None)),
            free: Mutex::new(vec![0, 1, 2]),
            available: Condvar::new(),
        }
    }
}
struct PoolSlot<'a> {
    pool: &'a Pool,
    index: usize,
}
impl Drop for PoolSlot<'_> {
    fn drop(&mut self) {
        self.pool.free.lock().unwrap().push(self.index);
        self.pool.available.notify_one();
    }
}
impl Pool {
    fn reserve(&self) -> PoolSlot<'_> {
        let mut free = self.free.lock().unwrap();
        while free.is_empty() {
            free = self.available.wait(free).unwrap();
        }
        PoolSlot {
            pool: self,
            index: free.pop().unwrap(),
        }
    }
    fn clear(&self) {
        for conn in &self.conns {
            if let Ok(mut conn) = conn.try_lock() {
                *conn = None;
            }
        }
    }
}
struct Stream {
    owner: String,
    channel: Channel<Value>,
    params: SubscribeParams,
    ready: Option<tokio::sync::oneshot::Sender<Result<(), BridgeError>>>,
}

pub struct Bridge {
    home: PathBuf,
    active: AtomicBool,
    last_health: AtomicI64,
    pool: Pool,
    streams: Mutex<HashMap<String, Stream>>,
    stream_worker_started: AtomicBool,
    stream_wake: Condvar,
    // Keep the connection alive: gateway.open is a lease, not a persistent setting.
    pub(crate) lease: Mutex<Option<crate::gateway_control::GatewayLease>>,
    pub snapshot: Mutex<Value>,
}
impl Default for Bridge {
    fn default() -> Self {
        Self::at_home(engine_home().unwrap_or_default())
    }
}
impl Bridge {
    pub fn at_home(home: PathBuf) -> Self {
        Self {
            home,
            active: AtomicBool::new(false),
            last_health: AtomicI64::new(0),
            pool: Pool::default(),
            streams: Mutex::new(HashMap::new()),
            stream_worker_started: AtomicBool::new(false),
            stream_wake: Condvar::new(),
            lease: Mutex::new(None),
            snapshot: Mutex::new(Value::Null),
        }
    }
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
            && chrono::Utc::now()
                .timestamp_millis()
                .saturating_sub(self.last_health.load(Ordering::Acquire))
                < 15_000
    }
    fn healthy(&self) {
        self.last_health
            .store(chrono::Utc::now().timestamp_millis(), Ordering::Release);
    }
    pub fn handed_over(&self) -> bool {
        self.lease.lock().unwrap().is_some()
    }
    pub fn call(
        &self,
        method: &str,
        params: Value,
        app: Option<&AppHandle>,
    ) -> Result<Value, BridgeError> {
        let session = (method == "conversation.get").then(|| {
            let key = params["sessionKey"].as_str().unwrap_or("");
            let id = key.split_once(':').map_or(key, |(_, id)| id).to_string();
            (id, params["provider"].clone())
        });
        self.call_with_progress(method, params, &mut |frame| {
            if let (Some(app), Some((id, provider))) = (app, session.as_ref()) {
                let _ = app.emit("conversation-progress", json!({ "sessionId": id, "provider": provider, "bytesRead": frame["bytesRead"], "bytesTotal": frame["bytesTotal"] }));
            }
        })
    }
    /// Progress frames belong to this request only. They go to `progress`,
    /// never to the webview event streams, which did not ask for them.
    fn call_with_progress(
        &self,
        method: &str,
        params: Value,
        progress: &mut dyn FnMut(Value),
    ) -> Result<Value, BridgeError> {
        if !self.active() {
            return Err(BridgeError::new(
                "core_unavailable",
                "The GUI is using its in-process fallback",
            ));
        }
        let slot = self.pool.reserve();
        let mut cached = self.pool.conns[slot.index].lock().unwrap();
        if cached.is_none() {
            let mut conn = connect(&self.home).map_err(|e| BridgeError::new("transport", e))?;
            if !core_usable(&conn.hello) {
                return Err(BridgeError::new(
                    "transport",
                    "Core changed; waiting for compatibility check",
                ));
            }
            // Long transcript/history requests must not poison unrelated connections.
            conn.set_read_timeout(Duration::from_secs(120));
            *cached = Some(conn);
        }
        let result = cached
            .as_mut()
            .unwrap()
            .call_raw_progress(method, params, None, progress);
        match result {
            Ok(mut response) if response["ok"] == true => Ok(response["result"].take()),
            Ok(mut response) => Err(BridgeError(response["error"].take())),
            Err(e) => {
                *cached = None;
                Err(BridgeError::new("transport", e))
            }
        }
    }
    /// Native-only path; no bridge allowlist entry for gateway writes.
    pub fn gateway_call(&self, method: &str, params: Value) -> Result<Value, String> {
        let mut lease = self.lease.lock().unwrap();
        let conn = lease
            .as_mut()
            .ok_or("Core gateway is not leased by this GUI")?;
        conn.read(method, params)
    }
    pub fn gateway_write(
        &self,
        app: &AppHandle,
        method: &str,
        params: Value,
        previous: &Value,
    ) -> Result<Value, String> {
        let mut lease = self.lease.lock().unwrap();
        lease
            .as_mut()
            .ok_or("Core gateway is not leased by this GUI")?
            .write(app, method, params, previous)
    }
}
fn connect(home: &std::path::Path) -> Result<EngineConn, String> {
    EngineConn::connect_kind(&sock_path(home), "gui", true).map_err(ConnectError::into_msg)
}
fn result(conn: &mut EngineConn, method: &str, params: Value) -> Result<Value, String> {
    let response = conn.call(method, params)?;
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        Err(crate::engine_proto::spawn::error_line(&response))
    }
}

#[tauri::command]
pub async fn c9p_request(
    window: WebviewWindow,
    app: AppHandle,
    bridge: tauri::State<'_, Arc<Bridge>>,
    method: String,
    params: Value,
) -> Result<Value, BridgeError> {
    if !allowed(&method, window.label() == "popover")
        || !matches!(window.label(), "main" | "popover")
    {
        return Err(BridgeError::new(
            "not_allowed",
            "Method is not exposed to this window",
        ));
    }
    let bridge = bridge.inner().clone();
    tauri::async_runtime::spawn_blocking(move || bridge.call(&method, params, Some(&app)))
        .await
        .map_err(|e| BridgeError::new("transport", e.to_string()))?
}

fn allowed_topics(params: &SubscribeParams, popover: bool) -> bool {
    !params.topics.is_empty()
        && gui_authorized("methods", "events.subscribe")
        && params.topics.iter().all(|t| {
            let topics: &[&str] = match t.as_str() {
                "session.*" => &["session.view", "session.gone"],
                "agent.*" if !popover => &[
                    "agent.upsert",
                    "agent.attention",
                    "agent.turn_end",
                    "agent.gone",
                    "agent.failed",
                ],
                "session.view" | "session.gone" | "diag.detection" | "annotation.changed"
                | "annotation.removed" => &[t.as_str()],
                "notification.needs_attention" | "notification.waiting_for_input" if !popover => {
                    &[t.as_str()]
                }
                _ => return false,
            };
            topics.iter().all(|topic| gui_authorized("topics", topic))
        })
}
#[tauri::command]
pub async fn c9p_subscribe(
    window: WebviewWindow,
    bridge: tauri::State<'_, Arc<Bridge>>,
    params: SubscribeParams,
    channel: Channel<Value>,
) -> Result<String, BridgeError> {
    if !matches!(window.label(), "main" | "popover")
        || !allowed_topics(&params, window.label() == "popover")
    {
        return Err(BridgeError::new(
            "not_allowed",
            "Topics are not exposed to this window",
        ));
    }
    if !bridge.active() {
        return Err(BridgeError::new("core_unavailable", "Core is not usable"));
    }
    if serde_json::to_vec(&params).map_or(true, |p| p.len() > 64 * 1024) {
        return Err(BridgeError::new(
            "bad_request",
            "Subscription params exceed 64 KiB",
        ));
    }
    let sub = uuid::Uuid::new_v4().to_string();
    let (ready, result) = tokio::sync::oneshot::channel();
    {
        let mut streams = bridge.streams.lock().unwrap();
        if streams.len() >= 8 {
            return Err(BridgeError::new(
                "rate_limited",
                "Too many GUI subscriptions",
            ));
        }
        streams.insert(
            sub.clone(),
            Stream {
                owner: window.label().into(),
                channel,
                params,
                ready: Some(ready),
            },
        );
    }
    bridge.stream_wake.notify_one();
    if !bridge.stream_worker_started.swap(true, Ordering::AcqRel) {
        let bridge = bridge.inner().clone();
        std::thread::spawn(move || stream_worker(bridge));
    }
    match tokio::time::timeout(Duration::from_secs(10), result).await {
        Ok(Ok(Ok(()))) => Ok(sub),
        result => {
            bridge.streams.lock().unwrap().remove(&sub);
            bridge.stream_wake.notify_one();
            match result {
                Ok(Ok(Err(error))) => Err(error),
                _ => Err(BridgeError::new(
                    "transport",
                    "Core subscription did not become ready",
                )),
            }
        }
    }
}

/// One dedicated connection multiplexes every webview stream. Subscriptions
/// keep their own cursor; response/event correlation never depends on order.
fn stream_worker(bridge: Arc<Bridge>) {
    run_stream_worker(bridge, || false);
}
fn run_stream_worker(bridge: Arc<Bridge>, stopped: impl Fn() -> bool) {
    let mut frames: Option<Frames> = None;
    let mut active: HashSet<String> = HashSet::new();
    loop {
        if stopped() {
            return;
        }
        let desired: HashMap<String, Value> = bridge
            .streams
            .lock()
            .unwrap()
            .iter()
            .map(|(id, stream)| (id.clone(), serde_json::to_value(&stream.params).unwrap()))
            .collect();
        if desired.is_empty() {
            frames = None;
            active.clear();
            let mut streams = bridge.streams.lock().unwrap();
            while streams.is_empty() {
                if stopped() {
                    return;
                }
                streams = bridge.stream_wake.wait(streams).unwrap();
            }
            continue;
        }
        if frames.is_none() && !desired.is_empty() {
            frames = connect(&bridge.home).and_then(EngineConn::into_frames).ok();
            if frames.is_none() {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        }
        let mut disconnected = false;
        if let Some(socket) = frames.as_ref() {
            for id in active
                .iter()
                .filter(|id| !desired.contains_key(*id))
                .cloned()
                .collect::<Vec<_>>()
            {
                let request_id = uuid::Uuid::new_v4().to_string();
                if socket
                    .request(&request_id, "events.unsubscribe", json!({ "sub": id }))
                    .is_err()
                {
                    disconnected = true;
                    break;
                }
                active.remove(&id);
            }
            if !disconnected {
                for (id, params) in &desired {
                    if !active.contains(id) {
                        if socket
                            .request(id, "events.subscribe", params.clone())
                            .is_err()
                        {
                            disconnected = true;
                            break;
                        }
                        active.insert(id.clone());
                    }
                }
            }
            let batch = if disconnected {
                Ok(Vec::new())
            } else {
                recv_batch(&socket.lines, STREAM_POLL, STREAM_BATCH)
            };
            match batch {
                Err(()) => disconnected = true,
                Ok(batch) => {
                    for line in batch {
                        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                            disconnected = true;
                            break;
                        };
                        let id = frame["sub"]
                            .as_str()
                            .or_else(|| frame["id"].as_str())
                            .unwrap_or("");
                        let mut streams = bridge.streams.lock().unwrap();
                        if let Some(stream) = streams.get_mut(id) {
                            if let Some(cursor) = frame["cursor"]
                                .as_str()
                                .or_else(|| frame["result"]["cursor"].as_str())
                            {
                                stream.params.since = Some(cursor.into());
                            }
                            let response = frame["ok"].is_boolean();
                            let initial = if response { stream.ready.take() } else { None };
                            let mut failed = false;
                            if let Some(ready) = initial {
                                let result = if frame["ok"] == true {
                                    Ok(())
                                } else {
                                    Err(BridgeError(frame["error"].clone()))
                                };
                                failed = ready.send(result).is_err();
                            } else if !response || frame["ok"] == false {
                                failed = stream.channel.send(frame.clone()).is_err();
                            }
                            if frame["ok"] == false || failed {
                                streams.remove(id);
                            }
                        }
                    }
                }
            }
        }
        if disconnected {
            frames = None;
            active.clear();
            // Back off before reconnecting so a broken core is not a busy loop.
            std::thread::sleep(STREAM_POLL);
        }
    }
}
/// The longest the multiplexer waits for a frame before it re-reads the
/// subscription set; a new or removed subscription is seen within this time.
const STREAM_POLL: Duration = Duration::from_millis(50);
/// The most frames handled before the subscription set is read again.
const STREAM_BATCH: usize = 128;
/// Block until the first frame arrives (or `wait` ends), then take the frames
/// already queued, up to `cap`. `Err` means the reader thread ended.
fn recv_batch(
    lines: &std::sync::mpsc::Receiver<String>,
    wait: Duration,
    cap: usize,
) -> Result<Vec<String>, ()> {
    use std::sync::mpsc::RecvTimeoutError;
    let first = match lines.recv_timeout(wait) {
        Ok(line) => line,
        Err(RecvTimeoutError::Timeout) => return Ok(Vec::new()),
        Err(RecvTimeoutError::Disconnected) => return Err(()),
    };
    let mut batch = vec![first];
    // A disconnect after queued frames is reported by the next call.
    batch.extend(lines.try_iter().take(cap.saturating_sub(1)));
    Ok(batch)
}

#[tauri::command]
pub fn c9p_unsubscribe(
    window: WebviewWindow,
    bridge: tauri::State<'_, Arc<Bridge>>,
    sub: String,
) -> Result<(), BridgeError> {
    let mut streams = bridge.streams.lock().unwrap();
    if let Some(stream) = streams.get(&sub) {
        if stream.owner != window.label() {
            return Err(BridgeError::new(
                "not_allowed",
                "Subscription belongs to another window",
            ));
        }
    }
    streams.remove(&sub);
    bridge.stream_wake.notify_one();
    Ok(())
}

pub fn core_active(app: &AppHandle) -> bool {
    app.try_state::<Arc<Bridge>>().is_some_and(|b| b.active())
}

fn controlled_agents(conn: &mut EngineConn) -> Option<bool> {
    let value = result(conn, "agent.list", json!({})).ok()?;
    let rows: AgentListResult = serde_json::from_value(value).ok()?;
    Some(rows.agents.iter().any(|a| {
        a.control != "none"
            && a.gone_at.is_none()
            && !matches!(a.state.as_deref(), Some("retired" | "failed" | "gone"))
    }))
}

/// The GUI opens immediately; all engine IO and version decisions are off
/// the event loop. Core is never stopped when the GUI exits.
pub fn start(app: AppHandle, bridge: Arc<Bridge>, server: Arc<crate::web_server::WsServer>) {
    std::thread::spawn(move || {
        let home = bridge.home.clone();
        if home.as_os_str().is_empty() {
            bind_gui(&server);
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            bind_gui(&server);
            return;
        };
        let mut restarted = false;
        let mut bound = false;
        let mut recovering = None;
        let mut snapshot: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut wait_agents = false;
            let attempt = (|| -> Result<(), String> {
                // A GUI-only dev binary has no engine CLI dispatch. Spawning
                // it with `engine` would launch another GUI, so it may only
                // connect to an existing core and otherwise uses fallback.
                let hello = if cfg!(feature = "engine") {
                    ensure_engine(&home, &exe)?
                } else {
                    connect(&home)?.hello
                };
                let mut conn = connect(&home)?;
                let controlled = if core_usable(&hello) {
                    None
                } else {
                    controlled_agents(&mut conn)
                };
                match core_action(&hello, env!("CARGO_PKG_VERSION"), controlled) {
                    CoreAction::Use => {}
                    CoreAction::Restart if !restarted && cfg!(feature = "engine") => {
                        bridge.active.store(false, Ordering::Release);
                        restarted = true;
                        restart_engine(&home, &exe)?;
                        conn = connect(&home)?;
                        if !core_usable(&conn.hello) {
                            return Err("Core still incompatible after restart".into());
                        }
                        cursor = None;
                    }
                    _ => {
                        bridge.active.store(false, Ordering::Release);
                        wait_agents = controlled == Some(true);
                        return Err("Core incompatible; keeping in-process fallback".into());
                    }
                }
                bridge.pool.clear();
                // Snapshot + cursor and resume have no gap. Replace on boot or
                // expired cursor; never discard the last snapshot on a disconnect.
                let stream = loop {
                    if cursor.is_none() {
                        let view = result(&mut conn, "session.views", json!({}))?;
                        snapshot = view["sessions"]
                            .as_array()
                            .ok_or("Bad session.views")?
                            .clone();
                        cursor = view["cursor"].as_str().map(str::to_string);
                    }
                    match subscribe(
                        &home,
                        json!({ "topics": ["session.*", "diag.detection", "agent.*"], "since": cursor }),
                    ) {
                        Ok(stream) => break stream,
                        Err(e) if e.starts_with("cursor_expired") => {
                            cursor = None;
                        }
                        Err(e) => return Err(e),
                    }
                };
                // Live-only: this subscription NEVER contains a since cursor.
                let notification_start = chrono::Utc::now().timestamp_millis();
                let notifications = subscribe(
                    &home,
                    json!({ "topics": ["notification.needs_attention", "notification.waiting_for_input"] }),
                )?;
                if !bound && !bridge.handed_over() {
                    if conn.hello.methods.contains_key("gateway.open") {
                        if let Ok(mut lease) =
                            crate::gateway_control::GatewayLease::attach(connect(&home)?)
                        {
                            if lease.read("gateway.open", json!({ "lease": true })).is_ok()
                                && connect(&home)?
                                    .hello
                                    .capabilities
                                    .iter()
                                    .any(|c| c == "gateway")
                            {
                                *bridge.lease.lock().unwrap() = Some(lease);
                            }
                        }
                    }
                    if !bridge.handed_over() {
                        bind_gui(&server);
                        bound = true;
                    }
                }
                bridge.healthy();
                bridge.active.store(true, Ordering::Release);
                recovering = None;
                publish(&app, &bridge, &server, &snapshot);
                // Diagnostics are not an outbox topic on older cores.
                if let Ok(diag) = result(&mut conn, "diag.detection", json!({})) {
                    let _ = app.emit("diagnostic-update", diag);
                }
                let mut diag_at = Instant::now();
                loop {
                    let mut batch = Vec::new();
                    match stream.lines.recv_timeout(Duration::from_millis(250)) {
                        Ok(line) => {
                            batch.push(line);
                            batch.extend(stream.lines.try_iter().take(127));
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => return Err("Core stream disconnected".into()),
                    }
                    let mut changed = false;
                    for line in batch {
                        let frame: Value =
                            serde_json::from_str(&line).map_err(|e| e.to_string())?;
                        if frame["error"]["code"] == "cursor_expired" {
                            cursor = None;
                            return Err("cursor_expired".into());
                        }
                        if let Ok(event) = serde_json::from_value::<EventFrame>(frame) {
                            changed |= apply_session_event(&mut snapshot, &event);
                            cursor = Some(event.cursor);
                        }
                    }
                    if changed {
                        publish(&app, &bridge, &server, &snapshot);
                    }
                    for line in notifications.lines.try_iter() {
                        if let Ok(event) = serde_json::from_str::<EventFrame>(&line) {
                            if event.ts >= notification_start {
                                if let Ok(payload) =
                                    serde_json::from_value::<NotificationEvent>(event.data)
                                {
                                    crate::notifications::notify_core(&app, &payload);
                                    let _ = server.state().notifications_tx.send(json!({ "title": payload.title, "redactedTitle": payload.redacted_title, "body": payload.body, "sessionId": payload.session_key.split_once(':').map(|(_, id)| id), "sessionKey": payload.session_key, "provider": payload.provider, "pid": payload.pid }).to_string());
                                }
                            }
                        }
                    }
                    if diag_at.elapsed() >= Duration::from_secs(4) {
                        let diag = result(&mut conn, "diag.detection", json!({}))?;
                        bridge.healthy();
                        let _ = app.emit("diagnostic-update", diag);
                        diag_at = Instant::now();
                    }
                }
            })();
            if let Err(e) = attempt {
                crate::debug_log::log_info(&format!("[core bridge] {e}"));
            }
            // A disconnect gets 15 s to reconnect while retaining the last data.
            // Incompatibility/spawn failure starts in fallback immediately.
            let since = recovering.get_or_insert_with(Instant::now);
            if !bridge.active() || since.elapsed() >= Duration::from_secs(15) {
                bridge.active.store(false, Ordering::Release);
                if !bound {
                    bridge.lease.lock().unwrap().take();
                    bind_gui(&server);
                    bound = true;
                }
            }
            if wait_agents {
                if let Ok(events) = subscribe(&home, json!({ "topics": ["agent.*"] })) {
                    let _ = events.lines.recv_timeout(Duration::from_secs(60));
                    continue;
                }
            }
            std::thread::sleep(if bridge.active() {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(60)
            });
        }
    });
}
fn subscribe(home: &std::path::Path, params: Value) -> Result<Frames, String> {
    let mut conn = connect(home)?;
    result(&mut conn, "events.subscribe", params)?;
    conn.into_frames()
}
fn bind_gui(server: &Arc<crate::web_server::WsServer>) {
    let server = server.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = server.apply(server.remote()).await {
            crate::debug_log::log_error(&format!("[ws-server] {e}"));
        }
    });
}
fn publish(
    app: &AppHandle,
    bridge: &Bridge,
    server: &crate::web_server::WsServer,
    snapshot: &[Value],
) {
    let data = json!(snapshot);
    *bridge.snapshot.lock().unwrap() = data.clone();
    let _ = app.emit("sessions-updated", &data);
    let _ = server.state().sessions_tx.send(data.to_string());
}
pub fn apply_session_event(snapshot: &mut Vec<Value>, event: &EventFrame) -> bool {
    let key = event.data["sessionKey"].as_str();
    if key.is_none() {
        return false;
    }
    match event.topic.as_str() {
        "session.view" => {
            if let Some(existing) = snapshot
                .iter_mut()
                .find(|s| s["sessionKey"].as_str() == key)
            {
                *existing = event.data.clone();
            } else {
                snapshot.push(event.data.clone());
            }
            true
        }
        "session.gone" => {
            snapshot.retain(|s| s["sessionKey"].as_str() != key);
            true
        }
        _ => false,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn webview_is_not_a_universal_pipe() {
        for m in [
            "agent.create",
            "agent.retire",
            "msg.send",
            "gateway.open",
            "gateway.policy.set",
            "gui.confirmer.attach",
            "hook.stop",
            "task.done",
            "made.up",
            "annotation.set",
            "annotation.delete",
            "annotation.patch",
        ] {
            assert!(!allowed(m, false), "{m}");
        }
        assert!(allowed("session.stop", false));
        assert!(!allowed("session.stop", true));
        assert!(!allowed("provider.opencode.connect", true));
        assert!(allowed("session.views", true));
        assert!(allowed("annotation.list", false));
        assert!(allowed("annotation.list", true));
    }
    #[cfg(feature = "engine")]
    #[test]
    fn webview_allowlist_is_a_subset_of_central_authority() {
        use crate::engine::{authz, principal::Principal};
        for method in GUI_METHODS {
            assert!(
                authz::can(method, &Principal::LocalUnverified, None).allowed,
                "{method}"
            );
            assert!(gui_authorized("methods", method), "{method}");
        }
        for method in crate::engine::discovery::methods().keys() {
            if allowed(method, false) {
                assert!(
                    authz::can(method, &Principal::LocalUnverified, None).allowed,
                    "{method}"
                );
            }
            if method.starts_with("msg.")
                || method.starts_with("mail.")
                || method.starts_with("gateway.")
                || method.starts_with("hook.")
                || matches!(
                    method.as_str(),
                    "agent.create" | "agent.retire" | "gui.confirmer.attach"
                )
            {
                assert!(!allowed(method, false), "{method}");
            }
        }
        for topic in ["annotation.changed", "annotation.removed"] {
            assert!(authz::topics(&Principal::LocalUnverified, None)
                .iter()
                .any(|t| t == topic));
            let params = SubscribeParams {
                topics: vec![topic.into()],
                ..Default::default()
            };
            assert!(allowed_topics(&params, false));
            assert!(allowed_topics(&params, true));
        }
    }
    #[test]
    fn popover_topics_are_read_only_and_no_wildcard_escape() {
        assert!(!allowed_topics(&SubscribeParams::default(), false));
        let mut p = SubscribeParams {
            topics: vec!["session.*".into()],
            ..Default::default()
        };
        assert!(allowed_topics(&p, true));
        p.topics.push("hook.*".into());
        assert!(!allowed_topics(&p, false));
    }
    #[test]
    fn session_delta_and_gone_keep_identity_and_order() {
        let mut snapshot = vec![
            json!({"sessionKey":"claudeCode:a", "status":"Working"}),
            json!({"sessionKey":"codex:a"}),
        ];
        let event = |topic: &str, data| {
            serde_json::from_value::<EventFrame>(
                json!({"v":1,"type":"event","cursor":"c","topic":topic,"ts":1,"data":data}),
            )
            .unwrap()
        };
        apply_session_event(
            &mut snapshot,
            &event(
                "session.view",
                json!({"sessionKey":"claudeCode:a","status":"WaitingForInput"}),
            ),
        );
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0]["status"], "WaitingForInput");
        apply_session_event(
            &mut snapshot,
            &event("session.gone", json!({"sessionKey":"claudeCode:a"})),
        );
        assert_eq!(snapshot, vec![json!({"sessionKey":"codex:a"})]);
    }
    #[test]
    fn a_slow_request_does_not_block_small_reads_and_errors_keep_fields() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let temp = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(sock_path(temp.path())).unwrap();
        let (started, wait_started) = std::sync::mpsc::channel();
        let release = Arc::new(std::sync::Barrier::new(2));
        let server_release = release.clone();
        let server = std::thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                let started = started.clone();
                let release = server_release.clone();
                workers.push(std::thread::spawn(move || {
                    let mut reader = BufReader::new(socket.try_clone().unwrap());
                    let mut line = String::new();
                    while { line.clear(); reader.read_line(&mut line).unwrap_or(0) > 0 } {
                        let req: Value = serde_json::from_str(&line).unwrap();
                        let reply = match req["method"].as_str().unwrap() {
                            "hello" => json!({"v":1,"id":req["id"],"ok":true,"result":{
                                "apiMajor":1,"apiMinor":6,"engineVersion":"0.11.1","machineId":"test","principal":"local-unverified","capabilities":[],"cursor":null,
                                "methods":GUI_METHODS.iter().map(|m| (m.to_string(), json!({"since":1,"rev":1,"stability":"preview"}))).collect::<serde_json::Map<String,Value>>()
                            }}),
                            "history.search" => {
                                started.send(()).unwrap(); release.wait();
                                json!({"v":1,"id":req["id"],"ok":true,"result":{"hits":[]}})
                            },
                            _ => json!({"v":1,"id":req["id"],"ok":false,"error":{"code":"future_denial","msg":"denied","retryable":true,"hint":"keep this hint","data":null,"extension":{"needs":"scope"}}}),
                        };
                        writeln!(socket,"{reply}").unwrap();
                    }
                }));
            }
            for w in workers {
                w.join().unwrap();
            }
        });
        let bridge = Arc::new(Bridge::at_home(temp.path().into()));
        bridge.healthy();
        bridge.active.store(true, Ordering::Release);
        let slow_bridge = bridge.clone();
        let slow = std::thread::spawn(move || {
            slow_bridge.call("history.search", json!({"query":"slow"}), None)
        });
        wait_started.recv_timeout(Duration::from_secs(3)).unwrap();
        let started = Instant::now();
        let error = bridge.call("session.views", json!({}), None).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(error.0["code"], "future_denial");
        assert_eq!(error.0["retryable"], true);
        assert_eq!(error.0["hint"], "keep this hint");
        assert!(error.0.as_object().unwrap().contains_key("data"));
        assert!(error.0["data"].is_null());
        assert_eq!(error.0["extension"]["needs"], "scope");
        release.wait();
        assert!(slow.join().unwrap().is_ok());
        drop(bridge);
        server.join().unwrap();
    }
    #[test]
    fn recv_batch_wakes_on_the_first_frame_and_caps_the_batch() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<String>(256);
        let sender = std::thread::spawn({
            let tx = tx.clone();
            move || {
                std::thread::sleep(Duration::from_millis(20));
                tx.send("first".into()).unwrap();
            }
        });
        let started = Instant::now();
        let batch = recv_batch(&rx, Duration::from_secs(5), 128).unwrap();
        assert_eq!(batch, vec!["first".to_string()]);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "woke on the frame, not on the timeout: {:?}",
            started.elapsed()
        );
        sender.join().unwrap();

        for i in 0..200 {
            tx.send(i.to_string()).unwrap();
        }
        assert_eq!(recv_batch(&rx, Duration::from_secs(5), 128).unwrap().len(), 128);
        assert_eq!(recv_batch(&rx, Duration::from_secs(5), 128).unwrap().len(), 72);
        let started = Instant::now();
        assert!(recv_batch(&rx, Duration::from_millis(30), 128).unwrap().is_empty());
        assert!(started.elapsed() >= Duration::from_millis(25));

        tx.send("last".into()).unwrap();
        drop(tx);
        assert_eq!(recv_batch(&rx, Duration::from_secs(5), 128).unwrap(), vec!["last".to_string()]);
        assert!(recv_batch(&rx, Duration::from_secs(5), 128).is_err());
    }
    #[test]
    fn progress_frames_go_to_the_caller_only_not_to_open_streams() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let temp = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(sock_path(temp.path())).unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            while { line.clear(); reader.read_line(&mut line).unwrap_or(0) > 0 } {
                let req: Value = serde_json::from_str(&line).unwrap();
                if req["method"] == "hello" {
                    writeln!(socket, "{}", json!({"v":1,"id":req["id"],"ok":true,"result":{
                        "apiMajor":1,"apiMinor":6,"engineVersion":"0.11.1","machineId":"test","principal":"local-unverified","capabilities":[],"cursor":null,
                        "methods":GUI_METHODS.iter().map(|m| (m.to_string(), json!({"since":1,"rev":1,"stability":"preview"}))).collect::<serde_json::Map<String,Value>>()
                    }})).unwrap();
                } else {
                    for read in [10, 20] {
                        writeln!(socket, "{}", json!({"v":1,"type":"progress","id":req["id"],"bytesRead":read,"bytesTotal":20})).unwrap();
                    }
                    writeln!(socket, "{}", json!({"v":1,"id":req["id"],"ok":true,"result":{"messages":[]}})).unwrap();
                }
            }
        });
        let bridge = Arc::new(Bridge::at_home(temp.path().into()));
        bridge.healthy();
        bridge.active.store(true, Ordering::Release);
        let stream_frames = Arc::new(Mutex::new(Vec::<String>::new()));
        let received = stream_frames.clone();
        let channel = Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(body) = body {
                received.lock().unwrap().push(body);
            }
            Ok(())
        });
        bridge.streams.lock().unwrap().insert(
            "popover-sessions".into(),
            Stream {
                owner: "popover".into(),
                channel,
                params: SubscribeParams {
                    topics: vec!["session.*".into()],
                    ..Default::default()
                },
                ready: None,
            },
        );
        let mut progress = Vec::new();
        let result = bridge
            .call_with_progress(
                "conversation.get",
                json!({"sessionKey":"claudeCode:a"}),
                &mut |frame| progress.push(frame["bytesRead"].as_i64().unwrap()),
            )
            .unwrap();
        assert_eq!(result, json!({"messages":[]}));
        assert_eq!(progress, vec![10, 20]);
        assert!(
            stream_frames.lock().unwrap().is_empty(),
            "a stream that did not make the request gets no progress frames"
        );
        drop(bridge);
        server.join().unwrap();
    }
    #[tokio::test]
    async fn stream_multiplexer_acks_and_rejects_on_one_connection_and_keeps_each_cursor() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let temp = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(sock_path(temp.path())).unwrap();
        let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = connections.clone();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            count.fetch_add(1, Ordering::Relaxed);
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            while {
                line.clear();
                reader.read_line(&mut line).unwrap_or(0) > 0
            } {
                let request: Value = serde_json::from_str(&line).unwrap();
                let id = &request["id"];
                if request["method"] == "hello" {
                    writeln!(socket,"{}",json!({"v":1,"id":id,"ok":true,"result":{"apiMajor":1,"apiMinor":6,"engineVersion":"0.11.1","machineId":"test","principal":"local-unverified","capabilities":[],"cursor":null}})).unwrap();
                } else if request["method"] == "events.subscribe"
                    && request["params"]["subjects"][0] == "expired"
                {
                    writeln!(socket,"{}",json!({"v":1,"id":id,"ok":false,"error":{"code":"cursor_expired","msg":"gone","retryable":false,"hint":"refresh"}})).unwrap();
                } else if request["method"] == "events.subscribe" {
                    writeln!(
                        socket,
                        "{}",
                        json!({"v":1,"id":id,"ok":true,"result":{"cursor":"m:e:0"}})
                    )
                    .unwrap();
                    writeln!(socket,"{}",json!({"v":1,"type":"event","sub":id,"cursor":"m:e:1","topic":"session.view","ts":1,"data":{"sessionKey":"claudeCode:a"}})).unwrap();
                }
            }
        });
        let bridge = Arc::new(Bridge::at_home(temp.path().into()));
        let messages = Arc::new(Mutex::new(Vec::<Value>::new()));
        let received = messages.clone();
        let channel = Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(body) = body {
                received
                    .lock()
                    .unwrap()
                    .push(serde_json::from_str(&body).unwrap());
            }
            Ok(())
        });
        let (a, ready_a) = tokio::sync::oneshot::channel();
        let (b, ready_b) = tokio::sync::oneshot::channel();
        {
            let mut streams = bridge.streams.lock().unwrap();
            streams.insert(
                "a".into(),
                Stream {
                    owner: "main".into(),
                    channel: channel.clone(),
                    params: SubscribeParams {
                        topics: vec!["session.view".into()],
                        ..Default::default()
                    },
                    ready: Some(a),
                },
            );
            streams.insert(
                "b".into(),
                Stream {
                    owner: "popover".into(),
                    channel,
                    params: SubscribeParams {
                        topics: vec!["session.view".into()],
                        subjects: vec!["expired".into()],
                        ..Default::default()
                    },
                    ready: Some(b),
                },
            );
        }
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let worker_bridge = bridge.clone();
        let worker = std::thread::spawn(move || {
            run_stream_worker(worker_bridge, || stop.load(Ordering::Acquire))
        });
        let a = tokio::time::timeout(Duration::from_secs(3), ready_a)
            .await
            .unwrap()
            .unwrap();
        let b = tokio::time::timeout(Duration::from_secs(3), ready_b)
            .await
            .unwrap()
            .unwrap();
        assert!(a.is_ok());
        assert_eq!(b.unwrap_err().0["code"], "cursor_expired");
        let deadline = Instant::now() + Duration::from_secs(3);
        while messages.lock().unwrap().is_empty() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let cursor = bridge
            .streams
            .lock()
            .unwrap()
            .get("a")
            .unwrap()
            .params
            .since
            .clone();
        bridge.streams.lock().unwrap().clear();
        stopped.store(true, Ordering::Release);
        bridge.stream_wake.notify_one();
        worker.join().unwrap();
        server.join().unwrap();
        assert_eq!(connections.load(Ordering::Relaxed), 1);
        assert_eq!(cursor.as_deref(), Some("m:e:1"));
        assert_eq!(
            messages.lock().unwrap().len(),
            1,
            "only raw event frames go to the channel; subscribe errors reject their call"
        );
    }
}
