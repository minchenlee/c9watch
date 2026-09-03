//! pi agent session detection.
//!
//! pi (the agent running this session) persists one transcript per session at
//! `~/.pi/agent/sessions/<encoded-cwd>/<timestamp>_<session-id>.jsonl`.
//!
//! Transcript line protocol (JSON per line, schema-tolerant parsing):
//! - `{"type":"session","session":{"id","timestamp"(ms),"cwd"}}` — header, first line.
//! - `{"type":"message","message":{"role":"user","content":[{"type":"text","text"}]}}`
//! - `{"type":"message","message":{"role":"assistant","model","content":[text|thinking|toolCall],"stopReason","usage":{...}}}`
//! - `{"type":"message","message":{"role":"toolResult","toolCallId","toolName","content","isError"}}`
//! - ledger lines (`model_change`, `compaction`, `custom`, `thinking_level_change`) carry
//!   no conversation state and are ignored.
//!
//! pi exposes no turn lifecycle events and no process-anchored identity (every
//! session is driven by a shared `pi` binary), so liveness is file-based like
//! the Cursor provider: message timestamps + file mtime drive Working/Idle,
//! and the pid is reported as 0.

use super::source::{DetectedSession, DetectionDiagnostics, SessionDetectorError, SessionSource};
use super::{AgentKind, SessionKind, SessionProvider, SessionSurface};
use crate::session::parser::MessageType;
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Idle pi transcripts older than this are treated as expired.
const PI_FRESHNESS_IDLE_SECS: u64 = 30 * 60;
/// Working pi transcripts older than this are treated as expired.
const PI_FRESHNESS_WORKING_SECS: u64 = 4 * 60 * 60;
/// A transcript written within this window counts as actively generating.
const PI_WORKING_RECENCY_SECS: i64 = 120;
/// Cap stored tool-result payloads so one dump cannot bloat conversation views.
const PI_MAX_TOOL_RESULT_CHARS: usize = 2000;
/// Cap stored tool-call argument previews.
const PI_MAX_TOOL_ARGS_CHARS: usize = 300;

// ── Summary ─────────────────────────────────────────────────────────

/// Rolled-up state for one pi transcript, mirroring CodexRolloutSummary's role.
#[derive(Debug, Clone, Default)]
pub struct PiTranscriptSummary {
    pub session_id: String,
    pub project_path: String,
    pub started_at_ms: Option<i64>,
    pub last_timestamp: String,
    pub first_prompt: Option<String>,
    pub latest_snippet: Option<String>,
    pub message_count: usize,
    pub lifecycle: PiLifecycle,
    pub empty: bool,
    pub pending_tool_name: Option<String>,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_cost: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PiLifecycle {
    Working,
    #[default]
    Idle,
}

/// One flattened conversation row for the conversation view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiConversationMessage {
    pub timestamp: String,
    pub message_type: MessageType,
    pub content: String,
}

// ── Source ──────────────────────────────────────────────────────────

pub struct PiSessionSource {
    root: PathBuf,
    cache: HashMap<PathBuf, CachedSummary>,
}

struct CachedSummary {
    len: u64,
    modified_nanos: u128,
    summary: PiTranscriptSummary,
}

impl PiSessionSource {
    pub fn new() -> Result<Self, SessionDetectorError> {
        let home = dirs::home_dir().ok_or(SessionDetectorError::HomeDirectoryNotFound)?;
        Ok(Self::at_root(pi_sessions_root(&home)))
    }

    pub(crate) fn at_root(root: PathBuf) -> Self {
        Self {
            root,
            cache: HashMap::new(),
        }
    }

    pub(crate) fn contains_session_id(&self, session_id: &str) -> bool {
        let Ok(dirs) = std::fs::read_dir(&self.root) else {
            return false;
        };
        for dir_entry in dirs.flatten() {
            let dir = dir_entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            for file_entry in files.flatten() {
                let path = file_entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                if pi_session_id_from_filename(&path).as_deref() == Some(session_id) {
                    return true;
                }
            }
        }
        false
    }

