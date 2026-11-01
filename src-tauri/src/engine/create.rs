//! Free-mode `agent.create` and `agent.retire` (plan C1a-26, A:794-828).
//!
//! `agent.create` is always on (C1a-35 dropped the experimental switch).
//! Only `local-unverified` may call it, or `agent.retire`
//! (§1.1, v3.1: any L may retire). There is no cap on live agents: the
//! legacy daemon's `DEFAULT_MAX_WORKERS=16` does not carry over.
//!
//! A create:
//! 1. Checks the engine's `c9watch-agent` copy, realpaths the cwd, and
//!    refuses a cwd inside the engine home or a managed worktree home
//!    (`.c9watch/worktrees`) with `cwd_collision` (D22.9), and a cwd that
//!    holds the engine home.
//! 2. One transaction: `cwd_collision` when a live engine agent's cwd is
//!    above or below this one, or any other cwd at all (one cwd at a time,
//!    M7 reviews H1), then the `agents` row (`creating`, `created_by` = the
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
//! **Confirmed kills (D2).** Every kill before a row ends (retire, exit,
//! a failed create or resume) must confirm that the process, its group
//! and its descendants are gone (`Driver::kill`: SIGTERM, then SIGKILL,
//! then a check). One that cannot makes the row `orphaned` instead
//! (`ops::orphan_agent`): never resumed, its PTY child kept in
//! `agent_state`. A retire of an `orphaned` agent kills that child again by
//! pid and start time (`pty::kill_orphan`) and makes the tombstone only
//! once it is gone.
//!
//! The supervisor owns the `Driver`. After `ready` it keeps following the
//! screen and feeds its state into the merge as a screen claim
//! (`hooks::screen_claim`), the fallback for the `UserPromptSubmit` and
//! `PostToolUse` hooks that claim `working` first (reviews H1). When a
//! hook's hold on the merge runs out with no new claim, it writes the
//! merge's state (`hooks::expire_hold`). On engine
//! shutdown it kills the PTY and leaves the row as it is; the next boot
//! reconciles it (`ops`).
//!
//! **Drain (C1a-27, `engine::drain`).** Once the engine's drain switch is
//! on, `agent.create` answers `draining`, a create that has its row but no
//! PTY spawns nothing (its op is left to boot reconcile), a PTY write
//! is not sent but audited, and mail stays queued. A drained supervisor kills its agent and then
//! writes `paused(drain)` or `interrupted` (`drain::settle`); a kill that
//! fails writes nothing, so boot reconcile still finds and kills the
//! process by its `agent_state` (reviews M1).
//!
//! **Boot resume (C1a-28, A:236, A:1038).** After boot reconcile, the
//! engine resumes every `paused(drain)` and `interrupted` agent
//! (`resume_on_boot`) unless it drains:
//! - The session is the one in the row's `resume_argv` (`claude --resume
//!   <id>`), or `provider_sid` for an agent that boot reconcile marked
//!   `interrupted` without a drain. The spawn is the same profile with
//!   `--resume` for `--session-id` and a new settings file for the new
//!   generation.
//! - One transaction starts the next generation (`agents.generation` + 1
//!   and a `session_alias` row on the same session and transcript) and
//!   opens an op of kind `agent.resume` (`engine::ops`). A resume after a
//!   drain has reason `engine_restart` and does not count in
//!   `resume_count`; one after a crash does.
//! - The supervisor then brings the agent up exactly as for a create. No
//!   quota is checked first (D28 Q7). An exit before `ready`, a vendor
//!   refusal or no `ready` within 30 s puts the row back to `paused` or
//!   `interrupted` with reason `resume_failed`, its `resume_argv` and its
//!   mail kept, and audits the vendor's text (`ops::resume_failed`). A
//!   boot never tries it again; `agent.resume` does (D4, `resume`).
//!   `ready` clears `state_reason` and `resume_argv`; the resume
//!   SessionStart takes the interrupted note (`engine::standing`).
//! - A drain or stop of this engine while the resume comes up is not a
//!   resume failure: the supervisor kills the agent and puts the row back
//!   to `paused` or `interrupted` with reason `drain` and its
//!   `resume_argv`, so the next boot resumes it again (reviews M2). Only a
//!   kill that fails, or an engine that dies, leaves it to boot reconcile,
//!   which marks it `resume_failed` (or `orphaned`, if its kill fails
//!   too).
//! - Events of the old generation are dropped as `stale_generation` and
//!   audited (`engine::hooks`).
//!
//! **Mail (C1b-M1 §4).** The supervisor is the only writer of mail to its
//! agent. One message at a time, it takes the oldest `queued` one when the
//! agent is ready, its row is `ready`, its input box is free and the
//! engine does not drain; it pastes the fenced message (bracketed paste)
//! and sends CR only once the input box shows that paste. The prompt hook
//! confirms delivery (`engine::hooks`); with no hook within
//! `SUBMIT_CONFIRM`, an open turn and a box without the paste confirm it.
//! Anything that may have reached the agent unconfirmed is `uncertain`,
//! raises attention and is never sent again. Only a write that sent
//! nothing (queue full, drain) puts the message back in the queue.

use super::adapter::cc::profile::{
    resolve_cwd, settings_path_of, spawn_spec, write_settings, Profile,
};
use super::adapter::{self, Descriptor};
use super::agent_bin::{AgentBin, Rehash};
use super::agents::LiveTable;
use super::cc_screen::{
    box_has_paste, box_is_free, input_box, paste_payload, vendor_refusal, CcScreen, ScreenEvent,
    ScreenFailure, ScreenState, KEY_CR, READY_TIMEOUT,
};
use super::db::audit::{self, AuditEntry};
use super::db::outbox::NewEvent;
use super::db::{agent_row, new_agent_id, now_ms, Db, Tx, AGENT_COLUMNS};
use super::drain::{self, DrainSwitch, InjectionPath};
use super::driver::{Driver, SpawnSpec};
use super::generation;
use super::generation::{begin_generation, emit_upsert, GenReason};
use super::hooks;
use super::machine::HeaderKey;
use super::mail::{self, MsgState};
use super::merge::{Activity, MergeTable};
use super::ops::{self, CreatePayload, Phase};
use super::principal::Principal;
use super::proto::{
    AgentCreateParams, AgentResumeParams, AgentRetireParams, AgentRow, ApiError, ErrorCode,
};
use super::pty::{kill_orphan, NativePty};
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

/// Why a create failed: the vendor refused, the agent exited or it was
/// not ready in time.
pub const SPAWN_FAILED: &str = "spawn_failed";

/// Why a ready agent's row failed: its process exited by itself.
pub const EXITED: &str = "exited";

/// How long the supervisor waits for output before it looks again.
const POLL: Duration = Duration::from_millis(100);

/// How often a ready agent's supervisor checks for commands and an exit.
const RUN_TICK: Duration = Duration::from_millis(250);

/// Mail (C1b-M1 §4.5): how long a paste settles before the supervisor
/// looks for it in the input box.
const PASTE_SETTLE: Duration = Duration::from_millis(600);

/// How long a paste may take to show in the input box, and a CR to get
/// into a full write queue. After it the message is `uncertain`.
const PASTE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long after the CR the prompt hook may take to confirm a message.
const SUBMIT_CONFIRM: Duration = Duration::from_secs(10);

/// Pastes of one message that found the PTY write queue full before it
/// fails.
const MAX_ATTEMPTS: i64 = 3;

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
}

type Reply = oneshot::Sender<Result<AgentRow, ApiError>>;

/// Why `Spawner::pty_write` did not send its bytes, or may have sent only
/// part of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyWriteError {
    /// The engine drains: nothing was sent (audited with path `pty`).
    Blocked,
    /// The driver's write failed. For `NativePty`, only `WouldBlock` (the
    /// write queue is full) means that nothing was sent; after `TimedOut`
    /// the bytes may still arrive, and any other error may leave a part
    /// sent.
    Io(io::ErrorKind, String),
}

/// Stop taking mail after an `uncertain` write on screen `text` (§4.6).
fn hold(text: &str, mail_pending: &mut bool) -> Delivery {
    *mail_pending = false;
    Delivery::PasteHeld {
        seen: text.to_string(),
    }
}

enum Cmd {
    /// Kill the agent and make its row a tombstone.
    Retire { principal: String, reply: Reply },
    /// The engine stops: kill the agent, leave the row to boot reconcile.
    Shutdown,
    /// The engine drains: kill the agent, then `drain::settle` its row.
    Drain,
    /// New mail for the agent (C1b-M1 §4.4). It carries nothing: the
    /// supervisor reads its queue from the store.
    Mail,
}

/// Where the supervisor is with the one message it delivers (§4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Delivery {
    Idle,
    /// The paste went out at `at`; the CR waits for the input box to
    /// show it.
    Pasted {
        id: String,
        at: Instant,
    },
    /// The CR went out at `at`; the prompt hook should confirm it.
    Submitted {
        id: String,
        at: Instant,
    },
    SocketSending {
        id: String,
        at: Instant,
    },
    SocketAwaiting {
        id: String,
        since: Instant,
        enqueued: Option<Instant>,
        turn_ended: Option<Instant>,
        held: Option<Instant>,
        receipt_seq: i64,
    },
    /// A paste or CR write went `uncertain` on screen `seen`: the agent
    /// may not read its terminal, and its bytes may still arrive. No mail
    /// goes until the screen changes, or a nudge comes, with a free box.
    PasteHeld {
        seen: String,
    },
}

struct Handle {
    cmds: Sender<Cmd>,
    thread: JoinHandle<()>,
}

/// Whether a supervisor starts a new session or resumes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Create,
    Resume,
}

impl Kind {
    /// The op kind (and audit method), and the reason of a failure.
    fn failure(self) -> (&'static str, &'static str) {
        match self {
            Kind::Create => (ops::KIND_CREATE, SPAWN_FAILED),
            Kind::Resume => (ops::KIND_RESUME, ops::RESUME_FAILED),
        }
    }
}

/// One create or resume, as the supervisor gets it.
#[derive(Debug, Clone)]
struct Start {
    id: String,
    gen: i64,
    sid: String,
    op_id: String,
    cwd: PathBuf,
    claude: PathBuf,
    kind: Kind,
    /// A resume: the row's state before it (`paused` or `interrupted`).
    from: Option<String>,
}

/// What `resume_on_boot` did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Resumed {
    /// A new generation and a supervisor each; `ready` comes later.
    pub started: Vec<String>,
    /// Failed before the spawn, now `failed` with `resume_failed`.
    pub failed: Vec<String>,
    /// Not resumed: the engine drains, or a write failed.
    pub left: Vec<String>,
}

/// A row the boot resume picks up.
#[derive(Debug, Clone)]
struct Resumable {
    id: String,
    state: String,
    reason: Option<String>,
    provider: String,
    sid: Option<String>,
    cwd: Option<String>,
    argv: Option<String>,
}

