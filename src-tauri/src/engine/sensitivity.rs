//! `x-sensitivity` on every field of every result and event type in the
//! schema. The gateway redacts by it.
//!
//! - `public`: ids, states, counts, flags, timestamps, versions.
//! - `metadata`: paths, project and model names, tool names, costs and
//!   token numbers, error text.
//! - `conversation`: prompts, messages, snippets, titles, session and agent
//!   names (they can be the first prompt), memory content, tool input.
//!
//! A field takes its class from `OVERRIDES` (`Type.field`), else from
//! `BY_NAME`. `unmarked` lists the fields that have none; a test keeps it
//! empty, so a new field must be classed before it ships.

use serde_json::Value;
use std::collections::BTreeSet;

pub const PUBLIC: &str = "public";
pub const METADATA: &str = "metadata";
pub const CONVERSATION: &str = "conversation";

const P: &str = PUBLIC;
const M: &str = METADATA;
const C: &str = CONVERSATION;

/// Classes by field name, for every type.
const BY_NAME: &[(&str, &str)] = &[
    // Ids, keys, states and flags.
    ("id", P),
    ("agentId", P),
    ("sessionId", P),
    ("sessionKey", P),
    ("parentSessionId", P),
    ("parentId", P),
    ("parentThreadId", P),
    ("rootSessionId", P),
    ("providerSid", P),
    ("msgId", P),
    ("taskId", P),
    ("toolUseId", P),
    ("machineId", P),
    ("generation", P),
    ("pid", P),
    ("parentClaudePid", P),
    ("provider", P),
    ("surface", P),
    ("agentKind", P),
    ("kind", P),
    ("internalKind", P),
    ("entrypoint", P),
    ("status", P),
    ("state", P),
    ("mode", P),
    ("control", P),
    ("role", P),
    ("source", P),
    ("sourceHealth", P),
    ("openTarget", P),
    ("messageType", P),
    ("mediaType", P),
    ("backend", P),
    ("by", P),
    ("canOpen", P),
    ("canRename", P),
    ("canStop", P),
    ("connected", P),
    ("costAvailable", P),
    ("partial", P),
    ("legacy", P),
    ("replayed", P),
    ("accepted", P),
    ("ended", P),
    ("ok", P),
    ("retryable", P),
    ("interruptedNotePending", P),
    ("fdaLikelyNeeded", P),
    ("code", P),
    // Frames and cursors.
    ("v", P),
    ("type", P),
    ("cursor", P),
    ("seq", P),
    ("sub", P),
    ("subject", P),
    ("topic", P),
    ("bytesRead", P),
    ("bytesTotal", P),
    ("total", P),
    ("offset", P),
    ("nextOffset", P),
    // Times.
    ("ts", P),
    ("at", P),
    ("timestamp", P),
    ("modified", P),
    ("date", P),
    ("createdAt", P),
    ("startedAt", P),
    ("startedAtMs", P),
    ("completedAt", P),
    ("goneAt", P),
    ("lastSeen", P),
    ("sinceMs", P),
    ("updatedAt", P),
    ("resetsAt", P),
    ("windowSeconds", P),
    ("durationMs", P),
    // Counts.
    ("attempts", P),
    ("resumeCount", P),
    ("messageCount", P),
    ("sessionCount", P),
    ("sessions", P),
    ("toolUses", P),
    ("toolCalls", P),
    ("commandsRun", P),
    ("filesEdited", P),
    ("subagentsCompleted", P),
    ("claudeProcessesFound", P),
    ("processesWithCwd", P),
    // The hello facts.
    ("apiMajor", P),
    ("apiMinor", P),
    ("engineVersion", P),
    ("principal", P),
    ("capabilities", P),
    ("methods", P),
    ("breakingSince", P),
    ("stability", P),
    ("topics", P),
    ("limits", P),
    ("since", P),
    ("rev", P),
    ("maxFrame", P),
    ("maxStreams", P),
    ("maxConnections", P),
    ("retentionHours", P),
    ("retentionEvents", P),
    // Paths, places and tools.
    ("cwd", M),
    ("project", M),
    ("projectPath", M),
    ("projectName", M),
    ("memoryDirPath", M),
    ("agentPath", M),
    ("filename", M),
    ("gitBranch", M),
    ("url", M),
    ("addr", M),
    ("to", M),
    ("from", M),
    ("createdBy", M),
    ("workerOf", M),
    ("agentType", M),
    ("agentRole", M),
    ("agentNickname", M),
    ("pendingTool", M),
    ("pendingToolName", M),
    ("waitingFor", M),
    ("tool", M),
    ("redactedTitle", M),
    ("hiddenClaudeSessions", M),
    // Money, tokens and models.
    ("cost", M),
    ("totalCost", M),
    ("totalTokens", M),
    ("unpricedTokens", M),
    ("inputTokens", M),
    ("outputTokens", M),
    ("cachedInputTokens", M),
    ("reasoningOutputTokens", M),
    ("percentage", M),
    ("usedPercent", M),
    ("model", M),
    ("displayName", M),
    ("plan", M),
    ("label", M),
    ("dailyCosts", M),
    ("modelCosts", M),
    ("projectCosts", M),
    ("usage", M),
    ("windows", M),
    ("activity", M),
    // Error and state text (can name paths).
    ("msg", M),
    ("hint", M),
    ("error", M),
    ("message", M),
    ("detail", M),
    ("reason", M),
    ("stateReason", M),
    ("origin", M),
    // Rows that carry names or paths.
    ("agent", C),
    ("agents", C),
    // Conversation text.
    ("name", C),
    ("sessionName", C),
    ("officialName", C),
    ("title", C),
    ("aiTitle", C),
    ("customTitle", C),
    ("codexTitle", C),
    ("cursorTitle", C),
    ("summary", C),
    ("firstPrompt", C),
    ("latestMessage", C),
    ("lastAssistantMessage", C),
    ("pendingToolInput", C),
    ("display", C),
    ("snippet", C),
    ("content", C),
    ("images", C),
    ("body", C),
    ("prompt", C),
    ("result", C),
    ("description", C),
    ("additionalContext", C),
    ("event", C),
    ("messages", C),
    ("entries", C),
    ("hits", C),
    ("projects", C),
    ("files", C),
    ("subagents", C),
    ("tasks", C),
    // Envelopes whose content depends on the method or topic: the worst
    // case. The method result or topic data type has the exact classes.
    ("data", C),
];

