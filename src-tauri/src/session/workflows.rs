//! Reads Claude Code *workflow* run journals and exposes them for the
//! WORKFLOWS dashboard tab.
//!
//! The `Workflow` tool persists every run as a single JSON file at
//! `~/.claude/projects/<encoded-project-dir>/<session-uuid>/workflows/wf_*.json`.
//! Each file is updated in place as the run progresses, so polling these files
//! is enough to drive both the live ("ongoing") and historical views.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Lightweight row for the workflow list view. Excludes the heavy
/// `script` / `progress` / `result` payloads so the list stays cheap.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSummary {
    pub run_id: String,
    pub workflow_name: String,
    pub summary: String,
    /// `running` | `completed` | `failed` (verbatim from the journal)
    pub status: String,
    /// Epoch milliseconds the run started.
    pub start_time: i64,
    pub duration_ms: i64,
    pub agent_count: i64,
    pub total_tokens: i64,
    pub total_tool_calls: i64,
    pub default_model: String,
    pub phase_count: usize,
    /// Human-readable project name (last path segment).
    pub project_name: String,
    /// Decoded full project path.
    pub project_path: String,
}

/// One agent within a workflow run (from a `workflow_agent` progress event).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAgent {
    pub label: String,
    pub phase_title: String,
    pub state: String,
    pub model: String,
    pub tokens: i64,
    pub tool_calls: i64,
    pub duration_ms: i64,
    pub last_tool_name: String,
    pub prompt_preview: String,
    pub result_preview: String,
}

/// Full detail for one run, including agents, script and result.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDetail {
    #[serde(flatten)]
    pub summary: WorkflowSummary,
    pub phases: Vec<String>,
    pub agents: Vec<WorkflowAgent>,
    pub script: String,
    /// Pretty-printed JSON of the workflow's `result`, or empty if none.
    pub result_json: String,
}

