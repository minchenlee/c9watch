//! Subagent detection by parsing parent session JSONL files.
//!
//! When Claude Code's Agent (a.k.a. Task) tool runs a subagent, the call appears in
//! the parent session's JSONL transcript as an assistant `tool_use` block with
//! `name = "Agent"` (or `"Task"` on some CC versions). The subagent's final output
//! comes back as a `tool_result` block referencing the same `tool_use_id`.
//!
//! While the subagent is running, the `tool_use` exists but no matching
//! `tool_result` has appeared yet — that's how we detect "running" subagents
//! without requiring users to install a hook.

use crate::session::parser::{parse_all_entries, MessageContent, SessionEntry};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

// JSONL entry type constants
const ENTRY_TYPE_TOOL_USE: &str = "tool_use";
const ENTRY_TYPE_TOOL_RESULT: &str = "tool_result";
const ENTRY_TYPE_QUEUE_OPERATION: &str = "queue-operation";
const ENTRY_TYPE_USER: &str = "user";

// Tag name constants
const TAG_TASK_ID: &str = "task-id";
const TAG_TOOL_USE_ID: &str = "tool-use-id";
const TAG_RESULT: &str = "result";
const TAG_USAGE: &str = "usage";
const TAG_NOTIFICATION: &str = "task-notification";

/// Status of a detected subagent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentStatus {
    Running,
    Completed,
}

/// A subagent invocation found in a parent session's JSONL.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentInfo {
    /// The Agent tool's `tool_use_id` — unique within the parent session.
    pub id: String,
    /// `subagent_type` from the Agent tool input (e.g. "general-purpose", "Explore").
    pub agent_type: String,
    /// Short description of the task (`description` field of Agent tool input).
    pub description: String,
    /// ISO-8601 timestamp when the tool_use was recorded.
    pub started_at: String,
    /// ISO-8601 timestamp when the tool_result was recorded (None if still running).
    pub completed_at: Option<String>,
    /// Parent session's UUID.
    pub parent_session_id: String,
    /// Running or Completed.
    pub status: SubagentStatus,
}

/// Tool names that indicate a subagent invocation.
/// Different CC versions use different names — match both.
const SUBAGENT_TOOL_NAMES: &[&str] = &["Agent", "Task"];

/// Parse a session JSONL file and extract subagent invocations.
fn extract_subagents_from_entries(
    entries: &[SessionEntry],
    parent_session_id: &str,
) -> Vec<SubagentInfo> {
    // First pass: collect tool_result IDs so we know which Agent calls are done.
    let mut completed_ids: HashMap<String, String> = HashMap::new(); // tool_use_id -> timestamp
    for entry in entries {
        if let SessionEntry::Assistant { base, message } = entry {
            for content in &message.content {
                if let MessageContent::ToolResult { tool_use_id, .. } = content {
                    completed_ids
                        .entry(tool_use_id.clone())
                        .or_insert(base.timestamp.clone());
                }
            }
        }
        // Tool results sometimes also appear inside user messages (the API
        // sends them back as user-role tool_result blocks). Those are parsed
        // into UserMessage with `is_tool_result = true`, but we lose the
        // tool_use_id mapping there. We handle that case via the raw JSON
        // pass below.
    }

    let mut subagents: Vec<SubagentInfo> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for entry in entries {
        if let SessionEntry::Assistant { base, message } = entry {
            for content in &message.content {
                if let MessageContent::ToolUse { id, name, input } = content {
                    if !SUBAGENT_TOOL_NAMES.contains(&name.as_str()) {
                        continue;
                    }
                    if !seen.insert(id.clone()) {
                        continue;
                    }
                    let agent_type = input
                        .get("subagent_type")
                        .and_then(Value::as_str)
                        .unwrap_or("subagent")
                        .to_string();
                    let description = input
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let completed_at = completed_ids.get(id).cloned();
                    let status = if completed_at.is_some() {
                        SubagentStatus::Completed
                    } else {
                        SubagentStatus::Running
                    };
                    subagents.push(SubagentInfo {
                        id: id.clone(),
                        agent_type,
                        description,
                        started_at: base.timestamp.clone(),
                        completed_at,
                        parent_session_id: parent_session_id.to_string(),
                        status,
                    });
                }
            }
        }
    }

    subagents
}

