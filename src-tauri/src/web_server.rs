use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;

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
}

// ── Protocol types ──────────────────────────────────────────────────

/// Client → Server messages
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ClientMsg {
    #[serde(rename = "getSessions")]
    GetSessions,

    #[serde(rename = "getConversation")]
    GetConversation {
        #[serde(rename = "sessionId")]
        session_id: String,
    },

    #[serde(rename = "stopSession")]
    StopSession { pid: u32 },

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
    },

    #[serde(rename = "getMemoryFiles")]
    GetMemoryFiles,
}

/// Server → Client messages
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ServerMsg {
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

    #[serde(rename = "memoryFiles")]
    MemoryFiles { data: serde_json::Value },
}

// ── Server entrypoint ───────────────────────────────────────────────

/// Start the axum WebSocket server (call from tauri::async_runtime::spawn)
pub async fn start_server(state: Arc<WsState>) {
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health))
        .route("/info", get(info))
        .fallback(get(serve_static_fallback))
        .with_state(state);

    // [::] accepts both IPv4 and IPv6 (localhost can resolve to ::1)
    let addr = format!("[::]:{}", WS_PORT);
    crate::debug_log::log_info(&format!("[ws-server] Listening on {}", addr));

    match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => {
            if let Err(e) = axum::serve(listener, app).await {
                crate::debug_log::log_error(&format!("[ws-server] Error: {}", e));
            }
        }
        Err(e) => {
            crate::debug_log::log_error(&format!("[ws-server] Failed to bind {}: {}", addr, e));
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
) -> axum::response::Response {
    match &params.token {
        Some(token) if token == &state.auth_token => ws
            .on_upgrade(move |socket| handle_socket(socket, state))
            .into_response(),
        _ => (
            axum::http::StatusCode::UNAUTHORIZED,
            "Invalid or missing token",
        )
            .into_response(),
    }
}

async fn handle_socket(mut socket: WebSocket, state: Arc<WsState>) {
    crate::debug_log::log_info("[ws-server] Client connected");
    let mut sessions_rx = state.sessions_tx.subscribe();
    let mut notifications_rx = state.notifications_tx.subscribe();

    loop {
        tokio::select! {
            // Incoming client message
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let text_str: &str = &text;
                        let response = match serde_json::from_str::<ClientMsg>(text_str) {
                            Ok(client_msg) => handle_message(client_msg).await,
                            Err(e) => ServerMsg::Error {
                                message: format!("Invalid message: {}", e),
                            },
                        };
                        let json = serde_json::to_string(&response).unwrap_or_default();
                        if socket.send(Message::Text(json)).await.is_err() {
                            break;
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
        }
    }

    crate::debug_log::log_info("[ws-server] Client disconnected");
}

// ── Message dispatch ────────────────────────────────────────────────

async fn handle_message(msg: ClientMsg) -> ServerMsg {
    match msg {
        ClientMsg::GetSessions => match crate::polling::detect_and_enrich_sessions() {
            Ok(sessions) => ServerMsg::Sessions {
                data: serde_json::to_value(&sessions).unwrap_or_default(),
            },
            Err(e) => ServerMsg::Error { message: e },
        },

        ClientMsg::GetConversation { session_id } => {
            match crate::get_conversation_data(&session_id) {
                Ok(conv) => ServerMsg::Conversation {
                    data: serde_json::to_value(&conv).unwrap_or_default(),
                },
                Err(e) => ServerMsg::Error { message: e },
            }
        }

        ClientMsg::StopSession { pid } => match crate::actions::stop_session(pid) {
            Ok(()) => ServerMsg::Ok,
            Err(e) => ServerMsg::Error { message: e },
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
        } => {
            // Write to Claude Code's native JSONL format
            crate::write_native_custom_title(&session_id, &new_name);
            // Also write to c9watch's own custom titles (fallback)
            let mut custom_titles = crate::session::CustomTitles::load();
            custom_titles.set(session_id, new_name);
            match custom_titles.save() {
                Ok(()) => ServerMsg::Ok,
                Err(e) => ServerMsg::Error { message: e },
            }
        }

        ClientMsg::GetMemoryFiles => match crate::session::get_memory_files() {
            Ok(files) => ServerMsg::MemoryFiles {
                data: serde_json::to_value(&files).unwrap_or_default(),
            },
            Err(e) => ServerMsg::Error { message: e },
        },
    }
}