/// How bringing an agent up ended.
enum Up {
    Ready(Box<dyn Driver>, Box<AgentRow>, Box<CcScreen>),
    Failed(ApiError),
    Retired,
    Shutdown,
    /// The engine drains: nothing was spawned.
    Draining,
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
    /// The engine's drain switch (C1a-27).
    drain: Arc<DrainSwitch>,
    /// The mail header key (C1b-M1-7). `None` only in tests.
    header_key: Option<HeaderKey>,
    sockets: Option<Arc<super::cc_listener::Sockets>>,
    socket_jobs: Mutex<HashMap<String, std::sync::mpsc::Receiver<super::cc_inbox::SocketOutcome>>>,
    socket_seen: Mutex<HashSet<(String, i64)>>,
    peer_tailers: Mutex<HashMap<(String, i64), super::peer_log::Tailer>>,
    /// How `agent.retire` kills an `orphaned` agent's recorded PTY child
    /// (`pty::kill_orphan`; a test swaps it).
    orphan_killer: Mutex<Arc<ops::Killer>>,
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
            drain: Arc::new(DrainSwitch::new()),
            header_key: None,
            sockets: None,
            socket_jobs: Mutex::new(HashMap::new()),
            socket_seen: Mutex::new(HashSet::new()),
            peer_tailers: Mutex::new(HashMap::new()),
            orphan_killer: Mutex::new(Arc::new(kill_orphan)),
        }
    }

    pub fn with_mail_sockets(mut self, sockets: Arc<super::cc_listener::Sockets>) -> Self {
        self.sockets = Some(sockets);
        self
    }

    /// Stamp mail with tokens from the engine's header key.
    pub fn with_header_key(mut self, key: HeaderKey) -> Spawner {
        self.header_key = Some(key);
        self
    }

    /// The mail header token of `agent_id`, as its SessionStart context
    /// states it. `None` without a header key (tests).
    pub fn header_token(&self, agent_id: &str) -> Option<String> {
        self.header_key.as_ref().map(|k| k.token(agent_id))
    }

    /// Share the engine's drain switch.
    pub fn with_drain(mut self, drain: Arc<DrainSwitch>) -> Spawner {
        self.drain = drain;
        self
    }

    /// The drain switch this spawner obeys.
    pub fn drain_switch(&self) -> &Arc<DrainSwitch> {
        &self.drain
    }

    /// The supervisors' clock (`with_clock`).
    pub(super) fn now(&self) -> Instant {
        (self.clock)()
    }

    pub(super) fn db(&self) -> &Db {
        &self.db
    }

    /// The agents whose supervisor runs, sorted.
    pub(super) fn supervised(&self) -> Vec<String> {
        let mut ids: Vec<String> = lock(&self.live).keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Tell the supervisor of `id` to stop its agent for the drain.
    /// Returns whether it took the command.
    pub(super) fn drain_agent(&self, id: &str) -> bool {
        let cmds = lock(&self.live).get(id).map(|h| h.cmds.clone());
        cmds.is_some_and(|c| c.send(Cmd::Drain).is_ok())
    }

    /// Tell `id`'s supervisor that it has new mail. No-op when `id` has
    /// none yet (still coming up): a supervisor reads its queue once it is
    /// ready anyway.
    pub fn nudge_mail(&self, id: &str) {
        if let Some(c) = lock(&self.live).get(id).map(|h| h.cmds.clone()) {
            let _ = c.send(Cmd::Mail);
        }
    }

    pub fn with_launcher(mut self, launcher: Arc<dyn Launcher>) -> Spawner {
        self.launcher = launcher;
        self
    }

    #[cfg(test)]
    fn set_orphan_killer(&self, killer: Arc<ops::Killer>) {
        *lock(&self.orphan_killer) = killer;
    }

    pub fn with_clock(mut self, clock: Clock) -> Spawner {
        self.clock = clock;
        self
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
        local_only(principal, "agent.create")?;
        if self.drain.on() {
            return Err(draining());
        }
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
            kind: Kind::Create,
            from: None,
        };
        let gen = self
            .write_row(&start, desc, principal, p.name.clone())
            .await?;
        #[cfg(feature = "live-test")]
        super::live_test::crash_after(Phase::Row);
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
                if let Some(other) = other_live_cwd(tx, &s.cwd)? {
                    return Ok(Err(ApiError::new(
                        ErrorCode::CwdCollision,
                        format!(
                            "live agent {} runs in another cwd {:?}: its deny rules do not \
                             name {:?}, so one agent could rewrite the other's .claude/ \
                             (one cwd at a time until a rule for every live agent is \
                             verified live)",
                            other.0, other.1, s.cwd
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
        if self.drain.on() {
            // A create's row and op stay for boot reconcile; a resume
            // leaves its row for the next boot.
            drop(live);
            if s.kind == Kind::Resume {
                self.drop_resume(&s);
            }
            return Err(draining());
        }
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
            Up::Ready(driver, row, screen) => {
                let _ = reply.send(Ok(*row));
                self.run(&s, driver, *screen, &cmds);
            }
            Up::Failed(e) => {
                if s.kind == Kind::Resume {
                    eprintln!("[engine] The resume of {} failed: {}", s.id, e.msg);
                }
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
            Up::Draining => {
                let _ = reply.send(Err(draining()));
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
        // A failure after the spawn: the agent goes first. A kill that
        // cannot confirm it leaves the row `orphaned`, not failed (D2).
        let kill_fail = |driver: &mut Box<dyn Driver>, why: String| {
            if self.kill_confirmed(s, driver.as_mut(), audit::SYSTEM, s.kind.failure().0) {
                fail(why)
            } else {
                Up::Failed(ApiError::new(
                    ErrorCode::SpawnFailed,
                    format!("{}: {}; {}", s.id, why, ORPHANED_TEXT),
                ))
            }
        };
        // The other live agents' cwds: their `.claude/` is denied too
        // (C1a-32). A failed read spawns nothing rather than spawn without.
        let peers = match self
            .db
            .pooled_reader()
            .map_err(|e| e.to_string())
            .and_then(|c| {
                peer_cwds(&c, &s.id).map_err(|e| format!("Failed to read the peer cwds: {}", e))
            }) {
            Ok(p) => p,
            Err(e) => return fail(e),
        };
        let profile = match Profile::new(
            self.agent_bin.path(),
            &self.config.sock,
            &s.cwd,
            &s.id,
            s.gen,
            &s.sid,
        ) {
            Ok(p) if s.kind == Kind::Resume => p.resuming().with_peers(peers),
            Ok(p) => p.with_peers(peers),
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
        // No spawn once the engine drains: a create's op stays open for
        // boot reconcile, which also removes the settings file; a resume
        // leaves its row for the next boot.
        if self.drain.on() {
            if s.kind == Kind::Resume {
                self.drop_resume(s);
            }
            return Up::Draining;
        }
        // The one spawn of this create or resume; nothing below spawns
        // again.
        let mut driver = match self.launcher.spawn(&spec) {
            Ok(d) => d,
            Err(e) => return fail(format!("could not start {:?}: {}", s.claude, e)),
        };
        let child = driver.child();
        if let Err(e) = self.agents.register(&s.id, s.gen, child) {
            return kill_fail(&mut driver, e);
        }
        match self.write_pty(s, child) {
            Ok(true) => {
                #[cfg(feature = "live-test")]
                super::live_test::crash_after(Phase::Pty);
            }
            Ok(false) => return self.retired_meanwhile(s, driver),
            Err(e) => return kill_fail(&mut driver, e),
        }
        // A drain that started during the spawn: the agent goes at once
        // (reviews L2). The PTY child is in `agent_state` by now, so a
        // kill that fails is still boot reconcile's.
        if self.drain.on() {
            return self.cut(s, driver);
        }
        let mut screen = CcScreen::new((self.clock)());
        loop {
            match cmds.try_recv() {
                Ok(Cmd::Retire { principal, reply }) => {
                    let _ = reply.send(self.retire_killed(s, driver.as_mut(), &principal));
                    return Up::Retired;
                }
                Ok(Cmd::Shutdown | Cmd::Drain) | Err(TryRecvError::Disconnected) => {
                    return self.cut(s, driver);
                }
                // `run` reads the queue once the agent is ready.
                Ok(Cmd::Mail) | Err(TryRecvError::Empty) => {}
            }
            let out = driver.output(POLL);
            let snap = driver.screen();
            if self.take_start(&s.id, s.gen) {
                screen.session_start();
            }
            if let Some(text) = vendor_refusal(&snap.text) {
                return kill_fail(
                    &mut driver,
                    format!("the vendor refused before ready: {}", text),
                );
            }
            let mut ready = false;
            for ev in screen.step(&snap.title, &snap.text, snap.quiet, (self.clock)()) {
                match ev {
                    ScreenEvent::Write(keys) => {
                        let _ = self.pty_write(&s.id, driver.as_mut(), keys);
                    }
                    ScreenEvent::Ready { first: true } => ready = true,
                    ScreenEvent::Failed(why) => {
                        let why = format!("{}: {}", failure_text(why), detail(&snap.text));
                        return kill_fail(&mut driver, why);
                    }
                    _ => {}
                }
            }
            if ready {
                return match self.write_bound(s) {
                    Ok(Some(row)) => {
                        #[cfg(feature = "live-test")]
                        super::live_test::crash_after(Phase::Bound);
                        Up::Ready(driver, Box::new(row), Box::new(screen))
                    }
                    Ok(None) => self.retired_meanwhile(s, driver),
                    Err(e) => kill_fail(&mut driver, e),
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
                let why = format!("exited before ready ({}): {}", status, detail(&text));
                return kill_fail(&mut driver, why);
            }
        }
    }

    /// The engine stops or drains while `s` comes up: kill the agent. A
    /// create's op stays open for boot reconcile, which fails it. A resume
    /// whose kill worked goes back to the row it came from
    /// (`drop_resume`): the session did not fail (reviews M2).
    fn cut(&self, s: &Start, mut driver: Box<dyn Driver>) -> Up {
        let killed = driver.kill();
        match killed {
            Ok(_) if s.kind == Kind::Resume => self.drop_resume(s),
            Ok(_) => {}
            Err(e) => eprintln!(
                "[engine] Failed to kill {} while it came up; left to boot reconcile: {}",
                s.id, e
            ),
        }
        if self.drain.on() {
            Up::Draining
        } else {
            Up::Shutdown
        }
    }

    /// A ready agent: follow its screen, and wait for a command or its
    /// exit.
    fn run(
        &self,
        s: &Start,
        mut driver: Box<dyn Driver>,
        mut screen: CcScreen,
        cmds: &Receiver<Cmd>,
    ) {
        #[cfg(feature = "live-test")]
        let mut last_screen = String::new();
        // The screen's last state: an `Idle` is claimed again when a turn
        // stays open with nothing said for SETTLE_MS (`stale_turn`).
        let mut screen_state: Option<ScreenState> = None;
        let mut reclaimed = false;
        // A new or resumed supervisor reads its queue once (§4.4): mail
        // may have come while it was down, or before it was live.
        let mut mail_pending = true;
        let mut delivery = Delivery::Idle;
        loop {
            match cmds.recv_timeout(RUN_TICK) {
                Ok(Cmd::Retire { principal, reply }) => {
                    self.stop_delivery(s, &delivery, "target retired", "target retired");
                    let _ = reply.send(self.retire_killed(s, driver.as_mut(), &principal));
                    return;
                }
                Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                    self.stop_delivery(s, &delivery, "shutdown before submit", "shutdown");
                    let _ = driver.kill();
                    return;
                }
                Ok(Cmd::Drain) => {
                    self.drain_delivery(s, &delivery);
                    self.drain_now(s, driver);
                    return;
                }
                Ok(Cmd::Mail) => mail_pending = true,
                Err(RecvTimeoutError::Timeout) => {}
            }
            // Nobody reads the bytes yet; the screen keeps up regardless.
            let _ = driver.output(Duration::ZERO);
            let snap = driver.screen();
            #[cfg(feature = "live-test")]
            {
                use super::live_test;
                live_test::dump_screen(&self.config.home, &s.id, &snap.text, &mut last_screen);
                // Not while mail is in the input box: both write the PTY.
                if delivery == Delivery::Idle {
                    if let Some(keys) = live_test::take_input(&self.config.home, &s.id) {
                        let _ = self.pty_write(&s.id, driver.as_mut(), &keys);
                    }
                }
            }
            for ev in screen.step(&snap.title, &snap.text, snap.quiet, (self.clock)()) {
                if let ScreenEvent::State(st) = ev {
                    screen_state = Some(st);
                    reclaimed = false;
                    self.screen_claim(s, st);
                }
            }
            // Once per screen state, so a row the claim cannot write is
            // not asked again every tick.
            if screen_state == Some(ScreenState::Idle) && !reclaimed {
                let now = now_ms();
                if self.merge.peek(&s.id, s.gen, |m| m.stale_turn(now)) == Some(true) {
                    reclaimed = true;
                    self.screen_claim(s, ScreenState::Idle);
                }
            }
            self.expire_hold(s);
            self.poll_peer_log(s);
            delivery = self.deliver(
                s,
                driver.as_mut(),
                &screen,
                &snap.text,
                delivery,
                &mut mail_pending,
            );
            if let Ok(Some(status)) = driver.try_wait() {
                let text = driver.screen().text;
                // The rest of its group and descendants go too.
                if !self.kill_confirmed(s, driver.as_mut(), audit::SYSTEM, ops::METHOD_EXIT) {
                    return;
                }
                let why = format!("exited ({}): {}", status, detail(&text));
                let (machine, id) = (self.db.machine_id().to_string(), s.id.clone());
                let wrote = self.db.write_blocking(move |tx| {
                    ops::fail_agent(tx, &machine, &id, ops::METHOD_EXIT, EXITED, &why, now_ms())
                });
                if let Err(e) = wrote {
                    eprintln!("[engine] Failed to record the exit of {}: {}", s.id, e);
                }
                remove_settings(&self.config.home, &s.id, s.gen);
                return;
            }
        }
    }

    /// PTY uses the ready input box; socket delivery can run mid-turn.
    /// Socket writes and CC acceptance are never context-delivery evidence.
    /// One step of mail delivery (§4.5) on this tick's screen; returns
    /// the next step.
    fn deliver(
        &self,
        s: &Start,
        driver: &mut dyn Driver,
        screen: &CcScreen,
        text: &str,
        delivery: Delivery,
        mail_pending: &mut bool,
    ) -> Delivery {
        let now = (self.clock)();
        match delivery {
            Delivery::Idle => {
                if self.drain.on() {
                    return Delivery::Idle;
                }
                // Adopt an in-flight socket delivery before considering the queue.
                if let Ok(c) = self.db.pooled_reader() {
                    if let Ok(Some(id)) = mail::socket_inflight(&c, &s.id) {
                        return socket_awaiting(id, now);
                    }
                }
                let candidate = match self
                    .db
                    .pooled_reader()
                    .ok()
                    .and_then(|c| mail::peek_next_id(&c, &s.id, now_ms()).ok())
                    .flatten()
                {
                    Some(id) => id,
                    None => {
                        *mail_pending = false;
                        return Delivery::Idle;
                    }
                };
                let route = self.socket_route(s, &candidate);
                if route.is_none() && !self.may_deliver(s, screen, text) {
                    return Delivery::Idle;
                }
                let selected_via = if route.is_some() { "socket" } else { "pty" };
                let (machine, id) = (self.db.machine_id().to_string(), s.id.clone());
                let taken = self.mail_write(s, move |tx| {
                    if mail::peek_next_id(tx, &id, now_ms())?.as_deref() != Some(&candidate) {
                        return Ok(None);
                    }
                    mail::take_next_via(tx, &machine, &id, Some(selected_via), now_ms())
                });
                let Some(Some(m)) = taken else {
                    // Empty, or the store failed: a nudge asks again.
                    *mail_pending = false;
                    return Delivery::Idle;
                };
                let fenced = mail::signed_fence(
                    &m.id,
                    self.header_token(&s.id).as_deref(),
                    self.header_key.as_ref(),
                    &m.provenance,
                    &m.body,
                );
                if let Some(route) = route {
                    return self.start_socket(s, &m, route, &fenced, now);
                }
                match self.pty_write(&s.id, driver, &paste_payload(&fenced)) {
                    Ok(()) => Delivery::Pasted { id: m.id, at: now },
                    Err(PtyWriteError::Blocked) => {
                        self.requeue_mail(s, &m.id, "drain");
                        Delivery::Idle
                    }
                    Err(PtyWriteError::Io(io::ErrorKind::WouldBlock, _))
                        if m.attempts < MAX_ATTEMPTS =>
                    {
                        self.requeue_mail(s, &m.id, "pty write queue full");
                        Delivery::Idle
                    }
                    Err(PtyWriteError::Io(io::ErrorKind::WouldBlock, _)) => {
                        self.mark_mail(s, &m.id, MsgState::Failed, "pty write queue full");
                        Delivery::Idle
                    }
                    Err(PtyWriteError::Io(io::ErrorKind::BrokenPipe, e)) => {
                        self.mark_mail(s, &m.id, MsgState::Failed, &format!("pty gone: {}", e));
                        Delivery::Idle
                    }
                    Err(PtyWriteError::Io(_, e)) => {
                        self.mail_uncertain(s, &m.id, &format!("paste may be partial: {}", e));
                        hold(text, mail_pending)
                    }
                }
            }
            Delivery::Pasted { id, at } => {
                let waited = now.saturating_duration_since(at);
                if waited >= PASTE_SETTLE && box_has_paste(&input_box(text), &id) {
                    match self.pty_write(&s.id, driver, KEY_CR) {
                        Ok(()) => return Delivery::Submitted { id, at: now },
                        // Nothing was submitted; the box goes with the
                        // agent, and a resume delivers it again.
                        Err(PtyWriteError::Blocked) => {
                            self.requeue_mail(s, &id, "drain before submit");
                            return Delivery::Idle;
                        }
                        Err(PtyWriteError::Io(io::ErrorKind::WouldBlock, _))
                            if waited < PASTE_TIMEOUT =>
                        {
                            return Delivery::Pasted { id, at };
                        }
                        Err(PtyWriteError::Io(_, e)) => {
                            self.mail_uncertain(s, &id, &format!("cr write: {}", e));
                            return hold(text, mail_pending);
                        }
                    }
                }
                if waited >= PASTE_TIMEOUT {
                    self.mail_uncertain(s, &id, "paste not seen in input box");
                    return hold(text, mail_pending);
                }
                Delivery::Pasted { id, at }
            }
            Delivery::Submitted { id, at } => {
                let read = match self.db.pooled_reader() {
                    Ok(c) => mail::state_of(&c, &id).map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                };
                // A failed read keeps the message tracked: try the next tick.
                let state = match read {
                    Ok(state) => state,
                    Err(e) => {
                        eprintln!("[engine] Failed to read mail {}: {}", id, e);
                        return Delivery::Submitted { id, at };
                    }
                };
                // The prompt hook confirmed it, or an end path took it.
                if state != Some(MsgState::Delivering) {
                    return Delivery::Idle;
                }
                let waited = now.saturating_duration_since(at);
                if waited < SUBMIT_CONFIRM {
                    return Delivery::Submitted { id, at };
                }
                let turn_open = self.merge.peek(&s.id, s.gen, |m| m.turn_open()) == Some(true);
                // As for the CR: CC collapses the fence to `[Pasted text #`.
                let in_box = box_has_paste(&input_box(text), &id);
                if turn_open && !in_box {
                    self.mark_mail(s, &id, MsgState::Delivered, "turn_open");
                } else {
                    let why = format!("no prompt hook after {} ms", waited.as_millis());
                    self.mail_uncertain(s, &id, &why);
                }
                Delivery::Idle
            }
            Delivery::SocketSending { id, at } => {
                let result =
                    self.socket_jobs
                        .lock()
                        .unwrap()
                        .get(&id)
                        .and_then(|rx| match rx.try_recv() {
                            Ok(r) => Some(r),
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                Some(super::cc_inbox::SocketOutcome::MaybeSent(
                                    "writer disconnected".into(),
                                ))
                            }
                            Err(_) => None,
                        });
                if let Some(out) = result {
                    self.socket_jobs.lock().unwrap().remove(&id);
                    // A receipt may have committed before the worker's result.
                    if self
                        .db
                        .pooled_reader()
                        .ok()
                        .and_then(|c| mail::state_of(&c, &id).ok())
                        .flatten()
                        != Some(MsgState::Delivering)
                    {
                        return Delivery::Idle;
                    }
                    match out {
                        super::cc_inbox::SocketOutcome::NotSent(e) => {
                            if self.drain.on() {
                                self.requeue_mail(s, &id, "drain");
                            } else {
                                self.retry_socket(s, &id, &e);
                            }
                            Delivery::Idle
                        }
                        super::cc_inbox::SocketOutcome::Blocked => {
                            self.requeue_mail(s, &id, "drain");
                            Delivery::Idle
                        }
                        super::cc_inbox::SocketOutcome::Sent => socket_awaiting(id, at),
                        super::cc_inbox::SocketOutcome::MaybeSent(e) => {
                            self.note_mail(s, &id, &e);
                            socket_awaiting(id, at)
                        }
                    }
                } else {
                    Delivery::SocketSending { id, at }
                }
            }
            Delivery::SocketAwaiting {
                id,
                mut since,
                mut enqueued,
                mut turn_ended,
                mut held,
                mut receipt_seq,
            } => {
                let Ok(c) = self.db.pooled_reader() else {
                    return Delivery::SocketAwaiting {
                        id,
                        since,
                        enqueued,
                        turn_ended,
                        held,
                        receipt_seq,
                    };
                };
                if mail::state_of(&c, &id).ok().flatten() != Some(MsgState::Delivering) {
                    return Delivery::Idle;
                }
                // Replay all unseen notes so a later enqueue cannot hide a held receipt.
                let notes = (|| -> rusqlite::Result<Vec<(i64, Option<String>)>> {
                    let mut st = c.prepare("SELECT seq, detail FROM message_status WHERE msg_id = ?1 AND seq > ?2 ORDER BY seq")?;
                    let rows = st.query_map(rusqlite::params![id, receipt_seq], |r| Ok((r.get(0)?, r.get(1)?)))?.collect(); rows
                })().unwrap_or_default();
                for (seq, detail) in notes {
                    receipt_seq = seq;
                    match detail.as_deref() {
                        Some(d) if d.starts_with("held:") => {
                            if held.is_none() {
                                held = Some(now);
                            }
                        }
                        Some("cc accepted; awaiting evidence") => {
                            held = None;
                            since = now;
                            enqueued = None;
                            turn_ended = None;
                        }
                        Some("cc enqueued") => {
                            if enqueued.is_none() {
                                enqueued = Some(now);
                            }
                        }
                        _ => {}
                    }
                }
                let ended = self
                    .merge
                    .peek(&s.id, s.gen, |m| m.last_turn_end())
                    .flatten();
                if let (Some(enqueued_at), Some(end)) = (enqueued, ended) {
                    // An old idle/turn-end observed before this enqueue is no evidence.
                    let age = now_ms().saturating_sub(end.at_ms).max(0) as u64;
                    let end_at = now
                        .checked_sub(Duration::from_millis(age))
                        .unwrap_or(enqueued_at);
                    if end_at >= since && turn_ended.is_none() {
                        turn_ended = Some(end_at);
                    }
                }
                let why = if held.is_some_and(|at| {
                    now.saturating_duration_since(at) >= Duration::from_secs(15 * 60)
                }) {
                    Some("held, no decision in 15 min".to_string())
                } else if held.is_some() {
                    None
                } else if enqueued.is_none()
                    && now.saturating_duration_since(since) >= Duration::from_secs(30)
                {
                    Some(format!(
                        "no queue-operation after {} ms",
                        now.saturating_duration_since(since).as_millis()
                    ))
                } else if turn_ended
                    .is_some_and(|at| now.saturating_duration_since(at) >= Duration::from_secs(15))
                {
                    Some("queued in CC, not seen after turn end".into())
                } else {
                    None
                };
                if let Some(why) = why {
                    self.mail_uncertain(s, &id, &why);
                    Delivery::Idle
                } else {
                    Delivery::SocketAwaiting {
                        id,
                        since,
                        enqueued,
                        turn_ended,
                        held,
                        receipt_seq,
                    }
                }
            }
            Delivery::PasteHeld { seen } => {
                if (text != seen || *mail_pending) && box_is_free(&input_box(text)) {
                    // The queue may have mail the hold kept back.
                    *mail_pending = true;
                    return Delivery::Idle;
                }
                Delivery::PasteHeld { seen }
            }
        }
    }

    /// Select socket only for agent/engine mail. Human input always retains
    /// user authority through PTY. The return socket must be bound first.
    fn socket_route(&self, s: &Start, msg_id: &str) -> Option<super::cc_inbox::Gate> {
        use super::cc_inbox::{self, Gate};
        if !cc_inbox::enabled() {
            return None;
        }
        let c = match self.db.pooled_reader() {
            Ok(c) => c,
            Err(e) => return Some(Gate::Unknown(e)),
        };
        let origin: String = match c.query_row(
            "SELECT origin_kind FROM messages WHERE id = ?1 AND to_agent = ?2",
            params![msg_id, s.id],
            |r| r.get(0),
        ) {
            Ok(o) => o,
            Err(e) => return Some(Gate::Unknown(e.to_string())),
        };
        if origin == "local-unverified" {
            return None;
        }
        let state = self
            .merge
            .peek(&s.id, s.gen, |m| m.row_state(now_ms()))
            .flatten();
        if matches!(
            state,
            Some("paused" | "interrupted" | "retired" | "retiring" | "failed" | "orphaned")
        ) {
            return None;
        }
        let identity: Option<(String, i32, i64, String)> = match c.query_row("SELECT a.provider, st.pty_child_pid, st.pty_child_start, coalesce(a.provider_sid, ?3) FROM agents a JOIN agent_state st ON st.agent_id = a.id AND st.gen = a.generation WHERE a.id = ?1 AND a.generation = ?2", rusqlite::params![s.id, s.gen, s.sid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional() { Ok(identity) => identity, Err(e) => return Some(Gate::Unknown(e.to_string())) };
        let Some((provider, pid, start, sid)) = identity else {
            return Some(Gate::Unknown("CC child identity unavailable".into()));
        };
        let seen = {
            let mut generations = self.socket_seen.lock().unwrap();
            generations.retain(|(id, gen)| id != &s.id || *gen == s.gen);
            generations.contains(&(s.id.clone(), s.gen))
        };
        let gate = cc_inbox::gate(
            &provider,
            pid,
            start as u64,
            &sid,
            &cc_inbox::sessions_dir(),
            seen,
        );
        match gate {
            Gate::NoProtocol => None,
            Gate::Socket(ref endpoint) => {
                self.socket_seen
                    .lock()
                    .unwrap()
                    .insert((s.id.clone(), s.gen));
                match self
                    .sockets
                    .as_ref()
                    .ok_or_else(|| "engine return socket unavailable".to_string())
                    .and_then(|s| s.bind(endpoint.path.parent().expect("gate checked parent")))
                {
                    Ok(_) => Some(gate),
                    Err(e) => Some(Gate::Unreachable(e)),
                }
            }
            g => Some(g),
        }
    }
    fn transcript_path(&self, s: &Start) -> Option<PathBuf> {
        self.db.pooled_reader().ok()?.query_row("SELECT transcript_path FROM session_alias WHERE agent_id = ?1 AND generation = ?2 AND transcript_path IS NOT NULL ORDER BY rowid DESC LIMIT 1", rusqlite::params![s.id, s.gen], |r| r.get::<_, String>(0)).optional().ok()?.map(PathBuf::from)
    }
    fn start_socket(
        &self,
        s: &Start,
        m: &mail::Mail,
        gate: super::cc_inbox::Gate,
        fenced: &str,
        now: Instant,
    ) -> Delivery {
        use super::cc_inbox::{self, Gate};
        let endpoint = match gate {
            Gate::Socket(e) => e,
            Gate::Unknown(e) | Gate::Unreachable(e) => {
                self.retry_socket(s, &m.id, &e);
                return Delivery::Idle;
            }
            Gate::NoProtocol => unreachable!(),
        };
        let c = match self.db.pooled_reader() {
            Ok(c) => c,
            Err(_) => {
                self.requeue_mail(s, &m.id, "store unavailable");
                return Delivery::Idle;
            }
        };
        let (wire, from, sender_name, kind): (String, Option<String>, Option<String>, String) = match c.query_row("SELECT m.wire_id, m.from_agent, a.name, m.kind FROM messages m LEFT JOIN agents a ON a.id = m.from_agent WHERE m.id = ?1", [&m.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))) { Ok(v) => v, Err(_) => { self.requeue_mail(s, &m.id, "store unavailable"); return Delivery::Idle; } };
        let return_path = match self
            .sockets
            .as_ref()
            .and_then(|s| s.bind(endpoint.path.parent()?).ok())
        {
            Some(p) => p,
            None => {
                self.retry_socket(s, &m.id, "return socket unavailable");
                return Delivery::Idle;
            }
        };
        let addr = match cc_inbox::address(&return_path) {
            Ok(a) => a,
            Err(e) => {
                self.retry_socket(s, &m.id, &e);
                return Delivery::Idle;
            }
        };
        let from_addr = from
            .as_ref()
            .map(|f| format!("{}/{}", self.db.machine_id(), f));
        let reply = if kind == "system" {
            None
        } else {
            from_addr.as_deref().map(|f| (f, m.id.as_str()))
        };
        let label = if kind == "system" {
            "c9watch engine".into()
        } else {
            format!(
                "{} via c9watch",
                sender_name
                    .as_deref()
                    .or(from.as_deref())
                    .unwrap_or("agent")
            )
        };
        let wrapper = cc_inbox::wrapper(
            &addr,
            &label,
            if kind == "system" {
                "prompting"
            } else {
                super::adapter::cc::profile::peer_mode()
            },
            fenced,
            reply,
        );
        let (line, uuid) = cc_inbox::frame(&endpoint, &wire, &addr, &wrapper);
        let (machine, msg) = (self.db.machine_id().to_string(), m.id.clone());
        let wrote = self.mail_write(s, move |tx| {
            mail::set_via(tx, &msg, "socket")?;
            mail::note(tx, &machine, &msg, &format!("uuid={}", uuid), now_ms())
        });
        if wrote.is_none() {
            return Delivery::Idle;
        }
        // fstat before starting the helper, not on the first polling tick.
        if let Some(path) = self.transcript_path(s) {
            let offset = super::peer_log::offset(&path);
            self.peer_tailers
                .lock()
                .unwrap()
                .entry((s.id.clone(), s.gen))
                .or_insert_with(|| super::peer_log::Tailer::at(&path, offset));
        }
        let rx = self.socket_write(&s.id, endpoint, line);
        self.socket_jobs.lock().unwrap().insert(m.id.clone(), rx);
        Delivery::SocketSending {
            id: m.id.clone(),
            at: now,
        }
    }
    /// All outgoing socket injection goes through this drain gate. A second
    /// check on the helper closes the scheduling gap before connect/write.
    fn socket_write(
        &self,
        agent: &str,
        endpoint: super::cc_inbox::Endpoint,
        line: String,
    ) -> std::sync::mpsc::Receiver<super::cc_inbox::SocketOutcome> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (db, drain, agent) = (self.db.clone(), self.drain.clone(), agent.to_string());
        std::thread::spawn(move || {
            let outcome = super::cc_inbox::write_guarded(&endpoint, &line, || drain.on());
            if outcome == super::cc_inbox::SocketOutcome::Blocked {
                let _ = db.write_blocking(move |tx| {
                    drain::audit_blocked(tx, "engine", &agent, InjectionPath::Socket, now_ms())
                });
            }
            let _ = tx.send(outcome);
        });
        rx
    }
    fn retry_socket(&self, s: &Start, id: &str, why: &str) {
        let (machine, msg, why) = (
            self.db.machine_id().to_string(),
            id.to_string(),
            why.to_string(),
        );
        let failed = self.mail_write(s, move |tx| {
            let attempts: i64 =
                tx.query_row("SELECT attempts FROM messages WHERE id = ?1", [&msg], |r| {
                    r.get(0)
                })?;
            if attempts >= mail::SOCKET_MAX_ATTEMPTS {
                mail::mark(
                    tx,
                    &machine,
                    &msg,
                    MsgState::Failed,
                    Some(&format!("socket unreachable: {}", why)),
                    now_ms(),
                )
            } else {
                mail::requeue_after(
                    tx,
                    &machine,
                    &msg,
                    &format!("socket: {}", why),
                    Some(now_ms() + mail::backoff(attempts)),
                    now_ms(),
                )?;
                Ok(false)
            }
        });
        if failed == Some(true) {
            self.mail_attention(s, id, "mail_unreachable");
        }
    }
    fn note_mail(&self, s: &Start, id: &str, detail: &str) {
        let (machine, id, detail) = (
            self.db.machine_id().to_string(),
            id.to_string(),
            detail.to_string(),
        );
        self.mail_write(s, move |tx| {
            mail::note(tx, &machine, &id, &detail, now_ms())
        });
    }
    fn mail_attention(&self, s: &Start, id: &str, reason: &str) {
        self.merge.peek(&s.id, s.gen, |m| {
            m.attention(&format!("{}: {}", reason, id), now_ms())
        });
        let (machine, agent, gen, id, reason) = (
            self.db.machine_id().to_string(),
            s.id.clone(),
            s.gen,
            id.to_string(),
            reason.to_string(),
        );
        self.mail_write(s, move |tx| {
            tx.emit(&NewEvent {
                topic: "agent.attention".into(),
                subject: Some(format!("{}/{}", machine, agent)),
                origin: None,
                data: serde_json::json!({"id":agent,"generation":gen,"reason":reason,"msgId":id}),
            })
            .map(|_| ())
        });
    }
    fn poll_peer_log(&self, s: &Start) {
        use super::transcript::MailFact;
        let Ok(c) = self.db.pooled_reader() else {
            return;
        };
        let eligible: bool = c.query_row("SELECT count(*) > 0 FROM messages WHERE to_agent = ?1 AND via = 'socket' AND (state = 'delivering' OR (state = 'uncertain' AND state_at >= ?2))", rusqlite::params![s.id, now_ms() - mail::LATE_WINDOW_MS], |r| r.get(0)).unwrap_or(false);
        if !eligible {
            self.peer_tailers
                .lock()
                .unwrap()
                .remove(&(s.id.clone(), s.gen));
            return;
        }
        let Some(path) = self.transcript_path(s) else {
            return;
        };
        let facts = self
            .peer_tailers
            .lock()
            .unwrap()
            .entry((s.id.clone(), s.gen))
            .or_insert_with(|| super::peer_log::Tailer::at(&path, 0))
            .read(&path, true)
            .unwrap_or_default();
        if facts.is_empty() {
            return;
        }
        let (machine, agent, key) = (
            self.db.machine_id().to_string(),
            s.id.clone(),
            self.header_key.clone(),
        );
        self.mail_write(s, move |tx| {
            for fact in facts {
                match fact {
                    MailFact::Enqueued {id} => {
                        let ours: bool = tx.query_row("SELECT count(*) > 0 FROM messages WHERE id = ?1 AND to_agent = ?2 AND via = 'socket' AND state = 'delivering'", rusqlite::params![id, agent], |r| r.get(0))?;
                        if ours { mail::note(tx, &machine, &id, "cc enqueued", now_ms())?; }
                    }
                    MailFact::PeerEntry {wire_id, ..} => {
                        let id: Option<String> = tx.query_row("SELECT id FROM messages WHERE wire_id = ?1 AND to_agent = ?2 AND via = 'socket'", rusqlite::params![wire_id, agent], |r| r.get(0)).optional()?;
                        if let Some(id) = id { mail::evidence(tx, &machine, &agent, &id, "peer_jsonl", now_ms())?; }
                    }
                    MailFact::UserEntry {headers} => { if let Some(k) = &key {
                        mail::confirm_user_entry(tx, &machine, &agent, &headers, k, now_ms())?;
                    } }
                }
            }
            Ok(())
        });
    }
    fn drain_delivery(&self, s: &Start, delivery: &Delivery) {
        match delivery {
            Delivery::SocketSending { id, .. } => {
                let result = self
                    .socket_jobs
                    .lock()
                    .unwrap()
                    .remove(id)
                    .and_then(|rx| rx.recv_timeout(super::cc_inbox::WRITE_TIMEOUT).ok());
                match result {
                    Some(
                        super::cc_inbox::SocketOutcome::NotSent(_)
                        | super::cc_inbox::SocketOutcome::Blocked,
                    ) => self.requeue_mail(s, id, "drain"),
                    _ => {} // preserve in-flight row; drain_now settles as interrupted.
                }
            }
            Delivery::SocketAwaiting { .. } => {}
            _ => self.stop_delivery(s, delivery, "drain before submit", "drain"),
        }
        self.poll_peer_log(s);
    }

    /// §4.3: the engine does not drain, the screen is ready, the merge
    /// says `ready` or nothing yet (a new agent's first idle screen comes
    /// before `run` and claims nothing), and the input box is free.
    fn may_deliver(&self, s: &Start, screen: &CcScreen, text: &str) -> bool {
        let merged = self
            .merge
            .peek(&s.id, s.gen, |m| m.row_state(now_ms()))
            .flatten();
        !self.drain.on()
            && screen.ready_now()
            && matches!(merged, None | Some("ready"))
            && box_is_free(&input_box(text))
    }

    /// The supervisor stops with `delivery` in flight: a paste not yet
    /// submitted goes back to the queue (`pasted`), a submitted one is
    /// `uncertain` (`submitted`). Before the kill.
    fn stop_delivery(&self, s: &Start, delivery: &Delivery, pasted: &str, submitted: &str) {
        match delivery {
            Delivery::Idle | Delivery::PasteHeld { .. } => {}
            Delivery::Pasted { id, .. } => self.requeue_mail(s, id, pasted),
            Delivery::Submitted { id, .. } => self.mail_uncertain(s, id, submitted),
            Delivery::SocketSending { id, .. } => {
                self.socket_jobs.lock().unwrap().remove(id);
                self.mail_uncertain(s, id, "stopped while sending");
            }
            Delivery::SocketAwaiting { id, .. } => self.mail_uncertain(s, id, submitted),
        }
    }

    /// Run a mail write; a failed one only logs, and the message stays
    /// as the store has it.
    fn mail_write<R, F>(&self, s: &Start, f: F) -> Option<R>
    where
        F: FnOnce(&mut Tx) -> rusqlite::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        match self.db.write_blocking(f) {
            Ok((r, _)) => Some(r),
            Err(e) => {
                eprintln!("[engine] Failed to record mail to {}: {}", s.id, e);
                None
            }
        }
    }

    fn requeue_mail(&self, s: &Start, msg_id: &str, why: &str) {
        let (machine, id, why) = (
            self.db.machine_id().to_string(),
            msg_id.to_string(),
            why.to_string(),
        );
        self.mail_write(s, move |tx| {
            mail::requeue(tx, &machine, &id, &why, now_ms())
        });
    }

    fn mark_mail(&self, s: &Start, msg_id: &str, state: MsgState, why: &str) {
        let (machine, id, why) = (
            self.db.machine_id().to_string(),
            msg_id.to_string(),
            why.to_string(),
        );
        self.mail_write(s, move |tx| {
            mail::mark(tx, &machine, &id, state, Some(&why), now_ms())
        });
    }

    /// `msg_id` may have reached the agent: `uncertain`, and the agent
    /// needs attention (`mail_uncertain: <id>`), raised in the merge and
    /// emitted as `agent.attention`.
    fn mail_uncertain(&self, s: &Start, msg_id: &str, why: &str) {
        let now = now_ms();
        let reason = format!("mail_uncertain: {}", msg_id);
        self.merge.peek(&s.id, s.gen, |m| m.attention(&reason, now));
        let (machine, agent, gen, id, why) = (
            self.db.machine_id().to_string(),
            s.id.clone(),
            s.gen,
            msg_id.to_string(),
            why.to_string(),
        );
        self.mail_write(s, move |tx| {
            if mail::mark(tx, &machine, &id, MsgState::Uncertain, Some(&why), now)? {
                tx.emit(&NewEvent {
                    topic: "agent.attention".into(),
                    subject: Some(format!("{}/{}", machine, agent)),
                    origin: None,
                    data: serde_json::to_value(crate::engine::proto::AgentAttentionEvent {
                        id: agent.to_string(),
                        generation: gen,
                        reason: reason.to_string(),
                        msg_id: Some(id.to_string()),
                        tool_use_id: None,
                    })
                    .expect("AgentAttentionEvent serializes"),
                })?;
            }
            Ok(())
        });
    }

    /// `Cmd::Drain`: SIGTERM first (`Driver::kill`), then `drain::settle`
    /// with the state read before the kill, so a Stop hook that CC sends
    /// on its way out does not turn a cut turn into `paused` (reviews L1).
    /// A kill that fails settles nothing: the row and its `agent_state`
    /// stay for boot reconcile, which kills the process by pid and start
    /// time before anything resumes it (reviews M1).
    fn drain_now(&self, s: &Start, mut driver: Box<dyn Driver>) {
        let mut before = drain::state_of(&self.db, &s.id).unwrap_or_else(|e| {
            eprintln!("[engine] Drain: {}", e);
            None
        });
        if let Ok(c) = self.db.pooled_reader() {
            if let Ok(Some(id)) = mail::socket_inflight(&c, &s.id) {
                before = Some("working".into());
                self.mail_uncertain(s, &id, "drain");
            }
        }
        let (machine, id) = (self.db.machine_id().to_string(), s.id.clone());
        if let Err(e) = driver.kill() {
            let why = e.to_string();
            eprintln!(
                "[engine] Drain: failed to kill {}; left to boot reconcile: {}",
                s.id, why
            );
            let wrote = self
                .db
                .write_blocking(move |tx| drain::audit_kill_failed(tx, &id, &why));
            if let Err(e) = wrote {
                eprintln!(
                    "[engine] Failed to audit the failed kill of {}: {}",
                    s.id, e
                );
            }
            return;
        }
        let wrote = self
            .db
            .write_blocking(move |tx| drain::settle(tx, &machine, &id, before.as_deref()));
        if let Err(e) = wrote {
            eprintln!("[engine] Failed to record the drain of {}: {}", s.id, e);
        }
        remove_settings(&self.config.home, &s.id, s.gen);
    }

    /// A screen state change of a ready agent, into the merge as a screen
    /// claim. Trust, other dialogs and no title claim nothing: the last
    /// claim stands.
    fn screen_claim(&self, s: &Start, st: ScreenState) {
        let activity = match st {
            ScreenState::Working => Activity::Working,
            ScreenState::Idle => Activity::Ready,
            ScreenState::Blocked => Activity::Blocked,
            ScreenState::Trust | ScreenState::Dialog | ScreenState::Unknown => return,
        };
        let (machine, id, gen, merge) = (
            self.db.machine_id().to_string(),
            s.id.clone(),
            s.gen,
            self.merge.clone(),
        );
        let wrote = self.db.write_blocking(move |tx| {
            hooks::screen_claim(tx, &machine, &merge, &id, gen, activity, now_ms())
        });
        if let Err(e) = wrote {
            eprintln!("[engine] Failed to record the screen of {}: {}", s.id, e);
        }
    }

    /// Write the merge's state when a hold ran out with no new claim. The
    /// check is in memory; the write runs only when the state changes.
    fn expire_hold(&self, s: &Start) {
        let now = now_ms();
        if self
            .merge
            .peek(&s.id, s.gen, |m| m.stale_row_state(now))
            .flatten()
            .is_none()
        {
            return;
        }
        let (machine, id, gen, merge) = (
            self.db.machine_id().to_string(),
            s.id.clone(),
            s.gen,
            self.merge.clone(),
        );
        let wrote = self
            .db
            .write_blocking(move |tx| hooks::expire_hold(tx, &machine, &merge, &id, gen, now));
        if let Err(e) = wrote {
            eprintln!("[engine] Failed to settle the state of {}: {}", s.id, e);
        }
    }

    /// Send `keys` to the agent's terminal, unless the engine drains: then
    /// nothing is sent, the block is audited with path `pty`, and the
    /// result is `Blocked`.
    fn pty_write(
        &self,
        id: &str,
        driver: &mut dyn Driver,
        keys: &[u8],
    ) -> Result<(), PtyWriteError> {
        if !self.drain.on() {
            return driver
                .write(keys)
                .map_err(|e| PtyWriteError::Io(e.kind(), e.to_string()));
        }
        let id2 = id.to_string();
        let wrote = self.db.write_blocking(move |tx| {
            drain::audit_blocked(tx, audit::SYSTEM, &id2, InjectionPath::Pty, now_ms())
        });
        if let Err(e) = wrote {
            eprintln!(
                "[engine] Failed to audit a blocked PTY write to {}: {}",
                id, e
            );
        }
        Err(PtyWriteError::Blocked)
    }

    /// Whether the create op of `s` is still open: a retire closes it.
    fn op_open(&self, s: &Start) -> Result<bool, String> {
        let conn = self.db.pooled_reader()?;
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

    /// The create or resume failed: close the op, fail the row with
    /// `spawn_failed`, or put a resume's row back with `resume_failed`
    /// (`ops::resume_failed`). A failed write only logs; boot
    /// reconcile finishes it.
    fn fail_create(&self, s: &Start, why: &str) {
        let (machine, s2, why2) = (self.db.machine_id().to_string(), s.clone(), why.to_string());
        let (method, reason) = s.kind.failure();
        let wrote = self.db.write_blocking(move |tx| {
            let now = now_ms();
            ops::finish(tx, &s2.op_id, reason, now)?;
            // A failed resume can be tried again (D4).
            if s2.kind == Kind::Resume {
                return ops::resume_failed(tx, &machine, &s2.id, s2.from.as_deref(), &why2, now);
            }
            ops::fail_agent(tx, &machine, &s2.id, method, reason, &why2, now)
        });
        if let Err(e) = wrote {
            eprintln!(
                "[engine] Failed to record the failed create of {}: {}",
                s.id, e
            );
        }
        remove_settings(&self.config.home, &s.id, s.gen);
    }

    /// A resume that the engine's drain or stop cut before `ready`, with
    /// no spawn or a killed one: close its op and put the row back
    /// (`undo_resume`), so the next boot resumes it.
    fn drop_resume(&self, s: &Start) {
        let (machine, s2) = (self.db.machine_id().to_string(), s.clone());
        let outcome = if self.drain.on() {
            "draining"
        } else {
            "stopped"
        };
        let wrote = self
            .db
            .write_blocking(move |tx| undo_resume(tx, &machine, &s2, outcome));
        if let Err(e) = wrote {
            eprintln!("[engine] Failed to close the resume op of {}: {}", s.id, e);
        }
        remove_settings(&self.config.home, &s.id, s.gen);
    }

    /// Resume every `paused(drain)` and `interrupted` engine agent (see
    /// the module doc). Run once at boot, after boot reconcile and the
    /// bind. Blocking: the supervisors bring the agents up on their own
    /// threads.
    /// Kill the agent of `s` and confirm that it ended (D2). When no kill
    /// can confirm it, the row becomes `orphaned` (`ops::orphan_agent`,
    /// audited with `principal` and `method`) and this returns `false`.
    fn kill_confirmed(
        &self,
        s: &Start,
        driver: &mut dyn Driver,
        principal: &str,
        method: &'static str,
    ) -> bool {
        let child = driver.child();
        let Err(e) = driver.kill() else {
            return true;
        };
        eprintln!(
            "[engine] Could not confirm the kill of {} (pid {}); marking it orphaned: {}",
            s.id, child.pid, e
        );
        let (machine, id, gen) = (self.db.machine_id().to_string(), s.id.clone(), s.gen);
        let (principal, e) = (principal.to_string(), e.to_string());
        let wrote = self.db.write_blocking(move |tx| {
            ops::orphan_agent(
                tx,
                &machine,
                &id,
                gen,
                child,
                &principal,
                method,
                &e,
                now_ms(),
            )
        });
        if let Err(e) = wrote {
            eprintln!("[engine] Failed to mark {} orphaned: {}", s.id, e);
        }
        false
    }

    /// `agent.retire` of a running agent: the tombstone once the kill is
    /// confirmed, else the `orphaned` row.
    fn retire_killed(
        &self,
        s: &Start,
        driver: &mut dyn Driver,
        principal: &str,
    ) -> Result<AgentRow, ApiError> {
        if self.kill_confirmed(s, driver, principal, "agent.retire") {
            return self.retire_row(&s.id, s.gen, principal);
        }
        let conn = self.db.pooled_reader().map_err(internal)?;
        engine_row(&conn, &s.id)
            .map_err(internal)?
            .ok_or_else(|| unknown_agent(&s.id))
    }

    /// A retire made the row a tombstone while the agent came up: the PTY
    /// goes. A kill that cannot confirm it turns the tombstone `orphaned`.
    fn retired_meanwhile(&self, s: &Start, mut driver: Box<dyn Driver>) -> Up {
        self.kill_confirmed(s, driver.as_mut(), audit::SYSTEM, "agent.retire");
        Up::Retired
    }

    /// `agent.retire` with no supervisor: the tombstone, or for an
    /// `orphaned` agent first a new kill (`retire_orphan`, D2). The state
    /// is read here, after `retire_live`: a supervisor that could not
    /// confirm its kill wrote `orphaned` before it left. Blocking.
    fn retire_idle(&self, id: &str, gen: i64, principal: &str) -> Result<AgentRow, ApiError> {
        let state = {
            let conn = self.db.pooled_reader().map_err(internal)?;
            engine_row(&conn, id)
                .map_err(internal)?
                .and_then(|r| r.state)
        };
        if state.as_deref() == Some("orphaned") {
            return self.retire_orphan(id, principal);
        }
        self.retire_row(id, gen, principal)
    }

    /// `agent.retire` of an `orphaned` agent: kill its recorded PTY child
    /// again, by pid and start time. Confirmed gone (or the pid now runs
    /// another process), the row becomes the tombstone. Otherwise it stays
    /// `orphaned`, the attempt is audited, and the row is returned as it
    /// is. Blocking.
    fn retire_orphan(&self, id: &str, principal: &str) -> Result<AgentRow, ApiError> {
        let (child, row) = {
            let conn = self.db.pooled_reader().map_err(internal)?;
            let child = conn
                .query_row(
                    "SELECT gen, pty_child_pid, pty_child_start FROM agent_state                      WHERE agent_id = ?1",
                    [id],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, Option<i64>>(1)?,
                            r.get::<_, Option<i64>>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(internal)?;
            let row = engine_row(&conn, id)
                .map_err(internal)?
                .ok_or_else(|| unknown_agent(id))?;
            (child, row)
        };
        let child = child.and_then(|(_, pid, start)| {
            Some(super::driver::ChildId {
                pid: pid? as libc::pid_t,
                start: start? as u64,
            })
        });
        if let Some(child) = child {
            let killer = lock(&self.orphan_killer).clone();
            if let Err(e) = killer(child) {
                let (id2, principal, why) = (
                    id.to_string(),
                    principal.to_string(),
                    format!(
                        "{}: pid {} start {}: {}",
                        ops::KILL_UNCONFIRMED,
                        child.pid,
                        child.start,
                        e
                    ),
                );
                self.db
                    .write_blocking(move |tx| {
                        audit::append(
                            tx,
                            now_ms(),
                            &AuditEntry {
                                principal: &principal,
                                method: "agent.retire",
                                target: Some(&id2),
                                decision: ops::ORPHANED,
                                reason: Some(&why),
                                ..Default::default()
                            },
                        )
                    })
                    .map_err(internal)?;
                return Ok(row);
            }
        }
        self.retire_row(id, row.generation, principal)
    }

    /// `agent.resume` (D4): resume a `paused` or `interrupted` agent now,
    /// also one whose last resume failed (`resume_failed`). The same steps
    /// as a boot resume; answers once the agent is `ready`, or with the
    /// failure, which puts the row back with `resume_failed` so it can be
    /// tried again. An agent with a supervisor or an open op is refused:
    /// one session never runs twice.
    pub async fn resume(
        self: &Arc<Self>,
        principal: &Principal,
        params: serde_json::Value,
    ) -> Result<AgentRow, ApiError> {
        local_only(principal, "agent.resume")?;
        let p: AgentResumeParams = parse(params, "agent.resume")?;
        if self.drain.on() {
            return Err(draining());
        }
        if self.stopping.load(Ordering::SeqCst) {
            return Err(stopping());
        }
        let not_resumable = |why: String| {
            ApiError::new(
                ErrorCode::ActionFailed,
                format!("{} cannot be resumed: {}", p.id, why),
            )
        };
        if lock(&self.live).contains_key(&p.id) {
            return Err(not_resumable("it runs in this engine".into()));
        }
        let (sp, id) = (self.clone(), p.id.clone());
        let r = blocking(move || {
            let conn = sp.db.pooled_reader().map_err(internal)?;
            let row = engine_row(&conn, &id)
                .map_err(internal)?
                .ok_or_else(|| unknown_agent(&id))?;
            let r = sp.resumables(Some(&id)).map_err(internal)?.pop();
            Ok((row, r))
        })
        .await?;
        let (row, r) = r;
        let Some(r) = r else {
            let state = row.state.as_deref().unwrap_or("none");
            let why = match state {
                "orphaned" => "it is orphaned: its process may still run".to_string(),
                _ => format!("it is {}, not paused or interrupted", state),
            };
            return Err(not_resumable(why));
        };
        let sp = self.clone();
        let r2 = r.clone();
        let start = blocking(move || sp.resume_plan(&r2).map_err(not_resumable_plain)).await?;
        let reason = if r.reason.as_deref() == Some(drain::REASON_DRAIN) {
            GenReason::EngineRestart
        } else {
            GenReason::Resume
        };
        let (machine, s2, state, who) = (
            self.db.machine_id().to_string(),
            start.clone(),
            r.state.clone(),
            principal.to_string(),
        );
        let (gen, _) = self
            .db
            .write(move |tx| write_resume(tx, &machine, &s2, &state, reason, &who))
            .await
            .map_err(internal)?;
        let Some(gen) = gen else {
            return Err(not_resumable(
                "its state changed, or a create or resume of it is open".into(),
            ));
        };
        remove_settings(&self.config.home, &r.id, gen - 1);
        let (reply, answer) = oneshot::channel();
        self.supervise(Start { gen, ..start }, reply)?;
        answer
            .await
            .map_err(|_| internal("the agent's supervisor ended without an answer"))?
    }

    pub fn resume_on_boot(self: &Arc<Self>) -> Result<Resumed, String> {
        let mut done = Resumed::default();
        for r in self.resumables(None)? {
            if self.drain.on() {
                done.left.push(r.id);
                continue;
            }
            let start = match self.resume_plan(&r) {
                Ok(s) => s,
                Err(why) => {
                    let (machine, id) = (self.db.machine_id().to_string(), r.id.clone());
                    let wrote = self.db.write_blocking(move |tx| {
                        ops::fail_agent(
                            tx,
                            &machine,
                            &id,
                            ops::KIND_RESUME,
                            ops::RESUME_FAILED,
                            &why,
                            now_ms(),
                        )
                    });
                    // One failed write does not stop the other resumes
                    // (reviews L3).
                    match wrote {
                        Ok(_) => done.failed.push(r.id),
                        Err(e) => {
                            eprintln!("[engine] Failed to fail the resume of {}: {}", r.id, e);
                            done.left.push(r.id);
                        }
                    }
                    continue;
                }
            };
            let reason = if r.reason.as_deref() == Some(drain::REASON_DRAIN) {
                GenReason::EngineRestart
            } else {
                GenReason::Resume
            };
            let (machine, s2, state) = (
                self.db.machine_id().to_string(),
                start.clone(),
                r.state.clone(),
            );
            let (gen, _) = self.db.write_blocking(move |tx| {
                write_resume(tx, &machine, &s2, &state, reason, audit::SYSTEM)
            })?;
            let Some(gen) = gen else {
                continue;
            };
            // The old generation's settings file: a crash left it, and
            // nothing else removes it (reviews L3).
            remove_settings(&self.config.home, &r.id, gen - 1);
            // Nobody waits for a resume's answer.
            let (reply, _) = oneshot::channel();
            match self.supervise(Start { gen, ..start }, reply) {
                Ok(()) => done.started.push(r.id),
                Err(e) if e.code == ErrorCode::Draining => done.left.push(r.id),
                Err(_) => done.failed.push(r.id),
            }
        }
        Ok(done)
    }

    /// The engine agents a boot resume picks up, by id: `paused(drain)`
    /// and `interrupted`, except a failed resume's (D4: only `agent.resume`
    /// tries it again). With `only`, that agent if it is `paused` or
    /// `interrupted` for any reason (`agent.resume`).
    fn resumables(&self, only: Option<&str>) -> Result<Vec<Resumable>, String> {
        let conn = self.db.pooled_reader()?;
        let fail = |e: rusqlite::Error| format!("Failed to read the agents to resume: {}", e);
        let mut st = conn
            .prepare(
                "SELECT id, state, state_reason, provider, provider_sid, cwd, resume_argv \
                 FROM agents WHERE source = 'engine' AND control = 'pty' AND gone_at IS NULL \
                 AND CASE WHEN ?3 IS NULL \
                 THEN (state = 'paused' AND state_reason = ?1) \
                 OR (state = 'interrupted' AND state_reason IS NOT ?2) \
                 ELSE id = ?3 AND state IN ('paused', 'interrupted') END \
                 ORDER BY id",
            )
            .map_err(fail)?;
        let rows = st
            .query_map(
                params![drain::REASON_DRAIN, ops::RESUME_FAILED, only],
                |r| {
                    Ok(Resumable {
                        id: r.get(0)?,
                        state: r.get(1)?,
                        reason: r.get(2)?,
                        provider: r.get(3)?,
                        sid: r.get(4)?,
                        cwd: r.get(5)?,
                        argv: r.get(6)?,
                    })
                },
            )
            .map_err(fail)?;
        rows.collect::<rusqlite::Result<_>>().map_err(fail)
    }

    /// The session, cwd and `claude` of a resume, or why there is none.
    /// Blocking.
    fn resume_plan(&self, r: &Resumable) -> Result<Start, String> {
        // The drain understands more descriptors than this CC supervisor.
        // Never resume a different CLI with Claude's executable/settings.
        if adapter::descriptor_for_provider(&r.provider).is_none_or(|d| d.kind != "claude") {
            return Err(format!("no spawn integration for a {} agent", r.provider));
        }
        let argv: Vec<String> = match (&r.argv, &r.sid) {
            (Some(a), _) => serde_json::from_str(a)
                .map_err(|e| format!("resume_argv {:?} is not an argv: {}", a, e))?,
            (None, Some(sid)) => drain::resume_argv(&r.provider, sid)
                .ok_or_else(|| format!("no resume for a {} agent", r.provider))?,
            (None, None) => return Err("no session to resume".into()),
        };
        // Only the adapter's own form: `claude --resume <id>`.
        let sid = match argv.get(2) {
            Some(sid) if drain::resume_argv(&r.provider, sid).as_ref() == Some(&argv) => {
                sid.clone()
            }
            _ => {
                return Err(format!(
                    "resume_argv {:?} is not a resume of a {} agent",
                    argv, r.provider
                ))
            }
        };
        let cwd = PathBuf::from(r.cwd.as_deref().ok_or("the agent has no cwd")?);
        let real = resolve_cwd(&cwd)?;
        if real != cwd {
            return Err(format!("cwd {:?} now resolves to {:?}", cwd, real));
        }
        check_collision(&self.config.home, &cwd).map_err(|e| e.msg)?;
        self.agent_bin.check(Rehash::Always).map_err(|e| e.msg)?;
        let claude = match &self.config.claude {
            Some(c) => c.clone(),
            None => find_on_path("claude")
                .ok_or("claude is not on the engine's PATH; nothing was spawned")?,
        };
        Ok(Start {
            id: r.id.clone(),
            gen: 0,
            sid,
            op_id: ops::new_op_id(),
            cwd,
            claude,
            kind: Kind::Resume,
            from: Some(r.state.clone()),
        })
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
                let conn = db.pooled_reader().map_err(internal)?;
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
        let r = blocking(move || sp.retire_idle(&id, row.generation, &principal_s)).await;
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
    // A create comes from `creating`, a resume from `paused` or
    // `interrupted`.
    tx.execute(
        "UPDATE agents SET state = 'starting', pid = ?2 WHERE id = ?1 \
         AND state IN ('creating', 'paused', 'interrupted')",
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
    // A resume's drain reason and argv are spent.
    tx.execute(
        "UPDATE agents SET state = 'ready', state_reason = NULL, resume_argv = NULL \
         WHERE id = ?1 AND state = 'starting'",
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

/// Phase `row` of a resume: the next generation on the same session and
/// transcript, the resume op, the audit row (by `principal`). `None` when
/// the row is no longer `state` (a retire came first), or a create or
/// resume op of the agent is still open.
fn write_resume(
    tx: &mut Tx,
    machine: &str,
    s: &Start,
    state: &str,
    reason: GenReason,
    principal: &str,
) -> rusqlite::Result<Option<i64>> {
    let now = now_ms();
    // One create or resume of an agent at a time: two would run one
    // session twice.
    let open: i64 = tx.query_row(
        "SELECT count(*) FROM ops WHERE kind IN (?1, ?2) AND done_at IS NULL \
         AND json_extract(payload, '$.agentId') = ?3",
        params![ops::KIND_CREATE, ops::KIND_RESUME, s.id],
        |r| r.get(0),
    )?;
    if open > 0 {
        return Ok(None);
    }
    let old: Option<i64> = tx
        .query_row(
            "SELECT generation FROM agents WHERE id = ?1 AND source = 'engine' \
             AND gone_at IS NULL AND state = ?2",
            params![s.id, state],
            |r| r.get(0),
        )
        .optional()?;
    let Some(old) = old else {
        return Ok(None);
    };
    let transcript = generation::transcript_path(tx, &s.id, old)?;
    let gen = begin_generation(
        tx,
        machine,
        &s.id,
        reason,
        &s.sid,
        transcript.as_deref(),
        now,
    )?;
    let cwd = s.cwd.display().to_string();
    let payload = CreatePayload {
        agent_id: s.id.clone(),
        generation: gen,
        session_id: s.sid.clone(),
        cwd,
        ..Default::default()
    };
    ops::open_resume(tx, &s.op_id, &payload, now)?;
    let why = format!(
        "{} from {}, generation {}",
        match reason {
            GenReason::EngineRestart => "engine_restart",
            _ => "resume",
        },
        state,
        gen
    );
    audit::append(
        tx,
        now,
        &AuditEntry {
            principal,
            method: ops::KIND_RESUME,
            target: Some(&s.id),
            decision: "allow",
            reason: Some(&why),
            ..Default::default()
        },
    )?;
    Ok(Some(gen))
}

/// Undo a resume cut before `ready` (see `Spawner::drop_resume`): the op
/// closes as `outcome`, the row goes back to the state it came from with
/// reason `drain`, so the next resume is an `engine_restart` that does not
/// count, its `resume_argv` kept and, from `interrupted`, the note pending
/// again; no PTY child is left in `agent_state`. A row that left
/// `starting` meanwhile (a retire) is not touched.
fn undo_resume(tx: &mut Tx, machine: &str, s: &Start, outcome: &str) -> rusqlite::Result<()> {
    ops::finish(tx, &s.op_id, outcome, now_ms())?;
    let Some(from) = s.from.as_deref() else {
        return Ok(());
    };
    let n = tx.execute(
        "UPDATE agents SET state = ?2, state_reason = ?3, \
         interrupted_note_pending = CASE WHEN ?2 = 'interrupted' THEN 1 \
         ELSE interrupted_note_pending END \
         WHERE id = ?1 AND gone_at IS NULL AND state IN ('starting', ?2)",
        params![s.id, from, drain::REASON_DRAIN],
    )?;
    if n == 0 {
        return Ok(());
    }
    tx.execute("DELETE FROM agent_state WHERE agent_id = ?1", [&s.id])?;
    emit_upsert(tx, machine, &s.id)
}

/// The retire transaction: the tombstone (`retired`, `gone_at`), no PTY
/// child, a create or resume op still open closed as `retired`, the audit row and
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
        // Its mail goes with it (C1b-M1 §4.8).
        mail::on_agent_end(tx, machine, id, "target retired", now)?;
        let open: Vec<String> = {
            let mut st = tx.prepare(
                "SELECT id FROM ops WHERE kind IN (?1, ?2) AND done_at IS NULL \
                 AND json_extract(payload, '$.agentId') = ?3",
            )?;
            let rows = st.query_map(params![ops::KIND_CREATE, ops::KIND_RESUME, id], |r| {
                r.get(0)
            })?;
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

/// The cwds of the live engine agents other than `id`: the peers whose
/// `.claude/` the profile of `id` denies (C1a-32). Only agents that exist
/// at this spawn: one created later is not in this agent's rules.
fn peer_cwds(conn: &Connection, id: &str) -> rusqlite::Result<Vec<PathBuf>> {
    let mut st = conn.prepare(
        "SELECT cwd FROM agents WHERE source = 'engine' AND gone_at IS NULL \
         AND cwd IS NOT NULL AND id != ?1",
    )?;
    let rows = st.query_map([id], |r| r.get::<_, String>(0))?;
    rows.map(|r| r.map(PathBuf::from)).collect()
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

/// A live engine agent whose cwd differs from `cwd`, as `(id, cwd)` (M7
/// reviews H1). An agent's rules deny the `.claude/` of only the agents
/// live at its spawn (`peer_cwds`), so an agent created earlier could
/// still write a later agent's settings, which CC hot-reloads. Until a
/// rule that names every live agent is verified live, `agent.create`
/// takes one cwd at a time. The same cwd is fine: both deny it. A boot
/// resume reads every live row, so resumed agents deny each other. In the
/// create transaction, so two creates cannot race past it.
fn other_live_cwd(conn: &Connection, cwd: &Path) -> rusqlite::Result<Option<(String, PathBuf)>> {
    let mut st = conn.prepare(
        "SELECT id, cwd FROM agents WHERE source = 'engine' AND gone_at IS NULL \
         AND cwd IS NOT NULL AND cwd != ?1 ORDER BY created_at, id LIMIT 1",
    )?;
    st.query_row([cwd.display().to_string()], |r| {
        Ok((
            r.get::<_, String>(0)?,
            PathBuf::from(r.get::<_, String>(1)?),
        ))
    })
    .optional()
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
                ErrorCode::Unsupported,
                "managed agents come in C1b; use mode \"free\"",
            ))
        }
        other => return Err(bad(format!("unknown mode {:?}", other))),
    }
    let desc = adapter::descriptor(&p.kind)
        .ok_or_else(|| bad(format!("unknown agent kind {:?}", p.kind)))?;
    // Descriptor discovery must not route a new kind through the CC-only
    // settings, executable and screen supervisor below.
    if desc.kind != "claude" {
        return Err(bad(format!(
            "agent kind {:?} is recognized but its spawn integration is not implemented",
            p.kind
        )));
    }
    Ok(desc)
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

fn draining() -> ApiError {
    ApiError::new(
        ErrorCode::Draining,
        "the engine is draining: no spawn and no injection until the next engine is up",
    )
}

/// What a create or resume error adds when its kill was not confirmed.
const ORPHANED_TEXT: &str = "the kill could not be confirmed; the agent is orphaned";

fn not_resumable_plain(why: String) -> ApiError {
    ApiError::new(
        ErrorCode::ActionFailed,
        format!("cannot be resumed: {}", why),
    )
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
    use crate::engine::pty::Orphan;
    use std::collections::VecDeque;
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
        /// The workspace trust dialog, forever. The engine starts to drain
        /// right after the spawn, as if a SIGTERM came then, or at the
        /// first look at the screen with `drain_on_screen`.
        Trust,
    }

    struct FakeDriver {
        script: Script,
        child: ChildId,
        exited: bool,
        kills: Arc<AtomicUsize>,
        /// Set by a test: the process exits by itself.
        exit_now: Arc<AtomicBool>,
        /// Bytes sent to the terminal, in writes.
        writes: Arc<AtomicUsize>,
        /// Every write's bytes, failed ones included, in order.
        sent: Arc<Mutex<Vec<Vec<u8>>>>,
        /// Set by a test: the results of the next writes, in order. `None`
        /// is `Ok`; when empty, every write succeeds.
        write_results: Arc<Mutex<VecDeque<Option<io::ErrorKind>>>>,
        /// Set by a test: a whole screen text per agent id, in place of
        /// the script's.
        screens: Arc<Mutex<HashMap<String, String>>>,
        /// What a submitted prompt (CR on a non-empty box) carried.
        prompts: Arc<Mutex<Vec<String>>>,
        /// `Script::Ready`'s input box. `None` until the first prompt shows
        /// CC's placeholder; after it, an empty box.
        input: FakeInput,
        /// Set by a test: `kill` does not return until it is cleared.
        stuck: Arc<AtomicBool>,
        /// The agent id, from the spawn's env.
        id: String,
        /// Set by a test: the agents whose screen shows a turn (CC's busy
        /// title).
        busy: Arc<Mutex<HashSet<String>>>,
        /// Set by a test: `kill` fails, as when a process survives SIGKILL.
        kill_fails: Arc<AtomicBool>,
        /// Starts the drain at the first look at the screen.
        drain_on_screen: Option<Arc<DrainSwitch>>,
    }

    /// The input box of a fake CC (`Script::Ready`), after the real CC
    /// 2.1.314 layouts in `tests/fixtures/cc_screens/`.
    #[derive(Default)]
    struct FakeInput {
        /// What the box holds now: typed text, a one-line paste, or the
        /// `[Pasted text #n +k lines]` of a longer one.
        text: String,
        /// The full text behind `text`, as CC submits it.
        full: String,
        /// A prompt was submitted: the placeholder is gone.
        submitted: bool,
        /// Pastes so far, for the `#n` of the collapsed form.
        pastes: usize,
    }

    impl FakeInput {
        /// Apply one successful write. A bracketed paste fills the box
        /// (collapsed when it has a newline), CR on a non-empty box
        /// submits it and returns the prompt, other escape sequences are
        /// keys that change nothing, and other bytes are typed.
        fn apply(&mut self, bytes: &[u8]) -> Option<String> {
            let text = String::from_utf8_lossy(bytes);
            if let Some(body) = text
                .strip_prefix("\x1b[200~")
                .and_then(|t| t.strip_suffix("\x1b[201~"))
            {
                self.pastes += 1;
                self.full.push_str(body);
                self.text = if body.contains('\n') {
                    format!(
                        "[Pasted text #{} +{} lines]",
                        self.pastes,
                        body.matches('\n').count()
                    )
                } else {
                    format!("{}{}", self.text, body)
                };
                None
            } else if bytes == crate::engine::cc_screen::KEY_CR {
                if self.full.is_empty() {
                    return None;
                }
                self.text.clear();
                self.submitted = true;
                Some(std::mem::take(&mut self.full))
            } else if bytes.first() == Some(&0x1b) {
                None
            } else {
                self.text.push_str(&text);
                self.full.push_str(&text);
                None
            }
        }

        /// The screen: chat, the box between two rules, the status line.
        fn screen(&self) -> String {
            let inside = if !self.text.is_empty() {
                self.text.as_str()
            } else if self.submitted {
                ""
            } else {
                "Try \"fix lint errors\""
            };
            let rule = "\u{2500}".repeat(40);
            format!(
                "\u{23fa} earlier output\n{}\n\u{276f} {}\n{}\n  \u{23f5}\u{23f5} auto mode on",
                rule, inside, rule
            )
        }
    }

    impl Driver for FakeDriver {
        fn spawn(_: &SpawnSpec) -> io::Result<Self> {
            Err(io::Error::other("use FakeLauncher"))
        }
        fn child(&self) -> ChildId {
            self.child
        }
        fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            lock(&self.sent).push(bytes.to_vec());
            if let Some(kind) = lock(&self.write_results).pop_front().flatten() {
                return Err(io::Error::new(kind, "fake write error"));
            }
            if let Some(prompt) = self.input.apply(bytes) {
                lock(&self.prompts).push(prompt);
                // CR submits: the title turns busy until the test ends
                // the turn.
                lock(&self.busy).insert(self.id.clone());
            }
            Ok(())
        }
        fn output(&mut self, _: Duration) -> Output {
            Output {
                eof: self.exited,
                ..Default::default()
            }
        }
        fn screen(&self) -> ScreenSnapshot {
            if let Some(d) = &self.drain_on_screen {
                d.start();
            }
            let busy = lock(&self.busy).contains(&self.id);
            let ready_screen = self.input.screen();
            let (title, text) = match self.script {
                Script::Ready if busy => ("\u{25d0} Claude Code", ready_screen.as_str()),
                Script::Ready => ("\u{2733} Claude Code", ready_screen.as_str()),
                Script::Refuse => (
                    "",
                    " \u{23bf}  Claude usage limit reached. Your limit resets 5pm\n",
                ),
                Script::Exit => ("", "Error: the session could not start\n"),
                Script::Hang => ("", ""),
                Script::Trust => (
                    "",
                    " Do you trust this folder?\n   1. Yes, I trust this folder\n   2. No, exit\n",
                ),
            };
            let text = lock(&self.screens)
                .get(&self.id)
                .cloned()
                .unwrap_or_else(|| text.to_string());
            ScreenSnapshot {
                text,
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
            if self.exit_now.load(Ordering::SeqCst) {
                self.exited = true;
            }
            Ok(self.exited.then(|| ExitStatus::from_raw(1 << 8)))
        }
        fn kill(&mut self) -> io::Result<KillReport> {
            while self.stuck.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            if self.kill_fails.load(Ordering::SeqCst) {
                return Err(io::Error::other("still alive after SIGKILL"));
            }
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
        exit_now: Arc<AtomicBool>,
        writes: Arc<AtomicUsize>,
        sent: Arc<Mutex<Vec<Vec<u8>>>>,
        write_results: Arc<Mutex<VecDeque<Option<io::ErrorKind>>>>,
        screens: Arc<Mutex<HashMap<String, String>>>,
        prompts: Arc<Mutex<Vec<String>>>,
        stuck: Arc<AtomicBool>,
        busy: Arc<Mutex<HashSet<String>>>,
        kill_fails: Arc<AtomicBool>,
        drain_on_screen: AtomicBool,
        env: Mutex<Vec<(OsString, OsString)>>,
        /// The argv of the last spawn.
        argv: Mutex<Vec<OsString>>,
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
            *lock(&self.argv) = spec.argv.clone();
            if self.script == Script::Ready {
                // The SessionStart hook, as `c9watch-agent` would send it.
                let id = env_of(spec, "C9_AGENT_ID").unwrap();
                let gen = env_of(spec, "C9_AGENT_GEN").unwrap().parse().unwrap();
                if let Some(sp) = self.spawner.get().and_then(Weak::upgrade) {
                    sp.session_started(&id, gen);
                }
            }
            let mut drain_on_screen = None;
            if self.script == Script::Trust {
                if let Some(sp) = self.spawner.get().and_then(Weak::upgrade) {
                    if self.drain_on_screen.load(Ordering::SeqCst) {
                        drain_on_screen = Some(sp.drain_switch().clone());
                    } else {
                        sp.drain_switch().start();
                    }
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
                exit_now: self.exit_now.clone(),
                writes: self.writes.clone(),
                sent: self.sent.clone(),
                write_results: self.write_results.clone(),
                screens: self.screens.clone(),
                prompts: self.prompts.clone(),
                input: FakeInput::default(),
                stuck: self.stuck.clone(),
                id: env_of(spec, "C9_AGENT_ID").unwrap_or_default(),
                busy: self.busy.clone(),
                kill_fails: self.kill_fails.clone(),
                drain_on_screen,
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

    fn setup(script: Script, clock: Option<Clock>) -> Env {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("engine home");
        let db = Arc::new(db::open(&home, MACHINE).unwrap());
        std::fs::write(t.path().join("agent-src"), b"#!/bin/sh\nexit 0\n").unwrap();
        let cwd = t.path().join("work");
        std::fs::create_dir(&cwd).unwrap();
        let (agents, launcher, sp) = engine_on(t.path(), &home, &db, script, clock);
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

    /// The next engine on the same home and store: a new live table, a
    /// new launcher and a new spawner, as after a restart.
    fn next_engine(
        e: &Env,
        script: Script,
        clock: Option<Clock>,
    ) -> (Arc<LiveTable>, Arc<FakeLauncher>, Arc<Spawner>) {
        engine_on(e.t.path(), &e.home, &e.db, script, clock)
    }

    fn engine_on(
        dir: &Path,
        home: &Path,
        db: &Arc<Db>,
        script: Script,
        clock: Option<Clock>,
    ) -> (Arc<LiveTable>, Arc<FakeLauncher>, Arc<Spawner>) {
        let bin = Arc::new(AgentBin::install(home, &dir.join("agent-src")));
        let agents = Arc::new(LiveTable::new());
        let launcher = Arc::new(FakeLauncher {
            script,
            spawns: AtomicUsize::new(0),
            next_pid: AtomicI32::new(2_000_000_000),
            kills: Arc::new(AtomicUsize::new(0)),
            exit_now: Arc::new(AtomicBool::new(false)),
            writes: Arc::new(AtomicUsize::new(0)),
            sent: Arc::new(Mutex::new(Vec::new())),
            write_results: Arc::new(Mutex::new(VecDeque::new())),
            screens: Arc::new(Mutex::new(HashMap::new())),
            prompts: Arc::new(Mutex::new(Vec::new())),
            stuck: Arc::new(AtomicBool::new(false)),
            busy: Arc::new(Mutex::new(HashSet::new())),
            kill_fails: Arc::new(AtomicBool::new(false)),
            drain_on_screen: AtomicBool::new(false),
            env: Mutex::new(Vec::new()),
            argv: Mutex::new(Vec::new()),
            spawner: OnceLock::new(),
        });
        let mut sp = Spawner::new(
            SpawnerConfig {
                home: home.to_path_buf(),
                sock: sock_path(home),
                claude: Some(PathBuf::from("/bin/false")),
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
        (agents, launcher, sp)
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
    async fn an_agent_may_not_create_or_retire() {
        let e = setup(Script::Ready, None);
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
    async fn descriptor_only_kinds_never_spawn_through_the_claude_supervisor() {
        let e = setup(Script::Ready, None);
        for desc in adapter::DESCRIPTORS.iter().filter(|d| d.kind != "claude") {
            let err =
                e.sp.create(
                    &Principal::LocalUnverified,
                    serde_json::json!({"mode": "free", "kind": desc.kind, "cwd": e.cwd}),
                )
                .await
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::BadRequest);
            assert!(err.msg.contains("spawn integration is not implemented"));
        }
        assert_eq!(e.launcher.spawns.load(Ordering::SeqCst), 0);
        assert!(e.db.agents(AgentFilter::All).unwrap().is_empty());
    }

    #[tokio::test]
    async fn bad_params_mode_kind_and_cwd_collisions_spawn_nothing() {
        let e = setup(Script::Ready, None);
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
                ErrorCode::Unsupported,
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
        let e = setup(Script::Refuse, None);
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
        let e = setup(Script::Exit, None);
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
        let e = setup(Script::Hang, Some(clock));
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
    async fn a_second_cwd_is_refused_while_an_agent_is_live() {
        // M7 reviews H1: A's rules, built at A's spawn, do not name B's
        // .claude/, so A could rewrite B's settings (live X14 row 8, the
        // other way round). One cwd at a time.
        let e = setup(Script::Ready, None);
        let l = Principal::LocalUnverified;
        let a = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
        let other = e.t.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let err = e.sp.create(&l, create_params(&other)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::CwdCollision);
        assert!(err.msg.contains(&a.id), "{}", err.msg);
        // The same cwd is fine: both agents deny its .claude/.
        let a2 = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
        let conn = e.db.reader().unwrap();
        assert!(peer_cwds(&conn, &a2.id)
            .unwrap()
            .iter()
            .all(|p| *p == e.cwd.canonicalize().unwrap()));
        drop(conn);
        // With no agent live, the other cwd is taken.
        for id in [&a.id, &a2.id] {
            e.sp.retire(&l, serde_json::json!({ "id": id }))
                .await
                .unwrap();
        }
        let b = e.sp.create(&l, create_params(&other)).await.unwrap();
        let conn = e.db.reader().unwrap();
        assert!(peer_cwds(&conn, &b.id).unwrap().is_empty());
        assert!(other_live_cwd(&conn, &other.canonicalize().unwrap())
            .unwrap()
            .is_none());
        assert_eq!(
            other_live_cwd(&conn, &e.cwd.canonicalize().unwrap())
                .unwrap()
                .map(|(id, _)| id),
            Some(b.id.clone())
        );
    }

    #[tokio::test]
    async fn create_walks_the_phases_and_retire_keeps_a_tombstone() {
        let e = setup(Script::Ready, None);
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
        let e = setup(Script::Ready, None);
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
        assert_eq!(done.interrupted, std::slice::from_ref(&r.id));
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
        let e = setup(Script::Ready, None);
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

    /// A ready agent that exits by itself ends `failed` with `exited`, and
    /// the audit names `agent.exit`, not `agent.create` (reviews).
    #[tokio::test]
    async fn a_ready_agent_that_exits_is_failed_as_an_exit() {
        let e = setup(Script::Ready, None);
        let r =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap();
        e.launcher.exit_now.store(true, Ordering::SeqCst);
        no_supervisors(&e.sp);
        assert_eq!(row(&e.db, &r.id).state.as_deref(), Some("failed"));
        let failed: Vec<(String, String)> = {
            let conn = e.db.reader().unwrap();
            let mut st = conn
                .prepare("SELECT method, reason FROM audit WHERE decision = 'failed'")
                .unwrap();
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            rows.map(Result::unwrap).collect()
        };
        assert_eq!(failed.len(), 1, "{:?}", failed);
        assert_eq!(failed[0].0, ops::METHOD_EXIT);
        assert!(failed[0].1.starts_with(EXITED), "{:?}", failed);
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
            kind: Kind::Create,
            from: None,
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
        let e = setup(Script::Ready, None);
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
        let e = setup(Script::Ready, None);
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
        let e = setup(Script::Ready, None);
        let s = Start {
            id: new_agent_id(),
            gen: 0,
            sid: uuid::Uuid::new_v4().to_string(),
            op_id: ops::new_op_id(),
            cwd: e.cwd.canonicalize().unwrap(),
            claude: PathBuf::from("/bin/false"),
            kind: Kind::Create,
            from: None,
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
        assert_eq!(done.failed, std::slice::from_ref(&r.id));
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
        assert_eq!(done.failed, std::slice::from_ref(&r.id));
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
        assert_eq!(done.interrupted, std::slice::from_ref(&r.id));
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

    /// A clock the test moves by hand, in whole seconds.
    fn hand_clock() -> (Clock, Arc<AtomicU64>) {
        let base = Instant::now();
        let secs = Arc::new(AtomicU64::new(0));
        let s = secs.clone();
        let clock: Clock = Arc::new(move || base + Duration::from_secs(s.load(Ordering::SeqCst)));
        (clock, secs)
    }

    fn set_state(db: &Db, id: &str, state: &str) {
        let (id, state) = (id.to_string(), state.to_string());
        db.write_blocking(move |tx| {
            tx.execute(
                "UPDATE agents SET state = ?2 WHERE id = ?1",
                params![id, state],
            )
        })
        .unwrap();
    }

    fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {}", what);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// (state, state_reason, resume_argv) of `id`.
    fn drained(db: &Db, id: &str) -> (Option<String>, Option<String>, Option<String>) {
        db.reader()
            .unwrap()
            .query_row(
                "SELECT state, state_reason, resume_argv FROM agents WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    /// What a drain leaves for `r`: `state`, reason `drain`, and the argv
    /// that resumes its session.
    fn drain_facts(r: &AgentRow, state: &str) -> (Option<String>, Option<String>, Option<String>) {
        let sid = r.provider_sid.clone().unwrap();
        let argv = serde_json::to_string(&["claude", "--resume", sid.as_str()]).unwrap();
        (
            Some(state.to_string()),
            Some(drain::REASON_DRAIN.to_string()),
            Some(argv),
        )
    }

    fn blocked_pty_writes(db: &Db) -> i64 {
        db.reader()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM audit WHERE method = ?1 AND reason = 'path=pty'",
                [drain::BLOCKED_INJECTION],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn fast_drain() -> drain::Timing {
        drain::Timing {
            tick: Duration::from_millis(5),
            ..Default::default()
        }
    }

    /// C1a-27, the state table on live supervisors: `ready` is paused at
    /// once, a turn that ends within 60 s is paused, a turn still running
    /// at 60 s is interrupted; rows without a supervisor are not touched;
    /// nothing is created meanwhile.
    #[tokio::test]
    async fn drain_pauses_ready_agents_and_interrupts_a_turn_at_60_s() {
        let (clock, secs) = hand_clock();
        let e = setup(Script::Ready, Some(clock));
        let l = Principal::LocalUnverified;
        let mut rows = Vec::new();
        for _ in 0..3 {
            rows.push(e.sp.create(&l, create_params(&e.cwd)).await.unwrap());
        }
        let (idle, ends, long) = (&rows[0], &rows[1], &rows[2]);
        set_state(&e.db, &ends.id, "working");
        set_state(&e.db, &long.id, "blocked");
        let others = ["paused", "interrupted", "failed", "retired", "orphaned"];
        e.db.write_blocking(move |tx| {
            for st in others {
                tx.execute(
                    "INSERT INTO agents (id, mode, role, control, provider, provider_sid, \
                     source, state) VALUES (?1, 'free', 'worker', 'pty', 'claudeCode', 's', \
                     'engine', ?1)",
                    [st],
                )?;
            }
            Ok(())
        })
        .unwrap();

        assert!(e.sp.drain_switch().start());
        let err = e.sp.create(&l, create_params(&e.cwd)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Draining);
        let sp = e.sp.clone();
        let start = sp.now();
        let drainer = std::thread::spawn(move || drain::run(&sp, fast_drain(), start));
        wait_for("the ready agent paused", || {
            drained(&e.db, &idle.id).0.as_deref() == Some("paused")
        });
        assert_eq!(drained(&e.db, &idle.id), drain_facts(idle, "paused"));
        // A turn that ends within the wait.
        secs.store(30, Ordering::SeqCst);
        set_state(&e.db, &ends.id, "ready");
        wait_for("the ended turn paused", || {
            drained(&e.db, &ends.id).0.as_deref() == Some("paused")
        });
        // At 59 s the long turn still runs.
        secs.store(59, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(row(&e.db, &long.id).state.as_deref(), Some("blocked"));
        assert_eq!(e.launcher.kills.load(Ordering::SeqCst), 2);
        assert!(!drainer.is_finished());
        secs.store(60, Ordering::SeqCst);
        let report = drainer.join().unwrap();

        let mut paused = vec![idle.id.clone(), ends.id.clone()];
        paused.sort();
        let mut got = report.paused.clone();
        got.sort();
        assert_eq!(got, paused);
        assert_eq!(report.interrupted, vec![long.id.clone()]);
        assert!(report.left.is_empty() && report.unsettled.is_empty());
        assert!(!report.timed_out);
        assert_eq!(drained(&e.db, &long.id), drain_facts(long, "interrupted"));
        assert!(row(&e.db, &long.id).interrupted_note_pending);
        assert!(!row(&e.db, &idle.id).interrupted_note_pending);
        assert_eq!(e.launcher.kills.load(Ordering::SeqCst), 3);
        no_supervisors(&e.sp);
        assert!(e.agents.is_empty());
        assert_eq!(agent_state_rows(&e.db), 0);
        for st in others {
            assert_eq!(drained(&e.db, st), (Some(st.to_string()), None, None));
        }
        // Nothing was spawned during the drain, and nothing is failed.
        assert_eq!(e.launcher.spawns.load(Ordering::SeqCst), 3);
        assert!(events(&e.db, "agent.failed").is_empty());
        // Boot reconcile leaves the drained rows for the resume.
        let done = ops::reconcile(&e.db, &e.home).unwrap();
        assert!(done.is_empty(), "{:?}", done);
    }

    /// The drain stops waiting at the hard deadline (90 s less the room
    /// for the final kills), even with a kill that hangs.
    #[tokio::test]
    async fn the_drain_gives_up_at_the_hard_deadline() {
        let (clock, secs) = hand_clock();
        let e = setup(Script::Ready, Some(clock));
        let r =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap();
        set_state(&e.db, &r.id, "working");
        e.launcher.stuck.store(true, Ordering::SeqCst);
        e.sp.drain_switch().start();
        let sp = e.sp.clone();
        let start = sp.now();
        let drainer = std::thread::spawn(move || drain::run(&sp, fast_drain(), start));
        secs.store(60, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        let last = (drain::HARD_DEADLINE - drain::SHUTDOWN_RESERVE).as_secs();
        secs.store(last - 1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        assert!(!drainer.is_finished());
        secs.store(last, Ordering::SeqCst);
        let report = drainer.join().unwrap();
        assert!(report.timed_out);
        assert_eq!(report.unsettled, vec![r.id.clone()]);
        assert!(report.paused.is_empty() && report.interrupted.is_empty());
        // The kill ends; the supervisor still settles the row.
        e.launcher.stuck.store(false, Ordering::SeqCst);
        no_supervisors(&e.sp);
        assert_eq!(drained(&e.db, &r.id), drain_facts(&r, "interrupted"));
        e.sp.shutdown();
    }

    /// C1b-M1-4: the fake CC's input box reads like the real one, a paste
    /// shows in it, and CR submits the full text and turns the title busy.
    #[test]
    fn fake_cc_draws_the_input_box_and_takes_a_paste() {
        use crate::engine::cc_screen::{input_box, paste_payload, InputBox, KEY_CR};
        let mut b = FakeInput::default();
        assert_eq!(input_box(&b.screen()), InputBox::Placeholder);
        assert_eq!(b.apply(KEY_CR), None, "CR on an empty box does nothing");
        assert_eq!(b.apply(&paste_payload("[c9watch mail msg_1]")), None);
        assert_eq!(
            input_box(&b.screen()),
            InputBox::Text("[c9watch mail msg_1]".into())
        );
        assert_eq!(b.apply(KEY_CR).as_deref(), Some("[c9watch mail msg_1]"));
        assert_eq!(input_box(&b.screen()), InputBox::Empty);
        b.apply(&paste_payload("a\nb\nc"));
        assert_eq!(
            input_box(&b.screen()),
            InputBox::Text("[Pasted text #2 +2 lines]".into())
        );
        assert_eq!(b.apply(b"\x1b[B"), None);
        assert_eq!(b.apply(KEY_CR).as_deref(), Some("a\nb\nc"));
        b.apply(b"hi");
        assert_eq!(input_box(&b.screen()), InputBox::Text("hi".into()));
    }

    /// C1b-M1-4: a test scripts the fake's write results and screen, and
    /// every write's bytes are kept.
    #[test]
    fn fake_driver_scripts_write_results_and_keeps_the_bytes() {
        let e = setup(Script::Hang, None);
        let spec = SpawnSpec::new(vec!["claude".into()], e.cwd.clone());
        let mut d = e.launcher.spawn(&spec).unwrap();
        lock(&e.launcher.write_results).extend([
            Some(io::ErrorKind::WouldBlock),
            None,
            Some(io::ErrorKind::TimedOut),
            Some(io::ErrorKind::BrokenPipe),
        ]);
        let kinds: Vec<Option<io::ErrorKind>> = (0..5)
            .map(|i| d.write(&[b'a' + i]).err().map(|e| e.kind()))
            .collect();
        assert_eq!(
            kinds,
            [
                Some(io::ErrorKind::WouldBlock),
                None,
                Some(io::ErrorKind::TimedOut),
                Some(io::ErrorKind::BrokenPipe),
                None
            ]
        );
        assert_eq!(*lock(&e.launcher.sent), [b"a", b"b", b"c", b"d", b"e"]);
        assert_eq!(e.launcher.writes.load(Ordering::SeqCst), 5);
        lock(&e.launcher.screens).insert(String::new(), "a menu".into());
        assert_eq!(d.screen().text, "a menu");
    }

    /// C1b-M1-3: a write the drain stops is `Blocked`, sends nothing and
    /// is audited; a driver error comes back with its kind.
    #[test]
    fn pty_write_reports_a_drain_block() {
        let e = setup(Script::Hang, None);
        let spec = SpawnSpec::new(vec!["claude".into()], e.cwd.clone());
        let mut d = e.launcher.spawn(&spec).unwrap();
        assert_eq!(e.sp.pty_write("agt_x", d.as_mut(), b"hi"), Ok(()));
        assert_eq!(e.launcher.writes.load(Ordering::SeqCst), 1);
        e.sp.drain_switch().start();
        assert_eq!(
            e.sp.pty_write("agt_x", d.as_mut(), b"hi"),
            Err(PtyWriteError::Blocked)
        );
        assert_eq!(e.launcher.writes.load(Ordering::SeqCst), 1);
        assert_eq!(blocked_pty_writes(&e.db), 1);
    }

    /// C1a-27: zero sends on the PTY path while draining. The drain starts
    /// at the first look at the screen; the trust dialog's Down is not
    /// written but audited. The open create is left to boot reconcile, and
    /// no other create gets a row or a spawn.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn while_draining_no_pty_write_and_no_spawn() {
        let (clock, _secs) = hand_clock();
        let e = setup(Script::Trust, Some(clock));
        e.launcher.drain_on_screen.store(true, Ordering::SeqCst);
        let (sp, cwd) = (e.sp.clone(), e.cwd.clone());
        let create = tokio::spawn(async move {
            sp.create(&Principal::LocalUnverified, create_params(&cwd))
                .await
        });
        wait_for("a blocked PTY write", || blocked_pty_writes(&e.db) >= 1);
        assert_eq!(e.launcher.writes.load(Ordering::SeqCst), 0);
        let id = only_row(&e.db).id;
        let err =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap_err();
        assert_eq!(err.code, ErrorCode::Draining);
        assert!(err.code.retryable());
        assert_eq!(e.launcher.spawns.load(Ordering::SeqCst), 1);
        assert_eq!(only_row(&e.db).id, id);

        let report = drain::run(&e.sp, fast_drain(), e.sp.now());
        assert_eq!(report.left, vec![id.clone()]);
        assert!(report.paused.is_empty() && report.interrupted.is_empty());
        assert!(!report.timed_out);
        e.sp.shutdown();
        assert_eq!(create.await.unwrap().unwrap_err().code, ErrorCode::Draining);
        assert_eq!(e.launcher.writes.load(Ordering::SeqCst), 0);
        assert_eq!(row(&e.db, &id).state.as_deref(), Some("starting"));
        assert_eq!(ops_of(&e.db), [("pty".to_string(), false, None)]);
        let done = ops::reconcile(&e.db, &e.home).unwrap();
        assert_eq!(done.failed, vec![id.clone()]);
    }

    /// (generation, resume_count, provider_sid) of `id`.
    fn gens(db: &Db, id: &str) -> (i64, i64, Option<String>) {
        db.reader()
            .unwrap()
            .query_row(
                "SELECT generation, resume_count, provider_sid FROM agents WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    /// (generation, provider_sid) of every alias row of `id`.
    fn aliases_of(db: &Db, id: &str) -> Vec<(i64, String)> {
        let conn = db.reader().unwrap();
        let mut st = conn
            .prepare(
                "SELECT generation, provider_sid FROM session_alias WHERE agent_id = ?1 \
                 ORDER BY rowid",
            )
            .unwrap();
        let rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        rows.map(Result::unwrap).collect()
    }

    /// (kind, phase, done, outcome) of every op of `id`.
    fn ops_for(db: &Db, id: &str) -> Vec<(String, String, bool, Option<String>)> {
        let conn = db.reader().unwrap();
        let mut st = conn
            .prepare(
                "SELECT kind, phase, done_at IS NOT NULL, payload FROM ops \
                 WHERE json_extract(payload, '$.agentId') = ?1 ORDER BY created_at, rowid",
            )
            .unwrap();
        let rows = st
            .query_map([id], |r| {
                let p: String = r.get(3)?;
                let p: CreatePayload = serde_json::from_str(&p).unwrap();
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, p.outcome))
            })
            .unwrap();
        rows.map(Result::unwrap).collect()
    }

    fn strings(v: Vec<OsString>) -> Vec<String> {
        v.into_iter().map(|s| s.into_string().unwrap()).collect()
    }

    /// Create one agent and drain it: `paused(drain)`, its supervisor gone.
    async fn drained_agent(e: &Env) -> AgentRow {
        let r =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap();
        e.sp.drain_switch().start();
        let report = drain::run(&e.sp, fast_drain(), e.sp.now());
        assert_eq!(report.paused, std::slice::from_ref(&r.id));
        no_supervisors(&e.sp);
        r
    }

    /// C1a-28, with a live agent at the restart: the drain pauses the
    /// ready agent and interrupts the long turn at 60 s; the next engine
    /// reconciles (nothing to do) and resumes both with `claude --resume`
    /// on the same session, the same profile, a new settings file and
    /// generation 2. An engine-restart resume does not count, and `ready`
    /// clears the drain reason and argv. The interrupted note waits for
    /// the resume's SessionStart.
    #[tokio::test]
    async fn a_restart_drains_then_resumes_with_the_same_session() {
        let (clock, secs) = hand_clock();
        let e = setup(Script::Ready, Some(clock));
        let l = Principal::LocalUnverified;
        let idle = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
        let busy = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
        set_state(&e.db, &busy.id, "working");
        e.sp.drain_switch().start();
        let sp = e.sp.clone();
        let start = sp.now();
        let drainer = std::thread::spawn(move || drain::run(&sp, fast_drain(), start));
        wait_for("the ready agent paused", || {
            drained(&e.db, &idle.id).0.as_deref() == Some("paused")
        });
        secs.store(60, Ordering::SeqCst);
        let report = drainer.join().unwrap();
        assert_eq!(report.interrupted, std::slice::from_ref(&busy.id));
        no_supervisors(&e.sp);

        // The next engine: reconcile first, then the resume.
        assert!(ops::reconcile(&e.db, &e.home).unwrap().is_empty());
        let (agents, l2, sp2) = next_engine(&e, Script::Ready, None);
        let resumed = sp2.resume_on_boot().unwrap();
        let mut want = vec![idle.id.clone(), busy.id.clone()];
        want.sort();
        assert_eq!(resumed.started, want);
        assert!(resumed.failed.is_empty() && resumed.left.is_empty());
        for r in [&idle, &busy] {
            wait_for("the resumed agent ready", || {
                row(&e.db, &r.id).state.as_deref() == Some("ready")
            });
            let sid = r.provider_sid.clone().unwrap();
            assert_eq!(gens(&e.db, &r.id), (2, 0, Some(sid.clone())));
            assert_eq!(drained(&e.db, &r.id), (Some("ready".into()), None, None));
            assert_eq!(aliases_of(&e.db, &r.id), [(1, sid.clone()), (2, sid)]);
            assert_eq!(
                ops_for(&e.db, &r.id),
                [
                    (
                        ops::KIND_CREATE.to_string(),
                        "bound".to_string(),
                        true,
                        Some("bound".to_string())
                    ),
                    (
                        ops::KIND_RESUME.to_string(),
                        "bound".to_string(),
                        true,
                        Some("bound".to_string())
                    ),
                ]
            );
            assert_eq!(agents.current(&r.id).unwrap().gen, 2);
            assert!(!settings_path_of(&e.home, &r.id, 1).exists());
            assert!(settings_path_of(&e.home, &r.id, 2).exists());
        }
        assert!(row(&e.db, &busy.id).interrupted_note_pending);
        assert!(!row(&e.db, &idle.id).interrupted_note_pending);
        // One spawn each, and nothing failed.
        assert_eq!(l2.spawns.load(Ordering::SeqCst), 2);
        assert!(events(&e.db, "agent.failed").is_empty());

        // The argv of the last spawn: the resumes may run in either order,
        // so find the agent by its settings path.
        let got = strings(lock(&l2.argv).clone());
        let last = want
            .iter()
            .find(|id| got.contains(&settings_path_of(&e.home, id, 2).display().to_string()))
            .expect("the last argv names a resumed agent");
        let sid = row(&e.db, last).provider_sid.unwrap();
        let settings = settings_path_of(&e.home, last, 2).display().to_string();
        let mut argv = vec![
            "/bin/false".to_string(),
            "--resume".into(),
            sid,
            "--settings".into(),
            settings,
        ];
        argv.extend(CLAUDE.permission_flags.auto.iter().map(|s| s.to_string()));
        argv.extend(
            crate::engine::adapter::cc::profile::ISOLATE_ARGS
                .iter()
                .map(|s| s.to_string()),
        );
        assert_eq!(got, argv);
        let env = lock(&l2.env).clone();
        let gen_env = env.iter().find(|(k, _)| k == "C9_AGENT_GEN").unwrap();
        assert_eq!(gen_env.1, "2");
        sp2.shutdown();
    }

    /// After a resume, an event of the old generation is dropped as
    /// `stale_generation` and audited; the new generation's is taken.
    #[tokio::test]
    async fn old_generation_hooks_after_a_resume_are_dropped_and_audited() {
        let e = setup(Script::Ready, None);
        let r = drained_agent(&e).await;
        let (agents, _l2, sp2) = next_engine(&e, Script::Ready, None);
        sp2.resume_on_boot().unwrap();
        wait_for("ready", || {
            row(&e.db, &r.id).state.as_deref() == Some("ready")
        });
        let merge = Arc::new(MergeTable::new());
        let live = agents.current(&r.id).map(|g| g.gen);
        assert_eq!(live, Some(2));
        let send = |gen: i64, seq: &str| {
            let (id, seq, merge) = (r.id.clone(), seq.to_string(), merge.clone());
            e.db.write_blocking(move |tx| {
                let params = crate::engine::proto::HookParams {
                    agent_id: id.clone(),
                    kind: "claude".into(),
                    seq,
                    input: serde_json::json!({}),
                    truncated: false,
                };
                let principal = format!("agent:{}", id);
                let ev = crate::engine::hooks::HookEvent {
                    method: "hook.stop",
                    principal: &principal,
                    gen,
                    live_gen: live,
                    params: &params,
                };
                crate::engine::hooks::intake(tx, MACHINE, &merge, &ev, now_ms())
            })
            .unwrap()
            .0
        };
        use crate::engine::hooks::{DropReason, Outcome};
        assert_eq!(send(1, "1"), Outcome::Dropped(DropReason::StaleGeneration));
        assert_eq!(send(2, "2"), Outcome::Accepted);
        let audited: Vec<(String, String)> = {
            let conn = e.db.reader().unwrap();
            let mut st = conn
                .prepare("SELECT method, reason FROM audit WHERE decision = 'drop' AND target = ?1")
                .unwrap();
            let rows = st
                .query_map([&r.id], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap();
            rows.map(Result::unwrap).collect()
        };
        assert_eq!(audited.len(), 1, "{:?}", audited);
        assert_eq!(audited[0].0, "hook.stop");
        assert!(
            audited[0]
                .1
                .starts_with("stale_generation: event of generation 1"),
            "{}",
            audited[0].1
        );
        sp2.shutdown();
    }

    /// D28 Q7 on resume: a usage-limit screen before ready puts the row
    /// back to `paused` with `resume_failed` and audits the vendor's text,
    /// after exactly one attempt. No later boot tries it again (D4: only
    /// `agent.resume` does).
    #[tokio::test]
    async fn a_vendor_refusal_on_resume_is_resume_failed_with_one_attempt() {
        let e = setup(Script::Ready, None);
        let r = drained_agent(&e).await;
        let (_, l2, sp2) = next_engine(&e, Script::Refuse, None);
        assert_eq!(sp2.resume_on_boot().unwrap().started, [r.id.clone()]);
        wait_for("resume_failed", || {
            row(&e.db, &r.id).state_reason.as_deref() == Some(ops::RESUME_FAILED)
        });
        no_supervisors(&sp2);
        assert_eq!(l2.spawns.load(Ordering::SeqCst), 1);
        let got = row(&e.db, &r.id);
        assert_eq!(got.state.as_deref(), Some("paused"));
        assert_eq!(got.gone_at, None);
        assert_eq!(gens(&e.db, &r.id).1, 0);
        // The agent did not fail; its resume did. The reason is on the
        // row, the vendor's text in the audit.
        assert!(events(&e.db, "agent.failed").is_empty());
        let why = audit_reasons(&e.db, ops::KIND_RESUME, "failed");
        assert_eq!(why.len(), 1);
        assert!(why[0].contains("Claude usage limit reached"), "{:?}", why);
        assert_eq!(
            ops_for(&e.db, &r.id)[1],
            (
                ops::KIND_RESUME.to_string(),
                "pty".to_string(),
                true,
                Some(ops::RESUME_FAILED.to_string())
            )
        );
        assert_eq!(agent_state_rows(&e.db), 0);
        assert!(!settings_path_of(&e.home, &r.id, 2).exists());
        // Never retried: not by reconcile, not by the next boot.
        assert!(ops::reconcile(&e.db, &e.home).unwrap().is_empty());
        let (_, l3, sp3) = next_engine(&e, Script::Ready, None);
        assert_eq!(sp3.resume_on_boot().unwrap(), Resumed::default());
        assert_eq!(l3.spawns.load(Ordering::SeqCst), 0);
    }

    /// An exit before ready, and no ready within 30 s, are `resume_failed`
    /// with one spawn each.
    #[tokio::test]
    async fn an_exit_or_no_ready_on_resume_is_resume_failed() {
        // A clock that moves 1 s per look.
        let base = Instant::now();
        let n = Arc::new(AtomicU64::new(0));
        let fast: Clock =
            Arc::new(move || base + Duration::from_secs(n.fetch_add(1, Ordering::SeqCst)));
        for (script, clock, text) in [
            (Script::Exit, None, "exited before ready"),
            (Script::Hang, Some(fast), "not ready within 30 s"),
        ] {
            let e = setup(Script::Ready, None);
            let r = drained_agent(&e).await;
            let (_, l2, sp2) = next_engine(&e, script, clock);
            sp2.resume_on_boot().unwrap();
            no_supervisors(&sp2);
            assert_eq!(l2.spawns.load(Ordering::SeqCst), 1, "{:?}", script);
            let got = row(&e.db, &r.id);
            assert_eq!(got.state.as_deref(), Some("paused"), "{:?}", script);
            assert_eq!(got.state_reason.as_deref(), Some(ops::RESUME_FAILED));
            let why = audit_reasons(&e.db, ops::KIND_RESUME, "failed");
            assert!(why[0].contains(text), "{:?}", why);
        }
    }

    /// An agent that boot reconcile marked `interrupted` after a crash
    /// (no drain) resumes on its `provider_sid`, and that resume counts.
    #[tokio::test]
    async fn a_crash_interrupted_agent_resumes_and_counts() {
        let e = setup(Script::Ready, None);
        let r =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap();
        e.sp.shutdown();
        let done = ops::reconcile(&e.db, &e.home).unwrap();
        assert_eq!(done.interrupted, std::slice::from_ref(&r.id));
        assert_eq!(
            drained(&e.db, &r.id),
            (Some("interrupted".into()), None, None)
        );
        let (_, l2, sp2) = next_engine(&e, Script::Ready, None);
        assert_eq!(sp2.resume_on_boot().unwrap().started, [r.id.clone()]);
        wait_for("ready", || {
            row(&e.db, &r.id).state.as_deref() == Some("ready")
        });
        assert_eq!(gens(&e.db, &r.id), (2, 1, r.provider_sid.clone()));
        // Reviews L3: the crashed generation's settings file goes.
        assert!(!settings_path_of(&e.home, &r.id, 1).exists());
        assert!(settings_path_of(&e.home, &r.id, 2).exists());
        let argv = strings(lock(&l2.argv).clone());
        assert_eq!(
            argv[1..3],
            ["--resume".to_string(), r.provider_sid.unwrap()]
        );
        sp2.shutdown();
    }

    /// Reviews M2: the engine stops (SIGINT) or drains (a second restart)
    /// while a resume comes up. The supervisor kills it and puts the row
    /// back to `paused(drain)` with its argv; the resume op closes; boot
    /// reconcile has nothing to do, and the next boot resumes it.
    #[tokio::test]
    async fn a_resume_cut_by_a_stop_or_drain_is_resumed_again() {
        for drains in [false, true] {
            let e = setup(Script::Ready, None);
            let r = drained_agent(&e).await;
            let before = drained(&e.db, &r.id);
            let (_, l2, sp2) = next_engine(&e, Script::Hang, None);
            sp2.resume_on_boot().unwrap();
            wait_for("the resume spawned", || {
                row(&e.db, &r.id).state.as_deref() == Some("starting")
            });
            if drains {
                sp2.drain_switch().start();
                let report = drain::run(&sp2, fast_drain(), sp2.now());
                assert_eq!(report.left, std::slice::from_ref(&r.id));
            }
            sp2.shutdown();
            assert_eq!(l2.kills.load(Ordering::SeqCst), 1);
            assert_eq!(drained(&e.db, &r.id), before, "drains {}", drains);
            let outcome = if drains { "draining" } else { "stopped" };
            assert_eq!(
                ops_for(&e.db, &r.id)[1],
                (
                    ops::KIND_RESUME.to_string(),
                    "pty".to_string(),
                    true,
                    Some(outcome.to_string())
                )
            );
            assert_eq!(agent_state_rows(&e.db), 0);
            assert!(!settings_path_of(&e.home, &r.id, 2).exists());
            assert!(ops::reconcile(&e.db, &e.home).unwrap().is_empty());
            let (_, l3, sp3) = next_engine(&e, Script::Ready, None);
            assert_eq!(sp3.resume_on_boot().unwrap().started, [r.id.clone()]);
            wait_for("ready", || {
                row(&e.db, &r.id).state.as_deref() == Some("ready")
            });
            assert_eq!(l3.spawns.load(Ordering::SeqCst), 1);
            // Neither cut resume counts.
            assert_eq!(gens(&e.db, &r.id).0, 3);
            assert_eq!(gens(&e.db, &r.id).1, 0);
            sp3.shutdown();
        }
    }

    /// A resume cut while the engine dies (no supervisor to undo it) is
    /// `resume_failed` by boot reconcile, and the next boot does not try
    /// again.
    #[tokio::test]
    async fn a_resume_cut_by_a_crash_is_resume_failed_by_reconcile() {
        let e = setup(Script::Ready, None);
        let r = drained_agent(&e).await;
        let (_, l2, sp2) = next_engine(&e, Script::Hang, None);
        sp2.resume_on_boot().unwrap();
        wait_for("the resume spawned", || {
            row(&e.db, &r.id).state.as_deref() == Some("starting")
        });
        // The kill fails, as it would for a process the supervisor cannot
        // stop: the op stays open, as after a crash.
        l2.kill_fails.store(true, Ordering::SeqCst);
        sp2.shutdown();
        let done = ops::reconcile(&e.db, &e.home).unwrap();
        assert_eq!(done.resume_failed, std::slice::from_ref(&r.id));
        let got = row(&e.db, &r.id);
        // It was `starting`: the engine stopped while it ran.
        assert_eq!(got.state.as_deref(), Some("interrupted"));
        assert_eq!(got.state_reason.as_deref(), Some(ops::RESUME_FAILED));
        assert_eq!(got.gone_at, None);
        assert_eq!(agent_state_rows(&e.db), 0);
        let (_, l3, sp3) = next_engine(&e, Script::Ready, None);
        assert_eq!(sp3.resume_on_boot().unwrap(), Resumed::default());
        assert_eq!(l3.spawns.load(Ordering::SeqCst), 0);
    }

    /// A retire while a resume comes up kills it and closes the resume op;
    /// boot reconcile then has nothing to do.
    #[tokio::test]
    async fn a_retire_during_a_resume_closes_its_op() {
        let e = setup(Script::Ready, None);
        let r = drained_agent(&e).await;
        let (_, l2, sp2) = next_engine(&e, Script::Hang, None);
        sp2.resume_on_boot().unwrap();
        wait_for("the resume spawned", || {
            row(&e.db, &r.id).state.as_deref() == Some("starting")
        });
        let got = sp2
            .retire(
                &Principal::LocalUnverified,
                serde_json::json!({ "id": r.id }),
            )
            .await
            .unwrap();
        assert_eq!(got.state.as_deref(), Some("retired"));
        no_supervisors(&sp2);
        assert_eq!(l2.kills.load(Ordering::SeqCst), 1);
        assert_eq!(
            ops_for(&e.db, &r.id)[1],
            (
                ops::KIND_RESUME.to_string(),
                "pty".to_string(),
                true,
                Some("retired".to_string())
            )
        );
        assert!(ops::reconcile(&e.db, &e.home).unwrap().is_empty());
    }

    /// A known reader/provider does not imply a supported spawn profile.
    #[test]
    fn descriptor_only_providers_cannot_boot_resume_as_claude() {
        let e = setup(Script::Ready, None);
        for provider in ["codex", "pi", "opencode", "cursor"] {
            let r = Resumable {
                id: "agt_other".into(),
                state: "paused".into(),
                reason: Some("drain".into()),
                provider: provider.into(),
                sid: Some("sid-1".into()),
                cwd: Some(e.cwd.to_string_lossy().into()),
                argv: Some(r#"["claude","--resume","sid-1"]"#.into()),
            };
            assert!(e
                .sp
                .resume_plan(&r)
                .unwrap_err()
                .contains("no spawn integration"));
        }
        assert_eq!(e.launcher.spawns.load(Ordering::SeqCst), 0);
    }

    /// No resume while draining: the row stays.
    /// A row with no session to resume fails at once, with no spawn.
    #[tokio::test]
    async fn rows_that_cannot_resume_spawn_nothing() {
        let e = setup(Script::Ready, None);
        let r = drained_agent(&e).await;
        let before = drained(&e.db, &r.id);
        let (_, l_dr, dr) = next_engine(&e, Script::Ready, None);
        dr.drain_switch().start();
        assert_eq!(dr.resume_on_boot().unwrap().left, [r.id.clone()]);
        assert_eq!(drained(&e.db, &r.id), before);
        assert_eq!(gens(&e.db, &r.id).0, 1);
        assert_eq!(l_dr.spawns.load(Ordering::SeqCst), 0);

        // An interrupted row with neither argv nor session.
        let cwd = e.cwd.canonicalize().unwrap().display().to_string();
        e.db.write_blocking(move |tx| {
            tx.execute(
                "INSERT INTO agents (id, mode, role, control, provider, source, state, cwd) \
                 VALUES ('agt_nosid', 'free', 'worker', 'pty', 'claudeCode', 'engine', \
                 'interrupted', ?1)",
                [cwd],
            )
        })
        .unwrap();
        let (_, l2, sp2) = next_engine(&e, Script::Ready, None);
        let resumed = sp2.resume_on_boot().unwrap();
        assert_eq!(resumed.failed, ["agt_nosid".to_string()]);
        assert_eq!(resumed.started, [r.id.clone()]);
        let got = row(&e.db, "agt_nosid");
        assert_eq!(got.state.as_deref(), Some("failed"));
        assert_eq!(got.state_reason.as_deref(), Some(ops::RESUME_FAILED));
        wait_for("ready", || {
            row(&e.db, &r.id).state.as_deref() == Some("ready")
        });
        assert_eq!(l2.spawns.load(Ordering::SeqCst), 1);
        sp2.shutdown();
    }

    /// Reviews H1, through the screen and not `set_state`: a ready agent
    /// whose screen shows a turn is `working`, so the drain waits for it.
    /// One turn ends at 30 s and is paused; the other still runs at 60 s
    /// and is interrupted with the note.
    #[tokio::test]
    async fn the_drain_waits_for_a_turn_seen_on_the_screen() {
        let (clock, secs) = hand_clock();
        let e = setup(Script::Ready, Some(clock));
        let l = Principal::LocalUnverified;
        let ends = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
        let long = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
        for r in [&ends, &long] {
            lock(&e.launcher.busy).insert(r.id.clone());
            wait_for("working from the screen", || {
                row(&e.db, &r.id).state.as_deref() == Some("working")
            });
        }
        e.sp.drain_switch().start();
        let sp = e.sp.clone();
        let start = sp.now();
        let drainer = std::thread::spawn(move || drain::run(&sp, fast_drain(), start));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(e.launcher.kills.load(Ordering::SeqCst), 0);
        secs.store(30, Ordering::SeqCst);
        lock(&e.launcher.busy).remove(&ends.id);
        wait_for("the ended turn paused", || {
            drained(&e.db, &ends.id).0.as_deref() == Some("paused")
        });
        assert!(events(&e.db, "agent.turn_end")
            .iter()
            .any(|ev| ev["id"] == ends.id.as_str() && ev["by"] == "screen"));
        secs.store(60, Ordering::SeqCst);
        let report = drainer.join().unwrap();
        assert_eq!(report.paused, std::slice::from_ref(&ends.id));
        assert_eq!(report.interrupted, std::slice::from_ref(&long.id));
        assert_eq!(drained(&e.db, &long.id), drain_facts(&long, "interrupted"));
        assert!(row(&e.db, &long.id).interrupted_note_pending);
        assert!(!row(&e.db, &ends.id).interrupted_note_pending);
        no_supervisors(&e.sp);
    }

    /// Reviews M1: a drained agent whose kill fails is not settled. The
    /// row and its `agent_state` stay, so boot reconcile kills the process
    /// by pid and start time before anything resumes it.
    #[tokio::test]
    async fn a_failed_drain_kill_leaves_the_row_to_boot_reconcile() {
        let e = setup(Script::Ready, None);
        let r =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap();
        e.launcher.kill_fails.store(true, Ordering::SeqCst);
        e.sp.drain_switch().start();
        let report = drain::run(&e.sp, fast_drain(), e.sp.now());
        assert_eq!(report.unsettled, std::slice::from_ref(&r.id));
        no_supervisors(&e.sp);
        assert_eq!(drained(&e.db, &r.id), (Some("ready".into()), None, None));
        assert_eq!(agent_state_rows(&e.db), 1);
        let audited: i64 =
            e.db.reader()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM audit WHERE method = ?1 AND decision = 'error'",
                    [drain::METHOD_DRAIN],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(audited, 1);
        // Boot reconcile finds the process (gone here: a fake pid) and
        // marks the row; only then is it resumable.
        let done = ops::reconcile(&e.db, &e.home).unwrap();
        assert_eq!(done.interrupted, std::slice::from_ref(&r.id));
        assert_eq!(agent_state_rows(&e.db), 0);
    }

    /// Reviews L2: a drain that starts during the spawn cuts the new agent
    /// before it is used. Nothing is written to it; the create answers
    /// `draining`, and its open op is boot reconcile's.
    #[tokio::test]
    async fn a_drain_during_the_spawn_cuts_the_agent() {
        let e = setup(Script::Trust, None);
        let err =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap_err();
        assert_eq!(err.code, ErrorCode::Draining);
        no_supervisors(&e.sp);
        assert_eq!(e.launcher.kills.load(Ordering::SeqCst), 1);
        assert_eq!(e.launcher.writes.load(Ordering::SeqCst), 0);
        assert_eq!(blocked_pty_writes(&e.db), 0);
        assert_eq!(ops_of(&e.db), [("pty".to_string(), false, None)]);
        let id = only_row(&e.db).id;
        assert_eq!(ops::reconcile(&e.db, &e.home).unwrap().failed, [id]);
    }

    // ── Mail delivery (C1b-M1-9) ───────────────────────────────────────

    /// Queue `body` for `to` from the terminal, as `msg.send` does;
    /// returns the message id.
    fn queue(db: &Db, to: &str, body: &str) -> String {
        let (to, body) = (to.to_string(), body.to_string());
        db.write_blocking(move |tx| {
            let sender = mail::sender(tx, &Principal::LocalUnverified)?;
            let target = mail::resolve(tx, MACHINE, &to)?.unwrap();
            let p = crate::engine::proto::MsgSendParams {
                to,
                body,
                ..Default::default()
            };
            let sent = mail::enqueue(tx, MACHINE, &sender, &target, &p, None, "me", now_ms())?;
            Ok(sent.unwrap().msg_id)
        })
        .unwrap()
        .0
    }

    /// (state, detail of its last status) of message `id`.
    fn mail_of(db: &Db, id: &str) -> (String, Option<String>) {
        db.reader()
            .unwrap()
            .query_row(
                "SELECT m.state, s.detail FROM messages m JOIN message_status s \
                 ON s.msg_id = m.id WHERE m.id = ?1 ORDER BY s.rowid DESC LIMIT 1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    fn wait_mail(db: &Db, id: &str, state: &str) -> Option<String> {
        wait_for(&format!("{} {}", id, state), || mail_of(db, id).0 == state);
        mail_of(db, id).1
    }

    fn sent(e: &Env) -> Vec<Vec<u8>> {
        lock(&e.launcher.sent).clone()
    }

    fn is_paste(b: &[u8], msg_id: &str) -> bool {
        b.starts_with(b"\x1b[200~")
            && String::from_utf8_lossy(b).contains(&format!("[c9watch mail {}]", msg_id))
    }

    /// A box between two rules holding `inside`, under the idle title.
    fn boxed(inside: &str) -> String {
        let rule = "\u{2500}".repeat(40);
        format!("{}\n\u{276f} {}\n{}\n  status", rule, inside, rule)
    }

    /// Let the supervisor run a few ticks.
    fn ticks() {
        std::thread::sleep(RUN_TICK * 3);
    }

    /// E8: when the agent is ready and its box free, the paste is exactly
    /// `ESC[200~<fence>ESC[201~`; the CR follows only after PASTE_SETTLE
    /// and once the box shows the paste. The prompt hook confirms it.
    #[tokio::test]
    async fn mail_is_pasted_then_submitted_once_the_box_shows_it() {
        let (clock, secs) = hand_clock();
        let e = setup(Script::Ready, Some(clock));
        let r =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap();
        let id = queue(&e.db, &r.id, "hello\nworld");
        e.sp.nudge_mail(&r.id);
        wait_for("the paste", || sent(&e).len() == 1);
        assert_eq!(mail_of(&e.db, &id).0, "delivering");
        let prov = mail::provenance(
            MACHINE,
            &mail::sender(&e.db.reader().unwrap(), &Principal::LocalUnverified).unwrap(),
            "me",
        );
        let fenced = mail::fence(&id, None, &prov, "hello\nworld");
        assert_eq!(sent(&e)[0], paste_payload(&fenced));
        ticks();
        assert_eq!(sent(&e).len(), 1, "no CR before PASTE_SETTLE");
        secs.store(1, Ordering::SeqCst);
        wait_for("the CR", || sent(&e).len() == 2);
        assert_eq!(sent(&e)[1], KEY_CR);
        assert_eq!(*lock(&e.launcher.prompts), std::slice::from_ref(&fenced));
        let machine = MACHINE.to_string();
        let (agent, prompt) = (r.id.clone(), fenced);
        e.db.write_blocking(move |tx| {
            mail::confirm_from_prompt(tx, &machine, &agent, &prompt, now_ms())
        })
        .unwrap();
        assert_eq!(
            mail_of(&e.db, &id),
            ("delivered".into(), Some("prompt_hook".into()))
        );
        secs.store(30, Ordering::SeqCst);
        ticks();
        assert_eq!(mail_of(&e.db, &id).0, "delivered");
        assert_eq!(sent(&e).len(), 2);
        e.sp.shutdown();
    }

    /// E7: a turn, a dialog, a menu with no input box and a half-typed
    /// box all write nothing; the paste goes once the agent is ready with
    /// a free box. Two messages go in two rounds.
    #[tokio::test]
    async fn mail_waits_for_a_ready_agent_with_a_free_box() {
        let (clock, secs) = hand_clock();
        let e = setup(Script::Ready, Some(clock));
        let r =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap();
        lock(&e.launcher.busy).insert(r.id.clone());
        wait_for("working", || {
            row(&e.db, &r.id).state.as_deref() == Some("working")
        });
        let first = queue(&e.db, &r.id, "one");
        let second = queue(&e.db, &r.id, "two");
        e.sp.nudge_mail(&r.id);
        ticks();
        lock(&e.launcher.busy).remove(&r.id);
        for screen in [
            " Do you want to proceed?\n \u{276f} 1. Yes\n   2. No\n".to_string(),
            " Pick a model\n   1. Opus\n   2. Sonnet\n".to_string(),
            boxed("half typed"),
        ] {
            lock(&e.launcher.screens).insert(r.id.clone(), screen.clone());
            ticks();
            assert!(sent(&e).is_empty(), "wrote on {:?}", screen);
        }
        assert_eq!(mail_of(&e.db, &first).0, "queued");
        lock(&e.launcher.screens).remove(&r.id);
        wait_for("the first paste", || sent(&e).len() == 1);
        assert!(is_paste(&sent(&e)[0], &first));
        secs.store(1, Ordering::SeqCst);
        wait_for("the CR", || sent(&e).len() == 2);
        // The CR turned the title busy: the second waits for the turn.
        e.db.write_blocking({
            let (agent, id) = (r.id.clone(), first.clone());
            move |tx| {
                mail::confirm_from_prompt(
                    tx,
                    MACHINE,
                    &agent,
                    &format!("[c9watch mail {}]", id),
                    now_ms(),
                )
            }
        })
        .unwrap();
        ticks();
        assert_eq!(sent(&e).len(), 2);
        assert_eq!(mail_of(&e.db, &second).0, "queued");
        lock(&e.launcher.busy).remove(&r.id);
        wait_for("the second paste", || sent(&e).len() == 3);
        assert!(is_paste(&sent(&e)[2], &second));
        e.sp.shutdown();
    }

    /// H2: a paste the box never shows (an unknown menu took the screen)
    /// gets no CR and is `uncertain`, with attention.
    #[tokio::test]
    async fn a_paste_not_seen_in_the_box_is_uncertain_without_cr() {
        let (clock, secs) = hand_clock();
        let e = setup(Script::Ready, Some(clock));
        let r =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap();
        let id = queue(&e.db, &r.id, "hi");
        e.sp.nudge_mail(&r.id);
        wait_for("the paste", || sent(&e).len() == 1);
        lock(&e.launcher.screens).insert(r.id.clone(), " Pick one\n   1. A\n".into());
        secs.store(1, Ordering::SeqCst);
        ticks();
        assert_eq!(sent(&e).len(), 1);
        secs.store(5, Ordering::SeqCst);
        let detail = wait_mail(&e.db, &id, "uncertain");
        assert_eq!(detail.as_deref(), Some("paste not seen in input box"));
        assert_eq!(sent(&e).len(), 1, "no CR");
        let att = events(&e.db, "agent.attention");
        assert_eq!(att.len(), 1);
        assert_eq!(att[0]["reason"], format!("mail_uncertain: {}", id));
        e.sp.shutdown();
    }

    /// E9 and §4.6: only `WouldBlock` puts a message back (three times at
    /// most, then `failed`); `TimedOut` is `uncertain` with no second
    /// paste and no CR; `BrokenPipe` fails it; a CR error is `uncertain`.
    #[tokio::test]
    async fn write_errors_are_classified() {
        use std::io::ErrorKind::{BrokenPipe, Other, TimedOut, WouldBlock};
        let (clock, secs) = hand_clock();
        let e = setup(Script::Ready, Some(clock));
        let r =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap();
        let results = |r: &[Option<io::ErrorKind>]| lock(&e.launcher.write_results).extend(r);

        results(&[Some(WouldBlock); 3]);
        let id = queue(&e.db, &r.id, "a");
        e.sp.nudge_mail(&r.id);
        let detail = wait_mail(&e.db, &id, "failed");
        assert_eq!(detail.as_deref(), Some("pty write queue full"));
        assert_eq!(sent(&e).len(), 3);
        let statuses: Vec<String> = {
            let conn = e.db.reader().unwrap();
            let mut st = conn
                .prepare("SELECT state FROM message_status WHERE msg_id = ?1 ORDER BY rowid")
                .unwrap();
            let rows = st.query_map([&id], |r| r.get(0)).unwrap();
            rows.map(Result::unwrap).collect()
        };
        assert_eq!(
            statuses,
            [
                "queued",
                "delivering",
                "queued",
                "delivering",
                "queued",
                "delivering",
                "failed"
            ]
        );

        results(&[Some(TimedOut)]);
        let id = queue(&e.db, &r.id, "b");
        e.sp.nudge_mail(&r.id);
        let detail = wait_mail(&e.db, &id, "uncertain").unwrap();
        assert!(detail.starts_with("paste may be partial"), "{}", detail);
        secs.store(10, Ordering::SeqCst);
        ticks();
        assert_eq!(sent(&e).len(), 4, "no second paste and no CR");

        results(&[Some(BrokenPipe)]);
        let id = queue(&e.db, &r.id, "c");
        e.sp.nudge_mail(&r.id);
        let detail = wait_mail(&e.db, &id, "failed").unwrap();
        assert!(detail.starts_with("pty gone"), "{}", detail);

        results(&[None, Some(Other)]);
        let id = queue(&e.db, &r.id, "d");
        e.sp.nudge_mail(&r.id);
        wait_for("the paste", || sent(&e).len() == 6);
        secs.store(11, Ordering::SeqCst);
        let detail = wait_mail(&e.db, &id, "uncertain").unwrap();
        assert!(detail.starts_with("cr write"), "{}", detail);
        assert_eq!(events(&e.db, "agent.attention").len(), 2);
        e.sp.shutdown();
    }

    /// E10: while the engine drains no mail is written (and no PTY write
    /// is even tried); the message stays `queued`.
    #[tokio::test]
    async fn no_mail_is_delivered_while_draining() {
        let e = setup(Script::Ready, None);
        let r =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap();
        e.sp.drain_switch().start();
        let id = queue(&e.db, &r.id, "hi");
        e.sp.nudge_mail(&r.id);
        ticks();
        assert!(sent(&e).is_empty());
        assert_eq!(blocked_pty_writes(&e.db), 0);
        assert_eq!(mail_of(&e.db, &id).0, "queued");
        e.sp.shutdown();
    }

    /// §4.6-4.7: a CR the drain blocks, and a drain command while the
    /// paste waits for its CR, both put the message back in the queue.
    #[tokio::test]
    async fn a_drain_before_the_cr_puts_the_mail_back() {
        let (clock, secs) = hand_clock();
        let e = setup(Script::Ready, Some(clock));
        let l = Principal::LocalUnverified;
        let a = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
        let b = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
        let (aid, bid) = (queue(&e.db, &a.id, "a"), queue(&e.db, &b.id, "b"));
        e.sp.nudge_mail(&a.id);
        e.sp.nudge_mail(&b.id);
        wait_for("both pastes", || sent(&e).len() == 2);
        // b is drained while its paste waits (the clock has not moved).
        e.sp.drain_agent(&b.id);
        let detail = wait_mail(&e.db, &bid, "queued");
        assert_eq!(detail.as_deref(), Some("drain before submit"));
        // a's CR comes once the drain is on: blocked, audited, queued.
        e.sp.drain_switch().start();
        secs.store(1, Ordering::SeqCst);
        let detail = wait_mail(&e.db, &aid, "queued");
        assert_eq!(detail.as_deref(), Some("drain before submit"));
        assert_eq!(blocked_pty_writes(&e.db), 1);
        assert_eq!(sent(&e).len(), 2, "no CR");
        e.sp.shutdown();
    }

    /// §4.7-4.8: a message submitted when a drain, a shutdown or a retire
    /// comes is `uncertain`; a retire fails the rest of the queue.
    #[tokio::test]
    async fn stopping_after_the_cr_leaves_mail_uncertain() {
        let (clock, secs) = hand_clock();
        let e = setup(Script::Ready, Some(clock));
        let l = Principal::LocalUnverified;
        let mut submitted = Vec::new();
        let mut agents = Vec::new();
        for _ in 0..3 {
            let r = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
            let id = queue(&e.db, &r.id, "x");
            let n = sent(&e).len();
            e.sp.nudge_mail(&r.id);
            wait_for("the paste", || sent(&e).len() == n + 1);
            secs.fetch_add(1, Ordering::SeqCst);
            wait_for("the CR", || sent(&e).len() == n + 2);
            submitted.push(id);
            agents.push(r);
        }
        let rest = queue(&e.db, &agents[2].id, "y");
        e.sp.drain_agent(&agents[0].id);
        assert_eq!(
            wait_mail(&e.db, &submitted[0], "uncertain").as_deref(),
            Some("drain")
        );
        e.sp.retire(&l, serde_json::json!({ "id": agents[2].id }))
            .await
            .unwrap();
        assert_eq!(
            mail_of(&e.db, &submitted[2]),
            ("uncertain".into(), Some("target retired".into()))
        );
        assert_eq!(
            mail_of(&e.db, &rest),
            ("failed".into(), Some("target retired".into()))
        );
        e.sp.shutdown();
        assert_eq!(
            wait_mail(&e.db, &submitted[1], "uncertain").as_deref(),
            Some("shutdown")
        );
    }

    /// §4.5 fallback: with no prompt hook within SUBMIT_CONFIRM, an open
    /// turn and a box without the paste confirm it (`turn_open`); a box
    /// that still shows it, as its fence line or as CC's collapsed
    /// `[Pasted text #n +k lines]` (a CR that got lost while a background
    /// turn opened), is `uncertain`.
    #[tokio::test]
    async fn without_the_hook_an_open_turn_confirms_delivery() {
        let (clock, secs) = hand_clock();
        let e = setup(Script::Ready, Some(clock));
        let l = Principal::LocalUnverified;
        let a = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
        let b = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
        let c = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
        let (aid, bid) = (queue(&e.db, &a.id, "a"), queue(&e.db, &b.id, "b"));
        let cid = queue(&e.db, &c.id, "c");
        for agent in [&a, &b, &c] {
            e.sp.nudge_mail(&agent.id);
        }
        wait_for("three pastes", || sent(&e).len() == 3);
        secs.store(1, Ordering::SeqCst);
        wait_for("three CRs", || sent(&e).len() == 6);
        for agent in [&a, &b, &c] {
            wait_for("the turn", || {
                row(&e.db, &agent.id).state.as_deref() == Some("working")
            });
        }
        lock(&e.launcher.screens).insert(b.id.clone(), boxed(&format!("[c9watch mail {}]", bid)));
        lock(&e.launcher.screens).insert(c.id.clone(), boxed("[Pasted text #1 +4 lines]"));
        ticks();
        secs.store(12, Ordering::SeqCst);
        assert_eq!(
            wait_mail(&e.db, &aid, "delivered").as_deref(),
            Some("turn_open")
        );
        for id in [&bid, &cid] {
            let detail = wait_mail(&e.db, id, "uncertain").unwrap();
            assert!(detail.starts_with("no prompt hook after"), "{}", detail);
        }
        e.sp.shutdown();
    }

    /// §4.6: after a paste that timed out the agent may not read its
    /// terminal, so the next message waits (no second write) until the
    /// screen changes with a free box.
    #[tokio::test]
    async fn an_uncertain_paste_holds_the_queue_until_the_screen_moves() {
        let (clock, _secs) = hand_clock();
        let e = setup(Script::Ready, Some(clock));
        let r =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap();
        lock(&e.launcher.write_results).push_back(Some(io::ErrorKind::TimedOut));
        let first = queue(&e.db, &r.id, "one");
        let second = queue(&e.db, &r.id, "two");
        e.sp.nudge_mail(&r.id);
        let detail = wait_mail(&e.db, &first, "uncertain").unwrap();
        assert!(detail.starts_with("paste may be partial"), "{}", detail);
        ticks();
        assert_eq!(sent(&e).len(), 1, "no second write");
        assert_eq!(mail_of(&e.db, &second).0, "queued");
        // New output with a free box: the agent reads again.
        lock(&e.launcher.screens).insert(r.id.clone(), boxed(""));
        wait_for("the second paste", || sent(&e).len() == 2);
        assert!(is_paste(&sent(&e)[1], &second));
        e.sp.shutdown();
    }

    /// §4.8: an agent that exits fails its queue; mail in flight is
    /// `uncertain` (`ops::fail_agent`).
    #[tokio::test]
    async fn an_exit_ends_the_mail_of_the_agent() {
        let (clock, _secs) = hand_clock();
        let e = setup(Script::Ready, Some(clock));
        let r =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap();
        let first = queue(&e.db, &r.id, "a");
        let second = queue(&e.db, &r.id, "b");
        e.sp.nudge_mail(&r.id);
        wait_for("the paste", || sent(&e).len() == 1);
        e.launcher.exit_now.store(true, Ordering::SeqCst);
        no_supervisors(&e.sp);
        let why = format!("target failed: {}", EXITED);
        assert_eq!(
            mail_of(&e.db, &first),
            ("uncertain".into(), Some(why.clone()))
        );
        assert_eq!(mail_of(&e.db, &second), ("failed".into(), Some(why)));
    }
    // M2 state-machine fixtures: use the real store and FakeDriver with a
    // hand clock, without running a background supervisor or a live CC.
    fn socket_fixture() -> (Env, Start, Box<dyn Driver>, Arc<AtomicU64>) {
        let (clock, secs) = hand_clock();
        let e = setup(Script::Ready, Some(clock));
        e.db.write_blocking(|tx| {
            tx.execute("INSERT INTO agents(id,role,mode,control,provider,provider_sid,source,state,generation) VALUES ('agt_socket','worker','free','pty','claudeCode','s1','engine','ready',1)",[])?;
            Ok(())
        }).unwrap();
        let s = Start {
            id: "agt_socket".into(),
            gen: 1,
            sid: "s1".into(),
            op_id: "op_test".into(),
            cwd: e.cwd.clone(),
            claude: PathBuf::from("/bin/false"),
            kind: Kind::Create,
            from: None,
        };
        let mut spec = SpawnSpec::new(vec!["/bin/false".into()], e.cwd.clone());
        spec.env("C9_AGENT_ID", &s.id).env("C9_AGENT_GEN", "1");
        let driver = e.launcher.spawn(&spec).unwrap();
        (e, s, driver, secs)
    }
    fn socket_taken(e: &Env, s: &Start) -> String {
        let id = queue(&e.db, &s.id, "socket body");
        let (agent, msg) = (s.id.clone(), id.clone());
        e.db.write_blocking(move |tx| {
            mail::take_next(tx, MACHINE, &agent, now_ms())?;
            mail::set_via(tx, &msg, "socket")
        })
        .unwrap();
        id
    }
    fn socket_step(e: &Env, s: &Start, d: &mut dyn Driver, delivery: Delivery) -> Delivery {
        e.sp.deliver(s, d, &CcScreen::new(e.sp.now()), "", delivery, &mut true)
    }
    #[test]
    fn no_queue_operation_in_thirty_seconds_is_uncertain() {
        let (e, s, mut d, secs) = socket_fixture();
        let id = socket_taken(&e, &s);
        let state = socket_awaiting(id.clone(), e.sp.now());
        secs.store(31, Ordering::SeqCst);
        assert_eq!(socket_step(&e, &s, d.as_mut(), state), Delivery::Idle);
        assert_eq!(mail_of(&e.db, &id).0, "uncertain");
        assert!(mail_of(&e.db, &id)
            .1
            .unwrap()
            .starts_with("no queue-operation"));
        assert!(sent(&e).is_empty());
    }
    #[test]
    fn queued_in_cc_waits_while_the_target_works() {
        let (e, s, mut d, secs) = socket_fixture();
        let id = socket_taken(&e, &s);
        e.sp.note_mail(&s, &id, "cc enqueued");
        let state = socket_awaiting(id.clone(), e.sp.now());
        secs.store(200, Ordering::SeqCst);
        assert!(matches!(
            socket_step(&e, &s, d.as_mut(), state),
            Delivery::SocketAwaiting {
                enqueued: Some(_),
                ..
            }
        ));
        assert_eq!(mail_of(&e.db, &id).0, "delivering");
        assert!(sent(&e).is_empty());
    }
    #[test]
    fn queued_but_not_seen_after_turn_end_is_uncertain() {
        let (e, s, mut d, secs) = socket_fixture();
        let id = socket_taken(&e, &s);
        let now = e.sp.now();
        let state = Delivery::SocketAwaiting {
            id: id.clone(),
            since: now,
            enqueued: Some(now),
            turn_ended: Some(now),
            held: None,
            receipt_seq: 0,
        };
        secs.store(16, Ordering::SeqCst);
        assert_eq!(socket_step(&e, &s, d.as_mut(), state), Delivery::Idle);
        assert_eq!(
            mail_of(&e.db, &id).1.as_deref(),
            Some("queued in CC, not seen after turn end")
        );
    }
    #[test]
    fn an_orphaned_socket_delivery_is_adopted_on_idle() {
        let (e, s, mut d, _secs) = socket_fixture();
        let id = socket_taken(&e, &s);
        assert!(
            matches!(socket_step(&e,&s,d.as_mut(),Delivery::Idle),Delivery::SocketAwaiting{id:ref got,..} if got==&id)
        );
        assert!(sent(&e).is_empty());
    }
    #[test]
    fn held_blocks_the_queue_until_held_wait_and_delivered_restarts_the_timer() {
        let (e, s, mut d, secs) = socket_fixture();
        let id = socket_taken(&e, &s);
        let next = queue(&e.db, &s.id, "second");
        e.sp.note_mail(&s, &id, "held: synthetic");
        let state = socket_step(&e, &s, d.as_mut(), socket_awaiting(id.clone(), e.sp.now()));
        assert!(matches!(
            state,
            Delivery::SocketAwaiting { held: Some(_), .. }
        ));
        secs.store(899, Ordering::SeqCst);
        let state = socket_step(&e, &s, d.as_mut(), state);
        assert_eq!(mail_of(&e.db, &next).0, "queued");
        e.sp.note_mail(&s, &id, "cc accepted; awaiting evidence");
        let state = socket_step(&e, &s, d.as_mut(), state);
        assert!(matches!(state, Delivery::SocketAwaiting { held: None, .. }));
        secs.store(929, Ordering::SeqCst);
        assert_eq!(socket_step(&e, &s, d.as_mut(), state), Delivery::Idle);
        assert_eq!(mail_of(&e.db, &id).0, "uncertain");
    }
    #[test]
    fn held_without_a_decision_eventually_becomes_uncertain() {
        let (e, s, mut d, secs) = socket_fixture();
        let id = socket_taken(&e, &s);
        e.sp.note_mail(&s, &id, "held: synthetic");
        let state = socket_step(&e, &s, d.as_mut(), socket_awaiting(id.clone(), e.sp.now()));
        secs.store(900, Ordering::SeqCst);
        assert_eq!(socket_step(&e, &s, d.as_mut(), state), Delivery::Idle);
        assert_eq!(
            mail_of(&e.db, &id).1.as_deref(),
            Some("held, no decision in 15 min")
        );
    }
    #[test]
    fn a_receipt_before_the_writer_result_does_not_resend_at_once() {
        let (e, s, mut d, _secs) = socket_fixture();
        let id = socket_taken(&e, &s);
        let msg = id.clone();
        e.db.write_blocking(move |tx| {
            mail::requeue_after(
                tx,
                MACHINE,
                &msg,
                "rate-limited",
                Some(now_ms() + 10000),
                now_ms(),
            )
        })
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(super::super::cc_inbox::SocketOutcome::Sent)
            .unwrap();
        e.sp.socket_jobs.lock().unwrap().insert(id.clone(), rx);
        assert_eq!(
            socket_step(
                &e,
                &s,
                d.as_mut(),
                Delivery::SocketSending {
                    id: id.clone(),
                    at: e.sp.now()
                }
            ),
            Delivery::Idle
        );
        assert_eq!(mail_of(&e.db, &id).0, "queued");
        assert!(sent(&e).is_empty());
    }
    #[test]
    fn retire_while_sending_is_uncertain_without_waiting() {
        let (e, s, _d, _secs) = socket_fixture();
        let id = socket_taken(&e, &s);
        let (_tx, rx) = std::sync::mpsc::channel();
        e.sp.socket_jobs.lock().unwrap().insert(id.clone(), rx);
        let now = Instant::now();
        e.sp.stop_delivery(
            &s,
            &Delivery::SocketSending {
                id: id.clone(),
                at: e.sp.now(),
            },
            "retired",
            "retired",
        );
        assert!(now.elapsed() < Duration::from_millis(500));
        assert_eq!(
            mail_of(&e.db, &id).1.as_deref(),
            Some("stopped while sending")
        );
    }
    #[test]
    fn drain_keeps_awaiting_and_settles_interrupted() {
        let (e, s, d, _secs) = socket_fixture();
        let id = socket_taken(&e, &s);
        let state = socket_awaiting(id.clone(), e.sp.now());
        e.sp.drain_switch().start();
        e.sp.drain_delivery(&s, &state);
        assert_eq!(mail_of(&e.db, &id).0, "delivering");
        e.sp.drain_now(&s, d);
        assert_eq!(mail_of(&e.db, &id).1.as_deref(), Some("drain"));
        assert_eq!(row(&e.db, &s.id).state.as_deref(), Some("interrupted"));
    }
    #[test]
    fn while_draining_no_socket_write_is_sent_and_the_path_is_audited() {
        let (e, s, _d, _secs) = socket_fixture();
        e.sp.drain_switch().start();
        let pid = std::process::id() as i32;
        let endpoint = super::super::cc_inbox::Endpoint {
            path: e.t.path().join("missing.sock"),
            session_id: "s1".into(),
            pid,
            start: peer::proc_info(pid).unwrap().start,
        };
        assert_eq!(
            e.sp.socket_write(&s.id, endpoint, "{}\n".into())
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            super::super::cc_inbox::SocketOutcome::Blocked
        );
        assert_eq!(e.db.reader().unwrap().query_row("SELECT count(*) FROM audit WHERE method='drain.blocked_injection' AND reason='path=socket'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    }
    #[test]
    fn five_failed_connects_fail_the_mail_and_notify_without_any_paste() {
        let (e, s, _d, _secs) = socket_fixture();
        e.db.write_blocking(|tx|tx.execute("INSERT INTO agents(id,role,mode,control,provider,source,state) VALUES ('agt_sender','worker','free','pty','claudeCode','engine','ready')",[])).unwrap();
        let id = socket_taken(&e, &s);
        let msg = id.clone();
        e.db.write_blocking(move|tx|tx.execute("UPDATE messages SET from_agent='agt_sender',from_principal='agent:agt_sender',origin_kind='agent' WHERE id=?1",[msg])).unwrap();
        for attempt in 1..=5 {
            e.sp.retry_socket(&s, &id, "connection refused");
            if attempt < 5 {
                assert_eq!(mail_of(&e.db, &id).0, "queued");
                let c = e.db.reader().unwrap();
                let after: i64 = c
                    .query_row("SELECT not_before FROM messages WHERE id=?1", [&id], |r| {
                        r.get(0)
                    })
                    .unwrap();
                assert!(after > now_ms());
                let agent = s.id.clone();
                e.db.write_blocking(move |tx| {
                    assert!(mail::take_next(tx, MACHINE, &agent, after)?.is_some());
                    Ok(())
                })
                .unwrap();
            }
        }
        assert_eq!(mail_of(&e.db, &id).0, "failed");
        assert!(mail_of(&e.db, &id)
            .1
            .unwrap()
            .starts_with("socket unreachable:"));
        assert!(sent(&e).is_empty());
        let c = e.db.reader().unwrap();
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM messages WHERE kind='system' AND to_agent='agt_sender'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert!(events(&e.db, "agent.attention")
            .iter()
            .any(|e| e["reason"] == "mail_unreachable"));
    }
    #[test]
    fn unknown_gate_and_close_timeout_never_switch_to_paste() {
        let (e, s, mut d, _secs) = socket_fixture();
        let id = socket_taken(&e, &s);
        let m = mail::Mail {
            id: id.clone(),
            provenance: "p".into(),
            body: "b".into(),
            attempts: 1,
        };
        assert_eq!(
            e.sp.start_socket(
                &s,
                &m,
                super::super::cc_inbox::Gate::Unknown("half-written pid.json".into()),
                "fence",
                e.sp.now()
            ),
            Delivery::Idle
        );
        assert_eq!(mail_of(&e.db, &id).0, "queued");
        assert!(sent(&e).is_empty());
        let agent = s.id.clone();
        e.db.write_blocking(move |tx| mail::take_next(tx, MACHINE, &agent, now_ms() + 10000))
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(super::super::cc_inbox::SocketOutcome::MaybeSent(
            "close_timeout".into(),
        ))
        .unwrap();
        e.sp.socket_jobs.lock().unwrap().insert(id.clone(), rx);
        assert!(matches!(
            socket_step(
                &e,
                &s,
                d.as_mut(),
                Delivery::SocketSending {
                    id: id.clone(),
                    at: e.sp.now()
                }
            ),
            Delivery::SocketAwaiting { .. }
        ));
        assert_eq!(mail_of(&e.db, &id).0, "delivering");
        assert!(sent(&e).is_empty());
    }

    /// The audit `reason` of every row with `method` and `decision`, in
    /// order.
    fn audit_reasons(db: &Db, method: &str, decision: &str) -> Vec<String> {
        let conn = db.reader().unwrap();
        let mut st = conn
            .prepare(
                "SELECT coalesce(reason, '') FROM audit WHERE method = ?1 AND decision = ?2 \
                 ORDER BY rowid",
            )
            .unwrap();
        let rows = st
            .query_map(params![method, decision], |r| r.get::<_, String>(0))
            .unwrap();
        rows.map(Result::unwrap).collect()
    }

    /// A real test process in a new process group, as a PTY child is, and
    /// its id once it printed `ready`. `script` must `echo ready`.
    fn test_process(script: &str) -> (std::process::Child, ChildId) {
        use std::io::BufRead;
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", script])
            .stdout(std::process::Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line.trim(), "ready");
        let pid = child.id() as libc::pid_t;
        let start = super::super::peer::proc_info(pid).unwrap().start;
        (child, ChildId { pid, start })
    }

    /// A ready agent whose engine stopped, with `child` as its recorded
    /// PTY child, as boot reconcile finds it.
    async fn stopped_agent_with(e: &Env, child: ChildId) -> AgentRow {
        let r =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap();
        e.sp.shutdown();
        let id = r.id.clone();
        e.db.write_blocking(move |tx| {
            tx.execute(
                "UPDATE agent_state SET pty_child_pid = ?2, pty_child_start = ?3 \
                 WHERE agent_id = ?1",
                params![id, child.pid as i64, child.start as i64],
            )
        })
        .unwrap();
        r
    }

    /// D2: boot reconcile kills a recorded child that ends on SIGTERM and
    /// confirms it before the row becomes `interrupted`.
    #[tokio::test]
    async fn reconcile_confirms_a_kill_before_it_marks_the_row() {
        let e = setup(Script::Ready, None);
        let (mut proc, child) = test_process("echo ready; exec sleep 30");
        let r = stopped_agent_with(&e, child).await;
        let done = ops::reconcile(&e.db, &e.home).unwrap();
        assert_eq!(done.killed, 1);
        assert_eq!(done.interrupted, std::slice::from_ref(&r.id));
        assert!(proc.try_wait().unwrap().is_some(), "the process still runs");
        assert_eq!(row(&e.db, &r.id).state.as_deref(), Some("interrupted"));
    }

    /// D2: a child that ignores SIGTERM gets SIGKILL once `KILL_GRACE`
    /// passes; its exit is confirmed, and the row is `interrupted`.
    #[tokio::test]
    async fn reconcile_escalates_to_sigkill_and_confirms_the_exit() {
        use std::os::unix::process::ExitStatusExt;
        let e = setup(Script::Ready, None);
        let (mut proc, child) =
            test_process("trap '' TERM; echo ready; while :; do sleep 0.05; done");
        let r = stopped_agent_with(&e, child).await;
        let t0 = Instant::now();
        let done = ops::reconcile(&e.db, &e.home).unwrap();
        assert!(t0.elapsed() >= super::super::driver::KILL_GRACE);
        assert_eq!(done.killed, 1);
        assert!(done.kill_errors.is_empty(), "{:?}", done.kill_errors);
        let status = proc.wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert_eq!(row(&e.db, &r.id).state.as_deref(), Some("interrupted"));
    }

    /// D2: a recorded pid that now runs another process (another start
    /// time) is never signalled; the row is reconciled as gone.
    #[tokio::test]
    async fn reconcile_never_signals_a_reused_pid() {
        let e = setup(Script::Ready, None);
        let (mut proc, child) = test_process("echo ready; exec sleep 30");
        let stale = ChildId {
            start: child.start + 1,
            ..child
        };
        let r = stopped_agent_with(&e, stale).await;
        let done = ops::reconcile(&e.db, &e.home).unwrap();
        assert_eq!((done.killed, done.pid_reused), (0, 1));
        assert_eq!(done.interrupted, std::slice::from_ref(&r.id));
        std::thread::sleep(Duration::from_millis(100));
        assert!(proc.try_wait().unwrap().is_none(), "a reused pid was hit");
        proc.kill().unwrap();
        proc.wait().unwrap();
    }

    /// D2: a kill boot reconcile cannot confirm leaves the row `orphaned`
    /// with its PTY child and an audit row. It is never resumed, and
    /// `agent.resume` refuses it.
    #[tokio::test]
    async fn an_unconfirmed_reconcile_kill_orphans_the_agent() {
        let e = setup(Script::Ready, None);
        let child = ChildId {
            pid: 1_999_999_999,
            start: 7,
        };
        let r = stopped_agent_with(&e, child).await;
        let fails: &ops::Killer = &|_| Err(io::Error::other("still alive after SIGKILL"));
        let done = ops::reconcile_with(&e.db, &e.home, fails).unwrap();
        assert_eq!(done.orphaned, std::slice::from_ref(&r.id));
        assert!(done.interrupted.is_empty());
        let got = row(&e.db, &r.id);
        assert_eq!(got.state.as_deref(), Some("orphaned"));
        assert_eq!(got.state_reason.as_deref(), Some(ops::KILL_UNCONFIRMED));
        assert_eq!(got.gone_at, None);
        assert_eq!(agent_state_rows(&e.db), 1);
        let why = audit_reasons(&e.db, "engine.reconcile", ops::ORPHANED);
        assert_eq!(why.len(), 1);
        assert!(why[0].contains("pid 1999999999 start 7"), "{:?}", why);
        // Not resumable: not by boot, not by reconcile, not by hand.
        assert!(ops::reconcile_with(&e.db, &e.home, fails)
            .unwrap()
            .is_empty());
        let (_, l2, sp2) = next_engine(&e, Script::Ready, None);
        assert_eq!(sp2.resume_on_boot().unwrap(), Resumed::default());
        let err = sp2
            .resume(
                &Principal::LocalUnverified,
                serde_json::json!({ "id": r.id }),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ActionFailed);
        assert!(err.msg.contains("orphaned"), "{}", err.msg);
        assert_eq!(l2.spawns.load(Ordering::SeqCst), 0);
    }

    /// D2: a retire whose kill cannot be confirmed answers with the
    /// `orphaned` row, not a tombstone. A second retire kills again by pid
    /// and start time: still unconfirmed, it stays `orphaned` and is
    /// audited; confirmed, the row is the tombstone.
    #[tokio::test]
    async fn a_retire_with_an_unconfirmed_kill_orphans_then_retires() {
        let e = setup(Script::Ready, None);
        let l = Principal::LocalUnverified;
        let r = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
        e.launcher.kill_fails.store(true, Ordering::SeqCst);
        let id = serde_json::json!({ "id": r.id });
        let got = e.sp.retire(&l, id.clone()).await.unwrap();
        assert_eq!(got.state.as_deref(), Some("orphaned"));
        assert_eq!(got.gone_at, None);
        no_supervisors(&e.sp);
        assert_eq!(agent_state_rows(&e.db), 1);
        assert_eq!(audit_reasons(&e.db, "agent.retire", ops::ORPHANED).len(), 1);
        let err = e.sp.resume(&l, id.clone()).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ActionFailed);
        e.sp.set_orphan_killer(Arc::new(|_| Err(io::Error::other("still alive"))));
        let got = e.sp.retire(&l, id.clone()).await.unwrap();
        assert_eq!(got.state.as_deref(), Some("orphaned"));
        assert_eq!(audit_reasons(&e.db, "agent.retire", ops::ORPHANED).len(), 2);
        e.sp.set_orphan_killer(Arc::new(|_| Ok(Orphan::Killed)));
        let got = e.sp.retire(&l, id).await.unwrap();
        assert_eq!(got.state.as_deref(), Some("retired"));
        assert!(got.gone_at.is_some());
        assert_eq!(agent_state_rows(&e.db), 0);
    }

    /// D2: a create that fails after the spawn and cannot confirm the kill
    /// ends `orphaned`, not `failed`, and says so.
    #[tokio::test]
    async fn a_failed_create_with_an_unconfirmed_kill_is_orphaned() {
        let e = setup(Script::Refuse, None);
        e.launcher.kill_fails.store(true, Ordering::SeqCst);
        let err =
            e.sp.create(&Principal::LocalUnverified, create_params(&e.cwd))
                .await
                .unwrap_err();
        assert_eq!(err.code, ErrorCode::SpawnFailed);
        assert!(err.msg.contains("orphaned"), "{}", err.msg);
        let got = only_row(&e.db);
        assert_eq!(got.state.as_deref(), Some("orphaned"));
        assert_eq!(ops_of(&e.db)[0].2.as_deref(), Some(ops::ORPHANED));
    }

    /// D4: a failed resume can be tried again with `agent.resume`, which
    /// answers once the agent is ready and clears the reason.
    #[tokio::test]
    async fn agent_resume_succeeds_after_a_failed_resume() {
        let e = setup(Script::Ready, None);
        let r = drained_agent(&e).await;
        let (_, _, sp2) = next_engine(&e, Script::Refuse, None);
        sp2.resume_on_boot().unwrap();
        no_supervisors(&sp2);
        assert_eq!(
            row(&e.db, &r.id).state_reason.as_deref(),
            Some(ops::RESUME_FAILED)
        );
        let l = Principal::LocalUnverified;
        let id = serde_json::json!({ "id": r.id });
        // Still refused by the vendor: an error with its text, and the
        // row is back as it was, ready for the next try.
        let err = sp2.resume(&l, id.clone()).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::SpawnFailed);
        assert!(err.msg.contains("usage limit"), "{}", err.msg);
        no_supervisors(&sp2);
        let got = row(&e.db, &r.id);
        assert_eq!(got.state.as_deref(), Some("paused"));
        assert_eq!(got.state_reason.as_deref(), Some(ops::RESUME_FAILED));
        let (_, l3, sp3) = next_engine(&e, Script::Ready, None);
        let got = sp3.resume(&l, id.clone()).await.unwrap();
        assert_eq!(got.state.as_deref(), Some("ready"));
        assert_eq!(got.state_reason, None);
        assert_eq!(l3.spawns.load(Ordering::SeqCst), 1);
        assert_eq!(
            audit_reasons(&e.db, ops::KIND_RESUME, "allow")
                .last()
                .map(String::as_str),
            Some("resume from paused, generation 4")
        );
        // A running agent is not resumed again.
        let err = sp3.resume(&l, id).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ActionFailed);
        assert_eq!(l3.spawns.load(Ordering::SeqCst), 1);
        sp3.shutdown();
    }

    /// D4: `agent.resume` takes only a local client, in the authz table
    /// and in the spawner.
    #[tokio::test]
    async fn agent_resume_is_refused_for_non_local_principals() {
        let e = setup(Script::Ready, None);
        let r = drained_agent(&e).await;
        let a = Principal::Agent {
            id: r.id.clone(),
            gen: 1,
        };
        assert_eq!(
            super::super::authz::authorize("agent.resume", &a, None)
                .unwrap_err()
                .code,
            ErrorCode::NotAllowed
        );
        assert!(
            super::super::authz::authorize("agent.resume", &Principal::LocalUnverified, None)
                .is_ok()
        );
        let (_, l2, sp2) = next_engine(&e, Script::Ready, None);
        let err = sp2
            .resume(&a, serde_json::json!({ "id": r.id }))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotAllowed);
        assert_eq!(l2.spawns.load(Ordering::SeqCst), 0);
        assert_eq!(row(&e.db, &r.id).state.as_deref(), Some("paused"));
    }
    /// D4: a boot never retries a failed resume. The row stays `paused`
    /// with `resume_failed` until `agent.resume` or `agent.retire`.
    #[tokio::test]
    async fn boot_resume_skips_a_failed_resume() {
        let e = setup(Script::Ready, None);
        let r = drained_agent(&e).await;
        let (_, _, sp2) = next_engine(&e, Script::Refuse, None);
        sp2.resume_on_boot().unwrap();
        no_supervisors(&sp2);
        for _ in 0..2 {
            let (_, l, sp) = next_engine(&e, Script::Ready, None);
            assert_eq!(sp.resume_on_boot().unwrap(), Resumed::default());
            assert_eq!(l.spawns.load(Ordering::SeqCst), 0);
            let got = row(&e.db, &r.id);
            assert_eq!(got.state.as_deref(), Some("paused"));
            assert_eq!(got.state_reason.as_deref(), Some(ops::RESUME_FAILED));
            assert_eq!(got.gone_at, None);
        }
    }

    /// D2: an `orphaned` agent counts as live for the one-cwd rule. Only
    /// `agent.retire` ends it: the kill goes to the recorded pid and start
    /// time, here a test process, and is confirmed before the tombstone.
    #[tokio::test]
    async fn an_orphaned_agent_holds_its_cwd_until_retire_kills_it() {
        let e = setup(Script::Ready, None);
        let l = Principal::LocalUnverified;
        let r = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
        e.launcher.kill_fails.store(true, Ordering::SeqCst);
        let id = serde_json::json!({ "id": r.id });
        let got = e.sp.retire(&l, id.clone()).await.unwrap();
        assert_eq!(got.state.as_deref(), Some("orphaned"));
        no_supervisors(&e.sp);
        let (mut proc, child) = test_process("echo ready; exec sleep 30");
        let aid = r.id.clone();
        e.db.write_blocking(move |tx| {
            tx.execute(
                "UPDATE agent_state SET pty_child_pid = ?2, pty_child_start = ?3 \
                 WHERE agent_id = ?1",
                params![aid, child.pid as i64, child.start as i64],
            )
        })
        .unwrap();
        let other = e.t.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let err = e.sp.create(&l, create_params(&other)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::CwdCollision);
        assert!(err.msg.contains(&r.id), "{}", err.msg);
        let err = e.sp.resume(&l, id.clone()).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ActionFailed);
        assert!(err.msg.contains("orphaned"), "{}", err.msg);
        assert!(proc.try_wait().unwrap().is_none(), "resume signalled it");
        // The default killer: pid and start time, SIGTERM, confirmed.
        let got = e.sp.retire(&l, id).await.unwrap();
        assert_eq!(got.state.as_deref(), Some("retired"));
        assert!(got.gone_at.is_some());
        assert_eq!(proc.wait().unwrap().signal(), Some(libc::SIGTERM));
        assert_eq!(agent_state_rows(&e.db), 0);
        e.sp.create(&l, create_params(&other)).await.unwrap();
    }

    /// D2: the retire of an `orphaned` agent whose pid now runs another
    /// process (another start time) never signals it.
    #[tokio::test]
    async fn retiring_an_orphan_never_signals_a_reused_pid() {
        let e = setup(Script::Ready, None);
        let l = Principal::LocalUnverified;
        let r = e.sp.create(&l, create_params(&e.cwd)).await.unwrap();
        e.launcher.kill_fails.store(true, Ordering::SeqCst);
        let id = serde_json::json!({ "id": r.id });
        e.sp.retire(&l, id.clone()).await.unwrap();
        no_supervisors(&e.sp);
        let (mut proc, child) = test_process("echo ready; exec sleep 30");
        let aid = r.id.clone();
        e.db.write_blocking(move |tx| {
            tx.execute(
                "UPDATE agent_state SET pty_child_pid = ?2, pty_child_start = ?3 \
                 WHERE agent_id = ?1",
                params![aid, child.pid as i64, child.start as i64 + 1],
            )
        })
        .unwrap();
        let got = e.sp.retire(&l, id).await.unwrap();
        assert_eq!(got.state.as_deref(), Some("retired"));
        std::thread::sleep(Duration::from_millis(100));
        assert!(proc.try_wait().unwrap().is_none(), "a reused pid was hit");
        proc.kill().unwrap();
        proc.wait().unwrap();
    }

    /// D4: a second `agent.resume` while the first is open fails with a
    /// clear error, in the same engine (a supervisor) and in another one
    /// (the row is no longer paused). One session never runs twice.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_agent_resume_while_one_is_open_is_refused() {
        let e = setup(Script::Ready, None);
        let r = drained_agent(&e).await;
        let (_, l2, sp2) = next_engine(&e, Script::Hang, None);
        let l = Principal::LocalUnverified;
        let id = serde_json::json!({ "id": r.id });
        let first = tokio::spawn({
            let (sp2, id) = (sp2.clone(), id.clone());
            async move { sp2.resume(&Principal::LocalUnverified, id).await }
        });
        let rid = r.id.clone();
        let db = e.db.clone();
        tokio::task::spawn_blocking(move || {
            wait_for("the resume spawned", || {
                row(&db, &rid).state.as_deref() == Some("starting")
            })
        })
        .await
        .unwrap();
        let err = sp2.resume(&l, id.clone()).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ActionFailed);
        assert!(err.msg.contains("runs in this engine"), "{}", err.msg);
        let (_, l3, sp3) = next_engine(&e, Script::Ready, None);
        let err = sp3.resume(&l, id.clone()).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ActionFailed);
        assert!(err.msg.contains("not paused or interrupted"), "{}", err.msg);
        assert_eq!(l2.spawns.load(Ordering::SeqCst), 1);
        assert_eq!(l3.spawns.load(Ordering::SeqCst), 0);
        // A retire ends the first; it answers with an error.
        sp2.retire(&l, id).await.unwrap();
        assert!(first.await.unwrap().is_err());
    }
}

fn socket_awaiting(id: String, now: Instant) -> Delivery {
    Delivery::SocketAwaiting {
        id,
        since: now,
        enqueued: None,
        turn_ended: None,
        held: None,
        receipt_seq: 0,
    }
}
