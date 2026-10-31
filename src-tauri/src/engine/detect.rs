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
//!   row emits `agent.upsert`. An unchanged one moves only `last_seen`, and
//!   only once `LAST_SEEN_EVERY` has passed, so an idle machine writes
//!   nothing most ticks and adds no events.
//! - A live `control=pty` row whose `provider_sid` is the session's id
//!   takes the session instead, so an engine-started agent is never listed
//!   twice. A detected row of that session goes. A pty row that no longer
//!   holds a detected session (it is gone, or a newer pty row of the same
//!   id took it) gives back its `session_key` and `status`.
//! - Detected rows whose session is gone get `gone_at` and emit
//!   `agent.gone`. A session that comes back gets its row back.
//!
//! - A session that moved from working to needs-attention or
//!   waiting-for-input emits `notification.needs_attention` or
//!   `notification.waiting_for_input`, by the rule the GUI uses
//!   (`session::transitions`) and with its 30 s cooldown per session. The
//!   GUI keeps firing its own notifications; these events are for engine
//!   clients. Their data carries the text the GUI shows (`title`,
//!   `redactedTitle`, `body`; `session::notification`), `pid` and
//!   `provider`. The engine's cooldown is the only cooldown on them.
//! - The tick adds the workers overlay to its sessions, keeps the
//!   away-activity index current (`session::away::refresh_index`; a
//!   transcript that did not grow costs one metadata read), and emits
//!   `session.view` and `session.gone` for the sessions whose view changed
//!   (`views::sessions`), in the same transaction.
//!
//! A failed detection writes nothing, so one bad pass does not mark every
//! session gone. A pass where only some providers failed (say `claude
//! agents --json` timed out, but Codex answered) keeps the rows of the
//! failed providers as they are, for up to `HOLD_TICKS` failed passes in a
//! row; after that their sessions count as gone. Between ticks a tick
//! keeps only those counts and the notifier: it reads the rows it compares
//! against from the db. After `quick_check` starts a new epoch, the next
//! tick rebuilds the observed rows from provider data (A:497).

use super::db::outbox::NewEvent;
use super::db::{agent_row, new_agent_id, now_ms, Db, Tx, AGENT_COLUMNS};
use super::proto::{AgentRow, TickResult};
use super::views::sessions::SessionViews;
use crate::session::enrichment::enrich_detected_sessions;
use crate::session::transitions::notification_transitions;
use crate::session::{
    DetectedSession, DetectionDiagnostics, DetectorState, Session, SessionProvider, SessionStatus,
};
use rusqlite::{params, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Time between ticks; the GUI polls at the same rate.
pub const TICK: Duration = Duration::from_millis(3500);

/// One session notifies at most once per this long, as in the GUI poll, so
/// a flickering status does not repeat a notification.
pub const NOTIFY_COOLDOWN: Duration = Duration::from_secs(30);

/// A provider that failed this many passes in a row stops holding its
/// rows. It outlasts the switch to the legacy backend after repeated
/// `claude` failures (`DOWNGRADE_THRESHOLD`, 5 passes).
pub const HOLD_TICKS: u32 = 6;

/// An unchanged row's `last_seen` is rewritten at most this often, so an
/// idle tick commits nothing.
pub const LAST_SEEN_EVERY: Duration = Duration::from_secs(60);

/// One detection pass.
#[derive(Debug, Clone, Default)]
pub struct Observation {
    pub sessions: Vec<Session>,
    /// Providers (`SessionProvider::as_key`) that did not answer: the pass
    /// says nothing about their sessions.
    pub failed: Vec<&'static str>,
}

/// Where a tick's sessions come from.
pub trait Observe: Send {
    fn observe(&mut self) -> Result<Observation, String>;

    /// The detected sessions without enrichment, for `session.stop`.
    fn detected(&mut self) -> Result<Vec<DetectedSession>, String> {
        Err("this detector cannot list its sessions".into())
    }

    /// Pick the backend again, as the GUI does on window focus.
    fn recheck(&mut self) {}

    /// `cli` or `legacy`.
    fn backend_name(&self) -> &'static str {
        "none"
    }
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
    fn detected(&mut self) -> Result<Vec<DetectedSession>, String> {
        self.state
            .detect()
            .map(|(detected, _)| detected)
            .map_err(|e| format!("Detect failed: {}", e))
    }

    fn recheck(&mut self) {
        self.state.recheck_and_maybe_swap();
    }

    fn backend_name(&self) -> &'static str {
        self.state.backend_name()
    }

    fn observe(&mut self) -> Result<Observation, String> {
        let (detected, diag) = self
            .state
            .detect()
            .map_err(|e| format!("Detect failed: {}", e))?;
        let failed = self
            .state
            .failed_providers()
            .iter()
            .map(|p| p.as_key())
            .collect();
        Ok(Observation {
            sessions: (self.enrich)(detected, diag)?,
            failed,
        })
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
    s.wire_name()
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

/// A notification a tick emits. `title` and `body` can carry conversation
/// text (the first prompt, a session title); `redacted_title` does not.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub topic: &'static str,
    pub key: String,
    pub status: &'static str,
    pub name: String,
    pub pending_tool: Option<String>,
    pub title: String,
    pub redacted_title: String,
    pub body: String,
    pub pid: u32,
    pub provider: SessionProvider,
}