// ── Raw journal shapes (defensive: every field optional / defaulted) ────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawWorkflow {
    #[serde(default)]
    run_id: String,
    #[serde(default)]
    workflow_name: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    start_time: i64,
    #[serde(default)]
    duration_ms: i64,
    #[serde(default)]
    agent_count: i64,
    #[serde(default)]
    total_tokens: i64,
    #[serde(default)]
    total_tool_calls: i64,
    #[serde(default)]
    default_model: String,
    #[serde(default)]
    phases: Vec<RawPhase>,
    #[serde(default)]
    workflow_progress: Vec<RawProgress>,
    #[serde(default)]
    script: String,
    #[serde(default)]
    result: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct RawPhase {
    #[serde(default)]
    title: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProgress {
    #[serde(rename = "type", default)]
    event_type: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    phase_title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    tokens: i64,
    #[serde(default)]
    tool_calls: i64,
    #[serde(default)]
    duration_ms: i64,
    #[serde(default)]
    last_tool_name: String,
    #[serde(default)]
    prompt_preview: String,
    #[serde(default)]
    result_preview: String,
}

/// Decode a Claude projects directory name back to a real path.
/// e.g. "-Users-liminchen-Documents-GitHub-c9watch" → "/Users/liminchen/Documents/GitHub/c9watch"
fn decode_project_dir(dir_name: &str) -> String {
    if dir_name.starts_with('-') {
        format!("/{}", dir_name[1..].replace('-', "/"))
    } else {
        dir_name.replace('-', "/")
    }
}

fn project_name_from_path(decoded_path: &str, fallback: &str) -> String {
    decoded_path
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// Parse a journal JSON string into a summary, tagging it with the project.
/// Returns `None` if the JSON is unparseable or has no `runId`.
fn parse_summary(json: &str, project_name: &str, project_path: &str) -> Option<WorkflowSummary> {
    let raw: RawWorkflow = serde_json::from_str(json).ok()?;
    if raw.run_id.is_empty() {
        return None;
    }
    Some(WorkflowSummary {
        run_id: raw.run_id,
        workflow_name: raw.workflow_name,
        summary: raw.summary,
        status: raw.status,
        start_time: raw.start_time,
        duration_ms: raw.duration_ms,
        agent_count: raw.agent_count,
        total_tokens: raw.total_tokens,
        total_tool_calls: raw.total_tool_calls,
        default_model: raw.default_model,
        phase_count: raw.phases.len(),
        project_name: project_name.to_string(),
        project_path: project_path.to_string(),
    })
}

/// Parse a journal JSON string into full detail.
fn parse_detail(json: &str, project_name: &str, project_path: &str) -> Option<WorkflowDetail> {
    let summary = parse_summary(json, project_name, project_path)?;
    let raw: RawWorkflow = serde_json::from_str(json).ok()?;

    let phases: Vec<String> = raw.phases.into_iter().map(|p| p.title).collect();
    let agents: Vec<WorkflowAgent> = raw
        .workflow_progress
        .into_iter()
        .filter(|e| e.event_type == "workflow_agent")
        .map(|e| WorkflowAgent {
            label: e.label,
            phase_title: e.phase_title,
            state: e.state,
            model: e.model,
            tokens: e.tokens,
            tool_calls: e.tool_calls,
            duration_ms: e.duration_ms,
            last_tool_name: e.last_tool_name,
            prompt_preview: e.prompt_preview,
            result_preview: e.result_preview,
        })
        .collect();

    let result_json = if raw.result.is_null() {
        String::new()
    } else {
        serde_json::to_string_pretty(&raw.result).unwrap_or_default()
    };

    Some(WorkflowDetail {
        summary,
        phases,
        agents,
        script: raw.script,
        result_json,
    })
}

/// `running` runs sort first; within a status group, newest `start_time` first.
fn sort_workflows(list: &mut [WorkflowSummary]) {
    list.sort_by(|a, b| {
        let a_running = a.status == "running";
        let b_running = b.status == "running";
        b_running
            .cmp(&a_running)
            .then(b.start_time.cmp(&a.start_time))
    });
}

/// Scan all project workflow dirs and return summaries, running-first then newest-first.
pub fn list_workflows() -> Result<Vec<WorkflowSummary>, String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    let projects_dir = home_dir.join(".claude").join("projects");
    if !projects_dir.exists() {
        return Ok(Vec::new());
    }

    let mut results: Vec<WorkflowSummary> = Vec::new();
    collect(&projects_dir, &mut |json, name, path| {
        if let Some(s) = parse_summary(json, name, path) {
            results.push(s);
        }
    })?;

    sort_workflows(&mut results);
    Ok(results)
}

/// Look up one run's full detail by `runId` across all project dirs.
pub fn get_workflow_detail(run_id: &str) -> Result<WorkflowDetail, String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    let projects_dir = home_dir.join(".claude").join("projects");

    let mut found: Option<WorkflowDetail> = None;
    collect(&projects_dir, &mut |json, name, path| {
        if found.is_some() {
            return;
        }
        if let Some(d) = parse_detail(json, name, path) {
            if d.summary.run_id == run_id {
                found = Some(d);
            }
        }
    })?;

    found.ok_or_else(|| format!("Workflow not found: {}", run_id))
}

