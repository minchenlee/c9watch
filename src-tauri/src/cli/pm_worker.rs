use crate::cli::pm_fs::{self, SpawnArgs, WorkerMeta};
use chrono::Utc;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

// ── Types ────────────────────────────────────────────────────────────────────

struct StdinMessage {
    text: String,
    ack: oneshot::Sender<Result<(), String>>,
}

#[derive(Debug, Clone)]
pub struct TurnResult {
    pub assistant_text: String,
    pub ended_at: String,
}

pub struct WorkerHandle {
    pub meta: WorkerMeta,
    child: Child,
    stdin_tx: mpsc::Sender<StdinMessage>,
    result_rx: mpsc::Receiver<TurnResult>,
    /// Held so the channel stays open as long as the handle lives.
    #[allow(dead_code)]
    result_tx: mpsc::Sender<TurnResult>,
}

// ── WorkerHandle impl ────────────────────────────────────────────────────────

impl WorkerHandle {
    /// Spawn a new Claude Code worker subprocess.
    pub async fn spawn(
        session_id: String,
        cwd: String,
        name: Option<String>,
        args: SpawnArgs,
        spawned_by: Option<String>,
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

        cmd.current_dir(&cwd)
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
            name,
            cwd: cwd.clone(),
            spawned_at: Utc::now().to_rfc3339(),
            spawned_by,
            spawn_args: args,
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

        // Resolve log paths once (synchronous, cheap)
        let stdout_log_path = pm_fs::worker_stdout_log_path(&session_id)
            .map_err(|e| format!("Failed to resolve stdout log path: {}", e))?;
        let stderr_log_path = pm_fs::worker_stderr_log_path(&session_id)
            .map_err(|e| format!("Failed to resolve stderr log path: {}", e))?;

        // Spawn background tasks
        tokio::spawn(stdin_writer_task(stdin_rx, child_stdin));
        tokio::spawn(stdout_tee_task(
            child_stdout,
            stdout_log_path,
            result_tx.clone(),
        ));
        tokio::spawn(stderr_tee_task(child_stderr, stderr_log_path));

        Ok(WorkerHandle {
            meta,
            child,
            stdin_tx,
            result_rx,
            result_tx,
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
        tokio::time::timeout(timeout, self.result_rx.recv())
            .await
            .map_err(|_| "WAIT_TIMEOUT".to_string())?
            .ok_or("result channel closed — worker stdout tee task exited".to_string())
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
async fn stdout_tee_task(
    stdout: tokio::process::ChildStdout,
    log_path: PathBuf,
    result_tx: mpsc::Sender<TurnResult>,
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

    loop {
        let line = match reader.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break, // EOF
            Err(e) => {
                eprintln!("[pm_worker] Error reading stdout line: {}", e);
                break;
            }
        };

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
                let result = TurnResult {
                    assistant_text: std::mem::take(&mut assistant_buf),
                    ended_at: Utc::now().to_rfc3339(),
                };
                // Non-blocking send: if receiver is gone, we just continue
                let _ = result_tx.send(result).await;
            }
            _ => {}
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
