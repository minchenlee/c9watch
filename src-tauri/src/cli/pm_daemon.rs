use crate::cli::pm_fs::{self, SpawnArgs};
use crate::cli::pm_rpc::RpcRequest;
use crate::cli::pm_worker::WorkerHandle;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

const DEFAULT_MAX_WORKERS: usize = 16;

struct DaemonState {
    workers: HashMap<String, WorkerHandle>,
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Called by the CLI's `Daemon` subcommand. Starts the RPC server loop.
pub async fn run_daemon() -> Result<(), String> {
    // 1. Ensure directories exist
    pm_fs::ensure_dirs()?;

    // 2. Get socket path and remove stale socket if it exists
    let sock_path = pm_fs::daemon_sock_path()?;
    if sock_path.exists() {
        std::fs::remove_file(&sock_path)
            .map_err(|e| format!("Failed to remove stale socket {:?}: {}", sock_path, e))?;
    }

    // 3. Write PID file
    let pid = std::process::id();
    let pid_path = pm_fs::daemon_pid_path()?;
    std::fs::write(&pid_path, pid.to_string())
        .map_err(|e| format!("Failed to write PID file {:?}: {}", pid_path, e))?;

    // 4. Bind the Unix socket
    let listener = UnixListener::bind(&sock_path)
        .map_err(|e| format!("Failed to bind Unix socket {:?}: {}", sock_path, e))?;

    // 5. Read max_workers from env
    let max_workers: usize = std::env::var("C9WATCH_MAX_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_WORKERS);

    eprintln!(
        "[pm_daemon] Listening on {:?}, pid={}, max_workers={}",
        sock_path, pid, max_workers
    );

    // 6. Shared state
    let state = Arc::new(Mutex::new(DaemonState {
        workers: HashMap::new(),
    }));

    // 7. Accept loop
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let state_clone = Arc::clone(&state);
                tokio::spawn(async move {
                    handle_connection(stream, state_clone, max_workers).await;
                });
            }
            Err(e) => {
                eprintln!("[pm_daemon] Accept error: {}", e);
            }
        }
    }
}

// ── Connection handler ────────────────────────────────────────────────────────

async fn handle_connection(
    stream: UnixStream,
    state: Arc<Mutex<DaemonState>>,
    max_workers: usize,
) {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    // Read one JSON line
    match reader.read_line(&mut line).await {
        Ok(0) => {
            eprintln!("[pm_daemon] Client disconnected without sending a request");
            return;
        }
        Err(e) => {
            eprintln!("[pm_daemon] Failed to read request line: {}", e);
            return;
        }
        Ok(_) => {}
    }

    // Deserialize the request
    let request: RpcRequest = match serde_json::from_str(line.trim()) {
        Ok(r) => r,
        Err(e) => {
            let resp = serde_json::json!({ "ok": false, "error": format!("PARSE_ERROR: {}", e) });
            write_response(&mut write_half, &resp).await;
            return;
        }
    };

    // Dispatch
    let response = match request {
        RpcRequest::Spawn {
            cwd,
            name,
            append_system_prompt,
            permission_mode,
            model,
            add_dirs,
        } => {
            handle_spawn(
                state,
                cwd,
                name,
                append_system_prompt,
                permission_mode,
                model,
                add_dirs,
                max_workers,
            )
            .await
        }
        RpcRequest::Send {
            session_id,
            text,
            wait,
            timeout_ms,
        } => handle_send(state, session_id, text, wait, timeout_ms).await,
        RpcRequest::List => handle_list(state).await,
        RpcRequest::Stop { session_id } => handle_stop(state, session_id).await,
        RpcRequest::Shutdown => handle_shutdown(state).await,
    };

    write_response(&mut write_half, &response).await;
}

async fn write_response(
    write_half: &mut tokio::io::WriteHalf<UnixStream>,
    value: &serde_json::Value,
) {
    let mut line = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[pm_daemon] Failed to serialize response: {}", e);
            return;
        }
    };
    line.push('\n');
    if let Err(e) = write_half.write_all(line.as_bytes()).await {
        eprintln!("[pm_daemon] Failed to write response: {}", e);
    }
}

// ── RPC handlers ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn handle_spawn(
    state: Arc<Mutex<DaemonState>>,
    cwd: String,
    name: Option<String>,
    append_system_prompt: Option<String>,
    permission_mode: String,
    model: Option<String>,
    add_dirs: Vec<String>,
    max_workers: usize,
) -> serde_json::Value {
    // Check worker limit
    {
        let st = state.lock().await;
        if st.workers.len() >= max_workers {
            return serde_json::json!({ "ok": false, "error": "TOO_MANY_WORKERS" });
        }
    }

    // Generate session ID
    let session_id = uuid::Uuid::new_v4().to_string();

    // Canonicalize cwd
    let canonical_cwd = match std::fs::canonicalize(&cwd) {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(_) => return serde_json::json!({ "ok": false, "error": "CWD_INVALID" }),
    };

    // Build SpawnArgs
    let args = SpawnArgs {
        append_system_prompt,
        permission_mode,
        model,
        add_dirs,
    };

    // Spawn the worker
    let worker = match WorkerHandle::spawn(
        session_id.clone(),
        canonical_cwd.clone(),
        name.clone(),
        args,
        Some("pm-daemon".to_string()),
    )
    .await
    {
        Ok(w) => w,
        Err(e) => return serde_json::json!({ "ok": false, "error": e }),
    };

    let pid = worker.meta.pid;
    let worker_name = worker.meta.name.clone();
    let spawned_at = worker.meta.spawned_at.clone();

    // Insert into state
    {
        let mut st = state.lock().await;
        st.workers.insert(session_id.clone(), worker);
    }

    serde_json::json!({
        "ok": true,
        "sessionId": session_id,
        "pid": pid,
        "name": worker_name,
        "cwd": canonical_cwd,
        "spawnedAt": spawned_at,
    })
}

