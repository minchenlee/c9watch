//! Inbox: async completion events from workers to their PM session.
//!
//! Events are plain JSON files at `~/.claude/c9watch/inbox/<worker-session-id>/<event-id>.json`.
//! Keying by worker (stable) instead of PM (unstable across `/clear` and CC
//! restarts) means old events survive PM identity change for free. Ownership
//! is resolved by the daemon via `meta.pm_pid` + the adoption sidecar.

use crate::cli::pm_fs;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InboxEvent {
    pub event_id: String,
    pub session_id: String,
    pub spawned_by: String,
    pub status: EventStatus,
    pub finished_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_turns: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EventStatus {
    Done,
    Error,
    Crashed,
}

/// Fields that vary between normal turn-end outcomes (success or error)
/// reported by Claude's terminal `result` event.
pub struct TurnResult {
    pub status: EventStatus,
    pub duration_ms: Option<u64>,
    pub num_turns: Option<u64>,
    pub stop_reason: Option<String>,
    pub total_cost_usd: Option<f64>,
    pub result_excerpt: Option<String>,
    pub error_message: Option<String>,
}

impl InboxEvent {
    /// Normal turn-end event from a `result` stream entry.
    pub fn from_turn_result(
        worker_session_id: &str,
        spawned_by: &str,
        tr: TurnResult,
    ) -> Self {
        Self {
            event_id: new_event_id(),
            session_id: worker_session_id.to_string(),
            spawned_by: spawned_by.to_string(),
            status: tr.status,
            finished_at: Utc::now().to_rfc3339(),
            duration_ms: tr.duration_ms,
            num_turns: tr.num_turns,
            stop_reason: tr.stop_reason,
            total_cost_usd: tr.total_cost_usd,
            result_excerpt: tr.result_excerpt,
            error_message: tr.error_message,
        }
    }

    /// Worker stdout closed without a terminal `result` event.
    pub fn crashed(worker_session_id: &str, spawned_by: &str, error_message: String) -> Self {
        Self {
            event_id: new_event_id(),
            session_id: worker_session_id.to_string(),
            spawned_by: spawned_by.to_string(),
            status: EventStatus::Crashed,
            finished_at: Utc::now().to_rfc3339(),
            duration_ms: None,
            num_turns: None,
            stop_reason: None,
            total_cost_usd: None,
            result_excerpt: None,
            error_message: Some(error_message),
        }
    }
}

/// Max characters for `result_excerpt` to keep inbox reads cheap.
pub const EXCERPT_LIMIT: usize = 500;

pub fn truncate_excerpt(s: &str) -> String {
    if s.chars().count() <= EXCERPT_LIMIT {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(EXCERPT_LIMIT).collect();
        out.push('…');
        out
    }
}

pub fn new_event_id() -> String {
    // ISO-ish sortable prefix + full UUID suffix so filesystem listings sort FIFO
    // but we display newest-first in `list()`. Full UUID eliminates birthday
    // collisions that the previous 8-char (32-bit) truncation risked at scale (H3).
    let ts = Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("{}-{}", ts, suffix)
}

fn event_path(worker_session_id: &str, event_id: &str) -> Result<PathBuf, String> {
    Ok(pm_fs::inbox_worker_dir(worker_session_id)?.join(format!("{}.json", event_id)))
}

/// Write an event to disk. Creates the worker's inbox dir if needed.
pub fn write_event(ev: &InboxEvent) -> Result<(), String> {
    pm_fs::validate_session_id(&ev.spawned_by)?;
    pm_fs::validate_session_id(&ev.session_id)?;
    let dir = pm_fs::inbox_worker_dir(&ev.session_id)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create inbox worker dir {:?}: {}", dir, e))?;
    let path = event_path(&ev.session_id, &ev.event_id)?;
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(ev)
        .map_err(|e| format!("Failed to serialize inbox event: {}", e))?;
    std::fs::write(&tmp, json)
        .map_err(|e| format!("Failed to write inbox tmp {:?}: {}", tmp, e))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| format!("Failed to rename {:?} -> {:?}: {}", tmp, path, e))?;
    Ok(())
}

/// List events for a worker, newest first (by filename, which embeds an ISO timestamp).
pub fn list(worker_session_id: &str) -> Result<Vec<InboxEvent>, String> {
    Ok(list_with_paths(worker_session_id)?
        .into_iter()
        .map(|(_, ev)| ev)
        .collect())
}

/// Like `list`, but also returns the on-disk path for each event so callers
/// (e.g. `consume`) can delete exactly the files they observed — avoiding the
/// race where events written between `list()` and a bulk `clear()` are lost.
pub fn list_with_paths(worker_session_id: &str) -> Result<Vec<(PathBuf, InboxEvent)>, String> {
    let dir = pm_fs::inbox_worker_dir(worker_session_id)?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("Failed to read inbox dir {:?}: {}", dir, e))?
        .filter_map(|e| e.ok().map(|de| de.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    // Filename starts with an ISO-ish timestamp, so reverse-lex sort ≈ newest first.
    paths.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let txt = std::fs::read_to_string(&p)
            .map_err(|e| format!("Failed to read inbox event {:?}: {}", p, e))?;
        let ev: InboxEvent = serde_json::from_str(&txt)
            .map_err(|e| format!("Failed to parse inbox event {:?}: {}", p, e))?;
        out.push((p, ev));
    }
    Ok(out)
}

