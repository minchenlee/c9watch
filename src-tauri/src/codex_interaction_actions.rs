//! Typed, fail-closed approval/form handling. Clients choose actions, never wire payloads.
use serde_json::{json, Value};

pub fn details(kind: &str, p: &Value, item: Option<&Value>) -> Value {
    let mut out = json!({});
    for key in [
        "command",
        "cwd",
        "reason",
        "grantRoot",
        "additionalPermissions",
        "networkApprovalContext",
        "permissions",
        "serverName",
        "mode",
        "message",
        "url",
        "requestedSchema",
        "kind",
        "environmentId",
    ] {
        if let Some(value) = p.get(key).filter(|v| !v.is_null()) {
            out[key] = value.clone();
        }
    }
    if let Some(item) = item {
        for key in ["command", "cwd"] {
            if out[key].is_null() && !item[key].is_null() {
                out[key] = item[key].clone();
            }
        }
        if kind == "file" {
            out["changes"] = item["changes"].clone();
        }
    }
    if kind == "permission" {
        out["choices"] = json!(permission_choices(&p["permissions"]));
    }
    if kind == "form" {
        out["supportedForm"] = json!(supported_form(&p["requestedSchema"]));
        out["safeUrl"] = json!(safe_url(p["url"].as_str().unwrap_or_default()));
    }
    out
}
pub fn available(kind: &str, p: &Value, d: &Value) -> Vec<String> {
    let mut actions = match kind {
        "command" | "file" => vec!["accept", "decline", "cancel"],
        "permission" => vec!["grant", "deny"],
        "form" => vec!["accept", "decline", "cancel"],
        _ => vec![],
    };
    if kind == "command" {
        if let Some(allowed) = p["availableDecisions"].as_array() {
            actions.retain(|action| allowed.iter().any(|v| v.as_str() == Some(action)));
        }
        if d["command"].as_str().is_none_or(str::is_empty) && d["networkApprovalContext"].is_null()
        {
            actions.retain(|a| *a != "accept");
        }
        // Unknown future command kinds must be inspected in the owner client.
        if p["kind"]
            .as_str()
            .is_some_and(|k| !["command", "stdin"].contains(&k))
        {
            actions.retain(|a| *a != "accept");
        }
    }
    if kind == "file" {
        let complete = d["changes"].as_array().is_some_and(|changes| {
            !changes.is_empty()
                && changes.iter().all(|c| {
                    c["path"].as_str().is_some_and(|p| !p.is_empty())
                        && c["diff"].as_str().is_some_and(|s| !s.is_empty())
                        && c["kind"]["type"]
                            .as_str()
                            .is_some_and(|k| ["add", "update", "delete"].contains(&k))
                })
        });
        // grantRoot requests session-wide access rather than a one-time approval.
        if !complete || !p["grantRoot"].is_null() {
            actions.retain(|a| *a != "accept");
        }
    }
    if kind == "permission"
        && (!supported_permissions(&p["permissions"])
            || permission_choices(&p["permissions"]).is_empty())
    {
        actions.retain(|a| *a != "grant");
    }
    if kind == "form" {
        let supported = match p["mode"].as_str() {
            Some("form" | "openai/form" | "openaiForm") => supported_form(&p["requestedSchema"]),
            Some("url") => safe_url(p["url"].as_str().unwrap_or_default()),
            _ => false,
        };
        if !supported {
            actions.retain(|a| *a != "accept");
        }
    }
    actions.into_iter().map(str::to_owned).collect()
}
fn supported_permissions(p: &Value) -> bool {
    if p.as_object().is_none_or(|o| {
        o.keys()
            .any(|k| !["fileSystem", "network"].contains(&k.as_str()))
    }) {
        return false;
    }
    if !p["network"].is_null()
        && p["network"]
            .as_object()
            .is_none_or(|o| o.keys().any(|k| k != "enabled"))
    {
        return false;
    }
    let fs = &p["fileSystem"];
    if !fs.is_null()
        && fs.as_object().is_none_or(|o| {
            o.keys()
                .any(|k| !["entries", "read", "write", "globScanMaxDepth"].contains(&k.as_str()))
        })
    {
        return false;
    }
    for key in ["read", "write"] {
        if !fs[key].is_null()
            && fs[key]
                .as_array()
                .is_none_or(|xs| xs.iter().any(|v| !v.is_string()))
        {
            return false;
        }
    }
    if !fs["entries"].is_null()
        && fs["entries"].as_array().is_none_or(|xs| {
            xs.iter().any(|v| {
                !matches!(v["access"].as_str(), Some("read" | "write" | "deny"))
                    || v["path"].is_null()
            })
        })
    {
        return false;
    }
    true
}
pub fn permission_choices(p: &Value) -> Vec<Value> {
    let mut choices = Vec::new();
    if p["network"]["enabled"] == true {
        choices.push(json!({"id":"network","label":"Network access"}));
    }
    for category in ["read", "write", "entries"] {
        if let Some(items) = p["fileSystem"][category].as_array() {
            for (i, value) in items.iter().enumerate() {
                if category == "entries"
                    && !matches!(value["access"].as_str(), Some("read" | "write"))
                {
                    continue;
                }
                let label = if category == "entries" {
                    format!(
                        "{}: {}",
                        value["access"].as_str().unwrap_or_default(),
                        value["path"]
                    )
                } else {
                    format!("{category}: {}", value.as_str().unwrap_or("Unknown path"))
                };
                choices.push(json!({"id":format!("{category}:{i}"),"label":label}));
            }
        }
    }
    choices
}
fn granted_permissions(p: &Value, selected: &Value) -> Result<Value, String> {
    let selected = selected
        .as_array()
        .ok_or("Select the permissions to grant")?;
    let choices = permission_choices(p);
    if selected.is_empty() || selected.len() > choices.len() {
        return Err("Select at least one requested permission".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut granted = json!({});
    for id in selected {
        let id = id.as_str().ok_or("Invalid permission selection")?;
        if !seen.insert(id) || !choices.iter().any(|c| c["id"] == id) {
            return Err("Permission was not requested".into());
        }
        if id == "network" {
            granted["network"] = json!({"enabled":true});
            continue;
        }
        let (category, index) = id.split_once(':').ok_or("Invalid permission")?;
        let index: usize = index.parse().map_err(|_| "Invalid permission index")?;
        if granted["fileSystem"].is_null() {
            granted["fileSystem"] = json!({});
        }
        if granted["fileSystem"][category].is_null() {
            granted["fileSystem"][category] = json!([]);
        }
        granted["fileSystem"][category]
            .as_array_mut()
            .unwrap()
            .push(p["fileSystem"][category][index].clone());
    }
    if !granted["fileSystem"].is_null() {
        // Keep deny constraints and glob scan limits; omitting them can broaden a grant.
        let denies: Vec<_> = p["fileSystem"]["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| e["access"] == "deny")
            .cloned()
            .collect();
        if !denies.is_empty() {
            if granted["fileSystem"]["entries"].is_null() {
                granted["fileSystem"]["entries"] = json!([]);
            }
            granted["fileSystem"]["entries"]
                .as_array_mut()
                .unwrap()
                .extend(denies);
        }
        if !p["fileSystem"]["globScanMaxDepth"].is_null() {
            granted["fileSystem"]["globScanMaxDepth"] = p["fileSystem"]["globScanMaxDepth"].clone();
        }
    }
    Ok(granted)
}
pub fn response(
    kind: &str,
    p: &Value,
    d: &Value,
    action: &str,
    input: &Value,
) -> Result<Value, String> {
    if !available(kind, p, d).iter().any(|a| a == action) {
        return Err("This action is not available for the current request. Check Codex.".into());
    }
    if kind == "file" && action == "accept" && input["reviewed"] != true {
        return Err("Review the full file changes before approving".into());
    }
    match kind {
        "command" | "file" => Ok(json!({"decision":action})),
        "permission" => Ok(
            json!({"permissions":if action == "deny" {json!({})} else {granted_permissions(&p["permissions"],&input["selected"])?},"scope":"turn"}),
        ),
        "form" => {
            let content = if action != "accept" {
                Value::Null
            } else if p["mode"] == "url" {
                if input["completed"] != true {
                    return Err("Complete the external step before confirming it".into());
                }
                Value::Null
            } else {
                validate_form(&p["requestedSchema"], &input["content"])?;
                input["content"].clone()
            };
            Ok(json!({"action":action,"content":content}))
        }
        _ => Err("Unsupported request".into()),
    }
}
pub fn safe_url(url: &str) -> bool {
    tauri::Url::parse(url).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
    })
}
fn enum_values(field: &Value) -> Option<Vec<Value>> {
    if let Some(values) = field["enum"].as_array() {
        return Some(values.clone());
    }
    field["oneOf"]
        .as_array()
        .or_else(|| field["anyOf"].as_array())
        .map(|options| options.iter().map(|o| o["const"].clone()).collect())
}
pub fn supported_form(schema: &Value) -> bool {
    let Some(properties) = schema["properties"].as_object() else {
        return false;
    };
    if schema["type"] != "object" || properties.len() > 32 {
        return false;
    }
    let root_keys = [
        "type",
        "properties",
        "required",
        "$schema",
        "title",
        "description",
        "additionalProperties",
    ];
    if schema
        .as_object()
        .is_none_or(|s| s.keys().any(|k| !root_keys.contains(&k.as_str())))
    {
        return false;
    }
    if !schema["required"].is_null()
        && schema["required"].as_array().is_none_or(|xs| {
            xs.iter()
                .any(|k| k.as_str().is_none_or(|k| !properties.contains_key(k)))
        })
    {
        return false;
    }
    properties.values().all(|field| {
        let Some(object) = field.as_object() else {
            return false;
        };
        let keys = [
            "type",
            "title",
            "description",
            "default",
            "enum",
            "enumNames",
            "oneOf",
            "items",
            "minItems",
            "maxItems",
            "minLength",
            "maxLength",
            "minimum",
            "maximum",
            "format",
        ];
        if object.keys().any(|k| !keys.contains(&k.as_str())) {
            return false;
        }
        let kind = field["type"].as_str().unwrap_or_default();
        if !matches!(kind, "string" | "number" | "integer" | "boolean" | "array") {
            return false;
        }
        if field["format"]
            .as_str()
            .is_some_and(|f| !["email", "uri", "date", "date-time"].contains(&f))
        {
            return false;
        }
        if kind == "array" {
            let item = &field["items"];
            if item.as_object().is_none_or(|o| {
                o.keys()
                    .any(|k| !["type", "enum", "anyOf", "oneOf"].contains(&k.as_str()))
            }) {
                return false;
            }
            if (!item["type"].is_null() && item["type"] != "string")
                || enum_values(item)
                    .is_none_or(|xs| xs.is_empty() || !xs.iter().all(Value::is_string))
            {
                return false;
            }
        }
        for container in [field, &field["items"]] {
            for key in ["oneOf", "anyOf"] {
                if let Some(options) = container[key].as_array() {
                    if options.iter().any(|o| {
                        o.as_object().is_none_or(|o| {
                            o.keys().any(|k| !["const", "title"].contains(&k.as_str()))
                        })
                    }) {
                        return false;
                    }
                }
            }
        }
        if let Some(values) = enum_values(field) {
            if values.is_empty() || !values.iter().all(Value::is_string) || kind != "string" {
                return false;
            }
        }
        true
    })
}
pub fn validate_form(schema: &Value, content: &Value) -> Result<(), String> {
    if !supported_form(schema) {
        return Err("This form requires the original Codex client".into());
    }
    let data = content.as_object().ok_or("Invalid form values")?;
    let properties = schema["properties"].as_object().unwrap();
    if content.to_string().len() > 32 * 1024 || data.keys().any(|k| !properties.contains_key(k)) {
        return Err("Unexpected or oversized form values".into());
    }
    for key in schema["required"].as_array().into_iter().flatten() {
        if !data.contains_key(key.as_str().unwrap_or_default()) {
            return Err(format!(
                "Required field: {}",
                key.as_str().unwrap_or_default()
            ));
        }
    }
    for (key, value) in data {
        let f = &properties[key];
        let invalid = || format!("Invalid value for {key}");
        match f["type"].as_str().unwrap_or_default() {
            "string" => {
                let s = value.as_str().ok_or_else(invalid)?;
                let len = s.chars().count() as u64;
                if f["minLength"].as_u64().is_some_and(|m| len < m)
                    || f["maxLength"].as_u64().is_some_and(|m| len > m)
                {
                    return Err(invalid());
                }
                if let Some(values) = enum_values(f) {
                    if !values.contains(value) {
                        return Err(invalid());
                    }
                }
                let valid = match f["format"].as_str() {
                    Some("uri") => tauri::Url::parse(s).is_ok(),
                    Some("date") => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok(),
                    Some("date-time") => chrono::DateTime::parse_from_rfc3339(s).is_ok(),
                    Some("email") => s.split_once('@').is_some_and(|(l, r)| {
                        !l.is_empty()
                            && !r.is_empty()
                            && !r.contains('@')
                            && !s.chars().any(char::is_whitespace)
                    }),
                    _ => true,
                };
                if !valid {
                    return Err(invalid());
                }
            }
            "number" | "integer" => {
                let n = value.as_f64().ok_or_else(invalid)?;
                if !n.is_finite()
                    || (f["type"] == "integer" && n.fract() != 0.0)
                    || f["minimum"].as_f64().is_some_and(|m| n < m)
                    || f["maximum"].as_f64().is_some_and(|m| n > m)
                {
                    return Err(invalid());
                }
            }
            "boolean" => {
                if !value.is_boolean() {
                    return Err(invalid());
                }
            }
            "array" => {
                let a = value.as_array().ok_or_else(invalid)?;
                let allowed = enum_values(&f["items"]).unwrap();
                if f["minItems"].as_u64().is_some_and(|m| (a.len() as u64) < m)
                    || f["maxItems"].as_u64().is_some_and(|m| (a.len() as u64) > m)
                    || a.iter()
                        .enumerate()
                        .any(|(i, v)| !allowed.contains(v) || a[..i].contains(v))
                {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_decisions_respect_server_choices_and_never_create_persistent_rules() {
        let p =
            json!({"command":"echo test","cwd":"/tmp","availableDecisions":["decline","cancel"]});
        let d = details("command", &p, None);
        assert_eq!(available("command", &p, &d), vec!["decline", "cancel"]);
        assert!(response("command", &p, &d, "accept", &json!({})).is_err());
        assert_eq!(
            response("command", &p, &d, "decline", &json!({})).unwrap(),
            json!({"decision":"decline"})
        );
        let p = json!({"command":"echo test","cwd":"/tmp"});
        let d = details("command", &p, None);
        assert_eq!(
            response("command", &p, &d, "accept", &json!({})).unwrap(),
            json!({"decision":"accept"})
        );
        assert!(response("command", &p, &d, "acceptForSession", &json!({})).is_err());
    }
    #[test]
    fn file_approval_requires_complete_review_and_does_not_grant_a_session_root() {
        let p = json!({"itemId":"file"});
        let item =
            json!({"changes":[{"path":"/tmp/a","kind":{"type":"update"},"diff":"-old\n+new"}]});
        let d = details("file", &p, Some(&item));
        assert!(response("file", &p, &d, "accept", &json!({})).is_err());
        assert_eq!(
            response("file", &p, &d, "accept", &json!({"reviewed":true})).unwrap(),
            json!({"decision":"accept"})
        );
        assert!(!available("file", &p, &details("file", &p, None)).contains(&"accept".into()));
        let p = json!({"itemId":"file","grantRoot":"/"});
        assert!(!available("file", &p, &d).contains(&"accept".into()));
    }
    #[test]
    fn grants_only_selected_requested_permissions_and_preserves_denies() {
        let p = json!({"permissions":{"network":{"enabled":true},"fileSystem":{"read":["/tmp/read"],"write":["/tmp/write"],"entries":[{"access":"deny","path":{"type":"path","path":"/tmp/secret"}}],"globScanMaxDepth":2}}});
        let d = details("permission", &p, None);
        let r=response("permission",&p,&d,"grant",&json!({"selected":["network","read:0"],"scope":"session","permissions":{"fileSystem":{"write":["/"]}}})).unwrap();
        assert_eq!(r["scope"], "turn");
        assert_eq!(r["permissions"]["network"]["enabled"], true);
        assert!(r["permissions"]["fileSystem"]["write"].is_null());
        assert_eq!(
            r["permissions"]["fileSystem"]["entries"][0]["access"],
            "deny"
        );
        assert_eq!(r["permissions"]["fileSystem"]["globScanMaxDepth"], 2);
        assert!(response(
            "permission",
            &p,
            &d,
            "grant",
            &json!({"selected":["write:1"]})
        )
        .is_err());
        assert_eq!(
            response("permission", &p, &d, "deny", &json!({})).unwrap(),
            json!({"permissions":{},"scope":"turn"})
        );
        let p = json!({"permissions":{"fileSystem":{"read":["/tmp"],"futureRestriction":true}}});
        assert!(
            !available("permission", &p, &details("permission", &p, None))
                .contains(&"grant".into())
        );
    }
    #[test]
    fn mcp_forms_validate_types_constraints_and_titled_multi_select() {
        let schema = json!({"type":"object","required":["name","count","ok","tags"],"properties":{
            "name":{"type":"string","minLength":2,"maxLength":6},"count":{"type":"integer","minimum":1,"maximum":3},
            "ok":{"type":"boolean"},"tags":{"type":"array","minItems":1,"maxItems":2,"items":{"anyOf":[{"const":"a","title":"Alpha"},{"const":"b","title":"Beta"}]}},
            "mode":{"type":"string","oneOf":[{"const":"x","title":"First"}]}
        }});
        assert!(supported_form(&schema));
        let good = json!({"name":"中文","count":2,"ok":false,"tags":["b"]});
        validate_form(&schema, &good).unwrap();
        for (key, value) in [
            ("count", json!(2.5)),
            ("count", json!(4)),
            ("tags", json!(["b", "b"])),
            ("ok", json!("false")),
            ("mode", json!("not-listed")),
        ] {
            let mut bad = good.clone();
            bad[key] = value;
            assert!(validate_form(&schema, &bad).is_err());
        }
        let mut bad = good.clone();
        bad["extra"] = json!(true);
        assert!(validate_form(&schema, &bad).is_err());
        let mut unsupported = schema.clone();
        unsupported["properties"]["name"]["pattern"] = json!(".*");
        assert!(!supported_form(&unsupported));
    }
    #[test]
    fn mcp_url_is_explicit_and_never_opens_executable_schemes() {
        let p = json!({"mode":"url","url":"https://example.com/confirm","message":"Confirm external step"});
        let d = details("form", &p, None);
        assert!(response("form", &p, &d, "accept", &json!({})).is_err());
        assert_eq!(
            response("form", &p, &d, "accept", &json!({"completed":true})).unwrap(),
            json!({"action":"accept","content":null})
        );
        for url in [
            "javascript:alert(1)",
            "file:///tmp/a",
            "https://user:password@example.com",
        ] {
            assert!(!safe_url(url));
        }
        let p = json!({"mode":"url","url":"file:///tmp/a"});
        assert!(!available("form", &p, &details("form", &p, None)).contains(&"accept".into()));
        assert_eq!(
            response("form", &p, &json!({}), "cancel", &json!({})).unwrap(),
            json!({"action":"cancel","content":null})
        );
    }

    #[test]
    fn zero_field_mcp_approval_preserves_an_empty_object() {
        let p = json!({"mode":"form","requestedSchema":{"type":"object","properties":{}}});
        let d = details("form", &p, None);
        assert!(supported_form(&p["requestedSchema"]));
        assert_eq!(response("form", &p, &d, "accept", &json!({"content":{}})).unwrap(), json!({"action":"accept","content":{}}));
        assert!(response("form", &p, &d, "accept", &json!({"content":{"unexpected":true}})).is_err());
    }
}