/// Second pass: scan raw JSONL lines for tool_result blocks inside user
/// messages. The typed parser collapses these into a single content string and
/// drops the `tool_use_id`, so we re-scan the raw JSON for the IDs.
fn collect_user_tool_result_ids<P: AsRef<Path>>(path: P) -> HashMap<String, String> {
    let mut completed: HashMap<String, String> = HashMap::new();
    let Ok(file) = fs::File::open(path.as_ref()) else {
        return completed;
    };
    use std::io::{BufRead, BufReader};
    let reader = BufReader::new(file);
    for line in reader.lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        let entry_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        if entry_type != "user" {
            continue;
        }
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let Some(content) = value
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                if let Some(tool_use_id) =
                    block.get("tool_use_id").and_then(Value::as_str)
                {
                    completed
                        .entry(tool_use_id.to_string())
                        .or_insert_with(|| timestamp.clone());
                }
            }
        }
    }
    completed
}

/// Returns subagent invocations for the given session JSONL path.
///
/// `session_id` is the parent session's UUID (used to populate `parent_session_id`).
pub fn active_subagents_for_path<P: AsRef<Path>>(
    session_id: &str,
    jsonl_path: P,
) -> Vec<SubagentInfo> {
    let path = jsonl_path.as_ref();
    let entries = match parse_all_entries(path) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut subagents = extract_subagents_from_entries(&entries, session_id);

    // Merge in tool_result IDs found in user-role messages.
    let user_results = collect_user_tool_result_ids(path);
    for sa in subagents.iter_mut() {
        if sa.completed_at.is_none() {
            if let Some(ts) = user_results.get(&sa.id) {
                sa.completed_at = Some(ts.clone());
                sa.status = SubagentStatus::Completed;
            }
        }
    }

    subagents
}

/// Full transcript of a single subagent invocation — the prompt (Agent tool
/// input), the final result text, and usage stats when available.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentTranscript {
    pub id: String,
    pub agent_type: String,
    pub description: String,
    pub parent_session_id: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub status: SubagentStatus,
    /// Prompt sent to the subagent (from the Agent tool_use input).
    pub prompt: String,
    /// Final result text from the subagent, if completed. Concatenation of
    /// text blocks in the matching tool_result content array, preferring
    /// `toolUseResult.content[].text` when present.
    pub result: Option<String>,
    /// Internal agent id from `toolUseResult.agentId` (CC sub-agent handle).
    pub agent_id: Option<String>,
    pub total_tokens: Option<u64>,
    pub tool_uses: Option<u64>,
    pub duration_ms: Option<u64>,
}

