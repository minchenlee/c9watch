use super::pm_fs;
use super::pm_rpc::{self, RpcRequest};
use std::process::{Command, Stdio};
use std::time::Duration;

// ── Daemon lifecycle ──────────────────────────────────────────────────

/// Ensure the PM daemon is running. Starts it if not already alive.
///
/// An exclusive advisory flock on `daemon.pid` serializes concurrent callers
/// so that only one process ever forks the daemon.
pub fn ensure_daemon() -> Result<(), String> {
    use std::os::unix::io::AsRawFd;

    // Ensure directories exist before opening the pid file
    pm_fs::ensure_dirs()?;

    let pid_path = pm_fs::daemon_pid_path()?;
    let sock_path = pm_fs::daemon_sock_path()?;

    // Open (or create) the pid file and take an exclusive advisory lock.
    // This serializes concurrent `c9watch spawn` calls so only one ever forks.
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .open(&pid_path)
        .map_err(|e| format!("Failed to open daemon.pid for locking: {}", e))?;

    let fd = lock_file.as_raw_fd();
    let lock_result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };

    if lock_result != 0 {
        // Another process holds the lock — it is starting the daemon.
        // Wait up to 3 s for the socket to appear, then return.
        drop(lock_file);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if sock_path.exists() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        return Err("Daemon failed to start within 3 seconds (waited for another process)".to_string());
    }

    // We hold the exclusive lock. Check if a healthy daemon is already running.
    if pid_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = content.trim().parse::<libc::pid_t>() {
                let alive = unsafe { libc::kill(pid, 0) } == 0;
                if alive && sock_path.exists() {
                    // Daemon is running — release lock and return
                    return Ok(());
                }
            }
        }
        // Stale PID file — clean up
        let _ = std::fs::remove_file(&sock_path);
    }

    // Get current executable path
    let exe = std::env::current_exe()
        .map_err(|e| format!("Failed to get current exe path: {}", e))?;

    // Open log file for append
    let log_path = pm_fs::daemon_log_path()?;
    let log_file_out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("Failed to open daemon log {:?}: {}", log_path, e))?;

    let log_file_err = log_file_out
        .try_clone()
        .map_err(|e| format!("Failed to clone log file handle: {}", e))?;

    // Spawn the daemon process
    Command::new(&exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file_out))
        .stderr(Stdio::from(log_file_err))
        .spawn()
        .map_err(|e| format!("Failed to spawn daemon process: {}", e))?;

    // Wait up to 3 seconds for the socket to appear.
    // The flock is released when `lock_file` drops at the end of this scope,
    // which happens after we return — that's fine because by then the daemon
    // has written its own PID and the socket exists, so other callers will
    // take the fast path above.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if sock_path.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    Err(format!(
        "Daemon did not start within 3 seconds (socket {:?} never appeared)",
        sock_path
    ))
}

// ── RPC helper ────────────────────────────────────────────────────────

fn daemon_rpc(request: &RpcRequest, timeout: Duration) -> Result<serde_json::Value, String> {
    let sock_path = pm_fs::daemon_sock_path()?;
    let response = pm_rpc::rpc_call(&sock_path, request, timeout)?;

    // If the daemon returned ok:false, print the error JSON and exit 1
    if response.get("ok").and_then(|v| v.as_bool()) == Some(false) {
        let err_code = response
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("UNKNOWN");
        let details = response
            .get("details")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if details.is_empty() {
            return Err(err_code.to_string());
        }
        return Err(format!("{}: {}", err_code, details));
    }

    Ok(response)
}

// ── Subcommand handlers ───────────────────────────────────────────────

pub fn cmd_spawn(
    cwd: Option<String>,
    name: Option<String>,
    append_system_prompt: Option<String>,
    append_system_prompt_file: Option<String>,
    permission_mode: String,
    model: Option<String>,
    add_dirs: Vec<String>,
    pretty: bool,
) -> Result<(), String> {
    ensure_daemon()?;

    // Resolve system prompt: file takes precedence if both provided.
    // Cap file size at 64KB — the prompt is passed as a CLI argv flag, and
    // platform ARG_MAX (~256KB on macOS, often smaller) would crash the spawn
    // if we loaded a multi-megabyte file. 64KB is well under any ARG_MAX and
    // still generous for a system prompt.
    const PROMPT_FILE_MAX_BYTES: u64 = 64 * 1024;
    let prompt = if let Some(path) = append_system_prompt_file {
        let meta = std::fs::metadata(&path)
            .map_err(|e| format!("Failed to stat system prompt file {:?}: {}", path, e))?;
        if meta.len() > PROMPT_FILE_MAX_BYTES {
            return Err(format!(
                "PROMPT_FILE_TOO_LARGE: {:?} is {} bytes (limit {} bytes)",
                path, meta.len(), PROMPT_FILE_MAX_BYTES
            ));
        }
        Some(
            std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read system prompt file {:?}: {}", path, e))?,
        )
    } else {
        append_system_prompt
    };

    // Resolve cwd: default to current directory
    let resolved_cwd = if let Some(c) = cwd {
        c
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Failed to get current directory: {}", e))?
            .to_string_lossy()
            .to_string()
    };

    let spawned_by = crate::cli::pm_caller::detect_caller_session_id_default();
    let pm_pid = crate::cli::pm_caller::detect_caller_pid_default();

    let request = RpcRequest::Spawn {
        cwd: resolved_cwd,
        name,
        append_system_prompt: prompt,
        permission_mode,
        model,
        add_dirs,
        spawned_by,
        pm_pid,
    };

    let response = daemon_rpc(&request, Duration::from_secs(30))?;
    crate::cli::print_json(&response, pretty)
}

