//! Inbox: async completion events from workers to their PM session.
//!
//! Events are plain JSON files at `~/.claude/c9watch/inbox/<pm-session-id>/<event-id>.json`.
//! Workers write events (via `stdout_tee_task` in `pm_worker`); PMs read with
//! `c9watch inbox`. No daemon involvement on read — it's pure filesystem.

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
    // ISO-ish sortable prefix + short random suffix so filesystem listings sort FIFO
    // but we display newest-first in `list()`.
    let ts = Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let suffix: String = uuid::Uuid::new_v4().to_string().chars().take(8).collect();
    format!("{}-{}", ts, suffix)
}

fn event_path(pm_session_id: &str, event_id: &str) -> Result<PathBuf, String> {
    Ok(pm_fs::inbox_pm_dir(pm_session_id)?.join(format!("{}.json", event_id)))
}

/// Write an event to disk. Creates the PM's inbox dir if needed.
pub fn write_event(ev: &InboxEvent) -> Result<(), String> {
    let dir = pm_fs::inbox_pm_dir(&ev.spawned_by)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create inbox pm dir {:?}: {}", dir, e))?;
    let path = event_path(&ev.spawned_by, &ev.event_id)?;
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(ev)
        .map_err(|e| format!("Failed to serialize inbox event: {}", e))?;
    std::fs::write(&tmp, json)
        .map_err(|e| format!("Failed to write inbox tmp {:?}: {}", tmp, e))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| format!("Failed to rename {:?} -> {:?}: {}", tmp, path, e))?;
    Ok(())
}

/// List events for a PM, newest first (by filename, which embeds an ISO timestamp).
pub fn list(pm_session_id: &str) -> Result<Vec<InboxEvent>, String> {
    Ok(list_with_paths(pm_session_id)?
        .into_iter()
        .map(|(_, ev)| ev)
        .collect())
}

/// Like `list`, but also returns the on-disk path for each event so callers
/// (e.g. `consume`) can delete exactly the files they observed — avoiding the
/// race where events written between `list()` and a bulk `clear()` are lost.
pub fn list_with_paths(pm_session_id: &str) -> Result<Vec<(PathBuf, InboxEvent)>, String> {
    let dir = pm_fs::inbox_pm_dir(pm_session_id)?;
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

/// Delete all events for a PM. Returns count removed.
pub fn clear(pm_session_id: &str) -> Result<usize, String> {
    let dir = pm_fs::inbox_pm_dir(pm_session_id)?;
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
pub fn consume(pm_session_id: &str) -> Result<Vec<InboxEvent>, String> {
    let listed = list_with_paths(pm_session_id)?;
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

    /// Use a unique PM id per test so tests don't interact via the real
    /// `$HOME/.claude/c9watch/inbox/` dir. Cleans up after itself.
    struct TestPm(String);

    impl TestPm {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let id = format!(
                "test-pm-{}-{}",
                std::process::id(),
                n
            );
            Self(id)
        }
        fn id(&self) -> &str { &self.0 }
    }

    impl Drop for TestPm {
        fn drop(&mut self) {
            let _ = clear(&self.0);
            if let Ok(dir) = pm_fs::inbox_pm_dir(&self.0) {
                let _ = std::fs::remove_dir(&dir);
            }
        }
    }

    fn make_event(pm: &str, session: &str, status: EventStatus) -> InboxEvent {
        InboxEvent {
            event_id: new_event_id(),
            session_id: session.to_string(),
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
        let pm = TestPm::new();
        let ev = make_event(pm.id(), "worker-1", EventStatus::Done);
        write_event(&ev).unwrap();
        let listed = list(pm.id()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], ev);
    }

    #[test]
    fn list_returns_newest_first() {
        let pm = TestPm::new();
        let e1 = make_event(pm.id(), "worker-1", EventStatus::Done);
        write_event(&e1).unwrap();
        // Sleep enough for the millisecond-resolution timestamp in the event_id
        // to advance.
        std::thread::sleep(std::time::Duration::from_millis(5));
        let e2 = make_event(pm.id(), "worker-2", EventStatus::Done);
        write_event(&e2).unwrap();
        let listed = list(pm.id()).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].session_id, "worker-2", "newest should come first");
        assert_eq!(listed[1].session_id, "worker-1");
    }

    #[test]
    fn consume_returns_and_removes() {
        let pm = TestPm::new();
        let ev = make_event(pm.id(), "worker-1", EventStatus::Done);
        write_event(&ev).unwrap();
        let consumed = consume(pm.id()).unwrap();
        assert_eq!(consumed.len(), 1);
        assert_eq!(list(pm.id()).unwrap().len(), 0);
    }

    #[test]
    fn consume_preserves_events_written_after_list() {
        // Regression test for the list-then-clear race: simulate a consume where
        // the caller has observed 3 events via `list_with_paths`, then a 4th event
        // arrives before the deletes happen. The 4th must NOT be wiped.
        let pm = TestPm::new();
        let e1 = make_event(pm.id(), "w1", EventStatus::Done);
        let e2 = make_event(pm.id(), "w2", EventStatus::Done);
        let e3 = make_event(pm.id(), "w3", EventStatus::Done);
        write_event(&e1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        write_event(&e2).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        write_event(&e3).unwrap();

        // Take a snapshot like consume() does internally.
        let listed = list_with_paths(pm.id()).unwrap();
        assert_eq!(listed.len(), 3);

        // A 4th event arrives after the snapshot but before the deletes happen.
        let e4 = make_event(pm.id(), "w4", EventStatus::Done);
        write_event(&e4).unwrap();

        // Mimic consume: only delete files we observed in the listing.
        for (path, _) in &listed {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => panic!("unexpected error removing {:?}: {}", path, e),
            }
        }

        // e4 must still be on disk.
        let remaining = list(pm.id()).unwrap();
        assert_eq!(remaining.len(), 1, "e4 should survive consume of e1..e3");
        assert_eq!(remaining[0].session_id, "w4");
    }

    #[test]
    fn clear_removes_all() {
        let pm = TestPm::new();
        write_event(&make_event(pm.id(), "w1", EventStatus::Done)).unwrap();
        write_event(&make_event(pm.id(), "w2", EventStatus::Error)).unwrap();
        assert_eq!(clear(pm.id()).unwrap(), 2);
        assert_eq!(list(pm.id()).unwrap().len(), 0);
    }

    #[test]
    fn list_on_nonexistent_pm_returns_empty() {
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
