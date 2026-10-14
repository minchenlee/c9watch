//! Open-file evidence for Codex CLI and Cursor Agent CLI session liveness.
//!
//! An agent process holds its transcript open while the session is loaded:
//! Codex keeps the rollout JSONL open in its rollout writer, and the Cursor
//! Agent CLI keeps its chat store (`store.db`, `-wal`, `-shm`) open. So a
//! candidate session is alive when any agent process has its transcript
//! open, or when the transcript was written in the last 10 s. Evidence only
//! ends a session early: when the process table or any agent's open files
//! cannot be read, every session keeps the mtime freshness windows.
//!
//! Only processes recognized as agents are inspected, never the whole table.
//!
//! Codex: every native `codex` process, servers included. Since codex 0.162 a
//! plain TUI runs its thread inside the shared `codex app-server` daemon,
//! which holds that rollout open while the thread is loaded (also after the
//! TUI exits, while a turn still runs).
//!
//! Cursor: the Agent CLI is `node …/cursor-agent/versions/<v>/index.js` with
//! argv[0] `…/bin/cursor-agent` (or the `agent` alias). Its chat store is
//! `~/.cursor/chats/<md5(cwd)>/<session-id>/store.db`. IDE chats have none.
//!
//! Evidence: `docs/research/codex-approval-signal-2026-10.md`.

use super::open_files::open_file_paths;
use super::pi_liveness::{basename, current_uid, uid_matches, PI_PROCESS_GRACE_MS};
use std::collections::HashSet;
use std::path::Path;

/// Transcript keys that some agent process has open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpenFileEvidence {
    /// The process table or one agent's open files could not be read.
    Unavailable,
    /// Keys (see `codex_rollout_key`, `cursor_chat_key`) of open transcripts.
    Open(HashSet<String>),
}

pub(crate) trait OpenFileProbe: Send {
    fn scan(&mut self) -> OpenFileEvidence;
}

/// One fresh transcript that open-file evidence may end.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OpenFileCandidate<'a> {
    pub key: &'a str,
    pub modified_ms: i64,
}

/// For each candidate, whether it stays alive.
pub(crate) fn open_file_alive_flags(
    candidates: &[OpenFileCandidate<'_>],
    evidence: &OpenFileEvidence,
    now_ms: i64,
) -> Vec<bool> {
    let OpenFileEvidence::Open(open) = evidence else {
        return vec![true; candidates.len()];
    };
    candidates
        .iter()
        .map(|candidate| {
            now_ms.saturating_sub(candidate.modified_ms) < PI_PROCESS_GRACE_MS
                || open.contains(candidate.key)
        })
        .collect()
}

/// What one process-table row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentProcess {
    Agent,
    NotAgent,
    /// Could be an agent, but its argv is unreadable.
    Unknown,
}

/// One process-table row, as the classifiers see it.
pub(crate) struct AgentProcessRow<'a> {
    pub name: &'a str,
    pub argv: &'a [&'a str],
}

/// Reads the open files of every same-user process that `classify` calls an
/// agent and maps them through `key`.
fn scan_open_files(
    system: &mut sysinfo::System,
    is_candidate_name: fn(&str) -> bool,
    needs_argv: bool,
    classify: fn(&AgentProcessRow<'_>) -> AgentProcess,
    key: fn(&Path) -> Option<String>,
) -> OpenFileEvidence {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};
    if !sysinfo::IS_SUPPORTED_SYSTEM {
        return OpenFileEvidence::Unavailable;
    }
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::new().with_user(UpdateKind::OnlyIfNotSet),
    );
    // The table always contains c9watch itself; empty means it failed.
    if system.processes().is_empty() {
        return OpenFileEvidence::Unavailable;
    }
    let own_uid = current_uid();
    let candidates: Vec<sysinfo::Pid> = system
        .processes()
        .iter()
        .filter(|(_, process)| process.name().to_str().is_some_and(is_candidate_name))
        // Other users' processes cannot be read and do not own our sessions.
        .filter(|(_, process)| match (own_uid, process.user_id()) {
            (Some(own), Some(uid)) => uid_matches(uid, own),
            _ => true,
        })
        .map(|(pid, _)| *pid)
        .collect();
    if needs_argv && !candidates.is_empty() {
        // A pid may be reused between polls, so argv is re-read.
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&candidates),
            false,
            ProcessRefreshKind::new().with_cmd(UpdateKind::Always),
        );
    }
    let mut open = HashSet::new();
    for pid in candidates {
        let Some(process) = system.process(pid) else {
            return OpenFileEvidence::Unavailable;
        };
        let name = process.name().to_string_lossy();
        let argv_owned: Vec<String> = process
            .cmd()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let argv: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        match classify(&AgentProcessRow {
            name: name.as_ref(),
            argv: &argv,
        }) {
            AgentProcess::NotAgent => continue,
            AgentProcess::Unknown => return OpenFileEvidence::Unavailable,
            AgentProcess::Agent => {}
        }
        let Ok(paths) = open_file_paths(pid.as_u32()) else {
            return OpenFileEvidence::Unavailable;
        };
        open.extend(paths.iter().filter_map(|path| key(path)));
    }
    OpenFileEvidence::Open(open)
}

