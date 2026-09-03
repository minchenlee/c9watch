//! Process-wide owners for provider-specific transcript sources.
//!
//! A source contains incremental parsing state, so creating one per caller
//! silently turns every poll/request into a cold parse.  The GUI detector and
//! the CLI/Web enrichment path therefore share these owners.  Provider
//! discovery and parsing remain separate; only the lifecycle of the mutable
//! source is shared.

use super::codex::CodexSessionSource;
use super::cursor::CursorSessionSource;
use super::pi::PiSessionSource;
use super::source::{DetectedSession, DetectionDiagnostics, SessionSource};
use std::sync::{Arc, LazyLock, Mutex};

pub(crate) type SharedCodexSource = Arc<Mutex<Option<CodexSessionSource>>>;
pub(crate) type SharedCursorSource = Arc<Mutex<Option<CursorSessionSource>>>;
pub(crate) type SharedPiSource = Arc<Mutex<Option<PiSessionSource>>>;

#[derive(Clone)]
pub(crate) struct ProviderSourceOwners {
    pub(crate) codex: SharedCodexSource,
    pub(crate) cursor: SharedCursorSource,
    pub(crate) pi: SharedPiSource,
    initialize_defaults: bool,
}

impl ProviderSourceOwners {
    fn new() -> Self {
        Self {
            codex: Arc::new(Mutex::new(None)),
            cursor: Arc::new(Mutex::new(None)),
            pi: Arc::new(Mutex::new(None)),
            initialize_defaults: true,
        }
    }

    pub(crate) fn detect_codex(&self) -> Option<(Vec<DetectedSession>, DetectionDiagnostics)> {
        let mut guard = recover_lock(&self.codex);
        if guard.is_none() {
            if !self.initialize_defaults {
                return None;
            }
            *guard = CodexSessionSource::new().ok();
        }
        guard.as_mut()?.detect().ok()
    }

    pub(crate) fn detect_cursor(&self) -> Option<(Vec<DetectedSession>, DetectionDiagnostics)> {
        let mut guard = recover_lock(&self.cursor);
        if guard.is_none() {
            if !self.initialize_defaults {
                return None;
            }
            *guard = CursorSessionSource::new().ok();
        }
        guard.as_mut()?.detect().ok()
    }

    pub(crate) fn detect_pi(&self) -> Option<(Vec<DetectedSession>, DetectionDiagnostics)> {
        let mut guard = recover_lock(&self.pi);
        if guard.is_none() {
            if !self.initialize_defaults {
                return None;
            }
            *guard = PiSessionSource::new().ok();
        }
        guard.as_mut()?.detect().ok()
    }

    pub(crate) fn has_non_claude_session(&self, session_id: &str) -> bool {
        if self.has_pi_session(session_id) {
            return true;
        }
        let codex = {
            let mut guard = recover_lock(&self.codex);
            if guard.is_none() && self.initialize_defaults {
                *guard = CodexSessionSource::new().ok();
            }
            guard
                .as_ref()
                .is_some_and(|source| source.contains_session_id(session_id))
        };
        if codex {
            return true;
        }

        let mut guard = recover_lock(&self.cursor);
        if guard.is_none() && self.initialize_defaults {
            *guard = CursorSessionSource::new().ok();
        }
        guard
            .as_ref()
            .is_some_and(|source| source.contains_session_id(session_id))
    }

    fn has_pi_session(&self, session_id: &str) -> bool {
        let mut guard = recover_lock(&self.pi);
        if guard.is_none() && self.initialize_defaults {
            *guard = PiSessionSource::new().ok();
        }
        guard
            .as_ref()
            .is_some_and(|source| source.contains_session_id(session_id))
    }

    #[cfg(test)]
    pub(crate) fn from_test_sources(
        codex: Option<CodexSessionSource>,
        cursor: Option<CursorSessionSource>,
        pi: Option<PiSessionSource>,
    ) -> Self {
        Self {
            codex: Arc::new(Mutex::new(codex)),
            cursor: Arc::new(Mutex::new(cursor)),
            pi: Arc::new(Mutex::new(pi)),
            initialize_defaults: false,
        }
    }
}

static GLOBAL_PROVIDER_SOURCE_OWNERS: LazyLock<ProviderSourceOwners> =
    LazyLock::new(ProviderSourceOwners::new);

pub(crate) fn global_provider_source_owners() -> ProviderSourceOwners {
    GLOBAL_PROVIDER_SOURCE_OWNERS.clone()
}

