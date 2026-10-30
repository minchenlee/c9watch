use crate::cli::pm_fs::{self, PersistedSpawnArgs, WorkerMeta};
use chrono::Utc;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::oneshot;

// ── Types ────────────────────────────────────────────────────────────────────

/// Caller-facing user options for spawning a worker: the set of flags a user
/// passes to `c9watch spawn`. Collected on the CLI side and sent over RPC.
#[derive(Debug, Clone)]
pub struct SpawnArgs {
    pub cwd: String,
    pub name: Option<String>,
    pub append_system_prompt: Option<String>,
    pub permission_mode: String,
    pub model: Option<String>,
    pub add_dirs: Vec<String>,
}

impl SpawnArgs {
    /// Append the optional `claude` flags shared by both worker backends, in
    /// a fixed order: `--append-system-prompt`, `--model`, then one
    /// `--add-dir` per directory. Each backend calls this right after its
    /// `--permission-mode` flag so the print and bg argv cannot drift.
    pub fn apply_optional_flags(&self, cmd: &mut Command) {
        if let Some(ref system_prompt) = self.append_system_prompt {
            cmd.arg("--append-system-prompt").arg(system_prompt);
        }
        if let Some(ref model) = self.model {
            cmd.arg("--model").arg(model);
        }
        for dir in &self.add_dirs {
            cmd.arg("--add-dir").arg(dir);
        }
    }
}

/// Runtime/context values assembled by the daemon at spawn time — identity of
/// the PM that requested the spawn, used for ownership and inbox routing.
#[derive(Debug, Clone, Default)]
pub struct SpawnContext {
    pub spawned_by: Option<String>,
    pub pm_pid: Option<u32>,
}

/// Info needed by `stdout_tee_task` to route inbox events to the spawning PM.
/// `None` when the worker has no `spawnedBy` (human caller) — events are skipped.
#[derive(Debug, Clone)]
pub struct InboxContext {
    pub session_id: String,
    pub spawned_by: String,
}

/// Build the `WorkerMeta` persisted to `meta.json` for a freshly spawned
/// worker. Shared by both backends so the GUI reads identical fields.
fn build_meta(session_id: String, pid: u32, args: SpawnArgs, ctx: SpawnContext) -> WorkerMeta {
    WorkerMeta {
        session_id,
        pid,
        name: args.name,
        cwd: args.cwd,
        spawned_at: Utc::now().to_rfc3339(),
        spawned_by: ctx.spawned_by,
        pm_pid: ctx.pm_pid,
        spawn_args: PersistedSpawnArgs {
            append_system_prompt: args.append_system_prompt,
            permission_mode: args.permission_mode,
            model: args.model,
            add_dirs: args.add_dirs,
        },
        stopped_at: None,
    }
}

/// Inbox routing for a worker: `Some` only when a PM spawned it.
fn inbox_context(meta: &WorkerMeta) -> Option<InboxContext> {
    meta.spawned_by.as_ref().map(|pm| InboxContext {
        session_id: meta.session_id.clone(),
        spawned_by: pm.clone(),
    })
}

pub struct WorkerHandle {
    pub meta: WorkerMeta,
    child: Child,
    /// Held so the worker's stdin stays open: `claude -p` with stream-json
    /// input ends when its stdin closes.
    #[allow(dead_code)]
    stdin: tokio::process::ChildStdin,
    /// Signals that the worker's stdout has produced its first line.
    ready_rx: Option<oneshot::Receiver<()>>,
}

// ── WorkerHandle impl ────────────────────────────────────────────────────────

