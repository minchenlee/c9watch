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

/// Error code returned when the daemon socket fails a trust check: its
/// directory is not a 0700 dir owned by us, the socket is not ours, or the
/// process serving it runs as another uid. Callers must not fall back to
/// any other action on this error.
pub const DAEMON_UNTRUSTED: &str = "DAEMON_UNTRUSTED";

fn untrusted(reason: String) -> String {
    format!("{}: {}", DAEMON_UNTRUSTED, reason)
}

/// Before connecting: the socket's dir must be a 0700 dir owned by us and
/// the socket must be a socket owned by us. A missing socket is
/// `DAEMON_UNREACHABLE`, as before.
fn verify_socket_path(sock_path: &Path, me: u32) -> Result<(), String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let sock_md = match std::fs::symlink_metadata(sock_path) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(DAEMON_UNREACHABLE.to_string())
        }
        Err(e) => return Err(untrusted(format!("cannot stat {:?}: {}", sock_path, e))),
    };
    let dir = sock_path.parent().unwrap_or_else(|| Path::new("/"));
    let dir_md = std::fs::symlink_metadata(dir)
        .map_err(|e| untrusted(format!("cannot stat {:?}: {}", dir, e)))?;
    if !dir_md.is_dir() {
        return Err(untrusted(format!("{:?} is not a directory", dir)));
    }
    if dir_md.uid() != me {
        return Err(untrusted(format!(
            "{:?} is owned by uid {}",
            dir,
            dir_md.uid()
        )));
    }
    if dir_md.mode() & 0o777 != 0o700 {
        return Err(untrusted(format!(
            "{:?} is {:o}, not 0700",
            dir,
            dir_md.mode() & 0o777
        )));
    }
    if !sock_md.file_type().is_socket() {
        return Err(untrusted(format!("{:?} is not a socket", sock_path)));
    }
    if sock_md.uid() != me {
        return Err(untrusted(format!(
            "{:?} is owned by uid {}",
            sock_path,
            sock_md.uid()
        )));
    }
    Ok(())
}

/// Effective uid of the process on the other end of `stream`.
#[cfg(not(target_os = "linux"))]
pub(crate) fn peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
    use std::os::unix::io::AsRawFd;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(uid)
}

/// Effective uid of the process on the other end of `stream`.
#[cfg(target_os = "linux")]
pub(crate) fn peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
    use std::os::unix::io::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(cred.uid)
}

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
/// connection is refused, and `Err("DAEMON_UNTRUSTED: ...")` if the socket
/// path or the serving process fails the trust checks. Other errors describe
/// what went wrong (serialization, I/O, parse failures, etc.).
pub fn rpc_call(
    sock_path: &Path,
    request: &RpcRequest,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    // Check the path, connect, then check who is serving it
    let me = unsafe { libc::geteuid() };
    verify_socket_path(sock_path, me)?;
    let stream = UnixStream::connect(sock_path).map_err(|_| DAEMON_UNREACHABLE.to_string())?;
    match peer_uid(&stream) {
        Ok(uid) if uid == me => {}
        Ok(uid) => return Err(untrusted(format!("daemon runs as uid {}", uid))),
        Err(e) => return Err(untrusted(format!("cannot read daemon uid: {}", e))),
    }

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

    // ── C0h-07: daemon trust checks ──────────────────────────────────────

    fn private_tmpdir(tag: &str, mode: u32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("c9w-rpc-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(mode)).unwrap();
        d
    }

    /// A listener that answers one request with `{"ok":true}`.
    fn serve_once(sock: &Path) -> std::thread::JoinHandle<()> {
        let l = std::os::unix::net::UnixListener::bind(sock).unwrap();
        std::thread::spawn(move || {
            if let Ok((c, _)) = l.accept() {
                let mut line = String::new();
                let _ = BufReader::new(&c).read_line(&mut line);
                let _ = (&c).write_all(b"{\"ok\":true}\n");
            }
        })
    }

    #[test]
    fn client_rejects_daemon_in_a_0755_dir() {
        let d = private_tmpdir("wide", 0o755);
        let sock = d.join("daemon.sock");
        let _h = serve_once(&sock);
        let err = rpc_call(&sock, &RpcRequest::List, Duration::from_secs(2)).unwrap_err();
        assert!(err.starts_with(DAEMON_UNTRUSTED), "got: {}", err);
        // Unblock the listener thread.
        let _ = UnixStream::connect(&sock);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn client_accepts_own_daemon_in_a_0700_dir() {
        let d = private_tmpdir("ok", 0o700);
        let sock = d.join("daemon.sock");
        let h = serve_once(&sock);
        let resp = rpc_call(&sock, &RpcRequest::List, Duration::from_secs(2)).unwrap();
        assert_eq!(resp["ok"], true);
        h.join().unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn client_reports_missing_socket_as_unreachable() {
        let d = private_tmpdir("missing", 0o700);
        let err = rpc_call(
            &d.join("daemon.sock"),
            &RpcRequest::List,
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert_eq!(err, DAEMON_UNREACHABLE);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn client_peer_uid_is_ours_on_a_socket_pair() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert_eq!(peer_uid(&a).unwrap(), unsafe { libc::geteuid() });
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
