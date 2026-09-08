//! Explicit OpenCode HTTP integration. No process, port or storage discovery.
//!
//! Configure C9WATCH_OPENCODE_URL to opt in. Detection only reads a background
//! snapshot; a slow/offline server never blocks the other providers.
use super::conversation::{Conversation, ConversationMessage};
use super::{
    AgentKind, DetectedSession, MessageType, SessionProvider, SessionStatus, SessionSurface,
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

const MAX_RESPONSE: u64 = 8 * 1024 * 1024;
const IDLE_FRESHNESS_MS: i64 = 30 * 60 * 1000;

#[derive(Clone, Debug)]
pub struct OpenCodeSummary {
    pub modified: String,
    pub status: SessionStatus,
    pub diagnostic: String,
}

#[derive(Clone)]
struct Connection {
    url: reqwest::Url,
    username: String,
    password: Option<String>,
}

impl Connection {
    fn from_env() -> Result<Option<Self>, String> {
        let Ok(raw) = std::env::var("C9WATCH_OPENCODE_URL") else {
            return Ok(None);
        };
        Self::parse(
            &raw,
            std::env::var("C9WATCH_OPENCODE_USERNAME").unwrap_or_else(|_| "opencode".into()),
            std::env::var("C9WATCH_OPENCODE_PASSWORD").ok(),
        )
        .map(Some)
    }

    fn parse(raw: &str, username: String, password: Option<String>) -> Result<Self, String> {
        let url = reqwest::Url::parse(raw).map_err(|_| "Invalid OpenCode URL")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "OpenCode URL must be HTTP(S), without credentials, query or fragment".into(),
            );
        }
        Ok(Self {
            url,
            username,
            password,
        })
    }

    fn get<T: serde::de::DeserializeOwned>(
        &self,
        client: &Client,
        segments: &[&str],
    ) -> Result<T, String> {
        self.get_page(client, segments, &[]).map(|(value, _)| value)
    }

    fn get_page<T: serde::de::DeserializeOwned>(
        &self,
        client: &Client,
        segments: &[&str],
        query: &[(&str, &str)],
    ) -> Result<(T, Option<String>), String> {
        let mut url = self.url.clone();
        url.path_segments_mut()
            .map_err(|_| "Invalid OpenCode URL")?
            .pop_if_empty()
            .extend(segments);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let mut request = client.get(url);
        if let Some(password) = &self.password {
            request = request.basic_auth(&self.username, Some(password));
        }
        let response = request
            .send()
            .map_err(|_| "OpenCode connection failed or timed out")?;
        if !response.status().is_success() {
            return Err(format!("OpenCode HTTP {}", response.status().as_u16()));
        }
        let next_cursor = response
            .headers()
            .get("x-next-cursor")
            .map(|v| v.to_str().map(str::to_owned))
            .transpose()
            .map_err(|_| "Invalid OpenCode pagination cursor")?;
        let mut bytes = Vec::new();
        response
            .take(MAX_RESPONSE + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "OpenCode response read failed")?;
        if bytes.len() as u64 > MAX_RESPONSE {
            return Err("OpenCode response exceeds 8 MiB".into());
        }
        serde_json::from_slice(&bytes)
            .map(|value| (value, next_cursor))
            .map_err(|_| "Malformed OpenCode response".into())
    }
}

fn client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|_| "Could not initialize OpenCode HTTP client".into())
}

#[derive(Deserialize)]
struct RemoteSession {
    id: String,
    directory: String,
    title: String,
    #[serde(rename = "parentID")]
    parent_id: Option<String>,
    time: RemoteTime,
}

#[derive(Deserialize)]
struct RemoteTime {
    created: i64,
    updated: i64,
    archived: Option<i64>,
}

#[derive(Deserialize)]
struct RemoteStatus {
    #[serde(rename = "type")]
    kind: String,
}

fn timestamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.to_rfc3339())
        .unwrap_or_default()
}