impl WorkerHandle {
    /// Spawn a new Claude Code worker subprocess. `initial_prompt`, when
    /// given, is written to its stdin as the first user message.
    pub async fn spawn(
        session_id: String,
        args: SpawnArgs,
        ctx: SpawnContext,
        initial_prompt: Option<String>,
    ) -> Result<Self, String> {
        // Build the command
        let mut cmd = Command::new("claude");
        cmd.arg("-p")
            .arg("--input-format=stream-json")
            .arg("--output-format=stream-json")
            .arg(format!("--session-id={}", session_id))
            .arg(format!("--permission-mode={}", args.permission_mode))
            .arg("--verbose");
        args.apply_optional_flags(&mut cmd);

        cmd.current_dir(&args.cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn claude process: {}", e))?;

        let pid = child
            .id()
            .ok_or("Failed to get PID of spawned claude process")?;

        // Build WorkerMeta and persist it
        let meta = build_meta(session_id.clone(), pid, args, ctx);
        pm_fs::write_worker_meta(&meta)?;

        // Grab stdin/stdout/stderr pipes
        let mut child_stdin = child
            .stdin
            .take()
            .ok_or("Failed to take stdin pipe from child")?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or("Failed to take stdout pipe from child")?;
        let child_stderr = child
            .stderr
            .take()
            .ok_or("Failed to take stderr pipe from child")?;

        if let Some(text) = initial_prompt {
            write_user_message(&mut child_stdin, &text).await?;
        }

        let (ready_tx, ready_rx) = oneshot::channel::<()>();

        // Resolve log paths once (synchronous, cheap)
        let stdout_log_path = pm_fs::worker_stdout_log_path(&session_id)
            .map_err(|e| format!("Failed to resolve stdout log path: {}", e))?;
        let stderr_log_path = pm_fs::worker_stderr_log_path(&session_id)
            .map_err(|e| format!("Failed to resolve stderr log path: {}", e))?;

        let inbox_ctx = inbox_context(&meta);

        // Spawn background tasks
        tokio::spawn(stdout_tee_task(
            child_stdout,
            stdout_log_path,
            Some(ready_tx),
            inbox_ctx,
        ));
        tokio::spawn(stderr_tee_task(child_stderr, stderr_log_path));

        Ok(WorkerHandle {
            meta,
            child,
            stdin: child_stdin,
            ready_rx: Some(ready_rx),
        })
    }

    /// Wait until the worker's stdout produces its first line (proof of life).
    /// Returns `Err` if the timeout expires before the worker emits anything.
    pub async fn wait_ready(&mut self, timeout: Duration) -> Result<(), String> {
        let rx = match self.ready_rx.take() {
            Some(r) => r,
            // Already fired — worker was already ready
            None => return Ok(()),
        };
        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| "SPAWN_TIMEOUT: worker did not emit output within timeout".to_string())?
            .map_err(|_| "SPAWN_FAILED: ready channel dropped before signal".to_string())
    }

    /// Returns `true` if the child process is still running.
    pub fn is_alive(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(Some(_)) => false, // exited
            Ok(None) => true,     // still running
            Err(_) => false,      // error querying — treat as dead
        }
    }

    /// Kill the worker subprocess.
    pub async fn kill(&mut self) -> Result<(), String> {
        self.child
            .kill()
            .await
            .map_err(|e| format!("Failed to kill worker process: {}", e))
    }
}

// ── Bg backend handle ─────────────────────────────────────────────────────────

use crate::cli::bg_backend::{self, SettleDetector};
use crate::cli::bg_protocol::{Request, SubscribeEvent};

/// Worker handle backed by `claude --bg` + control.sock RPC.
///
/// Lifecycle:
/// - `spawn` → `claude --bg <prompt>` subprocess returns short, then we open
///   a dedicated subscribe connection.
/// - `kill` → one-shot `kill` RPC + fallback `claude rm` subprocess for
///   jobs-dir cleanup.
pub struct BgWorkerHandle {
    pub meta: WorkerMeta,
    #[allow(dead_code)]
    short: String,
    sock_path: std::path::PathBuf,
    /// Held so `is_alive` can tell when the subscribe stream ends.
    events_rx: Option<tokio::sync::broadcast::Receiver<SubscribeEvent>>,
}

