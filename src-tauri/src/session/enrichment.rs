use crate::session::{
    determine_status, get_pending_tool_input, get_pending_tool_name, parse_last_n_entries,
    parse_sessions_index, LegacySessionSource, SessionStatus,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::{LazyLock, Mutex};

/// Combined session information
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
    /// The input/arguments of the pending tool (when status is NeedsPermission)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_tool_input: Option<serde_json::Value>,
    /// Session ID of the PM that spawned this session (if it's a c9watch worker).
    /// Populated in polling.rs by overlay from `~/.claude/c9watch/workers/*/meta.json`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_of: Option<String>,
}

/// Cache for native custom titles, keyed by file path.
/// Stores (mtime_as_nanos, cached_title) to avoid re-scanning JSONL files every poll cycle.
static NATIVE_TITLE_CACHE: LazyLock<Mutex<HashMap<std::path::PathBuf, (u64, Option<String>)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Look up the native custom title for a session JSONL, using a mtime-based cache.
pub(crate) fn get_cached_native_title(path: &Path) -> Option<String> {
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;

    if let Ok(mut cache) = NATIVE_TITLE_CACHE.lock() {
        if let Some((cached_mtime, cached_title)) = cache.get(path) {
            if *cached_mtime == mtime {
                return cached_title.clone();
            }
        }
        // Cache miss or stale — re-scan
        let title = crate::session::parser::get_native_custom_title_from_file(path);
        cache.insert(path.to_path_buf(), (mtime, title.clone()));
        title
    } else {
        // Mutex poisoned — fallback to direct read
        crate::session::parser::get_native_custom_title_from_file(path)
    }
}

/// Detect sessions and enrich them with status and conversation data
pub fn detect_and_enrich_sessions(
) -> Result<(Vec<Session>, crate::session::DetectionDiagnostics), String> {
    let mut detector =
        LegacySessionSource::new().map_err(|e| format!("Failed to create session detector: {}", e))?;
    detect_and_enrich_sessions_with_detector(&mut detector)
}

/// Detect sessions using an existing detector (avoids recreating System each call)
pub fn detect_and_enrich_sessions_with_detector(
    detector: &mut LegacySessionSource,
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
            Some(entry) => {
                // Guard: if sessions-index first_prompt is a system command, try JSONL fallback
                let fp = if crate::session::parser::is_system_content(&entry.first_prompt) {
                    let session_file_path =
                        detected.project_path.join(format!("{}.jsonl", session_id));
                    get_first_prompt_from_jsonl(&session_file_path)
                        .unwrap_or_else(|| entry.first_prompt.clone())
                } else {
                    entry.first_prompt.clone()
                };
                (
                    fp,
                    entry.summary.clone(),
                    entry.message_count,
                    entry.modified.clone(),
                    Some(entry.git_branch.clone()),
                )
            }
            None => {
                // Session not in index or index doesn't exist - use fallback values
                let session_file_path =
                    detected.project_path.join(format!("{}.jsonl", session_id));

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
        let pending_tool_input = get_pending_tool_input(&entries);

        // Skip empty sessions (0 messages)
        if message_count == 0 {
            continue;
        }

        // Use custom name if available, otherwise use detected project name
        let session_name = custom_names
            .get(&session_id)
            .cloned()
            .unwrap_or(detected.project_name);

        // Get custom title: Claude Code native /rename takes priority over c9watch's own.
        // Uses a static cache keyed by (path, mtime) to avoid re-scanning the JSONL every cycle.
        let native_title = get_cached_native_title(&session_file_path);
        let custom_title =
            native_title.or_else(|| custom_titles.get(&session_id).cloned());

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
            pending_tool_input,
            worker_of: None,
        });
    }

    Ok((sessions, diagnostics))
}

/// Checks if a file was modified within the last N seconds
pub fn is_file_recently_modified(path: &Path, seconds: u64) -> bool {
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

/// Extract the first user prompt from a session JSONL file (truncated to 100 chars).
pub fn get_first_prompt_from_jsonl(path: &Path) -> Option<String> {
    get_first_prompt_from_jsonl_raw(path).map(|s| truncate_string(&s, 100))
}

/// Extract the first user prompt from a session JSONL file (full text, no truncation).
pub fn get_first_prompt_from_jsonl_raw(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines().map_while(Result::ok).take(50) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
            if value.get("type").and_then(|t| t.as_str()) == Some("user") {
                if let Some(message) = value.get("message") {
                    if let Some(content) = message.get("content") {
                        if let Some(text) = content.as_str() {
                            // Skip system-generated local command messages
                            if crate::session::parser::is_system_content(text) {
                                continue;
                            }
                            return Some(text.to_string());
                        } else if let Some(arr) = content.as_array() {
                            for item in arr {
                                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                                    if let Some(text) =
                                        item.get("text").and_then(|t| t.as_str())
                                    {
                                        return Some(text.to_string());
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
pub fn truncate_string(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}

/// Extract the latest message content from session entries
pub fn get_latest_message_from_entries(
    entries: &[crate::session::parser::SessionEntry],
) -> String {
    if entries.is_empty() {
        return String::new();
    }

    for entry in entries.iter().rev() {
        match entry {
            crate::session::parser::SessionEntry::User { message, .. } => {
                // Skip tool result entries and system-generated command messages
                if message.is_tool_result
                    || crate::session::parser::is_system_content(&message.content)
                {
                    continue;
                }
                return truncate_string(&message.content, 200);
            }
            crate::session::parser::SessionEntry::Assistant { message, .. } => {
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

/// Count user/assistant messages in a JSONL file.
/// Skips system-injected user messages (local commands, slash commands, etc.)
pub fn count_messages_in_jsonl(path: &Path) -> u32 {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    let reader = BufReader::new(file);
    let mut count = 0u32;

    for line in reader.lines().map_while(Result::ok) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(msg_type) = value.get("type").and_then(|t| t.as_str()) {
                match msg_type {
                    "assistant" => count += 1,
                    "user" => {
                        // Skip system-injected user messages
                        if let Some(content) = value
                            .get("message")
                            .and_then(|m| m.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            if !crate::session::parser::is_system_content(content) {
                                count += 1;
                            }
                        } else {
                            // Array content (tool results) — still count them
                            count += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    count
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(truncate_string("héllo", 3), "hél...");
    }

    #[test]
    fn test_truncate_string_utf8_cjk() {
        assert_eq!(truncate_string("你好世界", 2), "你好...");
    }

    #[test]
    fn test_truncate_string_utf8_emoji() {
        assert_eq!(truncate_string("Hello 👋 World", 7), "Hello 👋...");
    }
}
