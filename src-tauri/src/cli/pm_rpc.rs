use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

// ── Request types ─────────────────────────────────────────────────────

/// All RPC operations the CLI can send to the PM daemon.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum RpcRequest {
    #[serde(rename = "spawn")]
    Spawn {
        cwd: String,
        name: Option<String>,
        append_system_prompt: Option<String>,
        permission_mode: String,
        model: Option<String>,
        add_dirs: Vec<String>,
        #[serde(rename = "spawnedBy")]
        spawned_by: Option<String>,
        #[serde(rename = "pmPid", default)]
        pm_pid: Option<u32>,
        #[serde(rename = "initialPrompt", default)]
        initial_prompt: Option<String>,
    },
    #[serde(rename = "send")]
    Send {
        session_id: String,
        text: String,
        wait: bool,
        timeout_ms: u64,
    },
    #[serde(rename = "list")]
    List,
    #[serde(rename = "stop")]
    Stop { session_id: String },
    #[serde(rename = "shutdown")]
    Shutdown,
    #[serde(rename = "adopt")]
    Adopt {
        session_id: String,
        force: bool,
        #[serde(rename = "callerPmSessionId")]
        caller_pm_session_id: String,
        #[serde(rename = "callerPmPid", default)]
        caller_pm_pid: Option<u32>,
    },
    #[serde(rename = "workersAll")]
    WorkersAll {
        #[serde(rename = "callerPmSessionId", default)]
        caller_pm_session_id: Option<String>,
        #[serde(rename = "callerPmPid", default)]
        caller_pm_pid: Option<u32>,
    },
    #[serde(rename = "inboxRead")]
    InboxRead {
        #[serde(rename = "callerPmSessionId")]
        caller_pm_session_id: String,
        #[serde(rename = "callerPmPid", default)]
        caller_pm_pid: Option<u32>,
        consume: bool,
        clear: bool,
        #[serde(rename = "workerId", default)]
        worker_id: Option<String>,
    },
}

// ── Client function ───────────────────────────────────────────────────

/// Sentinel string returned as the error message when the daemon socket is
/// unreachable (not running or not yet started).
pub const DAEMON_UNREACHABLE: &str = "DAEMON_UNREACHABLE";

/// Send an RPC request to the PM daemon over a Unix socket and return the
/// JSON response value. The caller inspects `response["ok"]` to distinguish
/// success (`true`) from error (`false` or absent).
///
/// * `sock_path`  – path to the daemon's Unix socket
/// * `request`    – the RPC operation to invoke
/// * `timeout`    – read timeout; write timeout is always 5 s
///
/// # Errors
///
/// Returns `Err(DAEMON_UNREACHABLE)` if the socket does not exist or the
/// connection is refused. Other errors describe what went wrong (serialization,
/// I/O, parse failures, etc.).
pub fn rpc_call(
    sock_path: &Path,
    request: &RpcRequest,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    // Connect
    let stream = UnixStream::connect(sock_path).map_err(|_| DAEMON_UNREACHABLE.to_string())?;

    // Set timeouts
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("Failed to set write timeout: {}", e))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("Failed to set read timeout: {}", e))?;

    // Serialize request as a single JSON line
    let mut line = serde_json::to_string(request)
        .map_err(|e| format!("Failed to serialize request: {}", e))?;
    line.push('\n');

    // Write
    let mut writer = stream
        .try_clone()
        .map_err(|e| format!("Failed to clone socket for writing: {}", e))?;
    writer
        .write_all(line.as_bytes())
        .map_err(|e| format!("Failed to write request: {}", e))?;

    // Read one response line
    let reader = BufReader::new(stream);
    let mut response_line = String::new();
    reader
        .lines()
        .next()
        .ok_or_else(|| "Daemon closed connection without sending a response".to_string())?
        .map_err(|e| format!("Failed to read response: {}", e))?
        .clone_into(&mut response_line);

    // Parse response
    serde_json::from_str::<serde_json::Value>(&response_line)
        .map_err(|e| format!("Failed to parse daemon response: {}", e))
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spawn_request_serializes() {
        let req = RpcRequest::Spawn {
            cwd: "/Users/me/project".to_string(),
            name: Some("worker-1".to_string()),
            append_system_prompt: None,
            permission_mode: "default".to_string(),
            model: None,
            add_dirs: vec![],
            spawned_by: Some("pm-session-abc".to_string()),
            pm_pid: Some(42),
            initial_prompt: None,
        };
        let json = serde_json::to_string(&req).expect("serialize should succeed");
        assert!(
            json.contains("\"op\":\"spawn\""),
            "should contain op:spawn, got: {}",
            json
        );
        assert!(
            json.contains("/Users/me/project"),
            "should contain cwd, got: {}",
            json
        );
        assert!(
            json.contains("\"spawnedBy\":\"pm-session-abc\""),
            "should contain spawnedBy, got: {}",
            json
        );
        assert!(
            json.contains("\"pmPid\":42"),
            "should contain pmPid, got: {}",
            json
        );
    }

    #[test]
    fn test_send_request_serializes() {
        let req = RpcRequest::Send {
            session_id: "abc-123".to_string(),
            text: "Hello, worker!".to_string(),
            wait: true,
            timeout_ms: 30_000,
        };
        let json = serde_json::to_string(&req).expect("serialize should succeed");
        assert!(
            json.contains("\"op\":\"send\""),
            "should contain op:send, got: {}",
            json
        );
        assert!(
            json.contains("\"wait\":true"),
            "should contain wait:true, got: {}",
            json
        );
    }

    #[test]
    fn test_list_request_serializes() {
        let req = RpcRequest::List;
        let json = serde_json::to_string(&req).expect("serialize should succeed");
        assert!(
            json.contains("\"op\":\"list\""),
            "should contain op:list, got: {}",
            json
        );
    }

    #[test]
    fn test_adopt_request_serializes() {
        let req = RpcRequest::Adopt {
            session_id: "w-1".to_string(),
            force: true,
            caller_pm_session_id: "pm-1".to_string(),
            caller_pm_pid: Some(100),
        };
        let json = serde_json::to_string(&req).expect("serialize should succeed");
        assert!(json.contains("\"op\":\"adopt\""), "got: {}", json);
        assert!(json.contains("callerPmSessionId"), "got: {}", json);
    }

    #[test]
    fn test_workers_all_request_serializes() {
        let req = RpcRequest::WorkersAll {
            caller_pm_session_id: Some("pm-1".to_string()),
            caller_pm_pid: None,
        };
        let json = serde_json::to_string(&req).expect("serialize should succeed");
        assert!(json.contains("\"op\":\"workersAll\""), "got: {}", json);
    }

    #[test]
    fn test_inbox_read_request_serializes() {
        let req = RpcRequest::InboxRead {
            caller_pm_session_id: "pm-1".to_string(),
            caller_pm_pid: None,
            consume: false,
            clear: false,
            worker_id: None,
        };
        let json = serde_json::to_string(&req).expect("serialize should succeed");
        assert!(json.contains("\"op\":\"inboxRead\""), "got: {}", json);
    }

    #[test]
    fn test_stop_request_serializes() {
        let req = RpcRequest::Stop {
            session_id: "dead-beef-1234".to_string(),
        };
        let json = serde_json::to_string(&req).expect("serialize should succeed");
        assert!(
            json.contains("\"op\":\"stop\""),
            "should contain op:stop, got: {}",
            json
        );
        assert!(
            json.contains("dead-beef-1234"),
            "should contain session_id, got: {}",
            json
        );
    }
}