impl BgWorkerHandle {
    pub async fn spawn(
        session_id: String,
        args: SpawnArgs,
        ctx: SpawnContext,
        initial_prompt: String,
    ) -> Result<Self, String> {
        let sock_path = bg_backend::resolve_control_sock()?;

        let worker_name = args
            .name
            .clone()
            .unwrap_or_else(|| format!("c9w-{}", &session_id[..8.min(session_id.len())]));

        let info = bg_backend::spawn_bg(&args, &session_id, &worker_name, &initial_prompt).await?;
        let short = info.short;
        // CC daemon assigns its own sessionId to bg-pinned sessions, ignoring
        // the `--session-id` flag we passed. The detector matches sessions by
        // CC's sessionId (from `claude agents --json`), so we MUST use it as
        // the worker's identity everywhere downstream — meta.json,
        // workers/<dir>, inbox/<dir>, and the daemon state map. The
        // pre-generated `session_id` was a placeholder; from here on,
        // `effective_session_id` is canonical.
        let effective_session_id = info.cc_session_id;

        // bg workers are daemon-managed; the CC daemon owns the actual PTY
        // pid. We can't take ownership of the subprocess here, so leave
        // pid=0 and rely on `claude agents --json` for live pid lookup
        // (see detector refactor PR #107).
        let meta = build_meta(effective_session_id, 0, args, ctx);
        pm_fs::write_worker_meta(&meta)?;

        let inbox_ctx = inbox_context(&meta);

        // Mint two receivers up-front when a PM is attached: one for the
        // long-lived settle watcher, one that `is_alive` holds. Tokio
        // broadcast receivers do NOT replay older messages, so both must
        // exist before any event lands — otherwise a fast initial snapshot
        // could slip past the watcher.
        let receiver_count = if inbox_ctx.is_some() { 2 } else { 1 };
        let (_tx, mut receivers) =
            bg_backend::subscribe(&sock_path, &short, receiver_count).await?;
        let events_rx = receivers.remove(0);

        if let Some(ref ctx) = inbox_ctx {
            let watcher_rx = receivers.pop().expect("subscribe(2) returned <2 receivers");
            let ctx = ctx.clone();
            let short_owned = short.clone();
            tokio::spawn(settle_watcher_task(watcher_rx, ctx, short_owned));
        }

        Ok(BgWorkerHandle {
            meta,
            short,
            sock_path,
            events_rx: Some(events_rx),
        })
    }

    pub async fn wait_ready(&mut self, _timeout: Duration) -> Result<(), String> {
        // Bg workers are ready as soon as `claude --bg` returns the short.
        // The subscribe stream will emit the initial snapshot ~immediately.
        Ok(())
    }

    pub fn is_alive(&self) -> bool {
        // Cheap heuristic: the subscribe broadcast channel is still open.
        // When the reader task drops `tx`, `is_closed` reports true.
        self.events_rx
            .as_ref()
            .map(|rx| !rx.is_closed())
            .unwrap_or(false)
    }

    pub async fn kill(&mut self) -> Result<(), String> {
        let reply = bg_backend::rpc(
            &self.sock_path,
            Request::Kill {
                short: self.short.clone(),
            },
        )
        .await?;
        if !reply.ok {
            eprintln!(
                "[pm_worker bg] kill RPC rejected: {}",
                reply.error.unwrap_or_default()
            );
        }
        // Parallel subprocess `claude rm` for jobs-dir cleanup (no daemon
        // equivalent). Best-effort; ignore errors.
        let short = self.short.clone();
        tokio::spawn(async move {
            let _ = Command::new("claude").arg("rm").arg(&short).output().await;
        });
        Ok(())
    }
}

