use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ── Path helpers ──────────────────────────────────────────────────────

/// Returns `~/.claude/c9watch/`
pub fn c9watch_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Failed to get home directory")?;
    Ok(home.join(".claude").join("c9watch"))
}

/// Returns `~/.claude/c9watch/daemon.sock`
pub fn daemon_sock_path() -> Result<PathBuf, String> {
    Ok(c9watch_dir()?.join("daemon.sock"))
}

/// Returns `~/.claude/c9watch/daemon.pid`
pub fn daemon_pid_path() -> Result<PathBuf, String> {
    Ok(c9watch_dir()?.join("daemon.pid"))
}

/// Returns `~/.claude/c9watch/daemon.log`
pub fn daemon_log_path() -> Result<PathBuf, String> {
    Ok(c9watch_dir()?.join("daemon.log"))
}

/// Returns `~/.claude/c9watch/workers/<session-id>/`
pub fn worker_dir(session_id: &str) -> Result<PathBuf, String> {
    Ok(c9watch_dir()?.join("workers").join(session_id))
}

/// Returns `~/.claude/c9watch/workers/<session-id>/meta.json`
pub fn worker_meta_path(session_id: &str) -> Result<PathBuf, String> {
    Ok(worker_dir(session_id)?.join("meta.json"))
}

/// Returns `~/.claude/c9watch/workers/<session-id>/stdout.log`
pub fn worker_stdout_log_path(session_id: &str) -> Result<PathBuf, String> {
    Ok(worker_dir(session_id)?.join("stdout.log"))
}

/// Returns `~/.claude/c9watch/workers/<session-id>/stderr.log`
pub fn worker_stderr_log_path(session_id: &str) -> Result<PathBuf, String> {
    Ok(worker_dir(session_id)?.join("stderr.log"))
}

pub fn inbox_dir() -> Result<PathBuf, String> {
    Ok(c9watch_dir()?.join("inbox"))
}

pub fn inbox_pm_dir(pm_session_id: &str) -> Result<PathBuf, String> {
    Ok(inbox_dir()?.join(pm_session_id))
}

// ── Structs ───────────────────────────────────────────────────────────

/// Arguments used to spawn a worker session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpawnArgs {
    /// System prompt text to append for this worker
    pub append_system_prompt: Option<String>,
    /// Permission mode (e.g. "default", "acceptEdits", "bypassPermissions")
    pub permission_mode: String,
    /// Model override (e.g. "claude-opus-4-5")
    pub model: Option<String>,
    /// Additional directories to make available
    pub add_dirs: Vec<String>,
}

/// Metadata written for each worker session spawned by the PM daemon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkerMeta {
    /// UUID of the worker's Claude Code session
    pub session_id: String,
    /// OS process ID of the worker
    pub pid: u32,
    /// Human-readable name for the worker
    pub name: Option<String>,
    /// Working directory the worker was spawned in
    pub cwd: String,
    /// ISO-8601 timestamp when the worker was spawned
    pub spawned_at: String,
    /// Identifier of who/what spawned the worker (e.g. "pm-daemon")
    pub spawned_by: Option<String>,
    /// Arguments used to spawn the worker
    pub spawn_args: SpawnArgs,
    /// ISO-8601 timestamp when the worker stopped, if known
    pub stopped_at: Option<String>,
}

// ── I/O functions ─────────────────────────────────────────────────────

/// Ensure the base c9watch dir and workers dir exist.
pub fn ensure_dirs() -> Result<(), String> {
    let base = c9watch_dir()?;
    std::fs::create_dir_all(&base)
        .map_err(|e| format!("Failed to create c9watch dir {:?}: {}", base, e))?;
    let workers = base.join("workers");
    std::fs::create_dir_all(&workers)
        .map_err(|e| format!("Failed to create workers dir {:?}: {}", workers, e))?;
    let inbox = inbox_dir()?;
    std::fs::create_dir_all(&inbox)
        .map_err(|e| format!("Failed to create inbox dir {:?}: {}", inbox, e))?;
    Ok(())
}

/// Write a `WorkerMeta` atomically (write to `.tmp` then rename).
pub fn write_worker_meta(meta: &WorkerMeta) -> Result<(), String> {
    let dir = worker_dir(&meta.session_id)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create worker dir {:?}: {}", dir, e))?;

    let dest = worker_meta_path(&meta.session_id)?;
    let tmp = dest.with_extension("json.tmp");

    let json = serde_json::to_string_pretty(meta)
        .map_err(|e| format!("Failed to serialize WorkerMeta: {}", e))?;

    std::fs::write(&tmp, &json)
        .map_err(|e| format!("Failed to write tmp meta file {:?}: {}", tmp, e))?;

    std::fs::rename(&tmp, &dest)
        .map_err(|e| format!("Failed to rename {:?} -> {:?}: {}", tmp, dest, e))?;

    Ok(())
}

/// Read a `WorkerMeta` for a given session ID.
pub fn read_worker_meta(session_id: &str) -> Result<WorkerMeta, String> {
    let path = worker_meta_path(session_id)?;
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read worker meta {:?}: {}", path, e))?;
    serde_json::from_str::<WorkerMeta>(&content)
        .map_err(|e| format!("Failed to parse worker meta {:?}: {}", path, e))
}