    fn summary_for(&mut self, path: &Path) -> Option<PiTranscriptSummary> {
        let metadata = std::fs::metadata(path).ok()?;
        let len = metadata.len();
        let modified_nanos = metadata
            .modified()
            .ok()?
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        if let Some(cached) = self.cache.get(path) {
            if cached.len == len && cached.modified_nanos == modified_nanos {
                return Some(cached.summary.clone());
            }
        }
        let summary = summarize_pi_transcript(path)?;
        self.cache.insert(
            path.to_path_buf(),
            CachedSummary {
                len,
                modified_nanos,
                summary: summary.clone(),
            },
        );
        Some(summary)
    }
}

impl SessionSource for PiSessionSource {
    fn detect(
        &mut self,
    ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
        let mut sessions = Vec::new();
        let Ok(dirs) = std::fs::read_dir(&self.root) else {
            return Ok((sessions, DetectionDiagnostics::default()));
        };
        let now = SystemTime::now();
        for dir_entry in dirs.flatten() {
            let dir = dir_entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            for file_entry in files.flatten() {
                let path = file_entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(summary) = self.summary_for(&path) else {
                    continue;
                };
                if summary.session_id.is_empty() {
                    continue;
                }
                // Freshness gate, mirroring the Cursor provider: idle
                // transcripts expire fast, working ones linger.
                let age_secs = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| now.duration_since(t).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(u64::MAX);
                let fresh = match summary.lifecycle {
                    PiLifecycle::Working => age_secs < PI_FRESHNESS_WORKING_SECS,
                    PiLifecycle::Idle => age_secs < PI_FRESHNESS_IDLE_SECS,
                };
                if !fresh {
                    continue;
                }
                let project_path = if summary.project_path.is_empty() {
                    decode_pi_cwd_dir(&dir)
                } else {
                    summary.project_path.clone()
                };
                let project_name = Path::new(&project_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                sessions.push(DetectedSession {
                    pid: 0,
                    cwd: PathBuf::from(&project_path),
                    project_path: PathBuf::from(&project_path),
                    session_id: Some(summary.session_id.clone()),
                    project_name,
                    kind: SessionKind::Interactive,
                    started_at_ms: summary.started_at_ms,
                    official_name: None,
                    cli_activity: None,
                    provider: SessionProvider::Pi,
                    surface: SessionSurface::Cli,
                    agent_kind: AgentKind::Root,
                    parent_thread_id: None,
                    root_session_id: None,
                    agent_path: None,
                    agent_nickname: None,
                    agent_role: None,
                    internal_kind: None,
                    can_open: false,
                    can_stop: false,
                    can_rename: false,
                    codex_summary: None,
                    cursor_summary: None,
                    pi_summary: Some(summary),
                });
            }
        }
        Ok((sessions, DetectionDiagnostics::default()))
    }

    fn backend_name(&self) -> &'static str {
        "pi"
    }
}

pub(crate) fn pi_sessions_root(home: &Path) -> PathBuf {
    home.join(".pi").join("agent").join("sessions")
}

/// Best-effort decode of pi's encoded cwd directory name (`--Users-foo-bar`
/// for `/Users/foo/bar`). Only a fallback: the session header's `cwd` wins.
fn decode_pi_cwd_dir(dir: &Path) -> String {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let stripped = name.strip_prefix('-').unwrap_or(name);
    let decoded = stripped.replace('-', "/");
    if decoded.starts_with('/') {
        decoded
    } else {
        format!("/{decoded}")
    }
}

/// Session id is the filename suffix after the last `_` (`<ts>_<uuid>.jsonl`).
fn pi_session_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let id = stem.rsplit('_').next()?.trim();
    if id.is_empty() {
        return None;
    }
    Some(id.to_string())
}

// ── Transcript parsing ──────────────────────────────────────────────