/// Which sessions notify, tick to tick.
#[derive(Default, Clone)]
pub struct Notifier {
    prev: HashMap<String, SessionStatus>,
    started: bool,
    last: HashMap<String, Instant>,
}

impl Notifier {
    /// This tick's notifications. The first tick only seeds the statuses.
    /// Sessions of the `held` providers (which did not answer) keep their
    /// previous status and cooldown.
    pub fn step(
        &mut self,
        sessions: &[Session],
        held: &HashSet<String>,
        now: Instant,
    ) -> Vec<Notification> {
        let first = !self.started;
        self.started = true;
        let is_held = |key: &str| held.contains(provider_of(key));
        let kept: Vec<(String, SessionStatus)> = self
            .prev
            .iter()
            .filter(|(k, _)| is_held(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut out = Vec::new();
        for s in notification_transitions(&mut self.prev, first, sessions) {
            // Transitions end only in the two statuses that have a text.
            let Some(text) = crate::session::notification::notification_text(s) else {
                continue;
            };
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
                title: text.title,
                redacted_title: text.redacted_title,
                body: text.body,
                pid: s.pid,
                provider: s.provider,
            });
        }
        for (k, v) in kept {
            self.prev.entry(k).or_insert(v);
        }
        let live: HashSet<&str> = sessions.iter().map(|s| s.session_key.as_str()).collect();
        self.last
            .retain(|k, _| live.contains(k.as_str()) || is_held(k));
        out
    }
}

/// The provider part of a `SessionIdentity::key()`.
fn provider_of(key: &str) -> &str {
    key.split_once(':').map_or(key, |(p, _)| p)
}

/// Detection for one engine.
pub struct Detector {
    observe: Box<dyn Observe>,
    machine_id: String,
    notifier: Notifier,
    /// Failed passes in a row, per provider key.
    misses: HashMap<&'static str, u32>,
    views: Arc<SessionViews>,
    /// Keep the away-activity index (a process-wide table) current. Off
    /// by default so unit tests do not touch it.
    away: bool,
    /// Where the workers overlay is read; `None` adds none.
    workers_dir: Option<PathBuf>,
}

impl Detector {
    pub fn new(observe: Box<dyn Observe>, machine_id: String) -> Self {
        Self {
            observe,
            machine_id,
            notifier: Notifier::default(),
            misses: HashMap::new(),
            views: Arc::new(SessionViews::default()),
            away: false,
            workers_dir: None,
        }
    }

    /// What the engine runs: the away index and the workers overlay of
    /// `~/.claude/c9watch/workers`, as the GUI poll keeps them.
    pub fn live(observe: Box<dyn Observe>, machine_id: String) -> Self {
        let mut d = Self::new(observe, machine_id);
        d.away = true;
        d.workers_dir = crate::session::workers::workers_dir();
        d
    }

    pub fn with_away(mut self, on: bool) -> Self {
        self.away = on;
        self
    }