/// Extract the prompt + result for a specific Agent tool_use id in a JSONL.
fn extract_transcript_from_file<P: AsRef<Path>>(
    path: P,
    parent_session_id: &str,
    subagent_id: &str,
) -> Option<SubagentTranscript> {
    use std::io::{BufRead, BufReader};

    // Read all raw lines once so we can do two passes without reopening.
    let file = fs::File::open(path.as_ref()).ok()?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();

    let mut agent_type = String::new();
    let mut description = String::new();
    let mut prompt = String::new();
    let mut started_at = String::new();
    let mut found_tool_use = false;

    // "Immediate" result from the tool_result block / toolUseResult sibling —
    // for async Agent/Task launches this is just a launch stub, but for
    // synchronous tool calls this is the final result.
    let mut immediate_result_text: Option<String> = None;
    let mut immediate_completed_at: Option<String> = None;
    let mut agent_id: Option<String> = None;
    let mut total_tokens: Option<u64> = None;
    let mut tool_uses: Option<u64> = None;
    let mut duration_ms: Option<u64> = None;

    // ── Pass 1: locate tool_use + immediate tool_result for this subagent_id.
    for line in &lines {
        if line.trim().is_empty() || !line.contains(subagent_id) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
        let entry_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let content = value
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array);

        if entry_type == "assistant" {
            if let Some(blocks) = content {
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                        continue;
                    }
                    if block.get("id").and_then(Value::as_str) != Some(subagent_id) {
                        continue;
                    }
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    if !SUBAGENT_TOOL_NAMES.contains(&name) {
                        continue;
                    }
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    agent_type = input
                        .get("subagent_type")
                        .and_then(Value::as_str)
                        .unwrap_or("subagent")
                        .to_string();
                    description = input
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    prompt = input
                        .get("prompt")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    started_at = timestamp.clone();
                    found_tool_use = true;
                }
            }
        }

        let is_result_here = content
            .map(|blocks| {
                blocks.iter().any(|b| {
                    b.get("type").and_then(Value::as_str) == Some("tool_result")
                        && b.get("tool_use_id").and_then(Value::as_str) == Some(subagent_id)
                })
            })
            .unwrap_or(false);

        if is_result_here {
            immediate_completed_at = Some(timestamp.clone());

            let mut is_async_launch = false;
            if let Some(tur) = value.get("toolUseResult") {
                if tur.get("isAsync").and_then(Value::as_bool) == Some(true)
                    || tur.get("status").and_then(Value::as_str) == Some("async_launched")
                {
                    is_async_launch = true;
                }
                if let Some(a) = tur.get("agentId").and_then(Value::as_str) {
                    agent_id = Some(a.to_string());
                }
                if let Some(n) = tur.get("totalTokens").and_then(Value::as_u64) {
                    total_tokens = Some(n);
                }
                if let Some(n) = tur.get("totalToolUseCount").and_then(Value::as_u64) {
                    tool_uses = Some(n);
                }
                if let Some(n) = tur.get("totalDurationMs").and_then(Value::as_u64) {
                    duration_ms = Some(n);
                }
                if let Some(blocks) = tur.get("content").and_then(Value::as_array) {
                    let text = collect_text_blocks(blocks);
                    if !text.is_empty() {
                        immediate_result_text = Some(text);
                    }
                }
            }
            // For async launches, the immediate "completed_at" is just the
            // launch moment — don't treat it as the true completion.
            if is_async_launch {
                immediate_completed_at = None;
            }

            if immediate_result_text.is_none() {
                if let Some(blocks) = content {
                    for b in blocks {
                        if b.get("type").and_then(Value::as_str) != Some("tool_result")
                            || b.get("tool_use_id").and_then(Value::as_str) != Some(subagent_id)
                        {
                            continue;
                        }
                        let inner = b.get("content");
                        if let Some(arr) = inner.and_then(Value::as_array) {
                            let text = collect_text_blocks(arr);
                            if !text.is_empty() {
                                immediate_result_text = Some(text);
                            }
                        } else if let Some(s) = inner.and_then(Value::as_str) {
                            immediate_result_text = Some(s.to_string());
                        }
                    }
                }
            }
        }
    }

    if !found_tool_use {
        return None;
    }

    // If the immediate result looks like an async-launch stub, it will embed
    // the real agentId. Parse it so we can find the real final report later.
    if agent_id.is_none() {
        if let Some(text) = immediate_result_text.as_deref() {
            if let Some(parsed) = parse_agent_id_from_stub(text) {
                agent_id = Some(parsed);
            }
        }
    }

    // ── Pass 2: if we have an agent_id, look for the latest async result.
    // Async final reports arrive either as `queue-operation` events whose
    // `content` is a `<task-notification>` XML-ish block, or (less commonly)
    // as plain user-role messages containing the same block. Prefer
    // queue-operation events — they carry `<usage>` stats too.
    let mut async_result: Option<(String, String)> = None; // (timestamp, text)
    if let Some(aid) = agent_id.as_deref() {
        for line in &lines {
            if line.trim().is_empty() || !line.contains(aid) {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
            let entry_type = value.get("type").and_then(Value::as_str).unwrap_or("");
            let timestamp = value
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            // 2a. queue-operation events: content is a string containing
            //     `<task-notification>…</task-notification>`.
            if entry_type == "queue-operation" {
                let Some(content_str) = value.get("content").and_then(Value::as_str) else {
                    continue;
                };
                if !content_str.contains("<task-notification>") {
                    continue;
                }
                let matches_id = extract_tag(content_str, "task-id")
                    .map(|v| v == aid)
                    .unwrap_or(false)
                    || extract_tag(content_str, "tool-use-id")
                        .map(|v| v == subagent_id)
                        .unwrap_or(false);
                if !matches_id {
                    continue;
                }
                let result_body = extract_tag(content_str, "result").unwrap_or_default();
                if result_body.is_empty() {
                    continue;
                }
                // Populate stats from `<usage>` block.
                if let Some(usage) = extract_tag(content_str, "usage") {
                    let (tt, tu, dm) = extract_usage_stats(&usage);
                    if tt.is_some() {
                        total_tokens = tt;
                    }
                    if tu.is_some() {
                        tool_uses = tu;
                    }
                    if dm.is_some() {
                        duration_ms = dm;
                    }
                }
                async_result = match async_result {
                    Some((prev_ts, prev_text)) if prev_ts > timestamp => {
                        Some((prev_ts, prev_text))
                    }
                    _ => Some((timestamp.clone(), result_body)),
                };
                continue;
            }

            // 2b. user-role messages that embed the notification text.
            if entry_type != "user" {
                continue;
            }
            let content = value.get("message").and_then(|m| m.get("content"));
            // Skip the launch tool_result itself.
            let is_launch_stub = content
                .and_then(Value::as_array)
                .map(|blocks| {
                    blocks.iter().any(|b| {
                        b.get("type").and_then(Value::as_str) == Some("tool_result")
                            && b.get("tool_use_id").and_then(Value::as_str)
                                == Some(subagent_id)
                    })
                })
                .unwrap_or(false);
            if is_launch_stub {
                continue;
            }
            let text = extract_user_message_text(content);
            if text.is_empty() || !text.contains(aid) {
                continue;
            }

            // If the text contains a task-notification block, extract the
            // `<result>` body + usage stats. Otherwise fall back to the raw text.
            let (result_text, usage_from_tag) = if text.contains("<task-notification>") {
                let body = extract_tag(&text, "result").unwrap_or_else(|| text.clone());
                let usage = extract_tag(&text, "usage");
                (body, usage)
            } else {
                (text, None)
            };
            if let Some(usage) = usage_from_tag {
                let (tt, tu, dm) = extract_usage_stats(&usage);
                if tt.is_some() {
                    total_tokens = tt;
                }
                if tu.is_some() {
                    tool_uses = tu;
                }
                if dm.is_some() {
                    duration_ms = dm;
                }
            }
            async_result = match async_result {
                Some((prev_ts, prev_text)) if prev_ts > timestamp => {
                    Some((prev_ts, prev_text))
                }
                _ => Some((timestamp.clone(), result_text)),
            };
        }
    }

    let (result_text, completed_at) = match async_result {
        Some((ts, text)) => (Some(text), Some(ts)),
        None => (immediate_result_text, immediate_completed_at),
    };

    let status = if completed_at.is_some() {
        SubagentStatus::Completed
    } else {
        SubagentStatus::Running
    };

    Some(SubagentTranscript {
        id: subagent_id.to_string(),
        agent_type,
        description,
        parent_session_id: parent_session_id.to_string(),
        started_at,
        completed_at,
        status,
        prompt,
        result: result_text,
        agent_id,
        total_tokens,
        tool_uses,
        duration_ms,
    })
}