/// Classes for one type's field, where its name means something else.
const OVERRIDES: &[(&str, &str, &str)] = &[
    ("SubscriptionUsage", "name", M),
    ("ErrorBody", "data", M),
    ("HiddenClaudeSession", "reason", P),
    ("SessionListResult", "sessions", C),
    ("SessionViewsResult", "sessions", C),
];

pub fn class(def: &str, field: &str) -> Option<&'static str> {
    OVERRIDES
        .iter()
        .find(|(d, f, _)| *d == def && *f == field)
        .map(|(_, _, c)| *c)
        .or_else(|| BY_NAME.iter().find(|(f, _)| *f == field).map(|(_, c)| *c))
}

/// The `$defs` that the engine writes: reachable from a method's result
/// (or its other written parts), a topic's data, or a written frame.
pub fn written_defs(schema: &Value) -> BTreeSet<String> {
    let mut roots: Vec<&Value> = Vec::new();
    if let Some(methods) = schema["methods"].as_object() {
        for m in methods.values() {
            for (part, s) in m.as_object().into_iter().flatten() {
                if part != "params" && !part.starts_with("x-") {
                    roots.push(s);
                }
            }
        }
    }
    if let Some(topics) = schema["topics"].as_object() {
        roots.extend(topics.values().map(|t| &t["data"]));
    }
    if let Some(frames) = schema["frames"].as_object() {
        roots.extend(
            frames
                .iter()
                .filter(|(k, _)| *k != "request")
                .map(|(_, v)| v),
        );
    }
    let defs = &schema["$defs"];
    let mut seen = BTreeSet::new();
    let mut stack = roots;
    while let Some(v) = stack.pop() {
        match v {
            Value::Object(o) => {
                if let Some(name) = o
                    .get("$ref")
                    .and_then(Value::as_str)
                    .and_then(|r| r.strip_prefix("#/$defs/"))
                {
                    if seen.insert(name.to_string()) {
                        stack.push(&defs[name]);
                    }
                }
                stack.extend(o.values());
            }
            Value::Array(a) => stack.extend(a.iter()),
            _ => {}
        }
    }
    seen
}

