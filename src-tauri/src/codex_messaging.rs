//! Control only threads already loaded by the local Codex daemon.
//! Never start a daemon, resume a transcript, or change execution permissions.
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tokio::net::UnixStream;
use tokio_tungstenite::{
    client_async_with_config,
    tungstenite::{protocol::WebSocketConfig, Message},
    WebSocketStream,
};

const MAX_TEXT: usize = 32 * 1024;
const UNAVAILABLE: &str = "This session is not connected to the local Codex CLI daemon. For Desktop sessions, launch Codex with c9watch support and open the conversation there.";
const NOT_LOADED: &str = "Connected to Codex, but this task is not loaded by the connected server. Open this task in Codex, then recheck. Viewing its history in c9watch does not load it in Codex.";
type Socket = WebSocketStream<UnixStream>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    available: bool,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    status: &'static str,
    detail: String,
    turn_id: Option<String>,
}

fn socket_path() -> Result<PathBuf, String> {
    // Match the archive reader's default home. No caller-supplied socket path.
    dirs::home_dir()
        .map(|p| p.join(".codex/app-server-control/app-server-control.sock"))
        .ok_or_else(|| "Home directory unavailable".into())
}

fn validate_id(id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| "Invalid Codex session UUID".into())
}

async fn rpc(ws: &mut Socket, id: u64, method: &str, params: Value) -> Result<Value, String> {
    ws.send(Message::Text(
        json!({"id":id,"method":method,"params":params}).to_string(),
    ))
    .await
    .map_err(|_| "Codex connection closed".to_string())?;
    while let Some(frame) = ws.next().await {
        let frame = frame.map_err(|_| "Codex connection failed".to_string())?;
        if let Message::Text(text) = frame {
            let value: Value =
                serde_json::from_str(&text).map_err(|_| "Invalid Codex response".to_string())?;
            if value.get("id") == Some(&json!(id)) {
                // Keep the RPC envelope so a definite rejection differs from lost acknowledgement.
                return Ok(value);
            }
        }
    }
    Err("Codex disconnected before acknowledging the request".into())
}

fn result(value: Value) -> Result<Value, String> {
    value
        .get("result")
        .cloned()
        .ok_or_else(|| "Codex does not support this request or rejected it".into())
}

async fn connect(path: &Path) -> Result<Socket, String> {
    let stream = UnixStream::connect(path)
        .await
        .map_err(|_| "Local Codex CLI daemon is unavailable".to_string())?;
    let config = WebSocketConfig {
        max_message_size: Some(8 * 1024 * 1024),
        max_frame_size: Some(8 * 1024 * 1024),
        ..Default::default()
    };
    let (mut ws, _) = client_async_with_config("ws://localhost/", stream, Some(config))
        .await
        .map_err(|_| "Cannot connect to the local Codex CLI daemon".to_string())?;
    result(
        rpc(
            &mut ws,
            1,
            "initialize",
            json!({"clientInfo":{"name":"c9watch","version":"0.10.0"}}),
        )
        .await?,
    )?;
    ws.send(Message::Text(json!({"method":"initialized"}).to_string()))
        .await
        .map_err(|_| "Codex disconnected".to_string())?;
    Ok(ws)
}

async fn require_loaded(ws: &mut Socket, session_id: &str) -> Result<(), String> {
    let mut cursor = Value::Null;
    // Bound discovery even if a server misbehaves or has very many threads.
    for page in 0..32 {
        let data = result(
            rpc(
                ws,
                10 + page,
                "thread/loaded/list",
                json!({"limit":100,"cursor":cursor}),
            )
            .await?,
        )?;
        if data["data"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(session_id)))
        {
            return Ok(());
        }
        cursor = data["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    Err(NOT_LOADED.into())
}

async fn resolve_session(session_id: &str) -> Result<Socket, String> {
    // Bound probes across composer windows as well as concurrent send preparation.
    static PROBES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
    let _permit = PROBES.try_acquire().map_err(|_| "Codex connection checks are busy. Recheck shortly.".to_string())?;
    let mut paths = crate::codex_bridge::endpoints();
    if paths.len() > 8 {
        return Err(
            "Too many Desktop bridge endpoints. Quit unused Codex instances and recheck.".into(),
        );
    }
    paths.push(socket_path()?);
    resolve_paths(paths, session_id).await
}

async fn resolve_paths(paths: Vec<PathBuf>, session_id: &str) -> Result<Socket, String> {
    let checks = paths.into_iter().map(|path| async move {
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut ws = connect(&path).await?;
            require_loaded(&mut ws, session_id).await?;
            Ok::<_, String>(ws)
        })
        .await
    });
    let mut matches = vec![];
    let mut task_not_loaded = false;
    for result in futures_util::future::join_all(checks).await {
        match result {
            Ok(Ok(ws)) => matches.push(ws),
            Ok(Err(error)) if error == NOT_LOADED => task_not_loaded = true,
            _ => {}
        }
    }
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(if task_not_loaded { NOT_LOADED } else { UNAVAILABLE }.into()),
        _ => Err("This session is loaded by more than one Codex server. Close the duplicate session before sending.".into()),
    }
}

