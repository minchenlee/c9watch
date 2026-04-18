use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct SessionFile {
    #[serde(rename = "sessionId")]
    session_id: String,
}

/// Returns the Claude Code session ID of the process tree containing `pid`,
/// or `None` if no ancestor within `max_depth` levels has a session file.
///
/// * `pid`          — starting PID (usually the CLI's own PID)
/// * `max_depth`    — max ancestors to walk (8 is plenty — CLI is invoked by shell by Claude)
/// * `sessions_dir` — directory where Claude writes `<pid>.json` files
/// * `parent_of`    — closure that returns the parent PID of a given PID, or `None` at root
pub fn detect_caller_session_id(
    pid: u32,
    max_depth: usize,
    sessions_dir: &Path,
    parent_of: impl Fn(u32) -> Option<u32>,
) -> Option<String> {
    let mut current = pid;
    for _ in 0..max_depth {
        let session_file = sessions_dir.join(format!("{}.json", current));
        if let Ok(content) = std::fs::read_to_string(&session_file) {
            if let Ok(parsed) = serde_json::from_str::<SessionFile>(&content) {
                return Some(parsed.session_id);
            }
        }
        current = parent_of(current)?;
    }
    None
}

/// Production entry point: walks parent PIDs via `ps` and uses `~/.claude/sessions/`.
/// Returns None if home dir cannot be determined or no ancestor has a session file.
pub fn detect_caller_session_id_default() -> Option<String> {
    let home = dirs::home_dir()?;
    let sessions_dir = home.join(".claude").join("sessions");
    let my_pid = std::process::id();
    detect_caller_session_id(my_pid, 8, &sessions_dir, get_parent_pid)
}

/// Returns the OS PID of the Claude Code process in this caller's ancestor
/// chain, or `None` if no ancestor within `max_depth` levels has a session
/// file. Mirrors `detect_caller_session_id` but returns the PID instead of
/// the session UUID.
pub fn detect_caller_pid(
    pid: u32,
    max_depth: usize,
    sessions_dir: &Path,
    parent_of: impl Fn(u32) -> Option<u32>,
) -> Option<u32> {
    let mut current = pid;
    for _ in 0..max_depth {
        let session_file = sessions_dir.join(format!("{}.json", current));
        if session_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&session_file) {
                if serde_json::from_str::<SessionFile>(&content).is_ok() {
                    return Some(current);
                }
            }
        }
        current = parent_of(current)?;
    }
    None
}

/// Production entry point: walks parent PIDs via `ps` and uses `~/.claude/sessions/`.
pub fn detect_caller_pid_default() -> Option<u32> {
    let home = dirs::home_dir()?;
    let sessions_dir = home.join(".claude").join("sessions");
    let my_pid = std::process::id();
    detect_caller_pid(my_pid, 8, &sessions_dir, get_parent_pid)
}

#[cfg(unix)]
fn get_parent_pid(pid: u32) -> Option<u32> {
    use std::process::Command;
    let output = Command::new("ps")
        .arg("-o")
        .arg("ppid=")
        .arg("-p")
        .arg(pid.to_string())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let ppid_str = String::from_utf8(output.stdout).ok()?;
    let ppid: u32 = ppid_str.trim().parse().ok()?;
    if ppid == 0 || ppid == 1 {
        None
    } else {
        Some(ppid)
    }
}

