//! Test-only input and screen path for `tests/live/cc_sandbox.sh` (plan
//! C1a-32). Built only with the `live-test` feature, which no release,
//! default or CI build turns on. The live X14 run (C1a) needs raw keys;
//! `msg.send` (C1b-M1) types only a fenced mail paste into an agent's
//! terminal, never keys of the caller's choosing.
//!
//! - Input: a file `<home>/live-test/<agent_id>.in`. The supervisor of a
//!   ready agent reads it, removes it and sends its bytes as they are
//!   through the same PTY write as the engine's own keys, so a drain blocks
//!   and audits them like any other PTY write. The script writes the file
//!   under a temp name and renames it, so a half-written file is never
//!   read.
//! - Screen: `<home>/live-test/<agent_id>.screen` holds the rendered screen
//!   whenever it changes, written through a temp file and a rename.
//!
//! The directory is created 0700 by the test script; the engine never
//! creates it, so without it nothing here does anything.
//!
//! - Crash: with `C9WATCH_LIVE_TEST_CRASH_AFTER=<phase>` (`row`, `pty` or
//!   `bound`) in its env, the engine SIGKILLs itself right after it
//!   commits that phase of a create (`tests/engine_cli.rs`, exit
//!   criterion X9). The CLI's auto-start does not pass the variable on, so
//!   only a foreground `c9watch engine` started with it crashes.

//!
//! - D16 settings (`tests/live/d16_root.sh`): when `<home>/live-test/
//!   d16-root` names a directory that is the parent of the agent's cwd,
//!   the spawn settings take the D16 managed-root rules
//!   (`profile::managed_root_deny_rules`, `managed_root_deny_write`) in
//!   place of the rules that name the cwd's own `.claude/`, so only the
//!   root rules can stop a write there. `<home>/live-test/extra-deny.json`
//!   (a JSON array of strings) is appended to `permissions.deny` as it is,
//!   and `extra-deny-write.json` to `sandbox.filesystem.denyWrite`: a
//!   sandbox-only probe that no file-tool rule and no `.claude` name (which
//!   the auto-mode classifier refuses on its own) can stand in for.
//!   The script checks the spawn settings file before it sends a prompt.

use std::path::{Path, PathBuf};

/// `<home>/live-test`
pub const DIR: &str = "live-test";

/// Longest input taken at once.
const MAX_INPUT: u64 = 64 * 1024;

fn dir(home: &Path) -> PathBuf {
    home.join(DIR)
}

/// The pending input for `agent_id`, removed once read. `None` when there
/// is none, or the file is too big (it is then removed unread).
pub fn take_input(home: &Path, agent_id: &str) -> Option<Vec<u8>> {
    let path = dir(home).join(format!("{}.in", agent_id));
    let meta = std::fs::symlink_metadata(&path).ok()?;
    let ok = meta.is_file() && meta.len() <= MAX_INPUT;
    let bytes = if ok { std::fs::read(&path).ok() } else { None };
    let _ = std::fs::remove_file(&path);
    bytes.filter(|b| !b.is_empty())
}

/// The env variable of the crash failpoint.
pub const CRASH_AFTER_ENV: &str = "C9WATCH_LIVE_TEST_CRASH_AFTER";

/// SIGKILL this process when `CRASH_AFTER_ENV` names `phase`. It waits
/// 1 s first, on the thread that would write the next phase, so an agent
/// spawned in phase `pty` is past its start when the engine dies.
pub fn crash_after(phase: super::ops::Phase) {
    if std::env::var(CRASH_AFTER_ENV).as_deref() == Ok(phase.as_str()) {
        std::thread::sleep(std::time::Duration::from_secs(1));
        unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
    }
}

/// `<home>/live-test/d16-root`
pub const D16_ROOT: &str = "d16-root";

/// `<home>/live-test/extra-deny.json`
pub const EXTRA_DENY: &str = "extra-deny.json";

/// `<home>/live-test/extra-deny-write.json`
pub const EXTRA_DENY_WRITE: &str = "extra-deny-write.json";

/// Apply the D16 test settings to the `--settings` value `s` of `p` (see
/// the module doc). Nothing changes without the files.
pub fn patch_settings(p: &crate::engine::adapter::cc::profile::Profile, s: &mut serde_json::Value) {
    use crate::engine::adapter::cc::profile::{managed_root_deny_rules, managed_root_deny_write};
    let Some(home) = p.agent_bin.parent().and_then(Path::parent) else {
        return;
    };
    let d = dir(home);
    let root = std::fs::read_to_string(d.join(D16_ROOT))
        .ok()
        .map(|r| PathBuf::from(r.trim()))
        .filter(|r| r.is_absolute() && p.cwd.parent() == Some(r.as_path()));
    if let Some(root) = root {
        let cwd = p.cwd.display().to_string();
        let own_rule = format!("Edit(/{}/", cwd);
        let own_dir = format!("{}/.claude", cwd);
        if let Some(deny) = s["permissions"]["deny"].as_array_mut() {
            deny.retain(|r| !r.as_str().is_some_and(|r| r.starts_with(&own_rule)));
            deny.extend(managed_root_deny_rules(&root).into_iter().map(Into::into));
        }
        if let Some(dw) = s["sandbox"]["filesystem"]["denyWrite"].as_array_mut() {
            dw.retain(|r| r.as_str() != Some(own_dir.as_str()));
            dw.extend(managed_root_deny_write(&root).into_iter().map(Into::into));
        }
    }
    let extra = |name: &str| -> Vec<String> {
        std::fs::read(d.join(name))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    };
    if let Some(deny) = s["permissions"]["deny"].as_array_mut() {
        deny.extend(extra(EXTRA_DENY).into_iter().map(Into::into));
    }
    if let Some(dw) = s["sandbox"]["filesystem"]["denyWrite"].as_array_mut() {
        dw.extend(extra(EXTRA_DENY_WRITE).into_iter().map(Into::into));
    }
}

