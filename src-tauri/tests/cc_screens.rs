//! Claude Code screen rules against real CC 2.1.312 screens, captured by the
//! C0 spike (runs R1, R3 and probe1) and rendered by its VT model. Ported
//! from the spike's `tests/cc_screens.rs`.

#![cfg(all(unix, feature = "engine"))]

use c9watch_lib::engine::cc_screen::{self, ReadyInputs, ScreenState};
use std::time::Duration;

const TRUST_DEFAULT: &str = include_str!("fixtures/cc_screens/cc2.1.312_trust_default.txt");
const TRUST_YES: &str = include_str!("fixtures/cc_screens/cc2.1.312_trust_yes.txt");
const READY: &str = include_str!("fixtures/cc_screens/cc2.1.312_ready_prompt.txt");
const BLOCKED: &str = include_str!("fixtures/cc_screens/cc2.1.312_blocked_bash.txt");
const AUTO_DENY: &str = include_str!("fixtures/cc_screens/cc2.1.312_auto_deny_screen.txt");
const IDLE_TITLE: &str = "\u{2733} Claude Code";

#[test]
fn trust_dialog_real_screens() {
    assert!(cc_screen::trust_dialog(TRUST_DEFAULT));
    // Default highlight is "No, exit": a CR here would exit CC.
    assert!(!cc_screen::trust_yes_selected(TRUST_DEFAULT));
    assert!(cc_screen::trust_yes_selected(TRUST_YES));
    assert_eq!(
        cc_screen::screen_state(IDLE_TITLE, TRUST_DEFAULT),
        ScreenState::Trust
    );
    // Moving the highlight is the same dialog.
    assert_eq!(
        cc_screen::prompt_hash(TRUST_DEFAULT),
        cc_screen::prompt_hash(TRUST_YES)
    );
}

#[test]
fn ready_prompt_real_screen() {
    let i = ReadyInputs {
        session_start: true,
        title: IDLE_TITLE,
        screen: READY,
        since_last_output: Duration::from_millis(350),
    };
    assert_eq!(cc_screen::ready(&i), Ok(()));
    assert!(!cc_screen::trust_dialog(READY));
}

#[test]
fn blocked_real_screen_with_idle_title() {
    // R3: the title was the idle glyph while this prompt was open (D24.5).
    let title = "\u{2733} Hello.txt commit test";
    assert_eq!(
        cc_screen::screen_state(title, BLOCKED),
        ScreenState::Blocked
    );
    let i = ReadyInputs {
        session_start: true,
        title,
        screen: BLOCKED,
        since_last_output: Duration::from_secs(5),
    };
    assert_eq!(cc_screen::ready(&i), Err(cc_screen::NotReady::DialogOpen));
    // Option 2 writes a persistent allow rule; the engine never sends it.
    assert!(BLOCKED.contains("2. Yes, and always allow"));
    // The prompt hash covers the prompt box only, not the chat above it.
    let prompt = cc_screen::prompt_text(BLOCKED);
    assert!(prompt.starts_with("Bash command\n"), "{}", prompt);
    assert!(!prompt.contains("Task brief"));
}

#[test]
fn auto_deny_notice_real_screen() {
    // Live run probe1: the classifier denied `curl ... | sh`; CC kept
    // working and showed this status-line notice, no dialog.
    assert_eq!(
        cc_screen::auto_deny_notice(AUTO_DENY).as_deref(),
        Some("bash denied by auto mode \u{b7} [Code from External] \u{b7} /permissions")
    );
    assert_eq!(
        cc_screen::screen_state("\u{25d0} Claude Code", AUTO_DENY),
        ScreenState::Working
    );
    assert_eq!(cc_screen::auto_deny_notice(READY), None);
    assert_eq!(cc_screen::auto_deny_notice(BLOCKED), None);
}
