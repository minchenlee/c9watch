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
    /// Element-object schemas extracted from the script's inline JSON-Schema
    /// literals. Empty when the script has none or nothing parses.
    pub result_schemas: Vec<ResultSchema>,
}

/// One property of a schema's element object.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FieldMeta {
    pub name: String,
    /// JSON-Schema `type` string: "string" | "number" | "integer" | "boolean" | "array" | "object" | "".
    pub ty: String,
    /// Present when the schema constrains the value to an enum of strings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enum_vals: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The element-object shape extracted from one inline JSON-Schema in the script.
/// Used by the frontend to shape-match a result array-of-objects.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResultSchema {
    /// Property names of the element object, in schema order.
    pub keys: Vec<String>,
    pub props: Vec<FieldMeta>,
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

// ── Script schema extraction ────────────────────────────────────────────────

/// Return the balanced `{...}` substring starting at byte index `start`
/// (which must point at a `{`). String-aware: braces inside '...', "...",
/// or `...` are ignored, and `\` escapes the next char inside a string.
/// Returns `None` if the braces never balance.
fn balanced_brace_span(s: &str, start: usize) -> Option<&str> {
    let bytes = s.as_bytes();
    if bytes.get(start) != Some(&b'{') {
        return None;
    }
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
        } else {
            match c {
                b'\'' | b'"' | b'`' => quote = Some(c),
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&s[start..=i]);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Best-effort convert a JS object literal to JSON. String-aware. Handles:
/// single/backtick quotes -> double, bare identifier keys -> quoted, line and
/// block comments stripped, trailing commas removed. Not a full JS parser;
/// good enough for the schema literals workflows emit.
fn js_literal_to_json(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len() + 16);
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            // ── string literal: re-emit as a double-quoted JSON string ──
            b'\'' | b'"' | b'`' => {
                let q = c;
                out.push('"');
                i += 1;
                while i < bytes.len() {
                    let d = bytes[i];
                    if d == b'\\' && i + 1 < bytes.len() {
                        let n = bytes[i + 1];
                        if n == q && q != b'"' {
                            out.push(n as char);
                        } else {
                            out.push('\\');
                            out.push(n as char);
                        }
                        i += 2;
                        continue;
                    }
                    if d == q {
                        i += 1;
                        break;
                    }
                    if d == b'"' {
                        out.push_str("\\\"");
                    } else if d == b'\n' {
                        out.push_str("\\n");
                    } else {
                        out.push(d as char);
                    }
                    i += 1;
                }
                out.push('"');
            }
            // ── comments ──
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            // ── trailing comma: lookahead past whitespace to a } or ] ──
            b',' => {
                let mut j = i + 1;
                while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                    j += 1;
                }
                if bytes.get(j) == Some(&b'}') || bytes.get(j) == Some(&b']') {
                    // skip the trailing comma
                } else {
                    out.push(',');
                }
                i += 1;
            }
            // ── bare identifier: a key if followed (after ws) by ':' ──
            _ if c == b'_' || c == b'$' || c.is_ascii_alphabetic() => {
                let start = i;
                while i < bytes.len() {
                    let d = bytes[i];
                    if d == b'_' || d == b'$' || d.is_ascii_alphanumeric() {
                        i += 1;
                    } else {
                        break;
                    }
                }
                let ident = &src[start..i];
                let mut j = i;
                while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                    j += 1;
                }
                if bytes.get(j) == Some(&b':') {
                    out.push('"');
                    out.push_str(ident);
                    out.push('"');
                } else {
                    out.push_str(ident);
                }
            }
            _ => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

/// From just after a `schema` token, if the next non-ws (after any trailing
/// identifier chars like `_SCHEMA`) is `=` or `:`, return the offset of the `{`
/// that opens the literal. None if the next token is not an object literal
/// (e.g. `schema: SOME_REF`).
fn next_assignment_brace(script: &str, mut i: usize) -> Option<usize> {
    let b = script.as_bytes();
    while i < b.len() && (b[i] == b'_' || b[i] == b'$' || b[i].is_ascii_alphanumeric()) {
        i += 1;
    }
    while i < b.len() && (b[i] as char).is_whitespace() {
        i += 1;
    }
    if b.get(i) != Some(&b'=') && b.get(i) != Some(&b':') {
        return None;
    }
    i += 1;
    while i < b.len() && (b[i] as char).is_whitespace() {
        i += 1;
    }
    if b.get(i) == Some(&b'{') {
        Some(i)
    } else {
        None
    }
}

