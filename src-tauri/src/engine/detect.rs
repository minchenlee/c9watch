//! Session detection and notification triggers in the engine (plan C1a-15,
//! C1a-16).
//!
//! A `DetectorState` with enrichment runs on its own thread every `TICK`,
//! like the GUI poll. The engine has no window, so the backend recheck that
//! the GUI runs on window focus comes from the clock here:
//! `DetectorState::detect` runs it when `legacy_recheck_due`.
//!
//! Each tick writes one transaction, with its events:
//! - One `role=observed`, `control=none`, `source=detected` row per
//!   session, keyed by `SessionIdentity::key()` (A:472). A new or changed
//!   row emits `agent.upsert`; an unchanged one only moves `last_seen`, so
//!   an idle machine adds no events.
//! - A live `control=pty` row whose `provider_sid` is the session's id
//!   takes the session instead, so an engine-started agent is never listed
//!   twice. A detected row of that session goes.
//! - Detected rows whose session is gone get `gone_at` and emit
//!   `agent.gone`. A session that comes back gets its row back.
//!
//! - A session that moved from working to needs-attention or
//!   waiting-for-input emits `notification.needs_attention` or
//!   `notification.waiting_for_input`, by the rule the GUI uses
//!   (`session::transitions`) and with its 30 s cooldown per session. The
//!   GUI keeps firing its own notifications; these events are for engine
//!   clients.
//!
//! A failed detection writes nothing, so one bad pass does not mark every
//! session gone. A tick keeps nothing between ticks: it reads the rows it
//! compares against from the db. After `quick_check` starts a new epoch,
//! the next tick rebuilds the observed rows from provider data (A:497).

use super::db::outbox::NewEvent;
use super::db::{agent_row, new_agent_id, now_ms, Db, Tx, AGENT_COLUMNS};
use super::proto::AgentRow;
use crate::session::enrichment::enrich_detected_sessions;
use crate::session::transitions::notification_transitions;
use crate::session::{
    DetectedSession, DetectionDiagnostics, DetectorState, Session, SessionStatus,
};
use rusqlite::{params, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Time between ticks; the GUI polls at the same rate.
pub const TICK: Duration = Duration::from_millis(3500);

/// One session notifies at most once per this long, as in the GUI poll, so
/// a flickering status does not repeat a notification.
pub const NOTIFY_COOLDOWN: Duration = Duration::from_secs(30);

/// Where a tick's sessions come from.
pub trait Observe: Send {
    fn observe(&mut self) -> Result<Vec<Session>, String>;
}

type Enrich = fn(Vec<DetectedSession>, DetectionDiagnostics) -> Result<Vec<Session>, String>;

/// Detection plus enrichment, as the GUI poll runs them.
pub struct Live {
    state: DetectorState,
    enrich: Enrich,
}

impl Live {
    /// Picks the backend, which may probe `claude`: call it on the
    /// detection thread.
    pub fn new() -> Self {
        Self::with(DetectorState::new(), |d, diag| {
            enrich_detected_sessions(d, diag).map(|(s, _)| s)
        })
    }

    pub fn with(state: DetectorState, enrich: Enrich) -> Self {
        Self { state, enrich }
    }

    pub fn backend_name(&self) -> &'static str {
        self.state.backend_name()
    }
}

impl Observe for Live {
    fn observe(&mut self) -> Result<Vec<Session>, String> {
        let (detected, diag) = self
            .state
            .detect()
            .map_err(|e| format!("Detect failed: {}", e))?;
        (self.enrich)(detected, diag)
    }
}

/// One detected session, as a tick writes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Observed {
    /// `SessionIdentity::key()`.
    pub key: String,
    pub provider: String,
    pub sid: String,
    pub status: &'static str,
    pub cwd: String,
    pub name: String,
    pub pid: i64,
    pub started_at: Option<i64>,
}

impl Observed {
    pub fn from_session(s: &Session) -> Self {
        Self {
            key: s.session_key.clone(),
            provider: s.provider.as_key().to_string(),
            sid: s.id.clone(),
            status: status_name(&s.status),
            cwd: s.project_path.clone(),
            name: s.session_name.clone(),
            pid: i64::from(s.pid),
            started_at: s.started_at_ms,
        }
    }
}