/// Walk `projects/*/<session>/workflows/wf_*.json`, invoking `f(json, project_name, project_path)`.
fn collect(
    projects_dir: &Path,
    f: &mut dyn FnMut(&str, &str, &str),
) -> Result<(), String> {
    if !projects_dir.exists() {
        return Ok(());
    }
    let entries =
        fs::read_dir(projects_dir).map_err(|e| format!("Failed to read projects dir: {}", e))?;

    for entry in entries.flatten() {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        let dir_name = project_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let decoded_path = decode_project_dir(&dir_name);
        let project_name = project_name_from_path(&decoded_path, &dir_name);

        // Each session-uuid subdir may hold a `workflows/` dir.
        let session_dirs = match fs::read_dir(&project_dir) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for sess in session_dirs.flatten() {
            let wf_dir = sess.path().join("workflows");
            if !wf_dir.is_dir() {
                continue;
            }
            if let Ok(files) = fs::read_dir(&wf_dir) {
                for file in files.flatten() {
                    let p = file.path();
                    let is_wf = p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("wf_") && n.ends_with(".json"))
                        .unwrap_or(false);
                    if !is_wf {
                        continue;
                    }
                    if let Ok(content) = fs::read_to_string(&p) {
                        f(&content, &project_name, &decoded_path);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(run_id: &str, status: &str, start: i64) -> String {
        format!(
            r#"{{
              "runId": "{run_id}",
              "workflowName": "demo-flow",
              "summary": "A demo workflow",
              "status": "{status}",
              "startTime": {start},
              "durationMs": 1234,
              "agentCount": 2,
              "totalTokens": 5000,
              "totalToolCalls": 9,
              "defaultModel": "claude-opus-4-8[1m]",
              "phases": [{{"title": "Find"}}, {{"title": "Verify"}}],
              "workflowProgress": [
                {{"type": "workflow_phase", "index": 1, "title": "Find"}},
                {{"type": "workflow_agent", "label": "find:a", "phaseTitle": "Find",
                  "state": "done", "model": "claude-opus-4-8[1m]", "tokens": 3000,
                  "toolCalls": 5, "durationMs": 800, "lastToolName": "StructuredOutput",
                  "promptPreview": "find bugs", "resultPreview": "{{found:1}}"}}
              ],
              "script": "export const meta = {{}}",
              "result": {{"confirmed": 1}}
            }}"#
        )
    }

    #[test]
    fn parses_summary_fields() {
        let s = parse_summary(&fixture("wf_a", "completed", 1000), "c9watch", "/x/c9watch")
            .expect("should parse");
        assert_eq!(s.run_id, "wf_a");
        assert_eq!(s.workflow_name, "demo-flow");
        assert_eq!(s.status, "completed");
        assert_eq!(s.agent_count, 2);
        assert_eq!(s.total_tokens, 5000);
        assert_eq!(s.phase_count, 2);
        assert_eq!(s.project_name, "c9watch");
        assert_eq!(s.project_path, "/x/c9watch");
    }

    #[test]
    fn skips_json_without_run_id() {
        assert!(parse_summary(r#"{"workflowName":"x"}"#, "p", "/p").is_none());
    }

    #[test]
    fn skips_malformed_json() {
        assert!(parse_summary("{not json", "p", "/p").is_none());
    }

    #[test]
    fn detail_extracts_only_agent_events() {
        let d = parse_detail(&fixture("wf_a", "completed", 1000), "p", "/p").expect("parse");
        assert_eq!(d.phases, vec!["Find".to_string(), "Verify".to_string()]);
        assert_eq!(d.agents.len(), 1, "phase events must be filtered out");
        assert_eq!(d.agents[0].label, "find:a");
        assert_eq!(d.agents[0].tokens, 3000);
        assert_eq!(d.agents[0].last_tool_name, "StructuredOutput");
        assert!(d.result_json.contains("confirmed"));
        assert!(d.script.contains("meta"));
    }

    #[test]
    fn running_sorts_before_completed() {
        let mut v = vec![
            parse_summary(&fixture("wf_old", "completed", 100), "p", "/p").unwrap(),
            parse_summary(&fixture("wf_run", "running", 50), "p", "/p").unwrap(),
        ];
        sort_workflows(&mut v);
        assert_eq!(v[0].run_id, "wf_run", "running must come first even if older");
        assert_eq!(v[1].run_id, "wf_old");
    }

    #[test]
    fn within_status_newest_first() {
        let mut v = vec![
            parse_summary(&fixture("wf_a", "completed", 100), "p", "/p").unwrap(),
            parse_summary(&fixture("wf_b", "completed", 300), "p", "/p").unwrap(),
            parse_summary(&fixture("wf_c", "completed", 200), "p", "/p").unwrap(),
        ];
        sort_workflows(&mut v);
        assert_eq!(
            v.iter().map(|w| w.run_id.as_str()).collect::<Vec<_>>(),
            vec!["wf_b", "wf_c", "wf_a"]
        );
    }

    #[test]
    fn decode_project_dir_roundtrip() {
        assert_eq!(
            decode_project_dir("-Users-liminchen-Documents-GitHub-c9watch"),
            "/Users/liminchen/Documents/GitHub/c9watch"
        );
    }
}
