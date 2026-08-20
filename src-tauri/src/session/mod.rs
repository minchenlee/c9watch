pub mod codex;
pub mod cursor;
pub mod custom_names;
pub mod detector;
pub mod parser;
pub mod permissions;
pub mod source;
pub mod status;

pub use custom_names::{CustomNames, CustomTitles};
pub use detector::LegacySessionSource;
pub mod detector_cli;
pub use detector_cli::CliSessionSource;
pub mod state;
pub use parser::{
    extract_messages, parse_all_entries, parse_last_n_entries, parse_sessions_index, ImageBlock,
    MessageContent, MessageType, SessionEntry, SessionIndexEntry, SessionsIndex,
};
pub use permissions::PermissionChecker;
pub use source::{
    AgentKind, CliActivity, DetectedSession, DetectionDiagnostics, SessionKind, SessionProvider,
    SessionSource, SessionSurface,
};
pub use state::DetectorState;
pub use status::{
    determine_status, determine_status_with_context, get_pending_tool_input, get_pending_tool_name,
    SessionStatus,
};

pub mod history;
pub use history::{deep_search, get_history, DeepSearchHit, HistoryEntry};

mod codex_archive;
pub mod cost;
pub use cost::{get_cost_data, CostData};

pub(crate) fn codex_session_ids(home: &std::path::Path, prefix: &str) -> Vec<String> {
    codex_archive::load_default_snapshots(home)
        .into_iter()
        .filter(|snapshot| snapshot.thread_id.starts_with(prefix))
        .map(|snapshot| snapshot.thread_id)
        .collect()
}

pub(crate) fn cursor_session_ids(home: &std::path::Path, prefix: &str) -> Vec<String> {
    cursor::list_session_ids(home, prefix)
}

pub mod memory;
pub use memory::{get_memory_files, MemoryFile, ProjectMemory};

pub mod enrichment;
pub use enrichment::{detect_and_enrich_sessions, Session};

pub mod sanitize;
pub use sanitize::strip_system_tags;

pub mod conversation;
pub use conversation::{get_conversation_data, Conversation, ConversationMessage};

pub mod subagents;
pub use subagents::{
    active_subagents_for_path, all_subagents_by_session, get_subagent_transcript, SubagentInfo,
    SubagentStatus, SubagentTranscript,
};

use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendMode {
    Auto,
    ForceCli,
    ForceLegacy,
}

pub fn mode_from_env() -> BackendMode {
    parse_mode(std::env::var("C9WATCH_DETECTOR_BACKEND").ok().as_deref())
}

fn parse_mode(value: Option<&str>) -> BackendMode {
    match value {
        Some("cli") => BackendMode::ForceCli,
        Some("legacy") => BackendMode::ForceLegacy,
        _ => BackendMode::Auto,
    }
}

fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let candidate = &s[start..i];
            let parts: Vec<&str> = candidate.split('.').collect();
            if parts.len() == 3 {
                if let (Ok(maj), Ok(min), Ok(pat)) =
                    (parts[0].parse(), parts[1].parse(), parts[2].parse())
                {
                    return Some((maj, min, pat));
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

fn semver_supports_agents_json(v: (u32, u32, u32)) -> bool {
    // `claude agents --json` shipped in 2.1.145. 2.1.145–146 omit the `kind`
    // field (background-pinned sessions came in 2.1.147); CliAgent::kind has
    // a default so the older schema still parses.
    v >= (2, 1, 145)
}

pub fn probe_claude_supports_agents_json() -> bool {
    if !probe_version_supports() {
        return false;
    }
    probe_command_works()
}

fn probe_version_supports() -> bool {
    use std::process::Stdio;
    use wait_timeout::ChildExt;
    let Ok(mut child) = Command::new("claude")
        .args(["--version"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    // Drain stdout on a bg thread to avoid pipe deadlock (same pattern as
    // detector_cli::detect). --version output is tiny, but the cost is trivial.
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => return false,
    };
    let reader = std::thread::spawn(move || {
        let mut stdout = stdout;
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut buf);
        buf
    });
    match child.wait_timeout(Duration::from_secs(2)) {
        Ok(Some(status)) if status.success() => {
            let buf = match reader.join() {
                Ok(b) => b,
                Err(_) => return false,
            };
            let s = String::from_utf8_lossy(&buf);
            parse_semver(&s)
                .map(semver_supports_agents_json)
                .unwrap_or(false)
        }
        _ => {
            let _ = child.kill();
            // Reap to avoid leaving a zombie on Unix.
            let _ = child.wait();
            let _ = reader.join();
            false
        }
    }
}

fn probe_command_works() -> bool {
    use std::process::Stdio;
    use wait_timeout::ChildExt;
    let Ok(mut child) = Command::new("claude")
        .args(["agents", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    // Drain stdout on a bg thread to avoid pipe-buffer deadlock when the child
    // writes more than the OS pipe buffer (typically 64KB) — without draining
    // concurrently, the child blocks on write and wait_timeout falsely fires.
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => return false,
    };
    let reader = std::thread::spawn(move || {
        let mut stdout = stdout;
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut buf);
        buf
    });
    match child.wait_timeout(Duration::from_secs(3)) {
        Ok(Some(status)) if status.success() => {
            let buf = match reader.join() {
                Ok(b) => b,
                Err(_) => return false,
            };
            serde_json::from_slice::<Vec<serde_json::Value>>(&buf).is_ok()
        }
        _ => {
            let _ = child.kill();
            // Reap to avoid leaving a zombie on Unix.
            let _ = child.wait();
            let _ = reader.join();
            false
        }
    }
}

pub fn create_session_source() -> Box<dyn SessionSource> {
    use crate::session::detector::LegacySessionSource;
    use crate::session::detector_cli::CliSessionSource;
    match mode_from_env() {
        BackendMode::ForceCli => Box::new(CliSessionSource::new()),
        BackendMode::ForceLegacy => Box::new(LegacySessionSource::new().expect("legacy ctor")),
        BackendMode::Auto => {
            if probe_claude_supports_agents_json() {
                Box::new(CliSessionSource::new())
            } else {
                Box::new(LegacySessionSource::new().expect("legacy ctor"))
            }
        }
    }
}

#[cfg(test)]
mod factory_tests {
    use super::*;

    #[test]
    fn parse_semver_extracts_triple_from_cc_output() {
        assert_eq!(parse_semver("2.1.150 (Claude Code)\n"), Some((2, 1, 150)));
    }

    #[test]
    fn parse_semver_handles_no_version_line() {
        assert_eq!(parse_semver("not a version string"), None);
    }

    #[test]
    fn parse_semver_handles_extra_text() {
        assert_eq!(
            parse_semver("Claude Code 2.2.0 — build abc"),
            Some((2, 2, 0))
        );
    }

    #[test]
    fn version_gate_accepts_2_1_145() {
        assert!(semver_supports_agents_json((2, 1, 145)));
    }

    #[test]
    fn version_gate_accepts_2_1_150() {
        assert!(semver_supports_agents_json((2, 1, 150)));
    }

    #[test]
    fn version_gate_accepts_2_2_0() {
        assert!(semver_supports_agents_json((2, 2, 0)));
    }

    #[test]
    fn version_gate_rejects_2_1_144() {
        assert!(!semver_supports_agents_json((2, 1, 144)));
    }

    #[test]
    fn mode_from_env_defaults_to_auto() {
        assert_eq!(parse_mode(None), BackendMode::Auto);
        assert_eq!(parse_mode(Some("garbage")), BackendMode::Auto);
        assert_eq!(parse_mode(Some("auto")), BackendMode::Auto);
    }

    #[test]
    fn mode_from_env_recognizes_cli_and_legacy() {
        assert_eq!(parse_mode(Some("cli")), BackendMode::ForceCli);
        assert_eq!(parse_mode(Some("legacy")), BackendMode::ForceLegacy);
    }
}
