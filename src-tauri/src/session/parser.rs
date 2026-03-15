use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Represents the sessions-index.json file structure
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsIndex {
    pub version: u32,
    pub entries: Vec<SessionIndexEntry>,
}

/// Individual session entry from sessions-index.json
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIndexEntry {
    pub session_id: String,
    pub full_path: PathBuf,
    pub file_mtime: u64,
    pub first_prompt: String,
    pub summary: Option<String>,
    pub message_count: u32,
    pub created: String,
    pub modified: String,
    pub git_branch: String,
    pub project_path: PathBuf,
    pub is_sidechain: bool,
}

/// A single line entry from a session JSONL file
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SessionEntry {
    User {
        #[serde(flatten)]
        base: SessionEntryBase,
        message: UserMessage,
    },
    Assistant {
        #[serde(flatten)]
        base: SessionEntryBase,
        message: AssistantMessage,
    },
    #[serde(rename = "file-history-snapshot")]
    FileHistorySnapshot {
        #[serde(rename = "messageId")]
        message_id: String,
        snapshot: serde_json::Value,
        #[serde(rename = "isSnapshotUpdate")]
        is_snapshot_update: bool,
    },
    Summary {
        summary: String,
        #[serde(rename = "leafUuid")]
        leaf_uuid: String,
    },
    #[serde(rename = "custom-title")]
    CustomTitle {
        #[serde(rename = "customTitle")]
        custom_title: String,
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    #[serde(other)]
    Unknown,
}

/// Common fields shared across session entries
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntryBase {
    pub uuid: String,
    pub timestamp: String,
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub version: Option<String>,
    pub git_branch: Option<String>,
    pub parent_uuid: Option<String>,
    pub is_sidechain: Option<bool>,
    pub slug: Option<String>,
}

/// User message structure
///
/// In Claude Code's JSONL format, user message content can be either:
/// - A plain string (for actual user prompts)
/// - An array of content blocks (for tool results sent back to Claude)
/// A base64-encoded image found in a user message content array
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImageBlock {
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserMessage {
    pub role: String,
    pub content: String,
    /// Whether this user entry is a tool result rather than an actual user prompt
    pub is_tool_result: bool,
    /// Base64-encoded images attached to this message
    pub images: Vec<ImageBlock>,
}

impl<'de> Deserialize<'de> for UserMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde_json::Value;

        let value = Value::deserialize(deserializer)?;
        let role = value
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("user")
            .to_string();

        let content_value = value.get("content");

        let mut images = Vec::new();
        let (content, is_tool_result) = match content_value {
            Some(Value::String(s)) => (s.clone(), false),
            Some(Value::Array(arr)) => {
                let mut parts = Vec::new();
                let mut has_tool_result = false;
                for item in arr {
                    match item.get("type").and_then(|t| t.as_str()) {
                        Some("tool_result") => {
                            has_tool_result = true;
                            if let Some(content) = item.get("content") {
                                match content {
                                    Value::String(s) => parts.push(s.clone()),
                                    Value::Array(inner) => {
                                        for block in inner {
                                            if let Some(text) =
                                                block.get("text").and_then(|t| t.as_str())
                                            {
                                                parts.push(text.to_string());
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Some("text") => {
                            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                parts.push(text.to_string());
                            }
                        }
                        Some("image") => {
                            if let Some(source) = item.get("source") {
                                let media_type = source
                                    .get("media_type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("image/png")
                                    .to_string();
                                if let Some(data) =
                                    source.get("data").and_then(|v| v.as_str())
                                {
                                    images.push(ImageBlock {
                                        media_type,
                                        data: data.to_string(),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let text = if parts.is_empty() && !has_tool_result {
                    String::new()
                } else if parts.is_empty() {
                    "[tool result]".to_string()
                } else {
                    parts.join("\n")
                };
                (text, has_tool_result)
            }
            _ => (String::new(), false),
        };

        Ok(UserMessage {
            role,
            content,
            is_tool_result,
            images,
        })
    }
}

/// Assistant message structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub model: String,
    pub id: String,
    pub role: String,
    pub content: Vec<MessageContent>,
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
    pub usage: Option<Usage>,
}

/// Content block within an assistant message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageContent {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: Option<bool>,
    },
    #[serde(other)]
    Unknown,
}

/// Token usage information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cache_creation_input_tokens: Option<u32>,
    pub cache_read_input_tokens: Option<u32>,
}

/// Parse a sessions-index.json file
pub fn parse_sessions_index<P: AsRef<Path>>(path: P) -> Result<SessionsIndex, String> {
    let file = File::open(path.as_ref())
        .map_err(|e| format!("Failed to open sessions-index.json: {}", e))?;

    let reader = BufReader::new(file);
    serde_json::from_reader(reader)
        .map_err(|e| format!("Failed to parse sessions-index.json: {}", e))
}

/// Read the last N lines from a JSONL file efficiently
///
/// This function uses a reverse-reading strategy to avoid loading
/// the entire file into memory for large files.
pub fn read_last_n_lines<P: AsRef<Path>>(path: P, n: usize) -> Result<Vec<String>, String> {
    let file =
        File::open(path.as_ref()).map_err(|e| format!("Failed to open JSONL file: {}", e))?;

    let metadata = file
        .metadata()
        .map_err(|e| format!("Failed to read file metadata: {}", e))?;

    let file_size = metadata.len();

    // If file is empty, return empty vec
    if file_size == 0 {
        return Ok(vec![]);
    }

    // For small files, just read everything
    if file_size < 10_000 {
        let reader = BufReader::new(file);
        let lines: Vec<String> = reader
            .lines()
            .map_while(Result::ok)
            .filter(|line| !line.trim().is_empty())
            .collect();

        let start = if lines.len() > n { lines.len() - n } else { 0 };
        return Ok(lines[start..].to_vec());
    }

    // For larger files, read from the end
    // Estimate: average line is ~1KB, so read last n*1KB + buffer
    let chunk_size = (n * 1024 * 2).min(file_size as usize);
    let mut file = file;

    file.seek(SeekFrom::End(-(chunk_size as i64)))
        .map_err(|e| format!("Failed to seek in file: {}", e))?;

    let reader = BufReader::new(file);
    let lines: Vec<String> = reader
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.trim().is_empty())
        .collect();

    let start = if lines.len() > n { lines.len() - n } else { 0 };
    Ok(lines[start..].to_vec())
}

/// Parse JSONL lines into SessionEntry structs
pub fn parse_jsonl_entries(lines: Vec<String>) -> Vec<SessionEntry> {
    lines
        .iter()
        .filter_map(|line| serde_json::from_str::<SessionEntry>(line).ok())
        .collect()
}

/// Parse the last N entries from a session JSONL file
pub fn parse_last_n_entries<P: AsRef<Path>>(
    path: P,
    n: usize,
) -> Result<Vec<SessionEntry>, String> {
    let lines = read_last_n_lines(path, n)?;
    Ok(parse_jsonl_entries(lines))
}

/// Parse all entries from a session JSONL file
pub fn parse_all_entries<P: AsRef<Path>>(path: P) -> Result<Vec<SessionEntry>, String> {
    let file =
        File::open(path.as_ref()).map_err(|e| format!("Failed to open JSONL file: {}", e))?;

    let reader = BufReader::new(file);
    let lines: Vec<String> = reader
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.trim().is_empty())
        .collect();

    Ok(parse_jsonl_entries(lines))
}

/// Known XML tag prefixes that indicate system-generated (non-user) messages.
/// Claude Code injects these into the JSONL as `type: "user"` entries.
const SYSTEM_TAG_PREFIXES: &[&str] = &[
    "<local-command-",     // /model, /bye, /rename output
    "<command-name>",      // slash command entries (/exit, /clear, /model)
    "<command-message>",   // slash command entries (alt ordering)
    "<task-notification>", // background task completion
    "<bash-input>",        // inline bash command + output
    "<bash-stdout>",       // standalone bash stdout
    "<bash-stderr>",       // standalone bash stderr
    "<result>",            // agent/subagent results
];

/// Public helper: check if content string is a system-generated message.
/// Used by both the parser and polling to skip system messages in titles/previews.
pub fn is_system_content(content: &str) -> bool {
    let trimmed = content.trim();
    SYSTEM_TAG_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// Tags whose inner text should be suppressed (not displayed in conversation preview).
const HIDDEN_TAGS: &[&str] = &[
    "local-command-caveat", // internal system disclaimer
    "bash-stderr",          // stderr noise
    "tool-use-id",          // internal ID
    "output-file",          // temp file path
    "duration_ms",          // agent stats
    "total_tokens",         // agent stats
    "tool_uses",            // agent stats
    "usage",                // agent usage block
];

/// Tags that are part of slash command entries — handled specially by `format_command_tags`.
const COMMAND_TAGS: &[&str] = &["command-name", "command-message", "command-args"];

/// Extract all tag name→value pairs from a system message string.
fn extract_tags(content: &str) -> Vec<(String, String)> {
    let mut tags = Vec::new();
    let mut remaining = content.trim();

    while !remaining.is_empty() {
        remaining = remaining.trim_start();
        if remaining.is_empty() {
            break;
        }

        if let Some(tag_start) = remaining.find('<') {
            if let Some(tag_end) = remaining[tag_start..].find('>') {
                let tag_end_abs = tag_start + tag_end + 1;
                let tag_content = &remaining[tag_start + 1..tag_start + tag_end];
                let tag_name = tag_content.split_whitespace().next().unwrap_or("");

                if tag_name.is_empty() || tag_name.starts_with('/') {
                    break;
                }

                let closing_tag = format!("</{}>", tag_name);

                if let Some(close_pos) = remaining[tag_end_abs..].find(&closing_tag) {
                    let inner = remaining[tag_end_abs..tag_end_abs + close_pos].trim();
                    let clean = strip_ansi_codes(inner);
                    tags.push((tag_name.to_string(), clean));
                    remaining = &remaining[tag_end_abs + close_pos + closing_tag.len()..];
                    continue;
                }
            }
        }
        break;
    }

    tags
}

/// Format extracted tags into a clean display string.
/// Slash commands are formatted as `/command args`. Other tags are joined with newlines.
fn format_system_tags(tags: &[(String, String)]) -> String {
    use std::fmt::Write;

    // Check if this is a slash command entry
    let has_command_tags = tags.iter().any(|(name, _)| COMMAND_TAGS.contains(&name.as_str()));

    if has_command_tags {
        // Format as: /command-name args
        let cmd_name = tags
            .iter()
            .find(|(name, _)| name == "command-name")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        let args = tags
            .iter()
            .find(|(name, _)| name == "command-args")
            .map(|(_, v)| v.trim())
            .unwrap_or("");

        if args.is_empty() {
            cmd_name.to_string()
        } else {
            format!("{} {}", cmd_name, args)
        }
    } else {
        // Generic: join non-hidden tags with newlines
        let mut result = String::new();
        for (name, value) in tags {
            if HIDDEN_TAGS.contains(&name.as_str()) || value.is_empty() {
                continue;
            }
            if !result.is_empty() {
                let _ = write!(result, "\n");
            }
            result.push_str(value);
        }
        result
    }
}

/// Strip XML tags from system messages and return clean display text.
fn clean_system_message(content: &str) -> String {
    let tags = extract_tags(content);
    format_system_tags(&tags)
}

/// Strip ANSI escape codes from a string (e.g. \x1b[1m, \x1b[22m).
fn strip_ansi_codes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip until we find a letter (the terminator of the escape sequence)
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Get all user and assistant messages from session entries.
/// Returns tuples of (timestamp, message_type, content, images).
pub fn extract_messages(
    entries: &[SessionEntry],
) -> Vec<(String, MessageType, String, Vec<ImageBlock>)> {
    let mut messages = Vec::new();

    for entry in entries {
        match entry {
            SessionEntry::User { base, message } => {
                if message.is_tool_result {
                    messages.push((
                        base.timestamp.clone(),
                        MessageType::ToolResult,
                        message.content.clone(),
                        vec![],
                    ));
                } else if is_system_content(&message.content) {
                    let cleaned = clean_system_message(&message.content);
                    if !cleaned.is_empty() {
                        messages.push((
                            base.timestamp.clone(),
                            MessageType::System,
                            cleaned,
                            vec![],
                        ));
                    }
                } else {
                    messages.push((
                        base.timestamp.clone(),
                        MessageType::User,
                        message.content.clone(),
                        message.images.clone(),
                    ));
                }
            }
            SessionEntry::Assistant { base, message } => {
                for content in &message.content {
                    match content {
                        MessageContent::Text { text } => {
                            messages.push((
                                base.timestamp.clone(),
                                MessageType::Assistant,
                                text.clone(),
                                vec![],
                            ));
                        }
                        MessageContent::Thinking { thinking, .. } => {
                            messages.push((
                                base.timestamp.clone(),
                                MessageType::Thinking,
                                thinking.clone(),
                                vec![],
                            ));
                        }
                        MessageContent::ToolUse { id, name, input } => {
                            let tool_desc = format!(
                                "[{}] {} - {}",
                                name,
                                id,
                                serde_json::to_string_pretty(input).unwrap_or_default()
                            );
                            messages.push((
                                base.timestamp.clone(),
                                MessageType::ToolUse,
                                tool_desc,
                                vec![],
                            ));
                        }
                        MessageContent::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            let result_type = if is_error.unwrap_or(false) {
                                "Error"
                            } else {
                                "Result"
                            };
                            let tool_desc =
                                format!("[{}] {}: {}", result_type, tool_use_id, content);
                            messages.push((
                                base.timestamp.clone(),
                                MessageType::ToolResult,
                                tool_desc,
                                vec![],
                            ));
                        }
                        MessageContent::Unknown => {}
                    }
                }
            }
            _ => {}
        }
    }

    messages
}

/// Message type enumeration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MessageType {
    User,
    Assistant,
    Thinking,
    ToolUse,
    ToolResult,
    System,
}

/// Extract the native custom title from Claude Code's JSONL entries.
/// Reads from the end since `/rename` re-appends metadata after compaction.
/// Returns the last `custom-title` entry's value, or None if not found.
pub fn get_native_custom_title(entries: &[SessionEntry]) -> Option<String> {
    for entry in entries.iter().rev() {
        if let SessionEntry::CustomTitle { custom_title, .. } = entry {
            return Some(custom_title.clone());
        }
    }
    None
}

/// Extract the native custom title from a JSONL file.
///
/// Scans the file for lines containing `"custom-title"` and parses only those.
/// Returns the last (most recent) custom title found.
pub fn get_native_custom_title_from_file(path: &std::path::Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);

    let mut last_title: Option<String> = None;
    for line in reader.lines().map_while(Result::ok) {
        if line.contains("\"custom-title\"") {
            if let Ok(SessionEntry::CustomTitle { custom_title, .. }) =
                serde_json::from_str::<SessionEntry>(&line)
            {
                last_title = Some(custom_title);
            }
        }
    }
    last_title
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_user_message() {
        let json = r#"{
            "type": "user",
            "uuid": "test-uuid",
            "timestamp": "2026-01-08T15:23:03.096Z",
            "sessionId": "test-session",
            "cwd": "/Users/test",
            "message": {
                "role": "user",
                "content": "Hello Claude"
            }
        }"#;

        let entry: Result<SessionEntry, _> = serde_json::from_str(json);
        assert!(entry.is_ok());

        if let Ok(SessionEntry::User { base, message }) = entry {
            assert_eq!(base.uuid, "test-uuid");
            assert_eq!(message.content, "Hello Claude");
        } else {
            panic!("Failed to parse user message");
        }
    }

    #[test]
    fn test_parse_assistant_message() {
        let json = r#"{
            "type": "assistant",
            "uuid": "test-uuid",
            "timestamp": "2026-01-08T15:23:03.096Z",
            "message": {
                "model": "claude-opus-4-5-20251101",
                "id": "msg_test",
                "role": "assistant",
                "content": [
                    {
                        "type": "text",
                        "text": "Hello user"
                    }
                ],
                "stop_reason": null,
                "stop_sequence": null
            }
        }"#;

        let entry: Result<SessionEntry, _> = serde_json::from_str(json);
        assert!(entry.is_ok());

        if let Ok(SessionEntry::Assistant { base, message }) = entry {
            assert_eq!(base.uuid, "test-uuid");
            assert_eq!(message.model, "claude-opus-4-5-20251101");
            assert_eq!(message.content.len(), 1);
        } else {
            panic!("Failed to parse assistant message");
        }
    }

    #[test]
    fn test_parse_tool_use() {
        let json = r#"{
            "type": "assistant",
            "uuid": "test-uuid",
            "timestamp": "2026-01-08T15:23:03.096Z",
            "message": {
                "model": "claude-opus-4-5-20251101",
                "id": "msg_test",
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_123",
                        "name": "Read",
                        "input": {"file_path": "/path/to/file.txt"}
                    }
                ],
                "stop_reason": "tool_use"
            }
        }"#;

        let entry: Result<SessionEntry, _> = serde_json::from_str(json);
        assert!(entry.is_ok());

        if let Ok(SessionEntry::Assistant { message, .. }) = entry {
            assert_eq!(message.content.len(), 1);
            if let MessageContent::ToolUse { id, name, .. } = &message.content[0] {
                assert_eq!(id, "toolu_123");
                assert_eq!(name, "Read");
            } else {
                panic!("Expected ToolUse content");
            }
        } else {
            panic!("Failed to parse tool use");
        }
    }

    #[test]
    fn test_parse_user_message_with_tool_result_content() {
        // In Claude Code's JSONL, tool result messages have content as an array
        let json = r#"{
            "type": "user",
            "uuid": "test-uuid",
            "timestamp": "2026-01-08T15:23:03.096Z",
            "sessionId": "test-session",
            "message": {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_123",
                        "content": "command output here"
                    }
                ]
            }
        }"#;

        let entry: Result<SessionEntry, _> = serde_json::from_str(json);
        assert!(
            entry.is_ok(),
            "Should parse user message with array content"
        );

        if let Ok(SessionEntry::User { message, .. }) = entry {
            assert!(message.content.contains("command output here"));
        } else {
            panic!("Expected User entry");
        }
    }

    #[test]
    fn test_parse_user_message_with_nested_tool_result() {
        // tool_result content can also be an array of content blocks
        let json = r#"{
            "type": "user",
            "uuid": "test-uuid",
            "timestamp": "2026-01-08T15:23:03.096Z",
            "sessionId": "test-session",
            "message": {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_456",
                        "content": [
                            {"type": "text", "text": "file contents here"}
                        ]
                    }
                ]
            }
        }"#;

        let entry: Result<SessionEntry, _> = serde_json::from_str(json);
        assert!(
            entry.is_ok(),
            "Should parse user message with nested array tool_result content"
        );

        if let Ok(SessionEntry::User { message, .. }) = entry {
            assert!(message.content.contains("file contents here"));
        } else {
            panic!("Expected User entry");
        }
    }

    #[test]
    fn test_parse_progress_entry() {
        // Progress entries should parse as Unknown (not cause errors)
        let json = r#"{
            "type": "progress",
            "uuid": "test-uuid",
            "timestamp": "2026-01-08T15:23:03.096Z",
            "data": {"type": "bash_progress"},
            "toolUseID": "toolu_123"
        }"#;

        let entry: Result<SessionEntry, _> = serde_json::from_str(json);
        assert!(entry.is_ok(), "Progress entries should parse as Unknown");
        assert!(matches!(entry.unwrap(), SessionEntry::Unknown));
    }

    fn make_base(ts: &str) -> SessionEntryBase {
        SessionEntryBase {
            uuid: "test-uuid".to_string(),
            timestamp: ts.to_string(),
            session_id: None,
            cwd: None,
            version: None,
            git_branch: None,
            parent_uuid: None,
            is_sidechain: None,
            slug: None,
        }
    }

    #[test]
    fn test_extract_messages_empty() {
        assert_eq!(extract_messages(&[]), vec![]);
    }

    #[test]
    fn test_extract_messages_user_message() {
        let entries = vec![SessionEntry::User {
            base: make_base("2026-01-01T00:00:00Z"),
            message: UserMessage {
                role: "user".to_string(),
                content: "Hello Claude".to_string(),
                is_tool_result: false,
                images: vec![],
            },
        }];
        let result = extract_messages(&entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, MessageType::User);
        assert_eq!(result[0].2, "Hello Claude");
        assert_eq!(result[0].0, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn test_extract_messages_tool_result_user_entry() {
        let entries = vec![SessionEntry::User {
            base: make_base("2026-01-01T00:00:00Z"),
            message: UserMessage {
                role: "user".to_string(),
                content: "tool output here".to_string(),
                is_tool_result: true,
                images: vec![],
            },
        }];
        let result = extract_messages(&entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, MessageType::ToolResult);
        assert_eq!(result[0].2, "tool output here");
        assert_eq!(result[0].0, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn test_extract_messages_assistant_text() {
        let entries = vec![SessionEntry::Assistant {
            base: make_base("2026-01-01T00:00:00Z"),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_1".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::Text {
                    text: "I can help with that.".to_string(),
                }],
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        let result = extract_messages(&entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, MessageType::Assistant);
        assert_eq!(result[0].2, "I can help with that.");
    }

    #[test]
    fn test_extract_messages_assistant_thinking() {
        let entries = vec![SessionEntry::Assistant {
            base: make_base("2026-01-01T00:00:00Z"),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_1".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::Thinking {
                    thinking: "Let me reason through this...".to_string(),
                    signature: None,
                }],
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            },
        }];
        let result = extract_messages(&entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, MessageType::Thinking);
        assert_eq!(result[0].2, "Let me reason through this...");
    }

    #[test]
    fn test_extract_messages_assistant_tool_use() {
        let entries = vec![SessionEntry::Assistant {
            base: make_base("2026-01-01T00:00:00Z"),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_1".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::ToolUse {
                    id: "toolu_abc".to_string(),
                    name: "Read".to_string(),
                    input: serde_json::json!({"file_path": "/test/file.txt"}),
                }],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        let result = extract_messages(&entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, MessageType::ToolUse);
        assert!(result[0].2.starts_with("[Read] toolu_abc - "));
        assert!(result[0].2.contains("file_path"));
    }

    #[test]
    fn test_extract_messages_assistant_tool_result_success() {
        let entries = vec![SessionEntry::Assistant {
            base: make_base("2026-01-01T00:00:00Z"),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_1".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::ToolResult {
                    tool_use_id: "toolu_abc".to_string(),
                    content: "file contents here".to_string(),
                    is_error: Some(false),
                }],
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            },
        }];
        let result = extract_messages(&entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, MessageType::ToolResult);
        assert_eq!(result[0].2, "[Result] toolu_abc: file contents here");
    }

    #[test]
    fn test_extract_messages_assistant_tool_result_error() {
        let entries = vec![SessionEntry::Assistant {
            base: make_base("2026-01-01T00:00:00Z"),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_1".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::ToolResult {
                    tool_use_id: "toolu_abc".to_string(),
                    content: "command not found".to_string(),
                    is_error: Some(true),
                }],
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            },
        }];
        let result = extract_messages(&entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, MessageType::ToolResult);
        assert_eq!(result[0].2, "[Error] toolu_abc: command not found");
    }

    #[test]
    fn test_extract_messages_assistant_tool_result_no_error_flag() {
        let entries = vec![SessionEntry::Assistant {
            base: make_base("2026-01-01T00:00:00Z"),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_1".to_string(),
                role: "assistant".to_string(),
                content: vec![MessageContent::ToolResult {
                    tool_use_id: "toolu_abc".to_string(),
                    content: "ok".to_string(),
                    is_error: None,
                }],
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            },
        }];
        let result = extract_messages(&entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, MessageType::ToolResult);
        assert_eq!(result[0].2, "[Result] toolu_abc: ok");
    }

    #[test]
    fn test_extract_messages_unknown_entries_skipped() {
        let entries = vec![
            SessionEntry::Unknown,
            SessionEntry::User {
                base: make_base("2026-01-01T00:00:00Z"),
                message: UserMessage {
                    role: "user".to_string(),
                    content: "hi".to_string(),
                    is_tool_result: false,
                    images: vec![],
                },
            },
            SessionEntry::Unknown,
        ];
        let result = extract_messages(&entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, MessageType::User);
    }

    #[test]
    fn test_extract_messages_mixed_content_in_one_assistant_message() {
        let entries = vec![SessionEntry::Assistant {
            base: make_base("2026-01-01T00:00:00Z"),
            message: AssistantMessage {
                model: "claude-opus-4-5-20251101".to_string(),
                id: "msg_1".to_string(),
                role: "assistant".to_string(),
                content: vec![
                    MessageContent::Text {
                        text: "Let me read that file.".to_string(),
                    },
                    MessageContent::ToolUse {
                        id: "toolu_xyz".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({}),
                    },
                ],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: None,
            },
        }];
        let result = extract_messages(&entries);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].1, MessageType::Assistant);
        assert_eq!(result[1].1, MessageType::ToolUse);
    }

    // ── System message tests ──────────────────────────────────────

    #[test]
    fn test_is_system_message_caveat() {
        assert!(is_system_content("<local-command-caveat>Caveat: ...</local-command-caveat>"));
    }

    #[test]
    fn test_is_system_message_stdout() {
        assert!(is_system_content(
            "<local-command-stdout>Set model to \x1b[1msonnet\x1b[22m</local-command-stdout>"
        ));
    }

    #[test]
    fn test_is_system_message_command_name() {
        assert!(is_system_content(
            "<command-name>/exit</command-name>\n<command-message>exit</command-message>"
        ));
    }

    #[test]
    fn test_is_system_message_task_notification() {
        assert!(is_system_content(
            "<task-notification>\n<task-id>abc</task-id>\n</task-notification>"
        ));
    }

    #[test]
    fn test_is_system_message_bash() {
        assert!(is_system_content("<bash-input>git status</bash-input>"));
        assert!(is_system_content("<bash-stdout>On branch main</bash-stdout>"));
    }

    #[test]
    fn test_is_system_message_regular_user() {
        assert!(!is_system_content("Hello Claude"));
        assert!(!is_system_content("check our backlog"));
        assert!(!is_system_content("<p>HTML but not a system tag</p>"));
    }

    #[test]
    fn test_clean_system_message_stdout_with_ansi() {
        let content =
            "<local-command-stdout>Set model to \x1b[1msonnet (claude-sonnet-4-5-20250929)\x1b[22m</local-command-stdout>";
        assert_eq!(
            clean_system_message(content),
            "Set model to sonnet (claude-sonnet-4-5-20250929)"
        );
    }

    #[test]
    fn test_clean_system_message_caveat_hidden() {
        let content = "<local-command-caveat>Caveat: DO NOT respond.</local-command-caveat>";
        assert_eq!(clean_system_message(content), "");
    }

    #[test]
    fn test_clean_system_message_caveat_plus_stdout() {
        let content = "<local-command-caveat>Caveat</local-command-caveat><local-command-stdout>Bye!</local-command-stdout>";
        assert_eq!(clean_system_message(content), "Bye!");
    }

    #[test]
    fn test_clean_system_message_command_no_args() {
        let content = "<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>";
        assert_eq!(clean_system_message(content), "/model");
    }

    #[test]
    fn test_clean_system_message_command_with_args() {
        let content = "<command-message>plan-ceo-review</command-message>\n<command-name>/plan-ceo-review</command-name>\n<command-args> \"/Users/me/docs/summary.md\"</command-args>";
        assert_eq!(
            clean_system_message(content),
            "/plan-ceo-review \"/Users/me/docs/summary.md\""
        );
    }

    #[test]
    fn test_clean_system_message_task_notification() {
        let content = "<task-notification>\n<task-id>abc</task-id>\n<status>completed</status>\n<summary>Build finished</summary>\n</task-notification>";
        let cleaned = clean_system_message(content);
        assert!(cleaned.contains("completed"));
        assert!(cleaned.contains("Build finished"));
    }

    #[test]
    fn test_extract_messages_system_stdout() {
        let entries = vec![SessionEntry::User {
            base: make_base("2026-01-01T00:00:00Z"),
            message: UserMessage {
                role: "user".to_string(),
                content: "<local-command-stdout>Session renamed to: my-task</local-command-stdout>"
                    .to_string(),
                is_tool_result: false,
                images: vec![],
            },
        }];
        let result = extract_messages(&entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, MessageType::System);
        assert_eq!(result[0].2, "Session renamed to: my-task");
    }

    #[test]
    fn test_extract_messages_caveat_only_hidden() {
        let entries = vec![SessionEntry::User {
            base: make_base("2026-01-01T00:00:00Z"),
            message: UserMessage {
                role: "user".to_string(),
                content: "<local-command-caveat>Caveat: DO NOT respond to these messages.</local-command-caveat>".to_string(),
                is_tool_result: false,
                images: vec![],
            },
        }];
        let result = extract_messages(&entries);
        assert_eq!(result.len(), 0, "Caveat-only messages should be hidden");
    }

    #[test]
    fn test_extract_messages_command_name_hidden() {
        // /exit command produces empty cleaned content (command-args is hidden)
        let entries = vec![SessionEntry::User {
            base: make_base("2026-01-01T00:00:00Z"),
            message: UserMessage {
                role: "user".to_string(),
                content: "<command-name>/exit</command-name>\n<command-message>exit</command-message>\n<command-args></command-args>".to_string(),
                is_tool_result: false,
                images: vec![],
            },
        }];
        let result = extract_messages(&entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, MessageType::System);
        assert!(result[0].2.contains("/exit"));
    }

    // ── Custom title tests ────────────────────────────────────────

    #[test]
    fn test_parse_custom_title_entry() {
        let json = r#"{"type":"custom-title","customTitle":"my-task","sessionId":"abc-123"}"#;
        let entry: Result<SessionEntry, _> = serde_json::from_str(json);
        assert!(entry.is_ok());
        if let Ok(SessionEntry::CustomTitle {
            custom_title,
            session_id,
        }) = entry
        {
            assert_eq!(custom_title, "my-task");
            assert_eq!(session_id, "abc-123");
        } else {
            panic!("Expected CustomTitle entry");
        }
    }

    #[test]
    fn test_get_native_custom_title_found() {
        let entries = vec![
            SessionEntry::Unknown,
            SessionEntry::CustomTitle {
                custom_title: "old-name".to_string(),
                session_id: "s1".to_string(),
            },
            SessionEntry::Unknown,
            SessionEntry::CustomTitle {
                custom_title: "latest-name".to_string(),
                session_id: "s1".to_string(),
            },
        ];
        // Should return the last one (reading from end)
        assert_eq!(
            get_native_custom_title(&entries),
            Some("latest-name".to_string())
        );
    }

    #[test]
    fn test_get_native_custom_title_not_found() {
        let entries = vec![SessionEntry::Unknown];
        assert_eq!(get_native_custom_title(&entries), None);
    }
}
