//! Core's persistent gateway client store (D15). This module does not expose
//! a network route. The caller performs native confirmation before issue or a
//! widening update. Each client is its own Ed25519 public key; the db holds no
//! secret a reader could replay. Handshake encodings live in `pubkey`.
use super::policy::RemoteAccess;
use super::pubkey::{self, AuthError, AuthRequest};
use crate::engine::api::{blocking, internal};
use crate::engine::authz::{self, Access};
use crate::engine::db::audit::{self, AuditEntry};
use crate::engine::db::Db;
use crate::engine::principal::Principal;
use crate::engine::proto::{ApiError, Ceiling, ErrorCode};
use ed25519_dalek::VerifyingKey;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;
use tokio::sync::watch;

/// A new gateway client id, `cli_<ulid>` in lowercase Crockford base32
/// (30 bytes). It fits the `client.<id>` annotation namespace (40-byte cap)
/// without shortening, and sorts by the ms it was made in.
pub fn new_client_id() -> String {
    let agent = crate::engine::db::new_agent_id();
    format!("cli_{}", agent["agt_".len()..].to_ascii_lowercase())
}
pub const IDLE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
pub const ABSOLUTE_MS: i64 = 180 * 24 * 60 * 60 * 1000;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Spec {
    pub label: String,
    pub origin: Option<String>,
    pub scopes: BTreeSet<String>,
    pub ceiling: Ceiling,
    pub expires_at: i64,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Client {
    pub client_id: String,
    pub label: String,
    pub origin: Option<String>,
    pub aud: String,
    pub scopes: BTreeSet<String>,
    pub ceiling: Ceiling,
    pub created_at: i64,
    pub last_used_at: i64,
    pub expires_at: i64,
}
impl Client {
    pub fn effective(&self, policy: &RemoteAccess) -> Access {
        let mut scopes = self.scopes.clone();
        if !policy.allow_conversation {
            scopes.remove("conversation:read");
        }
        if !policy.allow_history {
            scopes.remove("history:read");
        }
        if !policy.allow_memory {
            scopes.remove("memory:read");
        }
        Access {
            scopes,
            ceiling: self.ceiling.min(if policy.allow_conversation {
                Ceiling::Conversation
            } else {
                Ceiling::Metadata
            }),
            expires_at: Some(self.expires_at),
            idle_expires_at: Some(
                self.last_used_at
                    .saturating_add(IDLE_MS)
                    .min(self.expires_at),
            ),
        }
    }
}
fn denied(reason: &str) -> ApiError {
    ApiError::new(ErrorCode::NotAllowed, reason)
}
fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Client> {
    let scopes: String = row.get(4)?;
    let ceiling: String = row.get(5)?;
    let invalid = |index, error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    };
    Ok(Client {
        client_id: row.get(0)?,
        label: row.get(1)?,
        origin: row.get(2)?,
        aud: row.get(3)?,
        scopes: serde_json::from_str(&scopes).map_err(|e| invalid(4, e))?,
        ceiling: serde_json::from_str(&format!("\"{ceiling}\"")).map_err(|e| invalid(5, e))?,
        created_at: row.get(6)?,
        last_used_at: row.get(7)?,
        expires_at: row.get(8)?,
    })
}
const COLUMNS: &str =
    "client_id,label,origin,aud,scopes,ceiling,created_at,last_used_at,expires_at";