/// Long-lived task: drains a dedicated subscribe-receiver and emits an inbox
/// event (Done or Awaiting) on every settle transition. Runs for the worker's
/// lifetime; exits when the subscribe stream EOFs (worker killed) or the
/// channel lags out. Only spawned when the worker has a `spawned_by` PM.
async fn settle_watcher_task(
    mut rx: tokio::sync::broadcast::Receiver<SubscribeEvent>,
    ctx: InboxContext,
    short: String,
) {
    use crate::cli::pm_inbox;
    let mut det = SettleDetector::new();
    // Tracks the last state the detector reported as settled, so we only emit
    // on transitions INTO a settled state (not while staying settled).
    let mut last_emitted: Option<String> = None;
    loop {
        let ev = match rx.recv().await {
            Ok(e) => e,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                eprintln!(
                    "[pm_worker bg/{}] settle_watcher lagged {} events; resetting detector",
                    short, n
                );
                det = SettleDetector::new();
                last_emitted = None;
                continue;
            }
        };
        if matches!(ev, SubscribeEvent::Stream { .. }) {
            continue;
        }
        for p in ev.state_patches() {
            det.apply_patch(&p);
        }
        let current = det.last_settled_state().map(String::from);
        if !det.is_settled() {
            // Left the settled state — arm re-detection so the NEXT settle fires.
            last_emitted = None;
            continue;
        }
        if last_emitted == current {
            // Same settle as before; don't re-emit.
            continue;
        }
        let ev_to_write = match current.as_deref() {
            Some("blocked") => {
                let needs = det.needs().unwrap_or("user input").to_string();
                pm_inbox::InboxEvent::awaiting(
                    &ctx.session_id,
                    &ctx.spawned_by,
                    needs,
                    det.detail().map(String::from),
                )
            }
            _ => {
                let assistant_text = det.detail().map(String::from).unwrap_or_default();
                let excerpt = if assistant_text.is_empty() {
                    None
                } else {
                    Some(pm_inbox::truncate_excerpt(&assistant_text))
                };
                pm_inbox::InboxEvent::from_turn_result(
                    &ctx.session_id,
                    &ctx.spawned_by,
                    pm_inbox::TurnResult {
                        status: pm_inbox::EventStatus::Done,
                        duration_ms: None,
                        num_turns: None,
                        stop_reason: current.clone(),
                        total_cost_usd: None,
                        result_excerpt: excerpt,
                        error_message: None,
                    },
                )
            }
        };
        if let Err(e) = pm_inbox::write_event(&ev_to_write) {
            eprintln!("[pm_worker bg/{}] settle_watcher inbox write: {}", short, e);
        }
        last_emitted = current;
    }
}

// ── Backend-agnostic wrapper ──────────────────────────────────────────────────

pub enum AnyWorkerHandle {
    Print(WorkerHandle),
    Bg(BgWorkerHandle),
}

impl AnyWorkerHandle {
    pub fn meta(&self) -> &WorkerMeta {
        match self {
            Self::Print(h) => &h.meta,
            Self::Bg(h) => &h.meta,
        }
    }

    pub async fn wait_ready(&mut self, timeout: Duration) -> Result<(), String> {
        match self {
            Self::Print(h) => h.wait_ready(timeout).await,
            Self::Bg(h) => h.wait_ready(timeout).await,
        }
    }

    pub fn is_alive(&mut self) -> bool {
        match self {
            Self::Print(h) => h.is_alive(),
            Self::Bg(h) => h.is_alive(),
        }
    }

    pub async fn kill(&mut self) -> Result<(), String> {
        match self {
            Self::Print(h) => h.kill().await,
            Self::Bg(h) => h.kill().await,
        }
    }
}

// ── Internal tasks ───────────────────────────────────────────────────────────

/// Write one stream-json user envelope to the worker's stdin.
async fn write_user_message(
    stdin: &mut tokio::process::ChildStdin,
    text: &str,
) -> Result<(), String> {
    let envelope = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": text,
        }
    });
    let mut line = serde_json::to_string(&envelope)
        .map_err(|e| format!("Failed to serialize message: {}", e))?;
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("Failed to write to stdin: {}", e))?;
    stdin
        .flush()
        .await
        .map_err(|e| format!("Failed to flush stdin: {}", e))
}

/// Classify a print-path `result` event into the inbox status and error
/// message. A turn is an error when `is_error` is set or the subtype is
/// anything other than "success". The error message prefers the `subtype`
/// (e.g. "error_during_execution", "error_max_turns") since `stop_reason` is
/// typically only populated on success, then `stop_reason`, then "error".
fn classify_result(
    is_error: bool,
    subtype: Option<&str>,
    stop_reason: Option<&str>,
) -> (crate::cli::pm_inbox::EventStatus, Option<String>) {
    use crate::cli::pm_inbox::EventStatus;
    if is_error || subtype.map(|s| s != "success").unwrap_or(false) {
        let msg = subtype.or(stop_reason).unwrap_or("error").to_string();
        (EventStatus::Error, Some(msg))
    } else {
        (EventStatus::Done, None)
    }
}