/// Delete all events for a worker. Returns count removed.
pub fn clear(worker_session_id: &str) -> Result<usize, String> {
    let dir = pm_fs::inbox_worker_dir(worker_session_id)?;
    if !dir.exists() {
        return Ok(0);
    }
    let entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("Failed to read inbox dir {:?}: {}", dir, e))?
        .filter_map(|e| e.ok().map(|de| de.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    let mut removed = 0usize;
    for p in entries {
        if std::fs::remove_file(&p).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// List + remove in one call. Returns the events that were just removed.
///
/// Only deletes the files observed by the initial `list_with_paths` call —
/// events written between the listing and the deletes are left untouched so
/// callers don't silently lose them.
pub fn consume(worker_session_id: &str) -> Result<Vec<InboxEvent>, String> {
    let listed = list_with_paths(worker_session_id)?;
    let mut events = Vec::with_capacity(listed.len());
    for (path, ev) in listed {
        match std::fs::remove_file(&path) {
            Ok(()) => events.push(ev),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Another consumer beat us to this file; skip it.
            }
            Err(e) => {
                return Err(format!("Failed to remove inbox event {:?}: {}", path, e));
            }
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// Use a unique worker id per test so tests don't interact via the real
    /// `$HOME/.claude/c9watch/inbox/` dir. Cleans up after itself.
    struct TestWorker(String);

    impl TestWorker {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let id = format!(
                "test-w-{}-{}",
                std::process::id(),
                n
            );
            Self(id)
        }
        fn id(&self) -> &str { &self.0 }
    }

    impl Drop for TestWorker {
        fn drop(&mut self) {
            let _ = clear(&self.0);
            if let Ok(dir) = pm_fs::inbox_worker_dir(&self.0) {
                let _ = std::fs::remove_dir(&dir);
            }
        }
    }

    fn make_event(pm: &str, worker: &str, status: EventStatus) -> InboxEvent {
        InboxEvent {
            event_id: new_event_id(),
            session_id: worker.to_string(),
            spawned_by: pm.to_string(),
            status,
            finished_at: Utc::now().to_rfc3339(),
            duration_ms: Some(1234),
            num_turns: Some(1),
            stop_reason: Some("end_turn".to_string()),
            total_cost_usd: Some(0.01),
            result_excerpt: Some("ok".to_string()),
            error_message: None,
        }
    }

    #[test]
    fn write_then_list_returns_the_event() {
        let w = TestWorker::new();
        let ev = make_event("any-pm-ok", w.id(), EventStatus::Done);
        write_event(&ev).unwrap();
        let listed = list(w.id()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], ev);
    }

    #[test]
    fn list_returns_newest_first() {
        let w = TestWorker::new();
        let e1 = make_event("pm-1", w.id(), EventStatus::Done);
        write_event(&e1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let e2 = make_event("pm-2", w.id(), EventStatus::Done);
        write_event(&e2).unwrap();
        let listed = list(w.id()).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].spawned_by, "pm-2", "newest should come first");
        assert_eq!(listed[1].spawned_by, "pm-1");
    }

    #[test]
    fn consume_returns_and_removes() {
        let w = TestWorker::new();
        let ev = make_event("pm-x", w.id(), EventStatus::Done);
        write_event(&ev).unwrap();
        let consumed = consume(w.id()).unwrap();
        assert_eq!(consumed.len(), 1);
        assert_eq!(list(w.id()).unwrap().len(), 0);
    }

    #[test]
    fn consume_preserves_events_written_after_list() {
        // Regression test for the list-then-clear race.
        let w = TestWorker::new();
        let e1 = make_event("pm-a", w.id(), EventStatus::Done);
        let e2 = make_event("pm-b", w.id(), EventStatus::Done);
        let e3 = make_event("pm-c", w.id(), EventStatus::Done);
        write_event(&e1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        write_event(&e2).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        write_event(&e3).unwrap();

        let listed = list_with_paths(w.id()).unwrap();
        assert_eq!(listed.len(), 3);

        let e4 = make_event("pm-d", w.id(), EventStatus::Done);
        write_event(&e4).unwrap();

        for (path, _) in &listed {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => panic!("unexpected error removing {:?}: {}", path, e),
            }
        }

        let remaining = list(w.id()).unwrap();
        assert_eq!(remaining.len(), 1, "e4 should survive consume of e1..e3");
        assert_eq!(remaining[0].spawned_by, "pm-d");
    }

    #[test]
    fn clear_removes_all() {
        let w = TestWorker::new();
        write_event(&make_event("pm-1", w.id(), EventStatus::Done)).unwrap();
        write_event(&make_event("pm-2", w.id(), EventStatus::Error)).unwrap();
        assert_eq!(clear(w.id()).unwrap(), 2);
        assert_eq!(list(w.id()).unwrap().len(), 0);
    }

    #[test]
    fn list_on_nonexistent_worker_returns_empty() {
        let listed = list("definitely-does-not-exist-zzz").unwrap();
        assert!(listed.is_empty());
    }

    #[test]
    fn truncate_excerpt_leaves_short_strings_alone() {
        assert_eq!(truncate_excerpt("hello"), "hello");
    }

    #[test]
    fn truncate_excerpt_caps_long_strings_with_ellipsis() {
        let s = "a".repeat(EXCERPT_LIMIT + 100);
        let t = truncate_excerpt(&s);
        let char_count = t.chars().count();
        assert_eq!(char_count, EXCERPT_LIMIT + 1); // +1 for the ellipsis
        assert!(t.ends_with('…'));
    }
}
