//! Free-mode `agent.create` and `agent.retire` (plan C1a-26, A:794-828).
//!
//! `agent.create` is off unless the engine started with
//! `C9WATCH_EXPERIMENTAL_CREATE=1` (`experimental_disabled`; C1a-35 drops
//! the switch). Only `local-unverified` may call it, or `agent.retire`
//! (§1.1, v3.1: any L may retire). There is no cap on live agents: the
//! legacy daemon's `DEFAULT_MAX_WORKERS=16` does not carry over.
//!
//! A create:
//! 1. Checks the engine's `c9watch-agent` copy, realpaths the cwd, and
//!    refuses a cwd inside the engine home or a managed worktree home
//!    (`.c9watch/worktrees`) with `cwd_collision` (D22.9), and a cwd that
//!    holds the engine home.
//! 2. One transaction: `cwd_collision` when a live engine agent's cwd is
//!    above or below this one, then the `agents` row (`creating`, `created_by` = the
//!    caller's principal), its first generation with the `--session-id`,
//!    and the create op in phase `row` (`engine::ops`).
//! 3. A supervisor thread writes the per-spawn settings, spawns the agent
//!    once through the `Launcher`, registers the PTY child in the live
//!    table and records phase `pty`. It then follows the screen
//!    (`engine::cc_screen`) until the agent is `ready`, and records `bound`.
//! 4. `agent.create` answers when the agent is `ready`, or with the error.
//!
//! **Vendor refusal (D28 Q7).** The agent exits before `ready`, the screen
//! shows a usage-limit or login text before `ready`, or it is not `ready`
//! within `cc_screen::READY_TIMEOUT` (30 s): the row ends `failed` with
//! `spawn_failed`, the detail carries the vendor's text, and `agent.create`
//! returns `spawn_failed` with it. The engine never spawns again for that
//! create, and checks no quota first.
//!
//! A retire kills the PTY (outside any transaction) and then, in one
//! transaction, makes the row a tombstone: `retired`, `gone_at` set, the
//! row kept (A:828). An agent that exits by itself after `ready` ends
//! `failed` with `exited`.
//!
//! The supervisor owns the `Driver`. On engine shutdown it kills the PTY
//! and leaves the row as it is; the next boot reconciles it (`ops`).

use super::adapter::cc::profile::{
    resolve_cwd, settings_path_of, spawn_spec, write_settings, Profile,
};
use super::adapter::{self, Descriptor};
use super::agent_bin::{AgentBin, Rehash};
use super::agents::LiveTable;
use super::cc_screen::{vendor_refusal, CcScreen, ScreenEvent, ScreenFailure, READY_TIMEOUT};
use super::db::audit::{self, AuditEntry};
use super::db::{agent_row, new_agent_id, now_ms, Db, Tx, AGENT_COLUMNS};
use super::driver::{Driver, SpawnSpec};
use super::generation::{begin_generation, emit_upsert, GenReason};
use super::merge::MergeTable;
use super::ops::{self, CreatePayload, Phase};
use super::principal::Principal;
use super::proto::{AgentCreateParams, AgentRetireParams, AgentRow, ApiError, ErrorCode};
use super::pty::NativePty;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// The engine's env switch for `agent.create` (until C1a-35).
pub const EXPERIMENTAL_CREATE_ENV: &str = "C9WATCH_EXPERIMENTAL_CREATE";

/// Why a create failed: the vendor refused, the agent exited or it was
/// not ready in time.
pub const SPAWN_FAILED: &str = "spawn_failed";

/// Why a ready agent's row failed: its process exited by itself.
pub const EXITED: &str = "exited";

/// How long the supervisor waits for output before it looks again.
const POLL: Duration = Duration::from_millis(100);

/// How often a ready agent's supervisor checks for commands and an exit.
const RUN_TICK: Duration = Duration::from_millis(250);

/// Longest failure detail kept, in chars.
const MAX_DETAIL: usize = 2000;

/// Screen lines kept in a failure detail.
const DETAIL_LINES: usize = 20;

/// Longest agent name.
const MAX_NAME: usize = 64;

/// The supervisor's clock. Tests pass one that runs fast.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// Starts an agent process. The engine's is `NativeLauncher`; tests pass a
/// fake driver.
pub trait Launcher: Send + Sync {
    fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn Driver>>;
}

/// The native PTY (`engine::pty`).
pub struct NativeLauncher;

impl Launcher for NativeLauncher {
    fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn Driver>> {
        Ok(Box::new(NativePty::spawn(spec)?))
    }
}

/// What a `Spawner` runs with.
#[derive(Debug, Clone)]
pub struct SpawnerConfig {
    /// The engine home: `spawn/` for the settings files, and the
    /// `cwd_collision` check.
    pub home: PathBuf,
    /// The engine socket, absolute (`C9_ENGINE_SOCK`).
    pub sock: PathBuf,
    /// The `claude` binary. `None` looks `claude` up on the engine's
    /// `PATH` at each create.
    pub claude: Option<PathBuf>,
    /// `C9WATCH_EXPERIMENTAL_CREATE=1` at engine start.
    pub enabled: bool,
}

type Reply = oneshot::Sender<Result<AgentRow, ApiError>>;

enum Cmd {
    /// Kill the agent and make its row a tombstone.
    Retire { principal: String, reply: Reply },
    /// The engine stops: kill the agent, leave the row to boot reconcile.
    Shutdown,
}

struct Handle {
    cmds: Sender<Cmd>,
    thread: JoinHandle<()>,
}

/// One create, as the supervisor gets it.
#[derive(Debug, Clone)]
struct Start {
    id: String,
    gen: i64,
    sid: String,
    op_id: String,
    cwd: PathBuf,
    claude: PathBuf,
}

/// How bringing an agent up ended.
enum Up {
    Ready(Box<dyn Driver>, AgentRow),
    Failed(ApiError),
    Retired,
    Shutdown,
}

/// Creates, supervises and retires the engine's free agents.
pub struct Spawner {
    config: SpawnerConfig,
    db: Arc<Db>,
    agents: Arc<LiveTable>,
    merge: Arc<MergeTable>,
    agent_bin: Arc<AgentBin>,
    launcher: Arc<dyn Launcher>,
    clock: Clock,
    /// SessionStart hooks taken for `(agent, generation)` that the
    /// supervisor has not seen yet.
    starts: Mutex<HashSet<(String, i64)>>,
    /// The supervisor of every agent that runs in this engine.
    live: Mutex<HashMap<String, Handle>>,
    stopping: AtomicBool,
}

impl Spawner {
    pub fn new(
        config: SpawnerConfig,
        db: Arc<Db>,
        agents: Arc<LiveTable>,
        merge: Arc<MergeTable>,
        agent_bin: Arc<AgentBin>,
    ) -> Spawner {
        Spawner {
            config,
            db,
            agents,
            merge,
            agent_bin,
            launcher: Arc::new(NativeLauncher),
            clock: Arc::new(Instant::now),
            starts: Mutex::new(HashSet::new()),
            live: Mutex::new(HashMap::new()),
            stopping: AtomicBool::new(false),
        }
    }

    pub fn with_launcher(mut self, launcher: Arc<dyn Launcher>) -> Spawner {
        self.launcher = launcher;
        self
    }

    pub fn with_clock(mut self, clock: Clock) -> Spawner {
        self.clock = clock;
        self
    }

    /// Whether `agent.create` is switched on.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// The engine took a SessionStart hook of `agent_id`'s generation
    /// `gen`: one input of `ready` (D24.4).
    pub fn session_started(&self, agent_id: &str, gen: i64) {
        lock(&self.starts).insert((agent_id.to_string(), gen));
    }

    fn take_start(&self, agent_id: &str, gen: i64) -> bool {
        lock(&self.starts).remove(&(agent_id.to_string(), gen))
    }

