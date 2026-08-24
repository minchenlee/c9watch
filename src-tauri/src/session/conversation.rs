use crate::session::SessionProvider;
use crate::session::{extract_messages, parse_all_entries, ImageBlock, MessageType};
use serde::Serialize;

/// Conversation structure
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub session_id: String,
    pub provider: SessionProvider,
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

fn claude_conversation(session_id: &str) -> Result<Conversation, String> {
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
                let entries = parse_all_entries(&session_file)
                    .map_err(|e| format!("Failed to parse session file: {}", e))?;

                let messages = extract_messages(&entries);

                let conversation_messages: Vec<ConversationMessage> = messages
                    .into_iter()
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
                    provider: SessionProvider::ClaudeCode,
                    messages: conversation_messages,
                });
            }
        }
    }

    Err(format!(
        "Claude Code session {} not found in any project directory",
        session_id
    ))
}

fn codex_conversation(session_id: &str) -> Result<Conversation, String> {
    let messages = crate::session::codex::find_codex_conversation(session_id)?;
    Ok(Conversation {
        session_id: session_id.to_string(),
        provider: SessionProvider::Codex,
        messages: messages
            .into_iter()
            .map(|message| ConversationMessage {
                timestamp: message.timestamp,
                message_type: message.message_type,
                content: message.content,
                images: Vec::new(),
            })
            .collect(),
    })
}

fn cursor_conversation(session_id: &str) -> Result<Conversation, String> {
    let messages = crate::session::cursor::find_cursor_conversation_with_progress(
        session_id,
        true,
        &mut |_, _| {},
    )?;
    Ok(Conversation {
        session_id: session_id.to_string(),
        provider: SessionProvider::Cursor,
        messages: messages
            .into_iter()
            .map(|message| ConversationMessage {
                timestamp: message.timestamp,
                message_type: message.message_type,
                content: message.content,
                images: Vec::new(),
            })
            .collect(),
    })
}

fn select_provider_conversation(
    session_id: &str,
    mut matches: Vec<(SessionProvider, Conversation)>,
) -> Result<Conversation, String> {
    match matches.len() {
        0 => Err(format!("Session {} not found in any provider", session_id)),
        1 => Ok(matches.pop().unwrap().1),
        _ => Err(format!(
            "Session {} is ambiguous across providers; pass provider explicitly",
            session_id
        )),
    }
}

/// Get conversation data by provider-scoped identity when available.
///
/// The provider argument is optional for backward compatibility with older
/// clients. New clients should pass it so an opaque ID cannot select a
/// transcript from the wrong provider namespace.
pub fn get_conversation_data_for_provider(
    session_id: &str,
    provider: Option<SessionProvider>,
) -> Result<Conversation, String> {
    if let Some(provider) = provider {
        return match provider {
            SessionProvider::ClaudeCode => claude_conversation(session_id),
            SessionProvider::Codex => codex_conversation(session_id),
            SessionProvider::Cursor => cursor_conversation(session_id),
        };
    }

    let mut matches = Vec::new();
    for (provider, loader) in [
        (
            SessionProvider::ClaudeCode,
            claude_conversation as fn(&str) -> Result<Conversation, String>,
        ),
        (SessionProvider::Codex, codex_conversation),
        (SessionProvider::Cursor, cursor_conversation),
    ] {
        if let Ok(conversation) = loader(session_id) {
            matches.push((provider, conversation));
        }
    }

    select_provider_conversation(session_id, matches)
}

/// Backward-compatible ID-only lookup for CLI callers and old clients.
pub fn get_conversation_data(session_id: &str) -> Result<Conversation, String> {
    get_conversation_data_for_provider(session_id, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation(session_id: &str) -> Conversation {
        Conversation {
            session_id: session_id.to_string(),
            provider: SessionProvider::ClaudeCode,
            messages: Vec::new(),
        }
    }

    #[test]
    fn providerless_conversation_lookup_rejects_cross_provider_collision() {
        let error = select_provider_conversation(
            "same-id",
            vec![
                (SessionProvider::ClaudeCode, conversation("same-id")),
                (SessionProvider::Codex, conversation("same-id")),
            ],
        )
        .unwrap_err();

        assert!(error.contains("ambiguous across providers"));
        assert!(error.contains("pass provider explicitly"));
    }

    #[test]
    fn providerless_conversation_lookup_accepts_one_provider() {
        let result = select_provider_conversation(
            "only-id",
            vec![(SessionProvider::Cursor, conversation("only-id"))],
        )
        .unwrap();

        assert_eq!(result.session_id, "only-id");
    }
}
