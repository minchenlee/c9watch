//! Offline schema export, also available in a CLI-only build.

use serde_json::Value;
use std::collections::BTreeSet;

pub const GOLDEN: &str = include_str!("../../../docs/api/c9p-1.json");

pub fn embedded() -> Value {
    serde_json::from_str(GOLDEN).expect("embedded c9p schema is JSON")
}

/// Public docs omit private entries, private fields and unreachable types.
/// This changes documentation only, never the wire protocol.
pub fn public(mut schema: Value) -> Value {
    fn strip(v: &mut Value, codes: &regex::Regex) {
        match v {
            Value::Object(o) => {
                o.retain(|_, child| {
                    child["x-visibility"] != "internal" && child["x-stability"] != "internal"
                });
                // Internal planning references are not useful to API readers.
                if let Some(Value::String(s)) = o.get_mut("description") {
                    *s = codes.replace_all(s, "").into_owned();
                }
                for child in o.values_mut() {
                    strip(child, codes);
                }
                if let Some(Value::Array(required)) = o.get("required") {
                    let kept: Vec<Value> = required
                        .iter()
                        .filter(|k| o["properties"].get(k.as_str().unwrap_or("")).is_some())
                        .cloned()
                        .collect();
                    o.insert("required".into(), kept.into());
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| strip(x, codes)),
            _ => {}
        }
    }
    let codes = regex::Regex::new(r"\(?\b(?:A:\d+|C1[ab]-\d+|[DQN]\d+)\)?|§\d+(?:\.\d+)*").unwrap();
    strip(&mut schema, &codes);
    let defs = schema["$defs"].take();
    let mut reachable = BTreeSet::new();
    fn refs(v: &Value, defs: &Value, seen: &mut BTreeSet<String>) {
        match v {
            Value::Object(o) => {
                if let Some(name) = o
                    .get("$ref")
                    .and_then(Value::as_str)
                    .and_then(|s| s.strip_prefix("#/$defs/"))
                {
                    if seen.insert(name.into()) {
                        refs(&defs[name], defs, seen);
                    }
                }
                for child in o.values() {
                    refs(child, defs, seen);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| refs(x, defs, seen)),
            _ => {}
        }
    }
    refs(&schema, &defs, &mut reachable);
    schema["$defs"] = defs
        .as_object()
        .unwrap()
        .iter()
        .filter(|(name, _)| reachable.contains(*name))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<serde_json::Map<_, _>>()
        .into();
    schema
}

pub fn print(public_only: bool) -> Result<(), String> {
    let schema = if public_only {
        public(embedded())
    } else {
        embedded()
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&schema).map_err(|e| e.to_string())?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_schema_has_no_private_surface_or_orphan_hook_types() {
        let s = public(embedded());
        for name in ["hook.stop", "task.done", "agent.handoff"] {
            assert!(s["methods"].get(name).is_none());
        }
        assert!(s["topics"].get("legacy.inbox").is_none());
        assert!(s["$defs"].get("HookParams").is_none());
        for field in ["resumeCount", "interruptedNotePending", "stateReason"] {
            assert!(s["$defs"]["AgentRow"]["properties"].get(field).is_none());
            assert!(!s["$defs"]["AgentRow"]["required"]
                .as_array()
                .unwrap()
                .contains(&Value::from(field)));
        }
        assert_eq!(s["apiMinor"], embedded()["apiMinor"]);
    }
}
