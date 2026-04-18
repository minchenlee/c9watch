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

fn err_response(code: &str) -> serde_json::Value {
    serde_json::json!({ "ok": false, "error": code })
}

/// Display-friendly inbox dir hint for RPC responses (e.g.
/// `~/.claude/c9watch/inbox/<pm>/`). Single source of truth for the hint
/// format returned by spawn/send.
fn callback_inbox_hint(spawned_by: Option<&str>) -> Option<String> {
    spawned_by.map(|pm| format!("~/.claude/c9watch/inbox/{}/", pm))
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

    // 7. Accept loop — interruptible by ctrl_c / SIGTERM so the daemon can
    //    kill workers and clean up their worker dirs before exiting (fix H2a).
    loop {
        tokio::select! {
            accept = listener.accept() => {
                match accept {
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
            _ = tokio::signal::ctrl_c() => {
                eprintln!("[pm_daemon] Received ctrl_c / SIGINT, shutting down...");
                shutdown_daemon(state).await;
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
            let resp = err_response(&format!("PARSE_ERROR: {}", e));
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
            spawned_by,
        } => {
            handle_spawn(
                state,
                cwd,
                name,
                append_system_prompt,
                permission_mode,
                model,
                add_dirs,
                spawned_by,
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
    spawned_by: Option<String>,
    max_workers: usize,
) -> serde_json::Value {
    // Check worker limit
    {
        let st = state.lock().await;
        if st.workers.len() >= max_workers {
            return err_response("TOO_MANY_WORKERS");
        }
    }

    // Generate session ID
    let session_id = uuid::Uuid::new_v4().to_string();

    // Canonicalize cwd
    let canonical_cwd = match std::fs::canonicalize(&cwd) {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(_) => return err_response("CWD_INVALID"),
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
        spawned_by.clone(),
    )
    .await
    {
        Ok(w) => w,
        Err(e) => return err_response(&e),
    };

    let pid = worker.meta.pid;
    let worker_name = worker.meta.name.clone();
    let spawned_at = worker.meta.spawned_at.clone();

    // Insert into state
    {
        let mut st = state.lock().await;
        st.workers.insert(session_id.clone(), worker);
    }

    // Wait for worker readiness (first stdout event) before returning (fix I5).
    // Lock briefly to call wait_ready, which only holds until the oneshot fires.
    // We cannot hold the lock across the await, so we take a short-lived lock
    // per poll — but wait_ready is a single async await, so we grab the lock,
    // call the future, release the lock once it resolves.
    //
    // Because wait_ready uses a oneshot it completes in one await, so the lock
    // is held only for the duration of that single .await.
    let ready_result = {
        let mut st = state.lock().await;
        if let Some(worker) = st.workers.get_mut(&session_id) {
            worker.wait_ready(Duration::from_secs(15)).await
        } else {
            Err("Worker disappeared immediately after insert".to_string())
        }
    };

    if let Err(e) = ready_result {
        // Worker failed to initialize — clean up
        let mut st = state.lock().await;
        if let Some(mut w) = st.workers.remove(&session_id) {
            let _ = w.kill().await;
        }
        return err_response(&format!("SPAWN_FAILED: {}", e));
    }

    let callback_inbox = callback_inbox_hint(spawned_by.as_deref());

    serde_json::json!({
        "ok": true,
        "sessionId": session_id,
        "pid": pid,
        "name": worker_name,
        "cwd": canonical_cwd,
        "spawnedAt": spawned_at,
        "spawnedBy": spawned_by,
        "callbackInbox": callback_inbox,
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
            Err(e) => return err_response(&e),
        }
    };

    // Send the message under the lock, then release it before awaiting the
    // turn result so other RPCs aren't blocked for up to `timeout_ms`.
    let (rx_opt, callback_inbox) = {
        let mut st = state.lock().await;
        let worker = match st.workers.get_mut(&full_id) {
            Some(w) => w,
            None => return err_response("WORKER_NOT_FOUND"),
        };
        if let Err(e) = worker.send_message(&text).await {
            return err_response(&e);
        }
        let callback_inbox = callback_inbox_hint(worker.meta.spawned_by.as_deref());
        let rx = if wait && timeout_ms > 0 {
            match worker.take_result_receiver() {
                Some(r) => Some(r),
                None => {
                    return err_response(
                        "SEND_BUSY: another --wait is already pending for this worker",
                    );
                }
            }
        } else {
            None
        };
        (rx, callback_inbox)
    };

    if !wait || timeout_ms == 0 {
        return serde_json::json!({
            "ok": true,
            "sessionId": full_id,
            "sent": true,
            "callbackInbox": callback_inbox,
        });
    }

    // rx_opt is Some here because wait && timeout_ms > 0
    let mut rx = rx_opt.expect("rx must be Some when wait && timeout_ms > 0");

    // Await the turn result WITHOUT holding the state lock (fix C2)
    let timeout = Duration::from_millis(timeout_ms);
    let turn_result = tokio::time::timeout(timeout, rx.recv()).await;

    // Return the receiver so the worker can be used for future --wait calls
    {
        let mut st = state.lock().await;
        if let Some(worker) = st.workers.get_mut(&full_id) {
            worker.return_result_receiver(rx);
        }
    }

    match turn_result {
        Ok(Some(turn)) => serde_json::json!({
            "ok": true,
            "sessionId": full_id,
            "sent": true,
            "turnCompleted": true,
            "assistantText": turn.assistant_text,
            "endedAt": turn.ended_at,
            "callbackInbox": callback_inbox,
        }),
        Ok(None) => serde_json::json!({
            "ok": false,
            "error": "result channel closed — worker stdout tee task exited",
        }),
        Err(_timeout) => serde_json::json!({
            "ok": true,
            "sessionId": full_id,
            "sent": true,
            "turnCompleted": false,
            "callbackInbox": callback_inbox,
        }),
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
                "spawnedBy": worker.meta.spawned_by,
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
            Err(e) => return err_response(&e),
        }
    };

    let mut st = state.lock().await;
    if let Some(mut worker) = st.workers.remove(&full_id) {
        if let Err(e) = worker.kill().await {
            eprintln!("[pm_daemon] Failed to kill worker {}: {}", full_id, e);
        }
    }

    // Clean up the on-disk worker dir (meta.json + stdout.log + stderr.log)
    // so the GUI / `c9watch list` overlay doesn't keep showing a stopped
    // worker as live (fix H2a). The worker's conversation is archived under
    // \`~/.claude/projects/\` already, so nothing important is lost.
    cleanup_worker_dir(&full_id);

    serde_json::json!({ "ok": true, "sessionId": full_id })
}

/// Remove `~/.claude/c9watch/workers/<session_id>/`. Logs (not fails) on error.
fn cleanup_worker_dir(session_id: &str) {
    match pm_fs::worker_dir(session_id) {
        Ok(dir) => {
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "[pm_daemon] Failed to remove worker dir {:?}: {}",
                        dir, e
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("[pm_daemon] Cannot resolve worker dir for {}: {}", session_id, e);
        }
    }
}

async fn handle_shutdown(state: Arc<Mutex<DaemonState>>) -> serde_json::Value {
    shutdown_daemon(state).await;
}

/// Kill all workers, clean up their on-disk dirs, then remove the daemon
/// pid/sock files and exit. Called both by the `shutdown` RPC and by the
/// ctrl_c / SIGTERM signal handler in `run_daemon` (fix H2a).
async fn shutdown_daemon(state: Arc<Mutex<DaemonState>>) -> ! {
    // Kill all workers and remove their worker dirs
    let killed_ids: Vec<String> = {
        let mut st = state.lock().await;
        let mut ids = Vec::with_capacity(st.workers.len());
        for (id, mut worker) in st.workers.drain() {
            if let Err(e) = worker.kill().await {
                eprintln!("[pm_daemon] Failed to kill worker {} during shutdown: {}", id, e);
            }
            ids.push(id);
        }
        ids
    };
    for id in &killed_ids {
        cleanup_worker_dir(id);
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
    resolve_worker_id_from_keys(workers.keys().map(|k| k.as_str()), prefix)
}

/// Key-only variant of `resolve_worker_id`, factored out so it can be unit
/// tested without constructing real `WorkerHandle`s.
fn resolve_worker_id_from_keys<'a, I>(keys: I, prefix: &str) -> Result<String, String>
where
    I: IntoIterator<Item = &'a str>,
{
    // Reject empty prefix — otherwise `"".starts_with("")` would match every
    // worker and `c9watch send "" --message ...` would silently target the
    // sole live worker (fix C4).
    if prefix.is_empty() {
        return Err("worker id/prefix required".to_string());
    }

    let keys: Vec<&str> = keys.into_iter().collect();

    // Exact match first
    if keys.iter().any(|k| *k == prefix) {
        return Ok(prefix.to_string());
    }

    // Prefix match
    let matches: Vec<&&str> = keys.iter().filter(|k| k.starts_with(prefix)).collect();

    match matches.len() {
        0 => Err("WORKER_NOT_FOUND".to_string()),
        1 => Ok(matches[0].to_string()),
        _ => Err(format!(
            "AMBIGUOUS_SESSION_ID: prefix '{}' matches {} workers",
            prefix,
            matches.len()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_worker_id_rejects_empty_prefix() {
        let keys = ["abc-123", "def-456"];
        let err = resolve_worker_id_from_keys(keys.iter().copied(), "").unwrap_err();
        assert!(err.contains("required"), "got: {}", err);
    }

    #[test]
    fn resolve_worker_id_empty_prefix_with_sole_worker() {
        // Regression: `"".starts_with("")` is true, so without the guard this
        // would match the lone worker. Must still error.
        let keys = ["only-one"];
        assert!(resolve_worker_id_from_keys(keys.iter().copied(), "").is_err());
    }

    #[test]
    fn resolve_worker_id_exact_match_wins() {
        let keys = ["abc-123", "abc-123-extra"];
        assert_eq!(
            resolve_worker_id_from_keys(keys.iter().copied(), "abc-123").unwrap(),
            "abc-123"
        );
    }

    #[test]
    fn resolve_worker_id_unique_prefix() {
        let keys = ["abc-123", "def-456"];
        assert_eq!(
            resolve_worker_id_from_keys(keys.iter().copied(), "abc").unwrap(),
            "abc-123"
        );
    }

    #[test]
    fn resolve_worker_id_ambiguous() {
        let keys = ["abc-123", "abc-456"];
        let err = resolve_worker_id_from_keys(keys.iter().copied(), "abc").unwrap_err();
        assert!(err.contains("AMBIGUOUS"), "got: {}", err);
    }

    #[test]
    fn resolve_worker_id_not_found() {
        let keys = ["abc-123"];
        let err = resolve_worker_id_from_keys(keys.iter().copied(), "xyz").unwrap_err();
        assert_eq!(err, "WORKER_NOT_FOUND");
    }
}