/// Parse a pi transcript into a rollup summary. Returns `None` when the file
/// has no usable session header (not a pi transcript).
pub(crate) fn summarize_pi_transcript(path: &Path) -> Option<PiTranscriptSummary> {
    let content = std::fs::read_to_string(path).ok()?;
    let session_id = pi_session_id_from_filename(path)?;
    let mut summary = PiTranscriptSummary {
        session_id,
        ..PiTranscriptSummary::default()
    };
    let mut pending_tools: Vec<(String, String)> = Vec::new();
    let mut completed_tools: HashSet<String> = HashSet::new();
    let mut last_role: Option<String> = None;
    let mut last_message_ts: Option<String> = None;
    let mut header_cwd: Option<String> = None;
    let mut header_ts: Option<i64> = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match value.get("type").and_then(|t| t.as_str()) {
            Some("session") => {
                if let Some(session) = value.get("session") {
                    if let Some(cwd) = session.get("cwd").and_then(|c| c.as_str()) {
                        header_cwd = Some(cwd.to_string());
                    }
                    if let Some(ts) = session.get("timestamp").and_then(|t| t.as_i64()) {
                        header_ts = Some(ts);
                    }
                } else {
                    // Older pi transcripts keep the header flat:
                    // {"type":"session","cwd":"...","timestamp":"..."}.
                    if header_cwd.is_none() {
                        if let Some(cwd) = value.get("cwd").and_then(|c| c.as_str()) {
                            header_cwd = Some(cwd.to_string());
                        }
                    }
                    if header_ts.is_none() {
                        header_ts = value
                            .get("timestamp")
                            .and_then(parse_pi_timestamp_ms);
                    }
                }
            }
            Some("message") => {
                let Some(message) = value.get("message") else {
                    continue;
                };
                let role = message
                    .get("role")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string();
                let ts = value
                    .get("timestamp")
                    .and_then(|t| t.as_i64())
                    .map(millis_to_rfc3339)
                    .or_else(|| {
                        value
                            .get("timestamp")
                            .and_then(|t| t.as_str())
                            .map(str::to_string)
                    });
                match role.as_str() {
                    "user" => {
                        summary.message_count += 1;
                        last_role = Some(role);
                        if let Some(t) = ts.clone() {
                            last_message_ts = Some(t.clone());
                            summary.last_timestamp = t;
                        }
                        for text in message_texts(message) {
                            if summary.first_prompt.is_none() && !text.trim().is_empty() {
                                summary.first_prompt = Some(text.clone());
                            }
                            summary.latest_snippet = Some(text);
                        }
                    }
                    "assistant" => {
                        summary.message_count += 1;
                        last_role = Some(role);
                        if let Some(t) = ts.clone() {
                            last_message_ts = Some(t.clone());
                            summary.last_timestamp = t;
                        }
                        if summary.model.is_none() {
                            summary.model = message
                                .get("model")
                                .and_then(|m| m.as_str())
                                .map(str::to_string);
                        }
                        apply_pi_usage(&mut summary, message);
                        let blocks = message
                            .get("content")
                            .and_then(|c| c.as_array())
                            .cloned()
                            .unwrap_or_default();
                        for block in &blocks {
                            match block.get("type").and_then(|t| t.as_str()) {
                                Some("text") => {
                                    if let Some(text) =
                                        block.get("text").and_then(|t| t.as_str())
                                    {
                                        if !text.trim().is_empty() {
                                            summary.latest_snippet = Some(text.to_string());
                                        }
                                    }
                                }
                                Some("toolCall") => {
                                    let id = block
                                        .get("id")
                                        .and_then(|i| i.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let name = block
                                        .get("name")
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("tool")
                                        .to_string();
                                    if !id.is_empty() && !completed_tools.contains(&id) {
                                        pending_tools.push((id, name));
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    "toolResult" => {
                        last_role = Some(role);
                        if let Some(t) = ts.clone() {
                            last_message_ts = Some(t.clone());
                            summary.last_timestamp = t;
                        }
                        if let Some(id) =
                            message.get("toolCallId").and_then(|i| i.as_str())
                        {
                            completed_tools.insert(id.to_string());
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    if header_cwd.is_none() && header_ts.is_none() && summary.message_count == 0 {
        return None;
    }
    summary.project_path = header_cwd.unwrap_or_default();
    summary.started_at_ms = header_ts;
    summary.empty = summary.message_count == 0;

    pending_tools.retain(|(id, _)| !completed_tools.contains(id));
    summary.pending_tool_name = pending_tools.last().map(|(_, name)| name.clone());

    // Liveness mirrors the Cursor provider: pending tool calls and trailing
    // user messages mean the agent owns the turn; otherwise file recency
    // decides whether output is still streaming.
    let mtime_recent = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|d| d.as_secs() < PI_WORKING_RECENCY_SECS as u64);
    summary.lifecycle = if !pending_tools.is_empty() {
        PiLifecycle::Working
    } else if last_role.as_deref() == Some("user") {
        PiLifecycle::Working
    } else if last_message_ts.is_some() && mtime_recent {
        PiLifecycle::Working
    } else {
        PiLifecycle::Idle
    };
    Some(summary)
}

/// Plain-text blocks of a user message.
fn message_texts(message: &serde_json::Value) -> Vec<String> {
    let mut texts = Vec::new();
    if let Some(content) = message.get("content") {
        if let Some(text) = content.as_str() {
            texts.push(text.to_string());
        } else if let Some(blocks) = content.as_array() {
            for block in blocks {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        texts.push(text.to_string());
                    }
                }
            }
        }
    }
    texts
}

/// Fold an assistant message's `usage` ledger into the summary totals. pi
/// reports real per-message USD cost, so no price table is needed.
fn apply_pi_usage(summary: &mut PiTranscriptSummary, message: &serde_json::Value) {
    let Some(usage) = message.get("usage") else {
        return;
    };
    summary.input_tokens += usage
        .get("input")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    summary.output_tokens += usage
        .get("output")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    summary.cached_input_tokens += usage
        .get("cacheRead")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        + usage
            .get("cacheWrite")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
    summary.reasoning_tokens += usage
        .get("reasoning")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if let Some(cost) = usage.get("cost").and_then(|c| c.get("total")) {
        summary.total_cost += cost.as_f64().unwrap_or(0.0);
    }
}

fn millis_to_rfc3339(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

/// Epoch-ms, RFC3339 string, or float seconds — whatever the transcript uses.
fn parse_pi_timestamp_ms(value: &serde_json::Value) -> Option<i64> {
    if let Some(ms) = value.as_i64() {
        return Some(ms);
    }
    if let Some(secs) = value.as_f64() {
        return Some((secs * 1000.0) as i64);
    }
    value.as_str().and_then(|s| {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.timestamp_millis())
    })
}

/// toolResult bodies are usually strings, but older transcripts wrap them in
/// `[{type:text, text}]` blocks. Return the concatenated text either way.
fn tool_result_text(message: &serde_json::Value) -> String {
    if let Some(content) = message.get("content") {
        if let Some(text) = content.as_str() {
            return text.to_string();
        }
        if let Some(blocks) = content.as_array() {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    parts.push(text.to_string());
                }
            }
            if !parts.is_empty() {
                return parts.join("\n");
            }
        }
    }
    String::new()
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

// ── Conversation ────────────────────────────────────────────────────

/// Full conversation rows for one pi session, oldest first.
pub(crate) fn read_pi_conversation(session_id: &str) -> Result<Vec<PiConversationMessage>, String> {
    let home = dirs::home_dir().ok_or("Failed to get home directory")?;
    read_pi_conversation_under(&home, session_id)
}

pub(crate) fn read_pi_conversation_under(
    home: &Path,
    session_id: &str,
) -> Result<Vec<PiConversationMessage>, String> {
    let path = pi_conversation_path_for_session(&home, session_id)
        .ok_or_else(|| format!("pi session {session_id} not found"))?;
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read pi transcript: {e}"))?;
    let mut messages = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        let timestamp = value
            .get("timestamp")
            .and_then(|t| t.as_i64())
            .map(millis_to_rfc3339)
            .or_else(|| {
                value
                    .get("timestamp")
                    .and_then(|t| t.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        match message.get("role").and_then(|r| r.as_str()) {
            Some("user") => {
                for text in message_texts(message) {
                    if text.trim().is_empty() {
                        continue;
                    }
                    messages.push(PiConversationMessage {
                        timestamp: timestamp.clone(),
                        message_type: MessageType::User,
                        content: text,
                    });
                }
            }
            Some("assistant") => {
                let blocks = message
                    .get("content")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default();
                for block in &blocks {
                    match block.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                if text.trim().is_empty() {
                                    continue;
                                }
                                messages.push(PiConversationMessage {
                                    timestamp: timestamp.clone(),
                                    message_type: MessageType::Assistant,
                                    content: text.to_string(),
                                });
                            }
                        }
                        Some("thinking") => {
                            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                if text.trim().is_empty() {
                                    continue;
                                }
                                messages.push(PiConversationMessage {
                                    timestamp: timestamp.clone(),
                                    message_type: MessageType::Thinking,
                                    content: text.to_string(),
                                });
                            }
                        }
                        Some("toolCall") => {
                            let name = block
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("tool");
                            let args = block
                                .get("arguments")
                                .map(|a| {
                                    truncate_chars(
                                        &serde_json::to_string(a).unwrap_or_default(),
                                        PI_MAX_TOOL_ARGS_CHARS,
                                    )
                                })
                                .unwrap_or_default();
                            messages.push(PiConversationMessage {
                                timestamp: timestamp.clone(),
                                message_type: MessageType::ToolUse,
                                content: if args.is_empty() || args == "null" {
                                    name.to_string()
                                } else {
                                    format!("{name} {args}")
                                },
                            });
                        }
                        _ => {}
                    }
                }
            }
            Some("toolResult") => {
                let name = message
                    .get("toolName")
                    .and_then(|n| n.as_str())
                    .unwrap_or("tool");
                let body = tool_result_text(message);
                let is_error = message
                    .get("isError")
                    .and_then(|e| e.as_bool())
                    .unwrap_or(false);
                messages.push(PiConversationMessage {
                    timestamp: timestamp.clone(),
                    message_type: MessageType::ToolResult,
                    content: if is_error {
                        format!(
                            "{name} failed: {}",
                            truncate_chars(&body, PI_MAX_TOOL_RESULT_CHARS)
                        )
                    } else {
                        truncate_chars(&body, PI_MAX_TOOL_RESULT_CHARS)
                    },
                });
            }
            _ => {}
        }
    }
    Ok(messages)
}

pub(crate) fn pi_conversation_path_for_session(
    home: &Path,
    session_id: &str,
) -> Option<PathBuf> {
    let root = pi_sessions_root(home);
    let dirs = std::fs::read_dir(&root).ok()?;
    for dir_entry in dirs.flatten() {
        let dir = dir_entry.path();
        if !dir.is_dir() {
            continue;
        }
        let files = std::fs::read_dir(&dir).ok()?;
        for file_entry in files.flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if pi_session_id_from_filename(&path).as_deref() == Some(session_id) {
                return Some(path);
            }
        }
    }
    None
}

pub(crate) fn pi_session_ids(home: &Path, prefix: &str) -> Vec<String> {
    let root = pi_sessions_root(home);
    let mut ids = Vec::new();
    let Ok(dirs) = std::fs::read_dir(&root) else {
        return ids;
    };
    for dir_entry in dirs.flatten() {
        let dir = dir_entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for file_entry in files.flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(id) = pi_session_id_from_filename(&path) {
                if id.starts_with(prefix) && !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    ids
}

/// `(project_path, modified)` for search-hit enrichment.
pub(crate) fn pi_search_metadata(
    home: &Path,
    session_id: &str,
) -> (Option<String>, Option<String>) {
    let Some(path) = pi_conversation_path_for_session(home, session_id) else {
        return (None, None);
    };
    let project_path = summarize_pi_transcript(&path)
        .map(|summary| summary.project_path)
        .filter(|p| !p.is_empty());
    let modified = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| DateTime::<chrono::Utc>::from(t).to_rfc3339());
    (project_path, modified)
}

// ── History / search / cost ─────────────────────────────────────────

pub(crate) fn pi_history_entries(home: &Path) -> Vec<crate::session::history::HistoryEntry> {
    let root = pi_sessions_root(home);
    let mut entries = Vec::new();
    let Ok(dirs) = std::fs::read_dir(&root) else {
        return entries;
    };
    for dir_entry in dirs.flatten() {
        let dir = dir_entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for file_entry in files.flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(summary) = summarize_pi_transcript(&path) else {
                continue;
            };
            if summary.session_id.is_empty() {
                continue;
            }
            let project = if summary.project_path.is_empty() {
                decode_pi_cwd_dir(&dir)
            } else {
                summary.project_path.clone()
            };
            let project_name = Path::new(&project)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            entries.push(crate::session::history::HistoryEntry {
                session_id: summary.session_id,
                display: summary.first_prompt.unwrap_or_default(),
                timestamp: summary.started_at_ms.unwrap_or(0).max(0) as u64,
                project,
                project_name,
                custom_title: None,
                codex_title: None,
                cursor_title: None,
                provider: "pi".to_string(),
                surface: Some("cli".to_string()),
                agent_kind: Some("root".to_string()),
            });
        }
    }
    entries
}

pub(crate) fn pi_deep_search(
    home: &Path,
    query: &str,
    case_sensitive: bool,
    whole_word: bool,
) -> Vec<crate::session::history::DeepSearchHit> {
    let needle = if case_sensitive {
        query.to_string()
    } else {
        query.to_lowercase()
    };
    let root = pi_sessions_root(home);
    let mut hits = Vec::new();
    let Ok(dirs) = std::fs::read_dir(&root) else {
        return hits;
    };
    for dir_entry in dirs.flatten() {
        let dir = dir_entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for file_entry in files.flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut snippet: Option<String> = None;
            for line in content.lines() {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if value.get("type").and_then(|t| t.as_str()) != Some("message") {
                    continue;
                }
                let Some(message) = value.get("message") else {
                    continue;
                };
                let mut candidates = message_texts(message);
                if message.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                    if let Some(blocks) = message.get("content").and_then(|c| c.as_array()) {
                        for block in blocks {
                            match block.get("type").and_then(|t| t.as_str()) {
                                Some("text") | Some("thinking") => {
                                    if let Some(text) =
                                        block.get("text").and_then(|t| t.as_str())
                                    {
                                        candidates.push(text.to_string());
                                    }
                                }
                                Some("toolCall") => {
                                    if let Some(name) =
                                        block.get("name").and_then(|n| n.as_str())
                                    {
                                        candidates.push(name.to_string());
                                    }
                                    // Tool arguments often carry the most
                                    // searchable context (file paths, commands).
                                    if let Some(args) = block.get("arguments") {
                                        candidates.push(args.to_string());
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                if message.get("role").and_then(|r| r.as_str()) == Some("toolResult") {
                    let body = tool_result_text(message);
                    if !body.is_empty() {
                        candidates.push(body);
                    }
                }
                for candidate in candidates {
                    if pi_text_matches(&candidate, &needle, case_sensitive, whole_word) {
                        snippet = Some(pi_snippet(&candidate, query));
                        break;
                    }
                }
                if snippet.is_some() {
                    break;
                }
            }
            let Some(snippet) = snippet else {
                continue;
            };
            let Some(session_id) = pi_session_id_from_filename(&path) else {
                continue;
            };
            let (project_path, modified) = pi_search_metadata(home, &session_id);
            hits.push(crate::session::history::DeepSearchHit {
                session_id,
                snippet,
                project_path,
                modified,
                provider: "pi".to_string(),
                surface: Some("cli".to_string()),
                agent_kind: "root".to_string(),
            });
        }
    }
    hits
}

fn pi_text_matches(text: &str, needle: &str, case_sensitive: bool, whole_word: bool) -> bool {
    if needle.is_empty() {
        return false;
    }
    if whole_word {
        let haystack = if case_sensitive {
            text.to_string()
        } else {
            text.to_lowercase()
        };
        return haystack.split(|c: char| !c.is_alphanumeric()).any(|word| word == needle);
    }
    if case_sensitive {
        text.contains(needle)
    } else {
        text.to_lowercase().contains(needle)
    }
}

/// ~200-char snippet around the first match, mirroring Claude search hits.
fn pi_snippet(text: &str, query: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let pos = text
        .to_lowercase()
        .find(&query.to_lowercase())
        .map(|byte| text[..byte].chars().count())
        .unwrap_or(0);
    let start = pos.saturating_sub(80);
    let end = (pos + query.chars().count() + 120).min(chars.len());
    let mut snippet: String = chars[start..end].iter().collect();
    if start > 0 {
        snippet = format!("…{snippet}");
    }
    if end < chars.len() {
        snippet.push('…');
    }
    snippet
}

pub(crate) fn pi_cost_records(home: &Path) -> Vec<crate::session::cost::SessionCostRecord> {
    let root = pi_sessions_root(home);
    let mut records = Vec::new();
    let Ok(dirs) = std::fs::read_dir(&root) else {
        return records;
    };
    for dir_entry in dirs.flatten() {
        let dir = dir_entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for file_entry in files.flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(summary) = summarize_pi_transcript(&path) else {
                continue;
            };
            if summary.session_id.is_empty() {
                continue;
            }
            let total_tokens = summary.input_tokens + summary.output_tokens;
            if total_tokens == 0 {
                continue;
            }
            let project = if summary.project_path.is_empty() {
                decode_pi_cwd_dir(&dir)
            } else {
                summary.project_path.clone()
            };
            let project_name = Path::new(&project)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            let timestamp = if summary.last_timestamp.is_empty() {
                summary
                    .started_at_ms
                    .and_then(DateTime::from_timestamp_millis)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default()
            } else {
                summary.last_timestamp.clone()
            };
            let date = timestamp
                .get(..10)
                .filter(|d| d.len() == 10)
                .unwrap_or("unknown")
                .to_string();
            records.push(crate::session::cost::SessionCostRecord {
                session_id: summary.session_id.clone(),
                project,
                project_name,
                model: summary.model.clone().unwrap_or_default(),
                provider: "pi".to_string(),
                surface: Some("cli".to_string()),
                agent_kind: Some("root".to_string()),
                cost: summary.total_cost,
                cost_available: total_tokens > 0,
                input_tokens: summary.input_tokens,
                cached_input_tokens: summary.cached_input_tokens,
                output_tokens: summary.output_tokens,
                reasoning_output_tokens: summary.reasoning_tokens,
                total_tokens,
                timestamp,
                date,
                session_name: summary
                    .first_prompt
                    .map(|p| truncate_chars(p.trim(), 60))
                    .filter(|p| !p.is_empty())
                    .unwrap_or(summary.session_id.clone()),
            });
        }
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_transcript(dir: &TempDir, name: &str, lines: &[&str]) -> PathBuf {
        // Mirror production layout: <home>/.pi/agent/sessions/<encoded-cwd>/<file>.jsonl
        let cwd_dir = dir
            .path()
            .join(".pi")
            .join("agent")
            .join("sessions")
            .join("encoded-cwd");
        std::fs::create_dir_all(&cwd_dir).unwrap();
        let path = cwd_dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        path
    }

    fn sample_lines() -> Vec<String> {
        vec![
            r#"{"id":"2026-08-16T07:07:48.328Z","timestamp":"2026-08-16T07:07:48.328Z","type":"session","session":{"id":"01a06659-1111-2222-3333-444455556666","timestamp":1755325668328,"cwd":"/tmp/demo"}}"#.to_string(),
            r#"{"id":"msg_1","timestamp":1755325669000,"type":"message","message":{"role":"user","content":[{"type":"text","text":"Fix the login bug"}]}}"#.to_string(),
            r#"{"id":"msg_2","timestamp":1755325670000,"type":"message","message":{"role":"assistant","model":"anthropic/claude-opus-4-6","provider":"openrouter","content":[{"type":"toolCall","id":"call-1","name":"read","arguments":{"path":"/tmp/a"}}],"stopReason":"tooluse","usage":{"input":10,"output":5,"cacheRead":100,"cacheWrite":20,"reasoning":0,"totalTokens":135,"cost":{"total":0.01}}}}"#.to_string(),
            r#"{"id":"msg_3","timestamp":1755325671000,"type":"message","message":{"role":"toolResult","toolCallId":"call-1","toolName":"read","content":"file bytes","isError":false}}"#.to_string(),
            r#"{"id":"msg_4","timestamp":1755325672000,"type":"message","message":{"role":"assistant","model":"anthropic/claude-opus-4-6","content":[{"type":"text","text":"Done."}],"stopReason":"stop","usage":{"input":11,"output":6,"cacheRead":0,"cacheWrite":0,"reasoning":0,"totalTokens":17,"cost":{"total":0.002}}}}"#.to_string(),
        ]
    }

    #[test]
    fn session_id_comes_from_filename_suffix() {
        let dir = TempDir::new().unwrap();
        let path = write_transcript(
            &dir,
            "2026-08-16T07-07-48-328Z_01a06659-1111-2222-3333-444455556666.jsonl",
            &sample_lines().iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert_eq!(
            pi_session_id_from_filename(&path).as_deref(),
            Some("01a06659-1111-2222-3333-444455556666")
        );
    }

    #[test]
    fn summary_extracts_prompts_tokens_and_cost() {
        let dir = TempDir::new().unwrap();
        let path = write_transcript(&dir, "2026-08-16T07-07-48-328Z_abc123.jsonl", &sample_lines().iter().map(String::as_str).collect::<Vec<_>>());
        let summary = summarize_pi_transcript(&path).unwrap();
        assert_eq!(summary.session_id, "abc123");
        assert_eq!(summary.project_path, "/tmp/demo");
        assert_eq!(summary.first_prompt.as_deref(), Some("Fix the login bug"));
        assert_eq!(summary.latest_snippet.as_deref(), Some("Done."));
        assert_eq!(summary.message_count, 3);
        assert_eq!(summary.input_tokens, 21);
        assert_eq!(summary.output_tokens, 11);
        assert_eq!(summary.cached_input_tokens, 120);
        assert!((summary.total_cost - 0.012).abs() < 1e-9);
        assert_eq!(summary.model.as_deref(), Some("anthropic/claude-opus-4-6"));
        // toolCall was answered by a toolResult → no pending tool.
        assert_eq!(summary.pending_tool_name, None);
    }

    #[test]
    fn pending_tool_call_reports_working() {
        let dir = TempDir::new().unwrap();
        let lines = vec![
            r#"{"id":"s","timestamp":"2026-08-16T07:07:48.328Z","type":"session","session":{"id":"pend1","timestamp":1755325668328,"cwd":"/tmp/demo"}}"#,
            r#"{"id":"m1","timestamp":1755325669000,"type":"message","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"id":"m2","timestamp":1755325669100,"type":"message","message":{"role":"assistant","model":"m","content":[{"type":"toolCall","id":"call-9","name":"bash","arguments":{}}],"usage":{"input":1,"output":1,"totalTokens":2}}}"#,
        ];
        let path = write_transcript(&dir, "2026-08-16T07-07-48-328Z_pend1.jsonl", &lines);
        let summary = summarize_pi_transcript(&path).unwrap();
        assert_eq!(summary.lifecycle, PiLifecycle::Working);
        assert_eq!(summary.pending_tool_name.as_deref(), Some("bash"));
    }

    #[test]
    fn conversation_maps_roles_and_truncates_tool_results() {
        let dir = TempDir::new().unwrap();
        write_transcript(&dir, "2026-08-16T07-07-48-328Z_conv9.jsonl", &sample_lines().iter().map(String::as_str).collect::<Vec<_>>());
        let messages =
            read_pi_conversation_under(dir.path(), "conv9").unwrap();
        let kinds: Vec<MessageType> =
            messages.iter().map(|m| m.message_type.clone()).collect();
        assert_eq!(
            kinds,
            vec![
                MessageType::User,
                MessageType::ToolUse,
                MessageType::ToolResult,
                MessageType::Assistant,
            ]
        );
        assert_eq!(messages[0].content, "Fix the login bug");
        assert!(messages[1].content.starts_with("read "));
        assert_eq!(messages[3].content, "Done.");
    }

    #[test]
    fn legacy_flat_header_and_array_tool_result_parse() {
        let dir = TempDir::new().unwrap();
        let lines = vec![
            r#"{"type":"session","version":3,"id":"legacy1","timestamp":"2026-08-23T01:44:34.288Z","cwd":"/tmp/legacy"}"#,
            r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"old hello"}]}}"#,
            r#"{"type":"message","message":{"role":"toolResult","toolCallId":"c1","toolName":"bash","content":[{"type":"text","text":"old output"}]}}"#,
        ];
        let path = write_transcript(&dir, "2026-08-23T01-44-34-288Z_legacy1.jsonl", &lines);
        let summary = summarize_pi_transcript(&path).unwrap();
        assert_eq!(summary.project_path, "/tmp/legacy");
        assert!(summary.started_at_ms.unwrap() > 1_700_000_000_000);
        assert_eq!(summary.first_prompt.as_deref(), Some("old hello"));
        let messages = read_pi_conversation_under(dir.path(), "legacy1").unwrap();
        assert!(messages
            .iter()
            .any(|m| m.message_type == MessageType::ToolResult && m.content == "old output"));
    }

    #[test]
    fn decode_cwd_dir_falls_back_to_slash_path() {
        assert_eq!(
            decode_pi_cwd_dir(Path::new("--Users-liminchen-Documents-GitHub-c9watch")),
            "/Users/liminchen/Documents/GitHub/c9watch"
        );
    }
}
