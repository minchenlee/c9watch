use crate::gateway::assets::Assets;
use crate::gateway::limits::*;
use crate::gateway::security::*;
use crate::remote_access::{self, RemoteAccess, Transport};
use crate::ws_auth::{client_principal, ClientId, ClientRegistry, Pairing};
use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, Request, State,
    },
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::IntoResponse,
    routing::get,
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot, watch};

pub use crate::gateway::limits::WS_PORT;

/// Shared state for the WebSocket server
pub struct WsState {
    #[cfg(unix)]
    pub core: RwLock<Option<Arc<crate::bridge::Bridge>>>,
    /// Paired clients (token hashes only). In memory: a restart of the
    /// desktop app means pairing again.
    pub clients: Mutex<ClientRegistry>,
    /// The one live pairing code, if any.
    pub pairing: Mutex<Pairing>,
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
    /// Sockets upgraded but not yet through `authenticate` (see
    /// `MAX_UNAUTHENTICATED`).
    unauthenticated: std::sync::atomic::AtomicUsize,
}

impl WsState {
    pub fn new(
        sessions_tx: broadcast::Sender<String>,
        notifications_tx: broadcast::Sender<String>,
        detector: Arc<Mutex<crate::session::DetectorState>>,
    ) -> Self {
        Self {
            #[cfg(unix)]
            core: RwLock::new(None),
            clients: Mutex::new(ClientRegistry::new()),
            pairing: Mutex::new(Pairing::new()),
            sessions_tx,
            notifications_tx,
            detector,
            remote: RwLock::new(RemoteAccess::default()),
            epoch: watch::channel(0).0,
            unauthenticated: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Take one of the `MAX_UNAUTHENTICATED` slots, or `None` when all are
    /// in use. The slot frees when the returned guard drops.
    fn reserve_unauthenticated(self: &Arc<Self>) -> Option<UnauthenticatedSlot> {
        use std::sync::atomic::Ordering;
        self.unauthenticated
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_UNAUTHENTICATED).then_some(n + 1)
            })
            .ok()
            .map(|_| UnauthenticatedSlot(Arc::clone(self)))
    }

    /// The policy now. A poisoned lock fails closed: the defaults are
    /// loopback only.
    fn remote_now(&self) -> RemoteAccess {
        self.remote.read().map(|r| r.clone()).unwrap_or_default()
    }

    /// What remote clients may do now. A poisoned lock fails closed.
    fn policy_now(&self) -> WsPolicy {
        self.remote
            .read()
            .map(|r| WsPolicy::from(&*r))
            .unwrap_or_default()
    }

    fn lan_enabled(&self) -> bool {
        // A poisoned lock fails closed: loopback only.
        self.remote.read().map(|r| r.lan_enabled).unwrap_or(false)
    }
}

// ── Remote capabilities ─────────────────────────────────────────────

/// What a remote (WS) client may do beyond watching session cards. Both are
/// off by default. This is the real gate: the frontend only hides what the
/// server refuses anyway. It is sent to the client as `capabilities` in
/// `authOk` and `paired`; a change closes every socket, so a client always
/// learns the new value when it reconnects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct WsPolicy {
    /// Full transcripts (`getConversation`) and conversation text in pushes.
    #[serde(rename = "conversation")]
    pub allow_conversation: bool,
    /// `openSession`: focus a terminal or editor window on this Mac.
    #[serde(rename = "open")]
    pub allow_open: bool,
}

impl From<&RemoteAccess> for WsPolicy {
    fn from(remote: &RemoteAccess) -> Self {
        Self {
            allow_conversation: remote.allow_conversation,
            allow_open: remote.allow_open,
        }
    }
}

/// `code` of an error frame for a request the policy turned off.
const CAPABILITY_DISABLED: &str = "capabilityDisabled";

fn capability_disabled(capability: &'static str) -> ServerMsg {
    let message = match capability {
        "conversation" => "getConversation disabled over remote",
        _ => "openSession disabled over remote",
    };
    ServerMsg::CapabilityDisabled {
        message: message.into(),
        code: CAPABILITY_DISABLED,
        capability,
    }
}

/// Legacy transport adaptation over the same schema projection used by c9p.
fn redact_sessions(data: &mut serde_json::Value, policy: &WsPolicy) {
    use crate::gateway::redaction::{self, Sensitivity};
    redaction::legacy_sessions(
        data,
        if policy.allow_conversation {
            Sensitivity::Conversation
        } else {
            Sensitivity::Metadata
        },
    );
    if !policy.allow_open {
        for session in data
            .as_array_mut()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_object_mut)
        {
            session.insert("canOpen".into(), false.into());
            session.remove("openTarget");
        }
    }
}

/// A notification's `title` can be the session's first prompt. With
/// conversations off it becomes `redactedTitle` (the project name, else the
/// provider's name). `redactedTitle` itself never goes out.
fn redact_notification(data: &mut serde_json::Value, policy: &WsPolicy) {
    use crate::gateway::redaction::{self, Sensitivity};
    redaction::legacy_notification(
        data,
        if policy.allow_conversation {
            Sensitivity::Conversation
        } else {
            Sensitivity::Metadata
        },
    );
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

    /// Focus the window of the session at `addr`. Like `session.stop`, the
    /// server resolves the pid and project path itself; `pid`, when given,
    /// only has to agree. An old client's `projectPath` is ignored.
    #[serde(rename = "openSession")]
    OpenSession {
        addr: String,
        #[serde(default)]
        pid: Option<u32>,
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

    /// An `error` the client can tell apart: the remote-access policy
    /// turned `capability` off.
    #[serde(rename = "error")]
    CapabilityDisabled {
        message: String,
        code: &'static str,
        capability: &'static str,
    },

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

/// The port a listener serves on. `Host` and `Origin` must name it.
#[derive(Debug, Clone, Copy)]
struct ListenPort(u16);

fn router(state: Arc<WsState>, port: u16) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health))
        .route("/info", get(info))
        .fallback(get(serve_static_fallback))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            peer_guard,
        ))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            host_guard,
        ))
        .layer(axum::middleware::from_fn(security_headers))
        .layer(Extension(ListenPort(port)))
        .with_state(state)
}

/// Serve one bound listener until `shutdown` resolves. Taking a bound
/// listener (not an address) lets tests bind port 0.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<WsState>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) {
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(e) => {
            crate::debug_log::log_error(&format!("[ws-server] Error: {}", e));
            return;
        }
    };
    let app = router(state, port).into_make_service_with_connect_info::<SocketAddr>();
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
    {
        crate::debug_log::log_error(&format!("[ws-server] Error: {}", e));
    }
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

// ── Host and Origin checks ──────────────────────────────────────────

/// The `Host` of a request. HTTP/2 carries it in the URI authority instead.
fn request_host(request: &Request) -> Option<String> {
    request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| request.uri().authority().map(|a| a.as_str().to_string()))
}

/// Rejects a request whose `Host` this server does not answer to. Runs on
/// every route: `/ws`, `/health`, `/info` and the static files.
async fn host_guard(
    State(state): State<Arc<WsState>>,
    Extension(ListenPort(port)): Extension<ListenPort>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    let checked = request_host(&request)
        .filter(|host| {
            host_allowed(
                host,
                &state.remote_now(),
                port,
                remote_access::local_interface_ips,
            )
        })
        .and_then(|host| split_host(&host))
        .map(|(name, port)| match port {
            Some(port) => CheckedHost(format!("{name}:{port}")),
            None => CheckedHost(name),
        });
    match checked {
        Some(host) => {
            let mut response = next.run(request).await;
            response.extensions_mut().insert(host);
            response
        }
        None => (StatusCode::FORBIDDEN, "Host not allowed").into_response(),
    }
}

// ── Security headers ────────────────────────────────────────────────

