//! BgBackend: spawn and manage Claude Code workers via `claude --bg`
//! and `/tmp/cc-daemon-{uid}/{host-id}/control.sock` JSON-RPC.

use std::path::PathBuf;

/// Find the CC daemon's control.sock for the current user.
///
/// Pattern: `/tmp/cc-daemon-{uid}/{host-id}/control.sock` where `host-id`
/// is a per-machine hash chosen by the daemon. Typically exactly one
/// host-id subdirectory exists per user.
///
/// Returns `Err` if the daemon has never run on this machine OR the
/// host directory is missing.
pub fn resolve_control_sock() -> Result<PathBuf, String> {
    let uid = unsafe { libc::getuid() };
    let base = PathBuf::from(format!("/tmp/cc-daemon-{}", uid));

    if !base.exists() {
        return Err(format!(
            "CC daemon socket dir not found: {} (is claude installed and has it ever run?)",
            base.display()
        ));
    }

    // Pick the first (typically only) host-id subdir.
    let entries = std::fs::read_dir(&base)
        .map_err(|e| format!("read_dir {}: {}", base.display(), e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let sock = path.join("control.sock");
        if sock.exists() {
            return Ok(sock);
        }
    }

    Err(format!(
        "no control.sock found under {} (daemon may be down)",
        base.display()
    ))
}

use crate::cli::bg_protocol::{OneShotReply, Request, StatePatch};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Send one request, read one reply, drop the connection.
///
/// The daemon closes the connection after each one-shot reply, so this is
/// the only safe pattern for `reply`/`kill`/`nudge`/`ping`. Do NOT use for
/// `subscribe` (subscribe takes over the connection — use [`subscribe`]).
pub async fn rpc(sock: &Path, req: Request) -> Result<OneShotReply, String> {
    let wire = req.to_wire();

    let mut conn = tokio::time::timeout(
        Duration::from_secs(2),
        UnixStream::connect(sock),
    )
    .await
    .map_err(|_| "connect timeout".to_string())?
    .map_err(|e| format!("connect {}: {}", sock.display(), e))?;

    conn.write_all(wire.as_bytes())
        .await
        .map_err(|e| format!("write request: {}", e))?;
    conn.write_all(b"\n")
        .await
        .map_err(|e| format!("write newline: {}", e))?;
    conn.flush().await.map_err(|e| format!("flush: {}", e))?;

    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
        .await
        .map_err(|_| "reply timeout".to_string())?
        .map_err(|e| format!("read reply: {}", e))?;

    if line.is_empty() {
        return Err("daemon closed connection without reply".to_string());
    }

    serde_json::from_str::<OneShotReply>(line.trim())
        .map_err(|e| format!("parse reply {:?}: {}", line, e))
}

/// Parse the `backgrounded · <short> [· <name>]` line emitted by
/// `claude --bg`. Tolerates middle dot (·) and bullet (•) separators
/// and optional name / idle-hint suffixes.
///
/// Returns the 8-hex `short` ID (= sessionId prefix) on success.
pub fn parse_short_from_spawn(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        // Skip lines that don't start with the expected prefix — bg spawn may
        // emit banners, blank lines, or other notices before the real line.
        let Some(rest) = trimmed.strip_prefix("backgrounded") else {
            continue;
        };
        let after_sep = rest
            .trim_start()
            .trim_start_matches(['·', '•', '-', ':'])
            .trim_start();
        // First whitespace-delimited token should be 8 hex chars.
        let token: String = after_sep
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        if token.len() == 8 {
            return Some(token);
        }
    }
    None
}

use crate::cli::pm_worker::SpawnArgs;
use tokio::process::Command;