    pub fn with_workers_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.workers_dir = dir;
        self
    }

    pub fn views(&self) -> &Arc<SessionViews> {
        &self.views
    }

    /// Detect once and write the result. Blocking.
    pub fn tick(&mut self, db: &Db) -> Result<Applied, String> {
        let mut obs = self.observe.observe()?;
        if let Some(dir) = &self.workers_dir {
            let overlay = crate::session::workers::load_workers_overlay_at(dir);
            crate::session::workers::apply_workers_overlay_from(&mut obs.sessions, &overlay);
        }
        if self.away {
            crate::session::away::refresh_index(&obs.sessions);
        }
        self.misses.retain(|p, _| obs.failed.contains(p));
        for p in &obs.failed {
            *self.misses.entry(p).or_insert(0) += 1;
        }
        let held: HashSet<String> = self
            .misses
            .iter()
            .filter(|(_, n)| **n < HOLD_TICKS)
            .map(|(p, _)| p.to_string())
            .collect();
        // Step a copy: a tick whose write fails must not use up its
        // transitions, so the next tick finds them again.
        let mut notifier = self.notifier.clone();
        let notes = notifier.step(&obs.sessions, &held, Instant::now());
        let seen: Vec<Observed> = obs.sessions.iter().map(Observed::from_session).collect();
        let change = self.views.diff(&obs.sessions);
        let view_events = change.events(&self.machine_id);
        let machine = self.machine_id.clone();
        let (applied, _) = db.write_blocking(move |tx| {
            let mut applied = apply(tx, &machine, &seen, now_ms(), &held)?;
            applied.notified = emit_notifications(tx, &machine, &notes)?;
            for e in &view_events {
                tx.emit(e)?;
            }
            Ok(applied)
        })?;
        self.notifier = notifier;
        // Only ticks write `session.*` events, one at a time (`Detection`
        // holds its lock across a tick), so the head now has every one of
        // them that these views reflect.
        self.views.commit(change, db.head_cursor().to_string());
        Ok(applied)
    }
}

/// Why `Detection::stop` did not stop a session.
#[derive(Debug, Clone, PartialEq)]
pub enum StopError {
    /// No detected session has the key.
    Unknown(String),
    /// The session cannot be stopped, runs as another pid, or the signal
    /// failed.
    Failed(String),
    /// Detection failed or has not started.
    Internal(String),
}

/// The engine's one detector, shared by its clock and the c9p calls that
/// run a tick (`detect.recheck`, `session.stop`, `session.rename`). One
/// tick runs at a time.
pub struct Detection {
    db: Arc<Db>,
    detector: Mutex<Option<Detector>>,
    pub views: Arc<SessionViews>,
}

impl Detection {
    /// No detector yet: `spawn` installs one on its thread.
    pub fn new(db: Arc<Db>) -> Arc<Self> {
        Arc::new(Detection {
            db,
            detector: Mutex::new(None),
            views: Arc::new(SessionViews::default()),
        })
    }

    /// Use `d`; its ticks commit to these views.
    pub fn install(&self, mut d: Detector) {
        d.views = self.views.clone();
        *self.lock() = Some(d);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Detector>> {
        self.detector.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn with<R>(&self, f: impl FnOnce(&mut Detector) -> Result<R, String>) -> Result<R, String> {
        match self.lock().as_mut() {
            Some(d) => f(d),
            None => Err("detection has not started".into()),
        }
    }

    fn result(&self, d: &Detector) -> TickResult {
        TickResult {
            cursor: self
                .views
                .current()
                .map(|(c, _)| c)
                .unwrap_or_else(|| self.db.head_cursor().to_string()),
            backend: d.observe.backend_name().to_string(),
        }
    }

    /// One tick. Blocking.
    pub fn tick(&self) -> Result<TickResult, String> {
        self.with(|d| {
            d.tick(&self.db)?;
            Ok(self.result(d))
        })
    }

    /// Pick the backend again, then one tick. Blocking.
    pub fn recheck(&self) -> Result<TickResult, String> {
        self.with(|d| {
            d.observe.recheck();
            d.tick(&self.db)?;
            Ok(self.result(d))
        })
    }

    /// SIGTERM the stoppable session `key`, as the GUI's `stop_session`
    /// does; the pid comes from detection, never from the caller. Returns
    /// the pid. Blocking; runs no tick.
    pub fn stop(&self, key: &str, expect_pid: Option<u32>) -> Result<u32, StopError> {
        let detected = self
            .with(|d| d.observe.detected())
            .map_err(StopError::Internal)?;
        if !detected
            .iter()
            .any(|s| s.identity_key().as_deref() == Some(key))
        {
            return Err(StopError::Unknown(format!(
                "No running session matches {key}"
            )));
        }
        crate::actions::stop_session_by_key(
            key,
            expect_pid,
            move || Ok(detected),
            crate::actions::process_start_secs,
        )
        .map_err(StopError::Failed)
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
                "title": n.title,
                "redactedTitle": n.redacted_title,
                "body": n.body,
                "pid": n.pid,
                "provider": n.provider,
            }),
        })?;
        sent.push((n.topic.to_string(), id));
    }
    Ok(sent)
}

