//! Session views: the sessions of the last detection tick, in the shape
//! the GUI gets from `get_sessions` and `sessions-updated` (the enriched
//! `Session`, with the workers overlay), kept in memory.
//!
//! Each tick diffs its sessions against the last committed views, by their
//! JSON. A new or changed session emits `session.view` with the full view;
//! a session that is no longer listed emits `session.gone`. The events go
//! in the tick's own transaction, so a client that reads `session.views`
//! and subscribes with its cursor misses nothing. An unchanged tick emits
//! nothing. The views follow detection exactly, as the GUI poll does: a
//! provider that did not answer drops its sessions from the views (the
//! agent rows hold them for a few ticks; see `detect`).
//!
//! The c9p methods here: `session.views`, `session.away`, and the bodies of
//! `detect.recheck`, `session.stop` and `session.rename` (their principal
//! check is in `api`).

use super::params;
use crate::engine::api::{blocking, internal, ServerCtx};
use crate::engine::db::outbox::NewEvent;
use crate::engine::detect::{Detection, StopError};
use crate::engine::proto::{
    ApiError, ErrorCode, Request, SessionAwayParams, SessionAwayResult, SessionGoneEvent,
    SessionRenameParams, SessionStopParams, SessionViewsResult, TickResult,
};
use crate::session::{DetectionDiagnostics, Session};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const VIEW_TOPIC: &str = "session.view";
pub const GONE_TOPIC: &str = "session.gone";

/// How long `session.stop` waits after the SIGTERM before its tick, as the
/// GUI's `stop_session` does before it pushes a snapshot.
pub const STOP_SETTLE: Duration = Duration::from_millis(300);

/// One session's view as the last commit saw it.
struct Seen {
    id: String,
    json: String,
    /// The `seq` of the stored `session.view` event with this view, once
    /// written. A newer view (or the session's end) deletes that event.
    view_seq: Option<i64>,
}

struct Committed {
    cursor: String,
    sessions: Vec<Session>,
    by_key: HashMap<String, Seen>,
}

/// The last committed views. `None` until the first tick commits.
#[derive(Default)]
pub struct SessionViews {
    state: Mutex<Option<Committed>>,
    /// The last detection diagnostics; `None` before the first tick.
    diagnostics: Mutex<Option<DetectionDiagnostics>>,
    /// Woken on every commit, for `wait_ready`.
    committed: tokio::sync::Notify,
}

/// What one tick changes, computed before its write and committed after.
pub struct ViewChange {
    sessions: Vec<Session>,
    by_key: HashMap<String, Seen>,
    /// `(session key, view)` of new or changed sessions, in detection order.
    pub changed: Vec<(String, serde_json::Value)>,
    pub gone: Vec<SessionGoneEvent>,
    /// The stored `session.view` events this change supersedes: the last
    /// view of each changed or ended session.
    pub superseded: Vec<i64>,
    /// The first diff since the engine started: views stored by an earlier
    /// run are superseded too (`outbox::compact_views_before`).
    pub first: bool,
}

impl ViewChange {
    /// The tick's `session.*` events. Subject: `<machine>/<session key>`.
    pub fn events(&self, machine: &str) -> Vec<NewEvent> {
        let view = self.changed.iter().map(|(key, data)| NewEvent {
            topic: VIEW_TOPIC.into(),
            subject: Some(format!("{}/{}", machine, key)),
            origin: None,
            data: data.clone(),
        });
        let gone = self.gone.iter().map(|g| NewEvent {
            topic: GONE_TOPIC.into(),
            subject: Some(format!("{}/{}", machine, g.session_key)),
            origin: None,
            data: serde_json::to_value(g).expect("SessionGoneEvent serializes"),
        });
        view.chain(gone).collect()
    }

    /// Record the `seq` of each `session.view` event the tick wrote, in
    /// `changed` order.
    pub fn written(&mut self, seqs: &[i64]) {
        for ((key, _), seq) in self.changed.iter().zip(seqs) {
            if let Some(seen) = self.by_key.get_mut(key) {
                seen.view_seq = Some(*seq);
            }
        }
    }
}

