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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Idle pi transcripts older than this are treated as expired.
const PI_FRESHNESS_IDLE_SECS: u64 = 30 * 60;
/// Working pi transcripts older than this are treated as expired.
const PI_FRESHNESS_WORKING_SECS: u64 = 4 * 60 * 60;
/// A transcript counts as actively generating when its last *message*
/// is this recent. Ledger lines (`compaction`, `model_change`) bump the
/// file mtime without moving the conversation forward, so file mtime
/// alone must not flip a session back to Working.
const PI_MESSAGE_RECENCY_SECS: i64 = 120;
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
    last_message_ts_ms: Option<i64>,
    trailing_user: bool,
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
        self.summary_for_at(path, chrono::Utc::now().timestamp_millis())
    }

    fn summary_for_at(&mut self, path: &Path, now_ms: i64) -> Option<PiTranscriptSummary> {
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
                let mut summary = cached.summary.clone();
                summary.refresh_lifecycle(now_ms);
                return Some(summary);
            }
        }
        let mut summary = summarize_pi_transcript(path)?;
        summary.refresh_lifecycle(now_ms);
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
/// for `/Users/foo/bar`). The encoding is lossy: `/` becomes `-` while real
/// dashes are preserved, so names with dashes cannot round-trip. Callers
/// prefer [`pi_dir_cwd`] (exact, via sibling headers) and use this last.
fn decode_pi_cwd_dir(dir: &Path) -> String {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let stripped = name.trim_matches('-');
    let decoded = stripped.replace('-', "/");
    if decoded.starts_with('/') {
        decoded
    } else {
        format!("/{decoded}")
    }
}

/// Exact cwd for a transcript directory: reuse any sibling transcript's
/// session header (same cwd by construction). Falls back to lossy dirname
/// decoding when no sibling carries a cwd.
fn pi_dir_cwd(dir: &Path) -> String {
    if let Ok(files) = std::fs::read_dir(dir) {
        for entry in files.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(cwd) = read_pi_header_cwd(&path) {
                return cwd;
            }
        }
    }
    decode_pi_cwd_dir(dir)
}

