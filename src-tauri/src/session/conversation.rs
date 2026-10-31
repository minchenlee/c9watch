use crate::session::parser::parse_all_entries_with_progress;
use crate::session::{extract_messages, ImageBlock, MessageType};
use crate::session::{find_claude_session_file_under, SessionProvider};
use serde::Serialize;
use std::path::Path;
use std::time::{Duration, Instant};

/// Conversation structure
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub session_id: String,
    pub provider: SessionProvider,
    pub messages: Vec<ConversationMessage>,
}

/// Byte-level load progress for a conversation parse.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationProgress {
    pub session_id: String,
    pub provider: Option<SessionProvider>,
    /// WS request correlation id; omitted for Tauri events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<u64>,
    pub bytes_read: u64,
    pub bytes_total: u64,
}

/// Individual message in a conversation
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    pub timestamp: String,
    pub message_type: MessageType,
    pub content: String,
    /// Images attached to this message (screenshots pasted by the user)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageBlock>,
    /// The engine replaced `content` with a short note (and dropped
    /// `images`): the message alone was larger than a c9p page may be.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

fn claude_conversation(
    home_dir: &Path,
    session_id: &str,
    include_tools: bool,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Conversation, String> {
    let Some(session_file) = find_claude_session_file_under(home_dir, session_id) else {
        return Err(format!(
            "Claude Code session {} not found in any project directory",
            session_id
        ));
    };

    let entries = parse_all_entries_with_progress(&session_file, on_progress)
        .map_err(|e| format!("Failed to parse session file: {}", e))?;
    let messages = extract_messages(&entries);
    let conversation_messages = messages
        .into_iter()
        .filter(|(_, msg_type, _, _)| {
            include_tools || !matches!(msg_type, MessageType::ToolUse | MessageType::ToolResult)
        })
        .map(
            |(timestamp, msg_type, content, images)| ConversationMessage {
                timestamp,
                message_type: msg_type,
                content,
                images,
                truncated: false,
            },
        )
        .collect();

    Ok(Conversation {
        session_id: session_id.to_string(),
        provider: SessionProvider::ClaudeCode,
        messages: conversation_messages,
    })
}

fn codex_conversation(
    session_id: &str,
    include_tools: bool,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Conversation, String> {
    let messages = crate::session::codex::find_codex_conversation_with_progress(
        session_id,
        include_tools,
        on_progress,
    )?;
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
                truncated: false,
            })
            .collect(),
    })
}

fn cursor_conversation(
    session_id: &str,
    include_tools: bool,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Conversation, String> {
    let messages = crate::session::cursor::find_cursor_conversation_with_progress(
        session_id,
        include_tools,
        on_progress,
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
                truncated: false,
            })
            .collect(),
    })
}

fn pi_conversation(
    session_id: &str,
    include_tools: bool,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Conversation, String> {
    // pi transcripts are parsed in one pass; report completion for parity
    // with the progress-reporting providers.
    on_progress(0, 1);
    let messages = crate::session::pi::read_pi_conversation_with_tools(session_id, include_tools)?;
    on_progress(1, 1);
    Ok(Conversation {
        session_id: session_id.to_string(),
        provider: SessionProvider::Pi,
        messages: messages
            .into_iter()
            .map(|message| ConversationMessage {
                timestamp: message.timestamp,
                message_type: message.message_type,
                content: message.content,
                images: Vec::new(),
                truncated: false,
            })
            .collect(),
    })
}

fn load_for_provider(
    home_dir: &Path,
    session_id: &str,
    provider: SessionProvider,
    include_tools: bool,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Conversation, String> {
    match provider {
        SessionProvider::ClaudeCode => {
            claude_conversation(home_dir, session_id, include_tools, on_progress)
        }
        SessionProvider::Codex => codex_conversation(session_id, include_tools, on_progress),
        SessionProvider::Cursor => cursor_conversation(session_id, include_tools, on_progress),
        SessionProvider::Pi => pi_conversation(session_id, include_tools, on_progress),
        SessionProvider::Opencode => super::opencode::conversation(session_id, include_tools),
    }
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
pub fn get_conversation_data_for_provider_with_progress(
    session_id: &str,
    provider: Option<SessionProvider>,
    include_tools: bool,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Conversation, String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    get_conversation_data_in(&home_dir, session_id, provider, include_tools, on_progress)
}

/// `get_conversation_data_for_provider_with_progress` with Claude Code
/// transcripts read under the user home `home_dir`. The other providers
/// read their own stores, as there.
pub fn get_conversation_data_in(
    home_dir: &Path,
    session_id: &str,
    provider: Option<SessionProvider>,
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

    if let Some(provider) = provider {
        return load_for_provider(home_dir, session_id, provider, include_tools, &mut report);
    }

    probe_providerless_conversation(session_id, |provider| {
        load_for_provider(home_dir, session_id, provider, include_tools, &mut report)
    })
}

fn probe_providerless_conversation(
    session_id: &str,
    mut load: impl FnMut(SessionProvider) -> Result<Conversation, String>,
) -> Result<Conversation, String> {
    let mut matches = Vec::new();
    for provider in [
        SessionProvider::ClaudeCode,
        SessionProvider::Codex,
        SessionProvider::Cursor,
        SessionProvider::Pi,
        SessionProvider::Opencode,
    ] {
        if let Ok(conversation) = load(provider) {
            matches.push((provider, conversation));
        }
    }
    select_provider_conversation(session_id, matches)
}

/// Get conversation data with progress reporting and optional tool records.
pub fn get_conversation_data_with_progress(
    session_id: &str,
    include_tools: bool,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Conversation, String> {
    get_conversation_data_for_provider_with_progress(session_id, None, include_tools, on_progress)
}

/// Backward-compatible provider-aware lookup. Existing callers receive the
/// complete conversation, including tool records.
pub fn get_conversation_data_for_provider(
    session_id: &str,
    provider: Option<SessionProvider>,
) -> Result<Conversation, String> {
    get_conversation_data_for_provider_with_progress(session_id, provider, true, &mut |_, _| {})
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
    fn providerless_probe_finds_opencode_and_still_rejects_collisions() {
        let load = |provider| {
            if provider == SessionProvider::Opencode {
                Ok(Conversation {
                    provider,
                    ..conversation("remote-id")
                })
            } else {
                Err("not found".into())
            }
        };
        let result = probe_providerless_conversation("remote-id", load).unwrap();
        assert_eq!(result.provider, SessionProvider::Opencode);
        let error = probe_providerless_conversation("remote-id", |provider| {
            if provider == SessionProvider::Pi {
                Ok(Conversation {
                    provider,
                    ..conversation("remote-id")
                })
            } else {
                load(provider)
            }
        })
        .unwrap_err();
        assert!(error.contains("ambiguous across providers"));
    }

    #[test]
    fn providerless_conversation_lookup_rejects_cross_provider_collision() {
        let error = select_provider_conversation(
            "same-id",
            vec![
                (SessionProvider::ClaudeCode, conversation("same-id")),
                (SessionProvider::Codex, conversation("same-id")),
                (SessionProvider::Pi, conversation("same-id")),
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