/// Parse `agentId: <hex>` out of an async-launch stub tool_result body.
/// Accepts the common forms: `agentId: abc123`, `agent_id: abc123`,
/// `"agentId":"abc123"`. Returns the first hex-ish run after the key.
fn parse_agent_id_from_stub(text: &str) -> Option<String> {
    const KEYS: &[&str] = &["agentId", "agent_id"];
    for key in KEYS {
        let mut search_from = 0usize;
        while let Some(rel) = text[search_from..].find(key) {
            let idx = search_from + rel + key.len();
            // Advance past any quote/colon/space/equals chars.
            let rest = text[idx..].trim_start_matches(|c: char| {
                matches!(c, ':' | '=' | ' ' | '\t' | '"' | '\'')
            });
            let candidate: String = rest
                .chars()
                .take_while(|c| c.is_ascii_hexdigit() || *c == '-' || *c == '_')
                .collect();
            // Minimum 6 chars of hex-like id to reduce false positives.
            if candidate.chars().filter(|c| c.is_ascii_hexdigit()).count() >= 6 {
                return Some(candidate);
            }
            search_from = idx;
        }
    }
    None
}

/// Concatenate user-message text content. Handles both plain string form
/// (`content: "..."`) and structured block form (`content: [{type:text, text:"..."}]`).
fn extract_user_message_text(content: Option<&Value>) -> String {
    let Some(content) = content else { return String::new() };
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(blocks) = content.as_array() {
        let mut out = String::new();
        for b in blocks {
            // Skip tool_result blocks — those are handled separately.
            if b.get("type").and_then(Value::as_str) == Some("tool_result") {
                continue;
            }
            if let Some(t) = b.get("text").and_then(Value::as_str) {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(t);
            }
        }
        return out;
    }
    String::new()
}

/// Extract the inner body of the first `<tag>…</tag>` found in `haystack`.
/// Non-greedy: stops at the first matching close tag. Returns `None` if the
/// tag is not present or unclosed.
fn extract_tag(haystack: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = haystack.find(&open)? + open.len();
    let rest = &haystack[start..];
    let end = rest.find(&close)?;
    Some(rest[..end].to_string())
}

fn collect_text_blocks(blocks: &[Value]) -> String {
    let mut out = String::new();
    for b in blocks {
        if b.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(t) = b.get("text").and_then(Value::as_str) {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(t);
            }
        }
    }
    out
}

fn extract_usage_stats(usage_str: &str) -> (Option<u64>, Option<u64>, Option<u64>) {
    let total_tokens = extract_tag(usage_str, "total_tokens")
        .and_then(|s| s.parse::<u64>().ok());
    let tool_uses = extract_tag(usage_str, "tool_uses")
        .and_then(|s| s.parse::<u64>().ok());
    let duration_ms = extract_tag(usage_str, "duration_ms")
        .and_then(|s| s.parse::<u64>().ok());
    (total_tokens, tool_uses, duration_ms)
}

