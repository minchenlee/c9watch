use super::pm_fs;
use super::pm_rpc::{self, RpcRequest};
use super::pm_worker::SpawnArgs;
use std::process::{Command, Stdio};
use std::time::Duration;

// ── Daemon lifecycle ──────────────────────────────────────────────────

/// How long a client waits for the daemon to become ready. A new daemon may
/// first spend up to `LEGACY_RETIRE_BUDGET` shutting down a pre-lock daemon
/// (see `pm_daemon::start_listener`), so the client waits that long plus
/// time to bind.
const DAEMON_READY_TIMEOUT: Duration =
    Duration::from_secs(super::pm_daemon::LEGACY_RETIRE_BUDGET.as_secs() + 5);

/// Wait until the lock-holding daemon has marked itself ready and its socket
/// accepts a connection. A socket file alone is not proof of a listener (a
/// crashed daemon leaves one behind), and a connectable socket alone may
/// still be a pre-lock daemon that the new one is shutting down. With
/// `child`, stop early when the daemon we spawned has already exited.
fn wait_until_ready(
    lock_path: &std::path::Path,
    sock_path: &std::path::Path,
    mut child: Option<&mut std::process::Child>,
    what: &str,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + DAEMON_READY_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if pm_fs::daemon_ready_marked(lock_path)
            && std::os::unix::net::UnixStream::connect(sock_path).is_ok()
        {
            return Ok(());
        }
        if let Some(Ok(Some(status))) = child.as_mut().map(|c| c.try_wait()) {
            return Err(format!(
                "Daemon exited during startup ({}); see {:?}",
                status,
                pm_fs::daemon_log_path().unwrap_or_default()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "Daemon did not accept connections on {:?} within {} seconds ({})",
        sock_path,
        DAEMON_READY_TIMEOUT.as_secs(),
        what
    ))
}

/// Ensure the PM daemon is running. Starts it if not already alive.
///
/// Liveness is the daemon's lifetime flock on `daemon.lock`. A separate
/// exclusive flock on `daemon.pid` serializes concurrent callers so that
/// only one process ever forks the daemon. Clients never delete the socket:
/// only a daemon that holds `daemon.lock` may replace it.
pub fn ensure_daemon() -> Result<(), String> {
    use std::os::unix::io::AsRawFd;

    // Ensure directories exist (base dir 0700) before opening any file
    pm_fs::ensure_dirs()?;

    let pid_path = pm_fs::daemon_pid_path()?;
    let sock_path = pm_fs::daemon_sock_path()?;
    let lock_path = pm_fs::daemon_lock_path()?;

    // Fast path: a daemon holds the lifetime lock. It may still be binding.
    if pm_fs::daemon_lock_held(&lock_path)? {
        return wait_until_ready(&lock_path, &sock_path, None, "daemon is running");
    }

    // Open (or create) the pid file and take an exclusive advisory lock.
    // This serializes concurrent `c9watch spawn` calls so only one ever forks.
    let spawn_lock = pm_fs::open_private_file(
        &pid_path,
        std::fs::OpenOptions::new().create(true).write(true).read(true),
    )
    .map_err(|e| format!("Failed to open daemon.pid for locking: {}", e))?;

    let fd = spawn_lock.as_raw_fd();
    let lock_result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };

    if lock_result != 0 {
        // Another process holds the spawn lock — it is starting the daemon.
        drop(spawn_lock);
        return wait_until_ready(&lock_path, &sock_path, None, "waited for another process");
    }

    // Re-check under the spawn lock: another caller may have started the
    // daemon between our probe and taking the spawn lock. The probe has
    // already released its shared lock, so the daemon we spawn below can
    // take its exclusive one.
    if pm_fs::daemon_lock_held(&lock_path)? {
        return wait_until_ready(&lock_path, &sock_path, None, "daemon is running");
    }

    // Get current executable path
    let exe =
        std::env::current_exe().map_err(|e| format!("Failed to get current exe path: {}", e))?;

    // Open log file for append
    let log_path = pm_fs::daemon_log_path()?;
    let log_file_out =
        pm_fs::open_private_file(&log_path, std::fs::OpenOptions::new().create(true).append(true))?;

    let log_file_err = log_file_out
        .try_clone()
        .map_err(|e| format!("Failed to clone log file handle: {}", e))?;

    // Spawn the daemon process
    let mut child = Command::new(&exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file_out))
        .stderr(Stdio::from(log_file_err))
        .spawn()
        .map_err(|e| format!("Failed to spawn daemon process: {}", e))?;

    // The spawn lock is released when `spawn_lock` drops after we return; by
    // then the daemon holds `daemon.lock`, so other callers take the fast
    // path above.
    wait_until_ready(&lock_path, &sock_path, Some(&mut child), "spawned a new daemon")
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
    args: SpawnArgs,
    append_system_prompt_file: Option<String>,
    initial_prompt: Option<String>,
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
                path,
                meta.len(),
                PROMPT_FILE_MAX_BYTES
            ));
        }
        Some(
            std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read system prompt file {:?}: {}", path, e))?,
        )
    } else {
        args.append_system_prompt
    };

    let spawned_by = crate::cli::pm_caller::detect_caller_session_id_default();
    let pm_pid = crate::cli::pm_caller::detect_caller_pid_default();

    let request = RpcRequest::Spawn {
        cwd: args.cwd,
        name: args.name,
        append_system_prompt: prompt,
        permission_mode: args.permission_mode,
        model: args.model,
        add_dirs: args.add_dirs,
        spawned_by,
        pm_pid,
        initial_prompt,
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
        return Err("Specify at most one of --consume or --clear (they conflict)".to_string());
    }
    ensure_daemon()?;
    let caller_pm_session_id = crate::cli::pm_caller::detect_caller_session_id_default()
        .ok_or_else(|| "PM_SESSION_NOT_FOUND: run inside a Claude Code session".to_string())?;
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
        .ok_or_else(|| "PM_SESSION_NOT_FOUND: run inside a Claude Code session".to_string())?;
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