    /// `agent.create` (see the module doc). Answers when the agent is
    /// `ready` or has failed.
    pub async fn create(
        self: &Arc<Self>,
        principal: &Principal,
        params: serde_json::Value,
    ) -> Result<AgentRow, ApiError> {
        if !self.config.enabled {
            return Err(ApiError::new(
                ErrorCode::ExperimentalDisabled,
                format!(
                    "agent.create is off: the engine started without {}=1",
                    EXPERIMENTAL_CREATE_ENV
                ),
            ));
        }
        local_only(principal, "agent.create")?;
        let p: AgentCreateParams = parse(params, "agent.create")?;
        let desc = check_kind_and_mode(&p)?;
        if let Some(name) = &p.name {
            check_name(name)?;
        }
        if self.stopping.load(Ordering::SeqCst) {
            return Err(stopping());
        }
        let (cwd, claude) = {
            let sp = self.clone();
            let cwd = p.cwd.clone();
            blocking(move || sp.prepare(&cwd)).await?
        };
        let start = Start {
            id: new_agent_id(),
            gen: 0,
            sid: uuid::Uuid::new_v4().to_string(),
            op_id: ops::new_op_id(),
            cwd,
            claude,
        };
        let gen = self
            .write_row(&start, desc, principal, p.name.clone())
            .await?;
        let start = Start { gen, ..start };
        let (reply, answer) = oneshot::channel();
        self.supervise(start, reply)?;
        answer.await.unwrap_or_else(|_| {
            Err(ApiError::new(
                ErrorCode::Internal,
                "the agent's supervisor stopped without an answer",
            ))
        })
    }

    /// Check the agent binary, the cwd and `claude`. Blocking.
    fn prepare(&self, cwd: &str) -> Result<(PathBuf, PathBuf), ApiError> {
        self.agent_bin.check(Rehash::Always)?;
        let given = Path::new(cwd);
        if !given.is_absolute() {
            return Err(bad(format!("cwd {:?} is not absolute", cwd)));
        }
        let cwd = resolve_cwd(given).map_err(bad)?;
        check_collision(&self.config.home, &cwd)?;
        let claude = match &self.config.claude {
            Some(c) => c.clone(),
            None => find_on_path("claude").ok_or_else(|| {
                ApiError::new(
                    ErrorCode::SpawnFailed,
                    "claude is not on the engine's PATH; nothing was spawned",
                )
            })?,
        };
        Ok((cwd, claude))
    }

    /// Phase `row`, in one transaction. Returns the new generation.
    async fn write_row(
        &self,
        s: &Start,
        desc: &'static Descriptor,
        principal: &Principal,
        name: Option<String>,
    ) -> Result<i64, ApiError> {
        let s = s.clone();
        let machine = self.db.machine_id().to_string();
        let principal = principal.to_string();
        let (r, _) = self
            .db
            .write(move |tx| {
                let now = now_ms();
                if let Some(n) = &name {
                    if name_taken(tx, n)? {
                        return Ok(Err(bad(format!("name {:?} is in use by a live agent", n))));
                    }
                }
                if let Some(other) = nested_cwd(tx, &s.cwd)? {
                    return Ok(Err(ApiError::new(
                        ErrorCode::CwdCollision,
                        format!(
                            "cwd {:?} and the cwd {:?} of live agent {} are nested",
                            s.cwd, other.1, other.0
                        ),
                    )));
                }
                let cwd = s.cwd.display().to_string();
                tx.execute(
                    "INSERT INTO agents (id, mode, role, control, provider, source, created_by, \
                     state, cwd, name, machine_id, created_at) \
                     VALUES (?1, 'free', 'worker', 'pty', ?2, 'engine', ?3, 'creating', ?4, ?5, \
                     ?6, ?7)",
                    params![s.id, desc.provider, principal, cwd, name, machine, now],
                )?;
                let gen =
                    begin_generation(tx, &machine, &s.id, GenReason::Spawn, &s.sid, None, now)?;
                let payload = CreatePayload {
                    agent_id: s.id.clone(),
                    generation: gen,
                    session_id: s.sid.clone(),
                    cwd: cwd.clone(),
                    ..Default::default()
                };
                ops::open_create(tx, &s.op_id, &payload, now)?;
                audit::append(
                    tx,
                    now,
                    &AuditEntry {
                        principal: &principal,
                        method: "agent.create",
                        target: Some(&s.id),
                        decision: "allow",
                        reason: Some(&cwd),
                        ..Default::default()
                    },
                )?;
                Ok(Ok(gen))
            })
            .await
            .map_err(internal)?;
        r
    }

