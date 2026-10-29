use crate::session::enrichment::enrich_detected_sessions;
pub use crate::session::enrichment::{detect_and_enrich_sessions, truncate_string, Session};

/// Keep the per-transcript away-activity index current and drop indexes for
/// sessions that ended. Lives in the GUI poll (not enrichment) so one-shot CLI
/// commands never build it.
fn refresh_away_index(sessions: &[Session]) {
    for session in sessions {
        if let Some(path) = &session.transcript_path {
            crate::session::away::refresh(&session.session_key, path);
        }
    }
    let live: HashSet<&str> = sessions.iter().map(|s| s.session_key.as_str()).collect();
    crate::session::away::retain_sessions(&live);
}
use crate::session::{is_question_tool, pid_is_alive, DetectorState, SessionStatus};
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter, Manager};
#[cfg(not(target_os = "macos"))]
use tauri_plugin_notification::NotificationExt;

/// Cached overlay: session_id -> spawnedBy (i.e. worker_of).
/// Invalidated when the `~/.claude/c9watch/workers/` dir mtime changes.
static WORKERS_OVERLAY: LazyLock<Mutex<Option<WorkersCache>>> = LazyLock::new(|| Mutex::new(None));

struct WorkersCache {
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

fn hash_serialized_session_payload(serialized: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    serialized.hash(&mut hasher);
    hasher.finish()
}

/// Return whether a serialized session payload should be emitted.
///
/// An empty array is a valid successful result and must compare differently
/// from a previously non-empty payload so the frontend can clear stale cards.
fn session_payload_changed(previous_hash: &mut Option<u64>, serialized: Option<&str>) -> bool {
    let current_hash = serialized.map(hash_serialized_session_payload);
    let changed = current_hash != *previous_hash;
    if changed {
        *previous_hash = current_hash;
    }
    changed
}

fn load_workers_overlay() -> Arc<HashMap<String, String>> {
    let workers_dir = match dirs::home_dir() {
        Some(h) => h.join(".claude").join("c9watch").join("workers"),
        None => return Arc::new(HashMap::new()),
    };

    let dir_mtime = match std::fs::metadata(&workers_dir).and_then(|m| m.modified()) {
        Ok(m) => m,
        Err(_) => return Arc::new(HashMap::new()),
    };

    if let Ok(guard) = WORKERS_OVERLAY.lock() {
        if let Some(cache) = guard.as_ref() {
            if cache.dir_mtime == dir_mtime {
                return Arc::clone(&cache.map);
            }
        }
    }

    let mut map = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(&workers_dir) {
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
            dir_mtime,
            map: Arc::clone(&map),
        });
    }
    map
}

/// The diagnostics from the latest poll, for pages that start listening for
/// `diagnostic-update` after it was emitted.
#[derive(Default)]
pub struct LatestDiagnostics(pub Mutex<crate::session::DetectionDiagnostics>);

