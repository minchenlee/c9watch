use crate::remote_access::{self, RemoteAccess, Transport};
use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, Query, Request, State,
    },
    http::{header, StatusCode},
    middleware::Next,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot, watch};

/// Embed the SvelteKit build output into the binary
#[derive(Embed)]
#[folder = "../build/"]
struct Assets;

/// WebSocket server port
pub const WS_PORT: u16 = 9210;

/// Shared state for the WebSocket server
pub struct WsState {
    pub auth_token: String,
    pub sessions_tx: broadcast::Sender<String>,
    pub notifications_tx: broadcast::Sender<String>,
    /// The desktop app's detector, shared so stop resolves its target from
    /// the same detection the app shows.
    pub detector: Arc<Mutex<crate::session::DetectorState>>,
    /// The remote-access policy the listeners run under now.
    pub remote: RwLock<RemoteAccess>,
    /// Bumped on every policy change. Each open socket closes when it
    /// changes; the client reconnects under the new policy.
    pub epoch: watch::Sender<u64>,
}

impl WsState {
    pub fn new(
        auth_token: String,
        sessions_tx: broadcast::Sender<String>,
        notifications_tx: broadcast::Sender<String>,
        detector: Arc<Mutex<crate::session::DetectorState>>,
    ) -> Self {
        Self {
            auth_token,
            sessions_tx,
            notifications_tx,
            detector,
            remote: RwLock::new(RemoteAccess::default()),
            epoch: watch::channel(0).0,
        }
    }

    fn lan_enabled(&self) -> bool {
        // A poisoned lock fails closed: loopback only.
        self.remote.read().map(|r| r.lan_enabled).unwrap_or(false)
    }
}

// ── Protocol types ──────────────────────────────────────────────────

/// Client → Server messages
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ClientMsg {
    #[serde(rename = "getSessions")]
    GetSessions,

    #[serde(rename = "getSubscriptionUsage")]
    GetSubscriptionUsage,

    #[serde(rename = "getConversation")]
    GetConversation {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(default)]
        provider: Option<crate::session::SessionProvider>,
        #[serde(default, rename = "includeTools")]
        include_tools: bool,
    },

    /// Stop the session at `addr` (its session key, e.g. `claudeCode:<id>`).
    /// The server resolves the process itself; `pid`, when given, only has
    /// to agree with what it resolves.
    #[serde(rename = "session.stop")]
    SessionStop {
        addr: String,
        #[serde(default)]
        pid: Option<u32>,
    },

    /// Removed: it named a raw pid. Still parsed so an old client gets a
    /// clear error instead of "unknown message"; never stops anything.
    #[serde(rename = "stopSession")]
    StopSession {
        #[allow(dead_code)]
        pid: u32,
    },

    #[serde(rename = "openSession")]
    OpenSession {
        pid: u32,
        #[serde(rename = "projectPath")]
        project_path: String,
    },

    #[serde(rename = "renameSession")]
    RenameSession {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "newName")]
        new_name: String,
        #[serde(default)]
        provider: Option<crate::session::SessionProvider>,
    },

    #[serde(rename = "getAwayActivity")]
    GetAwayActivity {
        requests: Vec<crate::session::away::AwayRequest>,
    },
}

/// Server → Client messages
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ServerMsg {
    #[serde(rename = "subscriptionUsage")]
    SubscriptionUsage { data: serde_json::Value },
    #[serde(rename = "sessions")]
    Sessions { data: serde_json::Value },

    #[serde(rename = "conversation")]
    Conversation { data: serde_json::Value },

    #[serde(rename = "sessionsUpdated")]
    SessionsUpdated { data: serde_json::Value },

    #[serde(rename = "error")]
    Error { message: String },

    #[serde(rename = "ok")]
    Ok,

    #[serde(rename = "notification")]
    Notification { data: serde_json::Value },

    #[serde(rename = "conversationProgress")]
    ConversationProgress { data: serde_json::Value },

    #[serde(rename = "awayActivity")]
    AwayActivity { data: serde_json::Value },
}