    /// Start the supervisor thread of `s`.
    fn supervise(self: &Arc<Self>, s: Start, reply: Reply) -> Result<(), ApiError> {
        let (cmds, rx) = mpsc::channel();
        let mut live = lock(&self.live);
        if self.stopping.load(Ordering::SeqCst) {
            drop(live);
            self.fail_create(&s, "the engine stopped before the spawn");
            return Err(stopping());
        }
        let sp = self.clone();
        let id = s.id.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("agent-{}", id))
            .spawn({
                let s = s.clone();
                move || sp.supervisor(s, reply, rx)
            });
        match spawned {
            Ok(thread) => {
                live.insert(id, Handle { cmds, thread });
                Ok(())
            }
            Err(e) => {
                drop(live);
                let why = format!("Failed to start the agent's supervisor: {}", e);
                self.fail_create(&s, &why);
                Err(ApiError::new(ErrorCode::Internal, why))
            }
        }
    }

    /// The supervisor thread: bring the agent up, answer the create, then
    /// run it until it is retired, exits or the engine stops.
    fn supervisor(self: Arc<Self>, s: Start, reply: Reply, cmds: Receiver<Cmd>) {
        match self.bring_up(&s, &cmds) {
            Up::Ready(driver, row) => {
                let _ = reply.send(Ok(row));
                self.run(&s, driver, &cmds);
            }
            Up::Failed(e) => {
                let _ = reply.send(Err(e));
            }
            Up::Retired => {
                let _ = reply.send(Err(ApiError::new(
                    ErrorCode::SpawnFailed,
                    format!("{} was retired before it was ready", s.id),
                )));
            }
            Up::Shutdown => {
                let _ = reply.send(Err(stopping()));
            }
        }
        self.agents.unregister(&s.id, s.gen);
        self.merge.remove(&s.id);
        lock(&self.starts).retain(|(id, _)| *id != s.id);
        lock(&self.live).remove(&s.id);
    }

    /// Spawn once and follow the screen until `ready` or a failure.
    fn bring_up(&self, s: &Start, cmds: &Receiver<Cmd>) -> Up {
        let fail = |why: String| {
            self.fail_create(s, &why);
            Up::Failed(ApiError::new(
                ErrorCode::SpawnFailed,
                format!("{}: {}", s.id, why),
            ))
        };
        let profile = match Profile::new(
            self.agent_bin.path(),
            &self.config.sock,
            &s.cwd,
            &s.id,
            s.gen,
            &s.sid,
        ) {
            Ok(p) => p,
            Err(e) => return fail(e),
        };
        // The realpath again: a path part swapped for a symlink since the
        // collision check must not move the agent (reviews L1).
        if profile.cwd != s.cwd {
            return fail(format!("cwd {:?} now resolves to {:?}", s.cwd, profile.cwd));
        }
        let settings = match write_settings(&self.config.home, &profile) {
            Ok(f) => f,
            Err(e) => return fail(e),
        };
        let spec = spawn_spec(&profile, &s.claude, &settings);
        // A retire that found no supervisor yet (between the row and
        // `supervise`) closed the op: spawn nothing.
        match self.op_open(s) {
            Ok(true) => {}
            Ok(false) => {
                remove_settings(&self.config.home, &s.id, s.gen);
                return Up::Retired;
            }
            Err(e) => return fail(e),
        }
        // The one spawn of this create; nothing below spawns again.
        let mut driver = match self.launcher.spawn(&spec) {
            Ok(d) => d,
            Err(e) => return fail(format!("could not start {:?}: {}", s.claude, e)),
        };
        let child = driver.child();
        if let Err(e) = self.agents.register(&s.id, s.gen, child) {
            let _ = driver.kill();
            return fail(e);
        }
        match self.write_pty(s, child) {
            Ok(true) => {}
            Ok(false) => return retired_meanwhile(driver),
            Err(e) => {
                let _ = driver.kill();
                return fail(e);
            }
        }
        let mut screen = CcScreen::new((self.clock)());
        loop {
            match cmds.try_recv() {
                Ok(Cmd::Retire { principal, reply }) => {
                    let _ = driver.kill();
                    let _ = reply.send(self.retire_row(&s.id, s.gen, &principal));
                    return Up::Retired;
                }
                Ok(Cmd::Shutdown) | Err(TryRecvError::Disconnected) => {
                    let _ = driver.kill();
                    return Up::Shutdown;
                }
                Err(TryRecvError::Empty) => {}
            }
            let out = driver.output(POLL);
            let snap = driver.screen();
            if self.take_start(&s.id, s.gen) {
                screen.session_start();
            }
            if let Some(text) = vendor_refusal(&snap.text) {
                let _ = driver.kill();
                return fail(format!("the vendor refused before ready: {}", text));
            }
            let mut ready = false;
            for ev in screen.step(&snap.title, &snap.text, snap.quiet, (self.clock)()) {
                match ev {
                    ScreenEvent::Write(keys) => {
                        let _ = driver.write(keys);
                    }
                    ScreenEvent::Ready { first: true } => ready = true,
                    ScreenEvent::Failed(why) => {
                        let _ = driver.kill();
                        return fail(format!("{}: {}", failure_text(why), detail(&snap.text)));
                    }
                    _ => {}
                }
            }
            if ready {
                return match self.write_bound(s) {
                    Ok(Some(row)) => Up::Ready(driver, row),
                    Ok(None) => retired_meanwhile(driver),
                    Err(e) => {
                        let _ = driver.kill();
                        fail(e)
                    }
                };
            }
            let status = match driver.try_wait() {
                Ok(Some(st)) => Some(st.to_string()),
                _ if out.eof => Some("its terminal closed".to_string()),
                _ => None,
            };
            if let Some(status) = status {
                // The last output is in the screen by now.
                let text = driver.screen().text;
                let _ = driver.kill();
                return fail(format!(
                    "exited before ready ({}): {}",
                    status,
                    detail(&text)
                ));
            }
        }
    }

    /// A ready agent: wait for a command or its exit.
    fn run(&self, s: &Start, mut driver: Box<dyn Driver>, cmds: &Receiver<Cmd>) {
        loop {
            match cmds.recv_timeout(RUN_TICK) {
                Ok(Cmd::Retire { principal, reply }) => {
                    let _ = driver.kill();
                    let _ = reply.send(self.retire_row(&s.id, s.gen, &principal));
                    return;
                }
                Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                    let _ = driver.kill();
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            // Nobody reads the bytes yet; the screen keeps up regardless.
            let _ = driver.output(Duration::ZERO);
            if let Ok(Some(status)) = driver.try_wait() {
                let text = driver.screen().text;
                let _ = driver.kill();
                let why = format!("exited ({}): {}", status, detail(&text));
                let (machine, id) = (self.db.machine_id().to_string(), s.id.clone());
                let wrote = self.db.write_blocking(move |tx| {
                    ops::fail_agent(tx, &machine, &id, EXITED, &why, now_ms())
                });
                if let Err(e) = wrote {
                    eprintln!("[engine] Failed to record the exit of {}: {}", s.id, e);
                }
                remove_settings(&self.config.home, &s.id, s.gen);
                return;
            }
        }
    }

    /// Whether the create op of `s` is still open: a retire closes it.
    fn op_open(&self, s: &Start) -> Result<bool, String> {
        let conn = self.db.reader()?;
        conn.query_row(
            "SELECT 1 FROM ops WHERE id = ?1 AND done_at IS NULL",
            [&s.op_id],
            |_| Ok(()),
        )
        .optional()
        .map(|r| r.is_some())
        .map_err(|e| format!("Failed to read the create op: {}", e))
    }

    /// Phase `pty`: the PTY child in the op and in `agent_state`, and the
    /// row `starting`. `false` when a retire closed the op first.
    fn write_pty(&self, s: &Start, child: super::driver::ChildId) -> Result<bool, String> {
        let (s, machine) = (s.clone(), self.db.machine_id().to_string());
        self.db
            .write_blocking(move |tx| write_pty(tx, &machine, &s, child))
            .map(|(open, _)| open)
    }

    /// Phase `bound`: the op is done and the row `ready`. `None` when a
    /// retire closed the op first.
    fn write_bound(&self, s: &Start) -> Result<Option<AgentRow>, String> {
        let (s, machine) = (s.clone(), self.db.machine_id().to_string());
        self.db
            .write_blocking(move |tx| write_bound(tx, &machine, &s))
            .map(|(row, _)| row)
    }

    /// The create failed: close the op, fail the row. A failed write only
    /// logs; boot reconcile finishes it.
    fn fail_create(&self, s: &Start, why: &str) {
        let (machine, s2, why2) = (self.db.machine_id().to_string(), s.clone(), why.to_string());
        let wrote = self.db.write_blocking(move |tx| {
            let now = now_ms();
            ops::finish(tx, &s2.op_id, SPAWN_FAILED, now)?;
            ops::fail_agent(tx, &machine, &s2.id, SPAWN_FAILED, &why2, now)
        });
        if let Err(e) = wrote {
            eprintln!(
                "[engine] Failed to record the failed create of {}: {}",
                s.id, e
            );
        }
        remove_settings(&self.config.home, &s.id, s.gen);
    }

    /// The retire transaction. Blocking.
    fn retire_row(&self, id: &str, gen: i64, principal: &str) -> Result<AgentRow, ApiError> {
        let (machine, id2, principal) = (
            self.db.machine_id().to_string(),
            id.to_string(),
            principal.to_string(),
        );
        let row = self
            .db
            .write_blocking(move |tx| retire_tx(tx, &machine, &id2, &principal))
            .map_err(internal)?
            .0;
        remove_settings(&self.config.home, id, gen);
        row.ok_or_else(|| unknown_agent(id))
    }

    /// `agent.retire`: kill the agent if it runs here, then the tombstone.
    /// A retired or failed agent is returned as it is.
    pub async fn retire(
        self: &Arc<Self>,
        principal: &Principal,
        params: serde_json::Value,
    ) -> Result<AgentRow, ApiError> {
        local_only(principal, "agent.retire")?;
        let p: AgentRetireParams = parse(params, "agent.retire")?;
        let row = {
            let (db, id) = (self.db.clone(), p.id.clone());
            blocking(move || {
                let conn = db.reader().map_err(internal)?;
                engine_row(&conn, &id).map_err(internal)
            })
            .await?
        };
        let row = row.ok_or_else(|| unknown_agent(&p.id))?;
        if matches!(row.state.as_deref(), Some("retired" | "failed")) {
            return Ok(row);
        }
        if let Some(r) = self.retire_live(&p.id, principal).await {
            return r;
        }
        // No supervisor: the agent runs nowhere, or its create is between
        // the row and `supervise`. The tombstone closes the create op, so
        // that supervisor spawns nothing or kills what it spawned; one
        // that registered meanwhile is told to kill it here.
        let sp = self.clone();
        let (id, principal_s) = (p.id.clone(), principal.to_string());
        let r = blocking(move || sp.retire_row(&id, row.generation, &principal_s)).await;
        if r.is_ok() {
            self.retire_live(&p.id, principal).await;
        }
        r
    }

    /// Hand the retire of `id` to its supervisor, if it runs here. `None`
    /// when there is no supervisor, or it ended before it answered.
    async fn retire_live(
        &self,
        id: &str,
        principal: &Principal,
    ) -> Option<Result<AgentRow, ApiError>> {
        let cmds = lock(&self.live).get(id).map(|h| h.cmds.clone())?;
        let (reply, answer) = oneshot::channel();
        cmds.send(Cmd::Retire {
            principal: principal.to_string(),
            reply,
        })
        .ok()?;
        answer.await.ok()
    }

    /// Kill every agent this engine runs and wait for the supervisors. The
    /// rows stay; boot reconcile handles them. Blocking.
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let handles: Vec<Handle> = lock(&self.live).drain().map(|(_, h)| h).collect();
        for h in &handles {
            let _ = h.cmds.send(Cmd::Shutdown);
        }
        for h in handles {
            let _ = h.thread.join();
        }
    }

    /// How many agents run in this engine.
    pub fn live_count(&self) -> usize {
        lock(&self.live).len()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A retire made the row a tombstone while the agent came up: the PTY goes.
fn retired_meanwhile(mut driver: Box<dyn Driver>) -> Up {
    let _ = driver.kill();
    Up::Retired
}

/// Phase `pty` (see `Spawner::write_pty`). Writes nothing and returns
/// `false` when the op is closed.
fn write_pty(
    tx: &mut Tx,
    machine: &str,
    s: &Start,
    child: super::driver::ChildId,
) -> rusqlite::Result<bool> {
    let payload = CreatePayload {
        agent_id: s.id.clone(),
        generation: s.gen,
        session_id: s.sid.clone(),
        cwd: s.cwd.display().to_string(),
        pid: Some(child.pid as i64),
        start: Some(child.start),
        outcome: None,
    };
    if !ops::set_phase(tx, &s.op_id, Phase::Pty, &payload)? {
        return Ok(false);
    }
    tx.execute(
        "INSERT OR REPLACE INTO agent_state (agent_id, gen, pty_child_pid, pty_child_start) \
         VALUES (?1, ?2, ?3, ?4)",
        params![s.id, s.gen, child.pid as i64, child.start as i64],
    )?;
    tx.execute(
        "UPDATE agents SET state = 'starting', pid = ?2 WHERE id = ?1 AND state = 'creating'",
        params![s.id, child.pid as i64],
    )?;
    emit_upsert(tx, machine, &s.id)?;
    Ok(true)
}

/// Phase `bound` (see `Spawner::write_bound`). Writes nothing and returns
/// `None` when the op is closed.
fn write_bound(tx: &mut Tx, machine: &str, s: &Start) -> rusqlite::Result<Option<AgentRow>> {
    let payload: Option<String> = tx
        .query_row("SELECT payload FROM ops WHERE id = ?1", [&s.op_id], |r| {
            r.get(0)
        })
        .optional()?
        .flatten();
    let payload: CreatePayload = payload
        .and_then(|p| serde_json::from_str(&p).ok())
        .unwrap_or_default();
    if !ops::set_phase(tx, &s.op_id, Phase::Bound, &payload)? {
        return Ok(None);
    }
    ops::finish(tx, &s.op_id, Phase::Bound.as_str(), now_ms())?;
    tx.execute(
        "UPDATE agents SET state = 'ready' WHERE id = ?1 AND state = 'starting'",
        [&s.id],
    )?;
    emit_upsert(tx, machine, &s.id)?;
    tx.query_row(
        &format!("SELECT {} FROM agents WHERE id = ?1", AGENT_COLUMNS),
        [&s.id],
        agent_row,
    )
    .map(Some)
}

/// The retire transaction: the tombstone (`retired`, `gone_at`), no PTY
/// child, a create op still open closed as `retired`, the audit row and
/// the event. `None` when there is no such engine agent.
fn retire_tx(
    tx: &mut Tx,
    machine: &str,
    id: &str,
    principal: &str,
) -> rusqlite::Result<Option<AgentRow>> {
    let now = now_ms();
    let n = tx.execute(
        "UPDATE agents SET state = 'retired', gone_at = ?2 WHERE id = ?1 \
         AND source = 'engine' AND state NOT IN ('retired', 'failed')",
        params![id, now],
    )?;
    if n > 0 {
        tx.execute("DELETE FROM agent_state WHERE agent_id = ?1", [id])?;
        let open: Vec<String> = {
            let mut st = tx.prepare(
                "SELECT id FROM ops WHERE kind = ?1 AND done_at IS NULL \
                 AND json_extract(payload, '$.agentId') = ?2",
            )?;
            let rows = st.query_map(params![ops::KIND_CREATE, id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for op in open {
            ops::finish(tx, &op, "retired", now)?;
        }
        audit::append(
            tx,
            now,
            &AuditEntry {
                principal,
                method: "agent.retire",
                target: Some(id),
                decision: "allow",
                ..Default::default()
            },
        )?;
        emit_upsert(tx, machine, id)?;
    }
    engine_row(tx, id)
}

/// The engine agent `id`, any state.
fn engine_row(conn: &Connection, id: &str) -> rusqlite::Result<Option<AgentRow>> {
    conn.query_row(
        &format!(
            "SELECT {} FROM agents WHERE id = ?1 AND source = 'engine'",
            AGENT_COLUMNS
        ),
        [id],
        agent_row,
    )
    .optional()
}

/// A live engine agent whose cwd is strictly above or below `cwd`, as
/// `(id, cwd)` (reviews M4). The outer agent's sandbox may write the inner
/// one's `.claude/`, which CC hot-reloads. The same cwd is fine: both deny
/// it. In the create transaction, so two creates cannot race past it.
fn nested_cwd(conn: &Connection, cwd: &Path) -> rusqlite::Result<Option<(String, PathBuf)>> {
    let mut st = conn.prepare(
        "SELECT id, cwd FROM agents WHERE source = 'engine' AND gone_at IS NULL \
         AND cwd IS NOT NULL",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            PathBuf::from(r.get::<_, String>(1)?),
        ))
    })?;
    for row in rows {
        let (id, other) = row?;
        if other != cwd && (other.starts_with(cwd) || cwd.starts_with(&other)) {
            return Ok(Some((id, other)));
        }
    }
    Ok(None)
}

fn name_taken(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM agents WHERE name = ?1 AND source = 'engine' AND gone_at IS NULL \
             LIMIT 1",
            [name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn remove_settings(home: &Path, id: &str, gen: i64) {
    let _ = std::fs::remove_file(settings_path_of(home, id, gen));
}

/// C1a: only a local client creates or retires (§1.1).
fn local_only(principal: &Principal, method: &str) -> Result<(), ApiError> {
    match principal {
        Principal::LocalUnverified => Ok(()),
        other => Err(ApiError::new(
            ErrorCode::NotAllowed,
            format!("{} may not call {} in C1a", other, method),
        )),
    }
}

fn parse<T: serde::de::DeserializeOwned>(
    params: serde_json::Value,
    method: &str,
) -> Result<T, ApiError> {
    serde_json::from_value(params).map_err(|e| bad(format!("bad {} params: {}", method, e)))
}

/// `kind` has an adapter; `mode` is `free`.
fn check_kind_and_mode(p: &AgentCreateParams) -> Result<&'static Descriptor, ApiError> {
    match p.mode.as_str() {
        "free" => {}
        "managed" => {
            return Err(ApiError::new(
                ErrorCode::UnsupportedInC1a,
                "managed agents come in C1b; use mode \"free\"",
            ))
        }
        other => return Err(bad(format!("unknown mode {:?}", other))),
    }
    adapter::descriptor(&p.kind).ok_or_else(|| bad(format!("unknown agent kind {:?}", p.kind)))
}

fn check_name(name: &str) -> Result<(), ApiError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(bad(format!(
            "name {:?} must be 1-{} letters, digits, '.', '_' or '-'",
            name, MAX_NAME
        )))
    }
}

