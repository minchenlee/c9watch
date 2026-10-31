//! `c9watch engine`: the engine process itself.
//!
//! Order matters: the signal handlers go in before the bind, so a SIGTERM
//! or SIGINT from then on always reaches the cleanup; the lifetime lock goes
//! in before the bind, so only the lock holder can replace a leftover
//! socket, and before the machine key and the db, since creating the key
//! and opening the db (which may move a corrupt db aside) are both
//! lock-holder work. The legacy files are imported from a snapshot copy on
//! a thread of their own once the socket is bound, so a large legacy tree
//! never holds up readiness; a failed import only logs. Detection starts
//! once the db is open. Until its first tick (or the rebuild after a new
//! epoch) is written, `session.list` and `snapshot.get` wait for it
//! (`detect::FirstTick`), so a client that just started the engine never
//! reads an empty or stale table. The engine never talks to the legacy
//! `c9watch daemon` and never writes its files.
//!
//! `c9watch-agent` is copied into `<home>/bin` before the socket is bound,
//! so the first hook-class call already finds it (`engine::agent_bin`). A
//! missing source only logs: the engine runs without agents. The default
//! `<home>/standing-orders.md` is written on the first start
//! (`engine::standing`); a failure only logs.
//!
//! Boot reconcile (`engine::ops`) runs after the db opens and before the
//! bind, so no mutation can arrive before it is done; a failed reconcile
//! stops the boot. `agent.create` is always on (C1a-26; C1a-35 dropped the
//! experimental switch). On shutdown every agent the engine runs is killed
//! before the db closes; the next boot reconciles their rows.
//!
//! SIGTERM drains first (C1a-27, `engine::drain`): the socket keeps serving,
//! so hooks still arrive, while ready agents are paused and mid-turn agents
//! get up to 60 s; the engine exits by the 90 s hard deadline. SIGINT and
//! `run_until`'s `stop` skip the drain.
//!
//! Boot resume (C1a-28, `Spawner::resume_on_boot`) runs after boot
//! reconcile and after the bind, so the resumed agents' SessionStart hooks
//! find the socket: every `paused(drain)` and `interrupted` agent gets a
//! new generation and comes up on `claude --resume` on its supervisor's
//! thread. While the engine drains the rows stay as they are. A failed read
//! or write logs and the engine runs on.

use super::agent_bin::{self, AgentBin};
use super::agents::LiveTable;
use super::api::{serve, ServerCtx};
use super::create::{Spawner, SpawnerConfig};
use super::db;
use super::detect;
use super::drain::{self, DrainSwitch};
use super::fsutil::ensure_private_dir;
use super::home::{lock_path, sock_path};
use super::import;
use super::lock::acquire_engine_lock;
use super::machine;
use super::mail;
use super::ops;
use super::sock::{bind_private, ShutdownSignal, ShutdownSignals};
use super::standing;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixListener;

/// How often event retention runs (it also runs at boot).
const PRUNE_EVERY: Duration = Duration::from_secs(60);

/// What one engine runs with.
pub struct EngineConfig {
    /// The engine home (`engine::home`).
    pub home: PathBuf,
    /// Where the engine copies `c9watch-agent` from. `new` sets it next to
    /// `realpath(current_exe())`; there is no env override. Integration
    /// tests set `env!("CARGO_BIN_EXE_c9watch-agent")` here.
    pub agent_source: PathBuf,
    /// The live agents. Spawns register here (M5).
    pub agents: Arc<LiveTable>,
    /// Run session detection and the legacy import. Off only in tests of
    /// the agent path, which must not scan the machine.
    pub detection: bool,
    /// The `claude` binary for free agents. `None` looks it up on `PATH`
    /// at each create; tests set a script here.
    pub claude: Option<PathBuf>,
}

impl EngineConfig {
    pub fn new(home: PathBuf) -> Result<Self, String> {
        Ok(EngineConfig {
            home,
            agent_source: agent_bin::default_source()?,
            agents: Arc::new(LiveTable::new()),
            detection: true,
            claude: None,
        })
    }
}

/// Run the engine until SIGTERM or SIGINT. Must run inside a tokio runtime.
/// Fails with `engine_already_running` when another engine holds the lock.
pub async fn run(config: &EngineConfig) -> Result<(), String> {
    run_until(config, std::future::pending()).await
}