pub struct Store {
    db: Arc<Db>,
    changed: watch::Sender<u64>,
}
impl Store {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            changed: watch::channel(0).0,
        }
    }
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    pub async fn issue(
        &self,
        principal: &Principal,
        mut spec: Spec,
        policy: &RemoteAccess,
        now: i64,
        public_key: &VerifyingKey,
    ) -> Result<Client, ApiError> {
        authz::subset(principal, &spec.scopes, spec.ceiling, policy)?;
        if spec.label.len() > 256
            || spec.expires_at <= now
            || spec.expires_at > now.saturating_add(ABSOLUTE_MS)
        {
            return Err(denied("invalid client label or expiry"));
        }
        if public_key.is_weak() {
            return Err(denied("invalid client key"));
        }
        spec.origin = spec
            .origin
            .map(|v| super::security::canonical_origin(&v).ok_or_else(|| denied("invalid origin")))
            .transpose()?;
        let key = *public_key.as_bytes();
        let client = Client {
            client_id: new_client_id(),
            label: spec.label.clone(),
            origin: spec.origin.clone(),
            aud: self.db.machine_id().into(),
            scopes: spec.scopes.clone(),
            ceiling: spec.ceiling,
            created_at: now,
            last_used_at: now,
            expires_at: spec.expires_at,
        };
        let c = client.clone();
        let who = principal.to_string();
        let payload_hash =
            crate::engine::mail::sha256_hex(&serde_json::to_vec(&spec).map_err(internal)?);
        self.db.write(move |tx| {
            tx.execute("INSERT INTO gateway_clients (client_id,public_key,label,origin,aud,scopes,ceiling,created_at,last_used_at,expires_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8,?9)",
                params![c.client_id,key.as_slice(),c.label,c.origin,c.aud,serde_json::to_string(&c.scopes).unwrap(),serde_json::to_value(c.ceiling).unwrap().as_str().unwrap(),now,c.expires_at])?;
            audit::append(tx,now,&AuditEntry { principal:&who,method:"gateway.pair.register",target:Some(&c.client_id),decision:"allow",payload_hash:Some(&payload_hash),..Default::default() })?;
            Ok(())
        }).await.map_err(|e| {
            // One key pairs once; a second pairing must bring a new key.
            if e.to_string().contains("UNIQUE") { denied("client key already paired") } else { internal(e) }
        })?;
        Ok(client)
    }
    pub async fn list(&self) -> Result<Vec<Client>, ApiError> {
        let db = self.db.clone();
        blocking(move || {
            let conn = db.reader().map_err(internal)?;
            let mut st = conn
                .prepare(&format!(
                    "SELECT {COLUMNS} FROM gateway_clients ORDER BY created_at,client_id"
                ))
                .map_err(internal)?;
            let clients = st
                .query_map([], row)
                .map_err(internal)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(internal)?;
            Ok(clients)
        })
        .await
    }
    /// Audience and expiry are checked before any signature check runs.
    /// A native client has no Origin and must match that same binding later.
    pub async fn lookup(
        &self,
        id: &str,
        origin: Option<&str>,
        now: i64,
    ) -> Result<Client, ApiError> {
        let db = self.db.clone();
        let id = id.to_string();
        let origin = origin
            .map(|v| super::security::canonical_origin(v).ok_or_else(|| denied("invalid origin")))
            .transpose()?;
        blocking(move || {
            let conn = db.reader().map_err(internal)?;
            let c = conn
                .query_row(
                    &format!("SELECT {COLUMNS} FROM gateway_clients WHERE client_id=?1"),
                    [id],
                    row,
                )
                .optional()
                .map_err(internal)?
                .ok_or_else(|| denied("unknown client"))?;
            if c.aud != db.machine_id() {
                return Err(denied("wrong audience"));
            }
            if c.origin != origin {
                return Err(denied("wrong origin"));
            }
            if now < c.created_at
                || now >= c.expires_at
                || now.saturating_sub(c.last_used_at) >= IDLE_MS
            {
                return Err(denied("expired client"));
            }
            Ok(c)
        })
        .await
    }
    /// Check a reconnect `auth` frame against the client's stored key. Every
    /// failure, malformed or not, is `not_allowed` (close 4401). On success the
    /// caller replies with `pubkey::sign_auth` over the returned client nonce.
    pub async fn authenticate(
        &self,
        server_nonce: &[u8; 32],
        req: &AuthRequest,
        origin: Option<&str>,
        now: i64,
    ) -> Result<(Client, [u8; 32]), ApiError> {
        if !pubkey::valid_client_id(&req.client_id) {
            return Err(denied("malformed client id"));
        }
        let c = self.lookup(&req.client_id, origin, now).await?;
        let db = self.db.clone();
        let id = req.client_id.clone();
        let saved: Option<Vec<u8>> = blocking(move || {
            let conn = db.reader().map_err(internal)?;
            conn.query_row(
                "SELECT public_key FROM gateway_clients WHERE client_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)
        })
        .await?;
        let key = saved
            .and_then(|k| <[u8; 32]>::try_from(k.as_slice()).ok())
            .and_then(|k| pubkey::key_from_bytes(&k).ok())
            .ok_or_else(|| denied("unknown client"))?;
        let client_nonce =
            pubkey::verify_auth(server_nonce, &c.aud, &key, req).map_err(|e| match e {
                AuthError::Malformed => denied("malformed auth frame"),
                AuthError::WeakKey | AuthError::BadSignature => denied("bad client signature"),
            })?;
        let id = c.client_id.clone();
        let (changed, _)=self.db.write(move |tx|tx.execute("UPDATE gateway_clients SET last_used_at=?2 WHERE client_id=?1 AND expires_at>?2 AND last_used_at>?3",params![id,now,now.saturating_sub(IDLE_MS)])).await.map_err(internal)?;
        if changed != 1 {
            return Err(denied("expired or revoked client"));
        }
        Ok((
            Client {
                last_used_at: now,
                ..c
            },
            client_nonce,
        ))
    }
    pub async fn revoke(
        &self,
        principal: &Principal,
        id: Option<&str>,
        now: i64,
    ) -> Result<usize, ApiError> {
        if !matches!(principal, Principal::LocalUnverified) {
            return Err(denied("only local peers may revoke clients"));
        }
        let id = id.map(str::to_string);
        let who = principal.to_string();
        let (n, _) = self
            .db
            .write(move |tx| {
                let n = if let Some(id) = &id {
                    tx.execute("DELETE FROM gateway_clients WHERE client_id=?1", [id])?
                } else {
                    tx.execute("DELETE FROM gateway_clients", [])?
                };
                audit::append(
                    tx,
                    now,
                    &AuditEntry {
                        principal: &who,
                        method: "gateway.clients.revoke",
                        target: id.as_deref(),
                        decision: "allow",
                        ..Default::default()
                    },
                )?;
                Ok(n)
            })
            .await
            .map_err(internal)?;
        self.changed
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec(now: i64) -> Spec {
        Spec {
            label: "viewer".into(),
            origin: Some("https://view.example".into()),
            scopes: [
                "session:read".into(),
                "cost:read".into(),
                "annotation:read".into(),
            ]
            .into(),
            ceiling: Ceiling::Metadata,
            expires_at: now + ABSOLUTE_MS,
        }
    }
    fn client_key(seed: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
    }
    fn auth(
        key: &ed25519_dalek::SigningKey,
        server: &[u8; 32],
        aud: &str,
        id: &str,
    ) -> AuthRequest {
        use ed25519_dalek::Signer;
        let nonce = [0xabu8; 32];
        AuthRequest {
            client_id: id.into(),
            client_nonce: pubkey::to_hex(&nonce),
            sig: pubkey::to_hex(
                &key.sign(&pubkey::auth_client_message(server, &nonce, aud, id))
                    .to_bytes(),
            ),
        }
    }
    #[tokio::test]
    async fn persistence_origin_audience_expiry_and_revoke_fail_closed() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("engine");
        let db = Arc::new(crate::engine::db::open(&home, &"ab".repeat(32)).unwrap());
        let store = Store::new(db.clone());
        let now = 1000;
        let key = client_key(9);
        let c = store
            .issue(
                &Principal::LocalUnverified,
                spec(now),
                &RemoteAccess::default(),
                now,
                &key.verifying_key(),
            )
            .await
            .unwrap();
        assert!(c.client_id.starts_with("cli_") && c.client_id.len() == 30);
        let server = pubkey::new_nonce();
        let good = auth(&key, &server, &c.aud, &c.client_id);
        let (got, nonce) = store
            .authenticate(&server, &good, Some("https://view.example:443"), now + 1)
            .await
            .unwrap();
        assert_eq!(
            (got.client_id.as_str(), got.last_used_at),
            (c.client_id.as_str(), now + 1)
        );
        assert_eq!(nonce, [0xabu8; 32]);
        // Another key, another challenge, another audience: all refused.
        for bad in [
            auth(&client_key(10), &server, &c.aud, &c.client_id),
            auth(&key, &pubkey::new_nonce(), &c.aud, &c.client_id),
            auth(&key, &server, &"cd".repeat(32), &c.client_id),
            AuthRequest {
                client_nonce: "AB".repeat(32),
                ..good.clone()
            },
            AuthRequest {
                client_id: c.client_id.to_uppercase(),
                ..good.clone()
            },
        ] {
            assert_eq!(
                store
                    .authenticate(&server, &bad, Some("https://view.example"), now + 2)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::NotAllowed
            );
        }
        assert!(store
            .authenticate(&server, &good, Some("https://other.example"), now + 2)
            .await
            .is_err());
        assert!(store
            .authenticate(&server, &good, None, now + 2)
            .await
            .is_err());
        assert!(store
            .lookup(
                &c.client_id,
                Some("https://view.example"),
                now + 1 + IDLE_MS + 1
            )
            .await
            .is_err());
        assert!(store
            .lookup(
                &c.client_id,
                Some("https://view.example"),
                now + ABSOLUTE_MS
            )
            .await
            .is_err());
        // Only the public key is stored.
        let raw: Vec<u8> = db
            .reader()
            .unwrap()
            .query_row("SELECT public_key FROM gateway_clients", [], |r| r.get(0))
            .unwrap();
        assert_eq!(raw, key.verifying_key().as_bytes());
        // One key pairs once.
        assert!(store
            .issue(
                &Principal::LocalUnverified,
                spec(now),
                &RemoteAccess::default(),
                now,
                &key.verifying_key(),
            )
            .await
            .unwrap_err()
            .msg
            .contains("already paired"));
        let id = c.client_id.clone();
        db.write(move |tx| {
            tx.execute(
                "UPDATE gateway_clients SET aud='machine-b' WHERE client_id=?1",
                [id],
            )
        })
        .await
        .unwrap();
        assert!(store
            .lookup(&c.client_id, Some("https://view.example"), now + 2)
            .await
            .unwrap_err()
            .msg
            .contains("audience"));
        let mut changes = store.changes();
        assert_eq!(
            store
                .revoke(&Principal::LocalUnverified, Some(&c.client_id), now + 3)
                .await
                .unwrap(),
            1
        );
        changes.changed().await.unwrap();
        assert!(store
            .lookup(&c.client_id, Some("https://view.example"), now + 4)
            .await
            .is_err());
        assert!(store.list().await.unwrap().is_empty());
        drop(store);
        drop(db);
        let db = Arc::new(crate::engine::db::open(&home, &"ab".repeat(32)).unwrap());
        let store = Store::new(db);
        let c = store
            .issue(
                &Principal::LocalUnverified,
                spec(now),
                &RemoteAccess::default(),
                now,
                &key.verifying_key(),
            )
            .await
            .unwrap();
        drop(store);
        let store = Store::new(Arc::new(
            crate::engine::db::open(&home, &"ab".repeat(32)).unwrap(),
        ));
        let server = pubkey::new_nonce();
        assert!(store
            .authenticate(
                &server,
                &auth(&key, &server, &c.aud, &c.client_id),
                Some("https://view.example"),
                now + 1
            )
            .await
            .is_ok());
    }
    #[tokio::test]
    async fn weak_client_key_is_never_stored() {
        let t = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::engine::db::open(&t.path().join("e"), "m").unwrap());
        let store = Store::new(db);
        let mut identity = [0u8; 32];
        identity[0] = 1;
        let weak = VerifyingKey::from_bytes(&identity).unwrap();
        assert!(store
            .issue(
                &Principal::LocalUnverified,
                spec(0),
                &RemoteAccess::default(),
                0,
                &weak
            )
            .await
            .is_err());
        assert!(store.list().await.unwrap().is_empty());
    }
    #[test]
    fn client_ids_are_lowercase_prefixed_ulids() {
        let a = new_client_id();
        let b = new_client_id();
        assert_ne!(a, b);
        for id in [&a, &b] {
            assert_eq!(id.len(), 30, "{id}");
            let body = id.strip_prefix("cli_").unwrap();
            assert!(body
                .bytes()
                .all(|c| b"0123456789abcdefghjkmnpqrstvwxyz".contains(&c)));
            assert!(format!("client.{id}").len() <= 40);
        }
    }
    #[test]
    fn live_policy_never_widens_stored_scopes() {
        let c = Client {
            client_id: "c".into(),
            label: "viewer".into(),
            origin: None,
            aud: "m".into(),
            scopes: ["session:read".into()].into(),
            ceiling: Ceiling::Conversation,
            created_at: 0,
            last_used_at: 0,
            expires_at: ABSOLUTE_MS,
        };
        let mut p = RemoteAccess::default();
        assert_eq!(c.effective(&p).ceiling, Ceiling::Metadata);
        p.allow_conversation = true;
        let a = c.effective(&p);
        assert_eq!(a.ceiling, Ceiling::Conversation);
        assert!(!a.scopes.contains("conversation:read"));
        assert!(
            !authz::can(
                "conversation.get",
                &Principal::Client { id: "c".into() },
                Some(&a)
            )
            .allowed
        );
        p.allow_conversation = false;
        assert_eq!(c.effective(&p).ceiling, Ceiling::Metadata);
    }
}