async fn handle_send(
    state: Arc<Mutex<DaemonState>>,
    session_id: String,
    text: String,
    wait: bool,
    timeout_ms: u64,
) -> serde_json::Value {
    // Resolve the full session ID and send the message while holding the lock
    let full_id = {
        let st = state.lock().await;
        match resolve_worker_id(&st.workers, &session_id) {
            Ok(id) => id,
            Err(e) => return serde_json::json!({ "ok": false, "error": e }),
        }
    };

    // Send message (lock, send, release)
    {
        let st = state.lock().await;
        let worker = match st.workers.get(&full_id) {
            Some(w) => w,
            None => return serde_json::json!({ "ok": false, "error": "WORKER_NOT_FOUND" }),
        };
        if let Err(e) = worker.send_message(&text).await {
            return serde_json::json!({ "ok": false, "error": e });
        }
    }

    if !wait || timeout_ms == 0 {
        return serde_json::json!({
            "ok": true,
            "sessionId": full_id,
            "sent": true,
        });
    }

    // Wait for turn — hold the lock across wait_for_turn (acceptable for Phase 1)
    let timeout = Duration::from_millis(timeout_ms);
    let mut st = state.lock().await;
    let worker = match st.workers.get_mut(&full_id) {
        Some(w) => w,
        None => return serde_json::json!({ "ok": false, "error": "WORKER_NOT_FOUND" }),
    };

    match worker.wait_for_turn(timeout).await {
        Ok(turn) => serde_json::json!({
            "ok": true,
            "sessionId": full_id,
            "sent": true,
            "turnCompleted": true,
            "assistantText": turn.assistant_text,
            "endedAt": turn.ended_at,
        }),
        Err(e) if e == "WAIT_TIMEOUT" => serde_json::json!({
            "ok": true,
            "sessionId": full_id,
            "sent": true,
            "turnCompleted": false,
        }),
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    }
}

async fn handle_list(state: Arc<Mutex<DaemonState>>) -> serde_json::Value {
    let mut st = state.lock().await;
    let workers: Vec<serde_json::Value> = st
        .workers
        .iter_mut()
        .map(|(session_id, worker)| {
            let alive = worker.is_alive();
            serde_json::json!({
                "sessionId": session_id,
                "pid": worker.meta.pid,
                "name": worker.meta.name,
                "cwd": worker.meta.cwd,
                "spawnedAt": worker.meta.spawned_at,
                "alive": alive,
            })
        })
        .collect();

    serde_json::json!({ "ok": true, "workers": workers })
}

async fn handle_stop(state: Arc<Mutex<DaemonState>>, session_id: String) -> serde_json::Value {
    let full_id = {
        let st = state.lock().await;
        match resolve_worker_id(&st.workers, &session_id) {
            Ok(id) => id,
            Err(e) => return serde_json::json!({ "ok": false, "error": e }),
        }
    };

    let mut st = state.lock().await;
    if let Some(mut worker) = st.workers.remove(&full_id) {
        if let Err(e) = worker.kill().await {
            eprintln!("[pm_daemon] Failed to kill worker {}: {}", full_id, e);
        }
    }

    serde_json::json!({ "ok": true, "sessionId": full_id })
}

async fn handle_shutdown(state: Arc<Mutex<DaemonState>>) -> serde_json::Value {
    // Kill all workers
    {
        let mut st = state.lock().await;
        for (id, mut worker) in st.workers.drain() {
            if let Err(e) = worker.kill().await {
                eprintln!("[pm_daemon] Failed to kill worker {} during shutdown: {}", id, e);
            }
        }
    }

    // Clean up pid and sock files
    if let Ok(pid_path) = pm_fs::daemon_pid_path() {
        let _ = std::fs::remove_file(&pid_path);
    }
    if let Ok(sock_path) = pm_fs::daemon_sock_path() {
        let _ = std::fs::remove_file(&sock_path);
    }

    eprintln!("[pm_daemon] Shutdown complete, exiting.");
    std::process::exit(0);
}

// ── Helper ─────────────────────────────────────────────────────────────────────

/// Resolve a session_id prefix to a full session ID in the workers map.
/// - Exact match → return it.
/// - Prefix match with 1 result → return it.
/// - 0 matches → WORKER_NOT_FOUND error.
/// - 2+ matches → AMBIGUOUS_SESSION_ID error.
fn resolve_worker_id(
    workers: &HashMap<String, WorkerHandle>,
    prefix: &str,
) -> Result<String, String> {
    // Exact match first
    if workers.contains_key(prefix) {
        return Ok(prefix.to_string());
    }

    // Prefix match
    let matches: Vec<&String> = workers
        .keys()
        .filter(|k| k.starts_with(prefix))
        .collect();

    match matches.len() {
        0 => Err("WORKER_NOT_FOUND".to_string()),
        1 => Ok(matches[0].clone()),
        _ => Err(format!(
            "AMBIGUOUS_SESSION_ID: prefix '{}' matches {} workers",
            prefix,
            matches.len()
        )),
    }
}
