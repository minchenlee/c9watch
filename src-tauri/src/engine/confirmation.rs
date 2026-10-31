//! Native confirmation verification. No key is trusted until the GUI's
//! authenticated attachment path supplies it. Grants authorize exact params once.
use super::mail::sha256_hex;
use super::proto::{ApiError, ErrorCode};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

pub const GRANT_TTL_MS: i64 = 60_000;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Grant {
    pub grant_id: String,
    pub method: String,
    pub params_hash: String,
    pub expires_at: i64,
    pub signature: Vec<u8>,
}
fn message(id: &str, method: &str, hash: &str, expires: i64) -> Vec<u8> {
    serde_json::to_vec(
        &serde_json::json!({"grantId":id,"method":method,"paramsHash":hash,"expiresAt":expires}),
    )
    .expect("grant serializes")
}
pub fn sign(key: &SigningKey, method: &str, params: &Value, now: i64) -> Grant {
    let id = uuid::Uuid::new_v4().to_string();
    let hash = sha256_hex(&serde_json::to_vec(params).expect("params serialize"));
    let expires = now.saturating_add(GRANT_TTL_MS);
    Grant {
        signature: key
            .sign(&message(&id, method, &hash, expires))
            .to_bytes()
            .to_vec(),
        grant_id: id,
        method: method.into(),
        params_hash: hash,
        expires_at: expires,
    }
}

#[derive(Default)]
pub struct Verifier {
    key: Option<VerifyingKey>,
    used: HashMap<String, i64>,
}
fn required() -> ApiError {
    ApiError::new(
        ErrorCode::ConfirmationRequired,
        "a fresh native confirmation for these params is required",
    )
}
impl Verifier {
    /// The transport must first verify the signed GUI executable and connection.
    /// No c9p endpoint accepts a key without that admission check.
    pub fn attach(&mut self, key: VerifyingKey) -> Result<(), ApiError> {
        if self.key.is_some() {
            return Err(ApiError::new(
                ErrorCode::NotAllowed,
                "a confirmer is already attached for this boot",
            ));
        }
        self.key = Some(key);
        Ok(())
    }
    pub fn consume(
        &mut self,
        grant: &Grant,
        method: &str,
        params: &Value,
        now: i64,
    ) -> Result<(), ApiError> {
        self.used.retain(|_, expiry| *expiry > now);
        if grant.expires_at <= now
            || grant.expires_at > now.saturating_add(GRANT_TTL_MS)
            || grant.grant_id.len() > 64
            || grant.grant_id.is_empty()
            || self.used.contains_key(&grant.grant_id)
            || grant.method != method
        {
            return Err(required());
        }
        let hash = sha256_hex(&serde_json::to_vec(params).map_err(|_| required())?);
        if hash != grant.params_hash {
            return Err(required());
        }
        let signature = Signature::from_slice(&grant.signature).map_err(|_| required())?;
        self.key
            .as_ref()
            .ok_or_else(required)?
            .verify_strict(
                &message(&grant.grant_id, method, &hash, grant.expires_at),
                &signature,
            )
            .map_err(|_| required())?;
        self.used.insert(grant.grant_id.clone(), grant.expires_at);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn grants_bind_params_method_key_and_expiry_and_are_single_use() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let params = json!({"lanEnabled":true});
        let grant = sign(&key, "gateway.policy.set", &params, 1000);
        let mut v = Verifier::default();
        assert_eq!(
            v.consume(&grant, "gateway.policy.set", &params, 1000)
                .unwrap_err()
                .code,
            ErrorCode::ConfirmationRequired
        );
        v.attach(key.verifying_key()).unwrap();
        assert!(v.attach(key.verifying_key()).is_err());
        assert!(v
            .consume(
                &grant,
                "gateway.policy.set",
                &json!({"lanEnabled":false}),
                1000
            )
            .is_err());
        assert!(v
            .consume(&grant, "gateway.pair.register", &params, 1000)
            .is_err());
        let bad = sign(
            &SigningKey::from_bytes(&[8; 32]),
            "gateway.policy.set",
            &params,
            1000,
        );
        assert!(v
            .consume(&bad, "gateway.policy.set", &params, 1000)
            .is_err());
        assert!(
            v.consume(&grant, "gateway.policy.set", &params, 0).is_err(),
            "more than 60 seconds into the future"
        );
        v.consume(&grant, "gateway.policy.set", &params, 1001)
            .unwrap();
        assert_eq!(
            v.consume(&grant, "gateway.policy.set", &params, 1002)
                .unwrap_err()
                .code,
            ErrorCode::ConfirmationRequired
        );
        assert!(
            v.consume(&grant, "gateway.policy.set", &params, 61_001)
                .is_err(),
            "older than 60 seconds"
        );
    }
}
