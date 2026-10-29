//! Claude Code screen detection (plan C1a-20): `ready`, the trust dialog,
//! `blocked` and screen-sourced `needs_attention`, from the VT model's text
//! and window title.
//!
//! Ported from the C0 spike (`detect.rs`, and the main loop of
//! `c0-engine.rs:600-700`). The rules (D24.3-24.5, D:245-247):
//! - `ready` = a SessionStart of the current generation AND an OSC title
//!   that starts with `✳` AND no dialog on screen AND ~300 ms of PTY quiet.
//!   A spawn or resume that is not ready within `READY_TIMEOUT` has failed.
//! - The trust dialog is a spawn step: the engine sends Down, waits until
//!   "Yes, I trust this folder" is highlighted, then sends CR. Never a bare
//!   CR: the default option is "No, exit". Only before the first ready.
//! - `blocked` comes from the screen text: the `✳` title also shows while a
//!   permission prompt is open, and the PermissionRequest hook arrives
//!   after the title.
//! - The engine never answers a prompt. A prompt or dialog is reported as
//!   `needs_attention` once per prompt, keyed by a hash of the prompt's
//!   text (the spike keyed it by timestamp, `c0-engine.rs:648`, so every
//!   redraw was a new report).
//!
//! **Deviation from the design (A:1037, A:1065):** the design keeps screen
//! rules in a data file per agent, read by a shared screen-rule engine. C1a
//! has one agent, Claude Code, so its rules are hard-coded here. The data
//! file comes with the second agent kind (C2).
//!
//! Everything here is pure: the caller passes the screen and the clock.

use std::collections::HashSet;
use std::time::{Duration, Instant};

/// U+2733, bytes e2 9c b3: CC's idle window-title glyph (D24.4).
pub const IDLE_GLYPH: char = '\u{2733}';

/// The selection cursor of CC's menus.
const SELECT_GLYPH: char = '\u{276f}';

/// Screen text that means a dialog is open. Mail and briefs are never pasted
/// while any of these shows (D24.4, D24.5).
pub const DIALOG_MARKERS: &[&str] = &[
    "Do you want to proceed?",
    "Esc to cancel",
    "trust this folder",
];

/// Quiet time on the PTY before `ready` (D24.4).
pub const QUIET: Duration = Duration::from_millis(300);

/// A spawn or resume that is not `ready` by then has failed (D24.4: the
/// worst seen was 10.9 s). The spike had only a 240 s run timeout.
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Quiet time after Down before the engine checks the trust highlight.
const TRUST_SETTLE: Duration = Duration::from_millis(150);

/// How long the highlight may take to move to "Yes" after Down.
pub const TRUST_SELECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Cursor Down, as the keyboard sends it.
pub const KEY_DOWN: &[u8] = b"\x1b[B";

pub const KEY_CR: &[u8] = b"\r";

pub fn title_is_idle(title: &str) -> bool {
    title.starts_with(IDLE_GLYPH)
}

/// D24.3: the workspace trust dialog.
pub fn trust_dialog(screen: &str) -> bool {
    screen.contains("trust this folder") || screen.contains("Accessing workspace:")
}

/// The highlighted trust option is "Yes, I trust this folder". The engine
/// sends CR only when this holds, never a bare CR (default is "No, exit").
pub fn trust_yes_selected(screen: &str) -> bool {
    screen
        .lines()
        .any(|l| l.contains(SELECT_GLYPH) && l.contains("trust this folder") && l.contains("Yes"))
}

/// D24.5: a permission prompt is open.
pub fn permission_prompt(screen: &str) -> bool {
    screen.contains("Do you want to proceed?")
}

pub fn dialog_open(screen: &str) -> bool {
    DIALOG_MARKERS.iter().any(|m| screen.contains(m))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotReady {
    NoSessionStart,
    TitleNotIdle,
    DialogOpen,
    NotQuiet,
}

/// Inputs to the D24.4 ready rule.
pub struct ReadyInputs<'a> {
    /// SessionStart arrived for the current generation.
    pub session_start: bool,
    pub title: &'a str,
    pub screen: &'a str,
    pub since_last_output: Duration,
}

