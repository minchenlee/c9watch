use crate::session::enrichment::detect_and_enrich_sessions_with_detector;
pub use crate::session::enrichment::{detect_and_enrich_sessions, truncate_string, Session};
use crate::session::{SessionDetector, SessionStatus};
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap as StdHashMap;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Mutex as StdMutex;
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;

/// Cached overlay: session_id -> spawnedBy (i.e. worker_of).
/// Invalidated when the `~/.claude/c9watch/workers/` dir mtime changes.
static WORKERS_OVERLAY: LazyLock<StdMutex<Option<WorkersCache>>> =
    LazyLock::new(|| StdMutex::new(None));

struct WorkersCache {
    dir_mtime: SystemTime,
    map: StdHashMap<String, String>,
}

/// Cross-platform PID liveness check used to filter stale worker meta.json
/// entries out of the overlay (fix H2b). On Unix, `kill(pid, 0)` returns 0
/// if the process exists; `ESRCH` if it doesn't. On Windows we can't easily
/// check without adding a dependency, so assume the meta.json is fresh —
/// `stoppedAt` + the daemon cleanup on stop cover the common cases.
#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    true
}

fn load_workers_overlay() -> StdHashMap<String, String> {
    let workers_dir = match dirs::home_dir() {
        Some(h) => h.join(".claude").join("c9watch").join("workers"),
        None => return StdHashMap::new(),
    };

    let dir_mtime = match std::fs::metadata(&workers_dir).and_then(|m| m.modified()) {
        Ok(m) => m,
        Err(_) => return StdHashMap::new(),
    };

    if let Ok(guard) = WORKERS_OVERLAY.lock() {
        if let Some(cache) = guard.as_ref() {
            if cache.dir_mtime == dir_mtime {
                return cache.map.clone();
            }
        }
    }

    let mut map = StdHashMap::new();
    if let Ok(entries) = std::fs::read_dir(&workers_dir) {
        for entry in entries.flatten() {
            let meta_path = entry.path().join("meta.json");
            if let Ok(content) = std::fs::read_to_string(&meta_path) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                    let sid = val.get("sessionId").and_then(|v| v.as_str());
                    let by = val.get("spawnedBy").and_then(|v| v.as_str());
                    // Skip entries whose stoppedAt is set or whose PID is no
                    // longer alive (fix H2b): the daemon should clean up
                    // worker dirs on stop/shutdown, but crashes or kill -9
                    // can leave stale meta.json files behind.
                    if val.get("stoppedAt").and_then(|v| v.as_str()).is_some() {
                        continue;
                    }
                    if let Some(pid) = val.get("pid").and_then(|v| v.as_u64()) {
                        if !pid_is_alive(pid as u32) {
                            continue;
                        }
                    }
                    if let (Some(sid), Some(by)) = (sid, by) {
                        map.insert(sid.to_string(), by.to_string());
                    }
                }
            }
        }
    }

    if let Ok(mut guard) = WORKERS_OVERLAY.lock() {
        *guard = Some(WorkersCache {
            dir_mtime,
            map: map.clone(),
        });
    }
    map
}

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
    sessions_tx: tokio::sync::broadcast::Sender<String>,
    notifications_tx: tokio::sync::broadcast::Sender<String>,
) {
    thread::spawn(move || {
        let app_handle = Arc::new(app);
        let poll_interval = Duration::from_millis(3500);

        // Create detector once and reuse across poll cycles
        let mut detector = match SessionDetector::new() {
            Ok(d) => d,
            Err(e) => {
                crate::debug_log::log_error(&format!(
                    "Failed to create session detector: {}",
                    e
                ));
                return;
            }
        };

        // Track previous status for each session
        let previous_status: Arc<Mutex<HashMap<String, SessionStatus>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Track last notification time per session to prevent duplicates.
        // If status flickers (Working → Ready → Working → Ready), this cooldown
        // ensures we don't fire the same notification twice within a short window.
        let mut last_notification_time: HashMap<String, Instant> = HashMap::new();
        let notification_cooldown = Duration::from_secs(30);

        // Track if this is the first poll cycle
        let mut is_first_cycle = true;

        let mut prev_diagnostics: Option<crate::session::DetectionDiagnostics> = None;

        loop {
            // Detect and enrich sessions
            match detect_and_enrich_sessions_with_detector(&mut detector) {
                Ok((mut sessions, diagnostics)) => {
                    // Overlay worker_of from ~/.claude/c9watch/workers/*/meta.json
                    let overlay = load_workers_overlay();
                    if !overlay.is_empty() {
                        for s in sessions.iter_mut() {
                            if let Some(by) = overlay.get(&s.id) {
                                s.worker_of = Some(by.clone());
                            }
                        }
                    }

                    // Track current session IDs to clean up stale entries
                    let current_session_ids: HashSet<String> =
                        sessions.iter().map(|s| s.id.clone()).collect();

                    // Process status transitions and fire notifications
                    match previous_status.lock() {
                        Ok(mut prev_status_map) => {
                            if is_first_cycle {
                                // First cycle: seed the map without notifications
                                for session in &sessions {
                                    prev_status_map
                                        .insert(session.id.clone(), session.status.clone());
                                }
                                is_first_cycle = false;
                            } else {
                                // Check for status transitions
                                for session in &sessions {
                                    if let Some(prev_status) =
                                        prev_status_map.get(&session.id)
                                    {
                                        // Check for notification-worthy transitions
                                        let should_notify = matches!(
                                            (prev_status, &session.status),
                                            (
                                                SessionStatus::Working,
                                                SessionStatus::NeedsAttention
                                                    | SessionStatus::WaitingForInput,
                                            )
                                        );

                                        if should_notify {
                                            // Check cooldown to prevent duplicate notifications
                                            // from status flickering across poll cycles
                                            let on_cooldown = last_notification_time
                                                .get(&session.id)
                                                .map(|t| {
                                                    t.elapsed() < notification_cooldown
                                                })
                                                .unwrap_or(false);

                                            if !on_cooldown {
                                                fire_notification(
                                                    &app_handle,
                                                    &notifications_tx,
                                                    NotificationParams {
                                                        session_id: &session.id,
                                                        first_prompt: &session.first_prompt,
                                                        custom_title: session
                                                            .custom_title
                                                            .as_deref(),
                                                        session_name: &session.session_name,
                                                        status: &session.status,
                                                        pending_tool_name: session
                                                            .pending_tool_name
                                                            .as_deref(),
                                                        pid: session.pid,
                                                        project_path: &session.project_path,
                                                    },
                                                );
                                                last_notification_time.insert(
                                                    session.id.clone(),
                                                    Instant::now(),
                                                );
                                            }
                                        }
                                    }

                                    // Update the status map
                                    prev_status_map
                                        .insert(session.id.clone(), session.status.clone());
                                }
                            }

                            // Clean up disappeared sessions
                            prev_status_map
                                .retain(|id, _| current_session_ids.contains(id));
                            last_notification_time
                                .retain(|id, _| current_session_ids.contains(id));
                        }
                        Err(poisoned) => {
                            crate::debug_log::log_error("Mutex poisoned, recovering...");
                            let mut prev_status_map = poisoned.into_inner();
                            prev_status_map.clear(); // Clear stale state

                            // Seed the map with current sessions (no notifications after recovery)
                            for session in &sessions {
                                prev_status_map
                                    .insert(session.id.clone(), session.status.clone());
                            }
                            is_first_cycle = false; // Mark as initialized
                        }
                    }

                    // Emit event to Tauri frontend
                    if let Err(e) = app_handle.emit("sessions-updated", &sessions) {
                        crate::debug_log::log_error(&format!(
                            "Failed to emit sessions-updated: {}",
                            e
                        ));
                    }

                    // Broadcast to WebSocket clients
                    if let Ok(json) = serde_json::to_string(&sessions) {
                        let _ = sessions_tx.send(json);
                    }

                    // Emit diagnostics only when changed
                    let diag_changed = prev_diagnostics.as_ref().map_or(true, |prev| {
                        prev.claude_processes_found != diagnostics.claude_processes_found
                            || prev.processes_with_cwd != diagnostics.processes_with_cwd
                    });
                    if diag_changed {
                        crate::debug_log::log_info(&format!(
                            "Poll: found {} claude processes, {} with CWD",
                            diagnostics.claude_processes_found, diagnostics.processes_with_cwd
                        ));
                        if diagnostics.fda_likely_needed {
                            crate::debug_log::log_warn(
                                "Full Disk Access likely needed: processes found but none have readable CWD",
                            );
                        }
                        if let Err(e) =
                            app_handle.emit("diagnostic-update", &diagnostics)
                        {
                            crate::debug_log::log_error(&format!(
                                "Failed to emit diagnostic-update: {}",
                                e
                            ));
                        }
                        prev_diagnostics = Some(diagnostics);
                    }
                }
                Err(e) => {
                    crate::debug_log::log_error(&format!(
                        "Error detecting sessions: {}",
                        e
                    ));
                    // Continue polling even on error
                }
            }

            thread::sleep(poll_interval);
        }
    });
}

