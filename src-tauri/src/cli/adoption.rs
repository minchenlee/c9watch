//! PM adoption sidecar: records workers a PM has explicitly adopted after
//! its Claude Code process restarted. Primary ownership is PID-based
//! (`WorkerMeta.pm_pid`); this sidecar covers the restart case.
//!
//! Layout: `~/.claude/c9watch/adoptions/<pm-session-id>.json`
//!
//! Lazy GC on read: entries whose `workers/<id>/meta.json` no longer exists
//! are dropped. Empty files are deleted.

use crate::cli::pm_fs;
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AdoptionRecord {
    pub pm_session_id: String,
    pub adopted_at: String,
    pub worker_ids: Vec<String>,
}

/// Read the adoption record for a PM, applying lazy GC: worker ids whose
/// `workers/<id>/meta.json` no longer exists are removed from the returned
/// record and from disk. If the resulting list is empty, the file is deleted
/// and `Ok(None)` is returned.
pub fn read_filter(pm_session_id: &str) -> Result<Option<AdoptionRecord>, String> {
    pm_fs::validate_session_id(pm_session_id)?;
    let path = pm_fs::adoption_file(pm_session_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => return Err(format!("Failed to read adoption file {:?}: {}", path, e)),
    };
    let record: AdoptionRecord = match serde_json::from_str(&content) {
        Ok(r) => r,
        Err(_) => {
            eprintln!("[adoption] corrupt sidecar {:?} — treating as empty", path);
            return Ok(None);
        }
    };

    let live: Vec<String> = record
        .worker_ids
        .iter()
        .filter(|wid| {
            pm_fs::worker_meta_path(wid)
                .map(|p| p.exists())
                .unwrap_or(false)
        })
        .cloned()
        .collect();

    if live.len() == record.worker_ids.len() {
        return Ok(Some(record));
    }

    if live.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }

    let filtered = AdoptionRecord {
        pm_session_id: record.pm_session_id.clone(),
        adopted_at: record.adopted_at.clone(),
        worker_ids: live,
    };
    write(&filtered)?;
    Ok(Some(filtered))
}

/// Append `worker_id` to `pm_session_id`'s adoption sidecar. Idempotent: if
/// the id is already listed, no-op.
pub fn add(pm_session_id: &str, worker_id: &str) -> Result<(), String> {
    pm_fs::validate_session_id(pm_session_id)?;
    pm_fs::validate_session_id(worker_id)?;
    let existing = read_filter(pm_session_id)?;
    let record = match existing {
        Some(mut r) => {
            if !r.worker_ids.iter().any(|w| w == worker_id) {
                r.worker_ids.push(worker_id.to_string());
            }
            r
        }
        None => AdoptionRecord {
            pm_session_id: pm_session_id.to_string(),
            adopted_at: Utc::now().to_rfc3339(),
            worker_ids: vec![worker_id.to_string()],
        },
    };
    write(&record)
}

fn write(record: &AdoptionRecord) -> Result<(), String> {
    pm_fs::ensure_dirs()?;
    let path = pm_fs::adoption_file(&record.pm_session_id)?;
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(record)
        .map_err(|e| format!("Failed to serialize adoption record: {}", e))?;
    std::fs::write(&tmp, json)
        .map_err(|e| format!("Failed to write adoption tmp {:?}: {}", tmp, e))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| format!("Failed to rename {:?} -> {:?}: {}", tmp, path, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TestPm(String);

    impl TestPm {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            Self(format!("test-adopt-pm-{}-{}", std::process::id(), n))
        }
        fn id(&self) -> &str {
            &self.0
        }
    }

    impl Drop for TestPm {
        fn drop(&mut self) {
            if let Ok(p) = pm_fs::adoption_file(&self.0) {
                let _ = std::fs::remove_file(&p);
            }
        }
    }

    fn make_worker_meta(wid: &str) -> Result<(), String> {
        let meta = pm_fs::WorkerMeta {
            session_id: wid.to_string(),
            pid: 1,
            name: None,
            cwd: "/tmp".to_string(),
            spawned_at: "2026-04-18T00:00:00Z".to_string(),
            spawned_by: None,
            pm_pid: None,
            spawn_args: pm_fs::PersistedSpawnArgs {
                append_system_prompt: None,
                permission_mode: "default".to_string(),
                model: None,
                add_dirs: vec![],
            },
            stopped_at: None,
        };
        pm_fs::write_worker_meta(&meta)
    }

    fn cleanup_worker(wid: &str) {
        if let Ok(dir) = pm_fs::worker_dir(wid) {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn add_creates_record_with_one_worker() {
        let pm = TestPm::new();
        let wid = format!("w-add-{}-1", std::process::id());
        make_worker_meta(&wid).unwrap();
        add(pm.id(), &wid).unwrap();
        let r = read_filter(pm.id()).unwrap().unwrap();
        assert_eq!(r.worker_ids, vec![wid.clone()]);
        cleanup_worker(&wid);
    }

    #[test]
    fn add_is_idempotent() {
        let pm = TestPm::new();
        let wid = format!("w-idem-{}-2", std::process::id());
        make_worker_meta(&wid).unwrap();
        add(pm.id(), &wid).unwrap();
        add(pm.id(), &wid).unwrap();
        let r = read_filter(pm.id()).unwrap().unwrap();
        assert_eq!(r.worker_ids.len(), 1);
        cleanup_worker(&wid);
    }

    #[test]
    fn read_filter_drops_workers_whose_meta_is_gone() {
        let pm = TestPm::new();
        let w_live = format!("w-live-{}-3", std::process::id());
        let w_gone = format!("w-gone-{}-4", std::process::id());
        make_worker_meta(&w_live).unwrap();
        make_worker_meta(&w_gone).unwrap();
        add(pm.id(), &w_live).unwrap();
        add(pm.id(), &w_gone).unwrap();
        cleanup_worker(&w_gone);
        let r = read_filter(pm.id()).unwrap().unwrap();
        assert_eq!(r.worker_ids, vec![w_live.clone()]);
        cleanup_worker(&w_live);
    }

    #[test]
    fn read_filter_deletes_file_when_empty() {
        let pm = TestPm::new();
        let wid = format!("w-empty-{}-5", std::process::id());
        make_worker_meta(&wid).unwrap();
        add(pm.id(), &wid).unwrap();
        cleanup_worker(&wid);
        let r = read_filter(pm.id()).unwrap();
        assert!(r.is_none());
        let path = pm_fs::adoption_file(pm.id()).unwrap();
        assert!(!path.exists(), "empty sidecar should be deleted");
    }

    #[test]
    fn read_filter_returns_none_for_missing_file() {
        let pm = TestPm::new();
        assert!(read_filter(pm.id()).unwrap().is_none());
    }

    #[test]
    fn read_filter_treats_corrupt_as_none() {
        let pm = TestPm::new();
        pm_fs::ensure_dirs().unwrap();
        let path = pm_fs::adoption_file(pm.id()).unwrap();
        std::fs::write(&path, "not valid json").unwrap();
        assert!(read_filter(pm.id()).unwrap().is_none());
        let _ = std::fs::remove_file(&path);
    }
}
