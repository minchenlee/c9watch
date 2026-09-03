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
    pub progress_tx: broadcast::Sender<String>,
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
        #[serde(default)]
        provider: Option<crate::session::SessionProvider>,
        #[serde(default, rename = "includeTools")]
        include_tools: bool,
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
        #[serde(default)]
        provider: Option<crate::session::SessionProvider>,
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

    #[serde(rename = "conversationProgress")]
    ConversationProgress { data: serde_json::Value },

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
    let mut progress_rx = state.progress_tx.subscribe();

    let mut inflight: Option<tokio::task::JoinHandle<ServerMsg>> = None;

    loop {
        tokio::select! {
            // Incoming client message
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let text_str: &str = &text;
                        match serde_json::from_str::<ClientMsg>(text_str) {
                            Ok(ClientMsg::GetConversation {
                                session_id,
                                provider,
                                include_tools,
                            }) => {
                                if let Some(previous) = inflight.take() {
                                    previous.abort();
                                }
                                let progress_tx = state.progress_tx.clone();
                                inflight = Some(tokio::spawn(async move {
                                    load_conversation_response(
                                        session_id,
                                        provider,
                                        include_tools,
                                        progress_tx,
                                    )
                                    .await
                                }));
                            }
                            Ok(client_msg) => {
                                let response = handle_message(client_msg).await;
                                let json = serde_json::to_string(&response).unwrap_or_default();
                                if socket.send(Message::Text(json)).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                let response = ServerMsg::Error {
                                    message: format!("Invalid message: {}", e),
                                };
                                let json = serde_json::to_string(&response).unwrap_or_default();
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
                let json = serde_json::to_string(&response).unwrap_or_default();
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

    crate::debug_log::log_info("[ws-server] Client disconnected");
}

// ── Message dispatch ────────────────────────────────────────────────

async fn load_conversation_response(
    session_id: String,
    provider: Option<crate::session::SessionProvider>,
    include_tools: bool,
    progress_tx: broadcast::Sender<String>,
) -> ServerMsg {
    let emit_id = session_id.clone();
    match tokio::task::spawn_blocking(move || {
        crate::get_conversation_data_for_provider_with_progress(
            &session_id,
            provider,
            include_tools,
            &mut |bytes_read, bytes_total| {
                let payload = serde_json::json!({
                    "sessionId": emit_id,
                    "provider": provider,
                    "bytesRead": bytes_read,
                    "bytesTotal": bytes_total,
                });
                let _ = progress_tx.send(payload.to_string());
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

async fn handle_message(msg: ClientMsg) -> ServerMsg {
    handle_message_with_owners(msg, crate::session::global_provider_source_owners()).await
}

async fn handle_message_with_owners(
    msg: ClientMsg,
    owners: crate::session::ProviderSourceOwners,
) -> ServerMsg {
    match msg {
        ClientMsg::GetSessions => match crate::polling::detect_and_enrich_sessions() {
            Ok(sessions) => ServerMsg::Sessions {
                data: serde_json::to_value(&sessions).unwrap_or_default(),
            },
            Err(e) => ServerMsg::Error { message: e },
        },

        ClientMsg::GetConversation {
            session_id,
            provider,
            include_tools,
        } => {
            load_conversation_response(session_id, provider, include_tools, {
                let (tx, _) = broadcast::channel(1);
                tx
            })
            .await
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
            provider,
        } => {
            match crate::run_validated_rename(provider, &session_id, &owners, || {
                // Write to Claude Code's native JSONL format
                crate::write_native_custom_title(&session_id, &new_name);
                // Also write to c9watch's own custom titles (fallback)
                let mut custom_titles = crate::session::CustomTitles::load();
                custom_titles.set(session_id.clone(), new_name.clone());
                custom_titles.save()
            }) {
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

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION_ID: &str = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee";

    fn synthetic_cursor_owner(temp: &tempfile::TempDir) -> crate::session::ProviderSourceOwners {
        let transcript_dir = temp
            .path()
            .join("synthetic-project")
            .join("agent-transcripts")
            .join(SESSION_ID);
        std::fs::create_dir_all(&transcript_dir).unwrap();
        std::fs::write(
            transcript_dir.join(format!("{SESSION_ID}.jsonl")),
            b"{\"role\":\"user\",\"message\":{\"content\":[]}}\n",
        )
        .unwrap();

        crate::session::ProviderSourceOwners::from_test_sources(
            None,
            Some(crate::session::cursor::CursorSessionSource::at_root(
                temp.path().to_path_buf(),
            )),
            None,
        )
    }

    fn synthetic_codex_owner(temp: &tempfile::TempDir) -> crate::session::ProviderSourceOwners {
        let rollout_dir = temp.path().join("2026").join("08").join("24");
        std::fs::create_dir_all(&rollout_dir).unwrap();
        std::fs::write(
            rollout_dir.join(format!("rollout-synthetic-{SESSION_ID}.jsonl")),
            b"{}\n",
        )
        .unwrap();

        crate::session::ProviderSourceOwners::from_test_sources(
            Some(crate::session::codex::CodexSessionSource::at_root(
                temp.path().to_path_buf(),
            )),
            None,
            None,
        )
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
            synthetic_cursor_owner(&cursor_temp),
            synthetic_codex_owner(&codex_temp),
        ];

        for owners in owners {
            let response = handle_message_with_owners(
                ClientMsg::RenameSession {
                    session_id: SESSION_ID.to_string(),
                    new_name: "synthetic collision".to_string(),
                    provider: None,
                },
                owners,
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
