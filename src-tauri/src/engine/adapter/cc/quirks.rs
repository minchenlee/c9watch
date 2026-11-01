//! The Claude Code quirks profile (D7): every character, escape, prompt
//! text, delay and socket path the engine relies on to drive or read the
//! CC TUI, keyed by the CC versions it was recorded on.
//!
//! - One profile per range of CC versions. Today there is one,
//!   `CC_2_1`, recorded on CC 2.1.312 and 2.1.314 (the screens in
//!   `tests/fixtures/cc_screens/`) and run live on 2.1.316.
//! - The spawn path gates on it: `gate` reads `claude --version` before a
//!   create or resume, and a version that no profile covers is refused.
//!   It fails closed: a version that cannot be read is refused too.
//!   `C9_CC_ALLOW_UNTESTED=1` lets an untested version run with the
//!   newest profile, and says so in the refusal text it replaces.
//! - The readers keep their logic: `cc_screen` (screen rules), `cc_inbox`
//!   (peer inbox) and `create` (mail delivery) take their constants from
//!   `CURRENT`. The peer-inbox frame fields (`cc_inbox`, `cc_listener`,
//!   `peer_log`) are the schema of `peer_protocol`; the spawn flags and
//!   settings are the CC adapter descriptor and `profile.rs`.
//!
//! When CC changes one of these, add a profile for the new range, record
//! fixtures for it, and pick the profile by version in `profile_for`.

use std::path::Path;
use std::time::Duration;

/// A CC version, `major.minor.patch`.
pub type CcVersion = (u32, u32, u32);

/// What the screen rules look for.
#[derive(Debug)]
pub struct ScreenQuirks {
    /// U+2733: the idle window-title glyph (D24.4).
    pub idle_glyph: char,
    /// U+276F: the selection cursor of menus and the input box.
    pub select_glyph: char,
    /// U+2500: the glyph of the full-width rules around the input box.
    pub rule_glyph: char,
    /// Shortest run of `rule_glyph` that counts as a rule.
    pub rule_min_len: usize,
    /// Screen text that means a dialog is open (D24.4, D24.5).
    pub dialog_markers: &'static [&'static str],
    /// Screen text of the workspace trust dialog (D24.3).
    pub trust_markers: &'static [&'static str],
    /// The trust option's text, and the word of its "Yes" option.
    pub trust_option: &'static str,
    pub trust_yes: &'static str,
    /// A permission prompt is open (D24.5).
    pub permission_marker: &'static str,
    /// The status-line notice after an auto-mode denial (CC 2.1.312).
    pub auto_deny_marker: &'static str,
    /// Lower-case refusals of a vendor that will not start a session.
    pub vendor_refusals: &'static [&'static str],
    /// Lower-case words of a line that warns but does not refuse.
    pub not_refusals: &'static [&'static str],
    /// The fresh-session suggestion in the input box: `Try "…"`.
    pub placeholder_open: &'static str,
    pub placeholder_close: &'static str,
    /// CC's collapsed paste in the input box: `[Pasted text #n +k lines]`.
    pub pasted_marker: &'static str,
}

/// The bytes the engine writes to the TUI.
#[derive(Debug)]
pub struct KeyQuirks {
    /// Cursor Down, as the keyboard sends it.
    pub down: &'static [u8],
    pub cr: &'static [u8],
    /// Bracketed paste (D24.8).
    pub paste_start: &'static [u8],
    pub paste_end: &'static str,
}

/// The delays the engine waits on CC.
#[derive(Debug)]
pub struct TimingQuirks {
    /// PTY quiet before `ready` (D24.4).
    pub quiet: Duration,
    /// Not `ready` by then: the spawn or resume failed (worst seen 10.9 s).
    pub ready_timeout: Duration,
    /// Quiet after Down before the trust highlight is checked.
    pub trust_settle: Duration,
    /// How long the trust highlight may take to move to "Yes".
    pub trust_select_timeout: Duration,
    /// How long a paste settles before the input box is read (CC
    /// collapses a multi-line paste in this time).
    pub paste_settle: Duration,
    /// How long a paste may take to show in the box, and a CR to get into
    /// a full write queue.
    pub paste_timeout: Duration,
    /// How long after the CR the prompt hook may take.
    pub submit_confirm: Duration,
    /// Socket mail: the whole connect-write-close budget.
    pub socket_write_timeout: Duration,
    /// Socket mail: how long the frame stays open before the write half
    /// closes (CC reads the line before it sees EOF).
    pub socket_linger: Duration,
    /// Socket mail: no queue-operation in the transcript by then.
    pub socket_queue_timeout: Duration,
    /// Socket mail: queued in CC, not seen this long after a turn end.
    pub socket_after_turn_end: Duration,
}

