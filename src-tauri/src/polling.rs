use crate::session::{
    determine_status, get_pending_tool_name, parse_last_n_entries, parse_sessions_index,
    SessionDetector, SessionStatus,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;

/// Combined session information for the frontend
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub pid: u32,
    pub session_name: String,
    pub custom_title: Option<String>,
    pub project_path: String,
    pub git_branch: Option<String>,
    pub first_prompt: String,
    pub summary: Option<String>,
    pub message_count: u32,
    pub modified: String,
    pub status: SessionStatus,
    pub latest_message: String,
    pub pending_tool_name: Option<String>,
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
                crate::debug_log::log_error(&format!("Failed to create session detector: {}", e));
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
                Ok((sessions, diagnostics)) => {
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
                                    if let Some(prev_status) = prev_status_map.get(&session.id) {
                                        // Check for notification-worthy transitions
                                        let should_notify = matches!(
                                            (prev_status, &session.status),
                                            (
                                                SessionStatus::Working,
                                                SessionStatus::NeedsPermission
                                                    | SessionStatus::WaitingForInput,
                                            )
                                        );

                                        if should_notify {
                                            // Check cooldown to prevent duplicate notifications
                                            // from status flickering across poll cycles
                                            let on_cooldown = last_notification_time
                                                .get(&session.id)
                                                .map(|t| t.elapsed() < notification_cooldown)
                                                .unwrap_or(false);

                                            if !on_cooldown {
                                                fire_notification(
                                                    &app_handle,
                                                    &notifications_tx,
                                                    NotificationParams {
                                                        session_id: &session.id,
                                                        first_prompt: &session.first_prompt,
                                                        session_name: &session.session_name,
                                                        status: &session.status,
                                                        pending_tool_name: session
                                                            .pending_tool_name
                                                            .as_deref(),
                                                        pid: session.pid,
                                                        project_path: &session.project_path,
                                                    },
                                                );
                                                last_notification_time
                                                    .insert(session.id.clone(), Instant::now());
                                            }
                                        }
                                    }

                                    // Update the status map
                                    prev_status_map
                                        .insert(session.id.clone(), session.status.clone());
                                }
                            }

                            // Clean up disappeared sessions
                            prev_status_map.retain(|id, _| current_session_ids.contains(id));
                            last_notification_time.retain(|id, _| current_session_ids.contains(id));
                        }
                        Err(poisoned) => {
                            crate::debug_log::log_error("Mutex poisoned, recovering...");
                            let mut prev_status_map = poisoned.into_inner();
                            prev_status_map.clear(); // Clear stale state

                            // Seed the map with current sessions (no notifications after recovery)
                            for session in &sessions {
                                prev_status_map.insert(session.id.clone(), session.status.clone());
                            }
                            is_first_cycle = false; // Mark as initialized
                        }
                    }

                    // Emit event to Tauri frontend
                    if let Err(e) = app_handle.emit("sessions-updated", &sessions) {
                        crate::debug_log::log_error(&format!("Failed to emit sessions-updated: {}", e));
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
                        if let Err(e) = app_handle.emit("diagnostic-update", &diagnostics) {
                            crate::debug_log::log_error(&format!("Failed to emit diagnostic-update: {}", e));
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

/// Checks if a file was modified within the last N seconds
fn is_file_recently_modified(path: &Path, seconds: u64) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .map(|modified| {
            modified
                .elapsed()
                .map(|elapsed| elapsed.as_secs() < seconds)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// Detect sessions and enrich them with status and conversation data
pub fn detect_and_enrich_sessions() -> Result<(Vec<Session>, crate::session::DetectionDiagnostics), String> {
    let mut detector =
        SessionDetector::new().map_err(|e| format!("Failed to create session detector: {}", e))?;
    detect_and_enrich_sessions_with_detector(&mut detector)
}

/// Detect sessions using an existing detector (avoids recreating System each call)
fn detect_and_enrich_sessions_with_detector(
    detector: &mut SessionDetector,
) -> Result<(Vec<Session>, crate::session::DetectionDiagnostics), String> {
    let (detected_sessions, diagnostics) = detector
        .detect_sessions()
        .map_err(|e| format!("Failed to detect sessions: {}", e))?;

    let custom_names = crate::session::CustomNames::load();
    let custom_titles = crate::session::CustomTitles::load();
    let mut sessions = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    for detected in detected_sessions {
        // Get session ID - if not found, skip this session
        let session_id = match &detected.session_id {
            Some(id) => id.clone(),
            None => {
                continue;
            }
        };

        // Skip duplicate session IDs (same session can appear in multiple project dirs)
        if seen_ids.contains(&session_id) {
            continue;
        }
        seen_ids.insert(session_id.clone());

        // Try to parse sessions-index.json to get basic info (optional)
        let index_path = detected.project_path.join("sessions-index.json");
        let sessions_index = parse_sessions_index(&index_path).ok();

        // Find the matching entry in the index (if index exists)
        let session_entry = sessions_index.as_ref().and_then(|index| {
            index
                .entries
                .iter()
                .find(|entry| entry.session_id == session_id)
        });

        let (first_prompt, summary, message_count, modified, git_branch) = match session_entry {
            Some(entry) => (
                entry.first_prompt.clone(),
                entry.summary.clone(),
                entry.message_count,
                entry.modified.clone(),
                Some(entry.git_branch.clone()),
            ),
            None => {
                // Session not in index or index doesn't exist - use fallback values
                let session_file_path = detected.project_path.join(format!("{}.jsonl", session_id));

                // Try to get first prompt from JSONL file
                let first_prompt = get_first_prompt_from_jsonl(&session_file_path)
                    .unwrap_or_else(|| "(Active session)".to_string());

                // Count messages in the file
                let message_count = count_messages_in_jsonl(&session_file_path);

                // Get file modification time
                let modified = std::fs::metadata(&session_file_path)
                    .and_then(|m| m.modified())
                    .ok()
                    .map(|t| {
                        let datetime: DateTime<Utc> = t.into();
                        datetime.to_rfc3339()
                    })
                    .unwrap_or_default();

                (first_prompt, None, message_count, modified, None)
            }
        };

        // Parse the session JSONL file to determine status and get latest message
        let session_file_path = detected.project_path.join(format!("{}.jsonl", session_id));
        let entries = match parse_last_n_entries(&session_file_path, 20) {
            Ok(entries) => entries,
            Err(e) => {
                crate::debug_log::log_warn(&format!(
                    "Failed to parse session file for {}: {}",
                    session_id, e
                ));
                vec![]
            }
        };

        let status = if entries.is_empty() {
            SessionStatus::Connecting
        } else {
            let raw_status = determine_status(&entries);
            // Override WaitingForInput if the JSONL file was recently modified.
            // This catches progress entries (bash_progress, thinking updates) that
            // don't get parsed as meaningful entries but indicate active work.
            //
            // Why 8 seconds? Polling runs every 3.5s, Claude writes progress every 1-3s
            // during active work. 8s provides buffer for gaps without delaying "Ready"
            // transition when work truly finishes.
            if raw_status == SessionStatus::WaitingForInput
                && is_file_recently_modified(&session_file_path, 8)
            {
                SessionStatus::Working
            } else {
                raw_status
            }
        };

        let latest_message = get_latest_message_from_entries(&entries);
        let pending_tool_name = get_pending_tool_name(&entries);

        // Skip empty sessions (0 messages) - these are likely sessions where user
        // immediately used /resume to switch to a different session
        if message_count == 0 {
            continue;
        }

        // Use custom name if available, otherwise use detected project name
        let session_name = custom_names
            .get(&session_id)
            .cloned()
            .unwrap_or(detected.project_name);

        // Get custom title if available
        let custom_title = custom_titles.get(&session_id).cloned();

        sessions.push(Session {
            id: session_id,
            pid: detected.pid,
            session_name,
            custom_title,
            project_path: detected.cwd.to_string_lossy().to_string(),
            git_branch,
            first_prompt,
            summary,
            message_count,
            modified,
            status,
            latest_message,
            pending_tool_name,
        });
    }

    Ok((sessions, diagnostics))
}

/// Extract the first user prompt from a session JSONL file
fn get_first_prompt_from_jsonl(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines().map_while(Result::ok).take(50) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
            // Check if this is a user message
            if value.get("type").and_then(|t| t.as_str()) == Some("user") {
                // Try to get the message content
                if let Some(message) = value.get("message") {
                    if let Some(content) = message.get("content") {
                        // Content can be a string or array
                        if let Some(text) = content.as_str() {
                            return Some(truncate_string(text, 100));
                        } else if let Some(arr) = content.as_array() {
                            // Find the first text block
                            for item in arr {
                                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                        return Some(truncate_string(text, 100));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

/// Truncate a string to a maximum length (character-safe for UTF-8)
fn truncate_string(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}

/// Extract the latest message content from session entries
fn get_latest_message_from_entries(entries: &[crate::session::parser::SessionEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }

    // Iterate backwards to find the last user or assistant message
    for entry in entries.iter().rev() {
        match entry {
            crate::session::parser::SessionEntry::User { message, .. } => {
                // Skip tool result entries - only show actual user prompts
                if message.is_tool_result {
                    continue;
                }
                return truncate_string(&message.content, 200);
            }
            crate::session::parser::SessionEntry::Assistant { message, .. } => {
                // For assistant, try to find the last text block
                for content in message.content.iter().rev() {
                    match content {
                        crate::session::parser::MessageContent::Text { text } => {
                            return truncate_string(text, 200);
                        }
                        crate::session::parser::MessageContent::Thinking { thinking, .. } => {
                            return truncate_string(thinking, 200);
                        }
                        crate::session::parser::MessageContent::ToolUse { name, .. } => {
                            return format!("Executing {}...", name);
                        }
                        _ => continue,
                    }
                }
            }
            _ => continue,
        }
    }

    String::new()
}

/// Count user/assistant messages in a JSONL file
fn count_messages_in_jsonl(path: &Path) -> u32 {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    let reader = BufReader::new(file);
    let mut count = 0u32;

    for line in reader.lines().map_while(Result::ok) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(msg_type) = value.get("type").and_then(|t| t.as_str()) {
                if msg_type == "user" || msg_type == "assistant" {
                    count += 1;
                }
            }
        }
    }

    count
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
        session_name,
        status,
        pending_tool_name,
        pid,
        project_path,
    } = params;
    // Truncate title to 60 characters
    let title = truncate_string(first_prompt, 60);

    // Build the body based on the status
    let body = match status {
        SessionStatus::NeedsPermission => {
            let tool_name = pending_tool_name.unwrap_or("unknown tool");
            format!("🔐 {}: Needs permission for {}", session_name, tool_name)
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
        crate::debug_log::log_error(&format!("Failed to emit notification-fired: {}", e));
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