/// Reads stdout line-by-line, appends to stdout.log, and writes an inbox
/// event whenever a `"type":"result"` event is seen.
///
/// `ready_tx`: if `Some`, fired once on the first line of output.
async fn stdout_tee_task(
    stdout: tokio::process::ChildStdout,
    log_path: PathBuf,
    mut ready_tx: Option<oneshot::Sender<()>>,
    inbox: Option<InboxContext>,
) {
    use tokio::fs::OpenOptions;

    let mut reader = BufReader::new(stdout).lines();

    // Open log file for appending (create if needed)
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .await;

    let mut log_file = match log_file {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!(
                "[pm_worker] Failed to open stdout log {:?}: {}",
                log_path, e
            );
            None
        }
    };

    let mut saw_terminal_result = false;

    loop {
        let line = match reader.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break, // EOF
            Err(e) => {
                eprintln!("[pm_worker] Error reading stdout line: {}", e);
                break;
            }
        };

        // Signal readiness on first line of output. We considered waiting for
        // the `system:init` event specifically (QA finding M1), but in
        // practice SessionStart hooks run *before* the init event and can
        // legitimately take 10+ seconds — longer than wait_ready's timeout.
        // Any stream-json line out of the worker is proof-of-life enough.
        if let Some(tx) = ready_tx.take() {
            let _ = tx.send(());
        }

        // Append to log
        if let Some(ref mut f) = log_file {
            let log_line = format!("{}\n", line);
            if let Err(e) = f.write_all(log_line.as_bytes()).await {
                eprintln!("[pm_worker] Failed to write to stdout log: {}", e);
            }
        }

        // Parse and handle events
        let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };

        if event.get("type").and_then(|v| v.as_str()) == Some("result") {
            let subtype = event
                .get("subtype")
                .and_then(|v| v.as_str())
                .map(String::from);
            let is_error = event
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let duration_ms = event.get("duration_ms").and_then(|v| v.as_u64());
            let num_turns = event.get("num_turns").and_then(|v| v.as_u64());
            let stop_reason = event
                .get("stop_reason")
                .and_then(|v| v.as_str())
                .map(String::from);
            let total_cost_usd = event.get("total_cost_usd").and_then(|v| v.as_f64());
            let result_text = event
                .get("result")
                .and_then(|v| v.as_str())
                .map(String::from);
            saw_terminal_result = true;

            if let Some(ref ctx) = inbox {
                use crate::cli::pm_inbox::{self, InboxEvent, TurnResult};
                let (status, err_msg) =
                    classify_result(is_error, subtype.as_deref(), stop_reason.as_deref());
                let excerpt = result_text.as_ref().map(|s| pm_inbox::truncate_excerpt(s));
                let ev = InboxEvent::from_turn_result(
                    &ctx.session_id,
                    &ctx.spawned_by,
                    TurnResult {
                        status,
                        duration_ms,
                        num_turns,
                        stop_reason,
                        total_cost_usd,
                        result_excerpt: excerpt,
                        error_message: err_msg,
                    },
                );
                if let Err(e) = pm_inbox::write_event(&ev) {
                    eprintln!("[pm_worker] Failed to write inbox event: {}", e);
                }
            }
        }
    }

    // If the stdout closed without a terminal `result` event, the worker likely
    // crashed or was killed mid-turn. Emit a Crashed event so the PM learns.
    if !saw_terminal_result {
        if let Some(ref ctx) = inbox {
            use crate::cli::pm_inbox::{self, InboxEvent};
            let ev = InboxEvent::crashed(
                &ctx.session_id,
                &ctx.spawned_by,
                "worker stdout closed without a terminal result event".to_string(),
            );
            if let Err(e) = pm_inbox::write_event(&ev) {
                eprintln!("[pm_worker] Failed to write crash inbox event: {}", e);
            }
        }
    }
}

