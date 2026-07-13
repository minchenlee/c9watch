//! Backend selection and trait for PM workers.
//!
//! Two backends exist:
//! - [`BackendKind::Bg`]: spawn via `claude --bg`, control.sock RPC.
//!   Required for CC >= 2.1.150 to stay on Pro/Max subscription quota
//!   after 2026-06-15 (Agent SDK billing split).
//! - [`BackendKind::Print`]: spawn via `claude --print` stream-json.
//!   Legacy path, kept as fallback for older CC versions and as escape
//!   hatch if the bg path breaks.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Bg,
    Print,
}

/// Select the appropriate backend based on env override, CC version, and
/// daemon socket availability.
///
/// Precedence:
/// 1. `C9WATCH_WORKER_BACKEND=bg|print` env var (explicit user choice)
/// 2. `C9WATCH_WORKER_BACKEND=auto` or unset → probe + auto-detect
/// 3. Auto: Bg if `claude --version` >= 2.1.150 AND control.sock exists, else Print
pub fn select_backend() -> BackendKind {
    match std::env::var("C9WATCH_WORKER_BACKEND").as_deref() {
        Ok("bg") => return BackendKind::Bg,
        Ok("print") => return BackendKind::Print,
        Ok("auto") | Err(_) => {} // fall through to auto-detect
        Ok(other) => {
            eprintln!(
                "[worker_backend] unknown C9WATCH_WORKER_BACKEND={:?}, falling back to auto",
                other
            );
        }
    }

    if version_supports_bg() && crate::cli::bg_backend::resolve_control_sock().is_ok() {
        BackendKind::Bg
    } else {
        BackendKind::Print
    }
}

/// Returns true if `claude --version` reports >= 2.1.150 (when `--bg` shipped).
fn version_supports_bg() -> bool {
    let output = match std::process::Command::new("claude")
        .arg("--version")
        .output()
    {
        Ok(o) => o,
        Err(_) => return false,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Format: "2.1.150 (Claude Code)\n"
    let ver = stdout.split_whitespace().next().unwrap_or("");
    parse_version_ge(ver, (2, 1, 150))
}

fn parse_version_ge(ver: &str, min: (u32, u32, u32)) -> bool {
    let parts: Vec<u32> = ver.split('.').take(3).filter_map(|s| s.parse().ok()).collect();
    if parts.len() != 3 {
        return false;
    }
    (parts[0], parts[1], parts[2]) >= min
}

/// Validate the initial prompt against the selected backend.
///
/// - Bg backend: requires a non-empty prompt — returns it
/// - Print backend: ignores the prompt (returns None)
///
/// Returns Err with "INITIAL_PROMPT_REQUIRED" prefix when bg backend has no prompt.
pub fn validate_initial_prompt(
    kind: BackendKind,
    initial_prompt: Option<String>,
) -> Result<Option<String>, String> {
    match kind {
        BackendKind::Bg => match initial_prompt {
            Some(p) if !p.trim().is_empty() => Ok(Some(p)),
            _ => Err(
                "INITIAL_PROMPT_REQUIRED: the bg backend requires --prompt at spawn time. \
                 Pass one via `c9watch spawn --prompt \"<text>\"` or set C9WATCH_WORKER_BACKEND=print"
                    .to_string(),
            ),
        },
        BackendKind::Print => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ge_handles_exact_match() {
        assert!(parse_version_ge("2.1.150", (2, 1, 150)));
    }

    #[test]
    fn version_ge_handles_newer() {
        assert!(parse_version_ge("2.1.151", (2, 1, 150)));
        assert!(parse_version_ge("2.2.0", (2, 1, 150)));
        assert!(parse_version_ge("3.0.0", (2, 1, 150)));
    }

    #[test]
    fn version_ge_rejects_older() {
        assert!(!parse_version_ge("2.1.149", (2, 1, 150)));
        assert!(!parse_version_ge("2.0.999", (2, 1, 150)));
        assert!(!parse_version_ge("1.9.9", (2, 1, 150)));
    }

    #[test]
    fn version_ge_rejects_malformed() {
        assert!(!parse_version_ge("garbage", (2, 1, 150)));
        assert!(!parse_version_ge("2.1", (2, 1, 150)));
        assert!(!parse_version_ge("", (2, 1, 150)));
    }
}

#[cfg(test)]
mod backend_gating_tests {
    use super::*;

    /// Logic-only test: when backend is Bg, validate_initial_prompt must reject None.
    /// When backend is Print, it accepts None.
    #[test]
    fn validate_initial_prompt_bg_rejects_none() {
        let result = validate_initial_prompt(BackendKind::Bg, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("INITIAL_PROMPT_REQUIRED"));
    }

    #[test]
    fn validate_initial_prompt_bg_rejects_empty_string() {
        let result = validate_initial_prompt(BackendKind::Bg, Some("   ".to_string()));
        assert!(result.is_err());
    }

    #[test]
    fn validate_initial_prompt_bg_accepts_real_text() {
        let result = validate_initial_prompt(BackendKind::Bg, Some("do the thing".to_string()));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_deref(), Some("do the thing"));
    }

    #[test]
    fn validate_initial_prompt_print_accepts_none() {
        let result = validate_initial_prompt(BackendKind::Print, None);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn validate_initial_prompt_print_accepts_any_value() {
        let result = validate_initial_prompt(BackendKind::Print, Some("ignored".to_string()));
        assert!(result.is_ok());
        // Print backend ignores the prompt — return None to signal that.
        assert!(result.unwrap().is_none());
    }
}