/// Notification metadata for click-to-focus
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NotificationMetadata {
    notification_id: i32,
    session_id: String,
    pid: u32,
    project_path: String,
    title: String,
}

/// Parameters for firing a notification
struct NotificationParams<'a> {
    session_id: &'a str,
    first_prompt: &'a str,
    custom_title: Option<&'a str>,
    session_name: &'a str,
    status: &'a SessionStatus,
    pending_tool_name: Option<&'a str>,
    pid: u32,
    project_path: &'a str,
}

/// Fire a notification for a status transition
fn fire_notification(
    app_handle: &AppHandle,
    notifications_tx: &tokio::sync::broadcast::Sender<String>,
    params: NotificationParams<'_>,
) {
    let NotificationParams {
        session_id,
        first_prompt,
        custom_title,
        session_name,
        status,
        pending_tool_name,
        pid,
        project_path,
    } = params;
    let title = truncate_string(custom_title.unwrap_or(first_prompt), 60);

    // Build the body based on the status
    let body = match status {
        SessionStatus::NeedsAttention => {
            let tool_name = pending_tool_name.unwrap_or("unknown tool");
            match tool_name {
                "Question" | "AskUserQuestion" => {
                    format!("❓ {}: Waiting for your response", session_name)
                }
                _ => {
                    format!("🔐 {}: Needs permission for {}", session_name, tool_name)
                }
            }
        }
        SessionStatus::WaitingForInput => {
            format!("✅ {}: Finished working", session_name)
        }
        _ => return, // Should not happen based on the caller's logic
    };

    // Generate a stable i32 ID from the session_id string using hash
    let mut hasher = DefaultHasher::new();
    session_id.hash(&mut hasher);
    let notification_id = (hasher.finish() as i32).abs();

    // Fire native notification via Tauri plugin
    // Note: Notifications work in production builds (.app) but may not appear in dev mode
    if let Err(e) = app_handle
        .notification()
        .builder()
        .title(&title)
        .body(&body)
        .show()
    {
        crate::debug_log::log_error(&format!("Failed to show notification: {}", e));
    }

    // Emit event with session metadata for click-to-focus handling
    let metadata = NotificationMetadata {
        notification_id,
        session_id: session_id.to_string(),
        pid,
        project_path: project_path.to_string(),
        title: title.clone(),
    };

    if let Err(e) = app_handle.emit("notification-fired", &metadata) {
        crate::debug_log::log_error(&format!(
            "Failed to emit notification-fired: {}",
            e
        ));
    }

    // Broadcast to WebSocket clients for web notifications
    let ws_notification = serde_json::json!({
        "title": title,
        "body": body,
        "sessionId": session_id,
        "pid": pid,
    });
    if let Ok(json) = serde_json::to_string(&ws_notification) {
        let _ = notifications_tx.send(json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn pid_is_alive_detects_dead_pid() {
        // PID 0 is never a real process.
        assert!(!pid_is_alive(0));
        // PID 999999 is extremely unlikely to be live (kernel default pid_max
        // on macOS is 99999, and even on Linux systems with expanded range
        // it's highly unlikely to hit this).
        assert!(!pid_is_alive(999_999));
    }

    #[cfg(unix)]
    #[test]
    fn pid_is_alive_detects_live_pid() {
        // Our own pid must be alive.
        assert!(pid_is_alive(std::process::id()));
    }

    #[test]
    fn test_detect_and_enrich_sessions() {
        // This test will only work if there are active Claude sessions
        match detect_and_enrich_sessions() {
            Ok((sessions, _diagnostics)) => {
                println!("Detected {} sessions", sessions.len());
                for session in sessions {
                    println!(
                        "Session: {} - {} (PID: {}, Status: {:?})",
                        session.id, session.session_name, session.pid, session.status
                    );
                }
            }
            Err(e) => {
                println!("Error detecting sessions: {}", e);
            }
        }
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