/// The `agents.status` value of a detected status.
pub fn status_name(s: &SessionStatus) -> &'static str {
    match s {
        SessionStatus::Working => "working",
        SessionStatus::NeedsAttention => "needs_attention",
        SessionStatus::WaitingForInput => "waiting_for_input",
        SessionStatus::Connecting => "connecting",
    }
}

/// What one tick changed: agent ids.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Applied {
    /// Rows that were created, changed or came back.
    pub upserted: Vec<String>,
    /// Rows that got `gone_at`.
    pub gone: Vec<String>,
    /// `(topic, agent id)` of each notification event.
    pub notified: Vec<(String, String)>,
}

/// A notification a tick emits. It carries no conversation text: the
/// session name and the pending tool name only.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub topic: &'static str,
    pub key: String,
    pub status: &'static str,
    pub name: String,
    pub pending_tool: Option<String>,
}

/// Which sessions notify, tick to tick.
#[derive(Default)]
pub struct Notifier {
    prev: HashMap<String, SessionStatus>,
    started: bool,
    last: HashMap<String, Instant>,
}

impl Notifier {
    /// This tick's notifications. The first tick only seeds the statuses.
    pub fn step(&mut self, sessions: &[Session], now: Instant) -> Vec<Notification> {
        let first = !self.started;
        self.started = true;
        let mut out = Vec::new();
        for s in notification_transitions(&mut self.prev, first, sessions) {
            let cooling = self
                .last
                .get(&s.session_key)
                .is_some_and(|t| now.saturating_duration_since(*t) < NOTIFY_COOLDOWN);
            if cooling {
                continue;
            }
            self.last.insert(s.session_key.clone(), now);
            let topic = match s.status {
                SessionStatus::NeedsAttention => "notification.needs_attention",
                _ => "notification.waiting_for_input",
            };
            out.push(Notification {
                topic,
                key: s.session_key.clone(),
                status: status_name(&s.status),
                name: s.session_name.clone(),
                pending_tool: s.pending_tool_name.clone(),
            });
        }
        let live: HashSet<&str> = sessions.iter().map(|s| s.session_key.as_str()).collect();
        self.last.retain(|k, _| live.contains(k.as_str()));
        out
    }
}

/// Detection for one engine.
pub struct Detector {
    observe: Box<dyn Observe>,
    machine_id: String,
    notifier: Notifier,
}

impl Detector {
    pub fn new(observe: Box<dyn Observe>, machine_id: String) -> Self {
        Self {
            observe,
            machine_id,
            notifier: Notifier::default(),
        }
    }

    /// Detect once and write the result. Blocking.
    pub fn tick(&mut self, db: &Db) -> Result<Applied, String> {
        let sessions = self.observe.observe()?;
        let notes = self.notifier.step(&sessions, Instant::now());
        let seen: Vec<Observed> = sessions.iter().map(Observed::from_session).collect();
        let machine = self.machine_id.clone();
        let (applied, _) = db.write_blocking(move |tx| {
            let mut applied = apply(tx, &machine, &seen, now_ms())?;
            applied.notified = emit_notifications(tx, &machine, &notes)?;
            Ok(applied)
        })?;
        Ok(applied)
    }
}

/// Emit `notes` for the rows `apply` just wrote. Returns `(topic, id)`.
pub fn emit_notifications(
    tx: &mut Tx,
    machine: &str,
    notes: &[Notification],
) -> rusqlite::Result<Vec<(String, String)>> {
    let mut sent = Vec::new();
    for n in notes {
        // The engine-controlled row first: it holds the session over a
        // detected one.
        let id: Option<String> = tx
            .query_row(
                "SELECT id FROM agents WHERE session_key = ?1 AND gone_at IS NULL \
                 ORDER BY control = 'pty' DESC, id LIMIT 1",
                [&n.key],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = id else { continue };
        tx.emit(&NewEvent {
            topic: n.topic.into(),
            subject: Some(subject(machine, &id)),
            origin: None,
            data: serde_json::json!({
                "id": id,
                "sessionKey": n.key,
                "status": n.status,
                "name": n.name,
                "pendingTool": n.pending_tool,
            }),
        })?;
        sent.push((n.topic.to_string(), id));
    }
    Ok(sent)
}

/// Run detection on its own thread every `TICK` until the returned sender
/// is dropped. A thread, not a blocking task, so a tick in flight (which
/// may wait on `claude`) never holds up the runtime's shutdown.
pub fn spawn(db: Arc<Db>) -> Result<mpsc::Sender<()>, String> {
    let (stop, stopped) = mpsc::channel::<()>();
    let machine = db.machine_id().to_string();
    std::thread::Builder::new()
        .name("engine-detect".into())
        .spawn(move || {
            let mut detector = Detector::new(Box::new(Live::new()), machine);
            loop {
                if let Err(e) = detector.tick(&db) {
                    eprintln!("[engine] detection: {}", e);
                }
                match stopped.recv_timeout(TICK) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    _ => return,
                }
            }
        })
        .map_err(|e| format!("Failed to start the detection thread: {}", e))?;
    Ok(stop)
}