/// `run`, which also stops when `stop` completes.
pub async fn run_until(
    config: &EngineConfig,
    stop: impl std::future::Future<Output = ()>,
) -> Result<(), String> {
    let home = config.home.as_path();
    ensure_private_dir(home)?;
    let sock = sock_path(home);
    super::home::prepare_socket_dir(home, &sock)?;
    let mut signals = ShutdownSignals::install()
        .map_err(|e| format!("Failed to install signal handlers: {}", e))?;
    let lock = acquire_engine_lock(&lock_path(home))?;
    let key = machine::load_or_create(home)?;
    let header_key = key.header_key();
    let db = Arc::new(db::open(home, key.id())?);
    let agent_bin = Arc::new(AgentBin::install(home, &config.agent_source));
    match (agent_bin.sha256_hex(), agent_bin.missing_reason()) {
        (Some(sha), _) => eprintln!("[engine] Installed {:?} (sha256 {})", agent_bin.path(), sha),
        (None, why) => eprintln!(
            "[engine] No agent binary, agents cannot start: {}",
            why.unwrap_or_default()
        ),
    }
    match standing::ensure(home) {
        Ok(true) => eprintln!("[engine] Wrote the default {:?}", standing::path(home)),
        Ok(false) => {}
        Err(e) => eprintln!("[engine] {}", e),
    }
    let mail_merge = Arc::new(super::merge::MergeTable::new());
    let mail_sockets = super::cc_listener::Sockets::new(db.clone(), mail_merge.clone());
    let _mail_socket_cleanup = super::cc_listener::SocketCleanup(mail_sockets.clone());
    if let Err(e) = mail_sockets.bind(&super::cc_inbox::default_socket_dir()) {
        eprintln!("[engine] CC return socket unavailable: {}", e);
    }
    let reconciled = ops::reconcile(&db, home)?;
    if !reconciled.is_empty() {
        eprintln!("[engine] Boot reconcile: {:?}", reconciled);
    }
    // No supervisor holds mail now (C1b-M1 §4.9): after `ops::reconcile`,
    // which fails the agents that did not survive.
    let machine_id = key.id().to_string();
    let (mail, _) = db
        .write_blocking(move |tx| mail::reconcile_on_boot(tx, &machine_id, db::now_ms()))
        .map_err(|e| format!("Failed to reconcile the mail: {}", e))?;
    if mail != mail::Ended::default() {
        eprintln!("[engine] Mail reconcile: {:?}", mail);
    }
    let (std_listener, socket) = bind_private(&sock, &lock)?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| format!("Failed to set socket non-blocking: {}", e))?;
    let listener = UnixListener::from_std(std_listener)
        .map_err(|e| format!("Failed to register Unix socket: {}", e))?;

    let drain_switch = Arc::new(DrainSwitch::new());
    let first_tick = Arc::new(if config.detection {
        detect::FirstTick::pending()
    } else {
        detect::FirstTick::open_now()
    });
    let detection = detect::Detection::new(db.clone());
    let mut ctx = ServerCtx::new(key.id().to_string())
        .with_first_tick(first_tick.clone())
        .with_agents(config.agents.clone())
        .with_agent_bin(agent_bin.clone())
        .with_db(db.clone())
        .with_standing_orders(standing::path(home))
        .with_drain(drain_switch.clone())
        .with_header_key(header_key.clone())
        .with_cost_cache(home.join("cost-cache.json"))
        .with_usage(crate::engine::views::usage::live_source());
    ctx.merge = mail_merge;
    if config.detection {
        ctx = ctx.with_detection(detection.clone());
    }
    let spawner = Arc::new(
        Spawner::new(
            SpawnerConfig {
                home: home.to_path_buf(),
                sock: socket.path().to_path_buf(),
                claude: config.claude.clone(),
            },
            db.clone(),
            config.agents.clone(),
            ctx.merge.clone(),
            agent_bin,
        )
        .with_drain(drain_switch)
        .with_header_key(header_key)
        .with_mail_sockets(mail_sockets.clone()),
    );
    mail_sockets.set_spawner(&spawner);
    let ctx = Arc::new(ctx.with_spawner(spawner.clone()));
    match spawner.resume_on_boot() {
        Ok(r) if r == Default::default() => {}
        Ok(r) => eprintln!(
            "[engine] Boot resume: started {:?}, failed {:?}, left {:?}{}",
            r.started,
            r.failed,
            r.left,
            if r.left.is_empty() {
                ""
            } else {
                " (the engine drains, or a write failed)"
            }
        ),
        Err(e) => eprintln!("[engine] Boot resume: {}", e),
    }
    let pruner = tokio::spawn(prune_loop(db.clone()));
    let detection = if config.detection {
        let d = detect::spawn(detection, first_tick)?;
        spawn_import(db.clone(), home.to_path_buf());
        Some(d)
    } else {
        None
    };
    eprintln!(
        "[engine] Listening on {:?}, pid={}, machine={}, epoch={}",
        socket.path(),
        std::process::id(),
        key.id(),
        db.epoch()
    );

    let serving = serve(listener, ctx);
    tokio::pin!(serving);
    tokio::pin!(stop);
    let sig = tokio::select! {
        _ = &mut serving => None,
        sig = signals.recv() => Some(sig),
        _ = &mut stop => None,
    };
    // The hard deadline counts from the SIGTERM.
    let deadline = tokio::time::Instant::now() + drain::HARD_DEADLINE;
    if sig == Some(ShutdownSignal::Term) {
        // Keep serving while the drain runs: hooks still come in, and get
        // no context. A SIGINT or `stop` ends the drain early; another
        // SIGTERM (a second `engine restart`) does not (reviews M3).
        let drain = run_drain(&spawner);
        tokio::pin!(drain);
        loop {
            tokio::select! {
                _ = &mut serving => {
                    eprintln!("[engine] The listener failed during the drain");
                    break;
                }
                _ = &mut drain => break,
                sig = signals.recv() => {
                    if sig == ShutdownSignal::Term {
                        eprintln!("[engine] SIGTERM again: still draining");
                        continue;
                    }
                    eprintln!("[engine] {:?} during the drain", sig);
                    break;
                }
                _ = &mut stop => break,
            }
        }
    }
    eprintln!("[engine] Shutting down ({:?})", sig);
    pruner.abort();
    // The agents go before the db closes, so their supervisors can still
    // write; what they leave is reconciled at the next boot. After a drain
    // only what it left is still running, and the hard deadline holds:
    // a supervisor stuck past it is not waited for.
    let shut = tokio::task::spawn_blocking(move || spawner.shutdown());
    if sig == Some(ShutdownSignal::Term) {
        if tokio::time::timeout_at(deadline, shut).await.is_err() {
            eprintln!(
                "[engine] Hard deadline of {} s: exiting with agents still stopping",
                drain::HARD_DEADLINE.as_secs()
            );
        }
    } else {
        let _ = shut.await;
    }
    // Stops the detection thread after its current tick. A tick that is
    // still writing when the db closes fails, and nothing waits for it.
    drop(detection);
    // Unlink our socket first, then close the db, then release the lock, so
    // a new engine that takes the lock never sees our socket as live or our
    // db as open. Connection tasks may still hold the `Db`; `close` shuts
    // its writer anyway.
    drop(socket);
    mail_sockets.close();
    db.close();
    drop(lock);
    Ok(())
}