/// How long `session.list` and `snapshot.get` wait for detection's first
/// pass after the engine starts. A client that reads them allows more
/// (`cli::engine_sessions`).
pub const FIRST_TICK_WAIT: Duration = Duration::from_secs(15);

/// Open once detection's first pass is written. Until then the agents table
/// holds only what the previous engine left, so `session.list` and
/// `snapshot.get` wait for it (up to `FIRST_TICK_WAIT`): a CLI that starts
/// the engine reads the sessions, not an empty or stale table.
pub struct FirstTick(tokio::sync::watch::Sender<bool>);

impl FirstTick {
    /// Not open: detection has not run yet.
    pub fn pending() -> FirstTick {
        FirstTick(tokio::sync::watch::channel(false).0)
    }

    /// Open: an engine without detection, and tests.
    pub fn open_now() -> FirstTick {
        FirstTick(tokio::sync::watch::channel(true).0)
    }

    pub fn open(&self) {
        self.0.send_replace(true);
    }

    pub fn is_open(&self) -> bool {
        *self.0.borrow()
    }

    /// Wait until open, up to `timeout`. Returns whether it is open.
    pub async fn wait(&self, timeout: Duration) -> bool {
        let mut rx = self.0.subscribe();
        tokio::time::timeout(timeout, rx.wait_for(|open| *open))
            .await
            .is_ok_and(|r| r.is_ok())
    }
}

