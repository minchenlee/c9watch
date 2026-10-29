//! Facts read from the Claude Code session JSONL (CC 2.1.312 shapes), for
//! the state merge (plan C1a-22).
//!
//! Ported from the C0 spike (`transcript.rs:69-147`): each tool call with
//! its approval record, the calls the auto-mode classifier denied, and the
//! deferred tools / MCP servers the agent was given. Added here: the
//! activity each entry shows (`claim`), so the transcript is the second
//! source of the merge (hook > transcript > screen > process, A:1042), and
//! `system/turn_duration`, the second turn-end signal after the Stop hook
//! (D24.6).
//!
//! Everything here is pure: the caller reads the lines.

use super::merge::Activity;
use serde_json::Value;

#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub command: Option<String>,
    /// `permissionDecision` on the tool-result entry, e.g.
    /// {"decision":"accept","source":"config","reasonType":"classifier"}.
    pub decision: Option<Value>,
    pub sandboxed: Option<bool>,
}

/// CC 2.1.312 tool-result text when the auto-mode classifier denies a call.
pub const AUTO_DENY_TEXT: &str = "denied by the Claude Code auto mode classifier";

/// A tool call that the agent could not run and no human approved. The
/// engine reports it as `needs_attention`; it never answers (O1 = A).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Denial {
    pub tool_use_id: String,
    pub tool_name: String,
    pub command: Option<String>,
    /// The classifier's reason, from the text after `Reason: `.
    pub reason: String,
    pub decision: Option<Value>,
    pub result_text: String,
}

fn result_text(c: &Value) -> String {
    match &c["content"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Denied when the result text carries the classifier's denial, or the
/// entry's permissionDecision is not an accept.
fn denied(c: &Value, decision: &Value) -> Option<String> {
    let text = result_text(c);
    if let Some(i) = text.find(AUTO_DENY_TEXT) {
        let rest = &text[i..];
        // "Reason: [Code from External]. If you have other tasks ..." → first sentence.
        let reason = rest.split_once("Reason: ").map(|(_, r)| {
            let r = r.lines().next().unwrap_or("");
            r.split_once(". ").map_or(r, |(a, _)| a).trim().to_string()
        });
        return Some(reason.unwrap_or_else(|| "auto mode classifier denied".into()));
    }
    match decision["decision"].as_str() {
        Some(d) if !matches!(d, "accept" | "allow") => Some(format!("permissionDecision {}", d)),
        _ => None,
    }
}

/// One `system/turn_duration` row: CC wrote it when a turn ended.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TurnDuration {
    pub duration_ms: Option<i64>,
    /// The row's `timestamp`, unix ms.
    pub at_ms: Option<i64>,
}

/// What one transcript has shown so far.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Facts {
    pub tool_calls: Vec<ToolCall>,
    pub denials: Vec<Denial>,
    pub deferred_tools: Vec<String>,
    pub mcp_servers: Vec<String>,
    /// Every `system/turn_duration` row, in order.
    pub turn_ends: Vec<TurnDuration>,
}

impl Facts {
    /// Scan one JSONL entry. Returns the denials this entry adds.
    pub fn scan(&mut self, v: &Value) -> Vec<Denial> {
        let mut new = vec![];
        let content = v["message"]["content"].as_array();
        match v["type"].as_str() {
            Some("assistant") => {
                for c in content
                    .into_iter()
                    .flatten()
                    .filter(|c| c["type"] == "tool_use")
                {
                    let id = c["id"].as_str().unwrap_or_default().to_string();
                    if self.tool_calls.iter().any(|t| t.id == id) {
                        continue;
                    }
                    self.tool_calls.push(ToolCall {
                        id,
                        name: c["name"].as_str().unwrap_or_default().into(),
                        command: c["input"]["command"].as_str().map(String::from),
                        ..Default::default()
                    });
                }
            }
            Some("user") => {
                for c in content
                    .into_iter()
                    .flatten()
                    .filter(|c| c["type"] == "tool_result")
                {
                    let id = c["tool_use_id"].as_str().unwrap_or_default();
                    let decision = &v["permissionDecision"];
                    let mut call = None;
                    if let Some(t) = self.tool_calls.iter_mut().find(|t| t.id == id) {
                        if !decision.is_null() {
                            t.decision = Some(decision.clone());
                        }
                        t.sandboxed = v["sandboxed"].as_bool().or(t.sandboxed);
                        call = Some(t.clone());
                    }
                    if let Some(reason) = denied(c, decision) {
                        if !self.denials.iter().any(|d| d.tool_use_id == id) {
                            let d = Denial {
                                tool_use_id: id.to_string(),
                                tool_name: call
                                    .as_ref()
                                    .map(|t| t.name.clone())
                                    .unwrap_or_default(),
                                command: call.and_then(|t| t.command),
                                reason,
                                decision: (!decision.is_null()).then(|| decision.clone()),
                                result_text: result_text(c),
                            };
                            self.denials.push(d.clone());
                            new.push(d);
                        }
                    }
                }
            }
            Some("attachment") => {
                let a = &v["attachment"];
                let names = a["addedNames"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str);
                let into = match a["type"].as_str() {
                    Some("deferred_tools_delta") => &mut self.deferred_tools,
                    Some("mcp_instructions_delta") => &mut self.mcp_servers,
                    _ => return new,
                };
                for n in names {
                    if !into.iter().any(|x| x == n) {
                        into.push(n.into());
                    }
                }
            }
            Some("system") if v["subtype"] == "turn_duration" => {
                self.turn_ends.push(TurnDuration {
                    duration_ms: v["durationMs"].as_i64(),
                    at_ms: timestamp_ms(v),
                });
            }
            _ => {}
        }
        new
    }

