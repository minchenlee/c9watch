use crate::session::enrichment::enrich_detected_sessions;
pub use crate::session::enrichment::{detect_and_enrich_sessions, truncate_string, Session};
use crate::session::transitions::notification_transitions;

use crate::session::{DetectorState, SessionStatus};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
#[cfg(not(target_os = "macos"))]
use tauri_plugin_notification::NotificationExt;

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

        // Dev channel only: compare each poll with the engine (C1a-17).
        #[cfg(all(unix, c9_channel_dev))]
        let mut shadow = crate::engine_proto::shadow::ShadowDiff::for_gui();

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
                    // Before the workers overlay, so both sides compare
                    // plain detection. Never starts the engine.
                    #[cfg(all(unix, c9_channel_dev))]
                    if let Some(shadow) = shadow.as_mut() {
                        shadow.tick(&sessions);
                    }

                    // Overlay worker_of from ~/.claude/c9watch/workers/*/meta.json
                    crate::session::workers::apply_workers_overlay(&mut sessions);

                    // Track current session IDs to clean up stale entries
                    let current_session_ids: HashSet<String> =
                        sessions.iter().map(|s| s.session_key.clone()).collect();

                    crate::session::away::refresh_index(&sessions);

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

/// Fire a notification for a status transition
#[cfg_attr(target_os = "macos", allow(unused_variables))]
fn fire_notification(
    app_handle: &AppHandle,
    notifications_tx: &tokio::sync::broadcast::Sender<String>,
    session: &Session,
) {
    // Only NeedsAttention and WaitingForInput have a text; the caller's
    // transitions send no other status.
    let Some(text) = crate::session::notification::notification_text(session) else {
        return;
    };

    // Fire native notification via Tauri plugin
    // Note: Notifications work in production builds (.app) but may not appear in dev mode
    #[cfg(not(target_os = "macos"))]
    if let Err(e) = app_handle
        .notification()
        .builder()
        .title(&text.title)
        .body(&text.body)
        .show()
    {
        crate::debug_log::log_error(&format!("Failed to show notification: {}", e));
    }

    // Broadcast to WebSocket clients for web notifications
    let ws_notification = web_notification(session, text);
    if let Ok(json) = serde_json::to_string(&ws_notification) {
        let _ = notifications_tx.send(json);
    }
}

/// The notification the WS server broadcasts to web clients. The WS server
/// sends `redactedTitle` instead of `title` when remote clients may not see
/// conversation text (`title` can be the first prompt).
fn web_notification(
    session: &Session,
    text: crate::session::notification::NotificationText,
) -> serde_json::Value {
    serde_json::json!({
        "title": text.title,
        "redactedTitle": text.redacted_title,
        "body": text.body,
        "sessionId": session.id,
        "sessionKey": session.session_key,
        "provider": session.provider,
        "pid": session.pid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionSource;

    /// The web notification is the pre-move JSON, built from the same
    /// `notification_text` the engine's `notification.*` data uses.
    #[test]
    fn web_notification_payload_is_unchanged() {
        let mut s = Session::fixture("a", SessionStatus::NeedsAttention);
        s.pending_tool_name = Some("Bash".into());
        let text = crate::session::notification::notification_text(&s).unwrap();
        assert_eq!(
            web_notification(&s, text),
            serde_json::json!({
                "title": "investigate",
                "redactedTitle": "name-a",
                "body": "🔐 name-a: Needs permission for Bash",
                "sessionId": "a",
                "sessionKey": "claudeCode:a",
                "provider": "claudeCode",
                "pid": 4242,
            })
        );
    }

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