/// Write `text` as the screen of `agent_id` when it differs from `last`.
pub fn dump_screen(home: &Path, agent_id: &str, text: &str, last: &mut String) {
    if text == last.as_str() {
        return;
    }
    let d = dir(home);
    if !d.is_dir() {
        return;
    }
    let tmp = d.join(format!(".{}.screen.tmp", agent_id));
    let path = d.join(format!("{}.screen", agent_id));
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
        *last = text.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_is_taken_once_and_screen_only_when_dir_exists() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        let mut last = String::new();
        dump_screen(home, "agt_1", "hello", &mut last);
        assert!(last.is_empty(), "no dir, no dump");
        assert_eq!(take_input(home, "agt_1"), None);

        std::fs::create_dir(home.join(DIR)).unwrap();
        std::fs::write(home.join(DIR).join("agt_1.in"), b"hi\r").unwrap();
        assert_eq!(take_input(home, "agt_1"), Some(b"hi\r".to_vec()));
        assert_eq!(take_input(home, "agt_1"), None);

        dump_screen(home, "agt_1", "hello", &mut last);
        let screen = home.join(DIR).join("agt_1.screen");
        assert_eq!(std::fs::read_to_string(&screen).unwrap(), "hello");
        assert_eq!(last, "hello");
    }

    #[cfg(feature = "live-test")]
    #[test]
    fn d16_files_swap_the_own_claude_rules_for_the_root_rules() {
        use crate::engine::adapter::cc::profile::{settings, Profile};
        let t = tempfile::tempdir().unwrap();
        let home = t.path().canonicalize().unwrap().join("home");
        let root = home.parent().unwrap().join("worktrees");
        let cwd = root.join("projA");
        std::fs::create_dir_all(home.join(DIR)).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let bin = home.join("bin").join("c9watch-agent");
        let sid = "6f1c2b8e-3d4a-4b5c-9e7f-0a1b2c3d4e5f";
        let p = Profile::new(&bin, Path::new("/s/e.sock"), &cwd, "agt_1", 1, sid).unwrap();
        let strs =
            |v: &serde_json::Value| -> Vec<String> { serde_json::from_value(v.clone()).unwrap() };
        let own = format!("Edit(/{}/.claude/**)", cwd.display());
        let own_dir = format!("{}/.claude", cwd.display());

        // No files: the profile as built.
        let s = settings(&p);
        assert!(strs(&s["permissions"]["deny"]).contains(&own));

        std::fs::write(
            home.join(DIR).join(D16_ROOT),
            format!("{}\n", root.display()),
        )
        .unwrap();
        std::fs::write(home.join(DIR).join(EXTRA_DENY), r#"["Edit(//x/**)"]"#).unwrap();
        std::fs::write(home.join(DIR).join(EXTRA_DENY_WRITE), r#"["/x/*/probe"]"#).unwrap();
        let s = settings(&p);
        let deny = strs(&s["permissions"]["deny"]);
        let dw = strs(&s["sandbox"]["filesystem"]["denyWrite"]);
        let r = root.display();
        assert!(!deny
            .iter()
            .any(|d| d.starts_with(&format!("Edit(/{}/", cwd.display()))));
        assert!(deny.contains(&format!("Edit(/{}/**/.claude/**)", r)));
        assert!(deny.contains(&"Edit(//x/**)".to_string()));
        assert!(!dw.contains(&own_dir));
        assert!(dw.contains(&format!("{}/*/.claude", r)));
        assert!(dw.contains(&"/x/*/probe".to_string()));
        // The engine's own paths stay denied.
        assert!(dw.iter().any(|d| d.ends_with("/bin")));

        // A cwd that is not a child of the root keeps its own rules.
        let other = home.parent().unwrap().join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        let q = Profile::new(&bin, Path::new("/s/e.sock"), &other, "agt_2", 1, sid).unwrap();
        let s = settings(&q);
        assert!(strs(&s["permissions"]["deny"])
            .contains(&format!("Edit(/{}/.claude/**)", other.display())));
        assert!(
            !strs(&s["sandbox"]["filesystem"]["denyWrite"]).contains(&format!("{}/*/.claude", r))
        );
    }

    #[test]
    fn oversized_input_is_dropped() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join(DIR)).unwrap();
        let path = t.path().join(DIR).join("agt_1.in");
        std::fs::write(&path, vec![b'x'; MAX_INPUT as usize + 1]).unwrap();
        assert_eq!(take_input(t.path(), "agt_1"), None);
        assert!(!path.exists());
    }
}