#[tauri::command]
pub async fn codex_message_capability(session_id: String) -> Capability {
    let reason = match validate_id(&session_id) {
        Err(e) => Some(e),
        Ok(()) => resolve_session(&session_id).await.err(),
    };
    Capability {
        available: reason.is_none(),
        reason,
    }
}

// Reject concurrent sends, including those from a second c9watch window.
static SENDING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
struct Sending(String);
impl Drop for Sending {
    fn drop(&mut self) {
        if let Ok(mut set) = SENDING.get_or_init(Default::default).lock() {
            set.remove(&self.0);
        }
    }
}

#[cfg(test)]
async fn send_at(path: &Path, session_id: &str, text: &str) -> Result<Receipt, String> {
    let mut ws = tokio::time::timeout(Duration::from_secs(5), async {
        let mut ws = connect(path).await?;
        require_loaded(&mut ws, session_id).await?;
        Ok::<_, String>(ws)
    })
    .await
    .map_err(|_| "Codex connection check timed out; message was not sent".to_string())??;

    send_connected(&mut ws, session_id, text).await
}

async fn send_connected(ws: &mut Socket, session_id: &str, text: &str) -> Result<Receipt, String> {
    send_input(ws, session_id, message_input(text, &[])?, None).await
}

fn message_input(text: &str, images: &[String]) -> Result<Vec<Value>, String> {
    if text.len() > MAX_TEXT || (text.trim().is_empty() && images.is_empty()) || images.len() > 4 {
        return Err("Enter text (up to 32 KiB) or attach up to four images".into());
    }
    let mut input = Vec::new();
    if !text.trim().is_empty() {
        input.push(json!({"type":"text","text":text}));
    }
    let mut total = 0;
    for url in images {
        if url.len() > 6 * 1024 * 1024 {
            return Err("Images exceed 4 MiB total".into());
        }
        let (header, encoded) = url.split_once(',').ok_or("Invalid image")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| "Invalid image encoding")?;
        total += bytes.len();
        if total > 4 * 1024 * 1024 {
            return Err("Images exceed 4 MiB total".into());
        }
        let mime = crate::codex_images::validate(&bytes)?;
        if header != format!("data:{mime};base64") { return Err("Image type does not match its contents".into()); }
        input.push(json!({"type":"image","url":url}));
    }
    Ok(input)
}

#[tauri::command]
pub async fn validate_codex_images(images: Vec<String>) -> Result<(), String> {
    crate::run_session_scan(move || message_input("", &images).map(|_| ())).await
}

async fn send_input(
    ws: &mut Socket,
    session_id: &str,
    input: Vec<Value>,
    expected_turn_id: Option<&str>,
) -> Result<Receipt, String> {
    // A composer observing an active turn must not accidentally start a successor
    // if that turn finishes before delivery. A rejected steer is never retried as start.
    let (method, params) = match expected_turn_id {
        Some(turn) => ("turn/steer", json!({"threadId":session_id,"input":input,"expectedTurnId":turn})),
        None => ("turn/start", json!({"threadId":session_id,"input":input})),
    };
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        rpc(
            ws,
            100,
            method,
            params,
        ),
    )
    .await;
    Ok(match response {
        Ok(Ok(v)) if v.get("error").is_some() => Receipt { status: "rejected", detail: "Codex rejected the message. Check the original session before retrying.".into(), turn_id: None },
        Ok(Ok(v)) if expected_turn_id.is_some() && v["result"]["turnId"].as_str() == expected_turn_id => Receipt { status: "accepted", detail: "Accepted by the current Codex turn. Follow the reply in the original session.".into(), turn_id: expected_turn_id.map(str::to_owned) },
        Ok(Ok(v)) if expected_turn_id.is_none() && v["result"]["turn"]["id"].is_string() => Receipt { status: "accepted", detail: "Accepted by Codex. Follow the reply and any approval requests in the original session.".into(), turn_id: v["result"]["turn"]["id"].as_str().map(str::to_owned) },
        _ => Receipt { status: "unknown", detail: "Delivery could not be confirmed. Check the original session before sending again; c9watch will not retry automatically.".into(), turn_id: None },
    })
}