/// Find all anchor offsets where a schema literal's `{` begins. Anchors: any
/// `schema` token (covers `const X_SCHEMA = {` and inline `schema: {`) that is
/// followed by `= {` or `: {`. Returns the byte offset of each opening `{`.
fn schema_anchor_offsets(script: &str) -> Vec<usize> {
    let lower = script.to_lowercase();
    let mut offsets = Vec::new();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("schema") {
        let kw = from + rel;
        let after = kw + "schema".len();
        if let Some(brace) = next_assignment_brace(script, after) {
            offsets.push(brace);
        }
        from = after;
    }
    offsets.sort_unstable();
    offsets.dedup();
    offsets
}

/// Walk a parsed schema `Value` to the element object's properties. Handles
/// `{type:array, items:{object}}`, `{properties:{ k:{type:array, items} }}`
/// (descend one level), and a plain `{type:object, properties}`.
fn element_props(schema: &serde_json::Value) -> Option<ResultSchema> {
    fn props_of(obj: &serde_json::Value) -> Option<&serde_json::Map<String, serde_json::Value>> {
        obj.get("properties").and_then(|p| p.as_object())
    }
    if schema.get("type").and_then(|t| t.as_str()) == Some("array") {
        let items = schema.get("items")?;
        return element_props(items);
    }
    let props = props_of(schema)?;
    if props.len() == 1 {
        let only = props.values().next().unwrap();
        if only.get("type").and_then(|t| t.as_str()) == Some("array") {
            if let Some(items) = only.get("items") {
                if let Some(inner) = element_props(items) {
                    return Some(inner);
                }
            }
        }
    }
    let mut keys = Vec::new();
    let mut metas = Vec::new();
    for (k, v) in props {
        keys.push(k.clone());
        metas.push(FieldMeta {
            name: k.clone(),
            ty: v
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            enum_vals: v.get("enum").and_then(|e| e.as_array()).map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            }),
            description: v
                .get("description")
                .and_then(|d| d.as_str())
                .map(String::from),
        });
    }
    if keys.is_empty() {
        None
    } else {
        Some(ResultSchema { keys, props: metas })
    }
}

