use crate::session::{extract_messages, parse_all_entries, ImageBlock, MessageType};
use serde::Serialize;

/// Conversation structure
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub session_id: String,
    pub messages: Vec<ConversationMessage>,
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
pub fn get_conversation_data(session_id: &str) -> Result<Conversation, String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    let claude_projects_dir = home_dir.join(".claude").join("projects");

    let entries = std::fs::read_dir(&claude_projects_dir)
        .map_err(|e| format!("Failed to read projects directory: {}", e))?;

    let session_filename = format!("{}.jsonl", session_id);

    for entry in entries.flatten() {
        let project_path = entry.path();
        if !project_path.is_dir() {
            continue;
        }

        let session_file = project_path.join(&session_filename);
        if session_file.exists() {
            let entries = parse_all_entries(&session_file)
                .map_err(|e| format!("Failed to parse session file: {}", e))?;

            let messages = extract_messages(&entries);

            let conversation_messages: Vec<ConversationMessage> = messages
                .into_iter()
                .map(|(timestamp, msg_type, content, images)| ConversationMessage {
                    timestamp,
                    message_type: msg_type,
                    content,
                    images,
                })
                .collect();

            return Ok(Conversation {
                session_id: session_id.to_string(),
                messages: conversation_messages,
            });
        }
    }

    Err(format!(
        "Session {} not found in any project directory",
        session_id
    ))
}
