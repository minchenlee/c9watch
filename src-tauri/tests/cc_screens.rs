//! Claude Code screen rules against real CC 2.1.312 screens, captured by the
//! C0 spike (runs R1, R3 and probe1) and rendered by its VT model. Ported
//! from the spike's `tests/cc_screens.rs`.

#![cfg(all(unix, feature = "engine"))]

use c9watch_lib::engine::cc_screen::{self, InputBox, ReadyInputs, ScreenState};
use std::time::Duration;

const TRUST_DEFAULT: &str = include_str!("fixtures/cc_screens/cc2.1.312_trust_default.txt");
const TRUST_YES: &str = include_str!("fixtures/cc_screens/cc2.1.312_trust_yes.txt");
const READY: &str = include_str!("fixtures/cc_screens/cc2.1.312_ready_prompt.txt");
const BLOCKED: &str = include_str!("fixtures/cc_screens/cc2.1.312_blocked_bash.txt");
const AUTO_DENY: &str = include_str!("fixtures/cc_screens/cc2.1.312_auto_deny_screen.txt");
const IDLE_TITLE: &str = "\u{2733} Claude Code";

// CC 2.1.314 input box, captured live for C1b-M1-3 through the engine's
// `live-test` screen file. No prompt was submitted except the one before
// `input_empty` (a one-word Haiku turn): CC shows the `Try "…"`
// placeholder until the first prompt and a bare `❯` after it.
const BOX_PLACEHOLDER: &str = include_str!("fixtures/cc_screens/cc2.1.314_input_placeholder.txt");
const BOX_EMPTY: &str = include_str!("fixtures/cc_screens/cc2.1.314_input_empty.txt");
const BOX_TYPED: &str = include_str!("fixtures/cc_screens/cc2.1.314_input_typed.txt");
const BOX_PASTED_SHORT: &str = include_str!("fixtures/cc_screens/cc2.1.314_input_pasted_short.txt");
const BOX_PASTED_MULTILINE: &str =
    include_str!("fixtures/cc_screens/cc2.1.314_input_pasted_multiline.txt");
const BOX_PASTED_COLLAPSED: &str =
    include_str!("fixtures/cc_screens/cc2.1.314_input_pasted_collapsed.txt");
// `editorMode: vim` from the project settings, before the spawn profile
// pinned `normal`.
const VIM_INSERT: &str = include_str!("fixtures/cc_screens/cc2.1.314_vim_insert.txt");
const VIM_NORMAL: &str = include_str!("fixtures/cc_screens/cc2.1.314_vim_normal.txt");

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

#[test]
fn input_box_real_screens() {
    // The status line below the box is not the box (`prompt_text` reads it).
    assert_eq!(cc_screen::input_box(READY), InputBox::Placeholder);
    assert!(cc_screen::prompt_text(READY).contains("auto mode on"));
    assert_eq!(cc_screen::input_box(BOX_PLACEHOLDER), InputBox::Placeholder);
    assert_eq!(cc_screen::input_box(BOX_EMPTY), InputBox::Empty);
    assert_eq!(
        cc_screen::input_box(BOX_TYPED),
        InputBox::Text("please look at the".into())
    );
    let short = cc_screen::input_box(BOX_PASTED_SHORT);
    assert_eq!(
        short,
        InputBox::Text(
            "[c9watch mail msg_01JBCAPTURE00000000000001 \u{b7} tok 7f3a9c2e00000000]".into()
        )
    );
    assert!(cc_screen::box_has_paste(
        &short,
        "msg_01JBCAPTURE00000000000001"
    ));
    // A paste of more than one line is collapsed, even four short lines.
    for (screen, chip) in [
        (BOX_PASTED_MULTILINE, "[Pasted text #1 +3 lines]"),
        (BOX_PASTED_COLLAPSED, "[Pasted text #2 +61 lines]"),
    ] {
        let b = cc_screen::input_box(screen);
        assert_eq!(b, InputBox::Text(chip.into()));
        assert!(cc_screen::box_has_paste(&b, "msg_any"));
        assert!(!cc_screen::box_is_free(&b));
    }
    for free in [READY, BOX_PLACEHOLDER, BOX_EMPTY] {
        assert!(cc_screen::box_is_free(&cc_screen::input_box(free)));
    }
    assert!(!cc_screen::box_is_free(&cc_screen::input_box(BOX_TYPED)));
    // Not a box at all.
    assert_eq!(cc_screen::input_box(TRUST_DEFAULT), InputBox::Missing);
}

#[test]
fn vim_normal_mode_cannot_be_told_from_the_screen() {
    // INSERT shows a marker; NORMAL shows nothing, so the box reads as a
    // free placeholder although a paste would run as vim commands. The
    // spawn profile pins `editorMode: normal` instead (C1b-M1-3).
    assert!(VIM_INSERT.contains("-- INSERT --"));
    assert!(!VIM_NORMAL.contains("-- INSERT --") && !VIM_NORMAL.contains("NORMAL"));
    assert_eq!(cc_screen::input_box(VIM_NORMAL), InputBox::Placeholder);
}
