//! Test-only input and screen path for `tests/live/cc_sandbox.sh` (plan
//! C1a-32). Built only with the `live-test` feature, which no release,
//! default or CI build turns on: c9p/1 has no method that types into an
//! agent's terminal in C1a, and the live X14 run needs one.
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
