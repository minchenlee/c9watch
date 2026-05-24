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
}