/// D24.4: SessionStart AND idle title AND no dialog AND ~300 ms quiet.
/// `ESC[?2004h` alone is deliberately not an input.
pub fn ready(i: &ReadyInputs) -> Result<(), NotReady> {
    if !i.session_start {
        return Err(NotReady::NoSessionStart);
    }
    if !title_is_idle(i.title) {
        return Err(NotReady::TitleNotIdle);
    }
    if dialog_open(i.screen) {
        return Err(NotReady::DialogOpen);
    }
    if i.since_last_output < QUIET {
        return Err(NotReady::NotQuiet);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenState {
    Trust,
    Blocked,
    /// Some other dialog (`Esc to cancel`) that is not a permission prompt.
    Dialog,
    Idle,
    Working,
    /// No title yet: no rule matches, and the screen never assumes idle
    /// (A:1037).
    Unknown,
}

/// Screen-only state (D24.5: the idle title also shows while a permission
/// prompt is open, so screen text decides `blocked`).
pub fn screen_state(title: &str, screen: &str) -> ScreenState {
    if trust_dialog(screen) {
        ScreenState::Trust
    } else if permission_prompt(screen) {
        ScreenState::Blocked
    } else if dialog_open(screen) {
        ScreenState::Dialog
    } else if title_is_idle(title) {
        ScreenState::Idle
    } else if title.is_empty() {
        ScreenState::Unknown
    } else {
        ScreenState::Working
    }
}

/// CC 2.1.312 status-line notice after an auto-mode denial, e.g.
/// `bash denied by auto mode · [Code from External] · /permissions`.
/// Returns the notice line. It is transient (seen ~8 s in run probe1) and
/// carries no tool_use_id, so the PermissionDenied hook stays the primary
/// signal and this one only confirms.
pub fn auto_deny_notice(screen: &str) -> Option<String> {
    screen
        .lines()
        .map(str::trim)
        .find(|l| l.contains(" denied by auto mode"))
        .map(String::from)
}

/// Screen text, lower case, of a vendor that refuses to start a session:
/// a usage limit or a missing login (D28 Q7). The wording is CC's and may
/// change; when nothing matches, the ready timeout still fails the spawn,
/// only with a less clear message (plan §6.4).
pub const VENDOR_REFUSALS: &[&str] = &[
    "usage limit",
    "hit your limit",
    "limit reached",
    "credit balance is too low",
    "please run /login",
    "not logged in",
    "invalid api key",
    "oauth token has expired",
    "select login method",
];

/// The lines of `screen` that show a vendor refusal, joined, or `None`.
/// Only for the screen before the first ready: after it, the agent's own
/// output may contain the words.
pub fn vendor_refusal(screen: &str) -> Option<String> {
    let lines: Vec<&str> = screen
        .lines()
        .map(str::trim)
        .filter(|l| {
            let l = l.to_lowercase();
            VENDOR_REFUSALS.iter().any(|m| l.contains(m))
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Why a screen needs a human. The engine never answers a prompt (O1 = A).
/// The trust dialog is a spawn step (D24.3) and needs attention only when
/// it shows after the first ready.
pub fn attention_reason(state: ScreenState, after_first_ready: bool) -> Option<&'static str> {
    match state {
        ScreenState::Blocked => Some("permission_prompt"),
        ScreenState::Dialog => Some("dialog"),
        ScreenState::Trust if after_first_ready => Some("trust_dialog_after_ready"),
        _ => None,
    }
}

/// The trust matcher runs only before the first ready (C1a): after that a
/// worker's own output could contain the words.
pub fn answer_trust(screen: &str, after_first_ready: bool) -> bool {
    !after_first_ready && trust_dialog(screen)
}

/// Bracketed-paste payload (D24.8). The body loses any `ESC[201~`, which
/// would end the paste early. The CR is sent separately by the caller.
pub fn paste_payload(body: &str) -> Vec<u8> {
    let clean = body.replace("\x1b[201~", "");
    let mut v = b"\x1b[200~".to_vec();
    v.extend_from_slice(clean.as_bytes());
    v.extend_from_slice(b"\x1b[201~");
    v
}

/// A line drawn of `─` only: CC's full-width rule above a prompt.
fn is_rule(line: &str) -> bool {
    let t = line.trim();
    t.chars().count() >= 10 && t.chars().all(|c| c == '\u{2500}')
}

/// The text of the open prompt: the lines below the last full-width rule
/// that comes before the first dialog marker (the whole screen when there
/// is no rule), with the selection cursor and blank lines taken out. Moving
/// the highlight or redrawing the same prompt keeps the same text.
pub fn prompt_text(screen: &str) -> String {
    let lines: Vec<&str> = screen.lines().collect();
    let marker = lines
        .iter()
        .position(|l| DIALOG_MARKERS.iter().any(|m| l.contains(m)))
        .unwrap_or(lines.len());
    let start = lines[..marker]
        .iter()
        .rposition(|l| is_rule(l))
        .map_or(0, |i| i + 1);
    lines[start..]
        .iter()
        .map(|l| l.replace(SELECT_GLYPH, " ").trim().to_string())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// FNV-1a 64 of `prompt_text`, as 16 hex digits. Stable across runs and
/// builds (std's hasher is not), and no new dependency.
pub fn prompt_hash(screen: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in prompt_text(screen).bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:016x}", h)
}

/// What the screen tracker found in one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenEvent {
    /// Send these bytes to the terminal. Only ever the trust-dialog keys.
    Write(&'static [u8]),
    /// The trust dialog was answered (Down, then CR).
    TrustAnswered,
    /// The screen state changed.
    State(ScreenState),
    /// The agent became ready. `first` is the first ready of this
    /// generation, which ends the spawn or resume.
    Ready { first: bool },
    /// A prompt or dialog that a human must answer. Once per prompt hash.
    NeedsAttention {
        reason: &'static str,
        prompt_hash: String,
        screen: String,
    },
    /// The auto-mode denial notice appeared. It confirms the
    /// PermissionDenied hook; the state merge (C1a-22) pairs them.
    AutoDenyNotice(String),
    /// The spawn or resume failed; the caller stops the agent. No retry.
    Failed(ScreenFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenFailure {
    /// Not ready within `READY_TIMEOUT`.
    ReadyTimeout,
    /// The trust highlight did not move to "Yes" after Down.
    TrustSelectFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trust {
    Waiting,
    DownSent(Instant),
    Answered,
}

/// The screen side of one CC agent generation. The engine makes a new one
/// at each spawn and resume, feeds it every screen change and tick, and
/// tells it when the generation's SessionStart arrives.
pub struct CcScreen {
    started: Instant,
    session_start: bool,
    ever_ready: bool,
    ready_now: bool,
    trust: Trust,
    state: Option<ScreenState>,
    attention_seen: HashSet<String>,
    deny_notice: Option<String>,
    failed: bool,
}

impl CcScreen {
    pub fn new(now: Instant) -> CcScreen {
        CcScreen {
            started: now,
            session_start: false,
            ever_ready: false,
            ready_now: false,
            trust: Trust::Waiting,
            state: None,
            attention_seen: HashSet::new(),
            deny_notice: None,
            failed: false,
        }
    }

    /// The SessionStart hook of this generation arrived.
    pub fn session_start(&mut self) {
        self.session_start = true;
    }

    pub fn ever_ready(&self) -> bool {
        self.ever_ready
    }

    pub fn state(&self) -> Option<ScreenState> {
        self.state
    }

    /// One look at the terminal: its title, rendered text and how long it
    /// has been quiet. After `Failed` it returns nothing more.
    pub fn step(
        &mut self,
        title: &str,
        text: &str,
        quiet: Duration,
        now: Instant,
    ) -> Vec<ScreenEvent> {
        let mut out = Vec::new();
        if self.failed {
            return out;
        }

        // D24.3: the trust dialog, before the first ready only.
        if self.trust != Trust::Answered && answer_trust(text, self.ever_ready) {
            match self.trust {
                Trust::Waiting if quiet >= QUIET => {
                    out.push(ScreenEvent::Write(KEY_DOWN));
                    self.trust = Trust::DownSent(now);
                }
                Trust::DownSent(_) if trust_yes_selected(text) && quiet >= TRUST_SETTLE => {
                    out.push(ScreenEvent::Write(KEY_CR));
                    out.push(ScreenEvent::TrustAnswered);
                    self.trust = Trust::Answered;
                }
                Trust::DownSent(at) if now.duration_since(at) > TRUST_SELECT_TIMEOUT => {
                    return self.fail(out, ScreenFailure::TrustSelectFailed);
                }
                _ => {}
            }
        }

        // D24.5: screen text decides blocked; the engine only reports it.
        let st = screen_state(title, text);
        if self.state != Some(st) {
            self.state = Some(st);
            out.push(ScreenEvent::State(st));
        }
        if let Some(reason) = attention_reason(st, self.ever_ready) {
            let hash = prompt_hash(text);
            if self.attention_seen.insert(hash.clone()) {
                out.push(ScreenEvent::NeedsAttention {
                    reason,
                    prompt_hash: hash,
                    screen: text.to_string(),
                });
            }
        }

        match auto_deny_notice(text) {
            Some(n) if self.deny_notice.as_deref() != Some(n.as_str()) => {
                out.push(ScreenEvent::AutoDenyNotice(n.clone()));
                self.deny_notice = Some(n);
            }
            None => self.deny_notice = None,
            _ => {}
        }

        // D24.4: ready, on the edge from not ready.
        let ri = ReadyInputs {
            session_start: self.session_start,
            title,
            screen: text,
            since_last_output: quiet,
        };
        let ready_now = ready(&ri).is_ok();
        if ready_now && !self.ready_now {
            out.push(ScreenEvent::Ready {
                first: !self.ever_ready,
            });
            self.ever_ready = true;
        }
        self.ready_now = ready_now;

        if !self.ever_ready && now.duration_since(self.started) >= READY_TIMEOUT {
            return self.fail(out, ScreenFailure::ReadyTimeout);
        }
        out
    }

    fn fail(&mut self, mut out: Vec<ScreenEvent>, why: ScreenFailure) -> Vec<ScreenEvent> {
        self.failed = true;
        out.push(ScreenEvent::Failed(why));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRUST: &str = "Accessing workspace:\n /tmp/x\n Quick safety check: Is this a project you created or one you trust?\n \u{276f} 1. No, exit\n   2. Yes, I trust this folder\n Enter to confirm \u{b7} Esc to cancel";
    const TRUST_YES: &str = "Accessing workspace:\n   1. No, exit\n \u{276f} 2. Yes, I trust this folder\n Enter to confirm \u{b7} Esc to cancel";
    const PERM: &str = " Bash command\n   git commit -m x\n Do you want to proceed?\n \u{276f} 1. Yes\n   2. Yes, and don't ask again\n   3. No\n Esc to cancel \u{b7} Tab to amend";
    const PROMPT: &str = "\u{2500}\u{2500}\u{2500}\n\u{276f} Try \"fix lint errors\"\n\u{2500}\u{2500}\u{2500}\n  \u{23f5}\u{23f5} auto mode on";
    const IDLE: &str = "\u{2733} Claude Code";
    const BUSY: &str = "\u{25d0} Claude Code";

    fn inputs<'a>(screen: &'a str) -> ReadyInputs<'a> {
        ReadyInputs {
            session_start: true,
            title: IDLE,
            screen,
            since_last_output: Duration::from_millis(400),
        }
    }

    #[test]
    fn trust_dialog_default_is_no() {
        assert!(trust_dialog(TRUST));
        assert!(!trust_yes_selected(TRUST));
        assert!(trust_yes_selected(TRUST_YES));
        assert!(!trust_dialog(PROMPT));
    }

    #[test]
    fn ready_needs_all_four() {
        assert_eq!(ready(&inputs(PROMPT)), Ok(()));
        let r = |f: &dyn Fn(&mut ReadyInputs)| {
            let mut i = inputs(PROMPT);
            f(&mut i);
            ready(&i)
        };
        assert_eq!(
            r(&|i| i.session_start = false),
            Err(NotReady::NoSessionStart)
        );
        assert_eq!(r(&|i| i.title = BUSY), Err(NotReady::TitleNotIdle));
        assert_eq!(r(&|i| i.title = ""), Err(NotReady::TitleNotIdle));
        assert_eq!(r(&|i| i.screen = TRUST), Err(NotReady::DialogOpen));
        assert_eq!(r(&|i| i.screen = PERM), Err(NotReady::DialogOpen));
        assert_eq!(
            r(&|i| i.since_last_output = Duration::from_millis(100)),
            Err(NotReady::NotQuiet)
        );
    }

    #[test]
    fn idle_title_during_prompt_is_blocked() {
        // D24.5: the title shows the idle glyph while the prompt is open.
        assert_eq!(screen_state(IDLE, PERM), ScreenState::Blocked);
        assert_eq!(screen_state(IDLE, PROMPT), ScreenState::Idle);
        assert_eq!(
            screen_state("\u{25d1} Claude Code", PROMPT),
            ScreenState::Working
        );
        assert_eq!(screen_state(IDLE, TRUST), ScreenState::Trust);
        assert_eq!(screen_state("", PROMPT), ScreenState::Unknown);
    }

    #[test]
    fn attention_after_ready_only_for_trust() {
        assert_eq!(
            attention_reason(ScreenState::Blocked, false),
            Some("permission_prompt")
        );
        assert_eq!(attention_reason(ScreenState::Trust, false), None);
        assert_eq!(
            attention_reason(ScreenState::Trust, true),
            Some("trust_dialog_after_ready")
        );
        assert_eq!(attention_reason(ScreenState::Idle, true), None);
        let other = " Some question?\n \u{276f} 1. A\n   2. B\n Esc to cancel";
        assert_eq!(screen_state("\u{2733} x", other), ScreenState::Dialog);
        assert_eq!(attention_reason(ScreenState::Dialog, true), Some("dialog"));
        assert!(answer_trust(TRUST, false));
        assert!(!answer_trust(TRUST, true));
    }

    #[test]
    fn paste_strips_end_marker() {
        let p = paste_payload("a\x1b[201~b\nc");
        assert_eq!(p, b"\x1b[200~ab\nc\x1b[201~".to_vec());
    }

    // The spike's sixth detect test, `query_replies`, moved to `vt.rs`.

    fn writes(ev: &[ScreenEvent]) -> Vec<&'static [u8]> {
        ev.iter()
            .filter_map(|e| match e {
                ScreenEvent::Write(b) => Some(*b),
                _ => None,
            })
            .collect()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn trust_is_down_then_cr_after_the_highlight_moves() {
        let t0 = Instant::now();
        let mut s = CcScreen::new(t0);
        // Not quiet yet: nothing is sent.
        assert!(writes(&s.step("", TRUST, ms(100), t0)).is_empty());
        let ev = s.step("", TRUST, ms(300), t0 + ms(400));
        assert_eq!(writes(&ev), vec![KEY_DOWN]);
        // The highlight has not moved: no CR, never a bare CR.
        assert!(writes(&s.step("", TRUST, ms(500), t0 + ms(900))).is_empty());
        let ev = s.step("", TRUST_YES, ms(200), t0 + ms(1100));
        assert_eq!(writes(&ev), vec![KEY_CR]);
        assert!(ev.contains(&ScreenEvent::TrustAnswered));
        // Answered once: the same screen sends nothing more.
        assert!(writes(&s.step("", TRUST_YES, ms(900), t0 + ms(1500))).is_empty());
        // Before the first ready, the trust dialog is not needs_attention.
        assert!(!s
            .step("", TRUST, ms(900), t0 + ms(1600))
            .iter()
            .any(|e| matches!(e, ScreenEvent::NeedsAttention { .. })));
    }

    #[test]
    fn trust_highlight_that_never_moves_fails_the_spawn() {
        let t0 = Instant::now();
        let mut s = CcScreen::new(t0);
        s.step("", TRUST, ms(300), t0);
        let ev = s.step("", TRUST, ms(300), t0 + TRUST_SELECT_TIMEOUT + ms(1));
        assert!(writes(&ev).is_empty());
        assert_eq!(
            ev.last(),
            Some(&ScreenEvent::Failed(ScreenFailure::TrustSelectFailed))
        );
        assert!(s.step("", TRUST_YES, ms(300), t0 + ms(5000)).is_empty());
    }

    #[test]
    fn ready_is_an_edge_and_times_out_after_30_s() {
        let t0 = Instant::now();
        let mut s = CcScreen::new(t0);
        // Idle title and quiet, but no SessionStart yet.
        assert!(!s
            .step(IDLE, PROMPT, ms(400), t0 + ms(500))
            .iter()
            .any(|e| matches!(e, ScreenEvent::Ready { .. })));
        s.session_start();
        let ev = s.step(IDLE, PROMPT, ms(400), t0 + ms(600));
        assert!(ev.contains(&ScreenEvent::Ready { first: true }));
        assert!(s.ever_ready());
        // Still ready: no second event.
        assert!(!s
            .step(IDLE, PROMPT, ms(900), t0 + ms(900))
            .iter()
            .any(|e| matches!(e, ScreenEvent::Ready { .. })));
        s.step(BUSY, PROMPT, ms(0), t0 + ms(1000));
        let ev = s.step(IDLE, PROMPT, ms(400), t0 + ms(2000));
        assert!(ev.contains(&ScreenEvent::Ready { first: false }));
        // After the first ready there is no timeout.
        assert!(!s
            .step(BUSY, PROMPT, ms(0), t0 + READY_TIMEOUT * 2)
            .iter()
            .any(|e| matches!(e, ScreenEvent::Failed(_))));

        let mut late = CcScreen::new(t0);
        late.session_start();
        assert!(late.step(BUSY, PROMPT, ms(0), t0 + ms(29_000)).len() == 1);
        let ev = late.step(BUSY, PROMPT, ms(0), t0 + READY_TIMEOUT);
        assert_eq!(
            ev.last(),
            Some(&ScreenEvent::Failed(ScreenFailure::ReadyTimeout))
        );
    }

    #[test]
    fn a_prompt_is_reported_once_by_hash_and_never_answered() {
        let t0 = Instant::now();
        let mut s = CcScreen::new(t0);
        s.session_start();
        s.step(IDLE, PROMPT, ms(400), t0);
        let ev = s.step(IDLE, PERM, ms(400), t0 + ms(1000));
        assert!(writes(&ev).is_empty());
        assert!(ev.contains(&ScreenEvent::State(ScreenState::Blocked)));
        let first: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                ScreenEvent::NeedsAttention {
                    reason,
                    prompt_hash,
                    ..
                } => Some((*reason, prompt_hash.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, "permission_prompt");
        // The highlight moves, the prompt goes away and comes back: the
        // same prompt, the same hash, no second report and no keys.
        let moved = PERM
            .replace("\u{276f} 1. Yes", "  1. Yes")
            .replace("  3. No", "\u{276f} 3. No");
        assert_eq!(prompt_hash(&moved), first[0].1);
        for (i, screen) in [moved.as_str(), PROMPT, PERM].iter().enumerate() {
            let ev = s.step(IDLE, screen, ms(400), t0 + ms(2000 + i as u64 * 100));
            assert!(writes(&ev).is_empty());
            assert!(!ev
                .iter()
                .any(|e| matches!(e, ScreenEvent::NeedsAttention { .. })));
        }
        // A different prompt is a new report.
        let other = PERM.replace("git commit -m x", "rm -rf build");
        assert_ne!(prompt_hash(&other), first[0].1);
        let ev = s.step(IDLE, &other, ms(400), t0 + ms(3000));
        assert!(ev
            .iter()
            .any(|e| matches!(e, ScreenEvent::NeedsAttention { .. })));
        // A trust dialog after ready is never answered either.
        let ev = s.step(IDLE, TRUST, ms(400), t0 + ms(4000));
        assert!(writes(&ev).is_empty());
        assert!(ev.iter().any(|e| matches!(
            e,
            ScreenEvent::NeedsAttention {
                reason: "trust_dialog_after_ready",
                ..
            }
        )));
    }

    #[test]
    fn prompt_text_starts_below_the_last_rule() {
        let screen = format!(
            "chat line\n{}\n Bash command\n   ls\n Do you want to proceed?\n \u{276f} 1. Yes\n\n Esc to cancel",
            "\u{2500}".repeat(20)
        );
        assert_eq!(
            prompt_text(&screen),
            "Bash command\nls\nDo you want to proceed?\n1. Yes\nEsc to cancel"
        );
        // Chat above the rule does not change the hash.
        let scrolled = screen.replace("chat line", "other chat");
        assert_eq!(prompt_hash(&screen), prompt_hash(&scrolled));
    }

    #[test]
    fn vendor_refusals_are_found_with_their_lines() {
        let limit =
            "  \u{23bf}  Claude usage limit reached. Your limit will reset at 5pm\n\n\u{276f} ";
        assert_eq!(
            vendor_refusal(limit).as_deref(),
            Some("\u{23bf}  Claude usage limit reached. Your limit will reset at 5pm")
        );
        let login = "Invalid API key \u{b7} Please run /login\nother";
        assert_eq!(
            vendor_refusal(login).as_deref(),
            Some("Invalid API key \u{b7} Please run /login")
        );
        assert!(vendor_refusal(PROMPT).is_none());
        assert!(vendor_refusal(TRUST).is_none());
        assert!(vendor_refusal(PERM).is_none());
    }
}