// ── Codex ───────────────────────────────────────────────────────────

pub(crate) fn is_codex_candidate_name(name: &str) -> bool {
    name == "codex"
}

/// Every native `codex` process may hold a rollout: the TUI, `exec`,
/// `resume`, and the `app-server` daemon that runs TUI and Desktop threads.
fn classify_codex_process(row: &AgentProcessRow<'_>) -> AgentProcess {
    if is_codex_candidate_name(row.name) {
        AgentProcess::Agent
    } else {
        AgentProcess::NotAgent
    }
}

/// Key of a rollout: its file name, `rollout-<timestamp>-<thread-id>.jsonl`.
/// The file name is unique, and does not depend on symlinks in the path.
pub(crate) fn codex_rollout_key(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    (name.starts_with("rollout-") && name.ends_with(".jsonl")).then(|| name.to_string())
}

/// Open Codex rollouts.
pub(crate) struct SysinfoCodexOpenFileProbe {
    system: sysinfo::System,
}

impl SysinfoCodexOpenFileProbe {
    pub(crate) fn new() -> Self {
        Self {
            system: sysinfo::System::new(),
        }
    }
}

impl OpenFileProbe for SysinfoCodexOpenFileProbe {
    fn scan(&mut self) -> OpenFileEvidence {
        scan_open_files(
            &mut self.system,
            is_codex_candidate_name,
            false,
            classify_codex_process,
            codex_rollout_key,
        )
    }
}

// ── Cursor ──────────────────────────────────────────────────────────

/// The Agent CLI is a Node script, so sysinfo usually reports `node`.
pub(crate) fn is_cursor_candidate_name(name: &str) -> bool {
    matches!(name, "node" | "cursor-agent" | "agent")
}

pub(crate) fn classify_cursor_process(row: &AgentProcessRow<'_>) -> AgentProcess {
    if !is_cursor_candidate_name(row.name) {
        return AgentProcess::NotAgent;
    }
    let is_cursor_agent = row
        .argv
        .first()
        .is_some_and(|arg| basename(arg) == "cursor-agent")
        || row.argv.iter().any(|arg| arg.contains("/cursor-agent/"));
    if is_cursor_agent {
        AgentProcess::Agent
    } else if row.argv.is_empty() {
        AgentProcess::Unknown
    } else {
        AgentProcess::NotAgent
    }
}

/// Key of an Agent CLI chat: the `<session-id>` of an open
/// `…/chats/<hash>/<session-id>/store.db*` file.
pub(crate) fn cursor_chat_key(path: &Path) -> Option<String> {
    if !path.file_name()?.to_str()?.starts_with("store.db") {
        return None;
    }
    let chat_dir = path.parent()?;
    let chats_dir = chat_dir.parent()?.parent()?;
    if chats_dir.file_name()? != "chats" {
        return None;
    }
    chat_dir.file_name()?.to_str().map(str::to_string)
}

/// Open Cursor Agent CLI chat stores.
pub(crate) struct SysinfoCursorOpenFileProbe {
    system: sysinfo::System,
}

impl SysinfoCursorOpenFileProbe {
    pub(crate) fn new() -> Self {
        Self {
            system: sysinfo::System::new(),
        }
    }
}

