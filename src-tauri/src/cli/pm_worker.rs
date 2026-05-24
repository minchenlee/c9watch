use crate::cli::pm_fs::{self, PersistedSpawnArgs, WorkerMeta};
use chrono::Utc;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

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

/// Runtime/context values assembled by the daemon at spawn time — identity of
/// the PM that requested the spawn, used for ownership and inbox routing.
#[derive(Debug, Clone, Default)]
pub struct SpawnContext {
    pub spawned_by: Option<String>,
    pub pm_pid: Option<u32>,
}

struct StdinMessage {
    text: String,
    ack: oneshot::Sender<Result<(), String>>,
}

#[derive(Debug, Clone, Default)]
pub struct TurnResult {
    pub assistant_text: String,
    pub ended_at: String,
    /// `result` event subtype: "success", "error_during_execution", "error_max_turns", etc.
    pub subtype: Option<String>,
    pub is_error: bool,
    pub duration_ms: Option<u64>,
    pub num_turns: Option<u64>,
    pub stop_reason: Option<String>,
    pub total_cost_usd: Option<f64>,
    /// The `result` field from the stream-json result event (final assistant reply).
    pub result_text: Option<String>,
}

/// Info needed by `stdout_tee_task` to route inbox events to the spawning PM.
/// `None` when the worker has no `spawnedBy` (human caller) — events are skipped.
#[derive(Debug, Clone)]
pub struct InboxContext {
    pub session_id: String,
    pub spawned_by: String,
}

pub struct WorkerHandle {
    pub meta: WorkerMeta,
    child: Child,
    stdin_tx: mpsc::Sender<StdinMessage>,
    /// Wrapped in Option so it can be taken out while waiting without holding
    /// the daemon state Mutex.
    result_rx: Option<mpsc::Receiver<TurnResult>>,
    /// Held so the channel stays open as long as the handle lives.
    #[allow(dead_code)]
    result_tx: mpsc::Sender<TurnResult>,
    /// Signals that the worker's stdout has produced its first line.
    ready_rx: Option<oneshot::Receiver<()>>,
}

// ── WorkerHandle impl ────────────────────────────────────────────────────────

impl WorkerHandle {
    /// Spawn a new Claude Code worker subprocess.
    pub async fn spawn(
        session_id: String,
        args: SpawnArgs,
        ctx: SpawnContext,
    ) -> Result<Self, String> {
        // Build the command
        let mut cmd = Command::new("claude");
        cmd.arg("-p")
            .arg("--input-format=stream-json")
            .arg("--output-format=stream-json")
            .arg(format!("--session-id={}", session_id))
            .arg(format!("--permission-mode={}", args.permission_mode))
            .arg("--verbose");

        if let Some(ref system_prompt) = args.append_system_prompt {
            cmd.arg("--append-system-prompt").arg(system_prompt);
        }

        if let Some(ref model) = args.model {
            cmd.arg("--model").arg(model);
        }

        for dir in &args.add_dirs {
            cmd.arg("--add-dir").arg(dir);
        }

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
        let meta = WorkerMeta {
            session_id: session_id.clone(),
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
        };
        pm_fs::write_worker_meta(&meta)?;

        // Grab stdin/stdout/stderr pipes
        let child_stdin = child
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

        // Channels
        let (stdin_tx, stdin_rx) = mpsc::channel::<StdinMessage>(32);
        let (result_tx, result_rx) = mpsc::channel::<TurnResult>(16);
        let (ready_tx, ready_rx) = oneshot::channel::<()>();

        // Resolve log paths once (synchronous, cheap)
        let stdout_log_path = pm_fs::worker_stdout_log_path(&session_id)
            .map_err(|e| format!("Failed to resolve stdout log path: {}", e))?;
        let stderr_log_path = pm_fs::worker_stderr_log_path(&session_id)
            .map_err(|e| format!("Failed to resolve stderr log path: {}", e))?;

        let inbox_ctx = meta.spawned_by.as_ref().map(|pm| InboxContext {
            session_id: session_id.clone(),
            spawned_by: pm.clone(),
        });

        // Spawn background tasks
        tokio::spawn(stdin_writer_task(stdin_rx, child_stdin));
        tokio::spawn(stdout_tee_task(
            child_stdout,
            stdout_log_path,
            result_tx.clone(),
            Some(ready_tx),
            inbox_ctx,
        ));
        tokio::spawn(stderr_tee_task(child_stderr, stderr_log_path));

        Ok(WorkerHandle {
            meta,
            child,
            stdin_tx,
            result_rx: Some(result_rx),
            result_tx,
            ready_rx: Some(ready_rx),
        })
    }