fn snapshot(connection: &Connection, client: &Client) -> Result<Vec<DetectedSession>, String> {
    let sessions: Vec<RemoteSession> = connection.get(client, &["session"])?;
    // Status is directory-scoped even though the session list spans the project.
    let mut statuses_by_directory = HashMap::new();
    for directory in sessions
        .iter()
        .filter(|s| s.time.archived.is_none())
        .map(|s| s.directory.as_str())
        .collect::<BTreeSet<_>>()
    {
        let (statuses, _): (HashMap<String, RemoteStatus>, _) =
            connection.get_page(client, &["session", "status"], &[("directory", directory)])?;
        statuses_by_directory.insert(directory.to_owned(), statuses);
    }
    Ok(sessions
        .into_iter()
        .filter(|s| s.time.archived.is_none())
        .filter(|s| {
            statuses_by_directory
                .get(&s.directory)
                .and_then(|statuses| statuses.get(&s.id))
                .is_some_and(|status| status.kind != "idle")
                || chrono::Utc::now()
                    .timestamp_millis()
                    .saturating_sub(s.time.updated)
                    <= IDLE_FRESHNESS_MS
        })
        .map(|s| {
            let mut detected = DetectedSession::with_legacy_defaults(
                0,
                s.directory.clone().into(),
                s.directory.clone().into(),
                Some(s.id.clone()),
                s.title.clone(),
            );
            detected.provider = SessionProvider::Opencode;
            detected.surface = SessionSurface::Integration;
            detected.official_name = Some(s.title);
            detected.started_at_ms = Some(s.time.created);
            detected.agent_kind = if s.parent_id.is_some() {
                AgentKind::Subagent
            } else {
                AgentKind::Root
            };
            detected.parent_thread_id = s.parent_id;
            detected.can_open = false;
            detected.can_stop = false;
            detected.can_rename = false;
            // Missing entries in the status map are idle; unknown future values
            // are explicitly Connecting, never guessed to mean completion.
            let kind = statuses_by_directory
                .get(&s.directory)
                .and_then(|statuses| statuses.get(&s.id))
                .map(|s| s.kind.as_str())
                .unwrap_or("idle");
            detected.opencode_summary = Some(OpenCodeSummary {
                modified: timestamp(s.time.updated),
                status: match kind {
                    "busy" => SessionStatus::Working,
                    "idle" => SessionStatus::WaitingForInput,
                    _ => SessionStatus::Connecting,
                },
                diagnostic: match kind {
                    "retry" => "OpenCode is retrying".into(),
                    "busy" | "idle" => String::new(),
                    _ => "OpenCode reported an unrecognized status".into(),
                },
            });
            detected
        })
        .collect())
}

struct Cache {
    started: bool,
    generation: u64,
    connection: Option<Connection>,
    sessions: Vec<DetectedSession>,
    error: Option<String>,
    connected: bool,
}
static CACHE: LazyLock<Arc<Mutex<Cache>>> = LazyLock::new(|| {
    let result = Connection::from_env();
    Arc::new(Mutex::new(Cache {
        started: false,
        generation: 0,
        sessions: Vec::new(),
        connected: false,
        error: result.as_ref().err().cloned(),
        connection: result.ok().flatten(),
    }))
});

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionStatus {
    url: String,
    connected: bool,
    error: Option<String>,
    session_count: usize,
}

#[cfg(feature = "gui")]
#[tauri::command]
pub fn opencode_connection_status() -> ConnectionStatus {
    detect();
    let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    ConnectionStatus {
        url: cache
            .connection
            .as_ref()
            .map(|c| c.url.to_string())
            .unwrap_or_default(),
        connected: cache.connected,
        error: cache.error.clone(),
        session_count: cache.sessions.len(),
    }
}

/// Credentials stay in process memory and are never returned to the UI or logged.
#[cfg(feature = "gui")]
#[tauri::command]
pub fn opencode_connect(
    url: String,
    username: String,
    password: Option<String>,
) -> Result<(), String> {
    let connection = if url.trim().is_empty() {
        None
    } else {
        Some(Connection::parse(
            url.trim(),
            username,
            password.filter(|p| !p.is_empty()),
        )?)
    };
    {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.connection = connection;
        cache.generation += 1;
        cache.sessions.clear();
        cache.error = None;
        cache.connected = false;
    }
    detect();
    Ok(())
}

