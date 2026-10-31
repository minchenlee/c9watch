//! Native gateway control. Neither signing keys nor grants reach the webview.
//! Wire encoding is documented in docs/api/gui-gateway-bridge.md for M-b integration.
use crate::{bridge::Bridge, engine_proto::spawn::EngineConn, remote_access::RemoteAccess};
use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

pub struct GatewayLease {
    pub conn: EngineConn,
    key: SigningKey,
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn canonical(value: &Value) -> Vec<u8> {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_by_key(|(key, _)| *key);
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key.clone(), sorted(value)))
                        .collect(),
                )
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            _ => value.clone(),
        }
    }
    serde_json::to_vec(&sorted(value)).expect("JSON Value serializes")
}
fn call(conn: &mut EngineConn, method: &str, params: Value) -> Result<Value, String> {
    let mut response = conn.call_raw_progress(method, params, None, &mut |_| {})?;
    if response["ok"] == true {
        Ok(response["result"].take())
    } else {
        Err(format!(
            "{}: {}",
            response["error"]["code"].as_str().unwrap_or("unknown"),
            response["error"]["msg"]
                .as_str()
                .unwrap_or("Gateway refused the call")
        ))
    }
}
impl GatewayLease {
    pub fn attach(mut conn: EngineConn) -> Result<Self, String> {
        let mut secret = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut secret);
        let key = SigningKey::from_bytes(&secret);
        call(
            &mut conn,
            "gui.confirmer.attach",
            json!({"publicKey": hex(key.verifying_key().as_bytes())}),
        )?;
        Ok(Self { conn, key })
    }
    pub fn read(&mut self, method: &str, params: Value) -> Result<Value, String> {
        call(&mut self.conn, method, params)
    }
    /// The first refusal is authoritative: only confirmation_required can
    /// ask Rust to sign. No errors become implicit permission.
    pub fn write(
        &mut self,
        app: &AppHandle,
        method: &str,
        params: Value,
        difference: &str,
    ) -> Result<Value, String> {
        let mut response =
            self.conn
                .call_raw_progress(method, params.clone(), None, &mut |_| {})?;
        if response["ok"] == true {
            return Ok(response["result"].take());
        }
        if response["error"]["code"] != "confirmation_required" {
            return Err(format!(
                "{}: {}",
                response["error"]["code"], response["error"]["msg"]
            ));
        }
        if difference.is_empty() {
            return Err("confirmation_required: unclassified widening change".into());
        }
        confirm(app, difference)?;
        let claims = json!({
            "grantId": uuid::Uuid::new_v4().to_string(),
            "method": method,
            "paramsHash": hex(&Sha256::digest(canonical(&params))),
            "expiresAt": chrono::Utc::now().timestamp_millis() + 60_000,
        });
        let signature = hex(&self.key.sign(&canonical(&claims)).to_bytes());
        let mut signed = params;
        signed
            .as_object_mut()
            .ok_or("Gateway params must be an object")?
            .insert(
                "confirmation".into(),
                json!({"claims": claims,"signature": signature}),
            );
        call(&mut self.conn, method, signed)
    }
}
fn confirm(app: &AppHandle, difference: &str) -> Result<(), String> {
    if app
        .dialog()
        .message(difference)
        .title("c9watch Remote Access")
        .buttons(MessageDialogButtons::OkCancel)
        .blocking_show()
    {
        Ok(())
    } else {
        Err("Remote access change cancelled".into())
    }
}
/// Text is built only from Rust's old/new values. Never display a webview's
/// purported reason or use it to decide whether a grant is needed.
pub fn policy_difference(old: &RemoteAccess, new: &RemoteAccess) -> String {
    let mut lines = Vec::new();
    if !old.lan_enabled && new.lan_enabled {
        lines.push("New: allow devices on the local network".to_string());
    }
    if !old.allow_conversation && new.allow_conversation {
        lines.push("New: read conversation content (conversation:read)".to_string());
    }
    if !old.allow_open && new.allow_open {
        lines.push("New: open session windows".to_string());
    }
    for host in &new.extra_allowed_hosts {
        if !old.extra_allowed_hosts.contains(host) {
            lines.push(format!("New allowed host: {host}"));
        }
    }
    lines.join("\n")
}
pub fn confirm_pairing(app: &AppHandle, policy: &RemoteAccess) -> Result<(), String> {
    // The no-gateway fallback retains the existing legacy /ws permissions.
    // It must not present itself as the core gateway's scoped viewer preset.
    let mut text = "New device: read session metadata and usage, stop and rename observed sessions"
        .to_string();
    if policy.allow_conversation {
        text.push_str("\nNew: read conversation content");
    }
    if policy.allow_open {
        text.push_str("\nNew: open session windows");
    }
    confirm(app, &text)
}
pub fn confirm_policy(
    app: &AppHandle,
    old: &RemoteAccess,
    new: &RemoteAccess,
) -> Result<(), String> {
    let diff = policy_difference(old, new);
    if diff.is_empty() {
        Ok(())
    } else {
        confirm(app, &diff)
    }
}
pub fn core_bridge(app: &AppHandle) -> Option<std::sync::Arc<Bridge>> {
    app.try_state::<std::sync::Arc<Bridge>>()
        .filter(|b| b.handed_over())
        .map(|b| b.inner().clone())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diff_has_only_new_permissions_and_hosts() {
        let old = RemoteAccess::default();
        let new = RemoteAccess {
            lan_enabled: true,
            allow_conversation: true,
            allow_open: true,
            extra_allowed_hosts: vec!["host:9210".into()],
        };
        let diff = policy_difference(&old, &new);
        assert!(diff.contains("conversation:read"));
        assert!(diff.contains("host:9210"));
        assert!(policy_difference(&new, &old).is_empty());
        assert!(policy_difference(&new, &new).is_empty());
    }
    #[test]
    fn grant_signature_binds_method_params_id_and_expiry() {
        use ed25519_dalek::{Signature, Verifier};
        let key = SigningKey::from_bytes(&[7; 32]);
        let claims = json!({"grantId":"one","method":"gateway.policy.set","paramsHash":hex(&Sha256::digest(b"{}")),"expiresAt":60_000});
        let params = json!({"z":{"b":2,"a":1},"a":[{"b":2,"a":1}]});
        assert_eq!(
            String::from_utf8(canonical(&params)).unwrap(),
            r#"{"a":[{"a":1,"b":2}],"z":{"a":1,"b":2}}"#
        );
        let bytes = canonical(&claims);
        let signature: Signature = key.sign(&bytes);
        key.verifying_key().verify(&bytes, &signature).unwrap();
        let mut changed = claims;
        changed["grantId"] = "two".into();
        assert!(key
            .verifying_key()
            .verify(&canonical(&changed), &signature)
            .is_err());
    }
}