fn subject(machine: &str, id: &str) -> String {
    format!("{}/{}", machine, id)
}

fn read_row(tx: &Tx, id: &str) -> rusqlite::Result<AgentRow> {
    tx.query_row(
        &format!("SELECT {} FROM agents WHERE id = ?1", AGENT_COLUMNS),
        [id],
        agent_row,
    )
}

/// Emit `agent.upsert` for `id` when the row differs from `before` in
/// more than `last_seen`.
fn emit_if_changed(
    tx: &mut Tx,
    machine: &str,
    id: &str,
    before: Option<AgentRow>,
    applied: &mut Applied,
) -> rusqlite::Result<()> {
    let after = read_row(tx, id)?;
    let changed = match before {
        Some(mut b) => {
            b.last_seen = after.last_seen;
            b != after
        }
        None => true,
    };
    if changed {
        tx.emit(&NewEvent {
            topic: "agent.upsert".into(),
            subject: Some(subject(machine, id)),
            origin: None,
            data: serde_json::to_value(&after).expect("AgentRow serializes"),
        })?;
        applied.upserted.push(id.to_string());
    }
    Ok(())
}

fn mark_gone(
    tx: &mut Tx,
    machine: &str,
    id: &str,
    key: &str,
    now: i64,
    applied: &mut Applied,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE agents SET gone_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    tx.emit(&NewEvent {
        topic: "agent.gone".into(),
        subject: Some(subject(machine, id)),
        origin: None,
        data: serde_json::json!({ "id": id, "sessionKey": key, "goneAt": now }),
    })?;
    applied.gone.push(id.to_string());
    Ok(())
}