/// A `Host` that passed [`host_guard`], normalized. Only a checked host is
/// ever written into the CSP.
#[derive(Debug, Clone)]
struct CheckedHost(String);

/// The hashes of the inline scripts in the embedded `index.html` (the
/// SvelteKit bootstrap), computed once.
fn embedded_script_hashes() -> &'static str {
    static HASHES: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASHES.get_or_init(|| {
        Assets::read("index.html")
            .map(|file| inline_script_hashes(&String::from_utf8_lossy(&file)).join(" "))
            .unwrap_or_default()
    })
}

/// Adds the security headers to every response, rejections included.
async fn security_headers(
    Extension(ListenPort(port)): Extension<ListenPort>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    let host = response.extensions().get::<CheckedHost>().cloned();
    let csp = content_security_policy(
        embedded_script_hashes(),
        port,
        host.as_ref().map(|h| h.0.as_str()),
    );
    let headers = response.headers_mut();
    if let Ok(value) = header::HeaderValue::from_str(&csp) {
        headers.insert(header::CONTENT_SECURITY_POLICY, value);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_FRAME_OPTIONS,
        header::HeaderValue::from_static("DENY"),
    );
    response
}

/// How long a stopping listener may take to finish its connections before
/// its task is aborted.
const STOP_GRACE: Duration = Duration::from_secs(2);

struct Listening {
    addr: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

/// A pairing code for the MOBILE modal. `expires_at` is Unix milliseconds.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingCode {
    pub code: String,
    pub expires_at: u64,
}

/// Owns the WS listeners and swaps them when the remote-access policy
/// changes.
pub struct WsServer {
    state: Arc<WsState>,
    port: u16,
    config_path: PathBuf,
    listening: tokio::sync::Mutex<Vec<Listening>>,
    /// Serialize native consent with the exact policy/code write it approves.
    pub(crate) confirmation: tokio::sync::Mutex<()>,
}

impl WsServer {
    pub fn new(state: Arc<WsState>, port: u16, config_path: PathBuf) -> Self {
        Self {
            state,
            port,
            config_path,
            listening: tokio::sync::Mutex::new(Vec::new()),
            confirmation: tokio::sync::Mutex::new(()),
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

    /// Start a new pairing code; the previous one stops working. Returns
    /// the code and its expiry as Unix milliseconds.
    pub fn create_pairing_code(&self) -> Result<PairingCode, String> {
        let now = std::time::Instant::now();
        let (code, expires_at) = self
            .state
            .pairing
            .lock()
            .map_err(|_| "pairing state unavailable".to_string())?
            .create(now);
        let expires_at = std::time::SystemTime::now() + expires_at.saturating_duration_since(now);
        let expires_at_ms = expires_at
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Ok(PairingCode {
            code,
            expires_at: expires_at_ms,
        })
    }

    /// Forget every paired client, void the live pairing code, and close
    /// every open socket. Clients reconnect, their tokens fail, and they
    /// must pair again with a new code.
    pub fn forget_paired_clients(&self) -> Result<(), String> {
        // Lock order matches the pairing path in `check_auth_frame`:
        // pairing, then clients, held together.
        let mut pairing = self
            .state
            .pairing
            .lock()
            .map_err(|_| "pairing state unavailable".to_string())?;
        pairing.revoke();
        self.state
            .clients
            .lock()
            .map_err(|_| "client registry unavailable".to_string())?
            .forget_all();
        drop(pairing);
        self.state.epoch.send_modify(|e| *e = e.wrapping_add(1));
        Ok(())
    }

    /// The current policy.
    pub fn remote(&self) -> RemoteAccess {
        self.state.remote_now()
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
            allowed_origins: remote_access::normalize_origins(&remote.allowed_origins)?,
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
    match Assets::read(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                file,
            )
                .into_response()
        }
        // SPA fallback: serve index.html for unmatched routes
        None => match Assets::read("index.html") {
            Some(file) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html".to_string())],
                file,
            )
                .into_response(),
            None => (StatusCode::NOT_FOUND, "Not found").into_response(),
        },
    }
}

// ── WebSocket handler ───────────────────────────────────────────────

/// At most this many sockets may be waiting to authenticate at once. Each
/// can hold a buffered frame for up to `AUTH_TIMEOUT`.
/// Largest first frame read as an auth frame. A token or code frame is
/// well under 200 bytes.
/// Largest message (and frame) a client may send. Requests are small JSON;
/// `getAwayActivity` with a few hundred sessions is the biggest. axum sets
/// this for the whole socket at upgrade, so it is also the bound on what an
/// unauthenticated client can make the server buffer (axum's default is
/// 64 MiB).

/// One reserved unauthenticated-socket slot; released on drop.
struct UnauthenticatedSlot(Arc<WsState>);

impl Drop for UnauthenticatedSlot {
    fn drop(&mut self) {
        self.0
            .unauthenticated
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<WsState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(ListenPort(port)): Extension<ListenPort>,
    headers: HeaderMap,
) -> axum::response::Response {
    // Origin first: a foreign page cannot even open the socket. The token
    // is not in the URL; it comes in the first frame (see `authenticate`).
    let origin = headers
        .get(header::ORIGIN)
        .map(|v| v.to_str().unwrap_or(""));
    if !origin_allowed(
        origin,
        &state.remote_now(),
        port,
        remote_access::local_interface_ips,
    ) {
        return (StatusCode::FORBIDDEN, "Origin not allowed").into_response();
    }
    let Some(slot) = state.reserve_unauthenticated() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Too many pending connections",
        )
            .into_response();
    };
    ws.max_message_size(MAX_WS_MESSAGE_BYTES)
        .max_frame_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state, peer.ip(), slot))
        .into_response()
}

// ── Handshake ───────────────────────────────────────────────────────

/// How long a client has to send its first frame and get it accepted.
/// Close code: the first frame was missing, late, or not an auth frame.
/// Close code: the token or pairing code was not accepted. The client
/// must pair again; it should not retry on its own.
/// Close code: the pairing code was wrong, expired or already used. Unlike
/// 4401 it says nothing about a token the client may already hold.

/// The first frame a client sends. Nothing else is read before it.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum AuthFrame {
    /// A client paired before presents its token.
    #[serde(rename = "auth")]
    Auth { token: String },
    /// A new client redeems a one-time pairing code.
    #[serde(rename = "pair")]
    Pair { code: String },
}

/// The reply that admits a client.
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum AuthReply {
    #[serde(rename = "authOk")]
    AuthOk {
        #[serde(rename = "clientId")]
        client_id: ClientId,
        /// `client:<clientId>`, from the verified token. Added in C1a;
        /// older clients ignore it.
        principal: String,
        capabilities: WsPolicy,
    },
    /// Sent once, after a code is redeemed. `token` is the client's
    /// credential from now on; the server keeps only its hash.
    #[serde(rename = "paired")]
    Paired {
        #[serde(rename = "clientId")]
        client_id: ClientId,
        token: String,
        capabilities: WsPolicy,
    },
}

/// Checks a first frame against the registry and the pairing code. Returns
/// the reply to send, or the close code to send instead.
fn check_auth_frame(text: &str, state: &WsState) -> Result<AuthReply, u16> {
    if text.len() > MAX_AUTH_FRAME_BYTES {
        return Err(CLOSE_BAD_HANDSHAKE);
    }
    let frame: AuthFrame = serde_json::from_str(text).map_err(|_| CLOSE_BAD_HANDSHAKE)?;
    match frame {
        AuthFrame::Auth { token } => {
            let clients = state.clients.lock().map_err(|_| CLOSE_UNAUTHORIZED)?;
            clients
                .verify(&token)
                .map(|client_id| AuthReply::AuthOk {
                    principal: client_principal(&client_id),
                    client_id,
                    capabilities: state.policy_now(),
                })
                .ok_or(CLOSE_UNAUTHORIZED)
        }
        AuthFrame::Pair { code } => {
            // Hold the pairing lock until the token is issued. Forget takes
            // the same locks in the same order, so it either runs first (the
            // code is void) or after (it forgets the new token).
            let mut pairing = state.pairing.lock().map_err(|_| CLOSE_PAIR_REJECTED)?;
            pairing
                .redeem(&code, std::time::Instant::now())
                .map_err(|_| CLOSE_PAIR_REJECTED)?;
            let (client_id, token) = state
                .clients
                .lock()
                .map_err(|_| CLOSE_PAIR_REJECTED)?
                .issue();
            drop(pairing);
            Ok(AuthReply::Paired {
                client_id,
                token,
                capabilities: state.policy_now(),
            })
        }
    }
}

