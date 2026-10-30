use crate::cli::pm_fs;
use crate::cli::pm_rpc::RpcRequest;
use crate::cli::pm_worker::{
    AnyWorkerHandle, BgWorkerHandle, SpawnArgs, SpawnContext, WorkerHandle,
};
use crate::cli::worker_backend::{select_backend, validate_initial_prompt, BackendKind};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

const DEFAULT_MAX_WORKERS: usize = 16;

struct DaemonState {
    workers: HashMap<String, AnyWorkerHandle>,
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

/// Per-worker entry shared by `handle_list` and `handle_workers_all`.
fn worker_json(session_id: &str, alive: bool, meta: &pm_fs::WorkerMeta) -> serde_json::Value {
    serde_json::json!({
        "sessionId": session_id,
        "pid": meta.pid,
        "name": meta.name,
        "cwd": meta.cwd,
        "spawnedAt": meta.spawned_at,
        "spawnedBy": meta.spawned_by,
        "pmPid": meta.pm_pid,
        "alive": alive,
    })
}

// ── Peer check ────────────────────────────────────────────────────────────────

/// Only a process running as the daemon's own uid may talk to it.
pub(crate) fn peer_allowed(peer_uid: u32, me: u32) -> bool {
    peer_uid == me
}

/// Check the uid of the process on the other end of an accepted connection.
/// A peer whose credentials cannot be read is rejected too. `Err(None)`
/// means the peer already hung up (the `ensure_daemon` readiness probe does
/// this), which is rejected without a log line.
fn check_peer(stream: &UnixStream, me: u32) -> Result<(), Option<String>> {
    match stream.peer_cred() {
        Ok(cred) if peer_allowed(cred.uid(), me) => Ok(()),
        Ok(cred) => Err(Some(format!("peer uid mismatch uid={}", cred.uid()))),
        Err(e) if e.raw_os_error() == Some(libc::ENOTCONN) => Err(None),
        Err(e) => Err(Some(format!("peer uid mismatch uid=unknown ({})", e))),
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// How long a new daemon waits for a pre-lock daemon to exit after asking it
/// to shut down. Its shutdown kills every worker first.
const LEGACY_EXIT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the Shutdown RPC to a pre-lock daemon may take.
const LEGACY_SHUTDOWN_RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest a new daemon may spend retiring a pre-lock daemon before it
/// binds. Clients wait longer than this (see `pm::DAEMON_READY_TIMEOUT`).
pub(crate) const LEGACY_RETIRE_BUDGET: Duration =
    Duration::from_secs(LEGACY_SHUTDOWN_RPC_TIMEOUT.as_secs() + LEGACY_EXIT_TIMEOUT.as_secs());

/// Ask a daemon from before the lifetime lock (it never takes `daemon.lock`)
/// to shut down, and wait for its pid to exit. Must run while holding the
/// lock and before binding: the old daemon's shutdown deletes `daemon.sock`
/// unconditionally, so binding first would lose the new socket.
///
/// Only a daemon that answers on our socket is asked; a live pid with
/// nothing listening is a stale pid file pointing at a reused pid.
fn retire_legacy_daemon(sock_path: &Path, pid_path: &Path) -> Result<(), String> {
    let pid = match std::fs::read_to_string(pid_path)
        .ok()
        .and_then(|c| c.trim().parse::<libc::pid_t>().ok())
    {
        Some(pid) if pid > 1 && pid as u32 != std::process::id() => pid,
        _ => return Ok(()),
    };
    let alive = |pid: libc::pid_t| unsafe { libc::kill(pid, 0) } == 0;
    if !alive(pid) {
        return Ok(());
    }
    match crate::cli::pm_rpc::rpc_call(
        sock_path,
        &RpcRequest::Shutdown,
        LEGACY_SHUTDOWN_RPC_TIMEOUT,
    ) {
        Err(e) if e.starts_with(crate::cli::pm_rpc::DAEMON_UNREACHABLE) => return Ok(()),
        Err(e) if e.starts_with(crate::cli::pm_rpc::DAEMON_UNTRUSTED) => return Err(e),
        // The old daemon exits without answering Shutdown, so any other
        // result means it got the request.
        _ => {}
    }
    eprintln!("[pm_daemon] Asked old daemon pid={} to shut down", pid);
    let deadline = std::time::Instant::now() + LEGACY_EXIT_TIMEOUT;
    while alive(pid) {
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "DAEMON_LEGACY_RUNNING: old daemon pid {} did not exit after shutdown",
                pid
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Take the lifetime lock and bind the daemon socket in `dir`.
///
/// Order: lock, retire a pre-lock daemon, clear the stale socket and temp
/// sockets, bind 0600 under a temp name and rename, write the pid file, then
/// write the ready mark into the lock file.
/// A second daemon fails at the lock with `DAEMON_ALREADY_RUNNING` and never
/// touches the socket or the pid file. The returned `DaemonLock` holds the lock and
/// must live as long as the listener.
pub(crate) fn start_listener(
    dir: &Path,
) -> Result<(pm_fs::DaemonLock, std::os::unix::net::UnixListener), String> {
    let lock = pm_fs::acquire_daemon_lock(&dir.join(pm_fs::DAEMON_LOCK_NAME))?;

    let sock_path = dir.join(pm_fs::DAEMON_SOCK_NAME);
    let pid_path = dir.join(pm_fs::DAEMON_PID_NAME);
    retire_legacy_daemon(&sock_path, &pid_path)?;

    // We hold the lock, so nothing live is serving these paths.
    match std::fs::remove_file(&sock_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "Failed to remove stale socket {:?}: {}",
                sock_path, e
            ))
        }
    }
    let tmp_prefix = format!(".{}.tmp-", pm_fs::DAEMON_SOCK_NAME);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&tmp_prefix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    // Bind BEFORE writing the pid file, so the socket is listening by the
    // time anyone reads the pid (fix C3).
    let listener = pm_fs::bind_private(&sock_path)?;

    use std::io::Write;
    pm_fs::open_private_file(
        &pid_path,
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true),
    )?
    .write_all(std::process::id().to_string().as_bytes())
    .map_err(|e| format!("Failed to write PID file {:?}: {}", pid_path, e))?;

    pm_fs::mark_daemon_ready(&lock)?;
    Ok((lock, listener))
}

/// Called by the CLI's `Daemon` subcommand. Starts the RPC server loop.
pub async fn run_daemon() -> Result<(), String> {
    // 1. Ensure directories exist (the base dir private, 0700)
    pm_fs::ensure_dirs()?;

    // 2-4. Lock, bind and write the pid file. `_lock` holds the lifetime
    //      flock until the process exits.
    let dir = pm_fs::c9watch_dir()?;
    let sock_path = dir.join(pm_fs::DAEMON_SOCK_NAME);
    let (_lock, std_listener) = start_listener(&dir)?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| format!("Failed to set socket non-blocking: {}", e))?;
    let listener = UnixListener::from_std(std_listener)
        .map_err(|e| format!("Failed to register Unix socket: {}", e))?;
    let pid = std::process::id();

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
    //    kill workers and clean up their worker dirs before exiting. Every
    //    peer must run as our uid; the 0700 dir and 0600 socket are the
    //    first fence, this check is the second.
    let me = unsafe { libc::geteuid() };
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("Failed to install SIGTERM handler: {}", e))?;
    loop {
        tokio::select! {
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _addr)) => {
                        if let Err(reason) = check_peer(&stream, me) {
                            if let Some(reason) = reason {
                                eprintln!("[pm_daemon] {}", reason);
                            }
                            continue;
                        }
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
            _ = sigterm.recv() => {
                eprintln!("[pm_daemon] Received SIGTERM, shutting down...");
                shutdown_daemon(state).await;
            }
        }
    }
}