// ── Server entrypoint ───────────────────────────────────────────────

fn router(state: Arc<WsState>) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health))
        .route("/info", get(info))
        .fallback(get(serve_static_fallback))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            peer_guard,
        ))
        .with_state(state)
}

/// Serve one bound listener until `shutdown` resolves. Taking a bound
/// listener (not an address) lets tests bind port 0.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<WsState>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) {
    let app = router(state).into_make_service_with_connect_info::<SocketAddr>();
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
    {
        crate::debug_log::log_error(&format!("[ws-server] Error: {}", e));
    }
}

/// A peer may connect when it is loopback, or when LAN access is on.
fn peer_allowed(lan_enabled: bool, peer: IpAddr) -> bool {
    lan_enabled || remote_access::is_loopback_peer(peer)
}

/// Rejects non-loopback peers while LAN access is off. The listeners are
/// loopback-only then anyway; this also covers a keep-alive connection that
/// a LAN listener accepted before LAN was turned off.
async fn peer_guard(
    State(state): State<Arc<WsState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    if peer_allowed(state.lan_enabled(), peer.ip()) {
        next.run(request).await
    } else {
        (StatusCode::FORBIDDEN, "Remote access is off").into_response()
    }
}

/// How long a stopping listener may take to finish its connections before
/// its task is aborted.
const STOP_GRACE: Duration = Duration::from_secs(2);