pub fn cmd_send(
    session_id: String,
    message: Option<String>,
    stdin: bool,
    file: Option<String>,
    wait: bool,
    timeout: u64,
    pretty: bool,
) -> Result<(), String> {
    ensure_daemon()?;

    // Resolve text from exactly one source
    let text = if let Some(msg) = message {
        if stdin || file.is_some() {
            return Err("Specify exactly one of --message, --stdin, or --file".to_string());
        }
        msg
    } else if stdin {
        if file.is_some() {
            return Err("Specify exactly one of --message, --stdin, or --file".to_string());
        }
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("Failed to read from stdin: {}", e))?;
        buf
    } else if let Some(path) = file {
        std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read message file {:?}: {}", path, e))?
    } else {
        return Err("Specify one of --message, --stdin, or --file".to_string());
    };

    let request = RpcRequest::Send {
        session_id,
        text,
        wait,
        timeout_ms: timeout * 1000,
    };

    // RPC timeout: if waiting for completion, add 10s buffer; otherwise 10s
    let rpc_timeout = if wait {
        Duration::from_secs(timeout + 10)
    } else {
        Duration::from_secs(10)
    };

    let response = daemon_rpc(&request, rpc_timeout)?;
    crate::cli::print_json(&response, pretty)?;

    // Exit code 2 when --wait timed out: message was sent but the turn did not
    // complete within the requested timeout.
    if wait {
        if let Some(false) = response.get("turnCompleted").and_then(|v| v.as_bool()) {
            std::process::exit(2);
        }
    }

    Ok(())
}

pub fn cmd_stop(session_id: String, pretty: bool) -> Result<(), String> {
    ensure_daemon()?;
    let request = RpcRequest::Stop { session_id };
    let response = daemon_rpc(&request, Duration::from_secs(10))?;
    crate::cli::print_json(&response, pretty)
}

pub fn cmd_workers(all: bool, pretty: bool) -> Result<(), String> {
    ensure_daemon()?;

    let caller_pm_session_id = crate::cli::pm_caller::detect_caller_session_id_default();
    let caller_pm_pid = crate::cli::pm_caller::detect_caller_pid_default();

    let request = RpcRequest::WorkersAll {
        caller_pm_session_id: caller_pm_session_id.clone(),
        caller_pm_pid,
    };
    let mut response = daemon_rpc(&request, Duration::from_secs(10))?;

    if !all {
        if let Some(workers) = response.get("workers").and_then(|w| w.as_array()).cloned() {
            let mine: Vec<serde_json::Value> = workers
                .into_iter()
                .filter(|w| {
                    let alive = w.get("alive").and_then(|v| v.as_bool()).unwrap_or(false);
                    let status = w.get("status").and_then(|v| v.as_str()).unwrap_or("");
                    alive && status == "OWNED_BY_YOU"
                })
                .collect();
            if let Some(obj) = response.as_object_mut() {
                obj.insert("workers".to_string(), serde_json::json!(mine));
            }
        }
    }

    crate::cli::print_json(&response, pretty)
}

pub fn cmd_inbox(
    consume: bool,
    clear: bool,
    worker: Option<String>,
    pretty: bool,
) -> Result<(), String> {
    if consume && clear {
        return Err(
            "Specify at most one of --consume or --clear (they conflict)".to_string(),
        );
    }
    ensure_daemon()?;
    let caller_pm_session_id = crate::cli::pm_caller::detect_caller_session_id_default()
        .ok_or_else(|| {
            "PM_SESSION_NOT_FOUND: run inside a Claude Code session".to_string()
        })?;
    let caller_pm_pid = crate::cli::pm_caller::detect_caller_pid_default();

    let request = RpcRequest::InboxRead {
        caller_pm_session_id,
        caller_pm_pid,
        consume,
        clear,
        worker_id: worker,
    };
    let response = daemon_rpc(&request, Duration::from_secs(10))?;
    crate::cli::print_json(&response, pretty)
}

pub fn cmd_adopt(session_id: String, force: bool, pretty: bool) -> Result<(), String> {
    ensure_daemon()?;
    let caller_pm_session_id = crate::cli::pm_caller::detect_caller_session_id_default()
        .ok_or_else(|| {
            "PM_SESSION_NOT_FOUND: run inside a Claude Code session".to_string()
        })?;
    let caller_pm_pid = crate::cli::pm_caller::detect_caller_pid_default();

    let request = RpcRequest::Adopt {
        session_id,
        force,
        caller_pm_session_id,
        caller_pm_pid,
    };
    let response = daemon_rpc(&request, Duration::from_secs(10))?;
    crate::cli::print_json(&response, pretty)
}