/// Spawn a new bg-pinned worker via `claude --bg`. Returns the 8-hex `short`.
///
/// Requires CC >= 2.1.150 with the daemon running. The initial prompt is
/// REQUIRED (idle-spawn + later `reply` was found unreliable in Phase 0).
pub async fn spawn_bg(
    args: &SpawnArgs,
    session_id: &str,
    worker_name: &str,
    initial_prompt: &str,
) -> Result<String, String> {
    let mut cmd = Command::new("claude");
    cmd.arg("--bg")
        .arg("--session-id")
        .arg(session_id)
        .arg("--name")
        .arg(worker_name)
        .arg("--permission-mode")
        .arg(&args.permission_mode);

    if let Some(ref sp) = args.append_system_prompt {
        cmd.arg("--append-system-prompt").arg(sp);
    }
    if let Some(ref model) = args.model {
        cmd.arg("--model").arg(model);
    }
    for dir in &args.add_dirs {
        cmd.arg("--add-dir").arg(dir);
    }
    cmd.arg(initial_prompt);
    cmd.current_dir(&args.cwd);

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("claude --bg spawn failed: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "claude --bg exited {:?}: stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_short_from_spawn(&stdout).ok_or_else(|| {
        format!(
            "claude --bg did not emit 'backgrounded · <short>' line; stdout={:?}",
            stdout
        )
    })
}

/// Tracks whether a worker has settled into a turn-end state (done or blocked).
///
/// Per Phase 0 findings: pid-only patches and `tempo:"active"` patches mid-turn
/// must NOT count as settle. Only an explicit `state:"done"` or `state:"blocked"`
/// transition is authoritative.
#[derive(Debug, Default)]
pub struct SettleDetector {
    state: Option<String>,
    tempo: Option<String>,
    needs: Option<String>,
    detail: Option<String>,
}

impl SettleDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_patch(&mut self, patch: &StatePatch) {
        if let Some(s) = &patch.state {
            self.state = Some(s.clone());
        }
        if let Some(t) = &patch.tempo {
            self.tempo = Some(t.clone());
            // Worker started a new turn — invalidate the previous settle so
            // is_settled() flips back to false and a subsequent done/blocked
            // is recognised as a NEW settle. The daemon never emits a fresh
            // state:active patch; only tempo carries the resumption signal.
            if matches!(t.as_str(), "active" | "running") {
                self.state = None;
                self.needs = None;
            }
        }
        if let Some(n) = &patch.needs {
            self.needs = Some(n.clone());
        }
        if let Some(d) = &patch.detail {
            self.detail = Some(d.clone());
        }
    }

    pub fn is_settled(&self) -> bool {
        matches!(self.state.as_deref(), Some("done") | Some("blocked"))
    }

    pub fn last_settled_state(&self) -> Option<&str> {
        self.state.as_deref()
    }

    pub fn needs(&self) -> Option<&str> {
        self.needs.as_deref()
    }

    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

use tokio::sync::broadcast;

/// Open a dedicated control.sock connection and subscribe to `short`'s events.
///
/// Returns `(Sender, Vec<Receiver>)` where the receivers count matches
/// `receiver_count`. Receivers are minted BEFORE the reader task starts
/// streaming events so callers cannot race past the initial snapshot.
///
/// Tokio broadcast receivers start at the current channel position when
/// `subscribe()` is called; they do NOT replay older messages. So all
/// downstream consumers MUST be created up-front.
///
/// Reader task discards `Stream` frames (ANSI PTY redraws). Reader exits on
/// connection EOF or when all receivers are dropped.
pub async fn subscribe(
    sock: &std::path::Path,
    short: &str,
    receiver_count: usize,
) -> Result<
    (
        broadcast::Sender<crate::cli::bg_protocol::SubscribeEvent>,
        Vec<broadcast::Receiver<crate::cli::bg_protocol::SubscribeEvent>>,
    ),
    String,
> {
    use crate::cli::bg_protocol::SubscribeEvent;

    assert!(receiver_count >= 1, "subscribe requires receiver_count >= 1");

    let mut conn = UnixStream::connect(sock)
        .await
        .map_err(|e| format!("subscribe connect {}: {}", sock.display(), e))?;

    let wire = Request::Subscribe {
        short: short.to_string(),
    }
    .to_wire();
    conn.write_all(wire.as_bytes())
        .await
        .map_err(|e| format!("subscribe write: {}", e))?;
    conn.write_all(b"\n")
        .await
        .map_err(|e| format!("subscribe newline: {}", e))?;
    conn.flush().await.map_err(|e| format!("subscribe flush: {}", e))?;

    let (tx, first_rx) = broadcast::channel::<SubscribeEvent>(64);
    // Mint all requested receivers SYNCHRONOUSLY before spawning the reader
    // task so the channel is fully provisioned. New tokio broadcast receivers
    // start at the current sender cursor — late subscribers do NOT replay
    // older sends. So additional receivers must exist before any event flows.
    let mut receivers = Vec::with_capacity(receiver_count);
    receivers.push(first_rx);
    for _ in 1..receiver_count {
        receivers.push(tx.subscribe());
    }

    let reader_tx = tx.clone();
    let short_owned = short.to_string();
    tokio::spawn(async move {
        let mut reader = BufReader::new(conn);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    eprintln!("[bg_backend] subscribe({}) EOF", short_owned);
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("[bg_backend] subscribe({}) read err: {}", short_owned, e);
                    break;
                }
            }
            let ev = match serde_json::from_str::<SubscribeEvent>(line.trim()) {
                Ok(e) => e,
                Err(_) => continue, // skip unparseable frames silently
            };
            if matches!(ev, SubscribeEvent::Stream { .. }) {
                continue; // discard PTY frames
            }
            // Drop the event when all receivers are gone (post-kill); otherwise
            // it lands on every active receiver's queue.
            let _ = reader_tx.send(ev);
        }
    });

    Ok((tx, receivers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_returns_path_or_clear_error() {
        // Best-effort: if claude has run on this machine, path resolves.
        // If not, error message must mention the missing dir.
        match resolve_control_sock() {
            Ok(p) => assert!(p.ends_with("control.sock")),
            Err(e) => assert!(e.contains("/tmp/cc-daemon-")),
        }
    }

    #[test]
    fn rpc_request_serializes_one_line_no_trailing_newline() {
        use crate::cli::bg_protocol::Request;
        let req = Request::Kill { short: "abc12345".to_string() };
        let wire = req.to_wire();
        assert!(!wire.contains('\n'), "to_wire must not embed newlines");
        assert!(wire.starts_with('{') && wire.ends_with('}'));
    }

    #[test]
    fn parse_backgrounded_skips_leading_noise() {
        // Real spawns may emit blank lines, banners, or notices before the
        // "backgrounded · <short>" line. Earlier impl returned None on first
        // non-matching line — regression guard.
        let out = "\n[notice] checking auth\n\nbackgrounded · 545fd354 · w\n";
        assert_eq!(parse_short_from_spawn(out).as_deref(), Some("545fd354"));
    }

    #[test]
    fn parse_backgrounded_line_with_name() {
        let out = "backgrounded · 545fd354 · my-worker\n";
        assert_eq!(parse_short_from_spawn(out).as_deref(), Some("545fd354"));
    }

    #[test]
    fn parse_backgrounded_line_no_name() {
        let out = "backgrounded · 545fd354\n";
        assert_eq!(parse_short_from_spawn(out).as_deref(), Some("545fd354"));
    }

    #[test]
    fn parse_backgrounded_line_idle_hint() {
        let out = "backgrounded · 545fd354 (idle — send a prompt to start)\n";
        assert_eq!(parse_short_from_spawn(out).as_deref(), Some("545fd354"));
    }

    #[test]
    fn parse_backgrounded_line_unicode_bullet_variant() {
        // Some terminals render the middle dot as •.
        let out = "backgrounded • 545fd354\n";
        assert_eq!(parse_short_from_spawn(out).as_deref(), Some("545fd354"));
    }

    #[test]
    fn parse_backgrounded_missing_short_returns_none() {
        let out = "some unrelated output\n";
        assert!(parse_short_from_spawn(out).is_none());
    }

    #[test]
    fn settle_detector_treats_done_as_settled() {
        let mut det = SettleDetector::new();
        det.apply_patch(&StatePatch {
            state: Some("done".to_string()),
            tempo: Some("idle".to_string()),
            ..Default::default()
        });
        assert!(det.is_settled());
        assert_eq!(det.last_settled_state(), Some("done"));
    }

    #[test]
    fn settle_detector_treats_blocked_as_settled() {
        let mut det = SettleDetector::new();
        det.apply_patch(&StatePatch {
            state: Some("blocked".to_string()),
            tempo: Some("blocked".to_string()),
            needs: Some("user input".to_string()),
            ..Default::default()
        });
        assert!(det.is_settled());
        assert_eq!(det.last_settled_state(), Some("blocked"));
        assert_eq!(det.needs(), Some("user input"));
    }

    #[test]
    fn settle_detector_ignores_pid_only_patches() {
        let mut det = SettleDetector::new();
        det.apply_patch(&StatePatch {
            pid: Some(123),
            ..Default::default()
        });
        assert!(!det.is_settled());
    }

    #[test]
    fn settle_detector_resets_state_on_active_tempo() {
        // Worker settles, then resumes with a tempo:active patch (no state
        // field). Detector must clear `state` so the next done counts as a
        // NEW settle — otherwise the settle_watcher dedupe latches forever.
        let mut det = SettleDetector::new();
        det.apply_patch(&StatePatch {
            state: Some("done".to_string()),
            tempo: Some("idle".to_string()),
            ..Default::default()
        });
        assert!(det.is_settled());
        // Worker starts next turn — only tempo flips.
        det.apply_patch(&StatePatch {
            tempo: Some("active".to_string()),
            ..Default::default()
        });
        assert!(!det.is_settled(), "active tempo must clear prior settle");
        // Next done arrives.
        det.apply_patch(&StatePatch {
            state: Some("done".to_string()),
            tempo: Some("idle".to_string()),
            ..Default::default()
        });
        assert!(det.is_settled());
    }

    #[test]
    fn settle_detector_resets_state_on_running_tempo() {
        let mut det = SettleDetector::new();
        det.apply_patch(&StatePatch {
            state: Some("blocked".to_string()),
            tempo: Some("blocked".to_string()),
            needs: Some("user input".to_string()),
            ..Default::default()
        });
        assert!(det.is_settled());
        det.apply_patch(&StatePatch {
            tempo: Some("running".to_string()),
            ..Default::default()
        });
        assert!(!det.is_settled());
        assert_eq!(det.needs(), None, "needs must clear after resume");
    }

    #[test]
    fn statepatch_deserializes_from_top_level_record() {
        // wait_for_turn's snapshot path must accept `record.state` / `record.tempo`
        // (the round-trip PoC shape), not just nested `record.currentState.*`.
        let raw = serde_json::json!({
            "state": "done",
            "tempo": "idle",
            "detail": "all set"
        });
        let p: crate::cli::bg_protocol::StatePatch = serde_json::from_value(raw).unwrap();
        assert_eq!(p.state.as_deref(), Some("done"));
        assert_eq!(p.tempo.as_deref(), Some("idle"));
        assert_eq!(p.detail.as_deref(), Some("all set"));
    }

    #[test]
    fn settle_detector_ignores_active_tempo() {
        let mut det = SettleDetector::new();
        det.apply_patch(&StatePatch {
            tempo: Some("active".to_string()),
            ..Default::default()
        });
        assert!(!det.is_settled());
    }
}