/// `cwd_collision` (D22.9): `cwd` (a realpath) is inside the engine home,
/// or inside a managed worktree home `<repo>/.c9watch/worktrees`. Also
/// when the engine home is inside `cwd` (`$HOME`, `~/Library`): the
/// agent's sandbox could then write `engine.db` (reviews M2).
pub fn check_collision(home: &Path, cwd: &Path) -> Result<(), ApiError> {
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    if cwd.starts_with(&home) {
        return Err(ApiError::new(
            ErrorCode::CwdCollision,
            format!("cwd {:?} is inside the engine home {:?}", cwd, home),
        ));
    }
    if home.starts_with(cwd) {
        return Err(ApiError::new(
            ErrorCode::CwdCollision,
            format!("the engine home {:?} is inside cwd {:?}", home, cwd),
        ));
    }
    let parts: Vec<_> = cwd.components().collect();
    let in_worktrees = parts
        .windows(2)
        .any(|w| w[0].as_os_str() == ".c9watch" && w[1].as_os_str() == "worktrees");
    if in_worktrees {
        return Err(ApiError::new(
            ErrorCode::CwdCollision,
            format!("cwd {:?} is inside a managed worktree home", cwd),
        ));
    }
    Ok(())
}

/// The first executable `name` on the engine's `PATH`.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(name))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

