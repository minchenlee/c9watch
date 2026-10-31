use super::parser::{AssistantMessage, MessageContent, SessionEntry};
use super::permissions::PermissionChecker;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

/// Checker built from the user-level settings files.
fn get_permission_checker() -> Arc<PermissionChecker> {
    PermissionChecker::cached_for(None)
}

/// Represents the current status of a Claude Code session
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
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

impl SessionStatus {
    /// The snake_case name the engine stores in `agents.status` and sends on
    /// c9p/1.
    pub fn wire_name(&self) -> &'static str {
        match self {
            SessionStatus::Working => "working",
            SessionStatus::NeedsAttention => "needs_attention",
            SessionStatus::WaitingForInput => "waiting_for_input",
            SessionStatus::Connecting => "connecting",
        }
    }
}

/// Analyzes session entries to determine the current status
///
/// # Arguments
/// * `entries` - Recent session entries (typically last 10-20 entries)
///
/// # Returns
/// The determined session status
pub fn determine_status(entries: &[SessionEntry]) -> SessionStatus {
    determine_status_with(entries, &get_permission_checker())
}

/// [`determine_status`] using `checker` to decide whether a pending tool is
/// waiting on a permission prompt.
pub fn determine_status_with(
    entries: &[SessionEntry],
    checker: &PermissionChecker,
) -> SessionStatus {
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
            let raw_status = analyze_assistant_message(message, checker);

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
                    // 1. Pending tool_use (auto-approved) - the tool is running
                    // 2. Text with no stop_reason (but stop_reason is always None in JSONL)
                    //
                    // For text/thinking content, use recency: if the entry was written
                    // recently, Claude is likely still streaming or about to write more.
                    // If old, session is idle. 20s threshold accommodates streaming and
                    // thinking pauses.
                    if has_pending_tool_uses(&message.content)
                        || is_entry_recent(&base.timestamp, 20)
                    {
                        SessionStatus::Working
                    } else {
                        SessionStatus::WaitingForInput
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

/// The message of the last assistant entry in `entries`.
fn last_assistant(entries: &[SessionEntry]) -> Option<&AssistantMessage> {
    entries.iter().rev().find_map(|entry| match entry {
        SessionEntry::Assistant { message, .. } => Some(message),
        _ => None,
    })
}

/// Tool uses in `content` with no matching tool_result in the same content,
/// in order, as `(name, input)`.
fn pending_tool_uses(
    content: &[MessageContent],
) -> impl Iterator<Item = (&str, &serde_json::Value)> {
    let completed_ids: HashSet<&str> = content
        .iter()
        .filter_map(|c| match c {
            MessageContent::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect();
    content.iter().filter_map(move |c| match c {
        MessageContent::ToolUse { id, name, input } if !completed_ids.contains(id.as_str()) => {
            Some((name.as_str(), input))
        }
        _ => None,
    })
}

/// Checks if the message content has a pending AskUserQuestion tool use
fn has_pending_ask_user_question(content: &[MessageContent]) -> bool {
    pending_tool_uses(content).any(|(name, _)| name == "AskUserQuestion")
}

/// Checks if an assistant message is asking the user a question in text:
/// the last text block ends with a question mark (e.g., "Want me to proceed?")
/// and `stop_reason` is `end_turn`.
///
/// A pending `AskUserQuestion` tool use is detected separately, before this
/// check runs, by [`has_pending_ask_user_question`] and
/// [`get_pending_tool_name_with`].
fn is_assistant_asking_question(message: &AssistantMessage) -> bool {
    if message.stop_reason.as_deref() == Some("end_turn") {
        let last_text = message.content.iter().rev().find_map(|c| {
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
fn analyze_assistant_message(
    message: &AssistantMessage,
    checker: &PermissionChecker,
) -> SessionStatus {
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
            if are_pending_tools_auto_approved(&message.content, checker) {
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
fn are_pending_tools_auto_approved(
    content: &[MessageContent],
    checker: &PermissionChecker,
) -> bool {
    pending_tool_uses(content).all(|(name, input)| checker.is_auto_approved(name, input))
}

/// True when a pending tool name means Claude is asking the user a question
/// ("Question" for a text question, "AskUserQuestion" for the tool).
pub fn is_question_tool(name: Option<&str>) -> bool {
    matches!(name, Some("Question" | "AskUserQuestion"))
}

/// Gets the name describing why the session needs attention.
///
/// Returns one of:
/// - A tool name (e.g., "Bash", "Write") if a tool needs user permission
/// - "AskUserQuestion" if Claude is asking the user a question via tool
/// - "Question" if Claude's last text ends with a question mark
/// - None if the session doesn't need attention
pub fn get_pending_tool_name(entries: &[SessionEntry]) -> Option<String> {
    get_pending_tool_name_with(entries, &get_permission_checker())
}

/// [`get_pending_tool_name`] using `checker` for permission decisions.
pub fn get_pending_tool_name_with(
    entries: &[SessionEntry],
    checker: &PermissionChecker,
) -> Option<String> {
    let last_assistant = last_assistant(entries)?;

    // Find the first pending tool that needs permission or AskUserQuestion
    for (name, input) in pending_tool_uses(&last_assistant.content) {
        // Pending AskUserQuestion → return it directly
        if name == "AskUserQuestion" {
            return Some("AskUserQuestion".to_string());
        }

        // This tool is pending - check if it needs permission
        if !checker.is_auto_approved(name, input) {
            return Some(name.to_string());
        }
    }

    // Check if assistant is asking a text-based question
    if is_assistant_asking_question(last_assistant) {
        return Some("Question".to_string());
    }

    None
}

/// Gets the input of the first pending tool that needs permission.
/// Returns the full tool_use JSON value so the agent knows what action is being requested.
pub fn get_pending_tool_input_with(
    entries: &[SessionEntry],
    checker: &PermissionChecker,
) -> Option<serde_json::Value> {
    pending_tool_uses(&last_assistant(entries)?.content)
        .find(|(name, input)| !checker.is_auto_approved(name, input))
        .map(|(_, input)| input.clone())
}

/// Checks if there are any pending (incomplete) tool uses
fn has_pending_tool_uses(content: &[MessageContent]) -> bool {
    !check_all_tools_completed(content)
}

/// Checks if all tool uses in the content have corresponding tool results
/// (true when there are no tool uses).
fn check_all_tools_completed(content: &[MessageContent]) -> bool {
    pending_tool_uses(content).next().is_none()
}

#[cfg(test)]
mod tests {
    #[test]
    fn is_question_tool_matches_only_question_names() {
        assert!(super::is_question_tool(Some("Question")));
        assert!(super::is_question_tool(Some("AskUserQuestion")));
        assert!(!super::is_question_tool(Some("Bash")));
        assert!(!super::is_question_tool(Some("question")));
        assert!(!super::is_question_tool(Some("")));
        assert!(!super::is_question_tool(None));
    }

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
                tool_result_ids: vec![],
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
        let status = determine_status_with(&entries, &PermissionChecker::default());
        assert_ne!(status, SessionStatus::WaitingForInput);
        // Explicit empty rules keep this assertion independent of user allow settings.
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
                    tool_result_ids: vec![],
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
                tool_result_ids: vec![],
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
                tool_result_ids: vec![],
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
        assert_eq!(
            get_pending_tool_name_with(&entries, &PermissionChecker::default()),
            Some("Bash".to_string())
        );
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
        assert_eq!(
            get_pending_tool_name(&entries),
            Some("Question".to_string())
        );
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

    fn pending_tool(name: &str, base: SessionEntryBase) -> SessionEntry {
        SessionEntry::Assistant {
            base,
            message: AssistantMessage {
                model: "claude-opus-5-5".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::ToolUse {
                    id: "toolu_1".to_string(),
                    name: name.to_string(),
                    input: serde_json::json!({"command": "cargo build"}),
                }],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }
    }

    #[test]
    fn test_long_running_subagent_is_working_not_needs_attention() {
        let entries = vec![pending_tool("Agent", create_old_base())];
        assert_eq!(determine_status(&entries), SessionStatus::Working);
        assert_eq!(get_pending_tool_name(&entries), None);
    }

    #[test]
    fn test_assume_approved_checker_never_infers_a_prompt() {
        let entries = vec![pending_tool("Bash", create_old_base())];
        let checker = PermissionChecker::assume_approved();
        assert_eq!(
            determine_status_with(&entries, &checker),
            SessionStatus::Working
        );
        assert_eq!(get_pending_tool_name_with(&entries, &checker), None);
        assert_eq!(get_pending_tool_input_with(&entries, &checker), None);
    }

    fn assistant_with(content: Vec<MessageContent>) -> SessionEntry {
        SessionEntry::Assistant {
            base: create_base(),
            message: AssistantMessage {
                model: "claude-opus-5-5".to_string(),
                id: "msg_test".to_string(),
                role: "assistant".to_string(),
                content,
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }
    }

    fn tool_use(id: &str, name: &str, input: serde_json::Value) -> MessageContent {
        MessageContent::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
        }
    }

    fn tool_result(id: &str) -> MessageContent {
        MessageContent::ToolResult {
            tool_use_id: id.to_string(),
            content: "ok".to_string(),
            is_error: None,
        }
    }

    /// Pins the contract of `get_pending_tool_input_with`: the input of the
    /// first pending tool, in content order, that the checker does not
    /// auto-approve, read from the last assistant message only. Completed and
    /// auto-approved tools (including AskUserQuestion) are skipped.
    #[test]
    fn test_get_pending_tool_input_first_non_auto_approved_pending_tool() {
        // No allow rules: Read and AskUserQuestion never prompt, Bash and Write do.
        let checker = PermissionChecker::from_files(&[]);
        let older = assistant_with(vec![tool_use(
            "toolu_old",
            "Bash",
            serde_json::json!({"command": "old"}),
        )]);
        let last = assistant_with(vec![
            tool_use("toolu_1", "Read", serde_json::json!({"file_path": "/a"})),
            tool_use("toolu_2", "Bash", serde_json::json!({"command": "done"})),
            tool_use("toolu_3", "AskUserQuestion", serde_json::json!({"q": 1})),
            tool_use("toolu_4", "Write", serde_json::json!({"file_path": "/w"})),
            tool_use("toolu_5", "Bash", serde_json::json!({"command": "later"})),
            tool_result("toolu_2"),
        ]);
        let entries = vec![older, last];
        assert_eq!(
            get_pending_tool_input_with(&entries, &checker),
            Some(serde_json::json!({"file_path": "/w"}))
        );
        // The name lookup short-circuits on the pending AskUserQuestion first.
        assert_eq!(
            get_pending_tool_name_with(&entries, &checker),
            Some("AskUserQuestion".to_string())
        );

        // Every pending tool auto-approved, or all completed: no input.
        let approved = vec![assistant_with(vec![
            tool_use("toolu_1", "Read", serde_json::json!({})),
            tool_use("toolu_2", "Bash", serde_json::json!({"command": "x"})),
            tool_result("toolu_2"),
        ])];
        assert_eq!(get_pending_tool_input_with(&approved, &checker), None);
        assert_eq!(get_pending_tool_name_with(&approved, &checker), None);
        assert_eq!(get_pending_tool_input_with(&[], &checker), None);
    }

    #[test]
    fn test_check_all_tools_completed_without_tool_uses() {
        assert!(check_all_tools_completed(&[]));
        assert!(check_all_tools_completed(&[MessageContent::Text {
            text: "hi".to_string()
        }]));
    }
}