/// Start the background polling loop
///
/// This function spawns a background thread that:
/// 1. Detects active Claude sessions every 2-3 seconds
/// 2. Enriches them with status information
/// 3. Tracks status transitions and fires notifications
/// 4. Emits "sessions-updated" events to the frontend
/// 5. Broadcasts session data to WebSocket clients
pub fn start_polling(
    app: AppHandle,
    detector: Arc<Mutex<DetectorState>>,
    sessions_tx: tokio::sync::broadcast::Sender<String>,
    notifications_tx: tokio::sync::broadcast::Sender<String>,
) {
    thread::spawn(move || {
        let app_handle = Arc::new(app);
        let poll_interval = Duration::from_millis(3500);

        // Track previous status for each session
        let mut previous_status: HashMap<String, SessionStatus> = HashMap::new();

        // Track last notification time per session to prevent duplicates.
        // If status flickers (Working → Ready → Working → Ready), this cooldown
        // ensures we don't fire the same notification twice within a short window.
        let mut last_notification_time: HashMap<String, Instant> = HashMap::new();
        let notification_cooldown = Duration::from_secs(30);

        // Track if this is the first poll cycle
        let mut is_first_cycle = true;

        let mut prev_diagnostics: Option<crate::session::DetectionDiagnostics> = None;
        let mut prev_sessions_hash: Option<u64> = None;

        loop {
            // Lock the shared DetectorState only for the detection step itself,
            // then drop the guard BEFORE the slow enrichment pass so other call
            // sites (Tauri commands, focus handler) aren't blocked.
            let detect_result = {
                let mut state = match detector.lock() {
                    Ok(g) => g,
                    Err(poisoned) => poisoned.into_inner(),
                };
                state.detect()
            };

            let enriched = match detect_result {
                Ok((detected, diag)) => enrich_detected_sessions(detected, diag),
                Err(e) => Err(format!("Detect failed: {}", e)),
            };

            match enriched {
                Ok((mut sessions, diagnostics)) => {
                    // Overlay worker_of from ~/.claude/c9watch/workers/*/meta.json
                    let overlay = load_workers_overlay();
                    for s in sessions.iter_mut() {
                        if s.provider == crate::session::SessionProvider::ClaudeCode {
                            if let Some(by) = overlay.get(&s.id) {
                                s.worker_of = Some(by.clone());
                            }
                        }
                    }

                    // Track current session IDs to clean up stale entries
                    let current_session_ids: HashSet<String> =
                        sessions.iter().map(|s| s.session_key.clone()).collect();

                    refresh_away_index(&sessions);

                    // Process status transitions and fire notifications
                    for session in
                        notification_transitions(&mut previous_status, is_first_cycle, &sessions)
                    {
                        crate::notifications::notify(&app_handle, session);
                        // Check cooldown to prevent duplicate notifications
                        // from status flickering across poll cycles
                        let on_cooldown = last_notification_time
                            .get(&session.session_key)
                            .map(|t| t.elapsed() < notification_cooldown)
                            .unwrap_or(false);

                        if !on_cooldown {
                            fire_notification(&app_handle, &notifications_tx, session);
                            last_notification_time
                                .insert(session.session_key.clone(), Instant::now());
                        }
                    }
                    is_first_cycle = false;
                    last_notification_time.retain(|id, _| current_session_ids.contains(id));

                    // Serialize once; skip emit/broadcast when payload is unchanged
                    // so idle dashboards don't rerender every poll cycle.
                    let serialized = serde_json::to_string(&sessions).ok();
                    let changed =
                        session_payload_changed(&mut prev_sessions_hash, serialized.as_deref());
                    if changed {
                        // A successful empty result is meaningful: it replaces
                        // stale cards after all process-backed sessions exit.
                        // Only the Err branch below retains the last payload.
                        if let Err(e) = app_handle.emit("sessions-updated", &sessions) {
                            crate::debug_log::log_error(&format!(
                                "Failed to emit sessions-updated: {}",
                                e
                            ));
                        }
                        if let Some(json) = serialized {
                            let _ = sessions_tx.send(json);
                        }
                    }

                    // Emit diagnostics only when changed
                    if prev_diagnostics.as_ref() != Some(&diagnostics) {
                        if let Some(latest) = app_handle.try_state::<LatestDiagnostics>() {
                            if let Ok(mut latest) = latest.0.lock() {
                                *latest = diagnostics.clone();
                            }
                        }
                        crate::debug_log::log_info(&format!(
                            "Poll: found {} claude processes, {} with CWD",
                            diagnostics.claude_processes_found, diagnostics.processes_with_cwd
                        ));
                        if diagnostics.fda_likely_needed {
                            crate::debug_log::log_warn(
                                "Full Disk Access likely needed: processes found but none have readable CWD",
                            );
                        }
                        if let Err(e) = app_handle.emit("diagnostic-update", &diagnostics) {
                            crate::debug_log::log_error(&format!(
                                "Failed to emit diagnostic-update: {}",
                                e
                            ));
                        }
                        prev_diagnostics = Some(diagnostics);
                    }
                }
                Err(e) => {
                    crate::debug_log::log_error(&format!("Error detecting sessions: {}", e));
                    // Continue polling even on error
                }
            }

            thread::sleep(poll_interval);
        }
    });
}

/// Update `prev` with this cycle's statuses and return the sessions that
/// moved from `Working` to `NeedsAttention` or `WaitingForInput`.
///
/// On the first cycle the map is only seeded and nothing is returned.
/// Keys of sessions that are no longer present are pruned.
fn notification_transitions<'a>(
    prev: &mut HashMap<String, SessionStatus>,
    first_cycle: bool,
    sessions: &'a [Session],
) -> Vec<&'a Session> {
    let mut transitioned = Vec::new();
    for session in sessions {
        if !first_cycle {
            let should_notify = matches!(
                (prev.get(&session.session_key), &session.status),
                (
                    Some(SessionStatus::Working),
                    SessionStatus::NeedsAttention | SessionStatus::WaitingForInput,
                )
            );
            if should_notify {
                transitioned.push(session);
            }
        }
        prev.insert(session.session_key.clone(), session.status.clone());
    }
    let current: HashSet<&str> = sessions.iter().map(|s| s.session_key.as_str()).collect();
    prev.retain(|id, _| current.contains(id.as_str()));
    transitioned
}

