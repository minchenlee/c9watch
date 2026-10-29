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
//! once the db is open, so its first tick (or the rebuild after a new epoch)
//! is written before clients ask. The engine never talks to the legacy
//! `c9watch daemon` and never writes its files.

use super::agents::LiveTable;
use super::api::{serve, ServerCtx};
use super::db;
use super::detect;
use super::fsutil::ensure_private_dir;
use super::home::{lock_path, sock_path};
use super::import;
use super::lock::acquire_engine_lock;
use super::machine;
use super::sock::{bind_private, ShutdownSignals};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixListener;

/// How often event retention runs (it also runs at boot).
const PRUNE_EVERY: Duration = Duration::from_secs(60);

/// Run the engine in `home` until SIGTERM or SIGINT. Must run inside a tokio
/// runtime. Fails with `engine_already_running` when another engine holds
/// the lock.
pub async fn run(home: &Path) -> Result<(), String> {
    ensure_private_dir(home)?;
    let mut signals = ShutdownSignals::install()
        .map_err(|e| format!("Failed to install signal handlers: {}", e))?;
    let lock = acquire_engine_lock(&lock_path(home))?;
    let key = machine::load_or_create(home)?;
    let db = Arc::new(db::open(home, key.id())?);
    let (std_listener, socket) = bind_private(&sock_path(home), &lock)?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| format!("Failed to set socket non-blocking: {}", e))?;
    let listener = UnixListener::from_std(std_listener)
        .map_err(|e| format!("Failed to register Unix socket: {}", e))?;

    // The engine's live agents: empty until an agent is spawned (M5).
    let agents = Arc::new(LiveTable::new());
    let ctx = Arc::new(
        ServerCtx::new(key.id().to_string())
            .with_agents(agents)
            .with_db(db.clone()),
    );
    let pruner = tokio::spawn(prune_loop(db.clone()));
    let detection = detect::spawn(db.clone())?;
    spawn_import(db.clone(), home.to_path_buf());
    eprintln!(
        "[engine] Listening on {:?}, pid={}, machine={}, epoch={}",
        socket.path(),
        std::process::id(),
        key.id(),
        db.epoch()
    );

    let sig = tokio::select! {
        _ = serve(listener, ctx) => None,
        sig = signals.recv() => Some(sig),
    };
    eprintln!("[engine] Shutting down ({:?})", sig);
    pruner.abort();
    // Stops the detection thread after its current tick. A tick that is
    // still writing when the db closes fails, and nothing waits for it.
    drop(detection);
    // Unlink our socket first, then close the db, then release the lock, so
    // a new engine that takes the lock never sees our socket as live or our
    // db as open. Connection tasks may still hold the `Db`; `close` shuts
    // its writer anyway.
    drop(socket);
    db.close();
    drop(lock);
    Ok(())
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