    pub fn chrome_present(&self) -> bool {
        self.deferred_tools
            .iter()
            .chain(&self.mcp_servers)
            .any(|n| n.contains("claude-in-chrome"))
    }

    /// `reasonType` per Bash call, in order ("classifier", "rule", ...).
    pub fn bash_reasons(&self) -> Vec<String> {
        self.tool_calls
            .iter()
            .filter(|t| t.name == "Bash")
            .map(|t| {
                t.decision
                    .as_ref()
                    .and_then(|d| d["reasonType"].as_str())
                    .unwrap_or("none")
                    .to_string()
            })
            .collect()
    }
}

/// The activity one transcript entry shows, and when (the entry's own
/// `timestamp`, so a row read late cannot outvote newer evidence):
/// - `system/turn_duration`: the turn ended → `Ready` (D24.6, second after
///   the Stop hook).
/// - An assistant message or a tool result: the turn runs → `Working`.
/// - Anything else (attachments, meta rows, summaries): no claim.
///
/// A transcript never shows `Blocked`: CC writes nothing while a
/// permission prompt is open, so `blocked` comes from the screen (D24.5).
pub fn claim(v: &Value) -> Option<(Activity, Option<i64>)> {
    if v["isMeta"] == true || v["isSidechain"] == true {
        return None;
    }
    let activity = match v["type"].as_str()? {
        "system" if v["subtype"] == "turn_duration" => Activity::Ready,
        "assistant" => Activity::Working,
        "user" => Activity::Working,
        _ => return None,
    };
    Some((activity, timestamp_ms(v)))
}

/// The entry's `timestamp` (RFC 3339) as unix ms.
fn timestamp_ms(v: &Value) -> Option<i64> {
    let ts = v["timestamp"].as_str()?;
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pairs_tool_use_with_decision() {
        let mut f = Facts::default();
        f.scan(&json!({"type":"assistant","message":{"content":[{"type":"text","text":"x"},
            {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"echo \"a b\" | wc -w"}}]}}));
        f.scan(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Write","input":{"file_path":"/a"}}]}}));
        f.scan(&json!({"type":"user","sandboxed":false,
            "permissionDecision":{"decision":"accept","source":"config","reasonType":"classifier"},
            "message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}));
        assert_eq!(f.tool_calls.len(), 2);
        assert_eq!(
            f.tool_calls[0].command.as_deref(),
            Some("echo \"a b\" | wc -w")
        );
        assert_eq!(f.tool_calls[0].sandboxed, Some(false));
        assert_eq!(f.bash_reasons(), vec!["classifier"]);
        assert!(f.tool_calls[1].decision.is_none());
    }

    #[test]
    fn chrome_detection() {
        let mut f = Facts::default();
        f.scan(&json!({"type":"attachment","attachment":{"type":"deferred_tools_delta","addedNames":["Monitor","WebFetch"]}}));
        assert!(!f.chrome_present());
        f.scan(&json!({"type":"attachment","attachment":{"type":"mcp_instructions_delta","addedNames":["claude-in-chrome"]}}));
        assert!(f.chrome_present());
        let mut g = Facts::default();
        g.scan(&json!({"type":"attachment","attachment":{"type":"deferred_tools_delta","addedNames":["mcp__claude-in-chrome__navigate"]}}));
        assert!(g.chrome_present());
        g.scan(&json!({"type":"attachment","attachment":{"type":"deferred_tools_delta","addedNames":["mcp__claude-in-chrome__navigate"]}}));
        assert_eq!(g.deferred_tools.len(), 1);
    }

    #[test]
    fn turn_duration_is_a_turn_end_and_other_rows_are_working() {
        // The shape CC 2.1.299+ writes at the end of a turn.
        let end = json!({"type":"system","subtype":"turn_duration","durationMs":237125,
            "messageCount":141,"timestamp":"2026-10-14T09:11:47.021Z","isMeta":false});
        let mut f = Facts::default();
        f.scan(&end);
        assert_eq!(
            f.turn_ends,
            vec![TurnDuration {
                duration_ms: Some(237125),
                at_ms: Some(1_791_969_107_021)
            }]
        );
        assert_eq!(
            claim(&end),
            Some((Activity::Ready, Some(1_791_969_107_021)))
        );
        let tool = json!({"type":"assistant","timestamp":"2026-10-14T09:11:40Z",
            "message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}});
        assert_eq!(
            claim(&tool),
            Some((Activity::Working, Some(1_791_969_100_000)))
        );
        assert_eq!(
            claim(&json!({"type":"user","message":{"content":"hi"}})),
            Some((Activity::Working, None))
        );
        // No claim: attachments, meta rows, subagent rows, other system rows.
        for v in [
            json!({"type":"attachment","attachment":{"type":"deferred_tools_delta"}}),
            json!({"type":"user","isMeta":true,"message":{"content":"x"}}),
            json!({"type":"assistant","isSidechain":true,"message":{"content":[]}}),
            json!({"type":"system","subtype":"compact_boundary"}),
            json!({"type":"summary"}),
        ] {
            assert_eq!(claim(&v), None, "{}", v);
        }
    }
}