    /// Send a user message to the worker's stdin.
    pub async fn send_message(&self, text: &str) -> Result<(), String> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.stdin_tx
            .send(StdinMessage {
                text: text.to_string(),
                ack: ack_tx,
            })
            .await
            .map_err(|_| "stdin channel closed — worker may have exited".to_string())?;

        ack_rx
            .await
            .map_err(|_| "ack channel dropped before receiving response".to_string())?
    }

    /// Wait for the next completed turn (result event from stdout).
    ///
    /// Returns `Err("WAIT_TIMEOUT")` if the timeout elapses before a result arrives.
    pub async fn wait_for_turn(&mut self, timeout: Duration) -> Result<TurnResult, String> {
        let rx = self
            .result_rx
            .as_mut()
            .ok_or("result_rx not available — another --wait is pending".to_string())?;
        tokio::time::timeout(timeout, rx.recv())
            .await
            .map_err(|_| "WAIT_TIMEOUT".to_string())?
            .ok_or("result channel closed — worker stdout tee task exited".to_string())
    }

    /// Take the result receiver so it can be awaited outside the daemon state lock.
    /// Returns `None` if already taken (i.e., another `--wait` is in flight).
    pub fn take_result_receiver(&mut self) -> Option<mpsc::Receiver<TurnResult>> {
        self.result_rx.take()
    }

    /// Return the result receiver after awaiting outside the lock.
    pub fn return_result_receiver(&mut self, rx: mpsc::Receiver<TurnResult>) {
        self.result_rx = Some(rx);
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
/// - `send_message` → one-shot `reply` RPC on a fresh connection.
/// - `wait_for_turn` → consume subscribe events until SettleDetector reports
///   settled (`done` or `blocked`).
/// - `kill` → one-shot `kill` RPC + fallback `claude rm` subprocess for
///   jobs-dir cleanup.
pub struct BgWorkerHandle {
    pub meta: WorkerMeta,
    #[allow(dead_code)]
    short: String,
    sock_path: std::path::PathBuf,
    events_rx: Option<tokio::sync::broadcast::Receiver<SubscribeEvent>>,
    #[allow(dead_code)]
    ready_fired: bool,
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

        let short =
            bg_backend::spawn_bg(&args, &session_id, &worker_name, &initial_prompt).await?;

        let meta = WorkerMeta {
            session_id: session_id.clone(),
            // bg workers are daemon-managed; the CC daemon owns the actual PTY
            // pid. We can't take ownership of the subprocess here, so leave
            // pid=0 and rely on `claude agents --json` for live pid lookup
            // (see detector refactor PR #107).
            pid: 0,
            name: args.name.clone(),
            cwd: args.cwd.clone(),
            spawned_at: Utc::now().to_rfc3339(),
            spawned_by: ctx.spawned_by.clone(),
            pm_pid: ctx.pm_pid,
            spawn_args: PersistedSpawnArgs {
                append_system_prompt: args.append_system_prompt,
                permission_mode: args.permission_mode,
                model: args.model,
                add_dirs: args.add_dirs,
            },
            stopped_at: None,
        };
        pm_fs::write_worker_meta(&meta)?;

        let inbox_ctx = ctx.spawned_by.as_ref().map(|pm| InboxContext {
            session_id: session_id.clone(),
            spawned_by: pm.clone(),
        });

        // Mint two receivers up-front when a PM is attached: one for the
        // long-lived settle watcher, one for `wait_for_turn` callers. Tokio
        // broadcast receivers do NOT replay older messages, so both must
        // exist before any event lands — otherwise a fast initial snapshot
        // could slip past the second subscriber.
        let receiver_count = if inbox_ctx.is_some() { 2 } else { 1 };
        let (_tx, mut receivers) =
            bg_backend::subscribe(&sock_path, &short, receiver_count).await?;
        let events_rx = receivers.remove(0);

        if let Some(ref ctx) = inbox_ctx {
            let watcher_rx = receivers
                .pop()
                .expect("subscribe(2) returned <2 receivers");
            let ctx = ctx.clone();
            let short_owned = short.clone();
            tokio::spawn(settle_watcher_task(watcher_rx, ctx, short_owned));
        }

        Ok(BgWorkerHandle {
            meta,
            short,
            sock_path,
            events_rx: Some(events_rx),
            ready_fired: false,
        })
    }

    pub async fn send_message(&self, text: &str) -> Result<(), String> {
        let reply = bg_backend::rpc(
            &self.sock_path,
            Request::Reply {
                short: self.short.clone(),
                text: text.to_string(),
            },
        )
        .await?;
        if !reply.ok {
            return Err(format!(
                "daemon rejected reply: {}",
                reply.error.unwrap_or_default()
            ));
        }
        Ok(())
    }

    /// Wait for the next settle (done or blocked). Drains pid-only/active
    /// patches via SettleDetector. Returns `WAIT_TIMEOUT` error string on
    /// timeout, mirroring the print backend's contract.
    pub async fn wait_for_turn(&mut self, timeout: Duration) -> Result<TurnResult, String> {
        let rx = self
            .events_rx
            .as_mut()
            .ok_or_else(|| "events_rx not available".to_string())?;
        let mut det = SettleDetector::new();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .ok_or_else(|| "WAIT_TIMEOUT".to_string())?;
            let ev = tokio::time::timeout(remaining, rx.recv())
                .await
                .map_err(|_| "WAIT_TIMEOUT".to_string())?
                .map_err(|e| format!("event channel closed: {}", e))?;

            self.ready_fired = true;

            match ev {
                SubscribeEvent::Snapshot { record, .. } => {
                    // Try nested `currentState` object first (some daemon versions
                    // wrap state under it); fall back to top-level fields on the
                    // record itself (`record.state`, `record.tempo`, ...), which
                    // is what the round-trip PoC observed and what the integration
                    // test exercises.
                    let nested = record
                        .get("currentState")
                        .and_then(|v| {
                            serde_json::from_value::<crate::cli::bg_protocol::StatePatch>(v.clone())
                                .ok()
                        });
                    let from_record = serde_json::from_value::<crate::cli::bg_protocol::StatePatch>(
                        record.clone(),
                    )
                    .ok();
                    if let Some(p) = nested {
                        det.apply_patch(&p);
                    }
                    if let Some(p) = from_record {
                        det.apply_patch(&p);
                    }
                    if det.is_settled() {
                        return Ok(self.build_turn_result(&det));
                    }
                }
                SubscribeEvent::State { patch } => {
                    det.apply_patch(&patch);
                    if det.is_settled() {
                        return Ok(self.build_turn_result(&det));
                    }
                }
                SubscribeEvent::Stream { .. } => {} // filtered upstream, but be safe
            }
        }
    }

    fn build_turn_result(&self, det: &SettleDetector) -> TurnResult {
        // Inbox event emission lives in `settle_watcher_task`, which has a
        // dedicated subscribe-receiver and fires on EVERY settle (not just
        // when a `--wait` caller is listening). This function only constructs
        // the TurnResult for the optional `--wait` consumer.
        let assistant_text = det.detail().map(String::from).unwrap_or_default();
        TurnResult {
            assistant_text,
            ended_at: Utc::now().to_rfc3339(),
            subtype: det.last_settled_state().map(String::from),
            is_error: false,
            duration_ms: None,
            num_turns: None,
            stop_reason: det.last_settled_state().map(String::from),
            total_cost_usd: None,
            result_text: det.detail().map(String::from),
        }
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
        match ev {
            SubscribeEvent::Snapshot { ref record, .. } => {
                let nested = record.get("currentState").and_then(|v| {
                    serde_json::from_value::<crate::cli::bg_protocol::StatePatch>(v.clone()).ok()
                });
                let from_record = serde_json::from_value::<crate::cli::bg_protocol::StatePatch>(
                    record.clone(),
                )
                .ok();
                if let Some(p) = nested {
                    det.apply_patch(&p);
                }
                if let Some(p) = from_record {
                    det.apply_patch(&p);
                }
            }
            SubscribeEvent::State { ref patch } => {
                det.apply_patch(patch);
            }
            SubscribeEvent::Stream { .. } => continue,
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

    pub async fn send_message(&self, text: &str) -> Result<(), String> {
        match self {
            Self::Print(h) => h.send_message(text).await,
            Self::Bg(h) => h.send_message(text).await,
        }
    }

    pub async fn wait_for_turn(&mut self, timeout: Duration) -> Result<TurnResult, String> {
        match self {
            Self::Print(h) => h.wait_for_turn(timeout).await,
            Self::Bg(h) => h.wait_for_turn(timeout).await,
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

    /// Result-receiver methods only apply to the Print backend (Bg uses event
    /// stream subscription via `wait_for_turn` instead). Bg returns None so
    /// callers can branch on `matches!(handle, AnyWorkerHandle::Bg(_))` and
    /// avoid the receiver-take dance.
    pub fn take_result_receiver(&mut self) -> Option<mpsc::Receiver<TurnResult>> {
        match self {
            Self::Print(h) => h.take_result_receiver(),
            Self::Bg(_) => None,
        }
    }

    pub fn return_result_receiver(&mut self, rx: mpsc::Receiver<TurnResult>) {
        match self {
            Self::Print(h) => h.return_result_receiver(rx),
            Self::Bg(_) => {} // no-op
        }
    }
}

// ── Internal tasks ───────────────────────────────────────────────────────────

/// Reads StdinMessages from the channel and writes stream-json user envelopes
/// to the child's stdin pipe.
async fn stdin_writer_task(
    mut rx: mpsc::Receiver<StdinMessage>,
    mut stdin: tokio::process::ChildStdin,
) {
    while let Some(msg) = rx.recv().await {
        let envelope = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": msg.text,
            }
        });

        let line = match serde_json::to_string(&envelope) {
            Ok(s) => s,
            Err(e) => {
                let _ = msg.ack.send(Err(format!("Failed to serialize message: {}", e)));
                continue;
            }
        };

        let result = async {
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| format!("Failed to write to stdin: {}", e))?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|e| format!("Failed to write newline to stdin: {}", e))?;
            stdin
                .flush()
                .await
                .map_err(|e| format!("Failed to flush stdin: {}", e))?;
            Ok(())
        }
        .await;

        let _ = msg.ack.send(result);
    }
}