/// Turn the drain switch on and drain the agents (`engine::drain`).
async fn run_drain(spawner: &Arc<Spawner>) {
    spawner.drain_switch().start();
    let start = spawner.now();
    eprintln!(
        "[engine] SIGTERM: draining {} agent(s), up to {} s",
        spawner.live_count(),
        drain::HARD_DEADLINE.as_secs()
    );
    let sp = spawner.clone();
    match tokio::task::spawn_blocking(move || drain::run(&sp, drain::Timing::default(), start))
        .await
    {
        Ok(r) => eprintln!(
            "[engine] Drained: paused {:?}, interrupted {:?}, left to boot reconcile {:?}, \
             unsettled {:?}{}",
            r.paused,
            r.interrupted,
            r.left,
            r.unsettled,
            if r.timed_out {
                ", deadline reached"
            } else {
                ""
            }
        ),
        Err(e) => eprintln!("[engine] The drain failed: {}", e),
    }
}

/// Import the legacy files on their own thread. A thread, not a blocking
/// task, so an import in flight never holds up the runtime's shutdown; one
/// still writing when the db closes fails and logs.
fn spawn_import(db: Arc<db::Db>, home: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("engine-import".into())
        .spawn(move || import_legacy(&db, &home));
    if let Err(e) = spawned {
        eprintln!("[engine] Failed to start the legacy import: {}", e);
    }
}

/// Import `~/.claude/c9watch` (see `engine::import`). Blocking.
fn import_legacy(db: &db::Db, home: &Path) {
    let Some(source) = import::legacy_root() else {
        return;
    };
    match import::import(db, home, &source) {
        Ok(r) if !r.is_empty() => eprintln!(
            "[engine] Imported legacy files: {} copied, {} new rows, {} changed, {} events, \
             {} symlinks and {} other files skipped, {} unreadable",
            r.copied,
            r.created.len(),
            r.updated.len(),
            r.events,
            r.symlinks,
            r.skipped,
            r.bad
        ),
        Ok(_) => {}
        Err(e) => eprintln!("[engine] Legacy import failed: {}", e),
    }
}

async fn prune_loop(db: Arc<db::Db>) {
    let mut every = tokio::time::interval(PRUNE_EVERY);
    // The first tick is immediate; boot already pruned.
    every.tick().await;
    loop {
        every.tick().await;
        if let Err(e) = db.prune().await {
            eprintln!("[engine] {}", e);
        }
    }
}
