//! Schema-driven egress projection. Unknown fields and opaque objects have
//! no output authority, even at the conversation ceiling.
use serde_json::{Map, Value};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sensitivity {
    Public,
    Metadata,
    Conversation,
}
impl Sensitivity {
    fn parse(value: &Value) -> Option<Self> {
        match value.as_str() {
            Some("public") => Some(Self::Public),
            Some("metadata") => Some(Self::Metadata),
            Some("conversation") => Some(Self::Conversation),
            _ => None,
        }
    }
}
pub fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../docs/api/c9p-1.json"
        )))
        .expect("embedded schema contract")
    })
}
fn resolve<'a>(schema: &'a Value, root: &'a Value) -> &'a Value {
    schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix("#/$defs/"))
        .and_then(|n| root["$defs"].get(n))
        .unwrap_or(schema)
}
fn fits(value: &Value, schema: &Value, root: &Value, depth: usize) -> bool {
    if depth > 64 {
        return false;
    }
    let schema = resolve(schema, root);
    if let Some(c) = schema.get("const") {
        return value == c;
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values.contains(value);
    }
    if let Some(alts) = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)
    {
        return alts.iter().any(|s| fits(value, s, root, depth + 1));
    }
    let type_matches = |name: &str| match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        _ => false,
    };
    if let Some(t) = schema.get("type") {
        let matched = t.as_str().is_some_and(type_matches)
            || t.as_array()
                .is_some_and(|a| a.iter().filter_map(Value::as_str).any(type_matches));
        if !matched {
            return false;
        }
    }
    if let Some(obj) = value.as_object() {
        if schema["required"].as_array().is_some_and(|required| {
            required
                .iter()
                .filter_map(Value::as_str)
                .any(|k| !obj.contains_key(k))
        }) {
            return false;
        }
        // Discriminator constants select the correct object variant.
        if let Some(props) = schema["properties"].as_object() {
            for (k, s) in props {
                if s.get("const").is_some()
                    && obj.get(k).is_some_and(|v| !fits(v, s, root, depth + 1))
                {
                    return false;
                }
            }
        }
    }
    true
}
pub fn project(value: &Value, schema: &Value, root: &Value, ceiling: Sensitivity) -> Value {
    project_at(value, schema, root, ceiling, 0)
}
fn project_at(
    value: &Value,
    schema: &Value,
    root: &Value,
    ceiling: Sensitivity,
    depth: usize,
) -> Value {
    if depth > 64 {
        return Value::Null;
    }
    let schema = resolve(schema, root);
    if let Some(alts) = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)
    {
        let mut projections = alts
            .iter()
            .filter(|s| fits(value, s, root, depth + 1))
            .map(|s| project_at(value, s, root, ceiling, depth + 1));
        let Some(mut common) = projections.next() else {
            return Value::Null;
        };
        // Ambiguous variants only disclose fields every matching variant permits.
        for next in projections {
            if let (Value::Object(a), Value::Object(b)) = (&mut common, &next) {
                a.retain(|k, v| b.get(k) == Some(v));
            } else if common != next {
                common = Value::Null;
            }
        }
        return common;
    }
    match value {
        Value::Object(input) => {
            let mut out = Map::new();
            if let Some(props) = schema["properties"].as_object() {
                for (name, field) in props {
                    let Some(class) = Sensitivity::parse(&field["x-sensitivity"]) else {
                        continue;
                    };
                    if class > ceiling {
                        continue;
                    }
                    if let Some(v) = input.get(name) {
                        out.insert(name.clone(), project_at(v, field, root, ceiling, depth + 1));
                    }
                }
            } else if let Some(items) = schema.get("additionalProperties").filter(|s| s.is_object())
            {
                for (name, v) in input {
                    out.insert(name.clone(), project_at(v, items, root, ceiling, depth + 1));
                }
            }
            Value::Object(out)
        }
        Value::Array(input) => Value::Array(
            schema
                .get("items")
                .map(|s| {
                    input
                        .iter()
                        .map(|v| project_at(v, s, root, ceiling, depth + 1))
                        .collect()
                })
                .unwrap_or_default(),
        ),
        _ => {
            if fits(value, schema, root, depth + 1) {
                value.clone()
            } else {
                Value::Null
            }
        }
    }
}
pub fn result(method: &str, value: &Value, ceiling: Sensitivity) -> Value {
    let root = schema();
    root["methods"][method]
        .get("result")
        .map(|s| project(value, s, root, ceiling))
        .unwrap_or(Value::Null)
}
pub fn event(topic: &str, value: &Value, ceiling: Sensitivity) -> Value {
    let root = schema();
    root["topics"][topic]
        .get("data")
        .map(|s| project(value, s, root, ceiling))
        .unwrap_or(Value::Null)
}
/// Apply to each output frame. Event/result payloads use their own typed schema,
/// rather than the generic Value in the envelope. Errors use ErrorBody's schema.
pub fn frame(method: Option<&str>, value: &Value, ceiling: Sensitivity) -> Value {
    let root = schema();
    let part = match value["type"].as_str() {
        Some("event") => "event",
        Some("progress") => "progress",
        _ => "response",
    };
    let mut out = project(value, &root["frames"][part], root, ceiling);
    if let Some(output) = out.as_object_mut() {
        if part == "event" {
            output.insert(
                "data".into(),
                event(
                    value["topic"].as_str().unwrap_or(""),
                    &value["data"],
                    ceiling,
                ),
            );
        } else if value.get("result").is_some() {
            output.insert(
                "result".into(),
                result(method.unwrap_or(""), &value["result"], ceiling),
            );
        }
    }
    out
}
pub fn legacy_sessions(data: &mut Value, ceiling: Sensitivity) {
    let root = schema();
    let session = &root["$defs"]["SessionView"];
    if let Some(sessions) = data.as_array_mut() {
        for value in sessions {
            let mut out = project(value, session, root, ceiling);
            // Required conversation strings are blank on /ws for the old SPA.
            if let (Some(input), Some(output)) = (value.as_object(), out.as_object_mut()) {
                for name in session["required"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    let field = &session["properties"][name];
                    if input.get(name).is_some_and(Value::is_string)
                        && !output.contains_key(name)
                        && field["type"] == "string"
                        && Sensitivity::parse(&field["x-sensitivity"])
                            == Some(Sensitivity::Conversation)
                    {
                        output.insert(name.into(), "".into());
                    }
                }
            }
            *value = out;
        }
    }
}
pub fn legacy_notification(data: &mut Value, ceiling: Sensitivity) {
    let root = schema();
    let had_session_id = data.get("sessionId").is_some();
    let mut input = data.clone();
    if had_session_id {
        input["id"] = input["sessionId"].clone();
    }
    let mut out = project(&input, &root["$defs"]["NotificationEvent"], root, ceiling);
    if let Some(obj) = out.as_object_mut() {
        if ceiling < Sensitivity::Conversation {
            // Produced exclusively from projectPath, never from a session title.
            let title = obj
                .get("redactedTitle")
                .and_then(Value::as_str)
                .unwrap_or("c9watch")
                .to_string();
            obj.insert("title".into(), title.into());
        }
        obj.remove("redactedTitle");
        if had_session_id {
            if let Some(id) = obj.remove("id") {
                obj.insert("sessionId".into(), id);
            }
        }
    }
    *data = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn unknown_fields_and_nested_opaque_fields_do_not_leave_the_gateway() {
        let fixture = json!({"properties":{"known":{"type":"string","x-sensitivity":"metadata"},"opaque":{"x-sensitivity":"conversation"}}});
        let input =
            json!({"known":"path","unclassified":"secret","opaque":{"unclassified":"secret"}});
        let out = project(&input, &fixture, &json!({}), Sensitivity::Conversation);
        assert_eq!(out, json!({"known":"path","opaque":{}}));
        assert_eq!(
            project(&input, &fixture, &json!({}), Sensitivity::Public),
            json!({})
        );
    }
    #[test]
    fn results_events_and_errors_all_use_classified_projection() {
        let data = json!({"id":"s1","projectPath":"/tmp/project","firstPrompt":"secret","latestMessage":"secret","customTitle":"secret","futureField":"secret"});
        let result = result(
            "session.views",
            &json!({"cursor":"c","sessions":[data.clone()]}),
            Sensitivity::Metadata,
        );
        assert!(!result.to_string().contains("secret"));
        let output = frame(
            None,
            &json!({"v":1,"type":"event","topic":"session.view","data":data,"future":"secret"}),
            Sensitivity::Metadata,
        );
        assert!(!output.to_string().contains("secret"), "{output}");
        assert_eq!(output["data"]["projectPath"], "/tmp/project");
        let error = frame(
            None,
            &json!({"v":1,"ok":false,"error":{"code":"not_allowed","msg":"denied","retryable":false,"data":{"unclassified":"secret"},"future":"secret"}}),
            Sensitivity::Metadata,
        );
        assert!(!error.to_string().contains("secret"), "{error}");
    }
}