/// Write one tick's sessions; see the module docs.
pub fn apply(tx: &mut Tx, machine: &str, seen: &[Observed], now: i64) -> rusqlite::Result<Applied> {
    let mut applied = Applied::default();
    let mut keys: HashSet<&str> = HashSet::new();
    // Keys whose detected row stays live.
    let mut detected: HashSet<&str> = HashSet::new();
    for o in seen {
        // A provider listing the same session twice: the first one wins.
        if !keys.insert(o.key.as_str()) {
            continue;
        }
        let pty: Option<String> = tx
            .query_row(
                "SELECT id FROM agents WHERE control = 'pty' AND provider = ?1 \
                 AND provider_sid = ?2 AND gone_at IS NULL ORDER BY id DESC LIMIT 1",
                params![o.provider, o.sid],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = pty {
            // The engine owns the rest of a pty row.
            let before = read_row(tx, &id)?;
            tx.execute(
                "UPDATE agents SET session_key = ?2, status = ?3, last_seen = ?4 WHERE id = ?1",
                params![id, o.key, o.status, now],
            )?;
            emit_if_changed(tx, machine, &id, Some(before), &mut applied)?;
            continue;
        }
        detected.insert(o.key.as_str());
        let before = tx
            .query_row(
                &format!(
                    "SELECT {} FROM agents WHERE source = 'detected' AND session_key = ?1",
                    AGENT_COLUMNS
                ),
                [&o.key],
                agent_row,
            )
            .optional()?;
        let id = match &before {
            Some(row) => {
                tx.execute(
                    "UPDATE agents SET gone_at = NULL, provider = ?2, provider_sid = ?3, \
                     machine_id = ?4, status = ?5, cwd = ?6, name = ?7, pid = ?8, \
                     started_at = ?9, last_seen = ?10 WHERE id = ?1",
                    params![
                        row.id,
                        o.provider,
                        o.sid,
                        machine,
                        o.status,
                        o.cwd,
                        o.name,
                        o.pid,
                        o.started_at,
                        now
                    ],
                )?;
                row.id.clone()
            }
            None => {
                let id = new_agent_id();
                tx.execute(
                    "INSERT INTO agents (id, role, control, provider, provider_sid, source, \
                     session_key, machine_id, status, cwd, name, pid, started_at, created_at, \
                     last_seen) \
                     VALUES (?1, 'observed', 'none', ?2, ?3, 'detected', ?4, ?5, ?6, ?7, ?8, \
                     ?9, ?10, ?11, ?11)",
                    params![
                        id,
                        o.provider,
                        o.sid,
                        o.key,
                        machine,
                        o.status,
                        o.cwd,
                        o.name,
                        o.pid,
                        o.started_at,
                        now
                    ],
                )?;
                id
            }
        };
        emit_if_changed(tx, machine, &id, before, &mut applied)?;
    }

    let live: Vec<(String, String)> = {
        let mut st = tx.prepare(
            "SELECT id, session_key FROM agents \
             WHERE source = 'detected' AND gone_at IS NULL ORDER BY id",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (id, key) in live {
        if !detected.contains(key.as_str()) {
            mark_gone(tx, machine, &id, &key, now, &mut applied)?;
        }
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::db::{self, AgentFilter};
    use crate::session::source::{SessionDetectorError, SessionSource};
    use crate::session::BackendMode;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Instant;

    const M: &str = "m0";

    fn home() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let h = t.path().join("engine");
        (t, h)
    }

    /// Sessions the next tick sees; an `Err` fails the tick.
    #[derive(Clone)]
    struct Script(Arc<Mutex<Result<Vec<Session>, String>>>);

    impl Default for Script {
        fn default() -> Self {
            Script(Arc::new(Mutex::new(Ok(Vec::new()))))
        }
    }

    impl Script {
        fn set(&self, sessions: Vec<Session>) {
            *self.0.lock().unwrap() = Ok(sessions);
        }
        fn fail(&self) {
            *self.0.lock().unwrap() = Err("detect failed".into());
        }
    }

    impl Observe for Script {
        fn observe(&mut self) -> Result<Vec<Session>, String> {
            self.0.lock().unwrap().clone()
        }
    }

    fn detector(script: &Script) -> Detector {
        Detector::new(Box::new(script.clone()), M.into())
    }

    fn s(id: &str, status: SessionStatus) -> Session {
        Session::fixture(id, status)
    }

    fn rows(db: &Db, filter: AgentFilter) -> Vec<AgentRow> {
        db.agents(filter).unwrap()
    }

    fn topics(db: &Db) -> Vec<(String, String)> {
        db.reader()
            .unwrap()
            .prepare("SELECT topic, subject FROM events ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn a_tick_upserts_one_observed_row_per_session_without_duplicates() {
        let (_t, h) = home();
        let db = db::open(&h, M).unwrap();
        let script = Script::default();
        let mut d = detector(&script);
        script.set(vec![
            s("a", SessionStatus::Working),
            s("b", SessionStatus::WaitingForInput),
            // Listed twice by a provider: still one row.
            s("a", SessionStatus::Working),
        ]);
        let first = d.tick(&db).unwrap();
        assert_eq!(first.upserted.len(), 2);
        let r = rows(&db, AgentFilter::All);
        assert_eq!(r.len(), 2);
        let a = r
            .iter()
            .find(|r| r.session_key.as_deref() == Some("claudeCode:a"))
            .unwrap();
        assert!(a.id.starts_with("agt_") && a.id.len() == 30, "{}", a.id);
        assert_eq!(
            (a.role.as_str(), a.control.as_str(), a.source.as_str()),
            ("observed", "none", "detected")
        );
        assert_eq!(a.provider, "claudeCode");
        assert_eq!(a.provider_sid.as_deref(), Some("a"));
        assert_eq!(a.machine_id.as_deref(), Some(M));
        assert_eq!(a.status.as_deref(), Some("working"));
        assert_eq!(a.state, None, "an observed agent has no engine state");
        assert_eq!(a.cwd.as_deref(), Some("/tmp/project"));
        assert_eq!(a.name.as_deref(), Some("name-a"));
        assert_eq!(a.pid, Some(4242));
        assert_eq!(a.started_at, Some(1_752_364_800_000));
        assert!(a.created_at.is_some() && a.last_seen.is_some());
        assert_eq!(topics(&db).len(), 2);

        // The same sessions again: same rows, no event, `last_seen` moves.
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(d.tick(&db).unwrap(), Applied::default());
        let again = rows(&db, AgentFilter::All);
        assert_eq!(again.len(), 2);
        assert_eq!(
            again.iter().map(|r| &r.id).collect::<Vec<_>>(),
            r.iter().map(|r| &r.id).collect::<Vec<_>>()
        );
        assert!(again[0].last_seen > r[0].last_seen);
        assert_eq!(topics(&db).len(), 2);

        // A status change is one upsert of that row.
        script.set(vec![
            s("a", SessionStatus::NeedsAttention),
            s("b", SessionStatus::WaitingForInput),
        ]);
        let changed = d.tick(&db).unwrap();
        assert_eq!(changed.upserted, vec![a.id.clone()]);
        let t = topics(&db);
        let subject = format!("{}/{}", M, a.id);
        assert_eq!(
            t[t.len() - 2..],
            [
                ("agent.upsert".to_string(), subject.clone()),
                // Working to needs-attention also notifies (C1a-16).
                ("notification.needs_attention".to_string(), subject)
            ]
        );
    }

    #[test]
    fn a_gone_session_gets_gone_at_and_comes_back_on_the_same_row() {
        let (_t, h) = home();
        let db = db::open(&h, M).unwrap();
        let script = Script::default();
        let mut d = detector(&script);
        script.set(vec![
            s("a", SessionStatus::Working),
            s("b", SessionStatus::Working),
        ]);
        d.tick(&db).unwrap();
        let b_id = rows(&db, AgentFilter::All)
            .into_iter()
            .find(|r| r.provider_sid.as_deref() == Some("b"))
            .unwrap()
            .id;

        script.set(vec![s("a", SessionStatus::Working)]);
        let tick = d.tick(&db).unwrap();
        assert_eq!(tick.gone, vec![b_id.clone()]);
        let b = rows(&db, AgentFilter::All)
            .into_iter()
            .find(|r| r.id == b_id)
            .unwrap();
        assert!(b.gone_at.is_some());
        assert_eq!(
            topics(&db).last().unwrap(),
            &("agent.gone".to_string(), format!("{}/{}", M, b_id))
        );
        assert_eq!(rows(&db, AgentFilter::Sessions).len(), 1);
        // Gone stays gone: no second event.
        assert_eq!(d.tick(&db).unwrap(), Applied::default());

        script.set(vec![
            s("a", SessionStatus::Working),
            s("b", SessionStatus::Working),
        ]);
        assert_eq!(d.tick(&db).unwrap().upserted, vec![b_id.clone()]);
        let sessions = rows(&db, AgentFilter::Sessions);
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().any(|r| r.id == b_id && r.gone_at.is_none()));
        assert_eq!(rows(&db, AgentFilter::All).len(), 2);
    }

    #[test]
    fn a_failed_detection_writes_nothing() {
        let (_t, h) = home();
        let db = db::open(&h, M).unwrap();
        let script = Script::default();
        let mut d = detector(&script);
        script.set(vec![s("a", SessionStatus::Working)]);
        d.tick(&db).unwrap();
        let before = rows(&db, AgentFilter::All);
        script.fail();
        assert!(d.tick(&db).is_err());
        assert_eq!(rows(&db, AgentFilter::All), before);
        assert_eq!(rows(&db, AgentFilter::Sessions).len(), 1);
    }

    #[test]
    fn an_engine_controlled_row_takes_its_session_by_provider_sid() {
        let (_t, h) = home();
        let db = db::open(&h, M).unwrap();
        let script = Script::default();
        let mut d = detector(&script);
        // Seen before the engine bound it: a detected row.
        script.set(vec![s("p", SessionStatus::Working)]);
        d.tick(&db).unwrap();
        let observed = rows(&db, AgentFilter::All).remove(0);
        db.write_blocking(|tx| {
            tx.execute(
                "INSERT INTO agents (id, generation, mode, role, control, provider, \
                 provider_sid, source, state) \
                 VALUES ('agt_pty', 1, 'free', 'worker', 'pty', 'claudeCode', 'p', 'engine', \
                 'working')",
                [],
            )
        })
        .unwrap();

        let tick = d.tick(&db).unwrap();
        assert_eq!(tick.gone, vec![observed.id.clone()]);
        assert_eq!(tick.upserted, vec!["agt_pty".to_string()]);
        let live = rows(&db, AgentFilter::Sessions);
        assert_eq!(live.len(), 1, "{:?}", live);
        let pty = &live[0];
        assert_eq!(pty.id, "agt_pty");
        assert_eq!(pty.session_key.as_deref(), Some("claudeCode:p"));
        assert_eq!(pty.status.as_deref(), Some("working"));
        assert_eq!(pty.state.as_deref(), Some("working"));
        assert_eq!(pty.source, "engine");

        // Next ticks: still one row, and no new detected row.
        assert_eq!(d.tick(&db).unwrap(), Applied::default());
        assert_eq!(rows(&db, AgentFilter::All).len(), 2);
        assert_eq!(rows(&db, AgentFilter::Sessions).len(), 1);
    }

    #[test]
    fn after_a_new_epoch_the_next_tick_rebuilds_the_observed_rows() {
        let (_t, h) = home();
        let script = Script::default();
        let mut d = detector(&script);
        script.set(vec![
            s("a", SessionStatus::Working),
            s("b", SessionStatus::WaitingForInput),
        ]);
        let db = db::open(&h, M).unwrap();
        let epoch = db.epoch().to_string();
        d.tick(&db).unwrap();
        drop(db);

        // Not a db any more: `open` moves it aside and starts a new epoch.
        std::fs::write(db::db_path(&h), vec![0x5Au8; 8192]).unwrap();
        let db = db::open(&h, M).unwrap();
        assert_ne!(db.epoch(), epoch);
        assert!(rows(&db, AgentFilter::All).is_empty());

        d.tick(&db).unwrap();
        let mut keys: Vec<String> = rows(&db, AgentFilter::Sessions)
            .into_iter()
            .map(|r| r.session_key.unwrap())
            .collect();
        keys.sort();
        assert_eq!(keys, ["claudeCode:a", "claudeCode:b"]);
        assert_eq!(db.head_cursor().seq, 2);
    }

    #[test]
    fn agent_list_filters_hide_gone_retired_and_failed_rows() {
        let (_t, h) = home();
        let db = db::open(&h, M).unwrap();
        db.write_blocking(|tx| {
            for (id, state, gone) in [
                ("agt_1", None, None),
                ("agt_2", None, Some(5)),
                ("agt_3", Some("retired"), None),
                ("agt_4", Some("failed"), None),
                ("agt_5", Some("ready"), None),
            ] {
                tx.execute(
                    "INSERT INTO agents (id, role, provider, source, state, gone_at) \
                     VALUES (?1, 'worker', 'claudeCode', 'engine', ?2, ?3)",
                    params![id, state, gone],
                )?;
            }
            Ok(())
        })
        .unwrap();
        let ids = |f| -> Vec<String> { rows(&db, f).into_iter().map(|r| r.id).collect() };
        assert_eq!(ids(AgentFilter::Live), ["agt_1", "agt_5"]);
        assert_eq!(
            ids(AgentFilter::All),
            ["agt_1", "agt_2", "agt_3", "agt_4", "agt_5"]
        );
        // None of these has a session detection matched.
        assert!(ids(AgentFilter::Sessions).is_empty());
    }

    // ── Notification triggers (C1a-16) ─────────────────────────────────

    fn topics_of(notes: &[Notification]) -> Vec<(&str, &str)> {
        notes.iter().map(|n| (n.topic, n.key.as_str())).collect()
    }

    #[test]
    fn the_notifier_uses_the_gui_transitions_and_cooldown() {
        let mut n = Notifier::default();
        let t0 = Instant::now();
        let at = |secs| t0 + Duration::from_secs(secs);
        let working = vec![
            s("a", SessionStatus::Working),
            s("b", SessionStatus::Working),
        ];
        // The first tick only seeds, even for a session that is waiting.
        assert!(n
            .step(&[s("a", SessionStatus::WaitingForInput)], at(0))
            .is_empty());
        assert!(n.step(&working, at(1)).is_empty());

        let mut asking = s("b", SessionStatus::NeedsAttention);
        asking.pending_tool_name = Some("Bash".into());
        let notes = n.step(
            &[s("a", SessionStatus::WaitingForInput), asking.clone()],
            at(2),
        );
        assert_eq!(
            topics_of(&notes),
            [
                ("notification.waiting_for_input", "claudeCode:a"),
                ("notification.needs_attention", "claudeCode:b")
            ]
        );
        assert_eq!(notes[1].status, "needs_attention");
        assert_eq!(notes[1].name, "name-b");
        assert_eq!(notes[1].pending_tool.as_deref(), Some("Bash"));

        // Flicker inside the cooldown: no repeat.
        assert!(n.step(&working, at(3)).is_empty());
        assert!(n
            .step(&[s("a", SessionStatus::WaitingForInput), asking], at(4))
            .is_empty());
        // After the cooldown the same transition notifies again.
        assert!(n.step(&working, at(40)).is_empty());
        let notes = n.step(
            &[
                s("a", SessionStatus::WaitingForInput),
                s("b", SessionStatus::Working),
            ],
            at(41),
        );
        assert_eq!(
            topics_of(&notes),
            [("notification.waiting_for_input", "claudeCode:a")]
        );
    }

    #[test]
    fn a_gone_session_starts_a_fresh_cooldown() {
        let mut n = Notifier::default();
        let t0 = Instant::now();
        n.step(&[s("a", SessionStatus::Working)], t0);
        assert_eq!(
            n.step(&[s("a", SessionStatus::WaitingForInput)], t0).len(),
            1
        );
        n.step(&[], t0);
        n.step(&[s("a", SessionStatus::Working)], t0);
        assert_eq!(
            n.step(&[s("a", SessionStatus::WaitingForInput)], t0).len(),
            1
        );
    }

    #[test]
    fn a_tick_emits_notifications_with_its_rows() {
        let (_t, h) = home();
        let db = db::open(&h, M).unwrap();
        let script = Script::default();
        let mut d = detector(&script);
        script.set(vec![
            s("a", SessionStatus::Working),
            s("p", SessionStatus::Working),
        ]);
        d.tick(&db).unwrap();
        db.write_blocking(|tx| {
            tx.execute(
                "INSERT INTO agents (id, generation, mode, role, control, provider, \
                 provider_sid, source) \
                 VALUES ('agt_pty', 1, 'free', 'worker', 'pty', 'claudeCode', 'p', 'engine')",
                [],
            )
        })
        .unwrap();
        let a_id = rows(&db, AgentFilter::All)
            .into_iter()
            .find(|r| r.provider_sid.as_deref() == Some("a"))
            .unwrap()
            .id;

        script.set(vec![
            s("a", SessionStatus::WaitingForInput),
            s("p", SessionStatus::NeedsAttention),
        ]);
        let tick = d.tick(&db).unwrap();
        assert_eq!(
            tick.notified,
            [
                ("notification.waiting_for_input".to_string(), a_id.clone()),
                // The engine-controlled row holds session p now.
                ("notification.needs_attention".to_string(), "agt_pty".into())
            ]
        );
        let r = db.reader().unwrap();
        let (subject, data): (String, String) = r
            .query_row(
                "SELECT subject, payload FROM events WHERE topic = 'notification.needs_attention'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(subject, format!("{}/agt_pty", M));
        let data: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(data["sessionKey"], "claudeCode:p");
        assert_eq!(data["status"], "needs_attention");
        assert!(data.get("firstPrompt").is_none());
    }

    /// A backend that always succeeds with no sessions.
    struct Named(&'static str);

    impl SessionSource for Named {
        fn detect(
            &mut self,
        ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
            Ok((Vec::new(), DetectionDiagnostics::default()))
        }
        fn backend_name(&self) -> &'static str {
            self.0
        }
    }

    /// No window in the engine: the clock alone moves Auto mode back from
    /// the legacy backend to the CLI one, on the detection path.
    #[test]
    fn the_legacy_recheck_runs_on_the_clock() {
        let now = Arc::new(Mutex::new(Instant::now()));
        let clock = now.clone();
        let state = DetectorState::for_test(Box::new(Named("legacy")), BackendMode::Auto)
            .with_test_backends(Box::new(move || *clock.lock().unwrap()), Box::new(|| true));
        let mut live = Live::with(state, |_, _| Ok(Vec::new()));
        live.observe().unwrap();
        assert_eq!(live.backend_name(), "legacy");

        *now.lock().unwrap() += Duration::from_secs(5 * 60);
        live.observe().unwrap();
        assert_eq!(live.backend_name(), "cli");
    }
}