impl SessionViews {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Committed>> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Diff `sessions` against the last commit. A session key listed twice
    /// counts once, by its first entry; the views keep both, as the GUI
    /// does.
    pub fn diff(&self, sessions: &[Session]) -> ViewChange {
        let state = self.lock();
        let empty = HashMap::new();
        let before = state.as_ref().map_or(&empty, |c| &c.by_key);
        let mut by_key = HashMap::new();
        let mut changed = Vec::new();
        let mut superseded = Vec::new();
        for s in sessions {
            if by_key.contains_key(&s.session_key) {
                continue;
            }
            let value = serde_json::to_value(s).expect("Session serializes");
            let json = value.to_string();
            let prev = before.get(&s.session_key);
            let view_seq = match prev {
                Some(b) if b.json == json => b.view_seq,
                _ => {
                    changed.push((s.session_key.clone(), value));
                    superseded.extend(prev.and_then(|b| b.view_seq));
                    None
                }
            };
            by_key.insert(
                s.session_key.clone(),
                Seen {
                    id: s.id.clone(),
                    json,
                    view_seq,
                },
            );
        }
        let mut gone: Vec<SessionGoneEvent> = before
            .iter()
            .filter(|(k, _)| !by_key.contains_key(*k))
            .map(|(k, b)| {
                superseded.extend(b.view_seq);
                SessionGoneEvent {
                    session_key: k.clone(),
                    id: b.id.clone(),
                }
            })
            .collect();
        gone.sort_by(|a, b| a.session_key.cmp(&b.session_key));
        superseded.sort_unstable();
        ViewChange {
            sessions: sessions.to_vec(),
            by_key,
            changed,
            gone,
            superseded,
            first: state.is_none(),
        }
    }

    /// Make `change` the current views, consistent with `cursor`.
    pub fn commit(&self, change: ViewChange, cursor: String) {
        *self.lock() = Some(Committed {
            cursor,
            sessions: change.sessions,
            by_key: change.by_key,
        });
        self.committed.notify_waiters();
    }

    /// Wait up to `timeout` for the first commit: detection's first
    /// successful tick. A failed tick commits nothing. Returns whether
    /// views are committed.
    pub async fn wait_ready(&self, timeout: std::time::Duration) -> bool {
        let ready = async {
            loop {
                let woken = self.committed.notified();
                tokio::pin!(woken);
                woken.as_mut().enable();
                let committed = self.lock().is_some();
                if committed {
                    return;
                }
                woken.await;
            }
        };
        tokio::time::timeout(timeout, ready).await.is_ok()
    }