/// Waits for the first frame and checks it. On success the reply is
/// already sent and the admitted client's id is returned; it is the only
/// source of the connection's principal. On failure the socket gets a close
/// frame with the reason code. `Err(None)` means the client went away first.
async fn authenticate(socket: &mut WebSocket, state: &WsState) -> Result<ClientId, Option<u16>> {
    let first = tokio::time::timeout(AUTH_TIMEOUT, async {
        loop {
            match socket.recv().await {
                Some(Ok(Message::Text(text))) => return Some(text),
                // Control frames may come first; they are not the handshake.
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                Some(Ok(Message::Binary(_))) => return Some(String::new()),
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return None,
            }
        }
    })
    .await;
    let text = match first {
        Ok(Some(text)) => text,
        Ok(None) => return Err(None),
        Err(_) => return Err(Some(CLOSE_BAD_HANDSHAKE)),
    };
    let reply = check_auth_frame(&text, state).map_err(Some)?;
    let json = serde_json::to_string(&reply).unwrap_or_default();
    socket.send(Message::Text(json)).await.map_err(|_| None)?;
    // The id of the verified token, or of the token just issued for a
    // redeemed code.
    match reply {
        AuthReply::AuthOk { client_id, .. } | AuthReply::Paired { client_id, .. } => Ok(client_id),
    }
}

fn close_with(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }))
}

/// Close code sent when the remote-access policy changes (1012, "service
/// restart"); the client reconnects.