struct Listening {
    addr: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

/// Owns the WS listeners and swaps them when the remote-access policy
/// changes.
pub struct WsServer {
    state: Arc<WsState>,
    port: u16,
    config_path: PathBuf,
    listening: tokio::sync::Mutex<Vec<Listening>>,
}

impl WsServer {
    pub fn new(state: Arc<WsState>, port: u16, config_path: PathBuf) -> Self {
        Self {
            state,
            port,
            config_path,
            listening: tokio::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn state(&self) -> &Arc<WsState> {
        &self.state
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn config_path(&self) -> &std::path::Path {
        &self.config_path
    }

    /// The current policy.
    pub fn remote(&self) -> RemoteAccess {
        self.state
            .remote
            .read()
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// The addresses bound now.
    pub async fn bound(&self) -> Vec<SocketAddr> {
        self.listening.lock().await.iter().map(|l| l.addr).collect()
    }

    /// Run under `remote`: publish the policy, close every open socket, stop
    /// the old listeners, then bind the new ones. Returns the bound
    /// addresses. Fails when the first address of the plan cannot be bound;
    /// nothing is listening then.
    pub async fn apply(&self, remote: RemoteAccess) -> Result<Vec<SocketAddr>, String> {
        let mut listening = self.listening.lock().await;
        let plan = remote_access::bind_plan(&remote, self.port);
        // Policy first, so the peer check rejects stale LAN connections from
        // here on; then the epoch, so open sockets close.
        if let Ok(mut current) = self.state.remote.write() {
            *current = remote;
        }
        self.state.epoch.send_modify(|e| *e = e.wrapping_add(1));
        for old in listening.drain(..) {
            stop_listener(old).await;
        }
        for (i, addr) in plan.addrs.iter().enumerate() {
            match bind_with_retry(*addr).await {
                Ok(listener) => {
                    let addr = listener.local_addr().unwrap_or(*addr);
                    let (shutdown, rx) = oneshot::channel();
                    let stopped = async {
                        let _ = rx.await;
                    };
                    // A TLS transport would wrap the listener here.
                    let task = match plan.transport {
                        Transport::PlainWs => {
                            tokio::spawn(serve(listener, Arc::clone(&self.state), stopped))
                        }
                    };
                    crate::debug_log::log_info(&format!("[ws-server] Listening on {}", addr));
                    listening.push(Listening {
                        addr,
                        shutdown,
                        task,
                    });
                }
                Err(e) if i == 0 => {
                    return Err(format!("Failed to bind {}: {}", addr, e));
                }
                Err(e) => {
                    crate::debug_log::log_info(&format!(
                        "[ws-server] Optional listener {} not bound: {}",
                        addr, e
                    ));
                }
            }
        }
        Ok(listening.iter().map(|l| l.addr).collect())
    }
}

impl WsServer {
    /// Change the policy at runtime: clean it, write `remote-access.json`,
    /// then rebind. When the new policy cannot be bound, the old file and
    /// listeners come back and the error is returned.
    pub async fn update(&self, remote: RemoteAccess) -> Result<RemoteAccess, String> {
        let remote = RemoteAccess {
            extra_allowed_hosts: remote_access::normalize_hosts(&remote.extra_allowed_hosts)?,
            ..remote
        };
        let previous = self.remote();
        remote_access::save(&self.config_path, &remote)?;
        if let Err(e) = self.apply(remote.clone()).await {
            let _ = remote_access::save(&self.config_path, &previous);
            if let Err(restore) = self.apply(previous).await {
                crate::debug_log::log_error(&format!("[ws-server] {}", restore));
            }
            return Err(e);
        }
        Ok(remote)
    }
}

async fn stop_listener(old: Listening) {
    let Listening {
        shutdown, mut task, ..
    } = old;
    let _ = shutdown.send(());
    if tokio::time::timeout(STOP_GRACE, &mut task).await.is_err() {
        task.abort();
        let _ = task.await;
    }
}

/// A listener that was just stopped may hold the port for a moment, so an
/// `EADDRINUSE` is retried briefly.
async fn bind_with_retry(addr: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    let mut attempts = 0;
    loop {
        match tokio::net::TcpListener::bind(addr).await {
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && attempts < 10 => {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            result => return result,
        }
    }
}

// ── HTTP endpoints ──────────────────────────────────────────────────

async fn health() -> &'static str {
    "ok"
}

async fn info() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "name": "c9watch",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

// ── Static file serving (mobile client) ─────────────────────────────

async fn serve_static_fallback(uri: axum::http::Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    if path.is_empty() {
        return serve_embedded_file("index.html");
    }
    serve_embedded_file(path)
}

fn serve_embedded_file(path: &str) -> impl IntoResponse {
    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                file.data.into_owned(),
            )
                .into_response()
        }
        // SPA fallback: serve index.html for unmatched routes
        None => match Assets::get("index.html") {
            Some(file) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html".to_string())],
                file.data.into_owned(),
            )
                .into_response(),
            None => (StatusCode::NOT_FOUND, "Not found").into_response(),
        },
    }
}

// ── WebSocket handler ───────────────────────────────────────────────

#[derive(Deserialize)]
struct WsQuery {
    token: Option<String>,
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsQuery>,
    State(state): State<Arc<WsState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> axum::response::Response {
    match &params.token {
        Some(token) if token == &state.auth_token => ws
            .on_upgrade(move |socket| handle_socket(socket, state, peer.ip()))
            .into_response(),
        _ => (
            axum::http::StatusCode::UNAUTHORIZED,
            "Invalid or missing token",
        )
            .into_response(),
    }
}

/// Close code sent when the remote-access policy changes (1012, "service
/// restart"); the client reconnects.
const CLOSE_POLICY_CHANGED: u16 = 1012;

async fn handle_socket(mut socket: WebSocket, state: Arc<WsState>, peer: IpAddr) {
    // Subscribe before re-checking the policy: a change that lands between
    // the HTTP check and here either fires `changed()` or is already visible
    // to the re-check.
    let mut epoch_rx = state.epoch.subscribe();
    if !peer_allowed(state.lan_enabled(), peer) {
        let _ = socket.send(policy_changed_close()).await;
        return;
    }
    crate::debug_log::log_info("[ws-server] Client connected");
    let mut sessions_rx = state.sessions_tx.subscribe();
    let mut notifications_rx = state.notifications_tx.subscribe();
    let (progress_tx, mut progress_rx) = broadcast::channel::<String>(32);

    let mut inflight: Option<tokio::task::JoinHandle<ServerMsg>> = None;
    let mut inflight_id: Option<u64> = None;

    loop {
        tokio::select! {
            _ = epoch_rx.changed() => {
                let _ = socket.send(policy_changed_close()).await;
                break;
            }
            // Incoming client message
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let text_str: &str = &text;
                        let request_id = serde_json::from_str::<serde_json::Value>(text_str)
                            .ok().and_then(|value| value.get("requestId").and_then(|id| id.as_u64()));
                        match serde_json::from_str::<ClientMsg>(text_str) {
                            Ok(ClientMsg::GetConversation {
                                session_id,
                                provider,
                                include_tools,
                            }) => {
                                if let Some(previous) = inflight.take() {
                                    previous.abort();
                                    let cancelled = response_json(&ServerMsg::Error {
                                        message: "Conversation request superseded".into(),
                                    }, inflight_id.take());
                                    if socket.send(Message::Text(cancelled)).await.is_err() { break; }
                                }
                                let progress_tx = progress_tx.clone();
                                inflight_id = request_id;
                                inflight = Some(tokio::spawn(async move {
                                    load_conversation_response(
                                        session_id,
                                        provider,
                                        include_tools,
                                        progress_tx,
                                        request_id,
                                    )
                                    .await
                                }));
                            }
                            Ok(client_msg) => {
                                let response =
                                    handle_message(client_msg, Arc::clone(&state.detector)).await;
                                let json = response_json(&response, request_id);
                                if socket.send(Message::Text(json)).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                let response = ServerMsg::Error {
                                    message: format!("Invalid message: {}", e),
                                };
                                let json = response_json(&response, request_id);
                                if socket.send(Message::Text(json)).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            result = async {
                match inflight.as_mut() {
                    Some(handle) => Some(handle.await),
                    None => std::future::pending().await,
                }
            } => {
                inflight = None;
                let response = match result {
                    Some(Ok(response)) => response,
                    Some(Err(error)) => ServerMsg::Error {
                        message: format!("Failed to load conversation: {error}"),
                    },
                    None => continue,
                };
                let json = response_json(&response, inflight_id.take());
                if socket.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
            // Push session updates from polling loop
            Ok(sessions_json) = sessions_rx.recv() => {
                let msg = ServerMsg::SessionsUpdated {
                    data: serde_json::from_str(&sessions_json).unwrap_or_default(),
                };
                let json = serde_json::to_string(&msg).unwrap_or_default();
                if socket.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
            // Push notifications to WS clients
            Ok(notif_json) = notifications_rx.recv() => {
                let msg = ServerMsg::Notification {
                    data: serde_json::from_str(&notif_json).unwrap_or_default(),
                };
                let json = serde_json::to_string(&msg).unwrap_or_default();
                if socket.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
            Ok(progress_json) = progress_rx.recv() => {
                let msg = ServerMsg::ConversationProgress {
                    data: serde_json::from_str(&progress_json).unwrap_or_default(),
                };
                let json = serde_json::to_string(&msg).unwrap_or_default();
                if socket.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
        }
    }

    if let Some(handle) = inflight {
        handle.abort();
    }
    crate::debug_log::log_info("[ws-server] Client disconnected");
}

fn policy_changed_close() -> Message {
    Message::Close(Some(CloseFrame {
        code: CLOSE_POLICY_CHANGED,
        reason: "remote access changed".into(),
    }))
}

// ── Message dispatch ────────────────────────────────────────────────

fn response_json(response: &ServerMsg, request_id: Option<u64>) -> String {
    let mut value = serde_json::to_value(response).unwrap_or_default();
    if let Some(id) = request_id {
        value["requestId"] = id.into();
    }
    value.to_string()
}

async fn load_conversation_response(
    session_id: String,
    provider: Option<crate::session::SessionProvider>,
    include_tools: bool,
    progress_tx: broadcast::Sender<String>,
    request_id: Option<u64>,
) -> ServerMsg {
    let emit_id = session_id.clone();
    match tokio::task::spawn_blocking(move || {
        crate::get_conversation_data_for_provider_with_progress(
            &session_id,
            provider,
            include_tools,
            &mut |bytes_read, bytes_total| {
                let payload = crate::session::conversation::ConversationProgress {
                    session_id: emit_id.clone(),
                    provider,
                    request_id,
                    bytes_read,
                    bytes_total,
                };
                let _ = progress_tx.send(serde_json::to_string(&payload).unwrap_or_default());
            },
        )
    })
    .await
    {
        Ok(Ok(conv)) => ServerMsg::Conversation {
            data: serde_json::to_value(&conv).unwrap_or_default(),
        },
        Ok(Err(e)) => ServerMsg::Error { message: e },
        Err(e) => ServerMsg::Error {
            message: format!("Failed to load conversation: {e}"),
        },
    }
}

/// `getSessions` reply. `data` is the session array, the same shape as
/// `sessionsUpdated`; the detection diagnostics stay on the server.
fn sessions_response<S: Serialize, D>(scan: Result<(Vec<S>, D), String>) -> ServerMsg {
    match scan {
        Ok((sessions, _diagnostics)) => ServerMsg::Sessions {
            data: serde_json::to_value(&sessions).unwrap_or_default(),
        },
        Err(message) => ServerMsg::Error { message },
    }
}

const STOP_SESSION_REMOVED: &str =
    "stopSession removed; update the mobile client (use session.stop{addr})";

async fn handle_message(
    msg: ClientMsg,
    detector: Arc<Mutex<crate::session::DetectorState>>,
) -> ServerMsg {
    let stop = move |addr: String, pid: Option<u32>| {
        crate::actions::stop_session_by_key(
            &addr,
            pid,
            || crate::detect_sessions(&detector),
            crate::actions::process_start_secs,
        )
        .map(|_| ())
    };
    handle_message_with_owners(msg, crate::session::global_provider_source_owners(), stop).await
}

/// `stop` stops the session at an address and runs on the blocking pool;
/// tests inject it the way they inject `owners`.
async fn handle_message_with_owners(
    msg: ClientMsg,
    owners: crate::session::ProviderSourceOwners,
    stop: impl FnOnce(String, Option<u32>) -> Result<(), String> + Send + 'static,
) -> ServerMsg {
    match msg {
        ClientMsg::GetSubscriptionUsage => ServerMsg::SubscriptionUsage {
            data: serde_json::to_value(crate::subscription_usage::get_subscription_usage().await)
                .unwrap_or_default(),
        },
        ClientMsg::GetSessions => sessions_response(
            crate::blocking::scan(
                &crate::blocking::DISCOVERY,
                crate::polling::detect_and_enrich_sessions,
            )
            .await,
        ),

        // handle_socket owns getConversation (cancellation + progress).
        ClientMsg::GetConversation { .. } => ServerMsg::Error {
            message: "getConversation is handled by the socket loop".into(),
        },

        ClientMsg::SessionStop { addr, pid } => {
            match tokio::task::spawn_blocking(move || stop(addr, pid)).await {
                Ok(Ok(())) => ServerMsg::Ok,
                Ok(Err(e)) => ServerMsg::Error { message: e },
                Err(e) => ServerMsg::Error {
                    message: format!("Stop failed: {e}"),
                },
            }
        }

        ClientMsg::StopSession { .. } => ServerMsg::Error {
            message: STOP_SESSION_REMOVED.into(),
        },

        ClientMsg::OpenSession { pid, project_path } => {
            match crate::actions::open_session(pid, project_path) {
                Ok(()) => ServerMsg::Ok,
                Err(e) => ServerMsg::Error { message: e },
            }
        }

        ClientMsg::RenameSession {
            session_id,
            new_name,
            provider,
        } => {
            match crate::run_validated_rename(provider, &session_id, &owners, || {
                crate::write_session_title(&session_id, &new_name)
            }) {
                Ok(()) => ServerMsg::Ok,
                Err(e) => ServerMsg::Error { message: e },
            }
        }

        ClientMsg::GetAwayActivity { requests } => match tokio::task::spawn_blocking(move || {
            crate::session::away::away_activity(&requests)
        })
        .await
        {
            Ok(activity) => ServerMsg::AwayActivity {
                data: serde_json::to_value(&activity).unwrap_or_default(),
            },
            Err(e) => ServerMsg::Error {
                message: format!("Failed to read away activity: {e}"),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rename_test_fixtures::{synthetic_codex_owner, synthetic_cursor_owner};

    const SESSION_ID: &str = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee";

    fn no_stop(_: String, _: Option<u32>) -> Result<(), String> {
        panic!("stop must not run");
    }

    #[test]
    fn rename_message_keeps_provider_namespace_for_validation() {
        let message: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "renameSession",
            "sessionId": "same-id",
            "newName": "synthetic",
            "provider": "cursor"
        }))
        .unwrap();

        match message {
            ClientMsg::RenameSession { provider, .. } => {
                assert_eq!(provider, Some(crate::session::SessionProvider::Cursor));
                assert!(crate::session::validate_rename_provider(provider).is_err());
            }
            _ => panic!("expected rename message"),
        }
    }

    #[test]
    fn legacy_rename_message_keeps_compatibility_shape() {
        let message: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "renameSession",
            "sessionId": "legacy-id",
            "newName": "synthetic"
        }))
        .unwrap();

        match message {
            ClientMsg::RenameSession { provider, .. } => {
                assert!(provider.is_none());
                assert!(crate::session::validate_rename_provider(provider).is_ok());
            }
            _ => panic!("expected rename message"),
        }
    }

    #[tokio::test]
    async fn websocket_rename_collision_rejects_before_custom_title_write() {
        let cursor_temp = tempfile::tempdir().unwrap();
        let codex_temp = tempfile::tempdir().unwrap();
        let owners = [
            synthetic_cursor_owner(&cursor_temp, SESSION_ID),
            synthetic_codex_owner(&codex_temp, SESSION_ID),
        ];

        for owners in owners {
            let response = handle_message_with_owners(
                ClientMsg::RenameSession {
                    session_id: SESSION_ID.to_string(),
                    new_name: "synthetic collision".to_string(),
                    provider: None,
                },
                owners,
                no_stop,
            )
            .await;

            match response {
                ServerMsg::Error { message } => {
                    assert!(message.contains("requires an explicit provider"));
                }
                ServerMsg::Ok => panic!("collision must not reach title writes"),
                other => panic!("unexpected WebSocket response: {other:?}"),
            }
        }
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;

    #[test]
    fn correlated_success_and_error_keep_request_ids() {
        for response in [
            ServerMsg::Ok,
            ServerMsg::Error {
                message: "superseded".into(),
            },
        ] {
            let value: serde_json::Value =
                serde_json::from_str(&response_json(&response, Some(42))).unwrap();
            assert_eq!(value["requestId"], 42);
        }
        let value: serde_json::Value =
            serde_json::from_str(&response_json(&ServerMsg::Ok, None)).unwrap();
        assert!(value.get("requestId").is_none());
    }

    #[test]
    fn conversation_requests_support_default_hidden_tools_and_correlation() {
        let request: ClientMsg =
            serde_json::from_str(r#"{"type":"getConversation","sessionId":"test","requestId":42}"#)
                .unwrap();
        assert!(matches!(
            request,
            ClientMsg::GetConversation {
                include_tools: false,
                ..
            }
        ));
    }

    #[test]
    fn conversation_progress_request_id_is_omitted_when_none() {
        use crate::session::conversation::ConversationProgress;
        let progress = |request_id| ConversationProgress {
            session_id: "s1".into(),
            provider: None,
            request_id,
            bytes_read: 10,
            bytes_total: 20,
        };
        let with_id = serde_json::to_value(progress(Some(7))).unwrap();
        assert_eq!(with_id["requestId"], 7);
        assert_eq!(with_id["sessionId"], "s1");
        assert_eq!(with_id["bytesRead"], 10);
        assert_eq!(with_id["bytesTotal"], 20);
        let without_id = serde_json::to_value(progress(None)).unwrap();
        assert!(without_id.get("requestId").is_none());
        assert!(without_id.get("provider").unwrap().is_null());
    }

    #[test]
    fn session_stop_request_carries_an_address_and_optional_pid() {
        let request: ClientMsg = serde_json::from_str(
            r#"{"type":"session.stop","addr":"claudeCode:x","pid":1,"requestId":3}"#,
        )
        .unwrap();
        assert!(matches!(
            request,
            ClientMsg::SessionStop { ref addr, pid: Some(1) } if addr == "claudeCode:x"
        ));
        let request: ClientMsg =
            serde_json::from_str(r#"{"type":"session.stop","addr":"claudeCode:x"}"#).unwrap();
        assert!(matches!(request, ClientMsg::SessionStop { pid: None, .. }));
    }

    /// Records each stop the handler asks for, and answers `Ok`.
    fn recording_stop() -> (
        Arc<Mutex<Vec<(String, Option<u32>)>>>,
        impl FnOnce(String, Option<u32>) -> Result<(), String> + Send + 'static,
    ) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&calls);
        let stop = move |addr, pid| {
            sink.lock().unwrap().push((addr, pid));
            Ok(())
        };
        (calls, stop)
    }

    fn no_owners() -> crate::session::ProviderSourceOwners {
        crate::session::ProviderSourceOwners::from_test_sources(None, None, None)
    }

    #[tokio::test]
    async fn session_stop_passes_the_address_to_stop() {
        let (calls, stop) = recording_stop();
        let request =
            serde_json::from_str(r#"{"type":"session.stop","addr":"claudeCode:x","pid":7}"#)
                .unwrap();
        let response = handle_message_with_owners(request, no_owners(), stop).await;
        assert!(matches!(response, ServerMsg::Ok));
        assert_eq!(
            *calls.lock().unwrap(),
            [("claudeCode:x".to_string(), Some(7))]
        );
    }

    #[tokio::test]
    async fn removed_stop_session_by_pid_errors_without_stopping() {
        let (calls, stop) = recording_stop();
        let request = serde_json::from_str(r#"{"type":"stopSession","pid":4242}"#).unwrap();
        let response = handle_message_with_owners(request, no_owners(), stop).await;
        match response {
            ServerMsg::Error { message } => assert!(message.contains("removed")),
            other => panic!("expected the removed error, got {other:?}"),
        }
        assert!(calls.lock().unwrap().is_empty());
    }

    /// The mobile client reads `data` as a session array; a `[sessions,
    /// diagnostics]` pair left it stuck on "Connecting" (#161).
    #[test]
    fn get_sessions_reply_is_the_session_array() {
        let session = serde_json::json!({ "id": "s1", "projectPath": "/tmp/p" });
        let diagnostics = serde_json::json!({ "claudeProcessesFound": 1 });
        let reply = sessions_response(Ok((vec![session.clone()], diagnostics)));
        let value: serde_json::Value =
            serde_json::from_str(&response_json(&reply, Some(1))).unwrap();
        assert_eq!(value["type"], "sessions");
        assert_eq!(value["data"], serde_json::json!([session]));
    }
}

#[cfg(test)]
mod server_tests {
    use super::*;
    use crate::session::source::{
        DetectedSession, DetectionDiagnostics, SessionDetectorError, SessionSource,
    };
    use crate::session::{BackendMode, DetectorState};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct EmptySource;

    impl SessionSource for EmptySource {
        fn detect(
            &mut self,
        ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
            Ok((Vec::new(), DetectionDiagnostics::default()))
        }
        fn backend_name(&self) -> &'static str {
            "empty"
        }
    }

    pub(super) fn test_server(dir: &std::path::Path) -> WsServer {
        let detector = Arc::new(Mutex::new(DetectorState::for_test(
            Box::new(EmptySource),
            BackendMode::ForceCli,
        )));
        let state = Arc::new(WsState::new(
            "test-token".into(),
            broadcast::channel(4).0,
            broadcast::channel(4).0,
            detector,
        ));
        // Port 0: each listener gets a free port.
        WsServer::new(state, 0, dir.join(remote_access::FILE_NAME))
    }

    async fn health(addr: SocketAddr) -> std::io::Result<String> {
        let mut stream = tokio::net::TcpStream::connect(addr).await?;
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await?;
        Ok(reply)
    }

    #[test]
    fn non_loopback_peers_need_lan() {
        let lan_peer: IpAddr = "192.168.1.20".parse().unwrap();
        assert!(!peer_allowed(false, lan_peer));
        assert!(peer_allowed(true, lan_peer));
        assert!(peer_allowed(false, "::ffff:127.0.0.1".parse().unwrap()));
    }

    #[tokio::test]
    async fn default_policy_serves_on_loopback_only() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(dir.path());
        let bound = server.apply(RemoteAccess::default()).await.unwrap();
        assert!(!bound.is_empty());
        assert!(bound.iter().all(|a| a.ip().is_loopback()), "{bound:?}");
        let reply = health(bound[0]).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(reply.ends_with("ok"), "{reply}");
    }

    #[tokio::test]
    async fn policy_change_closes_open_sockets() {
        use futures_util::StreamExt;
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(dir.path());
        let bound = server.apply(RemoteAccess::default()).await.unwrap();
        let url = format!("ws://{}/ws?token=test-token", bound[0]);
        let (mut client, _) = tokio_tungstenite::connect_async(url).await.unwrap();

        server.update(RemoteAccess::default()).await.unwrap();

        let frame = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(msg) = client.next().await {
                if let Ok(tokio_tungstenite::tungstenite::Message::Close(frame)) = msg {
                    return frame;
                }
            }
            None
        })
        .await
        .expect("socket was not closed");
        let frame = frame.expect("close frame");
        assert_eq!(u16::from(frame.code), CLOSE_POLICY_CHANGED);
    }

    #[tokio::test]
    async fn update_writes_the_file_and_rejects_bad_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(dir.path());
        server.apply(RemoteAccess::default()).await.unwrap();
        let saved = server
            .update(RemoteAccess {
                extra_allowed_hosts: vec![" Box.LAN ".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(saved.extra_allowed_hosts, ["box.lan"]);
        assert_eq!(remote_access::load(server.config_path()), saved);
        assert_eq!(server.remote(), saved);

        let epoch = *server.state().epoch.borrow();
        let bad = RemoteAccess {
            extra_allowed_hosts: vec!["http://evil".into()],
            ..Default::default()
        };
        assert!(server.update(bad).await.is_err());
        assert_eq!(*server.state().epoch.borrow(), epoch, "nothing rebinds");
        assert_eq!(remote_access::load(server.config_path()), saved);
    }

    #[tokio::test]
    async fn apply_replaces_the_old_listeners() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(dir.path());
        let first = server.apply(RemoteAccess::default()).await.unwrap();
        let epoch = *server.state().epoch.borrow();
        let second = server.apply(RemoteAccess::default()).await.unwrap();
        assert_eq!(*server.state().epoch.borrow(), epoch + 1);
        assert_eq!(server.bound().await, second);
        for old in first.iter().filter(|a| !second.contains(a)) {
            assert!(health(*old).await.is_err(), "{old} still accepts");
        }
        assert!(health(second[0]).await.is_ok());
    }
}
