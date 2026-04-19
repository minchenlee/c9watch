use crate::cli::pm_fs;
use crate::cli::pm_rpc::RpcRequest;
use crate::cli::pm_worker::{SpawnArgs, SpawnContext, WorkerHandle};
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

/// Display-friendly inbox dir hint for RPC responses. Events are keyed by
/// worker session id, so the hint is the worker's dir. Callers list via
/// `c9watch inbox`, which resolves across all owned workers.
fn callback_inbox_hint(worker_session_id: &str) -> String {
    format!("~/.claude/c9watch/inbox/{}/", worker_session_id)
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

    // 3. Bind the Unix socket BEFORE writing the pid file. `ensure_daemon`
    //    treats a live pid file as proof that the daemon is accepting RPCs, so
    //    the socket must be listening first — otherwise a waiter can see the
    //    pid, dial the socket, and hit ECONNREFUSED (fix C3).
    let listener = UnixListener::bind(&sock_path)
        .map_err(|e| format!("Failed to bind Unix socket {:?}: {}", sock_path, e))?;

    // 4. Write PID file now that the socket is listening.
    let pid = std::process::id();
    let pid_path = pm_fs::daemon_pid_path()?;
    std::fs::write(&pid_path, pid.to_string())
        .map_err(|e| format!("Failed to write PID file {:?}: {}", pid_path, e))?;

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
    //    kill workers and clean up their worker dirs before exiting.
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
            pm_pid,
        } => {
            let args = SpawnArgs {
                cwd,
                name,
                append_system_prompt,
                permission_mode,
                model,
                add_dirs,
            };
            let ctx = SpawnContext { spawned_by, pm_pid };
            handle_spawn(state, args, ctx, max_workers).await
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
        RpcRequest::Adopt {
            session_id,
            force,
            caller_pm_session_id,
            caller_pm_pid,
        } => handle_adopt(state, session_id, force, caller_pm_session_id, caller_pm_pid).await,
        RpcRequest::WorkersAll {
            caller_pm_session_id,
            caller_pm_pid,
        } => handle_workers_all(state, caller_pm_session_id, caller_pm_pid).await,
        RpcRequest::InboxRead {
            caller_pm_session_id,
            caller_pm_pid,
            consume,
            clear,
            worker_id,
        } => {
            handle_inbox_read(
                state,
                caller_pm_session_id,
                caller_pm_pid,
                consume,
                clear,
                worker_id,
            )
            .await
        }
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

async fn handle_spawn(
    state: Arc<Mutex<DaemonState>>,
    mut args: SpawnArgs,
    ctx: SpawnContext,
    max_workers: usize,
) -> serde_json::Value {
    // Validate and canonicalize --cwd before acquiring the workers lock so we
    // fail fast without touching shared state.
    let canonical_cwd = match std::fs::canonicalize(&args.cwd) {
        Ok(p) => {
            if !p.is_dir() {
                return serde_json::json!({
                    "ok": false,
                    "error": format!("--cwd '{}' is not a directory", args.cwd),
                });
            }
            p.to_string_lossy().to_string()
        }
        Err(_) => {
            return serde_json::json!({
                "ok": false,
                "error": format!("--cwd '{}' does not exist", args.cwd),
            });
        }
    };
    args.cwd = canonical_cwd.clone();

    // Validate and canonicalize each --add-dir entry.
    let mut canonical_add_dirs: Vec<String> = Vec::with_capacity(args.add_dirs.len());
    for dir in &args.add_dirs {
        match std::fs::canonicalize(dir) {
            Ok(p) => {
                if !p.is_dir() {
                    return serde_json::json!({
                        "ok": false,
                        "error": format!("--add-dir '{}' is not a directory", dir),
                    });
                }
                canonical_add_dirs.push(p.to_string_lossy().to_string());
            }
            Err(_) => {
                return serde_json::json!({
                    "ok": false,
                    "error": format!("--add-dir '{}' does not exist", dir),
                });
            }
        }
    }
    args.add_dirs = canonical_add_dirs;

    eprintln!(
        "[pm_daemon] spawn: cwd={:?} add_dirs={:?}",
        args.cwd, args.add_dirs
    );

    // Generate session ID
    let session_id = uuid::Uuid::new_v4().to_string();

    let spawned_by = ctx.spawned_by.clone();

    // Spawn the worker
    let worker = match WorkerHandle::spawn(session_id.clone(), args, ctx).await {
        Ok(w) => w,
        Err(e) => return err_response(&e),
    };

    let pid = worker.meta.pid;
    let worker_name = worker.meta.name.clone();
    let spawned_at = worker.meta.spawned_at.clone();

    // Insert into state while holding the lock across the cap check so two
    // concurrent spawns cannot both read "count < max" and both proceed (H4).
    {
        let mut st = state.lock().await;
        if st.workers.len() >= max_workers {
            // Worker process is already running — kill it before returning.
            let mut w = worker;
            let _ = w.kill().await;
            return err_response("TOO_MANY_WORKERS");
        }
        st.workers.insert(session_id.clone(), worker);
    }

    // Wait for worker readiness (first stdout event) before returning.
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

    let callback_inbox = callback_inbox_hint(&session_id);

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
        let callback_inbox = callback_inbox_hint(&full_id);
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

    // Await the turn result WITHOUT holding the state lock
    let timeout = Duration::from_millis(timeout_ms);
    let turn_result = tokio::time::timeout(timeout, rx.recv()).await;

    // Return the receiver (and peek worker liveness) so the worker can be used
    // for future --wait calls, and so we can distinguish a genuine timeout
    // from a worker crash (M2).
    let still_alive = {
        let mut st = state.lock().await;
        if let Some(worker) = st.workers.get_mut(&full_id) {
            worker.return_result_receiver(rx);
            worker.is_alive()
        } else {
            false
        }
    };

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
        Err(_timeout) if !still_alive => serde_json::json!({
            "ok": false,
            "error": "WORKER_CRASHED",
            "sessionId": full_id,
            "callbackInbox": callback_inbox,
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

    // Reap dead workers (SIGKILL'd externally, crashed, etc.) before listing.
    // Without this, the 16-worker cap fills up with zombies and further
    // spawns fail (fix L4). We collect dead IDs first to avoid mutating the
    // map while iterating.
    let dead: Vec<String> = st
        .workers
        .iter_mut()
        .filter_map(|(id, worker)| if worker.is_alive() { None } else { Some(id.clone()) })
        .collect();
    for id in dead {
        st.workers.remove(&id);
        cleanup_worker_dir(&id);
    }

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
                "pmPid": worker.meta.pm_pid,
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
    // worker as live. The worker's conversation is archived under
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
/// ctrl_c / SIGTERM signal handler in `run_daemon`.
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
pub(crate) fn resolve_worker_id_from_keys<'a, I>(keys: I, prefix: &str) -> Result<String, String>
where
    I: IntoIterator<Item = &'a str>,
{
    // Reject empty prefix — otherwise `"".starts_with("")` would match every
    // worker and `c9watch send "" --message ...` would silently target the
    // sole live worker.
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

async fn handle_workers_all(
    state: Arc<Mutex<DaemonState>>,
    caller_pm_session_id: Option<String>,
    caller_pm_pid: Option<u32>,
) -> serde_json::Value {
    let mut st = state.lock().await;
    let workers: Vec<serde_json::Value> = st
        .workers
        .iter_mut()
        .map(|(session_id, worker)| {
            let alive = worker.is_alive();
            let status = resolve_status(
                &worker.meta,
                caller_pm_pid,
                caller_pm_session_id.as_deref(),
            );
            serde_json::json!({
                "sessionId": session_id,
                "pid": worker.meta.pid,
                "name": worker.meta.name,
                "cwd": worker.meta.cwd,
                "spawnedAt": worker.meta.spawned_at,
                "spawnedBy": worker.meta.spawned_by,
                "pmPid": worker.meta.pm_pid,
                "alive": alive,
                "status": status.as_str(),
            })
        })
        .collect();
    serde_json::json!({ "ok": true, "workers": workers })
}

async fn handle_adopt(
    state: Arc<Mutex<DaemonState>>,
    target: String,
    force: bool,
    caller_pm_session_id: String,
    caller_pm_pid: Option<u32>,
) -> serde_json::Value {
    if let Err(e) = pm_fs::validate_session_id(&caller_pm_session_id) {
        return err_response(&format!("INVALID_CALLER_PM: {}", e));
    }

    let full_id = {
        let st = state.lock().await;
        if !st.workers.is_empty() {
            match resolve_worker_id(&st.workers, &target) {
                Ok(id) => id,
                Err(e) => return err_response(&e),
            }
        } else {
            match resolve_worker_id_from_disk(&target) {
                Ok(id) => id,
                Err(e) => return err_response(&e),
            }
        }
    };

    let meta = match pm_fs::read_worker_meta(&full_id) {
        Ok(m) => m,
        Err(e) => return err_response(&format!("WORKER_META_READ_FAILED: {}", e)),
    };

    let status = resolve_status(&meta, caller_pm_pid, Some(&caller_pm_session_id));
    match status {
        WorkerStatus::OwnedByYou => serde_json::json!({
            "ok": true,
            "adopted": full_id,
            "pmSessionId": caller_pm_session_id,
            "alreadyOwned": true,
        }),
        WorkerStatus::Orphaned => {
            if let Err(e) = crate::cli::adoption::add(&caller_pm_session_id, &full_id) {
                return err_response(&format!("ADOPTION_WRITE_FAILED: {}", e));
            }
            serde_json::json!({
                "ok": true,
                "adopted": full_id,
                "pmSessionId": caller_pm_session_id,
            })
        }
        WorkerStatus::OwnedByOtherPm => {
            if !force {
                return serde_json::json!({
                    "ok": false,
                    "error": "WORKER_OWNED_BY_OTHER_PM",
                    "details": "pass --force to adopt anyway",
                });
            }
            if let Err(e) = crate::cli::adoption::add(&caller_pm_session_id, &full_id) {
                return err_response(&format!("ADOPTION_WRITE_FAILED: {}", e));
            }
            serde_json::json!({
                "ok": true,
                "adopted": full_id,
                "pmSessionId": caller_pm_session_id,
                "forced": true,
            })
        }
    }
}

fn resolve_worker_id_from_disk(target: &str) -> Result<String, String> {
    let ids = pm_fs::list_worker_ids()?;
    resolve_worker_id_from_keys(ids.iter().map(|s| s.as_str()), target)
}

async fn handle_inbox_read(
    state: Arc<Mutex<DaemonState>>,
    caller_pm_session_id: String,
    caller_pm_pid: Option<u32>,
    consume: bool,
    clear: bool,
    worker_id_filter: Option<String>,
) -> serde_json::Value {
    if let Err(e) = pm_fs::validate_session_id(&caller_pm_session_id) {
        return err_response(&format!("INVALID_CALLER_PM: {}", e));
    }

    let owned: Vec<String> = {
        let st = state.lock().await;
        if !st.workers.is_empty() {
            st.workers
                .iter()
                .filter_map(|(id, w)| {
                    let status = resolve_status(
                        &w.meta,
                        caller_pm_pid,
                        Some(&caller_pm_session_id),
                    );
                    if status == WorkerStatus::OwnedByYou {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            let ids = pm_fs::list_worker_ids().unwrap_or_default();
            ids.into_iter()
                .filter(|id| match pm_fs::read_worker_meta(id) {
                    Ok(m) => {
                        resolve_status(&m, caller_pm_pid, Some(&caller_pm_session_id))
                            == WorkerStatus::OwnedByYou
                    }
                    Err(_) => false,
                })
                .collect()
        }
    };

    let targets: Vec<String> = if let Some(wid) = &worker_id_filter {
        // Accept exact id OR unambiguous prefix of an owned worker, matching
        // the ergonomics of `send`/`stop`/`view`. Previously only exact UUIDs
        // worked, which didn't match the rest of the CLI.
        match resolve_worker_id_from_keys(owned.iter().map(|s| s.as_str()), wid) {
            Ok(full) => vec![full],
            Err(_) => return err_response("WORKER_NOT_OWNED"),
        }
    } else {
        owned
    };

    if clear {
        let mut cleared = 0usize;
        for wid in &targets {
            cleared += crate::cli::pm_inbox::clear(wid).unwrap_or(0);
        }
        return serde_json::json!({
            "ok": true,
            "cleared": cleared,
            "pmSessionId": caller_pm_session_id,
        });
    }

    let mut all_events: Vec<crate::cli::pm_inbox::InboxEvent> = Vec::new();
    for wid in &targets {
        let evs = if consume {
            crate::cli::pm_inbox::consume(wid).unwrap_or_default()
        } else {
            crate::cli::pm_inbox::list(wid).unwrap_or_default()
        };
        all_events.extend(evs);
    }
    all_events.sort_by(|a, b| b.finished_at.cmp(&a.finished_at));

    serde_json::json!({
        "ok": true,
        "pmSessionId": caller_pm_session_id,
        "count": all_events.len(),
        "consumed": consume,
        "events": all_events,
    })
}

// ── Ownership resolver ────────────────────────────────────────────────────────

/// Check whether a PID is alive via `kill(pid, 0)`.
#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn is_pid_alive(_pid: u32) -> bool {
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerStatus {
    OwnedByYou,
    OwnedByOtherPm,
    Orphaned,
}

impl WorkerStatus {
    fn as_str(self) -> &'static str {
        match self {
            WorkerStatus::OwnedByYou => "OWNED_BY_YOU",
            WorkerStatus::OwnedByOtherPm => "OWNED_BY_OTHER_PM",
            WorkerStatus::Orphaned => "ORPHANED",
        }
    }
}

/// Classify `meta` against the caller's identity.
fn resolve_status(
    meta: &pm_fs::WorkerMeta,
    caller_pm_pid: Option<u32>,
    caller_pm_session_id: Option<&str>,
) -> WorkerStatus {
    // 1. PID-based OWNED_BY_YOU
    if let (Some(mine), Some(theirs)) = (caller_pm_pid, meta.pm_pid) {
        if mine == theirs {
            return WorkerStatus::OwnedByYou;
        }
    }
    // 2. Adoption-based OWNED_BY_YOU
    if let Some(sid) = caller_pm_session_id {
        if let Ok(Some(record)) = crate::cli::adoption::read_filter(sid) {
            if record.worker_ids.iter().any(|w| w == &meta.session_id) {
                return WorkerStatus::OwnedByYou;
            }
        }
    }
    // 3. meta.pm_pid alive and not ours → OWNED_BY_OTHER_PM
    if let Some(owner_pid) = meta.pm_pid {
        if is_pid_alive(owner_pid) {
            return WorkerStatus::OwnedByOtherPm;
        }
    }
    // 4. Is any *other* PM's adoption sidecar claiming this worker?
    if let Ok(entries) = std::fs::read_dir(
        pm_fs::adoptions_dir().unwrap_or_else(|_| std::path::PathBuf::from("/dev/null")),
    ) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s,
                None => continue,
            };
            if Some(stem) == caller_pm_session_id {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(rec) =
                    serde_json::from_str::<crate::cli::adoption::AdoptionRecord>(&content)
                {
                    if rec.worker_ids.iter().any(|w| w == &meta.session_id) {
                        return WorkerStatus::OwnedByOtherPm;
                    }
                }
            }
        }
    }
    WorkerStatus::Orphaned
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

    fn make_meta(wid: &str, pm_pid: Option<u32>) -> pm_fs::WorkerMeta {
        pm_fs::WorkerMeta {
            session_id: wid.to_string(),
            pid: 1,
            name: None,
            cwd: "/tmp".to_string(),
            spawned_at: "t".to_string(),
            spawned_by: Some("pm-audit".to_string()),
            pm_pid,
            spawn_args: pm_fs::PersistedSpawnArgs {
                append_system_prompt: None,
                permission_mode: "default".to_string(),
                model: None,
                add_dirs: vec![],
            },
            stopped_at: None,
        }
    }

    #[test]
    fn resolve_status_owned_by_you_via_pid() {
        let meta = make_meta("w-pid-owned", Some(99999));
        assert_eq!(
            resolve_status(&meta, Some(99999), Some("pm-uuid-1")),
            WorkerStatus::OwnedByYou
        );
    }

    #[test]
    fn resolve_status_orphaned_when_pm_pid_dead_and_no_adoption() {
        // 999_999_999 is a positive pid_t too large to ever be alive on this
        // machine, so `kill(pid, 0)` returns ESRCH. u32::MAX would wrap to -1
        // (broadcast "all processes") — do not use it here.
        let meta = make_meta("w-orphan-unique-id-xyz", Some(999_999_999));
        let status = resolve_status(&meta, Some(1), Some("pm-uuid-stranger-xyz"));
        assert_eq!(status, WorkerStatus::Orphaned);
    }

    // ── H5: path validation helpers ──────────────────────────────────────────

    /// Validate a --cwd path: returns Ok(canonical_string) or a user-facing
    /// error message. Mirrors the logic in handle_spawn so it can be unit tested.
    fn validate_cwd(raw: &str) -> Result<String, String> {
        match std::fs::canonicalize(raw) {
            Ok(p) => {
                if !p.is_dir() {
                    return Err(format!("--cwd '{}' is not a directory", raw));
                }
                Ok(p.to_string_lossy().to_string())
            }
            Err(_) => Err(format!("--cwd '{}' does not exist", raw)),
        }
    }

    #[test]
    fn h5_cwd_nonexistent_gives_clean_error() {
        let err = validate_cwd("/this/path/definitely/does/not/exist/h5test").unwrap_err();
        assert!(
            err.contains("does not exist"),
            "expected 'does not exist', got: {}",
            err
        );
        assert!(err.contains("/this/path/definitely/does/not/exist/h5test"), "path should appear in error: {}", err);
    }

    #[test]
    fn h5_cwd_existing_dir_succeeds() {
        // /tmp is guaranteed to exist and be a directory on macOS/Linux.
        let result = validate_cwd("/tmp");
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }

    #[test]
    fn h5_cwd_file_is_rejected() {
        // Create a temp file and confirm it's rejected as "not a directory".
        let dir = std::env::temp_dir();
        let file_path = dir.join("h5_cwd_file_test.txt");
        std::fs::write(&file_path, b"x").expect("write temp file");
        let raw = file_path.to_string_lossy().to_string();
        let err = validate_cwd(&raw).unwrap_err();
        std::fs::remove_file(&file_path).ok();
        assert!(
            err.contains("is not a directory"),
            "expected 'is not a directory', got: {}",
            err
        );
    }

    #[test]
    fn h5_cwd_deleted_after_creation_gives_does_not_exist() {
        // Create a temp dir, delete it, then try to use it as --cwd.
        let dir = std::env::temp_dir().join("h5_deleted_dir_test");
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::remove_dir_all(&dir).expect("remove temp dir");
        let raw = dir.to_string_lossy().to_string();
        let err = validate_cwd(&raw).unwrap_err();
        assert!(
            err.contains("does not exist"),
            "expected 'does not exist', got: {}",
            err
        );
    }
}
