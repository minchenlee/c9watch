//! `idem` replay for the writes that keep no request record of their own
//! (D3): `agent.create`, `agent.retire` and `session.stop`. `msg.send`
//! keeps its key with the message (`engine::mail`).
//!
//! The rule is the one `msg.send` uses: the same principal, method and key
//! with the same params replays the first result and does not repeat the
//! write; other params are `duplicate`. Only a success is kept: after a
//! failure the next request with the key runs the write again. Two requests
//! with one key never run at once; the second waits for the first.
//!
//! The record is in memory, so it ends with the engine process, and holds
//! at most `CAP` keys for at most `TTL`. A key that a request still uses is
//! never dropped; when all `CAP` keys are in use, a new key gets `busy`.

use super::proto::{ApiError, ErrorCode};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a key replays.
pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Keys kept at most. Past it, the oldest kept result that no request uses
/// goes first.
pub const CAP: usize = 1024;

/// Longest key, in bytes.
pub const MAX_KEY: usize = 128;

/// The first success for one key.
pub(crate) struct Done {
    hash: String,
    result: serde_json::Value,
    at: Instant,
}

/// One key's slot. Its lock is held while the write runs.
pub(crate) type Slot = Arc<tokio::sync::Mutex<Option<Done>>>;

/// (principal, method, key)
pub(crate) type Key = (String, String, String);

#[derive(Default)]
pub struct IdemCache {
    slots: Mutex<HashMap<Key, Slot>>,
}

