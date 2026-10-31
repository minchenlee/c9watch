//! The c9watch workers overlay: `worker_of` on a Claude Code session that a
//! PM spawned, read from `~/.claude/c9watch/workers/*/meta.json`. Detection
//! does not set it; the GUI poll and the engine's session views add it after
//! enrichment.

use crate::session::{pid_is_alive, Session, SessionProvider};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

/// Cached overlay: session_id -> spawnedBy (i.e. worker_of).
/// Invalidated when the workers dir mtime (or the dir itself) changes.
static WORKERS_OVERLAY: LazyLock<Mutex<Option<WorkersCache>>> = LazyLock::new(|| Mutex::new(None));

struct WorkersCache {
    dir: PathBuf,
    dir_mtime: SystemTime,
    map: Arc<HashMap<String, String>>,
}

/// Narrow view of worker meta.json — only the fields the overlay needs.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerMetaOverlay {
    session_id: Option<String>,
    spawned_by: Option<String>,
    stopped_at: Option<String>,
    pid: Option<u64>,
}

/// `~/.claude/c9watch/workers`.
pub fn workers_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("c9watch").join("workers"))
}

/// The live workers under `workers_dir`: session id -> the PM that spawned
/// it. An unchanged dir mtime returns the cached map.
pub fn load_workers_overlay_at(workers_dir: &Path) -> Arc<HashMap<String, String>> {
    let dir_mtime = match std::fs::metadata(workers_dir).and_then(|m| m.modified()) {
        Ok(m) => m,
        Err(_) => return Arc::new(HashMap::new()),
    };

    if let Ok(guard) = WORKERS_OVERLAY.lock() {
        if let Some(cache) = guard.as_ref() {
            if cache.dir == workers_dir && cache.dir_mtime == dir_mtime {
                return Arc::clone(&cache.map);
            }
        }
    }

    let mut map = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(workers_dir) {
        for entry in entries.flatten() {
            let meta_path = entry.path().join("meta.json");
            let Ok(content) = std::fs::read_to_string(&meta_path) else {
                continue;
            };
            let Ok(meta) = serde_json::from_str::<WorkerMetaOverlay>(&content) else {
                continue;
            };
            if meta.stopped_at.is_some() {
                continue;
            }
            if let Some(pid) = meta.pid {
                if !pid_is_alive(pid as u32) {
                    continue;
                }
            }
            if let (Some(sid), Some(by)) = (meta.session_id, meta.spawned_by) {
                map.insert(sid, by);
            }
        }
    }

    let map = Arc::new(map);
    if let Ok(mut guard) = WORKERS_OVERLAY.lock() {
        *guard = Some(WorkersCache {
            dir: workers_dir.to_path_buf(),
            dir_mtime,
            map: Arc::clone(&map),
        });
    }
    map
}

/// Set `worker_of` on the Claude Code sessions in `overlay`.
pub fn apply_workers_overlay_from(sessions: &mut [Session], overlay: &HashMap<String, String>) {
    for s in sessions.iter_mut() {
        if s.provider == SessionProvider::ClaudeCode {
            if let Some(by) = overlay.get(&s.id) {
                s.worker_of = Some(by.clone());
            }
        }
    }
}

/// Set `worker_of` from `~/.claude/c9watch/workers`.
pub fn apply_workers_overlay(sessions: &mut [Session]) {
    let Some(dir) = workers_dir() else {
        return;
    };
    let overlay = load_workers_overlay_at(&dir);
    apply_workers_overlay_from(sessions, &overlay);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionStatus;

    #[test]
    fn a_live_worker_marks_its_claude_session_and_a_stopped_one_does_not() {
        let t = tempfile::tempdir().unwrap();
        let w = t.path().join("workers");
        for (name, meta) in [
            (
                "a",
                serde_json::json!({"sessionId": "s-live", "spawnedBy": "pm-1",
                    "pid": std::process::id()}),
            ),
            (
                "b",
                serde_json::json!({"sessionId": "s-stopped", "spawnedBy": "pm-1",
                    "stoppedAt": "2026-01-01T00:00:00Z"}),
            ),
        ] {
            std::fs::create_dir_all(w.join(name)).unwrap();
            std::fs::write(w.join(name).join("meta.json"), meta.to_string()).unwrap();
        }
        let overlay = load_workers_overlay_at(&w);
        let mut sessions = vec![
            Session::fixture("s-live", SessionStatus::Working),
            Session::fixture("s-stopped", SessionStatus::Working),
        ];
        apply_workers_overlay_from(&mut sessions, &overlay);
        assert_eq!(sessions[0].worker_of.as_deref(), Some("pm-1"));
        assert_eq!(sessions[1].worker_of, None);
    }
}