/// cwd from a transcript's session header (either schema), scanning only
/// the head of the file. Returns `None` when no header carries a cwd.
fn read_pi_header_cwd(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines().take(50) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("session") {
            continue;
        }
        if let Some(cwd) = value
            .get("session")
            .and_then(|s| s.get("cwd"))
            .and_then(|c| c.as_str())
        {
            return Some(cwd.to_string());
        }
        if let Some(cwd) = value.get("cwd").and_then(|c| c.as_str()) {
            return Some(cwd.to_string());
        }
    }
    None
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
    let mut pending_tools: HashMap<String, (u64, String)> = HashMap::new();
    let mut tool_seq: u64 = 0;
    let mut last_role: Option<String> = None;
    let mut last_message_ts_ms: Option<i64> = None;
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
                    if header_ts.is_none() {
                        if let Some(ts) = session.get("timestamp").and_then(parse_pi_timestamp_ms)
                        {
                            header_ts = Some(ts);
                        }
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
                let ts_ms = value.get("timestamp").and_then(parse_pi_timestamp_ms);
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
                        if ts_ms.is_some() {
                            last_message_ts_ms = ts_ms;
                        }
                        if let Some(t) = ts.clone() {
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
                        if ts_ms.is_some() {
                            last_message_ts_ms = ts_ms;
                        }
                        if let Some(t) = ts.clone() {
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
                                    // Order-sensitive: a later toolResult
                                    // clears the id, but a re-issued call
                                    // with the same id pends again.
                                    if !id.is_empty() {
                                        tool_seq += 1;
                                        pending_tools.insert(id, (tool_seq, name));
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    "toolResult" => {
                        last_role = Some(role);
                        if ts_ms.is_some() {
                            last_message_ts_ms = ts_ms;
                        }
                        if let Some(t) = ts.clone() {
                            summary.last_timestamp = t;
                        }
                        if let Some(id) =
                            message.get("toolCallId").and_then(|i| i.as_str())
                        {
                            pending_tools.remove(id);
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
    // Sibling headers share this directory's cwd, so a header-less file
    // still resolves dash-containing paths exactly; dirname decoding is
    // the last resort.
    summary.project_path = header_cwd
        .or_else(|| path.parent().map(pi_dir_cwd))
        .unwrap_or_default();
    summary.started_at_ms = header_ts;
    summary.empty = summary.message_count == 0;

    let mut pending: Vec<(u64, String)> = pending_tools.into_values().collect();
    pending.sort_by_key(|(seq, _)| *seq);
    summary.pending_tool_name = pending.last().map(|(_, name)| name.clone());

    summary.last_message_ts_ms = last_message_ts_ms;
    summary.trailing_user = last_role.as_deref() == Some("user");
    summary.refresh_lifecycle(chrono::Utc::now().timestamp_millis());
    Some(summary)
}

impl PiTranscriptSummary {
    /// Cache transcript facts, but recompute time-dependent state on every poll.
    fn refresh_lifecycle(&mut self, now_ms: i64) {
        let recent = self.last_message_ts_ms.is_some_and(|ms| {
            let age = now_ms.saturating_sub(ms);
            (0..PI_MESSAGE_RECENCY_SECS * 1000).contains(&age)
        });
        self.lifecycle = if self.pending_tool_name.is_some() || self.trailing_user || recent {
            PiLifecycle::Working
        } else {
            PiLifecycle::Idle
        };
    }
}

fn pi_thinking_text(block: &serde_json::Value) -> Option<&str> {
    block
        .get("thinking")
        .and_then(|v| v.as_str())
        .or_else(|| block.get("text").and_then(|v| v.as_str()))
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
    summary.input_tokens += usage.get("input").map(u64_from_json).unwrap_or(0);
    summary.output_tokens += usage.get("output").map(u64_from_json).unwrap_or(0);
    summary.cached_input_tokens += usage
        .get("cacheRead")
        .map(u64_from_json)
        .unwrap_or(0)
        + usage.get("cacheWrite").map(u64_from_json).unwrap_or(0);
    summary.reasoning_tokens += usage
        .get("reasoning")
        .map(u64_from_json)
        .unwrap_or(0);
    if let Some(cost) = usage.get("cost").and_then(|c| c.get("total")) {
        summary.total_cost += cost.as_f64().unwrap_or(0.0);
    }
}

/// Token counts are integers, but accept floats defensively so `10.0`
/// does not silently become 0 while cost still accrues.
fn u64_from_json(value: &serde_json::Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_f64().map(|f| f as u64))
        .unwrap_or(0)
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
                            if let Some(text) = pi_thinking_text(block) {
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

pub(crate) fn pi_conversation_path_for_session(home: &Path, session_id: &str) -> Option<PathBuf> {
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
    let mut seen = std::collections::HashSet::new();
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
                if id.starts_with(prefix) && seen.insert(id.clone()) {
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

fn file_mtime_ms(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
}

/// Drop tool records when the caller asked for a tool-free conversation,
/// mirroring the other providers' `include_tools=false` behavior.
pub(crate) fn apply_pi_tool_filter(
    messages: Vec<PiConversationMessage>,
    include_tools: bool,
) -> Vec<PiConversationMessage> {
    if include_tools {
        return messages;
    }
    messages
        .into_iter()
        .filter(|m| {
            !matches!(
                m.message_type,
                MessageType::ToolUse | MessageType::ToolResult
            )
        })
        .collect()
}

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
                // Header-less transcripts fall back to file mtime so they
                // do not all cluster at epoch in sorted history.
                timestamp: summary
                    .started_at_ms
                    .filter(|ms| *ms > 0)
                    .map(|ms| ms as u64)
                    .or_else(|| file_mtime_ms(&path))
                    .unwrap_or(0),
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
                                Some("thinking") => {
                                    if let Some(text) = pi_thinking_text(block) {
                                        candidates.push(text.to_string());
                                    }
                                }
                                Some("text") => {
                                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
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
/// Char-indexed throughout: lowercasing can change byte length (e.g. `İ`),
/// so byte offsets from the lowered string must never slice the original.
fn pi_snippet(text: &str, query: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let query_chars: Vec<char> = query.to_lowercase().chars().collect();
    if query_chars.is_empty() || chars.is_empty() {
        return chars.into_iter().take(200).collect();
    }
    let lower_chars: Vec<char> = text.to_lowercase().chars().collect();
    let pos = lower_chars
        .windows(query_chars.len())
        .position(|window| window == query_chars.as_slice())
        .unwrap_or(0);
    // Lowered/original lengths can differ; clamp so indices stay in range.
    // The window may be off by a char in exotic scripts, never a panic.
    let start = pos.saturating_sub(80).min(chars.len());
    let end = (pos + query_chars.len() + 120).min(chars.len());
    let start = start.min(end);
    let mut snippet: String = chars[start..end].iter().collect();
    if start > 0 {
        snippet = format!("…{snippet}");
    }
    if end < chars.len() {
        snippet.push('…');
    }
    snippet
}

#[derive(Default)]
struct PiCostDay {
    usage: PiTranscriptSummary,
    timestamp: String,
    model_costs: HashMap<String, f64>,
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
            let fallback_ms = summary
                .started_at_ms
                .or_else(|| file_mtime_ms(&path).map(|ms| ms as i64));
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut days: std::collections::BTreeMap<String, PiCostDay> = Default::default();
            for line in content.lines() {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if value.get("type").and_then(|v| v.as_str()) != Some("message") {
                    continue;
                }
                let Some(message) = value.get("message") else {
                    continue;
                };
                if message.get("role").and_then(|v| v.as_str()) != Some("assistant")
                    || message.get("usage").is_none()
                {
                    continue;
                }
                let timestamp = value
                    .get("timestamp")
                    .and_then(parse_pi_timestamp_ms)
                    .or_else(|| message.get("timestamp").and_then(parse_pi_timestamp_ms))
                    .or(fallback_ms)
                    .map(millis_to_rfc3339)
                    .unwrap_or_default();
                let date = timestamp.get(..10).unwrap_or("unknown").to_string();
                let day = days.entry(date).or_default();
                if day.timestamp.is_empty() || timestamp < day.timestamp {
                    day.timestamp = timestamp;
                }
                let previous_cost = day.usage.total_cost;
                apply_pi_usage(&mut day.usage, message);
                let model = message.get("model").and_then(|m| m.as_str()).unwrap_or("");
                *day.model_costs.entry(model.to_string()).or_default() +=
                    day.usage.total_cost - previous_cost;
            }
            for (date, day) in days {
                let total_tokens = day.usage.input_tokens + day.usage.output_tokens;
                if total_tokens == 0 {
                    continue;
                }
                let model = day
                    .model_costs
                    .into_iter()
                    .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
                    .map(|(model, _)| model)
                    .unwrap_or_default();
                records.push(crate::session::cost::SessionCostRecord {
                    session_id: summary.session_id.clone(),
                    project: project.clone(),
                    project_name: project_name.clone(),
                    model,
                    provider: "pi".to_string(),
                    surface: Some("cli".to_string()),
                    agent_kind: Some("root".to_string()),
                    cost: day.usage.total_cost,
                    cost_available: total_tokens > 0,
                    input_tokens: day.usage.input_tokens,
                    cached_input_tokens: day.usage.cached_input_tokens,
                    output_tokens: day.usage.output_tokens,
                    reasoning_output_tokens: day.usage.reasoning_tokens,
                    total_tokens,
                    timestamp: day.timestamp,
                    date,
                    session_name: summary
                        .first_prompt
                        .as_ref()
                        .map(|p| truncate_chars(p.trim(), 60))
                        .filter(|p| !p.is_empty())
                        .unwrap_or(summary.session_id.clone()),
                });
            }
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
    fn cached_lifecycle_ages_without_file_changes() {
        let dir = TempDir::new().unwrap();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let message = serde_json::json!({"type":"message","timestamp":now_ms,
            "message":{"role":"assistant","content":[{"type":"text","text":"done"}],"stopReason":"stop"}}).to_string();
        let path = write_transcript(
            &dir,
            "t_cached.jsonl",
            &[
                r#"{"type":"session","cwd":"/tmp/demo","timestamp":"2026-09-01T00:00:00Z"}"#,
                &message,
            ],
        );
        let mut source = PiSessionSource::at_root(pi_sessions_root(dir.path()));
        assert_eq!(
            source.summary_for_at(&path, now_ms).unwrap().lifecycle,
            PiLifecycle::Working
        );
        assert_eq!(
            source
                .summary_for_at(&path, now_ms + 119_999)
                .unwrap()
                .lifecycle,
            PiLifecycle::Working
        );
        assert_eq!(
            source
                .summary_for_at(&path, now_ms + 120_000)
                .unwrap()
                .lifecycle,
            PiLifecycle::Idle
        );
        // Cached facts are retained; only the returned snapshot ages.
        assert_eq!(source.cache[&path].summary.lifecycle, PiLifecycle::Working);
        assert_eq!(source.cache.len(), 1);

        let pending = write_transcript(
            &dir,
            "t_pending.jsonl",
            &[
                r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"bash"}]}}"#,
            ],
        );
        assert_eq!(
            source
                .summary_for_at(&pending, now_ms + 120_000)
                .unwrap()
                .lifecycle,
            PiLifecycle::Working
        );
    }

    #[test]
    fn official_thinking_is_visible_and_searchable() {
        let dir = TempDir::new().unwrap();
        write_transcript(
            &dir,
            "t_thought.jsonl",
            &[
                r#"{"type":"session","cwd":"/tmp/demo","timestamp":"2026-09-01T00:00:00Z"}"#,
                r#"{"type":"message","message":{"role":"assistant","content":[{"type":"thinking","thinking":"UniqueReasoning 正式內容","text":"wrong fallback"},{"type":"thinking","text":"legacy reasoning"},{"type":"text","text":"done"}]}}"#,
            ],
        );
        let rows = apply_pi_tool_filter(
            read_pi_conversation_under(dir.path(), "thought").unwrap(),
            false,
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].message_type, MessageType::Thinking);
        assert_eq!(rows[0].content, "UniqueReasoning 正式內容");
        assert_eq!(rows[1].content, "legacy reasoning");
        for (query, sensitive, whole) in [
            ("UniqueReasoning", true, true),
            ("uniquereasoning", false, false),
            ("正式內容", true, false),
        ] {
            let hits = pi_deep_search(dir.path(), query, sensitive, whole);
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].session_id, "thought");
            assert!(hits[0].snippet.contains("正式內容"));
        }
        assert!(pi_deep_search(dir.path(), "wrong fallback", true, false).is_empty());
    }

    #[test]
    fn cost_records_split_days_and_ignore_later_user_activity() {
        let dir = TempDir::new().unwrap();
        let mut lines = vec![
            r#"{"type":"session","cwd":"/tmp/demo","timestamp":"2026-09-01T00:00:00Z"}"#
                .to_string(),
        ];
        for (timestamp, model, cost) in [
            ("2026-09-01T02:00:00Z", "cheap", 1.0),
            ("2026-09-01T01:00:00Z", "expensive", 2.0),
            ("2026-09-02T01:00:00Z", "next-day", 4.0),
        ] {
            lines.push(serde_json::json!({"type":"message","timestamp":timestamp,"message":{
                "role":"assistant","model":model,"content":[],
                "usage":{"input":10,"output":5,"cacheRead":100,"cacheWrite":20,"reasoning":2,"cost":{"total":cost}}
            }}).to_string());
        }
        lines.push(r#"{"type":"message","timestamp":"2026-09-03T00:00:00Z","message":{"role":"user","content":"continue"}}"#.to_string());
        lines.push("{incomplete".to_string());
        write_transcript(
            &dir,
            "t_days.jsonl",
            &lines.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let records = pi_cost_records(dir.path());
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].date, "2026-09-01");
        assert_eq!(records[0].timestamp, "2026-09-01T01:00:00+00:00");
        assert_eq!(records[0].cost, 3.0);
        assert_eq!(records[0].input_tokens, 20);
        assert_eq!(records[0].output_tokens, 10);
        assert_eq!(records[0].cached_input_tokens, 240);
        assert_eq!(records[0].reasoning_output_tokens, 4);
        assert_eq!(records[0].model, "expensive");
        assert_eq!(records[1].date, "2026-09-02");
        assert_eq!(records[1].cost, 4.0);
        assert_eq!(records[1].model, "next-day");
        assert!(records
            .iter()
            .all(|r| r.session_id == "days" && r.provider == "pi"));
    }

    #[test]
    fn cost_dates_use_message_timestamp_then_header_then_mtime() {
        let dir = TempDir::new().unwrap();
        write_transcript(
            &dir,
            "t_fallback.jsonl",
            &[
                r#"{"type":"session","cwd":"/tmp/demo","timestamp":"2026-09-01T00:00:00Z"}"#,
                r#"{"type":"message","message":{"role":"assistant","timestamp":1788307200000,"usage":{"input":1,"output":1,"cost":{"total":1}}}}"#,
                r#"{"type":"message","message":{"role":"assistant","usage":{"input":1,"output":1,"cost":{"total":2}}}}"#,
            ],
        );
        let records = pi_cost_records(dir.path());
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].date, "2026-09-01");
        assert_eq!(records[0].cost, 2.0);
        assert_eq!(records[1].date, "2026-09-02");
        assert_eq!(records[1].cost, 1.0);

        let naked = TempDir::new().unwrap();
        let path = write_transcript(
            &naked,
            "t_naked.jsonl",
            &[
                r#"{"type":"message","message":{"role":"assistant","usage":{"input":1,"output":1,"cost":{"total":3}}}}"#,
            ],
        );
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1788307200))
            .unwrap();
        assert_eq!(pi_cost_records(naked.path())[0].date, "2026-09-02");
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
        let path = write_transcript(
            &dir,
            "2026-08-16T07-07-48-328Z_abc123.jsonl",
            &sample_lines()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
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
        write_transcript(
            &dir,
            "2026-08-16T07-07-48-328Z_conv9.jsonl",
            &sample_lines()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        let messages = read_pi_conversation_under(dir.path(), "conv9").unwrap();
        let kinds: Vec<MessageType> = messages.iter().map(|m| m.message_type.clone()).collect();
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
    fn headerless_file_reuses_sibling_cwd_with_dashes() {
        let dir = TempDir::new().unwrap();
        let lines = vec![
            r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"no header here"}]}}"#,
        ];
        // Header-less file next to a sibling whose header carries a
        // dash-containing cwd: the exact path must win over lossy decode.
        let headerless =
            write_transcript(&dir, "2026-09-03T00-00-00-000Z_naked1.jsonl", &lines);
        let sibling_dir = headerless.parent().unwrap();
        let mut sibling =
            std::fs::File::create(sibling_dir.join("2026-09-03T00-00-01-000Z_sib2.jsonl")).unwrap();
        use std::io::Write as _;
        writeln!(
            sibling,
            r#"{{"type":"session","session":{{"id":"sib2","timestamp":1756857600000,"cwd":"/tmp/my-project"}}}}"#
        )
        .unwrap();
        let summary = summarize_pi_transcript(&headerless).unwrap();
        assert_eq!(summary.project_path, "/tmp/my-project");
    }

    #[test]
    fn snippet_handles_multibyte_case_folding_without_panic() {
        // `ß` lowercases to two chars (`ss`), shifting char offsets.
        let text = "Die GROßE Straße führt nach Süden und weiter";
        let snippet = pi_snippet(text, "grosse");
        assert!(snippet.contains("GROßE"));
        let cjk = "請直接在目前的 c9watch repository 工作，然後回報結果給我看";
        let snippet = pi_snippet(cjk, "c9watch");
        assert!(snippet.contains("c9watch"));
        assert!(pi_snippet(text, "").chars().count() <= 200);
    }

    #[test]
    fn timestamp_parser_accepts_int_float_and_rfc3339() {
        use serde_json::json;
        assert_eq!(
            parse_pi_timestamp_ms(&json!(1755325668328i64)),
            Some(1755325668328)
        );
        assert_eq!(
            parse_pi_timestamp_ms(&json!(1755325668.5)),
            Some(1755325668500)
        );
        assert_eq!(
            parse_pi_timestamp_ms(&json!("2026-08-16T07:07:48.328Z")),
            Some(1786864068328)
        );
        assert_eq!(parse_pi_timestamp_ms(&json!(null)), None);
    }

    #[test]
    fn tool_filter_drops_tool_rows_when_disabled() {
        let messages = vec![
            PiConversationMessage {
                timestamp: String::new(),
                message_type: MessageType::User,
                content: "hi".to_string(),
            },
            PiConversationMessage {
                timestamp: String::new(),
                message_type: MessageType::ToolUse,
                content: "read".to_string(),
            },
            PiConversationMessage {
                timestamp: String::new(),
                message_type: MessageType::ToolResult,
                content: "bytes".to_string(),
            },
        ];
        assert_eq!(apply_pi_tool_filter(messages.clone(), true).len(), 3);
        let filtered = apply_pi_tool_filter(messages, false);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].message_type, MessageType::User);
    }

    #[test]
    fn ledger_append_after_idle_stays_idle() {
        // The exact regression for message-based liveness: a compaction
        // ledger line bumps the file mtime, but the conversation ended
        // long ago, so the session must stay Idle.
        let dir = TempDir::new().unwrap();
        let lines = vec![
            r#"{"type":"session","session":{"id":"idle1","timestamp":1755325668328,"cwd":"/tmp/demo"}}"#,
            r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"id":"m2","timestamp":1755325672000,"type":"message","message":{"role":"assistant","model":"m","content":[{"type":"text","text":"done"}],"stopReason":"stop"}}"#,
            r#"{"id":"c1","timestamp":1786864068328000,"type":"compaction","summary":"squashed"}"#,
        ];
        let path = write_transcript(&dir, "2026-08-16T07-07-48-328Z_idle1.jsonl", &lines);
        let summary = summarize_pi_transcript(&path).unwrap();
        assert_eq!(summary.lifecycle, PiLifecycle::Idle);
        assert_eq!(summary.pending_tool_name, None);
    }

    #[test]
    fn reissued_tool_call_pends_again_after_result() {
        let dir = TempDir::new().unwrap();
        let lines = vec![
            r#"{"type":"session","session":{"id":"re1","timestamp":1755325668328,"cwd":"/tmp/demo"}}"#,
            r#"{"id":"m1","timestamp":1755325669000,"type":"message","message":{"role":"assistant","model":"m","content":[{"type":"toolCall","id":"c1","name":"bash","arguments":{}}]}}"#,
            r#"{"id":"m2","timestamp":1755325670000,"type":"message","message":{"role":"toolResult","toolCallId":"c1","toolName":"bash","content":"ok"}}"#,
            r#"{"id":"m3","timestamp":1755325671000,"type":"message","message":{"role":"assistant","model":"m","content":[{"type":"toolCall","id":"c1","name":"bash","arguments":{}}]}}"#,
        ];
        let path = write_transcript(&dir, "2026-08-16T07-07-48-328Z_re1.jsonl", &lines);
        let summary = summarize_pi_transcript(&path).unwrap();
        assert_eq!(summary.lifecycle, PiLifecycle::Working);
        assert_eq!(summary.pending_tool_name.as_deref(), Some("bash"));
    }

    #[test]
    fn headerless_file_without_messages_is_rejected() {
        let dir = TempDir::new().unwrap();
        let lines = vec![
            r#"{"id":"c1","timestamp":1755325672000,"type":"compaction","summary":"squashed"}"#,
            "not json at all",
        ];
        let path = write_transcript(&dir, "2026-08-16T07-07-48-328Z_ghost1.jsonl", &lines);
        assert!(summarize_pi_transcript(&path).is_none());
    }

    #[test]
    fn detect_respects_freshness_windows() {
        use std::time::{Duration, SystemTime};
        let dir = TempDir::new().unwrap();
        let root = dir
            .path()
            .join(".pi")
            .join("agent")
            .join("sessions")
            .join("encoded-cwd");
        std::fs::create_dir_all(&root).unwrap();
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        // Idle transcript, mtime 40min ago (> 30min idle window) → dropped.
        let idle_lines = format!(
            "{{\"type\":\"session\",\"session\":{{\"id\":\"oldidle\",\"timestamp\":{now_ms},\"cwd\":\"/tmp/demo\"}}}}\n\
             {{\"id\":\"m\",\"timestamp\":{msg},\"type\":\"message\",\"message\":{{\"role\":\"assistant\",\"model\":\"m\",\"content\":[{{\"type\":\"text\",\"text\":\"done\"}}],\"stopReason\":\"stop\"}}}}\n",
            msg = now_ms - 40 * 60 * 1000,
        );
        let idle_path = root.join("2026-08-16T07-07-48-328Z_oldidle.jsonl");
        std::fs::write(&idle_path, idle_lines).unwrap();
        let old = SystemTime::now() - Duration::from_secs(40 * 60);
        std::fs::File::options()
            .write(true)
            .open(&idle_path)
            .unwrap()
            .set_modified(old)
            .unwrap();

        // Working transcript (pending tool), mtime 3h ago (< 4h working window) → kept.
        let work_lines = format!(
            "{{\"type\":\"session\",\"session\":{{\"id\":\"oldwork\",\"timestamp\":{now_ms},\"cwd\":\"/tmp/demo\"}}}}\n\
             {{\"id\":\"m\",\"timestamp\":{msg},\"type\":\"message\",\"message\":{{\"role\":\"assistant\",\"model\":\"m\",\"content\":[{{\"type\":\"toolCall\",\"id\":\"c9\",\"name\":\"bash\",\"arguments\":{{}}}}]}}}}\n",
            msg = now_ms - 3 * 60 * 60 * 1000,
        );
        let work_path = root.join("2026-08-16T07-07-48-328Z_oldwork.jsonl");
        std::fs::write(&work_path, work_lines).unwrap();
        let old = SystemTime::now() - Duration::from_secs(3 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&work_path)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let mut source =
            PiSessionSource::at_root(dir.path().join(".pi").join("agent").join("sessions"));
        let (sessions, _) = source.detect().unwrap();
        let ids: Vec<&str> = sessions
            .iter()
            .filter_map(|s| s.session_id.as_deref())
            .collect();
        assert!(!ids.contains(&"oldidle"), "stale idle must expire");
        assert!(ids.contains(&"oldwork"), "working within 4h must stay");
    }

    #[test]
    fn history_and_cost_carry_pi_provider_namespace() {
        let dir = TempDir::new().unwrap();
        write_transcript(
            &dir,
            "2026-08-16T07-07-48-328Z_ns9.jsonl",
            &sample_lines()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        let history = pi_history_entries(dir.path());
        // The transcript header carries a different id; the filename wins.
        let entry = history
            .iter()
            .find(|e| e.session_id == "ns9")
            .expect("pi history entry");
        assert_eq!(entry.provider, "pi");
        let costs = pi_cost_records(dir.path());
        assert!(
            costs.iter().all(|r| r.provider == "pi"),
            "cost records must stay in the pi namespace"
        );
        assert!(!costs.is_empty());
    }

    #[test]
    fn decode_cwd_dir_falls_back_to_slash_path() {
        assert_eq!(
            decode_pi_cwd_dir(Path::new("--Users-liminchen-Documents-GitHub-c9watch")),
            "/Users/liminchen/Documents/GitHub/c9watch"
        );
    }
}