fn recover_lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn cloned_owners_share_each_provider_source() {
        let owners = ProviderSourceOwners::new();
        let clone = owners.clone();

        assert!(Arc::ptr_eq(&owners.codex, &clone.codex));
        assert!(Arc::ptr_eq(&owners.cursor, &clone.cursor));
        assert!(Arc::ptr_eq(&owners.pi, &clone.pi));
    }

    #[test]
    fn global_owner_clones_share_each_provider_source() {
        let detector_owner = global_provider_source_owners();
        let enrichment_owner = global_provider_source_owners();

        assert!(Arc::ptr_eq(&detector_owner.codex, &enrichment_owner.codex));
        assert!(Arc::ptr_eq(
            &detector_owner.cursor,
            &enrichment_owner.cursor
        ));
        assert!(Arc::ptr_eq(&detector_owner.pi, &enrichment_owner.pi));
    }

    #[test]
    fn independent_test_owners_do_not_share_state() {
        let left = ProviderSourceOwners::new();
        let right = ProviderSourceOwners::new();

        assert!(!Arc::ptr_eq(&left.codex, &right.codex));
        assert!(!Arc::ptr_eq(&left.cursor, &right.cursor));
        assert!(!Arc::ptr_eq(&left.pi, &right.pi));
    }

    #[test]
    fn test_owners_without_sources_do_not_initialize_production_roots() {
        let owners = ProviderSourceOwners::from_test_sources(None, None, None);

        assert!(owners.detect_codex().is_none());
        assert!(owners.detect_cursor().is_none());
        assert!(owners.detect_pi().is_none());
        assert!(owners.codex.lock().unwrap().is_none());
        assert!(owners.cursor.lock().unwrap().is_none());
        assert!(owners.pi.lock().unwrap().is_none());
    }

    #[test]
    fn shared_cursor_owner_detects_a_synthetic_fixture() {
        const SESSION_ID: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let temp = tempfile::tempdir().unwrap();
        let transcript_dir = temp
            .path()
            .join("demo-project")
            .join("agent-transcripts")
            .join(SESSION_ID);
        std::fs::create_dir_all(&transcript_dir).unwrap();
        std::fs::write(
            transcript_dir.join(format!("{SESSION_ID}.jsonl")),
            r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>shared owner fixture</user_query>"}]}}
"#,
        )
        .unwrap();

        let owners = ProviderSourceOwners::from_test_sources(
            None,
            Some(CursorSessionSource::at_root(temp.path().to_path_buf())),
            None,
        );
        let first = owners.detect_cursor().unwrap().0;
        let parses_after_first = owners
            .cursor
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .parse_count_for_test();
        let second = owners.clone().detect_cursor().unwrap().0;
        let parses_after_second = owners
            .cursor
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .parse_count_for_test();

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(parses_after_first, parses_after_second);
        assert_eq!(first[0].session_id, Some(SESSION_ID.to_string()));
        assert_eq!(second[0].session_id, Some(SESSION_ID.to_string()));
        assert!(owners.has_non_claude_session(SESSION_ID));
        assert!(!owners.has_non_claude_session("missing-cursor-session"));
    }

    #[test]
    fn shared_codex_owner_detects_a_synthetic_fixture() {
        const SESSION_ID: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let temp = tempfile::tempdir().unwrap();
        let now = chrono::Local::now();
        let day = temp.path().join(now.format("%Y/%m/%d").to_string());
        std::fs::create_dir_all(&day).unwrap();
        let path = day.join(format!("rollout-synthetic-{SESSION_ID}.jsonl"));
        let event = |event_type: &str, payload: serde_json::Value| {
            serde_json::json!({
                "timestamp": now.to_rfc3339(),
                "type": event_type,
                "payload": payload,
            })
            .to_string()
        };
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                event(
                    "session_meta",
                    serde_json::json!({
                        "id": SESSION_ID,
                        "cwd": "/tmp/synthetic-codex",
                        "source": "cli",
                        "originator": "codex-tui"
                    })
                ),
                event("event_msg", serde_json::json!({"type": "task_started"}))
            ),
        )
        .unwrap();

        let owners = ProviderSourceOwners::from_test_sources(
            Some(CodexSessionSource::at_root(temp.path().to_path_buf())),
            None,
            None,
        );
        let first = owners.detect_codex().unwrap().0;
        let parses_after_first = owners
            .codex
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .parse_count_for_test();
        let second = owners.clone().detect_codex().unwrap().0;
        let parses_after_second = owners
            .codex
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .parse_count_for_test();

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(parses_after_first, parses_after_second);
        assert_eq!(first[0].session_id, Some(SESSION_ID.to_string()));
        assert_eq!(second[0].session_id, Some(SESSION_ID.to_string()));
        assert!(owners.has_non_claude_session(SESSION_ID));
        assert!(!owners.has_non_claude_session("missing-codex-session"));
    }

    #[test]
    fn shared_pi_owner_detects_a_synthetic_fixture() {
        const SESSION_ID: &str = "cccccccc-cccc-cccc-cccc-cccccccccccc";
        let temp = tempfile::tempdir().unwrap();
        let cwd_dir = temp.path().join("encoded-cwd");
        std::fs::create_dir_all(&cwd_dir).unwrap();
        std::fs::write(
            cwd_dir.join(format!("2026-09-03T00-00-00-000Z_{SESSION_ID}.jsonl")),
            format!(
                concat!(
                    r#"{{"type":"session","session":{{"id":"{id}","timestamp":1756857600000,"cwd":"/tmp/synthetic-pi"}}}}"#,
                    "\n",
                    r#"{{"type":"message","message":{{"role":"user","content":[{{"type":"text","text":"shared owner fixture"}}]}}}}"#,
                    "\n",
                ),
                id = SESSION_ID,
            ),
        )
        .unwrap();

        let owners = ProviderSourceOwners::from_test_sources(
            None,
            None,
            Some(crate::session::pi::PiSessionSource::at_root(
                temp.path().to_path_buf(),
            )),
        );
        let sessions = owners.detect_pi().unwrap().0;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, Some(SESSION_ID.to_string()));
        assert_eq!(
            sessions[0].provider,
            crate::session::SessionProvider::Pi
        );
        assert!(owners.has_non_claude_session(SESSION_ID));
        assert!(!owners.has_non_claude_session("missing-pi-session"));
    }
}