#[tauri::command]
pub async fn send_codex_message(
    session_id: String,
    text: String,
    images: Option<Vec<String>>,
    expected_turn_id: Option<String>,
) -> Result<Receipt, String> {
    // Only this preparation phase can produce a definite not-sent error.
    let prepared = async {
        validate_id(&session_id)?;
        if expected_turn_id.as_ref().is_some_and(|id| id.is_empty() || id.len() > 256) {
            return Err("Invalid expected turn identity".into());
        }
        {
            let mut set = SENDING
                .get_or_init(Default::default)
                .lock()
                .map_err(|_| "Messaging state unavailable")?;
            if set.len() >= 4 || !set.insert(session_id.clone()) {
                return Err("A message is already being sent. Wait for its result.".into());
            }
        }
        let _sending = Sending(session_id.clone());
        let input = crate::run_session_scan(move || message_input(&text, &images.unwrap_or_default())).await?;
        let ws = resolve_session(&session_id).await?;
        Ok::<_, String>((_sending, ws, input))
    }
    .await;
    let (_sending, mut ws, input) = match prepared {
        Ok(value) => value,
        Err(detail) => {
            return Ok(Receipt {
                status: "not_sent",
                detail: format!("Message was not sent: {detail}"),
                turn_id: None,
            })
        }
    };
    send_input(&mut ws, &session_id, input, expected_turn_id.as_deref()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;
    const ID: &str = "01a06f70-a045-76b1-b4ef-962f65b95462";

    async fn scenario(loaded: bool, acknowledge: bool) -> Result<Receipt, String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut methods = Vec::new();
            while let Some(Ok(Message::Text(raw))) = ws.next().await {
                let v: Value = serde_json::from_str(&raw).unwrap();
                let method = v["method"].as_str().unwrap();
                methods.push(method.to_string());
                let result = match method {
                    "initialize" => json!({}),
                    "initialized" => continue,
                    "thread/loaded/list" => {
                        json!({"data":if loaded {vec![ID]} else {vec![]},"nextCursor":null})
                    }
                    "turn/start" => {
                        assert_eq!(
                            v["params"],
                            json!({"threadId":ID,"input":[{"type":"text","text":"多行\n`$()`"}]})
                        );
                        if !acknowledge {
                            break;
                        }
                        json!({"turn":{"id":"turn-test"}})
                    }
                    other => panic!("Unexpected mutation: {other}"),
                };
                ws.send(Message::Text(
                    json!({"id":v["id"],"result":result}).to_string(),
                ))
                .await
                .unwrap();
                if method == "turn/start" || (method == "thread/loaded/list" && !loaded) {
                    break;
                }
            }
            methods
        });
        let receipt = send_at(&path, ID, "多行\n`$()`").await;
        let methods = server.await.unwrap();
        assert_eq!(
            methods.iter().filter(|m| *m == "turn/start").count(),
            usize::from(loaded)
        );
        receipt
    }

    #[tokio::test]
    async fn validation_failure_is_definitely_not_sent() {
        for (id, text, images) in [
            ("invalid", "hello", None),
            (ID, "", None),
            (ID, "hello", Some(vec!["invalid image".into()])),
        ] {
            let receipt = send_codex_message(id.into(), text.into(), images, None)
                .await
                .unwrap();
            assert_eq!(receipt.status, "not_sent");
            assert!(receipt.turn_id.is_none());
        }
    }

    #[test]
    fn image_inputs_are_bounded_and_use_native_protocol() {
        let url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(include_bytes!("../icons/32x32.png"))
        );
        let input = message_input("", &[url.clone()]).unwrap();
        assert_eq!(input, vec![json!({"type":"image","url":url})]);
        assert_eq!(message_input("caption", &[url.clone()]).unwrap().len(), 2);
        assert!(message_input("", &vec![url; 5]).is_err());
        assert!(message_input("", &["https://example.com/image.png".into()]).is_err());
        assert!(message_input("", &["data:image/png;base64,YmFk".into()]).is_err());
        assert!(message_input("", &[]).is_err());
    }

    #[tokio::test]
    async fn duplicate_servers_are_rejected_before_sending() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = vec![];
        let mut handles = vec![];
        for name in ["one.sock", "two.sock"] {
            let path = dir.path().join(name);
            let listener = UnixListener::bind(&path).unwrap();
            paths.push(path);
            handles.push(tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(Message::Text(raw))) = ws.next().await {
                    let v: Value = serde_json::from_str(&raw).unwrap();
                    let result = match v["method"].as_str().unwrap() {
                        "initialize" => json!({}),
                        "initialized" => continue,
                        "thread/loaded/list" => json!({"data":[ID],"nextCursor":null}),
                        other => panic!("Must not mutate ambiguous sessions: {other}"),
                    };
                    ws.send(Message::Text(
                        json!({"id":v["id"],"result":result}).to_string(),
                    ))
                    .await
                    .unwrap();
                    if v["method"] == "thread/loaded/list" {
                        break;
                    }
                }
            }));
        }
        assert!(resolve_paths(paths, ID)
            .await
            .unwrap_err()
            .contains("more than one"));
        for handle in handles {
            handle.await.unwrap();
        }
    }

    #[tokio::test]
    async fn sends_only_to_loaded_thread_preserving_text() {
        assert_eq!(scenario(true, true).await.unwrap().status, "accepted");
    }
    #[tokio::test]
    async fn never_resumes_unloaded_thread() {
        assert!(scenario(false, true)
            .await
            .unwrap_err()
            .contains("not loaded"));
    }
    #[tokio::test]
    async fn lost_ack_is_unknown_and_not_retried() {
        assert_eq!(scenario(true, false).await.unwrap().status, "unknown");
    }

    #[tokio::test]
    async fn steering_checks_turn_identity_and_never_falls_back_to_start() {
        use tokio_tungstenite::tungstenite::protocol::Role;
        for (response, status) in [
            (json!({"result":{"turnId":"original"}}), "accepted"),
            (json!({"result":{"turnId":"different"}}), "unknown"),
            (json!({"error":{"code":-32600,"message":"turn changed"}}), "rejected"),
        ] {
            let (client, server) = UnixStream::pair().unwrap();
            let mut client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
            let mut server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
            let owner = tokio::spawn(async move {
                let request: Value = serde_json::from_str(&server.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
                assert_eq!(request["method"], "turn/steer");
                assert_eq!(request["params"], json!({"threadId":ID,"expectedTurnId":"original","input":[{"type":"text","text":"change"}]}));
                // An unrelated response must never settle this send.
                server.send(Message::Text(json!({"id":999,"result":{"turnId":"original"}}).to_string())).await.unwrap();
                let mut response = response;
                response["id"] = request["id"].clone();
                server.send(Message::Text(response.to_string())).await.unwrap();
                assert!(!matches!(server.next().await, Some(Ok(Message::Text(_)))), "send was retried");
            });
            let receipt = send_input(&mut client, ID, message_input("change", &[]).unwrap(), Some("original")).await.unwrap();
            assert_eq!(receipt.status, status);
            drop(client);
            owner.await.unwrap();
        }
    }

    #[tokio::test]
    async fn late_ack_times_out_once_and_closes_the_connection() {
        use tokio_tungstenite::tungstenite::protocol::Role;
        let (client, server) = UnixStream::pair().unwrap();
        let mut client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
        let owner = tokio::spawn(async move {
            let request = server.next().await.unwrap().unwrap();
            assert!(request.into_text().unwrap().contains("turn/start"));
            tokio::time::sleep(Duration::from_millis(10_100)).await;
            let _ = server.send(Message::Text(json!({"id":100,"result":{"turn":{"id":"late"}}}).to_string())).await;
            assert!(!matches!(server.next().await, Some(Ok(Message::Text(_)))), "unknown delivery was retried");
        });
        let started = std::time::Instant::now();
        let receipt = send_input(&mut client, ID, message_input("once", &[]).unwrap(), None).await.unwrap();
        assert_eq!(receipt.status, "unknown");
        assert!(started.elapsed() < Duration::from_secs(12));
        drop(client);
        owner.await.unwrap();
    }

    #[test]
    fn oversized_image_is_rejected_before_delivery() {
        let mut bytes = vec![0; 4 * 1024 * 1024 + 1];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let url = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes));
        assert!(message_input("", &[url]).unwrap_err().contains("4 MiB"));
    }
    #[test]
    fn rejects_names_and_provider_qualified_ids() {
        assert!(validate_id("codex:abc").is_err());
        assert!(validate_id("my task").is_err());
    }

    /// Opt-in integration check: idle and active turns in a fresh ephemeral,
    /// read-only session. Never touches an existing user conversation.
    #[tokio::test]
    #[ignore = "requires a running local Codex daemon and performs model turns"]
    async fn live_codex_same_session() {
        let path = socket_path().unwrap();
        let mut owner = connect(&path).await.unwrap();
        let created = result(rpc(&mut owner, 90, "thread/start", json!({
            "ephemeral": true, "cwd": "/tmp", "sandbox":"read-only", "model":"gpt-5.4-mini",
            "approvalPolicy":"never",
            "developerInstructions":"This is a messaging transport test. Never use tools. Reply with the requested literal text only."
        })).await.unwrap()).unwrap();
        let id = created["thread"]["id"].as_str().unwrap();
        for expected in ["C9WATCH_FIRST_OK", "C9WATCH_SECOND_OK"] {
            let receipt = send_at(
                &path,
                id,
                &format!("Reply exactly {expected}. Do not use tools."),
            )
            .await
            .unwrap();
            assert_eq!(receipt.status, "accepted", "{receipt:?}");
            let turn_id = receipt.turn_id.unwrap();
            let text = tokio::time::timeout(Duration::from_secs(60), async {
                let mut text = String::new();
                while let Some(Ok(Message::Text(raw))) = owner.next().await {
                    let v: Value = serde_json::from_str(&raw).unwrap();
                    if v["params"]["threadId"] != id {
                        continue;
                    }
                    if v["method"] == "item/agentMessage/delta" {
                        text.push_str(v["params"]["delta"].as_str().unwrap_or_default());
                    }
                    if v["method"] == "turn/completed" && v["params"]["turn"]["id"] == turn_id {
                        assert_eq!(v["params"]["turn"]["status"], "completed", "{v}");
                        return text;
                    }
                }
                panic!("Owner disconnected before completion");
            })
            .await
            .expect("No completion within 60 seconds");
            assert!(text.contains(expected), "Missing reply: {text}");
            println!("same-session reply verified: {expected}");
        }
        let first = send_at(
            &path,
            id,
            "Reply with three short sentences about testing. Do not use tools.",
        )
        .await
        .unwrap();
        assert_eq!(first.status, "accepted");
        let mut steering_socket = connect(&path).await.unwrap();
        require_loaded(&mut steering_socket, id).await.unwrap();
        let steered = send_input(
            &mut steering_socket,
            id,
            message_input("Also include the literal C9WATCH_STEER_OK in your answer. Do not use tools.", &[]).unwrap(),
            first.turn_id.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(steered.status, "accepted");
        assert_eq!(
            first.turn_id, steered.turn_id,
            "Expected to append to the active turn"
        );
        let text = tokio::time::timeout(Duration::from_secs(60), async {
            let mut text = String::new();
            while let Some(Ok(Message::Text(raw))) = owner.next().await {
                let v: Value = serde_json::from_str(&raw).unwrap();
                if v["params"]["threadId"] != id {
                    continue;
                }
                if v["method"] == "item/agentMessage/delta" {
                    text.push_str(v["params"]["delta"].as_str().unwrap_or_default());
                }
                if v["method"] == "turn/completed"
                    && v["params"]["turn"]["id"].as_str() == first.turn_id.as_deref()
                {
                    assert_eq!(v["params"]["turn"]["status"], "completed");
                    return text;
                }
            }
            panic!("Owner disconnected");
        })
        .await
        .unwrap();
        assert!(
            text.contains("C9WATCH_STEER_OK"),
            "Steering reply missing: {text}"
        );
        println!("active-turn steering verified");
    }
}
