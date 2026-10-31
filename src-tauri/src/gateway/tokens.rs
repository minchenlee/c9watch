//! Core's persistent token store. This module does not expose a network route.
//! The caller performs native confirmation before issue or a widening update.
//! Only hashes are persisted; no raw-token HMAC verifier can be built from them.
use super::policy::RemoteAccess;
use crate::engine::api::{blocking, internal};
use crate::engine::authz::{self, Access};
use crate::engine::db::audit::{self, AuditEntry};
use crate::engine::db::Db;
use crate::engine::principal::Principal;
use crate::engine::proto::{ApiError, Ceiling, ErrorCode};
use rand::RngCore;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::watch;

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
fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
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
    ) -> Result<(Client, String), ApiError> {
        authz::subset(principal, &spec.scopes, spec.ceiling, policy)?;
        if spec.label.len() > 256
            || spec.expires_at <= now
            || spec.expires_at > now.saturating_add(ABSOLUTE_MS)
        {
            return Err(denied("invalid token label or expiry"));
        }
        spec.origin = spec
            .origin
            .map(|v| super::security::canonical_origin(&v).ok_or_else(|| denied("invalid origin")))
            .transpose()?;
        let mut bytes = [0; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let digest = hash(&token);
        let client = Client {
            client_id: uuid::Uuid::new_v4().to_string(),
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
            tx.execute("INSERT INTO gateway_tokens (client_id,sha256,label,origin,aud,scopes,ceiling,created_at,last_used_at,expires_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8,?9)",
                params![c.client_id,digest.as_slice(),c.label,c.origin,c.aud,serde_json::to_string(&c.scopes).unwrap(),serde_json::to_value(c.ceiling).unwrap().as_str().unwrap(),now,c.expires_at])?;
            audit::append(tx,now,&AuditEntry { principal:&who,method:"gateway.pair.register",target:Some(&c.client_id),decision:"allow",payload_hash:Some(&payload_hash),..Default::default() })?;
            Ok(())
        }).await.map_err(internal)?;
        Ok((client, token))
    }
    pub async fn list(&self) -> Result<Vec<Client>, ApiError> {
        let db = self.db.clone();
        blocking(move || {
            let conn = db.reader().map_err(internal)?;
            let mut st = conn
                .prepare(&format!(
                    "SELECT {COLUMNS} FROM gateway_tokens ORDER BY created_at,client_id"
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
    /// Audience and expiry are checked before a protocol's MAC check can run.
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
                    &format!("SELECT {COLUMNS} FROM gateway_tokens WHERE client_id=?1"),
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
                return Err(denied("expired token"));
            }
            Ok(c)
        })
        .await
    }
    /// Legacy /ws bearer check only. This is deliberately not a /c9p MAC check.
    pub async fn verify_bearer(
        &self,
        id: &str,
        token: &str,
        origin: Option<&str>,
        now: i64,
    ) -> Result<Client, ApiError> {
        let c = self.lookup(id, origin, now).await?;
        let db = self.db.clone();
        let id = id.to_string();
        let candidate = hash(token);
        let matched = blocking(move || {
            let conn = db.reader().map_err(internal)?;
            let saved: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT sha256 FROM gateway_tokens WHERE client_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(internal)?;
            Ok(saved.is_some_and(|s| bool::from(s.as_slice().ct_eq(&candidate))))
        })
        .await?;
        if !matched {
            return Err(denied("wrong token"));
        }
        let id = c.client_id.clone();
        let (changed, _)=self.db.write(move |tx|tx.execute("UPDATE gateway_tokens SET last_used_at=?2 WHERE client_id=?1 AND expires_at>?2 AND last_used_at>?3",params![id,now,now.saturating_sub(IDLE_MS)])).await.map_err(internal)?;
        if changed != 1 {
            return Err(denied("expired or revoked token"));
        }
        Ok(Client {
            last_used_at: now,
            ..c
        })
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
                    tx.execute("DELETE FROM gateway_tokens WHERE client_id=?1", [id])?
                } else {
                    tx.execute("DELETE FROM gateway_tokens", [])?
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
    #[tokio::test]
    async fn persistence_origin_audience_expiry_and_revoke_fail_closed() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("engine");
        let db = Arc::new(crate::engine::db::open(&home, "machine-a").unwrap());
        let store = Store::new(db.clone());
        let now = 1000;
        let (c, token) = store
            .issue(
                &Principal::LocalUnverified,
                spec(now),
                &RemoteAccess::default(),
                now,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .verify_bearer(
                    &c.client_id,
                    &token,
                    Some("https://view.example:443"),
                    now + 1
                )
                .await
                .unwrap()
                .client_id,
            c.client_id
        );
        assert!(store
            .verify_bearer(&c.client_id, "wrong", Some("https://view.example"), now + 2)
            .await
            .is_err());
        assert!(store
            .lookup(&c.client_id, Some("https://other.example"), now + 2)
            .await
            .is_err());
        assert!(store.lookup(&c.client_id, None, now + 2).await.is_err());
        assert!(store
            .lookup(
                &c.client_id,
                Some("https://view.example"),
                now + IDLE_MS + 1
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
        let raw: Vec<u8> = db
            .reader()
            .unwrap()
            .query_row("SELECT sha256 FROM gateway_tokens", [], |r| r.get(0))
            .unwrap();
        assert_eq!(raw.len(), 32);
        assert_ne!(raw, token.as_bytes());
        let id = c.client_id.clone();
        db.write(move |tx| {
            tx.execute(
                "UPDATE gateway_tokens SET aud='machine-b' WHERE client_id=?1",
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
        let db = Arc::new(crate::engine::db::open(&home, "machine-a").unwrap());
        let store = Store::new(db);
        let (c, token) = store
            .issue(
                &Principal::LocalUnverified,
                spec(now),
                &RemoteAccess::default(),
                now,
            )
            .await
            .unwrap();
        drop(store);
        let store = Store::new(Arc::new(
            crate::engine::db::open(&home, "machine-a").unwrap(),
        ));
        assert!(store
            .verify_bearer(&c.client_id, &token, Some("https://view.example"), now + 1)
            .await
            .is_ok());
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