/// CC's internal peer inbox (`C9_MAIL_SOCKET=on`).
#[derive(Debug)]
pub struct InboxQuirks {
    /// `peerProtocol` in `~/.claude/sessions/<pid>.json`.
    pub peer_protocol: u64,
    /// Sessions directory, under the home directory.
    pub sessions_dir: &'static str,
    /// The default socket directory, and the namespaces CC uses for it.
    pub socket_dir: &'static str,
    pub socket_namespaces: &'static [&'static str],
    /// The per-uid namespace: this prefix, then the uid.
    pub socket_uid_prefix: &'static str,
    /// The wrapper tag of a peer message.
    pub wrapper_tag: &'static str,
    /// Characters that CC may read as `<` before `wrapper_tag`.
    pub lt_lookalikes: &'static str,
    /// `priority` of a frame.
    pub frame_priority: &'static str,
}

#[derive(Debug)]
pub struct CcQuirksProfile {
    pub name: &'static str,
    /// The first version covered, and the first one past it.
    pub min_version: CcVersion,
    pub below_version: CcVersion,
    pub screen: ScreenQuirks,
    pub keys: KeyQuirks,
    pub timing: TimingQuirks,
    pub inbox: InboxQuirks,
}

impl CcQuirksProfile {
    pub fn covers(&self, v: CcVersion) -> bool {
        self.min_version <= v && v < self.below_version
    }

    pub fn range(&self) -> String {
        let f = |v: CcVersion| format!("{}.{}.{}", v.0, v.1, v.2);
        format!(">= {}, < {}", f(self.min_version), f(self.below_version))
    }
}

/// CC 2.1.x from 2.1.312, the oldest recorded screens.
pub const CC_2_1: CcQuirksProfile = CcQuirksProfile {
    name: "cc-2.1",
    min_version: (2, 1, 312),
    below_version: (2, 2, 0),
    screen: ScreenQuirks {
        idle_glyph: '\u{2733}',
        select_glyph: '\u{276f}',
        rule_glyph: '\u{2500}',
        rule_min_len: 10,
        dialog_markers: &[
            "Do you want to proceed?",
            "Esc to cancel",
            "trust this folder",
        ],
        trust_markers: &["trust this folder", "Accessing workspace:"],
        trust_option: "trust this folder",
        trust_yes: "Yes",
        permission_marker: "Do you want to proceed?",
        auto_deny_marker: " denied by auto mode",
        vendor_refusals: &[
            "usage limit reached",
            "hit your limit",
            "limit reached",
            "credit balance is too low",
            "please run /login",
            "not logged in",
            "invalid api key",
            "oauth token has expired",
            "select login method",
        ],
        not_refusals: &["approaching"],
        placeholder_open: "Try \"",
        placeholder_close: "\"",
        pasted_marker: "[Pasted text #",
    },
    keys: KeyQuirks {
        down: b"\x1b[B",
        cr: b"\r",
        paste_start: b"\x1b[200~",
        paste_end: "\x1b[201~",
    },
    timing: TimingQuirks {
        quiet: Duration::from_millis(300),
        ready_timeout: Duration::from_secs(30),
        trust_settle: Duration::from_millis(150),
        trust_select_timeout: Duration::from_secs(3),
        paste_settle: Duration::from_millis(600),
        paste_timeout: Duration::from_secs(5),
        submit_confirm: Duration::from_secs(10),
        socket_write_timeout: Duration::from_secs(5),
        socket_linger: Duration::from_millis(150),
        socket_queue_timeout: Duration::from_secs(30),
        socket_after_turn_end: Duration::from_secs(15),
    },
    inbox: InboxQuirks {
        peer_protocol: 1,
        sessions_dir: ".claude/sessions",
        socket_dir: "/tmp/cc-socks",
        socket_namespaces: &["/tmp/cc-socks", "/private/tmp/cc-socks"],
        socket_uid_prefix: "/tmp/cc-socks-",
        wrapper_tag: "cross-session-message",
        lt_lookalikes: "<＜﹤‹⟨〈",
        frame_priority: "next",
    },
};

/// Every profile, oldest first.
pub const PROFILES: &[&CcQuirksProfile] = &[&CC_2_1];

/// The profile the engine's readers use. A spawn runs only on a CC that
/// `gate` matched to it.
pub const CURRENT: &CcQuirksProfile = &CC_2_1;

/// Lets an untested CC version run with the newest profile.
pub const ENV_ALLOW_UNTESTED: &str = "C9_CC_ALLOW_UNTESTED";

/// How long `claude --version` may take.
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

pub fn profile_for(v: CcVersion) -> Option<&'static CcQuirksProfile> {
    PROFILES.iter().copied().find(|p| p.covers(v))
}