pub(crate) fn detect() -> Vec<DetectedSession> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if !cache.started {
        cache.started = true;
        let shared = Arc::clone(&CACHE);
        std::thread::spawn(move || {
            let client = match client() {
                Ok(c) => c,
                Err(e) => {
                    shared.lock().unwrap_or_else(|e| e.into_inner()).error = Some(e);
                    return;
                }
            };
            loop {
                let (connection, generation) = {
                    let cache = shared.lock().unwrap_or_else(|e| e.into_inner());
                    (cache.connection.clone(), cache.generation)
                };
                if let Some(connection) = connection {
                    let result = snapshot(&connection, &client);
                    let mut cache = shared.lock().unwrap_or_else(|e| e.into_inner());
                    // A response from an old endpoint must never populate a new connection.
                    if cache.generation == generation {
                        apply_result(&mut cache, result);
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    }
    cache.sessions.clone()
}

fn apply_result(cache: &mut Cache, result: Result<Vec<DetectedSession>, String>) {
    match result {
        Ok(sessions) => {
            cache.sessions = sessions;
            cache.connected = true;
            cache.error = None;
        }
        Err(error) => {
            cache.connected = false;
            cache.error = Some(error.clone());
            for session in &mut cache.sessions {
                if let Some(summary) = &mut session.opencode_summary {
                    summary.status = SessionStatus::Connecting;
                    summary.diagnostic = error.clone();
                }
            }
        }
    }
}

pub(crate) fn detect_once() -> Vec<DetectedSession> {
    let connection = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .connection
        .clone();
    let Some(connection) = connection else {
        return Vec::new();
    };
    std::thread::spawn(move || client().and_then(|client| snapshot(&connection, &client)))
        .join()
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
}

pub(crate) fn conversation(id: &str, include_tools: bool) -> Result<Conversation, String> {
    let connection = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .connection
        .clone()
        .ok_or("OpenCode is not configured")?;
    read_conversation(&connection, &client()?, id, include_tools)
}

fn read_conversation(
    connection: &Connection,
    client: &Client,
    id: &str,
    include_tools: bool,
) -> Result<Conversation, String> {
    let mut pages = Vec::new();
    let mut before: Option<String> = None;
    let mut cursors = HashSet::new();
    let mut limit = 20usize;
    let started = std::time::Instant::now();
    loop {
        if pages.len() >= 1000 || started.elapsed() > Duration::from_secs(60) {
            return Err("OpenCode conversation is too large to load in one minute".into());
        }
        let limit_text = limit.to_string();
        let mut query = vec![("limit", limit_text.as_str())];
        if let Some(cursor) = &before {
            query.push(("before", cursor.as_str()));
        }
        let result = connection.get_page::<Vec<Value>>(client, &["session", id, "message"], &query);
        let (rows, next) = match result {
            Err(error) if error == "OpenCode response exceeds 8 MiB" && limit > 1 => {
                limit = (limit / 2).max(1);
                continue;
            }
            other => other?,
        };
        pages.push(parse_conversation(id, rows, include_tools)?.messages);
        match next {
            Some(cursor) if !cursor.is_empty() && cursors.insert(cursor.clone()) => {
                before = Some(cursor)
            }
            Some(_) => return Err("OpenCode returned a repeated or empty pagination cursor".into()),
            None => break,
        }
    }
    // OpenCode pages are individually chronological but are fetched newest first.
    Ok(Conversation {
        session_id: id.into(),
        provider: SessionProvider::Opencode,
        messages: pages.into_iter().rev().flatten().collect(),
    })
}

fn parse_conversation(
    id: &str,
    rows: Vec<Value>,
    include_tools: bool,
) -> Result<Conversation, String> {
    let mut messages = Vec::new();
    for row in rows {
        let role = row["info"]["role"]
            .as_str()
            .ok_or("Malformed OpenCode message role")?;
        let time = timestamp(row["info"]["time"]["created"].as_i64().unwrap_or_default());
        let parts = row["parts"]
            .as_array()
            .ok_or("Malformed OpenCode message parts")?;
        for part in parts {
            let (message_type, content) = match part["type"].as_str() {
                Some("text") if role == "user" || role == "assistant" => (
                    if role == "user" {
                        MessageType::User
                    } else {
                        MessageType::Assistant
                    },
                    part["text"].as_str().unwrap_or_default().to_owned(),
                ),
                Some("tool") if include_tools => (
                    MessageType::ToolResult,
                    format!(
                        "{}: {}",
                        part["tool"].as_str().unwrap_or("tool"),
                        part["state"]["output"]
                            .as_str()
                            .or(part["state"]["error"].as_str())
                            .unwrap_or("(pending)")
                    ),
                ),
                _ => continue,
            };
            messages.push(ConversationMessage {
                timestamp: time.clone(),
                message_type,
                content,
                images: Vec::new(),
            });
        }
    }
    Ok(Conversation {
        session_id: id.into(),
        provider: SessionProvider::Opencode,
        messages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    fn server(responses: Vec<(&str, u16, String)>) -> (Connection, std::thread::JoinHandle<()>) {
        server_with_headers(
            responses
                .into_iter()
                .map(|(p, c, b)| (p, c, b, ""))
                .collect(),
        )
    }

    fn server_with_headers(
        responses: Vec<(&str, u16, String, &str)>,
    ) -> (Connection, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let connection = Connection::parse(
            &format!("http://{}", listener.local_addr().unwrap()),
            "opencode".into(),
            Some("test-secret".into()),
        )
        .unwrap();
        let paths: Vec<_> = responses
            .into_iter()
            .map(|(p, s, b, h)| (p.to_owned(), s, b, h.to_owned()))
            .collect();
        let handle = std::thread::spawn(move || {
            for (path, code, body, headers) in paths {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line.trim(), format!("GET {path} HTTP/1.1"));
                let mut auth = false;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if line.to_lowercase().starts_with("authorization: basic ") {
                        auth = true;
                    }
                }
                assert!(auth);
                write!(
                    socket,
                    "HTTP/1.1 {code} Test\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (connection, handle)
    }

    #[test]
    fn http_snapshot_maps_lifecycle_hierarchy_and_capabilities() {
        let rows: Vec<_> = ["busy", "idle", "retry", "future", "archived", "old"].into_iter().map(|id| json!({
            "id": id, "directory": "/synthetic/project", "title": id,
            "parentID": if id == "retry" { Some("busy") } else { None },
            "time": {"created": 1000, "updated": if id == "old" { 0 } else { chrono::Utc::now().timestamp_millis() }, "archived": if id == "archived" { Some(3000) } else { None }}
        })).collect();
        let (connection, thread) = server(vec![
            ("/session", 200, json!(rows).to_string()),
            ("/session/status?directory=%2Fsynthetic%2Fproject", 200, json!({"busy":{"type":"busy"}, "retry":{"type":"retry"}, "future":{"type":"new-state"}}).to_string()),
        ]);
        let sessions = snapshot(&connection, &client().unwrap()).unwrap();
        thread.join().unwrap();
        assert_eq!(sessions.len(), 4);
        assert_eq!(sessions[0].identity_key().as_deref(), Some("opencode:busy"));
        assert_eq!(
            sessions[0].opencode_summary.as_ref().unwrap().status,
            SessionStatus::Working
        );
        assert_eq!(
            sessions[1].opencode_summary.as_ref().unwrap().status,
            SessionStatus::WaitingForInput
        );
        assert_eq!(
            sessions[2].opencode_summary.as_ref().unwrap().status,
            SessionStatus::Connecting
        );
        assert_eq!(
            sessions[3].opencode_summary.as_ref().unwrap().status,
            SessionStatus::Connecting
        );
        assert_eq!(sessions[2].parent_thread_id.as_deref(), Some("busy"));
        assert!(sessions
            .iter()
            .all(|s| s.pid == 0 && !s.can_open && !s.can_stop && !s.can_rename));
        let enriched = super::super::enrichment::enrich_detected_sessions(
            sessions.clone(),
            Default::default(),
        )
        .unwrap()
        .0;
        assert_eq!(enriched[0].provider, SessionProvider::Opencode);
        assert_eq!(enriched[0].status, SessionStatus::Working);
        let mut cache = Cache {
            started: true,
            generation: 0,
            connection: None,
            sessions,
            error: None,
            connected: true,
        };
        apply_result(&mut cache, Err("OpenCode HTTP 401".into()));
        assert!(!cache.connected);
        assert_eq!(cache.sessions.len(), 4);
        assert!(cache
            .sessions
            .iter()
            .all(|s| s.opencode_summary.as_ref().unwrap().status == SessionStatus::Connecting));
        apply_result(&mut cache, Ok(Vec::new()));
        assert!(cache.connected && cache.sessions.is_empty() && cache.error.is_none());
    }

    #[test]
    fn malformed_and_auth_errors_are_explicit_without_response_body_or_secret() {
        for (code, body, expected) in [
            (401, "test-secret", "OpenCode HTTP 401"),
            (200, "not json", "Malformed OpenCode response"),
            (302, "redirect", "OpenCode HTTP 302"),
        ] {
            let (connection, thread) = server(vec![("/session", code, body.into())]);
            let error = snapshot(&connection, &client().unwrap()).unwrap_err();
            thread.join().unwrap();
            assert_eq!(error, expected);
            assert!(!error.contains("test-secret"));
        }
    }

    #[test]
    fn conversations_preserve_roles_unicode_and_tool_visibility() {
        let rows = vec![
            json!({"info":{"role":"user","time":{"created":1000}},"parts":[{"type":"text","text":"你好"}]}),
            json!({"info":{"role":"assistant","time":{"created":2000}},"parts":[{"type":"text","text":"Hello"},
                {"type":"tool","tool":"read","state":{"status":"completed","output":"fixture output"}},
                {"type":"future-part","text":"ignored"}]}),
        ];
        let result = parse_conversation("same-id", rows.clone(), false).unwrap();
        assert_eq!(result.provider, SessionProvider::Opencode);
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].content, "你好");
        assert!(matches!(result.messages[0].message_type, MessageType::User));
        assert!(matches!(
            result.messages[1].message_type,
            MessageType::Assistant
        ));
        assert_eq!(
            parse_conversation("same-id", rows, true)
                .unwrap()
                .messages
                .len(),
            3
        );
        assert!(parse_conversation("id", vec![json!({})], true).is_err());
    }

    #[test]
    fn directory_statuses_do_not_turn_other_worktrees_idle() {
        let now = chrono::Utc::now().timestamp_millis();
        let rows = json!([
            {"id":"idle","directory":"/a","title":"A","time":{"created":0,"updated":now}},
            {"id":"busy","directory":"/b","title":"B","time":{"created":0,"updated":0}}
        ]);
        let (connection, thread) = server(vec![
            ("/session", 200, rows.to_string()),
            ("/session/status?directory=%2Fa", 200, "{}".into()),
            (
                "/session/status?directory=%2Fb",
                200,
                r#"{"busy":{"type":"busy"}}"#.into(),
            ),
        ]);
        let sessions = snapshot(&connection, &client().unwrap()).unwrap();
        thread.join().unwrap();
        assert_eq!(sessions.len(), 2, "old busy worktree must not expire");
        assert_eq!(
            sessions[0].opencode_summary.as_ref().unwrap().status,
            SessionStatus::WaitingForInput
        );
        assert_eq!(
            sessions[1].opencode_summary.as_ref().unwrap().status,
            SessionStatus::Working
        );
    }

    #[test]
    fn paginated_conversation_larger_than_response_cap_preserves_order() {
        let page = |text: &str| {
            json!([{
                "info":{"role":"assistant","time":{"created":1000}},
                "parts":[{"type":"text","text":text},
                    {"type":"tool","tool":"read","state":{"output":"x".repeat(3 * 1024 * 1024)}}]
            }])
            .to_string()
        };
        let (connection, thread) = server_with_headers(vec![
            (
                "/session/ses_test/message?limit=20",
                200,
                page("newest"),
                "X-Next-Cursor: c2\r\n",
            ),
            (
                "/session/ses_test/message?limit=20&before=c2",
                200,
                page("middle"),
                "X-Next-Cursor: c1\r\n",
            ),
            (
                "/session/ses_test/message?limit=20&before=c1",
                200,
                page("oldest"),
                "",
            ),
        ]);
        let conversation =
            read_conversation(&connection, &client().unwrap(), "ses_test", false).unwrap();
        thread.join().unwrap();
        assert_eq!(
            conversation
                .messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["oldest", "middle", "newest"]
        );
    }

    #[test]
    fn repeated_pagination_cursor_returns_error_instead_of_looping() {
        let (connection, thread) = server_with_headers(vec![
            (
                "/session/ses_test/message?limit=20",
                200,
                "[]".into(),
                "X-Next-Cursor: same\r\n",
            ),
            (
                "/session/ses_test/message?limit=20&before=same",
                200,
                "[]".into(),
                "X-Next-Cursor: same\r\n",
            ),
        ]);
        let error =
            read_conversation(&connection, &client().unwrap(), "ses_test", false).unwrap_err();
        thread.join().unwrap();
        assert!(error.contains("repeated"));
    }

    #[test]
    fn rejects_credential_urls_and_non_http_schemes() {
        for raw in [
            "file:///tmp/data",
            "http://user:secret@localhost:4096",
            "http://localhost/?secret=1",
            "http://localhost/#fragment",
        ] {
            assert!(Connection::parse(raw, "opencode".into(), None).is_err());
        }
        assert!(Connection::parse("http://127.0.0.1:4096", "opencode".into(), None).is_ok());
    }
}
