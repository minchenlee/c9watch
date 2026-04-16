use super::pm_fs;
use super::pm_rpc::{self, RpcRequest};
use std::process::{Command, Stdio};
use std::time::Duration;

// ── Daemon lifecycle ──────────────────────────────────────────────────

/// Ensure the PM daemon is running. Starts it if not already alive.
pub fn ensure_daemon() -> Result<(), String> {
    let pid_path = pm_fs::daemon_pid_path()?;
    let sock_path = pm_fs::daemon_sock_path()?;

    // If PID file exists, check if the process is still alive
    if pid_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = content.trim().parse::<libc::pid_t>() {
                let alive = unsafe { libc::kill(pid, 0) } == 0;
                if alive && sock_path.exists() {
                    // Daemon is running
                    return Ok(());
                }
            }
        }
        // Stale PID file — clean up
        let _ = std::fs::remove_file(&pid_path);
        let _ = std::fs::remove_file(&sock_path);
    }

    // Ensure directories exist
    pm_fs::ensure_dirs()?;

    // Get current executable path
    let exe = std::env::current_exe()
        .map_err(|e| format!("Failed to get current exe path: {}", e))?;

    // Open log file for append
    let log_path = pm_fs::daemon_log_path()?;
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("Failed to open daemon log {:?}: {}", log_path, e))?;

    let log_file2 = log_file
        .try_clone()
        .map_err(|e| format!("Failed to clone log file handle: {}", e))?;

    // Spawn the daemon process
    Command::new(&exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file2))
        .spawn()
        .map_err(|e| format!("Failed to spawn daemon process: {}", e))?;

    // Wait up to 3 seconds for the socket to appear
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
    pm_rpc::rpc_call(&sock_path, request, timeout)
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

    // Resolve system prompt: file takes precedence if both provided
    let prompt = if let Some(path) = append_system_prompt_file {
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

    let request = RpcRequest::Spawn {
        cwd: resolved_cwd,
        name,
        append_system_prompt: prompt,
        permission_mode,
        model,
        add_dirs,
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
    crate::cli::print_json(&response, pretty)
}

pub fn cmd_workers(all: bool, pretty: bool) -> Result<(), String> {
    ensure_daemon()?;

    let request = RpcRequest::List;
    let mut response = daemon_rpc(&request, Duration::from_secs(10))?;

    // Filter to alive workers unless --all is specified
    if !all {
        if let Some(workers) = response.get("workers").and_then(|w| w.as_array()).cloned() {
            let alive: Vec<serde_json::Value> = workers
                .into_iter()
                .filter(|w| {
                    w.get("alive")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .collect();
            if let Some(obj) = response.as_object_mut() {
                obj.insert("workers".to_string(), serde_json::json!(alive));
            }
        }
    }

    crate::cli::print_json(&response, pretty)
}
