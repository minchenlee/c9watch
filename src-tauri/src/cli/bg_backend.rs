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
}