/// Reads stderr line-by-line and appends to stderr.log.
async fn stderr_tee_task(stderr: tokio::process::ChildStderr, log_path: PathBuf) {
    use tokio::fs::OpenOptions;

    let mut reader = BufReader::new(stderr).lines();

    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .await;

    let mut log_file = match log_file {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!(
                "[pm_worker] Failed to open stderr log {:?}: {}",
                log_path, e
            );
            None
        }
    };

    loop {
        let line = match reader.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                eprintln!("[pm_worker] Error reading stderr line: {}", e);
                break;
            }
        };

        if let Some(ref mut f) = log_file {
            let log_line = format!("{}\n", line);
            if let Err(e) = f.write_all(log_line.as_bytes()).await {
                eprintln!("[pm_worker] Failed to write to stderr log: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_result_success_is_done() {
        use crate::cli::pm_inbox::EventStatus;
        assert_eq!(
            classify_result(false, Some("success"), Some("end_turn")),
            (EventStatus::Done, None)
        );
    }

    #[test]
    fn classify_result_non_success_subtype_is_error_with_subtype() {
        use crate::cli::pm_inbox::EventStatus;
        assert_eq!(
            classify_result(false, Some("error_max_turns"), Some("x")),
            (EventStatus::Error, Some("error_max_turns".to_string()))
        );
    }

    #[test]
    fn classify_result_is_error_without_subtype_uses_stop_reason() {
        use crate::cli::pm_inbox::EventStatus;
        assert_eq!(
            classify_result(true, None, Some("x")),
            (EventStatus::Error, Some("x".to_string()))
        );
    }

    #[test]
    fn classify_result_is_error_alone_falls_back_to_error() {
        use crate::cli::pm_inbox::EventStatus;
        assert_eq!(
            classify_result(true, None, None),
            (EventStatus::Error, Some("error".to_string()))
        );
    }

    fn sample_args() -> SpawnArgs {
        SpawnArgs {
            cwd: "/tmp/proj".to_string(),
            name: Some("w1".to_string()),
            append_system_prompt: Some("be brief".to_string()),
            permission_mode: "acceptEdits".to_string(),
            model: Some("opus".to_string()),
            add_dirs: vec!["/a".to_string(), "/b".to_string()],
        }
    }

    fn argv(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn apply_optional_flags_pins_order() {
        let mut cmd = Command::new("claude");
        sample_args().apply_optional_flags(&mut cmd);
        assert_eq!(
            argv(&cmd),
            vec![
                "--append-system-prompt",
                "be brief",
                "--model",
                "opus",
                "--add-dir",
                "/a",
                "--add-dir",
                "/b",
            ]
        );
    }

    #[test]
    fn apply_optional_flags_skips_unset() {
        let mut args = sample_args();
        args.append_system_prompt = None;
        args.model = None;
        args.add_dirs.clear();
        let mut cmd = Command::new("claude");
        args.apply_optional_flags(&mut cmd);
        assert!(argv(&cmd).is_empty());
    }

    #[test]
    fn build_meta_copies_args_and_ctx() {
        let ctx = SpawnContext {
            spawned_by: Some("pm-uuid".to_string()),
            pm_pid: Some(42),
        };
        let meta = build_meta("sess-1".to_string(), 7, sample_args(), ctx);
        assert_eq!(meta.session_id, "sess-1");
        assert_eq!(meta.pid, 7);
        assert_eq!(meta.name.as_deref(), Some("w1"));
        assert_eq!(meta.cwd, "/tmp/proj");
        assert_eq!(meta.spawned_by.as_deref(), Some("pm-uuid"));
        assert_eq!(meta.pm_pid, Some(42));
        assert_eq!(
            meta.spawn_args,
            PersistedSpawnArgs {
                append_system_prompt: Some("be brief".to_string()),
                permission_mode: "acceptEdits".to_string(),
                model: Some("opus".to_string()),
                add_dirs: vec!["/a".to_string(), "/b".to_string()],
            }
        );
        assert!(meta.stopped_at.is_none());
        assert!(chrono::DateTime::parse_from_rfc3339(&meta.spawned_at).is_ok());

        let inbox = inbox_context(&meta).expect("PM-spawned worker has inbox ctx");
        assert_eq!(inbox.session_id, "sess-1");
        assert_eq!(inbox.spawned_by, "pm-uuid");
    }

    #[test]
    fn inbox_context_none_without_pm() {
        let meta = build_meta("s".to_string(), 0, sample_args(), SpawnContext::default());
        assert!(inbox_context(&meta).is_none());
    }
}