/// Return all session IDs that have a worker directory under workers/.
pub fn list_worker_ids() -> Result<Vec<String>, String> {
    let workers_dir = c9watch_dir()?.join("workers");

    if !workers_dir.exists() {
        return Ok(vec![]);
    }

    let mut ids: Vec<String> = std::fs::read_dir(&workers_dir)
        .map_err(|e| format!("Failed to read workers dir {:?}: {}", workers_dir, e))?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();

    ids.sort();
    Ok(ids)
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_c9watch_dir_is_under_home() {
        let dir = c9watch_dir().expect("c9watch_dir should succeed");
        let home = dirs::home_dir().expect("home dir should exist");
        assert!(
            dir.starts_with(&home),
            "c9watch_dir {:?} should be under home {:?}",
            dir,
            home
        );
        assert!(dir.ends_with(".claude/c9watch"));
    }

    #[test]
    fn test_daemon_paths_are_under_c9watch_dir() {
        let base = c9watch_dir().unwrap();

        let sock = daemon_sock_path().unwrap();
        assert!(sock.starts_with(&base));
        assert_eq!(sock.file_name().unwrap(), "daemon.sock");

        let pid = daemon_pid_path().unwrap();
        assert!(pid.starts_with(&base));
        assert_eq!(pid.file_name().unwrap(), "daemon.pid");

        let log = daemon_log_path().unwrap();
        assert!(log.starts_with(&base));
        assert_eq!(log.file_name().unwrap(), "daemon.log");
    }

    #[test]
    fn test_worker_dir_structure() {
        let session_id = "test-session-abc123";
        let base = c9watch_dir().unwrap();

        let dir = worker_dir(session_id).unwrap();
        assert!(dir.starts_with(base.join("workers")));
        assert!(dir.ends_with(session_id));

        let meta = worker_meta_path(session_id).unwrap();
        assert!(meta.starts_with(&dir));
        assert_eq!(meta.file_name().unwrap(), "meta.json");

        let stdout = worker_stdout_log_path(session_id).unwrap();
        assert!(stdout.starts_with(&dir));
        assert_eq!(stdout.file_name().unwrap(), "stdout.log");

        let stderr = worker_stderr_log_path(session_id).unwrap();
        assert!(stderr.starts_with(&dir));
        assert_eq!(stderr.file_name().unwrap(), "stderr.log");
    }

    #[test]
    fn test_worker_meta_roundtrip() {
        let session_id = "roundtrip-test-session-001";

        let meta = WorkerMeta {
            session_id: session_id.to_string(),
            pid: 12345,
            name: Some("test-worker".to_string()),
            cwd: "/Users/test/project".to_string(),
            spawned_at: "2026-04-16T00:00:00Z".to_string(),
            spawned_by: Some("pm-daemon".to_string()),
            spawn_args: SpawnArgs {
                append_system_prompt: Some("Be concise.".to_string()),
                permission_mode: "default".to_string(),
                model: Some("claude-opus-4-5".to_string()),
                add_dirs: vec!["/tmp/extra".to_string()],
            },
            stopped_at: None,
        };

        // Serialize and deserialize directly (without touching real ~/.claude/c9watch)
        let json = serde_json::to_string_pretty(&meta).expect("serialize should succeed");
        let parsed: WorkerMeta = serde_json::from_str(&json).expect("deserialize should succeed");

        assert_eq!(meta, parsed);

        // Verify camelCase field names in JSON
        assert!(json.contains("sessionId"));
        assert!(json.contains("spawnedAt"));
        assert!(json.contains("spawnedBy"));
        assert!(json.contains("stoppedAt"));
        assert!(json.contains("appendSystemPrompt"));
        assert!(json.contains("permissionMode"));
        assert!(json.contains("addDirs"));
    }

    #[test]
    fn test_list_worker_ids_empty_when_no_dir() {
        // If the workers directory doesn't exist, list_worker_ids returns empty vec (not error).
        // We test this by checking the function signature handles missing dir gracefully.
        // Since we can't easily redirect home, we verify that a non-existent workers dir
        // results in an empty vec by testing the code path directly.
        let workers_dir = c9watch_dir().unwrap().join("workers");
        if !workers_dir.exists() {
            // The real dir doesn't exist on this machine — list_worker_ids should return Ok([])
            let ids = list_worker_ids().expect("should not error when workers dir missing");
            assert!(ids.is_empty());
        } else {
            // Workers dir exists — just verify we get a Vec<String> back without panicking
            let ids = list_worker_ids().expect("should not error when workers dir exists");
            // All returned IDs should be non-empty strings
            for id in &ids {
                assert!(!id.is_empty());
            }
        }
    }
}

#[cfg(test)]
mod inbox_path_tests {
    use super::*;

    #[test]
    fn inbox_dir_is_child_of_c9watch_dir() {
        let d = inbox_dir().unwrap();
        assert!(d.ends_with("inbox"));
        assert!(d.starts_with(c9watch_dir().unwrap()));
    }

    #[test]
    fn inbox_pm_dir_includes_pm_session_id() {
        let d = inbox_pm_dir("abc-123").unwrap();
        assert!(d.ends_with("abc-123"));
        assert!(d.starts_with(inbox_dir().unwrap()));
    }
}
