//! Process evidence for Codex CLI and Cursor Agent CLI session liveness.
//!
//! Same model as `pi_liveness`: per cwd, N live agent processes keep the N
//! newest fresh transcripts, a process only keeps a transcript written after
//! it started, and evidence only ends a session early. When the process table
//! cannot be read, or one candidate process cannot be attributed, every
//! session keeps the mtime freshness windows.
//!
//! Codex: the session process is the native `codex` binary (TUI, `resume`,
//! `exec`). `codex app-server` (the shared daemon, also used by Codex Desktop
//! and the IDE extensions) and the other servers are not sessions. Since
//! codex 0.162 a plain TUI runs its thread inside the daemon, so the TUI is
//! only the attachment: see `codex.rs` for which rollouts are candidates.
//!
//! Cursor: the Agent CLI is `node …/cursor-agent/versions/<v>/index.js` with
//! argv[0] `…/bin/cursor-agent` (or the `agent` alias). Each CLI chat has a
//! store at `~/.cursor/chats/<md5(cwd)>/<session-id>/`, so the md5 of the
//! process cwd is an exact key for its transcripts. IDE chats have no store.
//!
//! Evidence: `docs/research/codex-approval-signal-2026-10.md`.

use super::pi_liveness::{
    basename, normalize_cwd, scan_processes, PiProcessClass, PiProcessEvidence, PiProcessProbe,
    PiProcessRow,
};
use std::collections::HashMap;

// ── Codex ───────────────────────────────────────────────────────────

/// `codex` subcommands that run a server, not a session.
const CODEX_SERVER_SUBCOMMANDS: &[&str] = &["app-server", "mcp-server", "exec-server"];

pub(crate) fn is_codex_candidate_name(name: &str) -> bool {
    name == "codex"
}

pub(crate) fn classify_codex_process(row: &PiProcessRow<'_>) -> PiProcessClass {
    if !is_codex_candidate_name(row.name) {
        return PiProcessClass::NotPi;
    }
    if row.argv.is_empty() {
        // Codex always runs as the user, so only same-user rows can hide one.
        return if row.same_user {
            PiProcessClass::Unknown
        } else {
            PiProcessClass::NotPi
        };
    }
    if row
        .argv
        .get(1)
        .is_some_and(|arg| CODEX_SERVER_SUBCOMMANDS.contains(arg))
    {
        return PiProcessClass::NotPi;
    }
    PiProcessClass::Pi {
        cwd: row.cwd.map(str::to_string),
        started_ms: row.started_ms,
    }
}

/// Live Codex session processes per normalized cwd.
pub(crate) struct SysinfoCodexProcessProbe {
    system: sysinfo::System,
}

impl SysinfoCodexProcessProbe {
    pub(crate) fn new() -> Self {
        Self {
            system: sysinfo::System::new(),
        }
    }
}

impl PiProcessProbe for SysinfoCodexProcessProbe {
    fn scan(&mut self) -> PiProcessEvidence {
        scan_processes(
            &mut self.system,
            is_codex_candidate_name,
            classify_codex_process,
        )
    }
}

// ── Cursor ──────────────────────────────────────────────────────────

/// The Agent CLI is a Node script, so sysinfo usually reports `node`.
pub(crate) fn is_cursor_candidate_name(name: &str) -> bool {
    matches!(name, "node" | "cursor-agent" | "agent")
}

pub(crate) fn classify_cursor_process(row: &PiProcessRow<'_>) -> PiProcessClass {
    if !is_cursor_candidate_name(row.name) {
        return PiProcessClass::NotPi;
    }
    let is_cursor_agent = row
        .argv
        .first()
        .is_some_and(|arg| basename(arg) == "cursor-agent")
        || row.argv.iter().any(|arg| arg.contains("/cursor-agent/"));
    if is_cursor_agent {
        return PiProcessClass::Pi {
            cwd: row.cwd.map(str::to_string),
            started_ms: row.started_ms,
        };
    }
    if row.argv.is_empty() && row.same_user {
        return PiProcessClass::Unknown;
    }
    PiProcessClass::NotPi
}

/// Re-key cwd evidence by Cursor's chat-store directory name, md5(cwd).
pub(crate) fn rekey_by_chat_hash(evidence: PiProcessEvidence) -> PiProcessEvidence {
    match evidence {
        PiProcessEvidence::Unavailable => PiProcessEvidence::Unavailable,
        PiProcessEvidence::Live(live) => {
            let mut rekeyed: HashMap<String, Vec<Option<i64>>> = HashMap::new();
            for (cwd, starts) in live {
                rekeyed
                    .entry(cursor_chat_hash(&cwd))
                    .or_default()
                    .extend(starts);
            }
            PiProcessEvidence::Live(rekeyed)
        }
    }
}

/// Cursor names a CLI chat-store directory by the md5 of the session cwd.
pub(crate) fn cursor_chat_hash(cwd: &str) -> String {
    md5_hex(normalize_cwd(cwd).as_bytes())
}

/// Live Cursor Agent CLI processes keyed by `cursor_chat_hash(cwd)`.
pub(crate) struct SysinfoCursorProcessProbe {
    system: sysinfo::System,
}

impl SysinfoCursorProcessProbe {
    pub(crate) fn new() -> Self {
        Self {
            system: sysinfo::System::new(),
        }
    }
}