// ── Connection handler ────────────────────────────────────────────────────────

async fn handle_connection(stream: UnixStream, state: Arc<Mutex<DaemonState>>, max_workers: usize) {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    // Read one JSON line
    match reader.read_line(&mut line).await {
        // `ensure_daemon` probes readiness by connecting and closing, so a
        // connection with no request is normal and not logged.
        Ok(0) => return,
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
            initial_prompt,
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
            handle_spawn(state, args, ctx, initial_prompt, max_workers).await
        }
        RpcRequest::List => handle_list(state).await,
        RpcRequest::Stop { session_id } => handle_stop(state, session_id).await,
        RpcRequest::Shutdown => handle_shutdown(state).await,
        RpcRequest::Adopt {
            session_id,
            force,
            caller_pm_session_id,
            caller_pm_pid,
        } => {
            handle_adopt(
                state,
                session_id,
                force,
                caller_pm_session_id,
                caller_pm_pid,
            )
            .await
        }
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

/// Canonicalize a directory argument and return a user-facing error tagged with
/// the flag name (e.g. "--cwd") if the path does not exist or is not a directory.
fn validate_dir_arg(flag: &str, raw: &str) -> Result<String, String> {
    match std::fs::canonicalize(raw) {
        Ok(p) if p.is_dir() => Ok(p.to_string_lossy().to_string()),
        Ok(_) => Err(format!("{} '{}' is not a directory", flag, raw)),
        Err(_) => Err(format!("{} '{}' does not exist", flag, raw)),
    }
}

async fn handle_spawn(
    state: Arc<Mutex<DaemonState>>,
    mut args: SpawnArgs,
    ctx: SpawnContext,
    initial_prompt: Option<String>,
    max_workers: usize,
) -> serde_json::Value {
    // Validate and canonicalize --cwd + each --add-dir before acquiring the
    // workers lock so we fail fast without touching shared state.
    args.cwd = match validate_dir_arg("--cwd", &args.cwd) {
        Ok(p) => p,
        Err(e) => return err_response(&e),
    };

    let mut canonical_add_dirs: Vec<String> = Vec::with_capacity(args.add_dirs.len());
    for dir in &args.add_dirs {
        match validate_dir_arg("--add-dir", dir) {
            Ok(p) => canonical_add_dirs.push(p),
            Err(e) => return err_response(&e),
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
    let canonical_cwd = args.cwd.clone();

    // Validate the initial prompt against the selected backend. Bg backend
    // requires a non-empty prompt; Print backend takes it when given.
    let backend = select_backend();
    let resolved_prompt = match validate_initial_prompt(backend, initial_prompt) {
        Ok(p) => p,
        Err(e) => return err_response(&e),
    };

    // Spawn the worker via the selected backend. PrintBackend is the legacy
    // path; BgBackend uses `claude --bg` + control.sock RPC.
    let worker: AnyWorkerHandle = match backend {
        BackendKind::Bg => {
            let prompt = resolved_prompt.expect("validate_initial_prompt guarantees Some for Bg");
            match BgWorkerHandle::spawn(session_id.clone(), args, ctx, prompt).await {
                Ok(w) => AnyWorkerHandle::Bg(w),
                Err(e) => return err_response(&e),
            }
        }
        BackendKind::Print => {
            match WorkerHandle::spawn(session_id.clone(), args, ctx, resolved_prompt).await {
                Ok(w) => AnyWorkerHandle::Print(w),
                Err(e) => return err_response(&e),
            }
        }
    };

    let pid = worker.meta().pid;
    let worker_name = worker.meta().name.clone();
    let spawned_at = worker.meta().spawned_at.clone();
    // Bg backend rewrites session_id to CC's actual UUID inside spawn (CC
    // ignores `--session-id` for bg sessions). From here on, the meta's
    // session_id is canonical — use it for the state map key and the
    // response. The pre-spawn `session_id` (still owned by us) was only a
    // placeholder for the Bg path; the Print path still uses it unchanged.
    let effective_session_id = worker.meta().session_id.clone();

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
        st.workers.insert(effective_session_id.clone(), worker);
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
        if let Some(worker) = st.workers.get_mut(&effective_session_id) {
            worker.wait_ready(Duration::from_secs(15)).await
        } else {
            Err("Worker disappeared immediately after insert".to_string())
        }
    };

    if let Err(e) = ready_result {
        // Worker failed to initialize — clean up
        let mut st = state.lock().await;
        if let Some(mut w) = st.workers.remove(&effective_session_id) {
            let _ = w.kill().await;
        }
        return err_response(&format!("SPAWN_FAILED: {}", e));
    }

    let callback_inbox = callback_inbox_hint(&effective_session_id);

    serde_json::json!({
        "ok": true,
        "sessionId": effective_session_id,
        "pid": pid,
        "name": worker_name,
        "cwd": canonical_cwd,
        "spawnedAt": spawned_at,
        "spawnedBy": spawned_by,
        "callbackInbox": callback_inbox,
    })
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
        .filter_map(|(id, worker)| {
            if worker.is_alive() {
                None
            } else {
                Some(id.clone())
            }
        })
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
            worker_json(session_id, alive, worker.meta())
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
                    eprintln!("[pm_daemon] Failed to remove worker dir {:?}: {}", dir, e);
                }
            }
        }
        Err(e) => {
            eprintln!(
                "[pm_daemon] Cannot resolve worker dir for {}: {}",
                session_id, e
            );
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
                eprintln!(
                    "[pm_daemon] Failed to kill worker {} during shutdown: {}",
                    id, e
                );
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
    workers: &HashMap<String, AnyWorkerHandle>,
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
    // worker and `c9watch stop ""` would silently target the sole live
    // worker.
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
            let meta = worker.meta();
            let status = resolve_status(meta, caller_pm_pid, caller_pm_session_id.as_deref());
            let mut entry = worker_json(session_id, alive, meta);
            entry["status"] = serde_json::Value::from(status.as_str());
            entry
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
                    let status =
                        resolve_status(w.meta(), caller_pm_pid, Some(&caller_pm_session_id));
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
    fn worker_json_shape() {
        let mut meta = make_meta("sid-1", Some(42));
        meta.name = Some("w".to_string());
        let v = worker_json("sid-1", true, &meta);
        assert_eq!(
            v,
            serde_json::json!({
                "sessionId": "sid-1",
                "pid": 1,
                "name": "w",
                "cwd": "/tmp",
                "spawnedAt": "t",
                "spawnedBy": "pm-audit",
                "pmPid": 42,
                "alive": true,
            })
        );
        assert_eq!(
            v.to_string(),
            r#"{"alive":true,"cwd":"/tmp","name":"w","pid":1,"pmPid":42,"sessionId":"sid-1","spawnedAt":"t","spawnedBy":"pm-audit"}"#
        );
    }

    #[test]
    fn worker_json_with_status_matches_inline_shape() {
        let meta = make_meta("sid-1", None);
        let mut v = worker_json("sid-1", false, &meta);
        v["status"] = serde_json::Value::from("orphaned");
        assert_eq!(
            v,
            serde_json::json!({
                "sessionId": "sid-1",
                "pid": 1,
                "name": null,
                "cwd": "/tmp",
                "spawnedAt": "t",
                "spawnedBy": "pm-audit",
                "pmPid": null,
                "alive": false,
                "status": "orphaned",
            })
        );
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

    fn validate_cwd(raw: &str) -> Result<String, String> {
        super::validate_dir_arg("--cwd", raw)
    }

    #[test]
    fn h5_cwd_nonexistent_gives_clean_error() {
        let err = validate_cwd("/this/path/definitely/does/not/exist/h5test").unwrap_err();
        assert!(
            err.contains("does not exist"),
            "expected 'does not exist', got: {}",
            err
        );
        assert!(
            err.contains("/this/path/definitely/does/not/exist/h5test"),
            "path should appear in error: {}",
            err
        );
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

    // ── C3: bind-before-pid invariant ────────────────────────────────────────

    fn private_tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("c9w-d-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        pm_fs::ensure_private_dir(&d).expect("private temp dir");
        d
    }

    fn mode_of(p: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
    }

    /// `start_listener` binds the socket (0600) before the pid file exists,
    /// so anyone who reads the pid finds the socket already listening.
    #[test]
    fn c3_socket_bound_before_pid_file_written() {
        let dir = private_tmpdir("c3");
        let sock_path = dir.join(pm_fs::DAEMON_SOCK_NAME);
        let pid_path = dir.join(pm_fs::DAEMON_PID_NAME);

        let _listener = pm_fs::bind_private(&sock_path).expect("bind");
        assert!(sock_path.exists(), "socket must exist after bind");
        assert_eq!(mode_of(&sock_path), 0o600);
        assert!(!pid_path.exists(), "pid file must not exist before write");
        drop(_listener);

        let (_lock, _l) = start_listener(&dir).expect("start_listener");
        assert_eq!(mode_of(&sock_path), 0o600);
        assert_eq!(mode_of(&pid_path), 0o600);
        assert_eq!(
            std::fs::read_to_string(&pid_path).unwrap(),
            std::process::id().to_string()
        );
        assert!(pm_fs::daemon_ready_marked(
            &dir.join(pm_fs::DAEMON_LOCK_NAME)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── C0h-07: peer uid check ───────────────────────────────────────────────

    #[test]
    fn peer_allowed_only_for_our_uid() {
        let me = unsafe { libc::geteuid() };
        assert!(peer_allowed(me, me));
        assert!(!peer_allowed(me + 1, me));
        assert!(!peer_allowed(0, me + 1));
    }

    #[test]
    fn peer_cred_same_uid_allowed() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        rt.block_on(async {
            let (a, _b) = UnixStream::pair().unwrap();
            let me = unsafe { libc::geteuid() };
            assert!(check_peer(&a, me).is_ok());
            // The same connection seen by a daemon running as another uid.
            let err = check_peer(&a, me + 1).unwrap_err();
            assert_eq!(err, Some(format!("peer uid mismatch uid={}", me)));
        });
    }

    // ── C0h-06: lifetime lock ────────────────────────────────────────────────

    #[test]
    fn second_daemon_cannot_steal_socket() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::fs::MetadataExt;
        let dir = private_tmpdir("steal");
        let sock_path = dir.join(pm_fs::DAEMON_SOCK_NAME);

        let (_lock, listener) = start_listener(&dir).expect("first daemon");
        let ino = std::fs::metadata(&sock_path).unwrap().ino();

        let err = start_listener(&dir).unwrap_err();
        assert_eq!(err, pm_fs::DAEMON_ALREADY_RUNNING);
        assert_eq!(std::fs::metadata(&sock_path).unwrap().ino(), ino);

        // The first listener still serves the path.
        let h = std::thread::spawn(move || {
            let (c, _) = listener.accept().unwrap();
            let mut s = String::new();
            BufReader::new(&c).read_line(&mut s).unwrap();
            s
        });
        let mut c = std::os::unix::net::UnixStream::connect(&sock_path).unwrap();
        writeln!(c, "ping").unwrap();
        assert_eq!(h.join().unwrap(), "ping\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_socket_is_replaced_when_unlocked() {
        use std::os::unix::fs::MetadataExt;
        let dir = private_tmpdir("stale");
        let sock_path = dir.join(pm_fs::DAEMON_SOCK_NAME);

        // A crashed daemon leaves its socket file and a temp socket behind.
        drop(std::os::unix::net::UnixListener::bind(&sock_path).unwrap());
        let stale_tmp = dir.join(format!(".{}.tmp-999999", pm_fs::DAEMON_SOCK_NAME));
        std::fs::write(&stale_tmp, b"").unwrap();
        let old_ino = std::fs::metadata(&sock_path).unwrap().ino();
        assert!(std::os::unix::net::UnixStream::connect(&sock_path).is_err());

        let (_lock, _listener) = start_listener(&dir).expect("start over stale socket");
        assert_ne!(std::fs::metadata(&sock_path).unwrap().ino(), old_ino);
        assert!(!stale_tmp.exists());
        assert!(std::os::unix::net::UnixStream::connect(&sock_path).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
