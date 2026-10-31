//! Native consent text and fail-closed decision logic. The webview supplies
//! settings values, never dialog text or a decision/grant.
use serde_json::Value;

fn label(field: &str) -> &str {
    match field {
        "lanEnabled" => "LAN access",
        "allowConversation" => "Read conversation content",
        "allowHistory" => "Read history",
        "allowMemory" => "Read memory",
        "allowOpen" => "Open sessions on this computer",
        "enabled" => "Core gateway",
        "scopes" => "Permissions",
        "ceiling" => "Sensitivity ceiling",
        "expiresAt" => "Token expiry",
        "allowedOrigins" => "Allowed origins",
        "extraAllowedHosts" => "Allowed hosts",
        _ => field,
    }
}
/// Only additions and extensions appear. Text is built by Rust from the
/// same immutable values that will be applied after consent.
pub fn additions(old: &Value, new: &Value) -> Vec<String> {
    let Some(fields) = new.as_object() else {
        return vec!["New remote access grant".into()];
    };
    let mut out = Vec::new();
    for (field, value) in fields {
        let before = &old[field];
        if field == "gateway" {
            out.extend(additions(before, value));
            continue;
        }
        if !super::policy::is_widening(
            &serde_json::json!({(field):before}),
            &serde_json::json!({(field):value}),
        ) {
            continue;
        }
        if let Some(values) = value.as_array() {
            for item in values {
                if !before.as_array().is_some_and(|a| a.contains(item)) {
                    out.push(format!("New {}: {}", label(field), item));
                }
            }
        } else if value == &Value::Bool(true) {
            out.push(format!("New: {}", label(field)));
        } else {
            out.push(format!("{}: {}", label(field), value));
        }
    }
    if out.is_empty() && super::policy::is_widening(old, new) {
        out.push("Review the changed remote access grant".into());
    }
    out
}
pub async fn confirm<F, Fut>(old: &Value, new: &Value, ask: F) -> Result<(), String>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<bool, String>>,
{
    if !super::policy::is_widening(old, new) {
        return Ok(());
    }
    let message = additions(old, new).join("\n");
    if ask(message).await? {
        Ok(())
    } else {
        Err("confirmation_required: remote access change was not approved".into())
    }
}
#[cfg(feature = "gui")]
pub async fn native_dialog(app: tauri::AppHandle, message: String) -> Result<bool, String> {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(message)
        .title("Allow remote access change?")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Allow".into(),
            "Cancel".into(),
        ))
        .show(move |allowed| {
            let _ = tx.send(allowed);
        });
    rx.await
        .map_err(|_| "confirmation_required: native dialog unavailable".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn narrowing_never_prompts_and_widening_must_be_approved() {
        confirm(
            &json!({"lanEnabled":true}),
            &json!({"lanEnabled":false}),
            |_| async { panic!("narrowing prompted") },
        )
        .await
        .unwrap();
        let old = json!({"lanEnabled":false,"allowConversation":true});
        let new = json!({"lanEnabled":true,"allowConversation":true});
        assert_eq!(additions(&old, &new), ["New: LAN access"]);
        assert!(confirm(&old, &new, |_| async { Ok(false) }).await.is_err());
        assert!(
            confirm(&old, &new, |_| async { Err("dialog closed".into()) })
                .await
                .is_err()
        );
        confirm(&old, &new, |_| async { Ok(true) }).await.unwrap();
    }
    #[test]
    fn incremental_consent_shows_only_new_scopes_and_ceiling() {
        let old = json!({"scopes":["session:read"],"ceiling":"metadata"});
        let new = json!({"scopes":["session:read","conversation:read"],"ceiling":"conversation"});
        let text = additions(&old, &new).join("\n");
        assert!(text.contains("conversation:read"));
        assert!(!text.contains("session:read"));
        assert!(text.contains("conversation"));
    }
}