/// Run detection on its own thread every `TICK` until the returned sender
/// is dropped. A thread, not a blocking task, so a tick in flight (which
/// may wait on `claude`) never holds up the runtime's shutdown. `first` is
/// opened after the first pass, whether it failed or not.
pub fn spawn(detection: Arc<Detection>, first: Arc<FirstTick>) -> Result<mpsc::Sender<()>, String> {
    let (stop, stopped) = mpsc::channel::<()>();
    let machine = detection.db.machine_id().to_string();
    std::thread::Builder::new()
        .name("engine-detect".into())
        .spawn(move || {
            detection.install(Detector::live(Box::new(Live::new()), machine));
            loop {
                if let Err(e) = detection.tick() {
                    eprintln!("[engine] detection: {}", e);
                }
                first.open();
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

/// Write one tick's sessions; see the module docs. The rows of `held`
/// providers (keys of providers that did not answer) stay as they are.
pub fn apply(
    tx: &mut Tx,
    machine: &str,
    seen: &[Observed],
    now: i64,
    held: &HashSet<String>,
) -> rusqlite::Result<Applied> {
    let mut applied = Applied::default();
    let stale = now - LAST_SEEN_EVERY.as_millis() as i64;
    let mut keys: HashSet<&str> = HashSet::new();
    // Keys whose detected row stays live.
    let mut detected: HashSet<&str> = HashSet::new();
    // Key -> the pty row that holds it this tick.
    let mut pty_of: HashMap<&str, String> = HashMap::new();
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
            let same = before.session_key.as_deref() == Some(o.key.as_str())
                && before.status.as_deref() == Some(o.status);
            if !same || before.last_seen.is_none_or(|t| t <= stale) {
                tx.execute(
                    "UPDATE agents SET session_key = ?2, status = ?3, last_seen = ?4 \
                     WHERE id = ?1",
                    params![id, o.key, o.status, now],
                )?;
                emit_if_changed(tx, machine, &id, Some(before), &mut applied)?;
            }
            pty_of.insert(o.key.as_str(), id);
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
            Some(row) if is_current(row, o, machine, stale) => continue,
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

    let live: Vec<(String, String, String)> = {
        let mut st = tx.prepare(
            "SELECT id, session_key, provider FROM agents \
             WHERE source = 'detected' AND gone_at IS NULL ORDER BY id",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (id, key, provider) in live {
        if !detected.contains(key.as_str()) && !held.contains(&provider) {
            mark_gone(tx, machine, &id, &key, now, &mut applied)?;
        }
    }

    // A pty row that no longer holds its session gives the key back, so
    // `session.list` neither keeps a stale session nor lists one twice.
    let ptys: Vec<(String, String, String)> = {
        let mut st = tx.prepare(
            "SELECT id, session_key, provider FROM agents \
             WHERE control = 'pty' AND gone_at IS NULL AND session_key IS NOT NULL ORDER BY id",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (id, key, provider) in ptys {
        let holder = pty_of.get(key.as_str());
        if holder == Some(&id) || (holder.is_none() && held.contains(&provider)) {
            continue;
        }
        let before = read_row(tx, &id)?;
        tx.execute(
            "UPDATE agents SET session_key = NULL, status = NULL WHERE id = ?1",
            [&id],
        )?;
        emit_if_changed(tx, machine, &id, Some(before), &mut applied)?;
    }
    Ok(applied)
}

/// Whether the detected `row` already says what `o` does, with a recent
/// enough `last_seen`: then the tick writes nothing for it.
fn is_current(row: &AgentRow, o: &Observed, machine: &str, stale: i64) -> bool {
    row.gone_at.is_none()
        && row.provider == o.provider
        && row.provider_sid.as_deref() == Some(o.sid.as_str())
        && row.machine_id.as_deref() == Some(machine)
        && row.status.as_deref() == Some(o.status)
        && row.cwd.as_deref() == Some(o.cwd.as_str())
        && row.name.as_deref() == Some(o.name.as_str())
        && row.pid == Some(o.pid)
        && row.started_at == o.started_at
        && row.last_seen.is_some_and(|t| t > stale)
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

    /// What the next tick sees; an `Err` fails the tick.
    #[derive(Clone)]
    struct Script(Arc<Mutex<Result<Observation, String>>>);

    impl Default for Script {
        fn default() -> Self {
            Script(Arc::new(Mutex::new(Ok(Observation::default()))))
        }
    }

    impl Script {
        fn set(&self, sessions: Vec<Session>) {
            self.partial(sessions, &[]);
        }
        /// `sessions`, while the `failed` providers did not answer.
        fn partial(&self, sessions: Vec<Session>, failed: &[&'static str]) {
            *self.0.lock().unwrap() = Ok(Observation {
                sessions,
                failed: failed.to_vec(),
            });
        }
        fn fail(&self) {
            *self.0.lock().unwrap() = Err("detect failed".into());
        }
    }

    impl Observe for Script {
        fn observe(&mut self) -> Result<Observation, String> {
            self.0.lock().unwrap().clone()
        }
    }

    fn none() -> HashSet<String> {
        HashSet::new()
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

    /// The agent and notification events; `session.*` has its own tests.
    fn topics(db: &Db) -> Vec<(String, String)> {
        db.reader()
            .unwrap()
            .prepare(
                "SELECT topic, subject FROM events WHERE topic NOT LIKE 'session.%' ORDER BY seq",
            )
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

        // The same sessions again: same rows, no event, no write.
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(d.tick(&db).unwrap(), Applied::default());
        let again = rows(&db, AgentFilter::All);
        assert_eq!(again, r);
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

    fn codex(id: &str, status: SessionStatus) -> Session {
        let mut x = s(id, status);
        x.provider = crate::session::SessionProvider::Codex;
        x.session_key = format!("codex:{}", id);
        x
    }

    #[test]
    fn a_provider_that_did_not_answer_keeps_its_rows() {
        let (_t, h) = home();
        let db = db::open(&h, M).unwrap();
        let script = Script::default();
        let mut d = detector(&script);
        script.set(vec![
            s("a", SessionStatus::Working),
            s("b", SessionStatus::Working),
            codex("x", SessionStatus::Working),
        ]);
        d.tick(&db).unwrap();
        let before = rows(&db, AgentFilter::All);
        let events = topics(&db).len();

        // `claude agents --json` timed out; Codex answered.
        script.partial(vec![codex("x", SessionStatus::Working)], &["claudeCode"]);
        for _ in 1..HOLD_TICKS {
            assert_eq!(d.tick(&db).unwrap(), Applied::default());
        }
        assert_eq!(rows(&db, AgentFilter::All), before);
        assert_eq!(rows(&db, AgentFilter::Sessions).len(), 3);
        assert_eq!(topics(&db).len(), events);

        // A transition across the blip still notifies.
        script.set(vec![
            s("a", SessionStatus::WaitingForInput),
            s("b", SessionStatus::Working),
            codex("x", SessionStatus::Working),
        ]);
        let tick = d.tick(&db).unwrap();
        assert_eq!(tick.gone, Vec::<String>::new());
        assert_eq!(tick.notified.len(), 1);
        assert_eq!(tick.notified[0].0, "notification.waiting_for_input");

        // A provider that keeps failing stops holding its rows.
        script.partial(vec![codex("x", SessionStatus::Working)], &["claudeCode"]);
        for _ in 1..HOLD_TICKS {
            assert!(d.tick(&db).unwrap().gone.is_empty());
        }
        assert_eq!(d.tick(&db).unwrap().gone.len(), 2);
        assert_eq!(rows(&db, AgentFilter::Sessions).len(), 1);
    }

    #[test]
    fn a_failed_write_keeps_its_notifications_for_the_next_tick() {
        let (_t, h) = home();
        let db = db::open(&h, M).unwrap();
        let script = Script::default();
        let mut d = detector(&script);
        script.set(vec![s("a", SessionStatus::Working)]);
        d.tick(&db).unwrap();
        script.set(vec![s("a", SessionStatus::WaitingForInput)]);
        db.close();
        assert!(d.tick(&db).is_err());
        drop(db);

        let db = db::open(&h, M).unwrap();
        let tick = d.tick(&db).unwrap();
        assert_eq!(tick.notified.len(), 1, "{:?}", tick);
    }

    #[test]
    fn an_idle_tick_writes_nothing_until_last_seen_is_due() {
        let (_t, h) = home();
        let db = db::open(&h, M).unwrap();
        let seen = vec![Observed::from_session(&s("a", SessionStatus::Working))];
        let at = |ms: i64| {
            let seen = seen.clone();
            db.write_blocking(move |tx| apply(tx, M, &seen, ms, &none()))
                .unwrap()
                .0
        };
        assert_eq!(at(1_000).upserted.len(), 1);
        let first = rows(&db, AgentFilter::All);
        assert_eq!(at(5_000), Applied::default());
        assert_eq!(rows(&db, AgentFilter::All), first);

        let due = 1_000 + LAST_SEEN_EVERY.as_millis() as i64;
        assert_eq!(at(due), Applied::default());
        let after = rows(&db, AgentFilter::All);
        assert_eq!(after[0].last_seen, Some(due));
        assert_eq!(topics(&db).len(), 1, "a last_seen refresh emits nothing");
    }

    #[test]
    fn a_pty_row_gives_back_its_session_when_it_goes_or_is_replaced() {
        let (_t, h) = home();
        let db = db::open(&h, M).unwrap();
        let script = Script::default();
        let mut d = detector(&script);
        let pty = |id: &'static str| {
            db.write_blocking(move |tx| {
                tx.execute(
                    "INSERT INTO agents (id, generation, mode, role, control, provider, \
                     provider_sid, source) \
                     VALUES (?1, 1, 'free', 'worker', 'pty', 'claudeCode', 'p', 'engine')",
                    [id],
                )
            })
            .unwrap();
        };
        pty("agt_pty1");
        script.set(vec![s("p", SessionStatus::Working)]);
        d.tick(&db).unwrap();
        let key_of = |id: &str| {
            rows(&db, AgentFilter::All)
                .into_iter()
                .find(|r| r.id == id)
                .unwrap()
                .session_key
        };
        assert_eq!(key_of("agt_pty1").as_deref(), Some("claudeCode:p"));

        // A respawn of the same session: the newer row takes the key.
        pty("agt_pty2");
        let tick = d.tick(&db).unwrap();
        assert_eq!(tick.upserted, vec!["agt_pty2", "agt_pty1"]);
        assert_eq!(key_of("agt_pty1"), None);
        let live = rows(&db, AgentFilter::Sessions);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, "agt_pty2");

        // Its session is gone: no stale session stays listed.
        script.set(vec![]);
        assert_eq!(d.tick(&db).unwrap().upserted, vec!["agt_pty2"]);
        assert!(rows(&db, AgentFilter::Sessions).is_empty());
        assert_eq!(d.tick(&db).unwrap(), Applied::default());
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
            .step(&[s("a", SessionStatus::WaitingForInput)], &none(), at(0))
            .is_empty());
        assert!(n.step(&working, &none(), at(1)).is_empty());

        let mut asking = s("b", SessionStatus::NeedsAttention);
        asking.pending_tool_name = Some("Bash".into());
        let notes = n.step(
            &[s("a", SessionStatus::WaitingForInput), asking.clone()],
            &none(),
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
        assert!(n.step(&working, &none(), at(3)).is_empty());
        assert!(n
            .step(
                &[s("a", SessionStatus::WaitingForInput), asking],
                &none(),
                at(4)
            )
            .is_empty());
        // After the cooldown the same transition notifies again.
        assert!(n.step(&working, &none(), at(40)).is_empty());
        let notes = n.step(
            &[
                s("a", SessionStatus::WaitingForInput),
                s("b", SessionStatus::Working),
            ],
            &none(),
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
        n.step(&[s("a", SessionStatus::Working)], &none(), t0);
        assert_eq!(
            n.step(&[s("a", SessionStatus::WaitingForInput)], &none(), t0)
                .len(),
            1
        );
        n.step(&[], &none(), t0);
        n.step(&[s("a", SessionStatus::Working)], &none(), t0);
        assert_eq!(
            n.step(&[s("a", SessionStatus::WaitingForInput)], &none(), t0)
                .len(),
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

    /// `(topic, data)` of every `session.*` event, in order.
    fn session_events(db: &Db) -> Vec<(String, serde_json::Value)> {
        db.reader()
            .unwrap()
            .prepare("SELECT topic, payload FROM events WHERE topic LIKE 'session.%' ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .map(|r| {
                let (t, p) = r.unwrap();
                (t, serde_json::from_str(&p).unwrap())
            })
            .collect()
    }

    /// The notification data carries the GUI's text (the code
    /// `polling::fire_notification` runs), the pid and the provider.
    #[test]
    fn notification_data_carries_the_gui_payload() {
        let (_t, h) = home();
        let db = db::open(&h, M).unwrap();
        let script = Script::default();
        let mut d = detector(&script);
        script.set(vec![
            s("a", SessionStatus::Working),
            s("q", SessionStatus::Working),
        ]);
        d.tick(&db).unwrap();
        let mut a = s("a", SessionStatus::NeedsAttention);
        a.pending_tool_name = Some("Bash".into());
        a.ai_title = Some("Fix the build".into());
        let q = s("q", SessionStatus::WaitingForInput);
        script.set(vec![a.clone(), q.clone()]);
        d.tick(&db).unwrap();
        let r = db.reader().unwrap();
        for (topic, session) in [
            ("notification.needs_attention", &a),
            ("notification.waiting_for_input", &q),
        ] {
            let data: String = r
                .query_row(
                    "SELECT payload FROM events WHERE topic = ?1",
                    [topic],
                    |row| row.get(0),
                )
                .unwrap();
            let data: serde_json::Value = serde_json::from_str(&data).unwrap();
            let want = crate::session::notification::notification_text(session).unwrap();
            assert_eq!(data["title"], want.title.as_str(), "{}", topic);
            assert_eq!(data["redactedTitle"], want.redacted_title.as_str());
            assert_eq!(data["body"], want.body.as_str());
            assert_eq!(data["pid"], session.pid);
            assert_eq!(data["provider"], "claudeCode");
            assert_eq!(data["sessionKey"], session.session_key.as_str());
        }
    }

    /// A tick emits `session.view` for new and changed sessions and
    /// `session.gone` for ended ones, and nothing when nothing changed; the
    /// views are the GUI's JSON, with the workers overlay.
    #[test]
    fn a_tick_emits_session_views_only_for_changes() {
        let (_t, h) = home();
        let db = db::open(&h, M).unwrap();
        let workers = h.parent().unwrap().join("workers");
        std::fs::create_dir_all(workers.join("w1")).unwrap();
        std::fs::write(
            workers.join("w1").join("meta.json"),
            serde_json::json!({"sessionId": "b", "spawnedBy": "pm-9"}).to_string(),
        )
        .unwrap();
        let script = Script::default();
        let mut d = detector(&script).with_workers_dir(Some(workers));
        script.set(vec![
            s("a", SessionStatus::Working),
            s("b", SessionStatus::Working),
        ]);
        d.tick(&db).unwrap();
        let ev = session_events(&db);
        assert_eq!(ev.len(), 2);
        assert!(ev.iter().all(|(t, _)| t == "session.view"));
        let mut b = s("b", SessionStatus::Working);
        b.worker_of = Some("pm-9".into());
        assert_eq!(ev[1].1, serde_json::to_value(&b).unwrap());
        let (cursor, views) = d.views().current().unwrap();
        assert_eq!(cursor, db.head_cursor().to_string());
        assert_eq!(views.len(), 2);
        assert_eq!(views[1].worker_of.as_deref(), Some("pm-9"));

        // Unchanged: no event.
        d.tick(&db).unwrap();
        assert_eq!(session_events(&db).len(), 2);

        // `a` changes, `b` ends.
        script.set(vec![s("a", SessionStatus::WaitingForInput)]);
        d.tick(&db).unwrap();
        let ev = session_events(&db);
        assert_eq!(ev.len(), 4);
        assert_eq!(ev[2].0, "session.view");
        assert_eq!(ev[2].1["status"], "WaitingForInput");
        assert_eq!(ev[3].0, "session.gone");
        assert_eq!(
            ev[3].1,
            serde_json::json!({"sessionKey": "claudeCode:b", "id": "b"})
        );
        assert_eq!(d.views().current().unwrap().1.len(), 1);
    }

    /// A tick whose write fails keeps the old views, so the next tick
    /// emits the change again.
    #[test]
    fn a_failed_detection_keeps_the_views() {
        let (_t, h) = home();
        let db = db::open(&h, M).unwrap();
        let script = Script::default();
        let mut d = detector(&script);
        script.set(vec![s("a", SessionStatus::Working)]);
        d.tick(&db).unwrap();
        script.fail();
        assert!(d.tick(&db).is_err());
        assert_eq!(d.views().current().unwrap().1.len(), 1);
        assert_eq!(session_events(&db).len(), 1);
    }

    /// Away parity: the tick keeps the away index current as a transcript
    /// grows, and `session.away`'s reader answers what the GUI's
    /// `get_away_activity` (the same `away::away_activity`) does.
    #[test]
    fn the_tick_keeps_the_away_index_current_as_a_transcript_grows() {
        use crate::session::away::{away_activity, AwayRequest};
        use std::io::Write;
        let (t, h) = home();
        let db = db::open(&h, M).unwrap();
        let path = t.path().join("away.jsonl");
        let tool = |ts: &str, id: &str, name: &str| {
            serde_json::json!({
                "type": "assistant", "timestamp": ts, "uuid": id,
                "message": {"role": "assistant", "content": [
                    {"type": "tool_use", "id": id, "name": name, "input": {"command": "ls"}}
                ]}
            })
            .to_string()
                + "\n"
        };
        std::fs::write(&path, tool("2026-07-13T00:00:01Z", "tu-away-1", "Bash")).unwrap();
        // A key no other test uses: the index is process-wide.
        let mut one = s("away-parity-7c1f", SessionStatus::Working);
        one.transcript_path = Some(path.clone());
        let script = Script::default();
        let mut d = detector(&script).with_away(true);
        script.set(vec![one.clone()]);
        let req = vec![AwayRequest {
            session_key: one.session_key.clone(),
            since_ms: 0,
        }];
        let away = |d: &mut Detector| {
            d.tick(&db).unwrap();
            away_activity(&req)
                .get(&one.session_key)
                .cloned()
                .expect("an index for the session")
        };
        let first = away(&mut d);
        assert_eq!((first.tool_calls, first.commands_run), (1, 1));
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(tool("2026-07-13T00:00:02Z", "tu-away-2", "Edit").as_bytes())
            .unwrap();
        drop(f);
        let grown = away(&mut d);
        assert_eq!(grown.tool_calls, 2, "{:?}", grown);
        // An unchanged transcript keeps the counts.
        assert_eq!(away(&mut d), grown);
        // A session that ends drops its index.
        script.set(vec![]);
        d.tick(&db).unwrap();
        assert!(away_activity(&req).is_empty());
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