impl OpenFileProbe for SysinfoCursorOpenFileProbe {
    fn scan(&mut self) -> OpenFileEvidence {
        scan_open_files(
            &mut self.system,
            is_cursor_candidate_name,
            true,
            classify_cursor_process,
            cursor_chat_key,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn row<'a>(name: &'a str, argv: &'a [&'a str]) -> AgentProcessRow<'a> {
        AgentProcessRow { name, argv }
    }

    /// Manual probe: prints each `codex` pid's open-file result, the live
    /// evidence, and the time one scan takes.
    /// `cargo test --lib agent_liveness::tests::live_probe_report -- --ignored --nocapture`
    #[test]
    #[ignore = "reads the live process table"]
    fn live_probe_report() {
        let mut codex = SysinfoCodexOpenFileProbe::new();
        let mut cursor = SysinfoCursorOpenFileProbe::new();
        let pids: Vec<u32> = std::process::Command::new("pgrep")
            .args(["-x", "codex"])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .collect();
        let started = Instant::now();
        for pid in &pids {
            match open_file_paths(*pid) {
                Ok(paths) => eprintln!("pid {pid}: {} open files", paths.len()),
                Err(error) => eprintln!("pid {pid}: {error}"),
            }
        }
        eprintln!(
            "open files of {} codex pids: {:?}",
            pids.len(),
            started.elapsed()
        );
        eprintln!("codex={:?}", codex.scan());
        eprintln!("cursor={:?}", cursor.scan());
        for (name, probe) in [
            ("codex", &mut codex as &mut dyn OpenFileProbe),
            ("cursor", &mut cursor),
        ] {
            let started = Instant::now();
            const RUNS: u32 = 20;
            for _ in 0..RUNS {
                probe.scan();
            }
            eprintln!("{name}: {:?} per scan", started.elapsed() / RUNS);
        }
    }

    fn candidate(key: &str, modified_ms: i64) -> OpenFileCandidate<'_> {
        OpenFileCandidate { key, modified_ms }
    }

    #[test]
    fn open_or_recently_written_transcripts_stay() {
        let now = 1_000_000;
        let evidence = OpenFileEvidence::Open(HashSet::from(["open".to_string()]));
        let candidates = [
            candidate("open", now - 3_600_000),
            candidate("closed-old", now - PI_PROCESS_GRACE_MS),
            candidate("closed-recent", now - PI_PROCESS_GRACE_MS + 1),
        ];
        assert_eq!(
            open_file_alive_flags(&candidates, &evidence, now),
            vec![true, false, true]
        );
    }

    #[test]
    fn unavailable_evidence_keeps_every_transcript() {
        let candidates = [candidate("a", 0), candidate("b", 0)];
        assert_eq!(
            open_file_alive_flags(&candidates, &OpenFileEvidence::Unavailable, 10_000_000),
            vec![true, true]
        );
    }

    #[test]
    fn every_codex_process_is_inspected() {
        for argv in [
            &["codex", "-a", "on-request"][..],
            &["/opt/bin/codex", "exec", "hi"][..],
            &[
                "codex",
                "app-server",
                "--listen",
                "unix://",
                "--managed-daemon",
            ][..],
            &[][..],
        ] {
            assert_eq!(
                classify_codex_process(&row("codex", argv)),
                AgentProcess::Agent
            );
        }
        assert_eq!(
            classify_codex_process(&row("codex-code-mode-host", &["x"])),
            AgentProcess::NotAgent
        );
    }

    #[test]
    fn codex_rollout_key_is_the_file_name() {
        assert_eq!(
            codex_rollout_key(Path::new(
                "/Users/u/.codex/sessions/2026/10/14/rollout-2026-10-14T17-44-45-01a139cc.jsonl"
            ))
            .as_deref(),
            Some("rollout-2026-10-14T17-44-45-01a139cc.jsonl")
        );
        assert_eq!(
            codex_rollout_key(Path::new("/Users/u/.codex/logs_2.sqlite")),
            None
        );
        assert_eq!(
            codex_rollout_key(Path::new("/Users/u/rollout-a.jsonl.zst")),
            None
        );
    }

    #[test]
    fn cursor_agent_node_process_is_recognized() {
        // Observed argv for cursor-agent 2026.10.01-e373342.
        let argv = [
            "/Users/me/.local/bin/cursor-agent",
            "--use-system-ca",
            "/Users/me/.local/share/cursor-agent/versions/2026.10.01-e373342/index.js",
        ];
        assert_eq!(
            classify_cursor_process(&row("node", &argv)),
            AgentProcess::Agent
        );
        let alias = [
            "/Users/me/.local/bin/agent",
            "/Users/me/.local/share/cursor-agent/versions/v/index.js",
        ];
        assert_eq!(
            classify_cursor_process(&row("node", &alias)),
            AgentProcess::Agent
        );
        assert_eq!(
            classify_cursor_process(&row("node", &["node", "server.js"])),
            AgentProcess::NotAgent
        );
        assert_eq!(
            classify_cursor_process(&row("node", &[])),
            AgentProcess::Unknown
        );
        assert_eq!(
            classify_cursor_process(&row("zsh", &["zsh"])),
            AgentProcess::NotAgent
        );
    }

    #[test]
    fn cursor_chat_key_is_the_store_directory() {
        // Observed open files of cursor-agent pid 72326 on 2026-10-14.
        let chat = "/Users/u/.cursor/chats/317f6f418cdadd651e8088e7cadc2bac/0bddd9b1-011c-4ba4-bf5f-c3d717c5cc81";
        for file in ["store.db", "store.db-wal", "store.db-shm"] {
            assert_eq!(
                cursor_chat_key(&Path::new(chat).join(file)).as_deref(),
                Some("0bddd9b1-011c-4ba4-bf5f-c3d717c5cc81")
            );
        }
        assert_eq!(
            cursor_chat_key(Path::new(
                "/Users/u/.cursor/ai-tracking/ai-code-tracking.db"
            )),
            None
        );
        assert_eq!(
            cursor_chat_key(Path::new("/Users/u/.cursor/other/h/id/store.db")),
            None
        );
    }
}