impl IdemCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `write` once for `key`, or replay its first result. Returns the
    /// result and whether it is a replay.
    pub async fn run<F, Fut>(
        &self,
        principal: &str,
        method: &str,
        key: &str,
        params: &serde_json::Value,
        write: F,
    ) -> Result<(serde_json::Value, bool), ApiError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<serde_json::Value, ApiError>>,
    {
        if key.is_empty() || key.len() > MAX_KEY || key.chars().any(char::is_control) {
            return Err(ApiError::new(
                ErrorCode::BadRequest,
                format!(
                    "idem must be 1 to {} bytes without control characters",
                    MAX_KEY
                ),
            ));
        }
        let hash = super::mail::sha256_hex(
            &serde_json::to_vec(&(method, params)).expect("params serialize"),
        );
        let slot = self.slot((principal.into(), method.into(), key.into()))?;
        let mut done = slot.lock().await;
        if let Some(d) = done.as_ref().filter(|d| d.at.elapsed() < TTL) {
            if d.hash != hash {
                return Err(ApiError::new(
                    ErrorCode::Duplicate,
                    format!("idem {:?} was used for another {} request", key, method),
                )
                .with_hint(format!("idem {} was used for another request", key)));
            }
            return Ok((d.result.clone(), true));
        }
        let result = write().await?;
        *done = Some(Done {
            hash,
            result: result.clone(),
            at: Instant::now(),
        });
        Ok((result, false))
    }

    /// The slot for `key`, made if new. At `CAP`, a new key first drops the
    /// unused slots that hold nothing or an expired result, then the unused
    /// slot with the oldest result.
    ///
    /// A slot is in use while a caller holds its `Arc`: a caller clones it
    /// under this lock and keeps it until its write ends, also before it
    /// locks the slot. Dropping a slot in use would let a second request
    /// with the key make another slot and run the write again, so only a
    /// slot the map alone holds may go.
    pub(crate) fn slot(&self, key: Key) -> Result<Slot, ApiError> {
        let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(s) = slots.get(&key) {
            return Ok(s.clone());
        }
        let unused = |s: &Slot| Arc::strong_count(s) == 1;
        if slots.len() >= CAP {
            slots.retain(|_, s| {
                !unused(s)
                    || s.try_lock()
                        .is_ok_and(|d| d.as_ref().is_some_and(|d| d.at.elapsed() < TTL))
            });
        }
        if slots.len() >= CAP {
            let oldest = slots
                .iter()
                .filter(|(_, s)| unused(s))
                .filter_map(|(k, s)| Some((k.clone(), s.try_lock().ok()?.as_ref()?.at)))
                .min_by_key(|(_, at)| *at)
                .map(|(k, _)| k);
            let Some(k) = oldest else {
                return Err(ApiError::new(
                    ErrorCode::Busy,
                    format!("all {} idem keys are in use", CAP),
                ));
            };
            slots.remove(&k);
        }
        Ok(slots.entry(key).or_default().clone())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.slots.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn count(
        c: &IdemCache,
        runs: &AtomicUsize,
        who: &str,
        key: &str,
        params: serde_json::Value,
    ) -> Result<(serde_json::Value, bool), ApiError> {
        c.run(who, "agent.create", key, &params, || async {
            let n = runs.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "n": n }))
        })
        .await
    }

    #[tokio::test]
    async fn the_same_key_and_params_replay_the_first_result() {
        let (c, runs) = (IdemCache::new(), AtomicUsize::new(0));
        let first = count(&c, &runs, "local-unverified", "k", json!({"a": 1})).await;
        assert_eq!(first.unwrap(), (json!({"n": 0}), false));
        let again = count(&c, &runs, "local-unverified", "k", json!({"a": 1})).await;
        assert_eq!(again.unwrap(), (json!({"n": 0}), true));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        // Another principal or key is another write.
        let other = count(&c, &runs, "observed:s", "k", json!({"a": 1})).await;
        assert_eq!(other.unwrap(), (json!({"n": 1}), false));
        let e = count(&c, &runs, "local-unverified", "k", json!({"a": 2}))
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Duplicate);
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_failure_is_not_kept() {
        let c = IdemCache::new();
        let e = c
            .run("l", "session.stop", "k", &json!({}), || async {
                Err(ApiError::new(ErrorCode::ActionFailed, "no"))
            })
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::ActionFailed);
        let ok = c
            .run("l", "session.stop", "k", &json!({}), || async {
                Ok(json!(1))
            })
            .await;
        assert_eq!(ok.unwrap(), (json!(1), false));
    }

    #[tokio::test]
    async fn concurrent_requests_with_one_key_write_once() {
        let c = Arc::new(IdemCache::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let call = |c: Arc<IdemCache>, runs: Arc<AtomicUsize>| async move {
            c.run("l", "agent.retire", "k", &json!({}), || async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(json!(runs.fetch_add(1, Ordering::SeqCst)))
            })
            .await
            .unwrap()
        };
        let (a, b) = tokio::join!(call(c.clone(), runs.clone()), call(c.clone(), runs.clone()));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(a.0, b.0);
        assert!(a.1 != b.1, "one ran, one replayed");
    }

    #[tokio::test]
    async fn keys_are_bounded() {
        let (c, runs) = (IdemCache::new(), AtomicUsize::new(0));
        for k in 0..CAP + 10 {
            count(&c, &runs, "l", &k.to_string(), json!({}))
                .await
                .unwrap();
        }
        assert!(c.len() <= CAP);
        for bad in ["", &"x".repeat(MAX_KEY + 1), "a\nb"] {
            let e = count(&c, &runs, "l", bad, json!({})).await.unwrap_err();
            assert_eq!(e.code, ErrorCode::BadRequest);
        }
    }

    /// A key whose caller has its slot but has not locked it yet is in use:
    /// keys that come after it at `CAP` may not drop it, so the key keeps
    /// one slot and its write runs once.
    #[tokio::test]
    async fn a_key_in_use_is_never_dropped() {
        let (c, runs) = (IdemCache::new(), AtomicUsize::new(0));
        let a = c
            .slot(("l".into(), "agent.create".into(), "a".into()))
            .unwrap();
        for k in 0..CAP + 10 {
            count(&c, &runs, "l", &k.to_string(), json!({}))
                .await
                .unwrap();
        }
        let again = c
            .slot(("l".into(), "agent.create".into(), "a".into()))
            .unwrap();
        assert!(Arc::ptr_eq(&a, &again), "the key kept its slot");
        assert!(c.len() <= CAP);
    }

    #[tokio::test]
    async fn a_new_key_is_busy_when_every_key_is_in_use() {
        let (c, runs) = (IdemCache::new(), AtomicUsize::new(0));
        let held: Vec<_> = (0..CAP)
            .map(|k| {
                c.slot(("l".into(), "agent.create".into(), k.to_string()))
                    .unwrap()
            })
            .collect();
        let e = count(&c, &runs, "l", "new", json!({})).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Busy);
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        drop(held);
        let ok = count(&c, &runs, "l", "new", json!({})).await;
        assert_eq!(ok.unwrap(), (json!({"n": 0}), false));
    }
}