#[cfg(not(unix))]
fn get_parent_pid(_pid: u32) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_tmpdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pm_caller_test_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_returns_session_id_when_current_pid_has_file() {
        let dir = make_tmpdir();
        let pid = 1000u32;
        std::fs::write(
            dir.join("1000.json"),
            r#"{"pid":1000,"sessionId":"abc-123","cwd":"/tmp","startedAt":0,"kind":"interactive","entrypoint":"cli"}"#,
        )
        .unwrap();

        let result = detect_caller_session_id(pid, 4, &dir, |_| None);
        assert_eq!(result, Some("abc-123".to_string()));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_walks_parent_chain_until_session_found() {
        let dir = make_tmpdir();
        // PID 100 has no session file; its parent 200 does.
        std::fs::write(
            dir.join("200.json"),
            r#"{"pid":200,"sessionId":"parent-session","cwd":"/tmp","startedAt":0,"kind":"interactive","entrypoint":"cli"}"#,
        )
        .unwrap();

        let mut parents: HashMap<u32, u32> = HashMap::new();
        parents.insert(100, 200);
        let result = detect_caller_session_id(100, 4, &dir, move |p| parents.get(&p).copied());
        assert_eq!(result, Some("parent-session".to_string()));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_returns_none_when_chain_exhausted() {
        let dir = make_tmpdir();
        // No session files anywhere.
        let mut parents: HashMap<u32, u32> = HashMap::new();
        parents.insert(100, 200);
        parents.insert(200, 300);
        let result = detect_caller_session_id(100, 4, &dir, move |p| parents.get(&p).copied());
        assert_eq!(result, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_returns_none_when_max_depth_exceeded() {
        let dir = make_tmpdir();
        // Session file exists 5 levels up, but max_depth is 2.
        std::fs::write(
            dir.join("500.json"),
            r#"{"pid":500,"sessionId":"far-away","cwd":"/tmp","startedAt":0,"kind":"interactive","entrypoint":"cli"}"#,
        )
        .unwrap();
        let mut parents: HashMap<u32, u32> = HashMap::new();
        parents.insert(100, 200);
        parents.insert(200, 300);
        parents.insert(300, 400);
        parents.insert(400, 500);
        let result = detect_caller_session_id(100, 2, &dir, move |p| parents.get(&p).copied());
        assert_eq!(result, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_detect_caller_pid_returns_current_when_file_present() {
        let dir = make_tmpdir();
        std::fs::write(
            dir.join("1000.json"),
            r#"{"pid":1000,"sessionId":"abc-123","cwd":"/tmp","startedAt":0,"kind":"interactive","entrypoint":"cli"}"#,
        )
        .unwrap();
        let result = detect_caller_pid(1000, 4, &dir, |_| None);
        assert_eq!(result, Some(1000));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_detect_caller_pid_walks_parent_chain() {
        let dir = make_tmpdir();
        std::fs::write(
            dir.join("200.json"),
            r#"{"pid":200,"sessionId":"parent-session","cwd":"/tmp","startedAt":0,"kind":"interactive","entrypoint":"cli"}"#,
        )
        .unwrap();
        let mut parents: HashMap<u32, u32> = HashMap::new();
        parents.insert(100, 200);
        let result = detect_caller_pid(100, 4, &dir, move |p| parents.get(&p).copied());
        assert_eq!(result, Some(200));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_detect_caller_pid_none_when_chain_exhausted() {
        let dir = make_tmpdir();
        let mut parents: HashMap<u32, u32> = HashMap::new();
        parents.insert(100, 200);
        let result = detect_caller_pid(100, 4, &dir, move |p| parents.get(&p).copied());
        assert_eq!(result, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_ignores_corrupt_session_file_and_keeps_walking() {
        let dir = make_tmpdir();
        std::fs::write(dir.join("100.json"), "not valid json").unwrap();
        std::fs::write(
            dir.join("200.json"),
            r#"{"pid":200,"sessionId":"good-one","cwd":"/tmp","startedAt":0,"kind":"interactive","entrypoint":"cli"}"#,
        )
        .unwrap();

        let mut parents: HashMap<u32, u32> = HashMap::new();
        parents.insert(100, 200);
        let result = detect_caller_session_id(100, 4, &dir, move |p| parents.get(&p).copied());
        assert_eq!(result, Some("good-one".to_string()));

        std::fs::remove_dir_all(&dir).ok();
    }
}