/// Fire a notification for a status transition
#[cfg_attr(target_os = "macos", allow(unused_variables))]
fn fire_notification(
    app_handle: &AppHandle,
    notifications_tx: &tokio::sync::broadcast::Sender<String>,
    session: &Session,
) {
    let custom_title = session
        .custom_title
        .as_deref()
        .or(session.codex_title.as_deref())
        .or(session.cursor_title.as_deref())
        .or(session.ai_title.as_deref());
    let session_name = &session.session_name;
    let pending_tool_name = session.pending_tool_name.as_deref();
    let title = truncate_string(custom_title.unwrap_or(&session.first_prompt), 60);
    // The WS server sends this instead of `title` when remote clients may not
    // see conversation text (`title` can be the first prompt).
    let redacted_title = truncate_string(custom_title.unwrap_or(session_name), 60);

    // Build the body based on the status
    let body = match session.status {
        SessionStatus::NeedsAttention => {
            if is_question_tool(pending_tool_name) {
                format!("❓ {}: Waiting for your response", session_name)
            } else {
                let tool_name = pending_tool_name.unwrap_or("unknown tool");
                format!("🔐 {}: Needs permission for {}", session_name, tool_name)
            }
        }
        SessionStatus::WaitingForInput => {
            format!("✅ {}: Finished working", session_name)
        }
        _ => return, // Should not happen based on the caller's logic
    };

    // Fire native notification via Tauri plugin
    // Note: Notifications work in production builds (.app) but may not appear in dev mode
    #[cfg(not(target_os = "macos"))]
    if let Err(e) = app_handle
        .notification()
        .builder()
        .title(&title)
        .body(&body)
        .show()
    {
        crate::debug_log::log_error(&format!("Failed to show notification: {}", e));
    }

    // Broadcast to WebSocket clients for web notifications
    let ws_notification = serde_json::json!({
        "title": title,
        "redactedTitle": redacted_title,
        "body": body,
        "sessionId": session.id,
        "sessionKey": session.session_key,
        "provider": session.provider,
        "pid": session.pid,
    });
    if let Ok(json) = serde_json::to_string(&ws_notification) {
        let _ = notifications_tx.send(json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionSource;

    #[test]
    fn empty_session_payload_after_nonempty_payload_is_emitted_as_change() {
        let nonempty = serde_json::to_string(&serde_json::json!([
            {"sessionKey": "claudeCode:stale", "status": "working"}
        ]))
        .unwrap();
        let empty = serde_json::to_string(&serde_json::json!([])).unwrap();
        let mut previous_hash = None;

        assert!(session_payload_changed(
            &mut previous_hash,
            Some(nonempty.as_str())
        ));
        assert!(session_payload_changed(
            &mut previous_hash,
            Some(empty.as_str())
        ));
        assert!(!session_payload_changed(
            &mut previous_hash,
            Some(empty.as_str())
        ));
        assert_eq!(empty, "[]");
    }

    #[test]
    fn synthetic_cursor_detection_and_enrichment_uses_fixture_cursor_root() {
        const SESSION_ID: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let tmp = tempfile::tempdir().unwrap();
        let transcript_dir = tmp
            .path()
            .join("projects")
            .join("demo-project")
            .join("agent-transcripts")
            .join(SESSION_ID);
        std::fs::create_dir_all(&transcript_dir).unwrap();
        std::fs::write(
            transcript_dir.join(format!("{SESSION_ID}.jsonl")),
            r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>synthetic Cursor fixture</user_query>"}]}}
"#,
        )
        .unwrap();

        let mut source =
            crate::session::cursor::CursorSessionSource::at_root(tmp.path().join("projects"));
        let (detected, diagnostics) = source.detect().unwrap();
        let (sessions, _) = enrich_detected_sessions(detected, diagnostics).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, SESSION_ID);
        assert_eq!(
            sessions[0].provider,
            crate::session::SessionProvider::Cursor
        );
        assert_eq!(sessions[0].first_prompt, "synthetic Cursor fixture");
    }

    fn session(key: &str, status: SessionStatus) -> Session {
        let mut s = crate::notifications::tests::fixture();
        s.session_key = key.to_string();
        s.status = status;
        s
    }

    fn keys(sessions: Vec<&Session>) -> Vec<&str> {
        sessions.iter().map(|s| s.session_key.as_str()).collect()
    }

    #[test]
    fn transitions_first_cycle_seeds_without_notifying() {
        let mut prev = HashMap::new();
        prev.insert("gone".to_string(), SessionStatus::Working);
        let sessions = vec![session("a", SessionStatus::WaitingForInput)];

        assert!(notification_transitions(&mut prev, true, &sessions).is_empty());
        assert_eq!(prev.len(), 1);
        assert_eq!(prev.get("a"), Some(&SessionStatus::WaitingForInput));
    }

    #[test]
    fn transitions_working_to_waiting_or_attention_notifies() {
        let mut prev = HashMap::new();
        prev.insert("a".to_string(), SessionStatus::Working);
        prev.insert("b".to_string(), SessionStatus::Working);
        prev.insert("c".to_string(), SessionStatus::Working);
        let sessions = vec![
            session("a", SessionStatus::WaitingForInput),
            session("b", SessionStatus::NeedsAttention),
            session("c", SessionStatus::Working),
        ];

        let fired = notification_transitions(&mut prev, false, &sessions);
        assert_eq!(keys(fired), vec!["a", "b"]);
        assert_eq!(prev.get("a"), Some(&SessionStatus::WaitingForInput));
        assert_eq!(prev.get("b"), Some(&SessionStatus::NeedsAttention));
        assert_eq!(prev.get("c"), Some(&SessionStatus::Working));
    }

    #[test]
    fn transitions_from_non_working_or_unknown_do_not_notify() {
        let mut prev = HashMap::new();
        prev.insert("a".to_string(), SessionStatus::WaitingForInput);
        prev.insert("b".to_string(), SessionStatus::NeedsAttention);
        let sessions = vec![
            session("a", SessionStatus::NeedsAttention),
            session("b", SessionStatus::WaitingForInput),
            session("new", SessionStatus::WaitingForInput),
        ];

        assert!(notification_transitions(&mut prev, false, &sessions).is_empty());
        assert_eq!(prev.get("new"), Some(&SessionStatus::WaitingForInput));
    }

    #[test]
    fn transitions_prune_disappeared_sessions() {
        let mut prev = HashMap::new();
        prev.insert("a".to_string(), SessionStatus::Working);
        prev.insert("gone".to_string(), SessionStatus::Working);
        let sessions = vec![session("a", SessionStatus::Working)];

        assert!(notification_transitions(&mut prev, false, &sessions).is_empty());
        assert!(!prev.contains_key("gone"));
        assert_eq!(prev.len(), 1);
    }

    #[test]
    fn test_truncate_string_no_truncation() {
        assert_eq!(truncate_string("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_string_exact_boundary() {
        assert_eq!(truncate_string("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_string_over_boundary() {
        assert_eq!(truncate_string("hello world", 5), "hello...");
    }

    #[test]
    fn test_truncate_string_empty() {
        assert_eq!(truncate_string("", 10), "");
    }

    #[test]
    fn test_truncate_string_zero_max() {
        assert_eq!(truncate_string("hello", 0), "...");
        assert!(!truncate_string("hello", 0).contains('h'));
    }

    #[test]
    fn test_truncate_string_single_char_limit() {
        assert_eq!(truncate_string("hello", 1), "h...");
    }

    #[test]
    fn test_truncate_string_utf8_accented() {
        // "héllo" has 5 chars — truncating to 3 gives "hél..."
        assert_eq!(truncate_string("héllo", 3), "hél...");
    }

    #[test]
    fn test_truncate_string_utf8_cjk() {
        // Each CJK char is 1 Unicode scalar value
        assert_eq!(truncate_string("你好世界", 2), "你好...");
    }

    #[test]
    fn test_truncate_string_utf8_emoji() {
        // Emoji counts as 1 char in .chars()
        assert_eq!(truncate_string("Hello 👋 World", 7), "Hello 👋...");
    }
}