fn failure_text(why: ScreenFailure) -> String {
    match why {
        ScreenFailure::ReadyTimeout => format!("not ready within {} s", READY_TIMEOUT.as_secs()),
        ScreenFailure::TrustSelectFailed => "the trust dialog did not select Yes".into(),
    }
}

/// The failure detail of a screen: the vendor's refusal lines when there
/// are any, else the last lines of the screen. At most `MAX_DETAIL` chars.
fn detail(text: &str) -> String {
    let d = vendor_refusal(text).unwrap_or_else(|| {
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.trim().is_empty())
            .collect();
        let from = lines.len().saturating_sub(DETAIL_LINES);
        lines[from..].join("\n")
    });
    let d = if d.is_empty() {
        "the screen was empty".to_string()
    } else {
        d
    };
    let n = d.chars().count();
    if n > MAX_DETAIL {
        d.chars().skip(n - MAX_DETAIL).collect()
    } else {
        d
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::BadRequest, msg)
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, e.to_string())
}

fn stopping() -> ApiError {
    ApiError::new(ErrorCode::Internal, "the engine is stopping")
}

fn unknown_agent(id: &str) -> ApiError {
    ApiError::new(ErrorCode::UnknownAgent, format!("no engine agent {:?}", id))
}

async fn blocking<R, F>(f: F) -> Result<R, ApiError>
where
    F: FnOnce() -> Result<R, ApiError> + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(internal)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::adapter::CLAUDE;
    use crate::engine::db::{self, AgentFilter};
    use crate::engine::driver::{ChildId, KillReport, Output, ScreenSnapshot};
    use crate::engine::home::sock_path;
    use crate::engine::peer;
    use std::ffi::OsString;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::sync::atomic::{AtomicI32, AtomicU64, AtomicUsize};
    use std::sync::{OnceLock, Weak};

    const MACHINE: &str = "m_test";

    /// What a fake agent shows.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Script {
        /// The idle title and a prompt; the SessionStart hook "arrives" at
        /// the spawn. Runs until killed.
        Ready,
        /// A usage-limit screen, and the process has exited.
        Refuse,
        /// An error line, and the process has exited.
        Exit,
        /// Nothing at all, forever.
        Hang,
    }

    struct FakeDriver {
        script: Script,
        child: ChildId,
        exited: bool,
        kills: Arc<AtomicUsize>,
    }

    impl Driver for FakeDriver {
        fn spawn(_: &SpawnSpec) -> io::Result<Self> {
            Err(io::Error::other("use FakeLauncher"))
        }
        fn child(&self) -> ChildId {
            self.child
        }
        fn write(&mut self, _: &[u8]) -> io::Result<()> {
            Ok(())
        }
        fn output(&mut self, _: Duration) -> Output {
            Output {
                eof: self.exited,
                ..Default::default()
            }
        }
        fn screen(&self) -> ScreenSnapshot {
            let (title, text) = match self.script {
                Script::Ready => ("\u{2733} Claude Code", "\u{276f} \n  auto mode on"),
                Script::Refuse => (
                    "",
                    " \u{23bf}  Claude usage limit reached. Your limit resets 5pm\n",
                ),
                Script::Exit => ("", "Error: the session could not start\n"),
                Script::Hang => ("", ""),
            };
            ScreenSnapshot {
                text: text.into(),
                title: title.into(),
                title_seq: 1,
                cursor: (0, 0),
                alt_screen: false,
                quiet: Duration::from_secs(1),
            }
        }
        fn resize(&mut self, _: u16, _: u16) -> io::Result<()> {
            Ok(())
        }
        fn signal(&mut self, _: libc::c_int) -> io::Result<()> {
            Ok(())
        }
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            Ok(self.exited.then(|| ExitStatus::from_raw(1 << 8)))
        }
        fn kill(&mut self) -> io::Result<KillReport> {
            if !self.exited {
                self.kills.fetch_add(1, Ordering::SeqCst);
            }
            self.exited = true;
            Ok(KillReport {
                term_was_enough: true,
                exit: None,
            })
        }
    }

    struct FakeLauncher {
        script: Script,
        spawns: AtomicUsize,
        next_pid: AtomicI32,
        kills: Arc<AtomicUsize>,
        env: Mutex<Vec<(OsString, OsString)>>,
        spawner: OnceLock<Weak<Spawner>>,
    }

    fn env_of(spec: &SpawnSpec, key: &str) -> Option<String> {
        spec.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.to_string_lossy().into_owned())
    }

    impl Launcher for FakeLauncher {
        fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn Driver>> {
            self.spawns.fetch_add(1, Ordering::SeqCst);
            *lock(&self.env) = spec.env.clone();
            if self.script == Script::Ready {
                // The SessionStart hook, as `c9watch-agent` would send it.
                let id = env_of(spec, "C9_AGENT_ID").unwrap();
                let gen = env_of(spec, "C9_AGENT_GEN").unwrap().parse().unwrap();
                if let Some(sp) = self.spawner.get().and_then(Weak::upgrade) {
                    sp.session_started(&id, gen);
                }
            }
            Ok(Box::new(FakeDriver {
                script: self.script,
                child: ChildId {
                    pid: self.next_pid.fetch_sub(1, Ordering::SeqCst),
                    start: 1,
                },
                exited: matches!(self.script, Script::Refuse | Script::Exit),
                kills: self.kills.clone(),
            }))
        }
    }

    struct Env {
        t: tempfile::TempDir,
        home: PathBuf,
        cwd: PathBuf,
        db: Arc<Db>,
        agents: Arc<LiveTable>,
        launcher: Arc<FakeLauncher>,
        sp: Arc<Spawner>,
    }

    fn setup(script: Script, enabled: bool, clock: Option<Clock>) -> Env {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("engine home");
        let db = Arc::new(db::open(&home, MACHINE).unwrap());
        let src = t.path().join("agent-src");
        std::fs::write(&src, b"#!/bin/sh\nexit 0\n").unwrap();
        let bin = Arc::new(AgentBin::install(&home, &src));
        let cwd = t.path().join("work");
        std::fs::create_dir(&cwd).unwrap();
        let agents = Arc::new(LiveTable::new());
        let launcher = Arc::new(FakeLauncher {
            script,
            spawns: AtomicUsize::new(0),
            next_pid: AtomicI32::new(2_000_000_000),
            kills: Arc::new(AtomicUsize::new(0)),
            env: Mutex::new(Vec::new()),
            spawner: OnceLock::new(),
        });
        let mut sp = Spawner::new(
            SpawnerConfig {
                home: home.clone(),
                sock: sock_path(&home),
                claude: Some(PathBuf::from("/bin/false")),
                enabled,
            },
            db.clone(),
            agents.clone(),
            Arc::new(MergeTable::new()),
            bin,
        )
        .with_launcher(launcher.clone());
        if let Some(c) = clock {
            sp = sp.with_clock(c);
        }
        let sp = Arc::new(sp);
        let _ = launcher.spawner.set(Arc::downgrade(&sp));
        Env {
            t,
            home,
            cwd,
            db,
            agents,
            launcher,
            sp,
        }
    }

    fn create_params(cwd: &Path) -> serde_json::Value {
        serde_json::json!({ "mode": "free", "cwd": cwd })
    }

    fn row(db: &Db, id: &str) -> AgentRow {
        engine_row(&db.reader().unwrap(), id).unwrap().unwrap()
    }

    fn only_row(db: &Db) -> AgentRow {
        let rows = db.agents(AgentFilter::All).unwrap();
        assert_eq!(rows.len(), 1, "{:?}", rows);
        rows.into_iter().next().unwrap()
    }

    /// (phase, done, outcome) of every op.
    fn ops_of(db: &Db) -> Vec<(String, bool, Option<String>)> {
        let conn = db.reader().unwrap();
        let mut st = conn
            .prepare("SELECT phase, done_at IS NOT NULL, payload FROM ops ORDER BY created_at")
            .unwrap();
        let rows = st
            .query_map([], |r| {
                let p: String = r.get(2)?;
                let p: CreatePayload = serde_json::from_str(&p).unwrap();
                Ok((r.get(0)?, r.get(1)?, p.outcome))
            })
            .unwrap();
        rows.map(Result::unwrap).collect()
    }

    fn agent_state_rows(db: &Db) -> i64 {
        db.reader()
            .unwrap()
            .query_row("SELECT count(*) FROM agent_state", [], |r| r.get(0))
            .unwrap()
    }

    /// Data of the events on `topic`.
    fn events(db: &Db, topic: &str) -> Vec<serde_json::Value> {
        let conn = db.reader().unwrap();
        let mut st = conn
            .prepare("SELECT payload FROM events WHERE topic = ?1 ORDER BY seq")
            .unwrap();
        let rows = st.query_map([topic], |r| r.get::<_, String>(0)).unwrap();
        rows.map(|r| serde_json::from_str(&r.unwrap()).unwrap())
            .collect()
    }

    /// Wait until the supervisors have all ended.
    fn no_supervisors(sp: &Spawner) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while sp.live_count() > 0 {
            assert!(Instant::now() < deadline, "a supervisor is still running");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[tokio::test]
    async fn switch_off_refuses_create() {
        let e = setup(Script::Ready, false, None);
        let err =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap_err();
        assert_eq!(err.code, ErrorCode::ExperimentalDisabled);
        assert!(err.msg.contains(EXPERIMENTAL_CREATE_ENV), "{}", err.msg);
        assert_eq!(e.launcher.spawns.load(Ordering::SeqCst), 0);
        assert!(e.db.agents(AgentFilter::All).unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_agent_may_not_create_or_retire() {
        let e = setup(Script::Ready, true, None);
        let a = Principal::Agent {
            id: "agt_x".into(),
            gen: 1,
        };
        let err = e.sp.create(&a, create_params(&e.cwd)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::NotAllowed);
        let err =
            e.sp.retire(&a, serde_json::json!({"id": "agt_x"}))
                .await
                .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotAllowed);
        assert_eq!(e.launcher.spawns.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn bad_params_mode_kind_and_cwd_collisions_spawn_nothing() {
        let e = setup(Script::Ready, true, None);
        let l = Principal::LocalUnverified;
        let code = |r: Result<AgentRow, ApiError>| r.unwrap_err().code;
        let inside = e.home.join("sub");
        std::fs::create_dir(&inside).unwrap();
        let wt = e.t.path().join("repo/.c9watch/worktrees/w-1");
        std::fs::create_dir_all(&wt).unwrap();
        // A symlink to the engine home resolves into it.
        let link = e.t.path().join("link");
        std::os::unix::fs::symlink(&e.home, &link).unwrap();
        // The engine home is inside it.
        let above = e.t.path().to_path_buf();
        // A glob character in the cwd.
        let glob = e.t.path().join("proj[v2]");
        std::fs::create_dir(&glob).unwrap();
        for (params, want) in [
            (create_params(&inside), ErrorCode::CwdCollision),
            (create_params(&above), ErrorCode::CwdCollision),
            (create_params(&glob), ErrorCode::BadRequest),
            (create_params(&wt), ErrorCode::CwdCollision),
            (create_params(&link), ErrorCode::CwdCollision),
            (
                serde_json::json!({"mode": "free", "cwd": "rel/dir"}),
                ErrorCode::BadRequest,
            ),
            (
                serde_json::json!({"mode": "free", "cwd": "/no/such/dir"}),
                ErrorCode::BadRequest,
            ),
            (
                serde_json::json!({"mode": "managed", "cwd": e.cwd}),
                ErrorCode::UnsupportedInC1a,
            ),
            (
                serde_json::json!({"mode": "free", "kind": "codex", "cwd": e.cwd}),
                ErrorCode::BadRequest,
            ),
            (
                serde_json::json!({"mode": "free", "cwd": e.cwd, "name": "a b"}),
                ErrorCode::BadRequest,
            ),
            (serde_json::json!({"cwd": e.cwd}), ErrorCode::BadRequest),
        ] {
            assert_eq!(
                code(e.sp.create(&l, params.clone()).await),
                want,
                "{}",
                params
            );
        }
        assert_eq!(e.launcher.spawns.load(Ordering::SeqCst), 0);
        assert!(e.db.agents(AgentFilter::All).unwrap().is_empty());
    }

    /// D28 Q7: a usage-limit screen before ready, then the process exits.
    #[tokio::test]
    async fn a_vendor_refusal_is_spawn_failed_with_one_spawn() {
        let e = setup(Script::Refuse, true, None);
        let err =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap_err();
        assert_eq!(err.code, ErrorCode::SpawnFailed);
        assert!(
            err.msg.contains("Claude usage limit reached"),
            "{}",
            err.msg
        );
        assert!(!err.code.retryable());
        // Never retried.
        no_supervisors(&e.sp);
        assert_eq!(e.launcher.spawns.load(Ordering::SeqCst), 1);
        let r = only_row(&e.db);
        assert_eq!(r.state.as_deref(), Some("failed"));
        assert!(r.gone_at.is_some());
        assert_eq!(
            ops_of(&e.db),
            [("pty".to_string(), true, Some(SPAWN_FAILED.to_string()))]
        );
        assert_eq!(agent_state_rows(&e.db), 0);
        let failed = events(&e.db, "agent.failed");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0]["reason"], SPAWN_FAILED);
        assert!(failed[0]["detail"]
            .as_str()
            .unwrap()
            .contains("Claude usage limit reached"));
        assert!(e.agents.is_empty());
        assert!(!settings_path_of(&e.home, &r.id, 1).exists());
    }

    #[tokio::test]
    async fn an_exit_before_ready_is_spawn_failed_with_the_screen() {
        let e = setup(Script::Exit, true, None);
        let err =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap_err();
        assert_eq!(err.code, ErrorCode::SpawnFailed);
        assert!(err.msg.contains("exited before ready"), "{}", err.msg);
        assert!(
            err.msg.contains("Error: the session could not start"),
            "{}",
            err.msg
        );
        no_supervisors(&e.sp);
        assert_eq!(e.launcher.spawns.load(Ordering::SeqCst), 1);
        assert_eq!(only_row(&e.db).state.as_deref(), Some("failed"));
    }

    #[tokio::test]
    async fn no_ready_in_30_s_is_spawn_failed_with_one_spawn() {
        // A clock that moves 1 s per look.
        let base = Instant::now();
        let n = Arc::new(AtomicU64::new(0));
        let clock: Clock =
            Arc::new(move || base + Duration::from_secs(n.fetch_add(1, Ordering::SeqCst)));
        let e = setup(Script::Hang, true, Some(clock));
        let err =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap_err();
        assert_eq!(err.code, ErrorCode::SpawnFailed);
        assert!(err.msg.contains("not ready within 30 s"), "{}", err.msg);
        no_supervisors(&e.sp);
        assert_eq!(e.launcher.spawns.load(Ordering::SeqCst), 1);
        assert_eq!(e.launcher.kills.load(Ordering::SeqCst), 1);
        assert_eq!(only_row(&e.db).state.as_deref(), Some("failed"));
    }

    #[tokio::test]
    async fn create_walks_the_phases_and_retire_keeps_a_tombstone() {
        let e = setup(Script::Ready, true, None);
        let l = Principal::LocalUnverified;
        let mut params = create_params(&e.cwd);
        params["name"] = "w-1".into();
        let r = e.sp.create(&l, params.clone()).await.unwrap();
        assert_eq!(r.state.as_deref(), Some("ready"));
        assert_eq!(r.generation, 1);
        assert_eq!(r.mode.as_deref(), Some("free"));
        assert_eq!(r.control, "pty");
        assert_eq!(r.source, "engine");
        assert_eq!(r.provider, CLAUDE.provider);
        assert_eq!(r.created_by.as_deref(), Some("local-unverified"));
        assert_eq!(r.name.as_deref(), Some("w-1"));
        let real = e.cwd.canonicalize().unwrap();
        assert_eq!(r.cwd.as_deref(), Some(real.to_str().unwrap()));
        assert!(uuid::Uuid::parse_str(r.provider_sid.as_deref().unwrap()).is_ok());
        assert_eq!(
            ops_of(&e.db),
            [("bound".to_string(), true, Some("bound".to_string()))]
        );
        assert_eq!(agent_state_rows(&e.db), 1);
        assert_eq!(e.agents.current(&r.id).unwrap().gen, 1);
        assert!(settings_path_of(&e.home, &r.id, 1).exists());
        // The env names the agent; no secret.
        let env = lock(&e.launcher.env).clone();
        let keys: Vec<String> = env
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert!(keys.iter().any(|k| k == "C9_AGENT_ID"));
        assert!(!keys.iter().any(|k| k.contains("TOKEN")));
        // The name is taken while the agent lives.
        let err = e.sp.create(&l, params.clone()).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert_eq!(e.launcher.spawns.load(Ordering::SeqCst), 1);

        let id = serde_json::json!({ "id": r.id });
        let t = e.sp.retire(&l, id.clone()).await.unwrap();
        assert_eq!(t.state.as_deref(), Some("retired"));
        assert!(t.gone_at.is_some());
        assert_eq!(e.launcher.kills.load(Ordering::SeqCst), 1);
        no_supervisors(&e.sp);
        assert!(e.agents.is_empty());
        assert_eq!(agent_state_rows(&e.db), 0);
        assert!(!settings_path_of(&e.home, &r.id, 1).exists());
        // A tombstone: kept, but not live.
        assert!(e.db.agents(AgentFilter::Live).unwrap().is_empty());
        assert_eq!(row(&e.db, &r.id), t);
        // Retiring again changes nothing.
        assert_eq!(e.sp.retire(&l, id).await.unwrap(), t);
        let err =
            e.sp.retire(&l, serde_json::json!({"id": "agt_none"}))
                .await
                .unwrap_err();
        assert_eq!(err.code, ErrorCode::UnknownAgent);
        // The name is free again.
        let r2 = e.sp.create(&l, params).await.unwrap();
        assert_eq!(r2.state.as_deref(), Some("ready"));
        e.sp.shutdown();
    }

    #[tokio::test]
    async fn shutdown_kills_the_agents_and_boot_marks_them_interrupted() {
        let e = setup(Script::Ready, true, None);
        let r =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap();
        e.sp.shutdown();
        assert_eq!(e.launcher.kills.load(Ordering::SeqCst), 1);
        assert_eq!(e.sp.live_count(), 0);
        let err =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        // The row stays as it was until the next boot.
        assert_eq!(row(&e.db, &r.id).state.as_deref(), Some("ready"));
        let done = ops::reconcile(&e.db, &e.home).unwrap();
        assert_eq!(done.interrupted, [r.id.clone()]);
        let r = row(&e.db, &r.id);
        assert_eq!(r.state.as_deref(), Some("interrupted"));
        assert!(r.interrupted_note_pending);
        assert_eq!(agent_state_rows(&e.db), 0);
    }

    /// A real PTY child that stands for the agent of a crashed engine.
    fn orphan() -> NativePty {
        let spec = SpawnSpec::new(
            vec!["/bin/sh".into(), "-c".into(), "echo up; sleep 30".into()],
            std::env::temp_dir(),
        );
        let mut pty = NativePty::spawn(&spec).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = String::new();
        while !seen.contains("up") {
            seen.push_str(&String::from_utf8_lossy(&pty.output(POLL).bytes));
            assert!(Instant::now() < deadline, "no output; got {:?}", seen);
        }
        pty
    }

    fn runs(pid: libc::pid_t) -> bool {
        peer::proc_entry(pid).is_some_and(|e| !e.zombie)
    }

    /// Reviews M4: a live agent's cwd may not hold, or sit inside, a new
    /// agent's cwd. The same cwd is fine, and a retired agent's no longer
    /// counts.
    #[tokio::test]
    async fn nested_cwds_of_live_agents_collide() {
        let e = setup(Script::Ready, true, None);
        let l = Principal::LocalUnverified;
        let (a, b) = (e.cwd.join("a"), e.cwd.join("a").join("b"));
        std::fs::create_dir_all(&b).unwrap();
        let first = e.sp.create(&l, create_params(&a)).await.unwrap();
        for dir in [&b, &e.cwd] {
            let err = e.sp.create(&l, create_params(dir)).await.unwrap_err();
            assert_eq!(err.code, ErrorCode::CwdCollision, "{:?}", dir);
            assert!(err.msg.contains(&first.id), "{}", err.msg);
        }
        let second = e.sp.create(&l, create_params(&a)).await.unwrap();
        assert_eq!(e.launcher.spawns.load(Ordering::SeqCst), 2);
        for r in [&first, &second] {
            e.sp.retire(&l, serde_json::json!({ "id": r.id }))
                .await
                .unwrap();
        }
        e.sp.create(&l, create_params(&b)).await.unwrap();
        e.sp.shutdown();
    }

    /// A new create's row, written as `create` writes it, with no
    /// supervisor yet.
    async fn row_only(e: &Env) -> Start {
        let s = Start {
            id: new_agent_id(),
            gen: 0,
            sid: uuid::Uuid::new_v4().to_string(),
            op_id: ops::new_op_id(),
            cwd: e.cwd.canonicalize().unwrap(),
            claude: PathBuf::from("/bin/false"),
        };
        let gen =
            e.sp.write_row(&s, &CLAUDE, &Principal::LocalUnverified, None)
                .await
                .unwrap();
        Start { gen, ..s }
    }

    /// Reviews M1: a retire between the row and `supervise` finds no
    /// supervisor. The tombstone closes the op, and the supervisor that
    /// starts after it spawns nothing.
    #[tokio::test]
    async fn a_retire_before_the_supervisor_spawns_nothing() {
        let e = setup(Script::Ready, true, None);
        let l = Principal::LocalUnverified;
        let s = row_only(&e).await;
        let r =
            e.sp.retire(&l, serde_json::json!({ "id": s.id }))
                .await
                .unwrap();
        assert_eq!(r.state.as_deref(), Some("retired"));
        let (reply, answer) = oneshot::channel();
        e.sp.supervise(s.clone(), reply).unwrap();
        let err = answer.await.unwrap().unwrap_err();
        assert_eq!(err.code, ErrorCode::SpawnFailed);
        assert!(
            err.msg.contains("retired before it was ready"),
            "{}",
            err.msg
        );
        no_supervisors(&e.sp);
        assert_eq!(e.launcher.spawns.load(Ordering::SeqCst), 0);
        assert_eq!(row(&e.db, &s.id).state.as_deref(), Some("retired"));
        assert_eq!(agent_state_rows(&e.db), 0);
        assert!(!settings_path_of(&e.home, &s.id, s.gen).exists());
    }

    /// Reviews M1: a retire that lands after the spawn but before phase
    /// `pty` or `bound` makes those writes no-ops, so the supervisor kills
    /// the PTY instead of answering with a retired row.
    #[tokio::test]
    async fn phase_writes_after_a_retire_write_nothing() {
        let e = setup(Script::Ready, true, None);
        let s = row_only(&e).await;
        e.sp.retire_row(&s.id, s.gen, "local-unverified").unwrap();
        let child = ChildId { pid: 7, start: 1 };
        assert_eq!(e.sp.write_pty(&s, child), Ok(false));
        assert_eq!(agent_state_rows(&e.db), 0);
        let s2 = row_only(&e).await;
        assert_eq!(e.sp.write_pty(&s2, child), Ok(true));
        e.sp.retire_row(&s2.id, s2.gen, "local-unverified").unwrap();
        assert_eq!(e.sp.write_bound(&s2), Ok(None));
        for id in [&s.id, &s2.id] {
            assert_eq!(row(&e.db, id).state.as_deref(), Some("retired"));
        }
        assert!(ops_of(&e.db)
            .iter()
            .all(|(_, done, out)| *done && out.as_deref() == Some("retired")));
    }

    /// The engine "crashes" after `phase`: nothing cleans up, the db is
    /// closed and opened again, and boot reconcile runs. `child` is the
    /// PTY child the journal records (it may differ from the real one).
    async fn crash_after(
        phase: Phase,
        child: impl Fn(ChildId) -> ChildId,
    ) -> (Env, ops::Reconciled, AgentRow, Option<NativePty>) {
        let e = setup(Script::Ready, true, None);
        let s = Start {
            id: new_agent_id(),
            gen: 0,
            sid: uuid::Uuid::new_v4().to_string(),
            op_id: ops::new_op_id(),
            cwd: e.cwd.canonicalize().unwrap(),
            claude: PathBuf::from("/bin/false"),
        };
        let gen =
            e.sp.write_row(&s, &CLAUDE, &Principal::LocalUnverified, None)
                .await
                .unwrap();
        let s = Start { gen, ..s };
        let mut pty = None;
        if phase != Phase::Row {
            let p = orphan();
            e.sp.write_pty(&s, child(p.child())).unwrap();
            pty = Some(p);
        }
        if phase == Phase::Bound {
            e.sp.write_bound(&s).unwrap();
        }
        e.db.close();
        let db = Arc::new(db::open(&e.home, MACHINE).unwrap());
        let done = ops::reconcile(&db, &e.home).unwrap();
        let r = row(&db, &s.id);
        assert_eq!(agent_state_rows(&db), 0);
        // Nothing is left open.
        let open: i64 = db
            .reader()
            .unwrap()
            .query_row("SELECT count(*) FROM ops WHERE done_at IS NULL", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(open, 0);
        // A second boot finds nothing to do.
        assert!(ops::reconcile(&db, &e.home).unwrap().is_empty());
        (Env { db, ..e }, done, r, pty)
    }

    #[tokio::test]
    async fn a_crash_after_phase_row_fails_the_agent() {
        let (e, done, r, _) = crash_after(Phase::Row, |c| c).await;
        assert_eq!(done.failed, [r.id.clone()]);
        assert_eq!(done.killed, 0);
        assert_eq!(r.state.as_deref(), Some("failed"));
        assert!(r.gone_at.is_some());
        assert_eq!(
            ops_of(&e.db),
            [(
                "row".to_string(),
                true,
                Some(ops::CREATE_INTERRUPTED.to_string())
            )]
        );
        let failed = events(&e.db, "agent.failed");
        assert_eq!(failed[0]["reason"], ops::CREATE_INTERRUPTED);
    }

    #[tokio::test]
    async fn a_crash_after_phase_pty_kills_the_orphan_and_fails_the_agent() {
        let (e, done, r, pty) = crash_after(Phase::Pty, |c| c).await;
        let pid = pty.as_ref().unwrap().child().pid;
        assert_eq!(done.failed, [r.id.clone()]);
        assert_eq!(done.killed, 1);
        assert!(!runs(pid));
        assert_eq!(r.state.as_deref(), Some("failed"));
        assert_eq!(
            ops_of(&e.db),
            [(
                "pty".to_string(),
                true,
                Some(ops::CREATE_INTERRUPTED.to_string())
            )]
        );
    }

    #[tokio::test]
    async fn a_crash_after_phase_bound_kills_the_orphan_and_interrupts_the_agent() {
        let (e, done, r, pty) = crash_after(Phase::Bound, |c| c).await;
        let pid = pty.as_ref().unwrap().child().pid;
        assert_eq!(done.interrupted, [r.id.clone()]);
        assert_eq!(done.killed, 1);
        assert!(!runs(pid));
        assert_eq!(r.state.as_deref(), Some("interrupted"));
        assert!(r.interrupted_note_pending);
        assert!(r.gone_at.is_none());
        assert_eq!(
            ops_of(&e.db),
            [("bound".to_string(), true, Some("bound".to_string()))]
        );
    }

    #[tokio::test]
    async fn boot_never_kills_a_reused_pid() {
        // The journal's pid now runs another process (another start time).
        let (_e, done, r, pty) = crash_after(Phase::Pty, |c| ChildId {
            pid: c.pid,
            start: c.start + 1,
        })
        .await;
        let mut pty = pty.unwrap();
        assert_eq!(done.pid_reused, 1);
        assert_eq!(done.killed, 0);
        assert!(runs(pty.child().pid));
        assert_eq!(r.state.as_deref(), Some("failed"));
        pty.kill().unwrap();
    }

    #[test]
    fn find_on_path_needs_an_executable_file() {
        assert!(find_on_path("sh").is_some());
        assert!(find_on_path("c9watch-no-such-binary").is_none());
    }

    #[test]
    fn detail_prefers_the_vendor_lines_and_is_capped() {
        assert_eq!(detail("a\nUsage limit reached\nb"), "Usage limit reached");
        assert_eq!(detail("   \n"), "the screen was empty");
        let long = "x".repeat(MAX_DETAIL + 10);
        assert_eq!(detail(&long).chars().count(), MAX_DETAIL);
        let many: String = (0..50).map(|i| format!("l{}\n", i)).collect();
        assert!(detail(&many).starts_with("l30\n"));
    }
}