/// The first `x.y.z` in `claude --version` output, e.g.
/// `2.1.316 (Claude Code)`.
pub fn parse_version(out: &str) -> Option<CcVersion> {
    out.split(|c: char| c.is_whitespace() || c == 'v')
        .find_map(|w| {
            let mut it = w.split('.').map(|n| n.parse::<u32>().ok());
            match (it.next(), it.next(), it.next(), it.next()) {
                (Some(Some(a)), Some(Some(b)), Some(Some(c)), None) => Some((a, b, c)),
                _ => None,
            }
        })
}

/// The version gate of the spawn path. `out` is the output of `claude
/// --version`, or why it could not be run. Fails closed.
pub fn gate(
    out: Result<String, String>,
    allow_untested: bool,
) -> Result<&'static CcQuirksProfile, String> {
    let out = out.map_err(|e| {
        format!(
            "cannot read the Claude Code version ({}); nothing was spawned",
            e
        )
    })?;
    let Some(v) = parse_version(&out) else {
        return Err(format!(
            "cannot read the Claude Code version from {:?}; nothing was spawned",
            out.trim()
        ));
    };
    if let Some(p) = profile_for(v) {
        return Ok(p);
    }
    let newest = PROFILES[PROFILES.len() - 1];
    if allow_untested {
        return Ok(newest);
    }
    Err(format!(
        "Claude Code {}.{}.{} is not supported by this engine (supported: {}); \
         nothing was spawned. Set {}=1 to run it with the {} profile anyway",
        v.0,
        v.1,
        v.2,
        PROFILES
            .iter()
            .map(|p| p.range())
            .collect::<Vec<_>>()
            .join("; "),
        ENV_ALLOW_UNTESTED,
        newest.name
    ))
}

pub fn allow_untested() -> bool {
    std::env::var(ENV_ALLOW_UNTESTED).as_deref() == Ok("1")
}

/// Run `<claude> --version` with a timeout. Blocking.
pub fn probe_version(claude: &Path) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use wait_timeout::ChildExt;
    let mut child = Command::new(claude)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {}", claude.display(), e))?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    // The output is one short line: read it on a thread so a full pipe
    // never blocks the wait.
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut stdout).take(4096).read_to_string(&mut s);
        s
    });
    match child.wait_timeout(VERSION_TIMEOUT) {
        Ok(Some(status)) if status.success() => {}
        Ok(Some(status)) => return Err(format!("--version exited with {}", status)),
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "--version did not answer in {} s",
                VERSION_TIMEOUT.as_secs()
            ));
        }
        Err(e) => return Err(e.to_string()),
    }
    reader
        .join()
        .map_err(|_| "version reader panicked".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_version_output() {
        assert_eq!(parse_version("2.1.316 (Claude Code)"), Some((2, 1, 316)));
        assert_eq!(parse_version("Claude Code v2.2.0"), Some((2, 2, 0)));
        assert_eq!(parse_version("2.1 (Claude Code)"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn the_gate_covers_the_recorded_range_and_fails_closed() {
        let ok = |s: &str| gate(Ok(s.into()), false).map(|p| p.name);
        assert_eq!(ok("2.1.312 (Claude Code)"), Ok("cc-2.1"));
        // The CC installed when the profile was recorded.
        assert_eq!(ok("2.1.316 (Claude Code)"), Ok("cc-2.1"));
        assert_eq!(ok("2.1.999 (Claude Code)"), Ok("cc-2.1"));
        for bad in ["2.1.311 (Claude Code)", "2.2.0 (Claude Code)", "3.0.0"] {
            let e = gate(Ok(bad.into()), false).unwrap_err();
            assert!(e.contains("is not supported"), "{}", e);
            assert!(e.contains(">= 2.1.312, < 2.2.0"), "{}", e);
            assert!(e.contains(ENV_ALLOW_UNTESTED), "{}", e);
        }
        assert!(gate(Ok("garbage".into()), false)
            .unwrap_err()
            .contains("cannot read"));
        assert!(gate(Err("no such file".into()), true)
            .unwrap_err()
            .contains("cannot read"));
        // The override takes an untested version, never an unreadable one.
        assert_eq!(gate(Ok("2.2.0".into()), true).map(|p| p.name), Ok("cc-2.1"));
    }

    #[test]
    fn profiles_do_not_overlap() {
        for w in PROFILES.windows(2) {
            assert!(w[0].below_version <= w[1].min_version);
        }
        assert!(PROFILES.iter().any(|p| p.name == CURRENT.name));
    }

    #[test]
    fn probe_reads_a_fake_claude_and_fails_closed() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let fake = t.path().join("claude");
        std::fs::write(&fake, "#!/bin/sh\necho '2.1.316 (Claude Code)'\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            probe_version(&fake).unwrap().trim(),
            "2.1.316 (Claude Code)"
        );
        assert!(probe_version(&t.path().join("missing")).is_err());
        assert!(probe_version(Path::new("/usr/bin/false")).is_err());
    }
}
