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
    fn empty_or_missing_file_returns_empty() {
        let f = NamedTempFile::new().unwrap();
        let subs = active_subagents_for_path("s1", f.path());
        assert!(subs.is_empty());
        let subs = active_subagents_for_path("s1", "/nonexistent/path.jsonl");
        assert!(subs.is_empty());
    }
}