/// The `properties` maps of one def: its own and its `oneOf` variants'.
fn property_maps(def: &mut Value) -> Vec<&mut serde_json::Map<String, Value>> {
    let mut out = Vec::new();
    let Some(o) = def.as_object_mut() else {
        return out;
    };
    for (k, v) in o.iter_mut() {
        match k.as_str() {
            "properties" => {
                if let Some(p) = v.as_object_mut() {
                    out.push(p);
                }
            }
            "oneOf" | "anyOf" => {
                for variant in v.as_array_mut().into_iter().flatten() {
                    if let Some(p) = variant.get_mut("properties").and_then(Value::as_object_mut) {
                        out.push(p);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Put `x-sensitivity` on every field of every written def.
pub fn mark(schema: &mut Value) {
    let written = written_defs(schema);
    let Some(defs) = schema["$defs"].as_object_mut() else {
        return;
    };
    for name in &written {
        let Some(def) = defs.get_mut(name) else {
            continue;
        };
        for props in property_maps(def) {
            for (field, s) in props.iter_mut() {
                let Some(c) = class(name, field) else {
                    continue;
                };
                // `true` (any value) becomes the equal `{}`, to hold the mark.
                if s == &Value::Bool(true) {
                    *s = Value::Object(Default::default());
                }
                if let Some(o) = s.as_object_mut() {
                    o.insert("x-sensitivity".into(), c.into());
                }
            }
        }
    }
}

/// `Type.field` of every written field without `x-sensitivity`.
pub fn unmarked(schema: &Value) -> Vec<String> {
    let mut schema = schema.clone();
    let written = written_defs(&schema);
    let mut out = Vec::new();
    let defs = schema["$defs"].as_object_mut().expect("$defs");
    for name in &written {
        let Some(def) = defs.get_mut(name) else {
            continue;
        };
        for props in property_maps(def) {
            for (field, s) in props.iter() {
                let c = s.get("x-sensitivity").and_then(Value::as_str);
                if !matches!(c, Some(PUBLIC | METADATA | CONVERSATION)) {
                    out.push(format!("{}.{}", name, field));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_win_over_names() {
        assert_eq!(class("SubscriptionUsage", "name"), Some(METADATA));
        assert_eq!(class("AgentRow", "name"), Some(CONVERSATION));
        assert_eq!(class("DailyCost", "sessions"), Some(PUBLIC));
        assert_eq!(class("X", "no_such_field"), None);
    }

    /// A field nobody classed is reported by `unmarked`.
    #[test]
    fn an_unclassed_field_is_reported() {
        let mut s = serde_json::json!({
            "methods": {"a.b": {"params": {}, "result": {"$ref": "#/$defs/R"}}},
            "topics": {},
            "frames": {},
            "$defs": {
                "R": {"type": "object", "properties": {
                    "id": {"type": "string"},
                    "brandNew": {"type": "string"},
                    "inner": {"$ref": "#/$defs/I"},
                }},
                "I": {"oneOf": [{"type": "object", "properties": {"kind": {"const": "x"}}}]},
                "ParamsOnly": {"type": "object", "properties": {"zzz": {}}},
            },
        });
        mark(&mut s);
        assert_eq!(unmarked(&s), ["R.brandNew", "R.inner"]);
        assert_eq!(
            s["$defs"]["R"]["properties"]["id"]["x-sensitivity"],
            "public"
        );
        assert_eq!(
            s["$defs"]["I"]["oneOf"][0]["properties"]["kind"]["x-sensitivity"],
            "public"
        );
    }

    /// Every field of every result and event type in the c9p/1 schema has
    /// a class.
    #[test]
    fn every_written_field_has_a_class() {
        let s = crate::engine::api::schema();
        assert_eq!(unmarked(&s), Vec::<String>::new());
        assert!(written_defs(&s).contains("SessionView"));
        assert!(written_defs(&s).contains("MsgStateEvent"));
    }
}
