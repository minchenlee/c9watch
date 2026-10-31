//! Native gateway control. Neither signing keys nor grants reach the webview.
//! Wire encoding is documented in docs/api/gui-gateway-bridge.md for M-b integration.
use crate::{
    bridge::Bridge,
    engine_proto::{confirmation, spawn::EngineConn},
};
use ed25519_dalek::SigningKey;
use rand::RngCore;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

pub struct GatewayLease {
    pub conn: EngineConn,
    key: SigningKey,
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
        previous: &Value,
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
        if !crate::gateway::policy::is_widening(previous, &params) {
            return Err("confirmation_required: unclassified widening change".into());
        }
        tauri::async_runtime::block_on(crate::gateway::consent::confirm(
            previous,
            &params,
            |message| crate::gateway::consent::native_dialog(app.clone(), message),
        ))?;
        let grant = confirmation::sign(
            &self.key,
            method,
            &params,
            chrono::Utc::now().timestamp_millis(),
        );
        let mut signed = params;
        signed
            .as_object_mut()
            .ok_or("Gateway params must be an object")?
            .insert(
                "confirmation".into(),
                serde_json::to_value(grant).map_err(|e| e.to_string())?,
            );
        call(&mut self.conn, method, signed)
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
    fn gui_grants_use_the_core_verifier_and_wire_shape() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let params = json!({"z":{"b":2,"a":1},"a":[{"b":2,"a":1}]});
        let grant = confirmation::sign(&key, "gateway.policy.set", &params, 1000);
        let wire = serde_json::to_value(&grant).unwrap();
        assert!(wire["signature"].is_array());
        assert!(wire.get("claims").is_none());
        let decoded: confirmation::Grant = serde_json::from_value(wire).unwrap();
        let mut verifier = confirmation::Verifier::default();
        verifier.attach(key.verifying_key()).unwrap();
        verifier
            .consume(&decoded, "gateway.policy.set", &params, 1001)
            .unwrap();
        assert!(verifier
            .consume(&decoded, "gateway.policy.set", &params, 1002)
            .is_err());
    }
}