/// Extract every inline JSON-Schema's element-object shape from a workflow script.
pub fn extract_result_schemas(script: &str) -> Vec<ResultSchema> {
    let mut out: Vec<ResultSchema> = Vec::new();
    for off in schema_anchor_offsets(script) {
        let Some(span) = balanced_brace_span(script, off) else {
            continue;
        };
        let json = js_literal_to_json(span);
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) else {
            continue;
        };
        if let Some(rs) = element_props(&value) {
            if !out.iter().any(|e| e.keys == rs.keys) {
                out.push(rs);
            }
        }
    }
    out
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

    let result_schemas = extract_result_schemas(&raw.script);

    Some(WorkflowDetail {
        summary,
        phases,
        agents,
        script: raw.script,
        result_json,
        result_schemas,
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

    // ── Script schema extraction ─────────────────────────────────────

    #[test]
    fn balanced_span_skips_braces_in_strings() {
        let s = r#"x = { a: '}{', b: { c: 1 } } ;"#;
        let start = s.find('{').unwrap();
        let span = balanced_brace_span(s, start).expect("span");
        assert_eq!(span, r#"{ a: '}{', b: { c: 1 } }"#);
    }

    #[test]
    fn balanced_span_handles_escaped_quote() {
        let s = r#"{ a: 'it\'s {' }"#;
        let span = balanced_brace_span(s, 0).expect("span");
        assert_eq!(span, r#"{ a: 'it\'s {' }"#);
    }

    #[test]
    fn balanced_span_none_when_unterminated() {
        assert!(balanced_brace_span("{ a: 1", 0).is_none());
    }

    #[test]
    fn normalizes_js_literal_to_json() {
        let js = r#"{ type: 'object', properties: { findings: { type: 'array' } }, }"#;
        let json = js_literal_to_json(js);
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(v["type"], "object");
        assert_eq!(v["properties"]["findings"]["type"], "array");
    }

    #[test]
    fn normalizes_enum_array_and_strips_trailing_comma() {
        let js = r#"{ severity: { type: 'string', enum: ['bug', 'warning', 'nit',] } }"#;
        let json = js_literal_to_json(js);
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(v["severity"]["enum"][0], "bug");
        assert_eq!(v["severity"]["enum"][2], "nit");
    }

    #[test]
    fn keeps_braces_and_punctuation_inside_strings() {
        let js = r#"{ description: 'a {b}, c: d' }"#;
        let json = js_literal_to_json(js);
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(v["description"], "a {b}, c: d");
    }

    const SCRIPT_WITH_SCHEMA: &str = r#"
        export const meta = { name: 'x' }
        const FINDINGS_SCHEMA = {
          type: 'object',
          properties: {
            findings: {
              type: 'array',
              items: {
                type: 'object',
                properties: {
                  title: { type: 'string' },
                  severity: { type: 'string', enum: ['bug', 'warning', 'nit'] },
                  detail: { type: 'string', description: 'why it is a {bug}' },
                },
                required: ['title', 'severity'],
              },
            },
          },
        }
        const x = await agent('go', { schema: FINDINGS_SCHEMA })
    "#;

    #[test]
    fn extracts_element_shape_from_array_schema() {
        let schemas = extract_result_schemas(SCRIPT_WITH_SCHEMA);
        assert_eq!(schemas.len(), 1, "one schema literal");
        let s = &schemas[0];
        let mut keys = s.keys.clone();
        keys.sort();
        assert_eq!(keys, vec!["detail", "severity", "title"]);
        let sev = s.props.iter().find(|p| p.name == "severity").unwrap();
        assert_eq!(sev.ty, "string");
        assert_eq!(
            sev.enum_vals,
            Some(vec!["bug".into(), "warning".into(), "nit".into()])
        );
        let detail = s.props.iter().find(|p| p.name == "detail").unwrap();
        assert_eq!(detail.description.as_deref(), Some("why it is a {bug}"));
    }

    #[test]
    fn extracts_top_level_object_schema() {
        let script = r#"const VERDICT_SCHEMA = {
            type: 'object',
            properties: { isReal: { type: 'boolean' }, reasoning: { type: 'string' } },
        }"#;
        let schemas = extract_result_schemas(script);
        assert_eq!(schemas.len(), 1);
        let mut keys = schemas[0].keys.clone();
        keys.sort();
        assert_eq!(keys, vec!["isReal", "reasoning"]);
    }

    #[test]
    fn returns_empty_when_no_schema() {
        assert!(extract_result_schemas("const x = 1; return x;").is_empty());
    }

    #[test]
    fn skips_unparseable_literal_without_failing() {
        let script = "const A_SCHEMA = { type: 'object', properties: { ";
        assert!(extract_result_schemas(script).is_empty());
    }

    #[test]
    fn detail_carries_result_schemas() {
        let json = format!(
            r#"{{"runId":"wf_s","status":"completed","script":{script},"result":{{"x":1}}}}"#,
            script = serde_json::to_string(SCRIPT_WITH_SCHEMA).unwrap()
        );
        let d = parse_detail(&json, "p", "/p").expect("parse");
        assert_eq!(d.result_schemas.len(), 1);
        assert!(d.result_schemas[0].keys.contains(&"severity".to_string()));
    }

    #[test]
    fn decode_project_dir_roundtrip() {
        assert_eq!(
            decode_project_dir("-Users-liminchen-Documents-GitHub-c9watch"),
            "/Users/liminchen/Documents/GitHub/c9watch"
        );
    }
}
