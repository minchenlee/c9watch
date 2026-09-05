use crate::session::parser::parse_all_entries_with_progress;
use crate::session::{extract_messages, ImageBlock, MessageType};
use serde::Serialize;
use std::time::{Duration, Instant};

/// Conversation structure
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub session_id: String,
    pub messages: Vec<ConversationMessage>,
}

/// Byte-level load progress for a conversation parse.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationProgress {
    pub session_id: String,
    pub bytes_read: u64,
    pub bytes_total: u64,
}

/// Individual message in a conversation
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    pub timestamp: String,
    pub message_type: MessageType,
    pub content: String,
    /// Images attached to this message (screenshots pasted by the user)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageBlock>,
}

/// Get conversation data for a session by ID.
/// Searches all project directories under ~/.claude/projects/ for the session file.
/// When `include_tools` is false, tool use/result records are omitted so large
/// Codex transcripts can open without shipping megabytes of tool dumps.
pub fn get_conversation_data(
    session_id: &str,
    include_tools: bool,
) -> Result<Conversation, String> {
    get_conversation_data_with_progress(session_id, include_tools, &mut |_, _| {})
}

/// Like [`get_conversation_data`], reporting `(bytes_read, bytes_total)` while scanning.
pub fn get_conversation_data_with_progress(
    session_id: &str,
    include_tools: bool,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Conversation, String> {
    let mut last_read = u64::MAX;
    let mut last_at = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    let mut report = |read: u64, total: u64| {
        let step = total.saturating_div(50).max(256 * 1024);
        let due = last_read == u64::MAX
            || read >= total
            || read.saturating_sub(last_read) >= step
            || last_at.elapsed() >= Duration::from_millis(80);
        if due {
            last_read = read;
            last_at = Instant::now();
            on_progress(read, total);
        }
    };

    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    let claude_projects_dir = home_dir.join(".claude").join("projects");

    let session_filename = format!("{}.jsonl", session_id);

    if let Ok(entries) = std::fs::read_dir(&claude_projects_dir) {
        for entry in entries.flatten() {
            let project_path = entry.path();
            if !project_path.is_dir() {
                continue;
            }

            let session_file = project_path.join(&session_filename);
            if session_file.exists() {
                let entries = parse_all_entries_with_progress(&session_file, &mut report)
                    .map_err(|e| format!("Failed to parse session file: {}", e))?;

                let messages = extract_messages(&entries);

                let conversation_messages: Vec<ConversationMessage> = messages
                    .into_iter()
                    .filter(|(_, msg_type, _, _)| {
                        include_tools
                            || !matches!(msg_type, MessageType::ToolUse | MessageType::ToolResult)
                    })
                    .map(
                        |(timestamp, msg_type, content, images)| ConversationMessage {
                            timestamp,
                            message_type: msg_type,
                            content,
                            images,
                        },
                    )
                    .collect();

                return Ok(Conversation {
                    session_id: session_id.to_string(),
                    messages: conversation_messages,
                });
            }
        }
    }

    if let Ok(messages) = crate::session::codex::find_codex_conversation_with_progress(
        session_id,
        include_tools,
        &mut report,
    ) {
        return Ok(Conversation {
            session_id: session_id.to_string(),
            messages: messages
                .into_iter()
                .map(|message| ConversationMessage {
                    timestamp: message.timestamp,
                    message_type: message.message_type,
                    content: message.content,
                    images: Vec::new(),
                })
                .collect(),
        });
    }

    Err(format!(
        "Session {} not found in any project directory",
        session_id
    ))
}