impl PiProcessProbe for SysinfoCursorProcessProbe {
    fn scan(&mut self) -> PiProcessEvidence {
        rekey_by_chat_hash(scan_processes(
            &mut self.system,
            is_cursor_candidate_name,
            classify_cursor_process,
        ))
    }
}

/// MD5 (RFC 1321) as lowercase hex. Used only to name Cursor's chat-store
/// directories, not for security.
fn md5_hex(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
        .collect();
    let mut message = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_le_bytes());

    let mut state: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
    for chunk in message.chunks_exact(64) {
        let words: Vec<u32> = chunk
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let rotated = a
                .wrapping_add(f)
                .wrapping_add(k[i])
                .wrapping_add(words[g])
                .rotate_left(S[i]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(rotated);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }
    state
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row<'a>(name: &'a str, argv: &'a [&'a str], cwd: Option<&'a str>) -> PiProcessRow<'a> {
        PiProcessRow {
            name,
            argv,
            cwd,
            started_ms: Some(1_000),
            same_user: true,
        }
    }

    fn live(cwd: &str) -> PiProcessClass {
        PiProcessClass::Pi {
            cwd: Some(cwd.into()),
            started_ms: Some(1_000),
        }
    }

    /// Manual probe: prints the live Codex and Cursor evidence.
    /// `cargo test --lib agent_liveness::tests::live_probe_report -- --ignored --nocapture`
    #[test]
    #[ignore = "reads the live process table"]
    fn live_probe_report() {
        eprintln!("codex={:?}", SysinfoCodexProcessProbe::new().scan());
        eprintln!("cursor={:?}", SysinfoCursorProcessProbe::new().scan());
    }

    #[test]
    fn md5_matches_rfc_1321_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn cursor_chat_hash_matches_observed_store_directories() {
        // Observed on 2026-10-14: ~/.cursor/chats/<hash>/<session-id>/store.db.
        assert_eq!(
            cursor_chat_hash("/Users/liminchen/Documents/GitHub/pi"),
            "12e6c3832807165cd48a898645c411ad"
        );
        assert_eq!(
            cursor_chat_hash("/private/var/folders/lh/qf0ljlb96ps80nc6mlc49rvw0000gn/T/dsh-cursor-acp-probe-QXVcYk/"),
            "09811535cd5d9fd3047b6556d12cb950"
        );
    }

    #[test]
    fn codex_tui_and_exec_are_sessions_but_servers_are_not() {
        let cwd = Some("/w/a");
        assert_eq!(
            classify_codex_process(&row("codex", &["codex", "-a", "on-request"], cwd)),
            live("/w/a")
        );
        assert_eq!(
            classify_codex_process(&row("codex", &["/opt/bin/codex", "exec", "hi"], cwd)),
            live("/w/a")
        );
        assert_eq!(
            classify_codex_process(&row("codex", &["codex", "resume", "id"], cwd)),
            live("/w/a")
        );
        for server in [
            &[
                "codex",
                "app-server",
                "--listen",
                "unix://",
                "--managed-daemon",
            ][..],
            &["codex", "app-server", "daemon", "pid-update-loop"][..],
            &["codex", "mcp-server"][..],
        ] {
            assert_eq!(
                classify_codex_process(&row("codex", server, cwd)),
                PiProcessClass::NotPi
            );
        }
        assert_eq!(
            classify_codex_process(&row("codex-code-mode-host", &["x"], cwd)),
            PiProcessClass::NotPi
        );
    }

    #[test]
    fn unreadable_codex_argv_is_unknown_for_same_user_only() {
        assert_eq!(
            classify_codex_process(&row("codex", &[], Some("/w"))),
            PiProcessClass::Unknown
        );
        let mut other = row("codex", &[], Some("/w"));
        other.same_user = false;
        assert_eq!(classify_codex_process(&other), PiProcessClass::NotPi);
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
            classify_cursor_process(&row("node", &argv, Some("/w/a"))),
            live("/w/a")
        );
        let alias = [
            "/Users/me/.local/bin/agent",
            "/Users/me/.local/share/cursor-agent/versions/v/index.js",
        ];
        assert_eq!(
            classify_cursor_process(&row("node", &alias, Some("/w/a"))),
            live("/w/a")
        );
        assert_eq!(
            classify_cursor_process(&row("node", &["node", "server.js"], Some("/w"))),
            PiProcessClass::NotPi
        );
        assert_eq!(
            classify_cursor_process(&row("node", &[], Some("/w"))),
            PiProcessClass::Unknown
        );
    }

    #[test]
    fn cursor_evidence_is_rekeyed_by_chat_hash() {
        let evidence = PiProcessEvidence::Live(HashMap::from([(
            "/Users/liminchen/Documents/GitHub/pi".to_string(),
            vec![Some(5)],
        )]));
        assert_eq!(
            rekey_by_chat_hash(evidence),
            PiProcessEvidence::Live(HashMap::from([(
                "12e6c3832807165cd48a898645c411ad".to_string(),
                vec![Some(5)],
            )]))
        );
        assert_eq!(
            rekey_by_chat_hash(PiProcessEvidence::Unavailable),
            PiProcessEvidence::Unavailable
        );
    }
}