async fn handle_socket(
    mut socket: WebSocket,
    state: Arc<WsState>,
    peer: IpAddr,
    slot: UnauthenticatedSlot,
) {
    // Subscribe before re-checking the policy: a change that lands between
    // the HTTP check and here either fires `changed()` or is already visible
    // to the re-check.
    let mut epoch_rx = state.epoch.subscribe();
    if !peer_allowed(state.lan_enabled(), peer) {
        let _ = socket.send(policy_changed_close()).await;
        return;
    }
    // Nothing is subscribed and nothing is sent until the client is in.
    let admitted = tokio::select! {
        _ = epoch_rx.changed() => {
            let _ = socket.send(policy_changed_close()).await;
            return;
        }
        result = authenticate(&mut socket, &state) => result,
    };
    let client_id = match admitted {
        Ok(client_id) => client_id,
        Err(Some(code)) => {
            let reason = if code == CLOSE_UNAUTHORIZED {
                "unauthorized"
            } else if code == CLOSE_PAIR_REJECTED {
                "pairing code rejected"
            } else {
                "expected auth frame"
            };
            let _ = socket.send(close_with(code, reason)).await;
            return;
        }
        Err(None) => return,
    };
    drop(slot);
    let principal = client_principal(&client_id);
    crate::debug_log::log_info(&format!("[ws-server] Client connected as {principal}"));
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
                            Ok(ClientMsg::GetConversation { .. })
                                if !state.policy_now().allow_conversation =>
                            {
                                let refused = response_json(&capability_disabled("conversation"), request_id);
                                if socket.send(Message::Text(refused)).await.is_err() { break; }
                            }
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
                                #[cfg(unix)]
                                let core = state.core.read().ok().and_then(|b| b.clone()).filter(|b| b.active());
                                inflight_id = request_id;
                                inflight = Some(tokio::spawn(async move {
                                    #[cfg(unix)]
                                    if let Some(core) = core {
                                        return core_conversation(core, session_id, provider, include_tools).await;
                                    }
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
                                #[cfg(unix)]
                                let core = state.core.read().ok().and_then(|b| b.clone()).filter(|b| b.active());
                                #[cfg(unix)]
                                if let Some(core) = core {
                                    let response = handle_core_message(core, client_msg, &client_id, &state.policy_now()).await;
                                    if socket.send(Message::Text(response_json(&response, request_id))).await.is_err() { break; }
                                    continue;
                                }
                                let response = handle_message(
                                    client_msg,
                                    &client_id,
                                    &state.policy_now(),
                                    Arc::clone(&state.detector),
                                )
                                .await;
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
                let mut data = serde_json::from_str(&sessions_json).unwrap_or_default();
                redact_sessions(&mut data, &state.policy_now());
                let msg = ServerMsg::SessionsUpdated { data };
                let json = serde_json::to_string(&msg).unwrap_or_default();
                if socket.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
            // Push notifications to WS clients
            Ok(notif_json) = notifications_rx.recv() => {
                let mut data = serde_json::from_str(&notif_json).unwrap_or_default();
                redact_notification(&mut data, &state.policy_now());
                let msg = ServerMsg::Notification { data };
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
    crate::debug_log::log_info(&format!("[ws-server] Client disconnected: {principal}"));
}

fn policy_changed_close() -> Message {
    close_with(CLOSE_POLICY_CHANGED, "remote access changed")
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

/// Every legacy request is still gated by the GUI's remote policy. Once
/// core is usable, the GUI gateway reads the core instead of scanning twice.
#[cfg(unix)]
async fn core_conversation(
    core: Arc<crate::bridge::Bridge>,
    session_id: String,
    provider: Option<crate::session::SessionProvider>,
    include_tools: bool,
) -> ServerMsg {
    let task = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let mut messages = Vec::new();
        let mut offset = 0;
        let mut conversation = serde_json::Value::Null;
        loop {
            let p = serde_json::json!({ "sessionKey": session_id, "provider": provider, "includeTools": include_tools, "page": { "offset": offset, "limit": 1000 } });
            let page = core.call("conversation.get", p, None).map_err(|e| e.msg())?;
            messages.extend(page["messages"].as_array().ok_or("Bad conversation page")?.clone());
            let next = page["nextOffset"].as_u64();
            if conversation.is_null() { conversation = page; }
            match next {
                Some(n) if n > offset => offset = n,
                Some(_) => return Err("Non-advancing conversation page".into()),
                None => break,
            }
        }
        conversation["messages"] = messages.into();
        Ok(conversation)
    }).await;
    match task {
        Ok(Ok(data)) => ServerMsg::Conversation { data },
        Ok(Err(message)) => ServerMsg::Error { message },
        Err(e) => ServerMsg::Error {
            message: e.to_string(),
        },
    }
}
#[cfg(unix)]
async fn handle_core_message(
    core: Arc<crate::bridge::Bridge>,
    msg: ClientMsg,
    client: &ClientId,
    policy: &WsPolicy,
) -> ServerMsg {
    if matches!(msg, ClientMsg::OpenSession { .. }) && !policy.allow_open {
        return capability_disabled("open");
    }
    // Open is the one native action. Resolve identity from the core snapshot,
    // never from a remote-supplied path or a second detection run.
    if let ClientMsg::OpenSession { addr, pid } = &msg {
        let target = core
            .snapshot
            .lock()
            .unwrap()
            .as_array()
            .and_then(|rows| rows.iter().find(|s| s["sessionKey"] == *addr))
            .cloned();
        let response = match target {
            Some(s)
                if s["canOpen"] != false
                    && s["pid"]
                        .as_u64()
                        .is_some_and(|p| p > 0 && pid.is_none_or(|asked| p == asked as u64)) =>
            {
                let pid = s["pid"].as_u64().unwrap() as u32;
                let path = s["projectPath"].as_str().unwrap_or("").to_string();
                match tokio::task::spawn_blocking(move || crate::actions::open_session(pid, path))
                    .await
                {
                    Ok(Ok(())) => ServerMsg::Ok,
                    Ok(Err(message)) => ServerMsg::Error { message },
                    Err(e) => ServerMsg::Error {
                        message: e.to_string(),
                    },
                }
            }
            _ => ServerMsg::Error {
                message: "Session is gone or its process identity changed".into(),
            },
        };
        log_action(client, "openSession", addr, &response);
        return response;
    }
    let (method, params, field, action) = match msg {
        ClientMsg::GetSessions => (
            "session.views",
            serde_json::json!({}),
            Some("sessions"),
            None,
        ),
        ClientMsg::GetSubscriptionUsage => (
            "usage.subscriptions",
            serde_json::json!({}),
            Some("usage"),
            None,
        ),
        ClientMsg::GetAwayActivity { requests } => (
            "session.away",
            serde_json::json!({"requests": requests}),
            Some("activity"),
            None,
        ),
        ClientMsg::SessionStop { addr, pid } => (
            "session.stop",
            serde_json::json!({"sessionKey": addr, "pid": pid}),
            None,
            Some(("session.stop", addr)),
        ),
        ClientMsg::RenameSession {
            session_id,
            new_name,
            provider,
        } => (
            "session.rename",
            serde_json::json!({"sessionId": session_id, "newName": new_name, "provider": provider}),
            None,
            Some(("renameSession", session_id)),
        ),
        ClientMsg::StopSession { .. } => {
            return ServerMsg::Error {
                message: STOP_SESSION_REMOVED.into(),
            }
        }
        _ => {
            return ServerMsg::Error {
                message: "Unsupported native action".into(),
            }
        }
    };
    let task = tokio::task::spawn_blocking(move || core.call(method, params, None)).await;
    let response = match task {
        Ok(Ok(mut value)) => match field {
            Some("sessions") => {
                let mut data = value["sessions"].take();
                redact_sessions(&mut data, policy);
                ServerMsg::Sessions { data }
            }
            Some("usage") => ServerMsg::SubscriptionUsage {
                data: value["usage"].take(),
            },
            Some(_) => ServerMsg::AwayActivity {
                data: value["activity"].take(),
            },
            None => ServerMsg::Ok,
        },
        Ok(Err(e)) => ServerMsg::Error { message: e.msg() },
        Err(e) => ServerMsg::Error {
            message: e.to_string(),
        },
    };
    if let Some((method, target)) = action {
        log_action(client, method, &target, &response);
    }
    response
}

/// `getSessions` reply. `data` is the session array, the same shape as
/// `sessionsUpdated`; the detection diagnostics stay on the server.
fn sessions_response<S: Serialize, D>(
    scan: Result<(Vec<S>, D), String>,
    policy: &WsPolicy,
) -> ServerMsg {
    match scan {
        Ok((sessions, _diagnostics)) => {
            let mut data = serde_json::to_value(&sessions).unwrap_or_default();
            redact_sessions(&mut data, policy);
            ServerMsg::Sessions { data }
        }
        Err(message) => ServerMsg::Error { message },
    }
}

const STOP_SESSION_REMOVED: &str =
    "stopSession removed; update the mobile client (use session.stop{addr})";

async fn handle_message(
    msg: ClientMsg,
    client_id: &ClientId,
    policy: &WsPolicy,
    detector: Arc<Mutex<crate::session::DetectorState>>,
) -> ServerMsg {
    let open_detector = Arc::clone(&detector);
    let stop = move |addr: String, pid: Option<u32>| {
        crate::actions::stop_session_by_key(
            &addr,
            pid,
            || crate::detect_sessions(&detector),
            crate::actions::process_start_secs,
        )
        .map(|_| ())
    };
    let open = move |addr: String, pid: Option<u32>| {
        crate::actions::open_session_by_key(&addr, pid, || crate::detect_sessions(&open_detector))
    };
    handle_message_with_policy(
        msg,
        client_id,
        policy,
        crate::session::global_provider_source_owners(),
        stop,
        open,
    )
    .await
}

/// `stop` and `open` act on the session at an address and run on the
/// blocking pool; tests inject them the way they inject `owners`. `policy`
/// gates `openSession`; `getConversation` is gated in the socket loop.
/// `client_id` is the connection's verified client; actions that change a
/// session are logged with its `client:<id>` principal.
async fn handle_message_with_policy(
    msg: ClientMsg,
    client_id: &ClientId,
    policy: &WsPolicy,
    owners: crate::session::ProviderSourceOwners,
    stop: impl FnOnce(String, Option<u32>) -> Result<(), String> + Send + 'static,
    open: impl FnOnce(String, Option<u32>) -> Result<(), String> + Send + 'static,
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
            policy,
        ),

        // handle_socket owns getConversation (cancellation + progress).
        ClientMsg::GetConversation { .. } => ServerMsg::Error {
            message: "getConversation is handled by the socket loop".into(),
        },

        ClientMsg::SessionStop { addr, pid } => {
            let target = addr.clone();
            let response = match tokio::task::spawn_blocking(move || stop(addr, pid)).await {
                Ok(Ok(())) => ServerMsg::Ok,
                Ok(Err(e)) => ServerMsg::Error { message: e },
                Err(e) => ServerMsg::Error {
                    message: format!("Stop failed: {e}"),
                },
            };
            log_action(client_id, "session.stop", &target, &response);
            response
        }

        ClientMsg::StopSession { .. } => ServerMsg::Error {
            message: STOP_SESSION_REMOVED.into(),
        },

        ClientMsg::OpenSession { addr, .. } if !policy.allow_open => {
            let response = capability_disabled("open");
            log_action(client_id, "openSession", &addr, &response);
            response
        }
        ClientMsg::OpenSession { addr, pid } => {
            let target = addr.clone();
            let response = match tokio::task::spawn_blocking(move || open(addr, pid)).await {
                Ok(Ok(())) => ServerMsg::Ok,
                Ok(Err(e)) => ServerMsg::Error { message: e },
                Err(e) => ServerMsg::Error {
                    message: format!("Open failed: {e}"),
                },
            };
            log_action(client_id, "openSession", &target, &response);
            response
        }

        ClientMsg::RenameSession {
            session_id,
            new_name,
            provider,
        } => {
            let response = match crate::run_validated_rename(provider, &session_id, &owners, || {
                crate::write_session_title(&session_id, &new_name)
            }) {
                Ok(()) => ServerMsg::Ok,
                Err(e) => ServerMsg::Error { message: e },
            };
            log_action(client_id, "renameSession", &session_id, &response);
            response
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

/// Longest target a log line keeps; the target is client input.
const LOG_TARGET_MAX_CHARS: usize = 128;

/// Logs a session action with the principal that asked for it. Only the
/// principal, the action, its target (quoted and cut to
/// `LOG_TARGET_MAX_CHARS`) and ok or refused are logged, never a frame.
/// Only `ServerMsg::Ok` counts as ok. These lines go to the debug log, a
/// ring buffer of `debug_log::MAX_ENTRIES` lines that a busy client can
/// push old lines out of; it is not an audit log.
fn log_action(client_id: &ClientId, action: &str, target: &str, response: &ServerMsg) {
    let outcome = match response {
        ServerMsg::Ok => "ok",
        _ => "refused",
    };
    let target: String = target.chars().take(LOG_TARGET_MAX_CHARS).collect();
    crate::debug_log::log_info(&format!(
        "[ws-server] {} {action} {target:?}: {outcome}",
        client_principal(client_id)
    ));
}

#[cfg(test)]
fn test_client() -> ClientId {
    "test-client".to_string()
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
            let response = handle_message_with_policy(
                ClientMsg::RenameSession {
                    session_id: SESSION_ID.to_string(),
                    new_name: "synthetic collision".to_string(),
                    provider: None,
                },
                &test_client(),
                &WsPolicy::default(),
                owners,
                no_stop,
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
        let response = handle_message_with_policy(
            request,
            &test_client(),
            &WsPolicy::default(),
            no_owners(),
            stop,
            no_open,
        )
        .await;
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
        let response = handle_message_with_policy(
            request,
            &test_client(),
            &WsPolicy::default(),
            no_owners(),
            stop,
            no_open,
        )
        .await;
        match response {
            ServerMsg::Error { message } => assert!(message.contains("removed")),
            other => panic!("expected the removed error, got {other:?}"),
        }
        assert!(calls.lock().unwrap().is_empty());
    }

    fn no_open(_: String, _: Option<u32>) -> Result<(), String> {
        panic!("open must not run")
    }

    fn no_stop(_: String, _: Option<u32>) -> Result<(), String> {
        panic!("stop must not run")
    }

    const OPEN_ON: WsPolicy = WsPolicy {
        allow_conversation: false,
        allow_open: true,
    };

    #[test]
    fn open_session_request_carries_an_address_and_optional_pid() {
        let request: ClientMsg = serde_json::from_str(
            r#"{"type":"openSession","addr":"claudeCode:x","pid":9,"projectPath":"--x"}"#,
        )
        .unwrap();
        assert!(matches!(
            request,
            ClientMsg::OpenSession { ref addr, pid: Some(9) } if addr == "claudeCode:x"
        ));
    }

    #[tokio::test]
    async fn open_session_is_refused_when_remote_open_is_off() {
        let (calls, open) = recording_stop();
        let request =
            serde_json::from_str(r#"{"type":"openSession","addr":"claudeCode:x","pid":7}"#)
                .unwrap();
        let response = handle_message_with_policy(
            request,
            &test_client(),
            &WsPolicy::default(),
            no_owners(),
            no_stop,
            open,
        )
        .await;
        let value: serde_json::Value =
            serde_json::from_str(&response_json(&response, Some(2))).unwrap();
        assert_eq!(value["type"], "error", "{value}");
        assert_eq!(value["code"], "capabilityDisabled");
        assert_eq!(value["capability"], "open");
        assert_eq!(value["requestId"], 2);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn open_session_passes_the_address_when_remote_open_is_on() {
        let (calls, open) = recording_stop();
        let request =
            serde_json::from_str(r#"{"type":"openSession","addr":"claudeCode:x","pid":7}"#)
                .unwrap();
        let response = handle_message_with_policy(
            request,
            &test_client(),
            &OPEN_ON,
            no_owners(),
            no_stop,
            open,
        )
        .await;
        assert!(matches!(response, ServerMsg::Ok), "{response:?}");
        assert_eq!(
            *calls.lock().unwrap(),
            [("claudeCode:x".to_string(), Some(7))]
        );
    }

    fn content_session() -> serde_json::Value {
        serde_json::json!({
            "id": "s1",
            "sessionName": "proj",
            "customTitle": "My title",
            "aiTitle": "secret ai title",
            "codexTitle": "secret codex title",
            "cursorTitle": "secret cursor title",
            "firstPrompt": "secret prompt",
            "latestMessage": "secret latest",
            "lastAssistantMessage": "secret reply",
            "summary": "secret summary",
            "pendingToolName": "Bash",
            "pendingToolInput": { "command": "cat ~/.ssh/id_rsa" },
            "openTarget": "terminal",
            "canOpen": true,
            "canStop": true,
        })
    }

    #[test]
    fn sessions_lose_conversation_content_and_open_when_both_are_off() {
        let mut data = serde_json::json!([content_session()]);
        redact_sessions(&mut data, &WsPolicy::default());
        let s = &data[0];
        assert_eq!(s["firstPrompt"], "");
        assert_eq!(s["latestMessage"], "");
        assert!(s["summary"].is_null());
        assert!(s.get("lastAssistantMessage").is_none());
        assert!(s.get("pendingToolInput").is_none());
        // AI-generated titles summarise the conversation.
        assert!(s.get("aiTitle").is_none());
        assert!(s.get("codexTitle").is_none());
        assert!(s.get("cursorTitle").is_none());
        assert!(!data.to_string().contains("secret"), "{data}");
        assert!(!data.to_string().contains("id_rsa"), "{data}");
        // Session and custom titles are conversation-class too.
        assert_eq!(s["sessionName"], "");
        assert!(s.get("customTitle").is_none());
        assert_eq!(s["pendingToolName"], "Bash");
        assert_eq!(s["canOpen"], false);
        assert!(s.get("openTarget").is_none());
        assert_eq!(s["canStop"], true);
    }

    #[test]
    fn sessions_keep_classified_content_and_drop_unclassified_content_at_conversation_ceiling() {
        let mut data = serde_json::json!([content_session()]);
        let all = WsPolicy {
            allow_conversation: true,
            allow_open: true,
        };
        redact_sessions(&mut data, &all);
        let mut expected = content_session();
        expected["pendingToolInput"] = serde_json::json!({});
        assert_eq!(data, serde_json::json!([expected]));
    }

    #[test]
    fn notifications_fall_back_to_the_name_when_conversations_are_off() {
        let push = || {
            serde_json::json!({
                "title": "secret prompt",
                "redactedTitle": "proj",
                "body": "proj: Finished working",
                "sessionId": "s1",
            })
        };
        let mut off = push();
        redact_notification(&mut off, &WsPolicy::default());
        assert_eq!(off["title"], "proj");
        assert!(off.get("redactedTitle").is_none());
        assert!(off.get("body").is_none());
        assert!(!off.to_string().contains("secret"));

        let mut on = push();
        redact_notification(
            &mut on,
            &WsPolicy {
                allow_conversation: true,
                allow_open: false,
            },
        );
        assert_eq!(on["title"], "secret prompt");
        assert!(on.get("redactedTitle").is_none());
    }

    /// The mobile client reads `data` as a session array; a `[sessions,
    /// diagnostics]` pair left it stuck on "Connecting" (#161).
    #[test]
    fn get_sessions_reply_is_the_session_array() {
        let session = serde_json::json!({ "id": "s1", "projectPath": "/tmp/p" });
        let diagnostics = serde_json::json!({ "claudeProcessesFound": 1 });
        let all = WsPolicy {
            allow_conversation: true,
            allow_open: true,
        };
        let reply = sessions_response(Ok((vec![session.clone()], diagnostics)), &all);
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

    /// Reports the same sessions on every scan.
    struct FixedSource(Vec<DetectedSession>);

    impl SessionSource for FixedSource {
        fn detect(
            &mut self,
        ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
            Ok((self.0.clone(), DetectionDiagnostics::default()))
        }
        fn backend_name(&self) -> &'static str {
            "fixed"
        }
    }

    pub(super) fn test_server(dir: &std::path::Path) -> WsServer {
        test_server_with(dir, Box::new(EmptySource))
    }

    fn test_server_with(dir: &std::path::Path, source: Box<dyn SessionSource>) -> WsServer {
        let detector = Arc::new(Mutex::new(DetectorState::for_test(
            source,
            BackendMode::ForceCli,
        )));
        let state = Arc::new(WsState::new(
            broadcast::channel(4).0,
            broadcast::channel(4).0,
            detector,
        ));
        // Port 0: each listener gets a free port.
        WsServer::new(state, 0, dir.join(remote_access::FILE_NAME))
    }

    async fn health(addr: SocketAddr) -> std::io::Result<String> {
        get_with_host(addr, "/health", &format!("127.0.0.1:{}", addr.port())).await
    }

    async fn get_with_host(addr: SocketAddr, path: &str, host: &str) -> std::io::Result<String> {
        let mut stream = tokio::net::TcpStream::connect(addr).await?;
        let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await?;
        Ok(reply)
    }

    /// Opens `/ws` with the given `Origin`; returns the HTTP status of the
    /// upgrade reply (101 when it upgraded).
    async fn upgrade_status(addr: SocketAddr, origin: Option<&str>) -> u16 {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = format!("ws://{addr}/ws").into_client_request().unwrap();
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert("origin", origin.parse().unwrap());
        }
        match tokio_tungstenite::connect_async(request).await {
            Ok((_, response)) => response.status().as_u16(),
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                response.status().as_u16()
            }
            Err(e) => panic!("upgrade failed: {e}"),
        }
    }

    #[tokio::test]
    async fn upgrade_from_a_foreign_origin_is_forbidden() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(dir.path());
        let addr = server.apply(RemoteAccess::default()).await.unwrap()[0];
        assert_eq!(upgrade_status(addr, Some("http://evil.example")).await, 403);
        let same_origin = format!("http://127.0.0.1:{}", addr.port());
        assert_eq!(upgrade_status(addr, Some(&same_origin)).await, 101);
        assert_eq!(upgrade_status(addr, None).await, 101, "non-browser client");
    }

    fn header_value<'a>(reply: &'a str, name: &str) -> Option<&'a str> {
        let head = reply.split("\r\n\r\n").next()?;
        head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    #[tokio::test]
    async fn responses_carry_security_headers_and_a_strict_csp() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(dir.path());
        let addr = server.apply(RemoteAccess::default()).await.unwrap()[0];
        let port = addr.port();
        let host = format!("localhost:{port}");
        // The static routes answer 404 when the frontend is not built (as in
        // CI); the headers must be there either way.
        for path in ["/", "/health", "/no-such-page"] {
            let reply = get_with_host(addr, path, &host).await.unwrap();
            assert_eq!(
                header_value(&reply, "x-content-type-options"),
                Some("nosniff")
            );
            assert_eq!(header_value(&reply, "referrer-policy"), Some("no-referrer"));
            assert_eq!(header_value(&reply, "x-frame-options"), Some("DENY"));
            let csp = header_value(&reply, "content-security-policy").expect("CSP");
            let script_src = csp
                .split(';')
                .map(str::trim)
                .find(|d| d.starts_with("script-src "))
                .expect("script-src");
            assert!(!script_src.contains("'unsafe-inline'"), "{csp}");
            assert!(!script_src.contains("'unsafe-eval'"), "{csp}");
            for directive in [
                "default-src 'self'",
                "object-src 'none'",
                "base-uri 'none'",
                "frame-ancestors 'none'",
                "form-action 'none'",
            ] {
                assert!(csp.contains(directive), "{directive} missing: {csp}");
            }
            // The page rewrites localhost to 127.0.0.1 for its socket, so
            // every loopback alias must be allowed, not only this Host.
            for ws in [
                format!("ws://127.0.0.1:{port}"),
                format!("ws://localhost:{port}"),
                format!("ws://[::1]:{port}"),
            ] {
                assert!(csp.contains(&ws), "{ws} missing: {csp}");
            }
        }
        let rejected = get_with_host(addr, "/", "evil.example").await.unwrap();
        assert!(rejected.starts_with("HTTP/1.1 403"), "{rejected}");
        let csp = header_value(&rejected, "content-security-policy").expect("CSP on 403");
        assert!(
            !csp.contains("evil.example"),
            "an unchecked Host is never echoed: {csp}"
        );
    }

    #[tokio::test]
    async fn every_route_rejects_a_foreign_host() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(dir.path());
        let addr = server.apply(RemoteAccess::default()).await.unwrap()[0];
        let port = addr.port();
        for path in ["/health", "/info", "/", "/ws"] {
            let reply = get_with_host(addr, path, &format!("evil.example:{port}"))
                .await
                .unwrap();
            assert!(reply.starts_with("HTTP/1.1 403"), "{path}: {reply}");
        }
        let reply = get_with_host(addr, "/info", &format!("localhost:{port}"))
            .await
            .unwrap();
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    }

    const PORT: u16 = 9210;
    const LAN_IP: &str = "192.168.1.20";

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
        let mut client = open(bound[0]).await;
        let token = issue_token(&server);
        send_json(
            &mut client,
            serde_json::json!({"type": "auth", "token": token}),
        )
        .await;
        assert!(matches!(next_event(&mut client).await, Event::Text(v) if v["type"] == "authOk"));

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

    // ── First-frame handshake (end to end) ──────────────────────────

    type Client = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    #[derive(Debug)]
    enum Event {
        Text(serde_json::Value),
        Close(Option<u16>),
    }

    async fn open(addr: SocketAddr) -> Client {
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .expect("upgrade")
            .0
    }

    async fn send_json(client: &mut Client, value: serde_json::Value) {
        use futures_util::SinkExt;
        client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                value.to_string(),
            ))
            .await
            .unwrap();
    }

    /// The next text frame or close, skipping control frames.
    async fn next_event(client: &mut Client) -> Event {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message as T;
        let wait = AUTH_TIMEOUT + Duration::from_secs(3);
        tokio::time::timeout(wait, async {
            loop {
                match client.next().await {
                    Some(Ok(T::Text(text))) => {
                        return Event::Text(serde_json::from_str(&text).unwrap())
                    }
                    Some(Ok(T::Close(frame))) => {
                        return Event::Close(frame.map(|f| u16::from(f.code)))
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(_)) | None => return Event::Close(None),
                }
            }
        })
        .await
        .expect("no frame from the server")
    }

    fn issue_token(server: &WsServer) -> String {
        server.state().clients.lock().unwrap().issue().1
    }

    async fn loopback_server(dir: &std::path::Path) -> (WsServer, SocketAddr) {
        let server = test_server(dir);
        let addr = server.apply(RemoteAccess::default()).await.unwrap()[0];
        (server, addr)
    }

    #[tokio::test]
    async fn no_first_frame_closes_with_4400() {
        let dir = tempfile::tempdir().unwrap();
        let (_server, addr) = loopback_server(dir.path()).await;
        let mut client = open(addr).await;
        let started = std::time::Instant::now();
        assert!(matches!(
            next_event(&mut client).await,
            Event::Close(Some(CLOSE_BAD_HANDSHAKE))
        ));
        assert!(started.elapsed() >= AUTH_TIMEOUT - Duration::from_millis(100));
    }

    #[tokio::test]
    async fn an_oversized_first_frame_closes_with_4400() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        let token = issue_token(&server);
        let mut client = open(addr).await;
        // A valid token padded past the pre-auth limit is still refused.
        let padding = "x".repeat(MAX_AUTH_FRAME_BYTES);
        send_json(
            &mut client,
            serde_json::json!({"type": "auth", "token": token, "pad": padding}),
        )
        .await;
        assert!(matches!(
            next_event(&mut client).await,
            Event::Close(Some(CLOSE_BAD_HANDSHAKE))
        ));
    }

    #[tokio::test]
    async fn a_frame_past_the_message_limit_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        let (mut client, _) = signed_in(&server, addr).await;
        let huge = "x".repeat(MAX_WS_MESSAGE_BYTES + 1);
        send_json(
            &mut client,
            serde_json::json!({"type": "getSessions", "pad": huge}),
        )
        .await;
        // The server drops the connection instead of buffering the frame.
        assert!(matches!(next_event(&mut client).await, Event::Close(_)));
    }

    #[tokio::test]
    async fn unauthenticated_connections_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        let mut waiting = Vec::new();
        for _ in 0..MAX_UNAUTHENTICATED {
            waiting.push(open(addr).await);
        }
        let refused = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await;
        match refused {
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE)
            }
            other => panic!("expected 503, got {:?}", other.map(|_| ())),
        }
        // One of them signs in: its slot frees up for a new connection.
        let token = issue_token(&server);
        send_json(
            &mut waiting[0],
            serde_json::json!({"type": "auth", "token": token}),
        )
        .await;
        assert!(
            matches!(next_event(&mut waiting[0]).await, Event::Text(v) if v["type"] == "authOk")
        );
        let _next = open(addr).await;
    }

    #[tokio::test]
    async fn a_first_frame_that_is_not_auth_closes_with_4400() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        issue_token(&server);
        let mut client = open(addr).await;
        send_json(&mut client, serde_json::json!({"type": "getSessions"})).await;
        assert!(matches!(
            next_event(&mut client).await,
            Event::Close(Some(CLOSE_BAD_HANDSHAKE))
        ));
    }

    #[tokio::test]
    async fn wrong_token_closes_with_4401() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        let token = issue_token(&server);
        let mut client = open(addr).await;
        let wrong = format!("{}0", &token[..63]);
        let wrong = if wrong == token {
            format!("{}1", &token[..63])
        } else {
            wrong
        };
        send_json(
            &mut client,
            serde_json::json!({"type": "auth", "token": wrong}),
        )
        .await;
        assert!(matches!(
            next_event(&mut client).await,
            Event::Close(Some(CLOSE_UNAUTHORIZED))
        ));
    }

    #[tokio::test]
    async fn nothing_is_pushed_before_auth_and_a_valid_token_gets_auth_ok() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        let token = issue_token(&server);
        let state = Arc::clone(server.state());
        let mut client = open(addr).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        // Not subscribed yet, so a push now cannot reach the socket.
        assert_eq!(state.sessions_tx.receiver_count(), 0);
        assert_eq!(state.notifications_tx.receiver_count(), 0);
        let _ = state.sessions_tx.send("[]".into());

        send_json(
            &mut client,
            serde_json::json!({"type": "auth", "token": token}),
        )
        .await;
        match next_event(&mut client).await {
            Event::Text(v) => {
                assert_eq!(v["type"], "authOk", "{v}");
                assert!(v["clientId"].as_str().is_some_and(|id| !id.is_empty()));
                assert!(v.get("token").is_none());
            }
            other => panic!("{other:?}"),
        }
        for _ in 0..50 {
            if state.sessions_tx.receiver_count() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        state.sessions_tx.send("[]".into()).unwrap();
        assert!(matches!(
            next_event(&mut client).await,
            Event::Text(v) if v["type"] == "sessionsUpdated"
        ));
    }

    #[tokio::test]
    async fn pairing_returns_a_token_that_works_on_the_next_connection() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        let code = server.create_pairing_code().unwrap().code;

        let mut client = open(addr).await;
        send_json(
            &mut client,
            serde_json::json!({"type": "pair", "code": code}),
        )
        .await;
        let (client_id, token) = match next_event(&mut client).await {
            Event::Text(v) if v["type"] == "paired" => (
                v["clientId"].as_str().unwrap().to_string(),
                v["token"].as_str().unwrap().to_string(),
            ),
            other => panic!("{other:?}"),
        };
        assert_eq!(token.len(), 64);
        drop(client);

        let mut next = open(addr).await;
        send_json(
            &mut next,
            serde_json::json!({"type": "auth", "token": token}),
        )
        .await;
        assert!(matches!(
            next_event(&mut next).await,
            Event::Text(v) if v["type"] == "authOk" && v["clientId"] == client_id.as_str()
        ));
    }

    /// Pairs from 127.0.0.1 and returns the new client's id, its token and
    /// the code it redeemed.
    async fn pair_in_process(server: &WsServer, addr: SocketAddr) -> (String, String, String) {
        let code = server.create_pairing_code().unwrap().code;
        let mut client = open(addr).await;
        send_json(
            &mut client,
            serde_json::json!({"type": "pair", "code": code}),
        )
        .await;
        match next_event(&mut client).await {
            Event::Text(v) if v["type"] == "paired" => (
                v["clientId"].as_str().unwrap().to_string(),
                v["token"].as_str().unwrap().to_string(),
                code,
            ),
            other => panic!("{other:?}"),
        }
    }

    /// Connects with `token` and returns the socket and its `authOk`.
    async fn connect_with(addr: SocketAddr, token: &str) -> (Client, serde_json::Value) {
        let mut client = open(addr).await;
        send_json(
            &mut client,
            serde_json::json!({"type": "auth", "token": token}),
        )
        .await;
        match next_event(&mut client).await {
            Event::Text(v) if v["type"] == "authOk" => (client, v),
            other => panic!("{other:?}"),
        }
    }

    /// The server logs after it sends a reply, so a line may land a moment
    /// after the client reads the frame.
    async fn wait_for_log(needle: &str) {
        for _ in 0..100 {
            if crate::debug_log::get_logs()
                .iter()
                .any(|entry| entry.message.contains(needle))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("no log line contains {needle:?}");
    }

    // X11 (C1a-30): the principal comes from the verified token. A loopback
    // peer gets `client:<id>` like any other; there is no `human`.
    #[tokio::test]
    async fn a_paired_client_connects_as_its_client_principal() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        assert!(addr.ip().is_loopback());
        let (client_id, token, _) = pair_in_process(&server, addr).await;
        let (_client, ok) = connect_with(addr, &token).await;
        assert_eq!(ok["clientId"], client_id.as_str());
        assert_eq!(ok["principal"], format!("client:{client_id}"));

        // A second client gets its own principal.
        let (other_id, other_token, _) = pair_in_process(&server, addr).await;
        assert_ne!(other_id, client_id);
        let (_other, ok) = connect_with(addr, &other_token).await;
        assert_eq!(ok["principal"], format!("client:{other_id}"));
    }

    // Older clients keep working: `authOk` still has `clientId` and
    // `capabilities`, and `principal` is only added.
    #[tokio::test]
    async fn auth_ok_keeps_its_old_fields() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        let (_client, ok) = signed_in(&server, addr).await;
        let mut keys: Vec<&str> = ok.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["capabilities", "clientId", "principal", "type"]);
    }

    // E3: neither the token nor the pairing code reaches the log; the
    // connection is logged by its principal.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn the_log_has_the_principal_and_no_token_or_code() {
        // Hold the log lock: debug_log tests clear and flood the buffer.
        let _log_lock = crate::debug_log::TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        let (client_id, token, code) = pair_in_process(&server, addr).await;
        let (mut client, _) = connect_with(addr, &token).await;
        send_json(
            &mut client,
            serde_json::json!({"type": "session.stop", "addr": "claudeCode:none", "requestId": 1}),
        )
        .await;
        assert!(matches!(next_event(&mut client).await, Event::Text(v) if v["type"] == "error"));
        // A wrong token is refused without echoing it.
        let wrong = "f".repeat(64);
        let mut refused = open(addr).await;
        send_json(
            &mut refused,
            serde_json::json!({"type": "auth", "token": wrong}),
        )
        .await;
        assert!(matches!(
            next_event(&mut refused).await,
            Event::Close(Some(CLOSE_UNAUTHORIZED))
        ));

        let principal = format!("client:{client_id}");
        wait_for_log(&format!("Client connected as {principal}")).await;
        wait_for_log(&format!(
            "{principal} session.stop \"claudeCode:none\": refused"
        ))
        .await;
        for entry in crate::debug_log::get_logs() {
            assert!(!entry.message.contains(&token), "{}", entry.message);
            assert!(!entry.message.contains(&wrong), "{}", entry.message);
            assert!(!entry.message.contains(&code), "{}", entry.message);
        }
    }

    // E4: a stop whose pid is not the detected pid is refused, and the log
    // names the principal that asked.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_stop_with_the_wrong_pid_is_refused() {
        // Hold the log lock: debug_log tests clear and flood the buffer.
        let _log_lock = crate::debug_log::TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Past the macOS pid range, so even a mistaken signal hits nothing.
        const PID: u32 = 4_000_000;
        let session = DetectedSession {
            process_start_secs: Some(1_000),
            ..DetectedSession::with_legacy_defaults(
                PID,
                "/tmp".into(),
                "/tmp/proj".into(),
                Some("wrong-pid-fixture".to_string()),
                "proj".to_string(),
            )
        };
        let dir = tempfile::tempdir().unwrap();
        let server = test_server_with(dir.path(), Box::new(FixedSource(vec![session])));
        let addr = server.apply(RemoteAccess::default()).await.unwrap()[0];
        let (mut client, ok) = signed_in(&server, addr).await;
        let principal = ok["principal"].as_str().unwrap().to_string();
        send_json(
            &mut client,
            serde_json::json!({
                "type": "session.stop",
                "addr": "claudeCode:wrong-pid-fixture",
                "pid": PID + 1,
                "requestId": 7,
            }),
        )
        .await;
        match next_event(&mut client).await {
            Event::Text(v) => {
                assert_eq!(v["type"], "error", "{v}");
                assert_eq!(v["requestId"], 7);
                assert!(
                    v["message"].as_str().unwrap().contains("no longer running"),
                    "{v}"
                );
            }
            other => panic!("{other:?}"),
        }
        wait_for_log(&format!(
            "{principal} session.stop \"claudeCode:wrong-pid-fixture\": refused"
        ))
        .await;
    }

    #[tokio::test]
    async fn a_pairing_code_works_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        let code = server.create_pairing_code().unwrap().code;

        let mut first = open(addr).await;
        send_json(
            &mut first,
            serde_json::json!({"type": "pair", "code": code}),
        )
        .await;
        assert!(matches!(next_event(&mut first).await, Event::Text(v) if v["type"] == "paired"));

        // A refused code gets its own close code, so a client that also
        // holds a valid token knows the token is not the problem.
        let mut second = open(addr).await;
        send_json(
            &mut second,
            serde_json::json!({"type": "pair", "code": code}),
        )
        .await;
        assert!(matches!(
            next_event(&mut second).await,
            Event::Close(Some(CLOSE_PAIR_REJECTED))
        ));
    }

    #[tokio::test]
    async fn forgetting_clients_closes_sockets_and_revokes_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        let token = issue_token(&server);
        let mut client = open(addr).await;
        send_json(
            &mut client,
            serde_json::json!({"type": "auth", "token": token}),
        )
        .await;
        assert!(matches!(next_event(&mut client).await, Event::Text(v) if v["type"] == "authOk"));

        server.forget_paired_clients().unwrap();
        assert!(matches!(
            next_event(&mut client).await,
            Event::Close(Some(CLOSE_POLICY_CHANGED))
        ));

        let mut again = open(addr).await;
        send_json(
            &mut again,
            serde_json::json!({"type": "auth", "token": token}),
        )
        .await;
        assert!(matches!(
            next_event(&mut again).await,
            Event::Close(Some(CLOSE_UNAUTHORIZED))
        ));
    }

    #[tokio::test]
    async fn forgetting_clients_also_voids_the_live_pairing_code() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        let code = server.create_pairing_code().unwrap().code;
        server.forget_paired_clients().unwrap();

        let mut client = open(addr).await;
        send_json(
            &mut client,
            serde_json::json!({"type": "pair", "code": code}),
        )
        .await;
        assert!(matches!(
            next_event(&mut client).await,
            Event::Close(Some(CLOSE_PAIR_REJECTED))
        ));
    }

    /// Opens an authenticated socket and returns it with the `authOk` frame.
    async fn signed_in(server: &WsServer, addr: SocketAddr) -> (Client, serde_json::Value) {
        let token = issue_token(server);
        let mut client = open(addr).await;
        send_json(
            &mut client,
            serde_json::json!({"type": "auth", "token": token}),
        )
        .await;
        match next_event(&mut client).await {
            Event::Text(v) if v["type"] == "authOk" => (client, v),
            other => panic!("{other:?}"),
        }
    }

    async fn wait_for_subscriber(state: &WsState) {
        for _ in 0..50 {
            if state.sessions_tx.receiver_count() == 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("socket never subscribed");
    }

    #[tokio::test]
    async fn auth_ok_carries_the_capabilities_and_they_default_off() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        let (_client, ok) = signed_in(&server, addr).await;
        assert_eq!(
            ok["capabilities"],
            serde_json::json!({"conversation": false, "open": false})
        );

        server
            .apply(RemoteAccess {
                allow_conversation: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let addr = server.bound().await[0];
        let (_client, ok) = signed_in(&server, addr).await;
        assert_eq!(
            ok["capabilities"],
            serde_json::json!({"conversation": true, "open": false})
        );
    }

    #[tokio::test]
    async fn get_conversation_is_refused_without_progress_when_off() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        let (mut client, _) = signed_in(&server, addr).await;
        send_json(
            &mut client,
            serde_json::json!({"type": "getConversation", "sessionId": SESSION, "requestId": 5}),
        )
        .await;
        match next_event(&mut client).await {
            Event::Text(v) => {
                assert_eq!(v["type"], "error", "{v}");
                assert_eq!(v["code"], "capabilityDisabled");
                assert_eq!(v["capability"], "conversation");
                assert_eq!(v["requestId"], 5);
            }
            other => panic!("{other:?}"),
        }
        // The next frame is this push, not a conversationProgress.
        let state = Arc::clone(server.state());
        wait_for_subscriber(&state).await;
        state.sessions_tx.send("[]".into()).unwrap();
        assert!(matches!(
            next_event(&mut client).await,
            Event::Text(v) if v["type"] == "sessionsUpdated"
        ));
    }

    const SESSION: &str = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee";

    #[tokio::test]
    async fn pushes_carry_no_conversation_content_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let (server, addr) = loopback_server(dir.path()).await;
        let (mut client, _) = signed_in(&server, addr).await;
        let state = Arc::clone(server.state());
        wait_for_subscriber(&state).await;
        let sessions = serde_json::json!([{
            "id": "s1",
            "firstPrompt": "secret prompt",
            "latestMessage": "secret latest",
            "lastAssistantMessage": "secret reply",
            "canOpen": true,
        }]);
        state.sessions_tx.send(sessions.to_string()).unwrap();
        match next_event(&mut client).await {
            Event::Text(v) => {
                assert_eq!(v["type"], "sessionsUpdated");
                assert!(!v.to_string().contains("secret"), "{v}");
                assert_eq!(v["data"][0]["canOpen"], false);
            }
            other => panic!("{other:?}"),
        }
        let notification = serde_json::json!({
            "title": "secret prompt",
            "redactedTitle": "proj",
            "body": "proj: Finished working",
        });
        state
            .notifications_tx
            .send(notification.to_string())
            .unwrap();
        match next_event(&mut client).await {
            Event::Text(v) => {
                assert_eq!(v["type"], "notification");
                assert_eq!(v["data"]["title"], "proj");
                assert!(!v.to_string().contains("secret"), "{v}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pairing_code_expiry_is_five_minutes_ahead_in_unix_ms() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(dir.path());
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let code = server.create_pairing_code().unwrap();
        let ttl = crate::ws_auth::PAIRING_TTL.as_millis() as u64;
        assert!(code.expires_at >= now_ms + ttl - 1000, "{code:?}");
        assert!(code.expires_at <= now_ms + ttl + 1000, "{code:?}");
        assert_eq!(code.code.len(), crate::ws_auth::PAIRING_CODE_LEN);
    }
}