/// Locate a parent session's JSONL under `~/.claude/projects/*/` and extract
/// the transcript for the named subagent tool_use id.
pub fn get_subagent_transcript(
    parent_session_id: &str,
    subagent_id: &str,
) -> Option<SubagentTranscript> {
    let home = dirs::home_dir()?;
    let projects_dir = home.join(".claude").join("projects");
    let project_iter = fs::read_dir(&projects_dir).ok()?;
    for project_entry in project_iter.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let Ok(file_iter) = fs::read_dir(&project_path) else { continue };
        for file_entry in file_iter.flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            if stem != parent_session_id {
                continue;
            }
            return extract_transcript_from_file(&path, parent_session_id, subagent_id);
        }
    }
    None
}

/// Build a map of parent_session_id -> subagents for all sessions found under
/// `~/.claude/projects/`. Caller filters/joins as needed.
pub fn all_subagents_by_session() -> HashMap<String, Vec<SubagentInfo>> {
    let mut out: HashMap<String, Vec<SubagentInfo>> = HashMap::new();
    let Some(home) = dirs::home_dir() else {
        return out;
    };
    let projects_dir = home.join(".claude").join("projects");
    let Ok(project_iter) = fs::read_dir(&projects_dir) else {
        return out;
    };
    for project_entry in project_iter.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let Ok(file_iter) = fs::read_dir(&project_path) else { continue };
        for file_entry in file_iter.flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            let subs = active_subagents_for_path(stem, &path);
            if !subs.is_empty() {
                out.insert(stem.to_string(), subs);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_jsonl(lines: &[&str]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        for line in lines {
            writeln!(f, "{}", line).unwrap();
        }
        f
    }

    #[test]
    fn detects_running_subagent() {
        let line = r#"{"type":"assistant","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","message":{"id":"m1","role":"assistant","model":"claude","content":[{"type":"tool_use","id":"toolu_running","name":"Agent","input":{"subagent_type":"general-purpose","description":"do thing","prompt":"..."}}],"stop_reason":null,"stop_sequence":null}}"#;
        let f = write_jsonl(&[line]);
        let subs = active_subagents_for_path("s1", f.path());
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].status, SubagentStatus::Running);
        assert_eq!(subs[0].agent_type, "general-purpose");
        assert_eq!(subs[0].description, "do thing");
        assert_eq!(subs[0].id, "toolu_running");
        assert_eq!(subs[0].parent_session_id, "s1");
        assert!(subs[0].completed_at.is_none());
    }

    #[test]
    fn detects_completed_subagent_via_user_tool_result() {
        // tool_use in assistant turn, then tool_result in user turn (the
        // standard CC pattern).
        let assistant = r#"{"type":"assistant","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","message":{"id":"m1","role":"assistant","model":"claude","content":[{"type":"tool_use","id":"toolu_done","name":"Agent","input":{"subagent_type":"Explore","description":"explore"}}],"stop_reason":null,"stop_sequence":null}}"#;
        let user = r#"{"type":"user","uuid":"u2","timestamp":"2026-01-01T00:01:00Z","sessionId":"s1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_done","content":"all done"}]}}"#;
        let f = write_jsonl(&[assistant, user]);
        let subs = active_subagents_for_path("s1", f.path());
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].status, SubagentStatus::Completed);
        assert_eq!(subs[0].completed_at.as_deref(), Some("2026-01-01T00:01:00Z"));
    }

    #[test]
    fn matches_task_tool_name_too() {
        // Some CC versions use "Task" instead of "Agent".
        let line = r#"{"type":"assistant","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","message":{"id":"m1","role":"assistant","model":"claude","content":[{"type":"tool_use","id":"toolu_task","name":"Task","input":{"subagent_type":"Explore","description":"task variant"}}],"stop_reason":null,"stop_sequence":null}}"#;
        let f = write_jsonl(&[line]);
        let subs = active_subagents_for_path("s1", f.path());
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].agent_type, "Explore");
    }

    #[test]
    fn ignores_non_subagent_tool_uses() {
        let line = r#"{"type":"assistant","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","message":{"id":"m1","role":"assistant","model":"claude","content":[{"type":"tool_use","id":"toolu_bash","name":"Bash","input":{"command":"ls"}}],"stop_reason":null,"stop_sequence":null}}"#;
        let f = write_jsonl(&[line]);
        let subs = active_subagents_for_path("s1", f.path());
        assert!(subs.is_empty());
    }

    #[test]
    fn handles_multiple_concurrent_subagents() {
        let line = r#"{"type":"assistant","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","message":{"id":"m1","role":"assistant","model":"claude","content":[{"type":"tool_use","id":"a1","name":"Agent","input":{"subagent_type":"x","description":"one"}},{"type":"tool_use","id":"a2","name":"Agent","input":{"subagent_type":"y","description":"two"}}],"stop_reason":null,"stop_sequence":null}}"#;
        let user = r#"{"type":"user","uuid":"u2","timestamp":"2026-01-01T00:01:00Z","sessionId":"s1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"a1","content":"done"}]}}"#;
        let f = write_jsonl(&[line, user]);
        let subs = active_subagents_for_path("s1", f.path());
        assert_eq!(subs.len(), 2);
        let by_id: HashMap<&str, &SubagentInfo> =
            subs.iter().map(|s| (s.id.as_str(), s)).collect();
        assert_eq!(by_id["a1"].status, SubagentStatus::Completed);
        assert_eq!(by_id["a2"].status, SubagentStatus::Running);
    }

    #[test]
    fn extract_transcript_completed_with_tool_use_result_stats() {
        // Assistant turn: Agent tool_use with prompt + description + subagent_type.
        let assistant = r#"{"type":"assistant","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","message":{"id":"m1","role":"assistant","model":"claude","content":[{"type":"tool_use","id":"toolu_t","name":"Agent","input":{"subagent_type":"Explore","description":"find it","prompt":"go look for X"}}],"stop_reason":null,"stop_sequence":null}}"#;
        // User turn: tool_result block + rich toolUseResult sibling with stats.
        let user = r#"{"type":"user","uuid":"u2","timestamp":"2026-01-01T00:02:00Z","sessionId":"s1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_t","content":[{"type":"text","text":"found X at file.rs"}]}]},"toolUseResult":{"status":"completed","agentId":"agent_abc","content":[{"type":"text","text":"found X at file.rs"}],"totalTokens":1234,"totalToolUseCount":5,"totalDurationMs":9876}}"#;
        let f = write_jsonl(&[assistant, user]);
        let tr = extract_transcript_from_file(f.path(), "s1", "toolu_t").unwrap();
        assert_eq!(tr.id, "toolu_t");
        assert_eq!(tr.agent_type, "Explore");
        assert_eq!(tr.description, "find it");
        assert_eq!(tr.prompt, "go look for X");
        assert_eq!(tr.status, SubagentStatus::Completed);
        assert_eq!(tr.result.as_deref(), Some("found X at file.rs"));
        assert_eq!(tr.agent_id.as_deref(), Some("agent_abc"));
        assert_eq!(tr.total_tokens, Some(1234));
        assert_eq!(tr.tool_uses, Some(5));
        assert_eq!(tr.duration_ms, Some(9876));
        assert_eq!(tr.completed_at.as_deref(), Some("2026-01-01T00:02:00Z"));
    }

    #[test]
    fn extract_transcript_running_has_no_result() {
        let assistant = r#"{"type":"assistant","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","message":{"id":"m1","role":"assistant","model":"claude","content":[{"type":"tool_use","id":"toolu_r","name":"Agent","input":{"subagent_type":"general-purpose","description":"running","prompt":"please do"}}],"stop_reason":null,"stop_sequence":null}}"#;
        let f = write_jsonl(&[assistant]);
        let tr = extract_transcript_from_file(f.path(), "s1", "toolu_r").unwrap();
        assert_eq!(tr.status, SubagentStatus::Running);
        assert!(tr.result.is_none());
        assert!(tr.completed_at.is_none());
        assert_eq!(tr.prompt, "please do");
    }

    #[test]
    fn extract_transcript_falls_back_to_tool_result_block_when_no_sibling() {
        let assistant = r#"{"type":"assistant","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","message":{"id":"m1","role":"assistant","model":"claude","content":[{"type":"tool_use","id":"toolu_fallback","name":"Agent","input":{"subagent_type":"x","description":"d","prompt":"p"}}],"stop_reason":null,"stop_sequence":null}}"#;
        // No toolUseResult sibling; content is a string.
        let user = r#"{"type":"user","uuid":"u2","timestamp":"2026-01-01T00:01:00Z","sessionId":"s1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_fallback","content":"plain string result"}]}}"#;
        let f = write_jsonl(&[assistant, user]);
        let tr = extract_transcript_from_file(f.path(), "s1", "toolu_fallback").unwrap();
        assert_eq!(tr.result.as_deref(), Some("plain string result"));
        assert_eq!(tr.status, SubagentStatus::Completed);
        assert!(tr.total_tokens.is_none());
    }

    #[test]
    fn extract_transcript_returns_none_for_unknown_id() {
        let assistant = r#"{"type":"assistant","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","message":{"id":"m1","role":"assistant","model":"claude","content":[{"type":"tool_use","id":"toolu_present","name":"Agent","input":{"subagent_type":"x","description":"d","prompt":"p"}}],"stop_reason":null,"stop_sequence":null}}"#;
        let f = write_jsonl(&[assistant]);
        let tr = extract_transcript_from_file(f.path(), "s1", "toolu_missing");
        assert!(tr.is_none());
    }

    #[test]
    fn extract_transcript_uses_later_async_notification_over_launch_stub() {
        // Pattern for async Task/Agent: immediate tool_result is a launch
        // stub with "agentId: <hex>", then later a user-role message arrives
        // with the real final report referencing that agentId.
        let assistant = r#"{"type":"assistant","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","message":{"id":"m1","role":"assistant","model":"claude","content":[{"type":"tool_use","id":"toolu_async","name":"Task","input":{"subagent_type":"general-purpose","description":"long job","prompt":"go"}}],"stop_reason":null,"stop_sequence":null}}"#;
        // Launch stub — agentId embedded in the text.
        let launch_stub = r#"{"type":"user","uuid":"u2","timestamp":"2026-01-01T00:00:01Z","sessionId":"s1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_async","content":"Async agent launched successfully.\nagentId: abc123def456"}]}}"#;
        let unrelated = r#"{"type":"assistant","uuid":"u3","timestamp":"2026-01-01T00:00:30Z","sessionId":"s1","message":{"id":"m2","role":"assistant","model":"claude","content":[{"type":"text","text":"meanwhile"}],"stop_reason":null,"stop_sequence":null}}"#;
        // Final report — plain user message, text references agentId.
        let final_report = r#"{"type":"user","uuid":"u4","timestamp":"2026-01-01T00:05:00Z","sessionId":"s1","message":{"role":"user","content":[{"type":"text","text":"<task-notification>agent abc123def456 completed: here is the final report content.</task-notification>"}]}}"#;
        let f = write_jsonl(&[assistant, launch_stub, unrelated, final_report]);
        let tr = extract_transcript_from_file(f.path(), "s1", "toolu_async").unwrap();
        assert_eq!(tr.status, SubagentStatus::Completed);
        assert_eq!(tr.agent_id.as_deref(), Some("abc123def456"));
        let result = tr.result.as_deref().unwrap_or("");
        assert!(
            result.contains("final report content"),
            "expected final report, got: {result}"
        );
        assert!(
            !result.starts_with("Async agent launched"),
            "should not use launch stub, got: {result}"
        );
        assert_eq!(tr.completed_at.as_deref(), Some("2026-01-01T00:05:00Z"));
    }

    #[test]
    fn parse_agent_id_from_stub_variants() {
        assert_eq!(
            parse_agent_id_from_stub("Async agent launched successfully.\nagentId: abc123def"),
            Some("abc123def".to_string())
        );
        assert_eq!(
            parse_agent_id_from_stub("\"agentId\":\"deadbeef\""),
            Some("deadbeef".to_string())
        );
        assert_eq!(parse_agent_id_from_stub("no id here"), None);
    }

    #[test]
    fn extract_transcript_async_queue_operation_final_report() {
        // Async launch: stub carries isAsync:true + agentId in toolUseResult.
        let assistant = r#"{"type":"assistant","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","message":{"id":"m1","role":"assistant","model":"claude","content":[{"type":"tool_use","id":"toolu_01MzKTSB","name":"Task","input":{"subagent_type":"general-purpose","description":"long","prompt":"go"}}],"stop_reason":null,"stop_sequence":null}}"#;
        let launch_stub = r#"{"type":"user","uuid":"u2","timestamp":"2026-01-01T00:00:01Z","sessionId":"s1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01MzKTSB","content":[{"type":"text","text":"Async agent launched successfully.\nagentId: a106ed7b5288e9506"}]}]},"toolUseResult":{"isAsync":true,"status":"async_launched","agentId":"a106ed7b5288e9506","description":"long","prompt":"go","outputFile":"/tmp/x","canReadOutputFile":true}}"#;
        let unrelated = r#"{"type":"assistant","uuid":"u3","timestamp":"2026-01-01T00:00:30Z","sessionId":"s1","message":{"id":"m2","role":"assistant","model":"claude","content":[{"type":"text","text":"meanwhile"}],"stop_reason":null,"stop_sequence":null}}"#;
        let queue_op = r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-01-01T00:05:00Z","sessionId":"s1","content":"<task-notification>\n<task-id>a106ed7b5288e9506</task-id>\n<tool-use-id>toolu_01MzKTSB</tool-use-id>\n<output-file>/tmp/x</output-file>\n<status>completed</status>\n<summary>done</summary>\n<result>FINAL REPORT HERE</result>\n<usage><total_tokens>47440</total_tokens><tool_uses>0</tool_uses><duration_ms>28873</duration_ms></usage>\n</task-notification>"}"#;
        let f = write_jsonl(&[assistant, launch_stub, unrelated, queue_op]);
        let tr = extract_transcript_from_file(f.path(), "s1", "toolu_01MzKTSB").unwrap();
        assert_eq!(tr.agent_id.as_deref(), Some("a106ed7b5288e9506"));
        assert_eq!(tr.result.as_deref(), Some("FINAL REPORT HERE"));
        assert_eq!(tr.total_tokens, Some(47440));
        assert_eq!(tr.tool_uses, Some(0));
        assert_eq!(tr.duration_ms, Some(28873));
        assert_eq!(tr.status, SubagentStatus::Completed);
        assert_eq!(tr.completed_at.as_deref(), Some("2026-01-01T00:05:00Z"));
    }

    #[test]
    fn extract_transcript_async_stub_only_is_still_running() {
        // Launch stub present, no completion event yet → Running + no stats.
        let assistant = r#"{"type":"assistant","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","message":{"id":"m1","role":"assistant","model":"claude","content":[{"type":"tool_use","id":"toolu_01MzKTSB","name":"Task","input":{"subagent_type":"general-purpose","description":"long","prompt":"go"}}],"stop_reason":null,"stop_sequence":null}}"#;
        let launch_stub = r#"{"type":"user","uuid":"u2","timestamp":"2026-01-01T00:00:01Z","sessionId":"s1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01MzKTSB","content":[{"type":"text","text":"Async agent launched successfully.\nagentId: a106ed7b5288e9506"}]}]},"toolUseResult":{"isAsync":true,"status":"async_launched","agentId":"a106ed7b5288e9506"}}"#;
        let f = write_jsonl(&[assistant, launch_stub]);
        let tr = extract_transcript_from_file(f.path(), "s1", "toolu_01MzKTSB").unwrap();
        assert_eq!(tr.agent_id.as_deref(), Some("a106ed7b5288e9506"));
        assert_eq!(tr.status, SubagentStatus::Running);
        assert!(tr.completed_at.is_none());
        assert!(tr.total_tokens.is_none());
        // Still exposes the launch stub text as a provisional result.
        let result = tr.result.as_deref().unwrap_or("");
        assert!(result.contains("Async agent launched"));
    }

    #[test]
    fn extract_transcript_async_user_message_with_task_notification() {
        // Event 3 pattern: completion arrives as a type:"user" message whose
        // text contains the full <task-notification> block.
        let assistant = r#"{"type":"assistant","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","message":{"id":"m1","role":"assistant","model":"claude","content":[{"type":"tool_use","id":"toolu_async","name":"Task","input":{"subagent_type":"general-purpose","description":"long","prompt":"go"}}],"stop_reason":null,"stop_sequence":null}}"#;
        let launch_stub = r#"{"type":"user","uuid":"u2","timestamp":"2026-01-01T00:00:01Z","sessionId":"s1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_async","content":"Async agent launched successfully.\nagentId: abc123def456"}]},"toolUseResult":{"isAsync":true,"status":"async_launched","agentId":"abc123def456"}}"#;
        let user_with_tn = r#"{"type":"user","uuid":"u3","timestamp":"2026-01-01T00:06:00Z","sessionId":"s1","message":{"role":"user","content":[{"type":"text","text":"<task-notification>\n<task-id>abc123def456</task-id>\n<tool-use-id>toolu_async</tool-use-id>\n<status>completed</status>\n<result>USER-ROUTED REPORT</result>\n<usage><total_tokens>100</total_tokens><tool_uses>2</tool_uses><duration_ms>5000</duration_ms></usage>\n</task-notification>"}]}}"#;
        let f = write_jsonl(&[assistant, launch_stub, user_with_tn]);
        let tr = extract_transcript_from_file(f.path(), "s1", "toolu_async").unwrap();
        assert_eq!(tr.agent_id.as_deref(), Some("abc123def456"));
        assert_eq!(tr.result.as_deref(), Some("USER-ROUTED REPORT"));
        assert_eq!(tr.total_tokens, Some(100));
        assert_eq!(tr.tool_uses, Some(2));
        assert_eq!(tr.duration_ms, Some(5000));
        assert_eq!(tr.status, SubagentStatus::Completed);
    }

    #[test]
    fn extract_tag_helper() {
        assert_eq!(
            extract_tag("<a>foo</a>", "a"),
            Some("foo".to_string())
        );
        assert_eq!(
            extract_tag("x<result>multi\nline\nbody</result>y", "result"),
            Some("multi\nline\nbody".to_string())
        );
        assert_eq!(extract_tag("<a>foo", "a"), None);
        assert_eq!(extract_tag("nothing here", "a"), None);
    }

    #[test]
    fn empty_or_missing_file_returns_empty() {
        let f = NamedTempFile::new().unwrap();
        let subs = active_subagents_for_path("s1", f.path());
        assert!(subs.is_empty());
        let subs = active_subagents_for_path("s1", "/nonexistent/path.jsonl");
        assert!(subs.is_empty());
    }
}