    pub fn diagnostics(&self) -> Option<DetectionDiagnostics> {
        self.diagnostics
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn set_diagnostics(&self, d: DetectionDiagnostics) {
        *self.diagnostics.lock().unwrap_or_else(|p| p.into_inner()) = Some(d);
    }

    /// The current views and their cursor; `None` before the first commit.
    pub fn current(&self) -> Option<(String, Vec<Session>)> {
        self.lock()
            .as_ref()
            .map(|c| (c.cursor.clone(), c.sessions.clone()))
    }
}

fn detection(ctx: &ServerCtx) -> Result<&Arc<Detection>, ApiError> {
    ctx.detection
        .as_ref()
        .ok_or_else(|| ApiError::new(ErrorCode::Internal, "this engine runs no detection"))
}

/// `session.views`. Waits for detection's first pass, as `session.list`
/// does. An engine without detection answers no sessions at the head
/// cursor.
pub async fn session_views(ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    ctx.detected().await;
    let current = ctx.detection.as_ref().and_then(|d| d.views.current());
    let (cursor, sessions) = match current {
        Some(c) => c,
        None => (ctx.db()?.head_cursor().to_string(), Vec::new()),
    };
    Ok(
        serde_json::to_value(SessionViewsResult { cursor, sessions })
            .expect("SessionViewsResult serializes"),
    )
}

/// `diag.detection`: the diagnostics of the last tick, as the GUI's
/// `get_detection_diagnostics` (all zero before the first tick).
pub async fn diag_detection(req: Request, ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let _: crate::engine::proto::DiagDetectionParams = params("diag.detection", req)?;
    ctx.detected().await;
    let d = ctx
        .detection
        .as_ref()
        .and_then(|d| d.views.diagnostics())
        .unwrap_or_default();
    Ok(serde_json::to_value(d).expect("DetectionDiagnostics serializes"))
}

/// `session.away`: the away-activity index that each detection tick keeps
/// current. It reads the index only and never parses a transcript.
pub async fn session_away(req: Request, ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let p: SessionAwayParams = params("session.away", req)?;
    ctx.detected().await;
    let activity = blocking(move || Ok(crate::session::away::away_activity(&p.requests))).await?;
    Ok(serde_json::to_value(SessionAwayResult { activity }).expect("SessionAwayResult serializes"))
}

/// Run one tick and answer its cursor. Blocking work runs off the async
/// workers.
async fn tick(d: Arc<Detection>) -> Result<TickResult, ApiError> {
    blocking(move || d.tick().map_err(internal)).await
}

/// `detect.recheck`: pick the detection backend again (as the GUI does on
/// window focus), then run one tick. Its changes are emitted before the
/// answer.
pub async fn detect_recheck(ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let d = detection(ctx)?.clone();
    ctx.detected().await;
    let r = blocking(move || d.recheck().map_err(internal)).await?;
    Ok(serde_json::to_value(r).expect("TickResult serializes"))
}

/// `session.stop`: SIGTERM an observed Claude Code session, as the GUI's
/// `stop_session` does. After `STOP_SETTLE` one tick runs, so the
/// session's `session.gone` (or its changed `session.view`) is in the log
/// before the answer.
pub async fn session_stop(req: Request, ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let p: SessionStopParams = params("session.stop", req)?;
    if p.session_key.is_empty() {
        return Err(ApiError::new(
            ErrorCode::BadRequest,
            "bad session.stop params: sessionKey is required",
        ));
    }
    let d = detection(ctx)?.clone();
    ctx.detected().await;
    let stopper = d.clone();
    blocking(move || {
        stopper.stop(&p.session_key, p.pid).map_err(|e| match e {
            StopError::Unknown(m) => ApiError::new(ErrorCode::UnknownSession, m),
            StopError::Failed(m) => ApiError::new(ErrorCode::ActionFailed, m),
            StopError::Internal(m) => internal(m),
        })
    })
    .await?;
    tokio::time::sleep(STOP_SETTLE).await;
    Ok(serde_json::to_value(tick(d).await?).expect("TickResult serializes"))
}

/// `session.rename`: the GUI's `rename_session` (validate, then write the
/// native and fallback titles), then one tick so the new title's
/// `session.view` is in the log before the answer.
pub async fn session_rename(req: Request, ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let p: SessionRenameParams = params("session.rename", req)?;
    if p.session_id.is_empty() {
        return Err(ApiError::new(
            ErrorCode::BadRequest,
            "bad session.rename params: sessionId is required",
        ));
    }
    let d = detection(ctx)?.clone();
    ctx.detected().await;
    blocking(move || {
        let owners = crate::session::global_provider_source_owners();
        // The GUI's `run_validated_rename`, split so that a refused request
        // is `bad_request` and a failed write `action_failed`.
        crate::session::validate_rename_request(p.provider, &p.session_id, &owners)
            .map_err(|m| ApiError::new(ErrorCode::BadRequest, m))?;
        crate::session::rename::write_session_title(&p.session_id, &p.new_name)
            .map_err(|m| ApiError::new(ErrorCode::ActionFailed, m))
    })
    .await?;
    Ok(serde_json::to_value(tick(d).await?).expect("TickResult serializes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionStatus;

    fn s(id: &str, status: SessionStatus) -> Session {
        Session::fixture(id, status)
    }

    #[test]
    fn a_diff_names_new_changed_and_gone_sessions_only() {
        let v = SessionViews::default();
        let c = v.diff(&[
            s("a", SessionStatus::Working),
            s("b", SessionStatus::Working),
        ]);
        assert_eq!(c.changed.len(), 2);
        assert!(c.gone.is_empty());
        v.commit(c, "m:e:1".into());

        // Unchanged: nothing.
        let c = v.diff(&[
            s("a", SessionStatus::Working),
            s("b", SessionStatus::Working),
        ]);
        assert!(c.changed.is_empty() && c.gone.is_empty());

        // `a` changes, `b` goes.
        let c = v.diff(&[s("a", SessionStatus::WaitingForInput)]);
        assert_eq!(c.changed.len(), 1);
        assert_eq!(c.changed[0].0, "claudeCode:a");
        assert_eq!(c.changed[0].1["status"], "WaitingForInput");
        assert_eq!(
            c.gone,
            vec![SessionGoneEvent {
                session_key: "claudeCode:b".into(),
                id: "b".into()
            }]
        );
        let ev = c.events("m");
        assert_eq!(ev[0].topic, VIEW_TOPIC);
        assert_eq!(ev[0].subject.as_deref(), Some("m/claudeCode:a"));
        assert_eq!(ev[1].topic, GONE_TOPIC);
        assert_eq!(ev[1].data["sessionKey"], "claudeCode:b");
    }

    #[test]
    fn a_view_is_the_gui_session_json() {
        let v = SessionViews::default();
        let mut one = s("a", SessionStatus::Working);
        one.worker_of = Some("pm".into());
        let c = v.diff(std::slice::from_ref(&one));
        assert_eq!(c.changed[0].1, serde_json::to_value(&one).unwrap());
        v.commit(c, "m:e:7".into());
        let (cursor, sessions) = v.current().unwrap();
        assert_eq!(cursor, "m:e:7");
        assert_eq!(
            serde_json::to_value(&sessions).unwrap(),
            serde_json::to_value(vec![one]).unwrap()
        );
    }
}