/// Reads stdout line-by-line, appends to stdout.log, and sends TurnResults
/// whenever a `"type":"result"` event is seen.
///
/// `ready_tx`: if `Some`, fired once on the first line of output.
async fn stdout_tee_task(
    stdout: tokio::process::ChildStdout,
    log_path: PathBuf,
    result_tx: mpsc::Sender<TurnResult>,
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
            eprintln!("[pm_worker] Failed to open stdout log {:?}: {}", log_path, e);
            None
        }
    };

    // Accumulate assistant text between result events
    let mut assistant_buf = String::new();
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

        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match event_type {
            "assistant" => {
                // Collect text from message.content (string or array of text blocks)
                if let Some(message) = event.get("message") {
                    let text = extract_content_text(message.get("content"));
                    if !text.is_empty() {
                        if !assistant_buf.is_empty() {
                            assistant_buf.push('\n');
                        }
                        assistant_buf.push_str(&text);
                    }
                }
            }
            "result" => {
                let subtype = event.get("subtype").and_then(|v| v.as_str()).map(String::from);
                let is_error = event.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                let duration_ms = event.get("duration_ms").and_then(|v| v.as_u64());
                let num_turns = event.get("num_turns").and_then(|v| v.as_u64());
                let stop_reason = event.get("stop_reason").and_then(|v| v.as_str()).map(String::from);
                let total_cost_usd = event.get("total_cost_usd").and_then(|v| v.as_f64());
                let result_text = event.get("result").and_then(|v| v.as_str()).map(String::from);

                let result = TurnResult {
                    assistant_text: std::mem::take(&mut assistant_buf),
                    ended_at: Utc::now().to_rfc3339(),
                    subtype: subtype.clone(),
                    is_error,
                    duration_ms,
                    num_turns,
                    stop_reason: stop_reason.clone(),
                    total_cost_usd,
                    result_text: result_text.clone(),
                };
                // Non-blocking send: if receiver is gone, we just continue
                let _ = result_tx.send(result).await;
                saw_terminal_result = true;

                if let Some(ref ctx) = inbox {
                    use crate::cli::pm_inbox::{self, EventStatus, InboxEvent, TurnResult};
                    let status = if is_error || subtype.as_deref().map(|s| s != "success").unwrap_or(false) {
                        EventStatus::Error
                    } else {
                        EventStatus::Done
                    };
                    let excerpt = result_text
                        .as_ref()
                        .map(|s| pm_inbox::truncate_excerpt(s));
                    let err_msg = if matches!(status, EventStatus::Error) {
                        // Prefer the result `subtype` (e.g. "error_during_execution",
                        // "error_max_turns") since `stop_reason` is typically only
                        // populated on success.
                        Some(
                            subtype
                                .clone()
                                .or_else(|| stop_reason.clone())
                                .unwrap_or_else(|| "error".to_string()),
                        )
                    } else {
                        None
                    };
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
            _ => {}
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

/// Extract text from a Claude content field, which may be:
/// - a plain string
/// - an array of content blocks with `{"type":"text","text":"..."}` entries
fn extract_content_text(content: Option<&serde_json::Value>) -> String {
    match content {
        None => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    block.get("text").and_then(|t| t.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
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
            eprintln!("[pm_worker] Failed to open stderr log {:?}: {}", log_path, e);
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
