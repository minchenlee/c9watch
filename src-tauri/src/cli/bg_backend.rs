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

use crate::cli::bg_protocol::{OneShotReply, Request};
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
        // Strip the "backgrounded " prefix and any separator char.
        let rest = trimmed.strip_prefix("backgrounded")?.trim_start();
        let after_sep = rest
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
}
