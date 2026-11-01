//! The engine store: `<home>/engine.db`, SQLite in WAL mode (A:340).
//!
//! Boot order in `open`:
//! 1. A db newer than this engine is refused with `db_newer_than_engine`
//!    and left as it is. The file header is checked before the db is
//!    opened, and the open db again before anything writes to it.
//! 2. `PRAGMA quick_check`. A db that fails it (or is not a db at all) is
//!    moved to `<home>/backups/corrupt-<ms>.db` and a new one is created,
//!    which starts a new epoch (A:497). Detection rebuilds the observed
//!    agents from provider data on its next tick (`engine::detect`).
//! 3. Migrations, each after a `VACUUM INTO` backup (see `migrate`).
//! 4. This machine's row in `machines` (`is_self`), keyed by the machine
//!    key id. A new self row is audited.
//! 5. Event retention runs once (see `outbox`), and audit retention (see
//!    `audit::AuditRetention`).
//! 6. The writable connection moves to the writer thread (see `writer`).
//!
//! State changes go through `Db::write`, which runs on the writer thread in
//! one transaction and can emit events into the outbox in that same
//! transaction.
//!
//! Call `open` only while holding the engine lock: it may move the db.

pub mod audit;
pub mod migrate;
pub mod outbox;
pub mod writer;

use super::fsutil::ensure_private_dir;
use super::proto::AgentRow;
use audit::{AuditEntry, AuditRetention};
use migrate::{check_not_newer, latest_version, migrate, MIGRATIONS};
use outbox::{Cursor, NewEvent, ReadError, Subscription};
use rusqlite::{params, Connection, ErrorCode, OpenFlags, Transaction};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use writer::Writer;

/// The db file inside the engine home.
pub const DB_NAME: &str = "engine.db";
/// Migration backups and moved-aside dbs.
pub const BACKUPS_DIR: &str = "backups";

/// How long a reader waits for a lock (only the WAL checkpoint takes one).
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Idle read-only connections `Db::pooled_reader` keeps for reuse.
const READER_POOL_IDLE: usize = 8;

/// `<home>/engine.db`
pub fn db_path(home: &Path) -> PathBuf {
    home.join(DB_NAME)
}

/// `<home>/backups`
pub fn backups_dir(home: &Path) -> PathBuf {
    home.join(BACKUPS_DIR)
}

/// An open store.
pub struct Db {
    path: PathBuf,
    machine_id: String,
    epoch: String,
    moved_aside: Option<PathBuf>,
    writer: Writer,
    /// The newest committed event `seq`, sent after each commit that emitted
    /// events. Subscribers wake on it and read the rows from SQLite.
    head: Arc<watch::Sender<i64>>,
    /// Idle read-only connections for `pooled_reader`.
    readers: Arc<Mutex<Vec<Connection>>>,
    audit_retention: AuditRetention,
}

/// What `open_with` takes beyond the home and machine id.
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    pub audit_retention: AuditRetention,
}

/// A read-only connection borrowed from the `Db`'s pool. Derefs to
/// `Connection`, so code written for `Db::reader` works on it unchanged
/// (`&reader` coerces to `&Connection`). On drop it goes back to the pool,
/// unless it is inside a transaction (a `BEGIN` without its `COMMIT`) or
/// the pool already has `READER_POOL_IDLE` idle connections.
pub struct PooledReader {
    conn: Option<Connection>,
    pool: Arc<Mutex<Vec<Connection>>>,
}

impl std::ops::Deref for PooledReader {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("pooled reader used after drop")
    }
}

impl Drop for PooledReader {
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else {
            return;
        };
        if !conn.is_autocommit() {
            return; // An open read transaction: close it, never reuse it.
        }
        let mut idle = self.pool.lock().unwrap_or_else(|p| p.into_inner());
        if idle.len() < READER_POOL_IDLE {
            idle.push(conn);
        }
    }
}

/// A write transaction on the writer thread. Derefs to the connection, so
/// state changes run on it directly; `emit` adds an event to the same
/// transaction.
pub struct Tx<'a> {
    tx: Transaction<'a>,
    epoch: String,
    last: Option<i64>,
    after_commit: Vec<Box<dyn FnOnce() + Send>>,
}

impl Tx<'_> {
    /// Insert `e` into the outbox. It becomes visible, and is broadcast,
    /// only when the transaction commits.
    pub fn emit(&mut self, e: &NewEvent) -> rusqlite::Result<i64> {
        let seq = outbox::append(&self.tx, &self.epoch, now_ms(), e)?;
        self.last = Some(seq);
        Ok(seq)
    }

    /// The db's epoch, for a cursor that has to be stored in the same
    /// transaction as its event.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Run `f` on the writer thread once the transaction commits, before
    /// the next write starts. For memory that must not move ahead of the db
    /// (`MergeTable::publish_on_commit`).
    ///
    /// - If `f` (the closure of `write`) fails or the commit fails, the
    ///   hooks are dropped and never run.
    /// - A hook holds up every write behind it: keep it short, and never
    ///   call `Db` or the writer from it. A writer call from inside a job
    ///   is a hard error.
    /// - A hook that panics is logged and skipped. The hooks after it
    ///   still run, the new head is still sent to subscribers, and the
    ///   caller still gets its committed result.
    pub fn after_commit(&mut self, f: impl FnOnce() + Send + 'static) {
        self.after_commit.push(Box::new(f));
    }
}

impl std::ops::Deref for Tx<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.tx
    }
}

impl Db {
    /// The machine key id this db was opened for.
    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }

    /// Changes only when the db is recreated. Part of every cursor
    /// (`machine:epoch:seq`).
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Where a db that failed `quick_check` at this boot was moved.
    pub fn moved_aside(&self) -> Option<&Path> {
        self.moved_aside.as_deref()
    }

    pub fn writer(&self) -> &Writer {
        &self.writer
    }

    /// Run `f` in one write transaction on the writer thread. It commits
    /// when `f` returns `Ok` and rolls back otherwise. Returns `f`'s value
    /// and the cursor of the last event it emitted.
    pub async fn write<R, F>(&self, f: F) -> Result<(R, Option<Cursor>), String>
    where
        F: FnOnce(&mut Tx) -> rusqlite::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let job = tx_job(self.epoch.clone(), self.head.clone(), f);
        let (r, seq) = self.writer.call(job).await??;
        Ok((r, seq.map(|s| self.cursor_at(s))))
    }

    /// `write` for code outside the async runtime (boot, tests).
    pub fn write_blocking<R, F>(&self, f: F) -> Result<(R, Option<Cursor>), String>
    where
        F: FnOnce(&mut Tx) -> rusqlite::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let job = tx_job(self.epoch.clone(), self.head.clone(), f);
        let (r, seq) = self.writer.call_blocking(job)??;
        Ok((r, seq.map(|s| self.cursor_at(s))))
    }

    /// Run audit retention (see `audit::AuditRetention`) on the writer
    /// thread. Returns how many audit rows went. The engine runs it at boot
    /// (in `open`) and then every `audit::PRUNE_EVERY`.
    pub async fn prune_audit(&self) -> Result<usize, String> {
        let keep = self.audit_retention.clone();
        self.writer
            .call(move |c| audit::prune(c, now_ms(), &keep))
            .await?
            .map_err(|e| format!("Failed to prune the audit log: {}", e))
    }

    /// Run event retention (24 h / 50k) and the dedupe window on the
    /// writer thread. Returns how many events went.
    pub async fn prune(&self) -> Result<usize, String> {
        self.writer
            .call(|c| {
                let now = now_ms();
                prune_dedupe(c, now)?;
                outbox::prune(c, now)
            })
            .await?
            .map_err(|e| format!("Failed to prune events: {}", e))
    }

    /// Finish the queued writes and close the writable connection; see
    /// `Writer::close`. Readers and subscribers may still hold the `Db`.
    pub fn close(&self) {
        self.writer.close();
    }

    /// The cursor for `seq` in this db.
    pub fn cursor_at(&self, seq: i64) -> Cursor {
        Cursor {
            machine: self.machine_id.clone(),
            epoch: self.epoch.clone(),
            seq,
        }
    }

    /// The cursor of the newest committed event.
    pub fn head_cursor(&self) -> Cursor {
        self.cursor_at(*self.head.borrow())
    }

    /// Check that `since` belongs to this db and can be read on from
    /// without a gap. Returns its `seq`. Blocking (one short read).
    pub fn check_cursor(&self, since: &Cursor) -> Result<i64, ReadError> {
        let conn = self.pooled_reader().map_err(ReadError::Db)?;
        outbox::check_cursor(&conn, since, &self.machine_id, &self.epoch)
    }

    /// Follow the log from `after`, reading `batch` rows at a time.
    pub fn subscribe(&self, after: i64, batch: usize) -> Result<Subscription, String> {
        // Take the receiver before the reader's first read (A:218).
        let rx = self.head.subscribe();
        Ok(Subscription::new(self.reader()?, rx, after, batch))
    }

    /// The state for `snapshot.get` and the cursor it is consistent with,
    /// read in one read transaction. Blocking.
    pub fn snapshot(&self) -> Result<(Cursor, Vec<AgentRow>), String> {
        let conn = self.pooled_reader()?;
        let fail = |e: rusqlite::Error| format!("Failed to read the snapshot: {}", e);
        conn.execute_batch("BEGIN").map_err(fail)?;
        let read = (|| {
            let seq = outbox::head(&conn)?;
            let agents = agent_rows(&conn, AgentFilter::All)?;
            Ok((seq, agents))
        })();
        conn.execute_batch("COMMIT").map_err(fail)?;
        let (seq, agents) = read.map_err(fail)?;
        Ok((self.cursor_at(seq), agents))
    }

    /// The `agents` rows that pass `filter`. Blocking (one short read).
    pub fn agents(&self, filter: AgentFilter) -> Result<Vec<AgentRow>, String> {
        let conn = self.pooled_reader()?;
        agent_rows(&conn, filter).map_err(|e| format!("Failed to read agents: {}", e))
    }

    /// A read-only connection from a small pool: cheap to call often, for
    /// example several times per tick. Opening a SQLite connection costs a
    /// file open, a lock-file check and a schema parse; a pooled one only a
    /// mutex. Use it for short reads and drop it soon after (see
    /// `PooledReader`): it is the drop-in replacement for `reader()` where
    /// the connection does not have to be owned (`db.reader()?` becomes
    /// `db.pooled_reader()?`, with no other change at most call sites).
    ///
    /// A long-lived thread (an agent supervisor) may also keep one
    /// `PooledReader` for its whole life instead of borrowing per read; it
    /// stays valid after `close` (WAL readers do not need the writer). A
    /// reader that sees an error is still fine to reuse: SQLite errors do
    /// not poison a connection.
    pub fn pooled_reader(&self) -> Result<PooledReader, String> {
        let idle = self.readers.lock().unwrap_or_else(|p| p.into_inner()).pop();
        let conn = match idle {
            Some(c) => c,
            None => self.reader()?,
        };
        Ok(PooledReader {
            conn: Some(conn),
            pool: self.readers.clone(),
        })
    }

    /// A new read-only connection the caller owns. WAL lets readers run
    /// beside the writer. Prefer `pooled_reader` on hot paths; use this when
    /// the connection must be moved or kept (a `Subscription`).
    pub fn reader(&self) -> Result<Connection, String> {
        let conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("Failed to open db reader {:?}: {}", self.path, e))?;
        conn.busy_timeout(BUSY_TIMEOUT)
            .map_err(|e| format!("Failed to set busy_timeout: {}", e))?;
        Ok(conn)
    }
}

/// Open (or create) the store in `home` for the machine `machine_id` (the
/// machine key id, see `engine::machine`).
pub fn open(home: &Path, machine_id: &str) -> Result<Db, String> {
    open_with(home, machine_id, OpenOptions::default())
}

/// `open` with explicit options (audit retention).
pub fn open_with(home: &Path, machine_id: &str, opts: OpenOptions) -> Result<Db, String> {
    ensure_private_dir(home)?;
    let backups = backups_dir(home);
    ensure_private_dir(&backups)?;
    let path = db_path(home);
    // Before anything opens the file. A newer engine's schema may not parse
    // in this SQLite, and `quick_check` would then call the db corrupt and
    // move it aside.
    if let Some(have) = migrate::header_user_version(&path)? {
        if have > latest_version() {
            return Err(migrate::newer_than_engine(have, latest_version()));
        }
    }

    let mut moved_aside = None;
    let mut conn = match open_checked(&path)? {
        Some(conn) => conn,
        None => {
            let dest = backups.join(format!("corrupt-{}.db", now_ms()));
            move_aside(&path, &dest)?;
            crate::elog!(
                Warn,
                "db",
                "{:?} failed quick_check; moved to {:?}, starting a new epoch",
                path,
                dest
            );
            moved_aside = Some(dest);
            open_checked(&path)?.ok_or_else(|| format!("New db {:?} failed quick_check", path))?
        }
    };

    // Again on the open db, which also sees a version that is still in the
    // WAL, and before `configure` writes to it.
    check_not_newer(&conn, MIGRATIONS)?;
    configure(&conn)?;
    migrate(&mut conn, MIGRATIONS, &backups)?;
    let epoch = ensure_epoch(&conn)?;
    register_self(&mut conn, machine_id)?;
    outbox::prune(&conn, now_ms()).map_err(|e| format!("Failed to prune events: {}", e))?;
    prune_dedupe(&conn, now_ms()).map_err(|e| format!("Failed to prune dedupe: {}", e))?;
    audit::prune(&conn, now_ms(), &opts.audit_retention)
        .map_err(|e| format!("Failed to prune the audit log: {}", e))?;
    let head = outbox::head(&conn).map_err(|e| format!("Failed to read the event head: {}", e))?;
    Ok(Db {
        path,
        machine_id: machine_id.to_string(),
        epoch,
        moved_aside,
        writer: Writer::spawn(conn)?,
        head: Arc::new(watch::channel(head).0),
        readers: Arc::new(Mutex::new(Vec::new())),
        audit_retention: opts.audit_retention,
    })
}

type TxResult<R> = Result<(R, Option<i64>), String>;

/// The writer job behind `Db::write`: run `f` in a transaction, commit, and
/// only then broadcast the new head.
fn tx_job<R, F>(
    epoch: String,
    head: Arc<watch::Sender<i64>>,
    f: F,
) -> impl FnOnce(&mut Connection) -> TxResult<R> + Send + 'static
where
    F: FnOnce(&mut Tx) -> rusqlite::Result<R> + Send + 'static,
    R: Send + 'static,
{
    move |c| {
        let fail = |e: rusqlite::Error| format!("Write failed: {}", e);
        let mut tx = Tx {
            tx: c.transaction().map_err(fail)?,
            epoch,
            last: None,
            after_commit: Vec::new(),
        };
        // On error `tx` drops here, which rolls back.
        let r = f(&mut tx).map_err(fail)?;
        let last = tx.last;
        let after_commit = std::mem::take(&mut tx.after_commit);
        tx.tx.commit().map_err(fail)?;
        for f in after_commit {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
                crate::elog!(
                    Warn,
                    "db",
                    "an after-commit hook panicked; the commit stands"
                );
            }
        }
        if let Some(seq) = last {
            head.send_replace(seq);
        }
        Ok((r, last))
    }
}

/// Which `agents` rows a read returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentFilter {
    /// Every row: gone, retired, failed and tombstones too.
    All,
    /// Not gone, and not retired or failed.
    Live,
    /// Rows whose session detection sees now (`session.list`).
    Sessions,
}

impl AgentFilter {
    fn sql(self) -> &'static str {
        match self {
            AgentFilter::All => "1",
            AgentFilter::Live => {
                "gone_at IS NULL AND (state IS NULL OR state NOT IN ('retired', 'failed'))"
            }
            AgentFilter::Sessions => "gone_at IS NULL AND session_key IS NOT NULL",
        }
    }
}

/// The `agents` columns of an `AgentRow`, in its field order. Named, not
/// `*`, so a schema change has to change `AgentRow`, and with it the c9p/1
/// contract.
pub const AGENT_COLUMNS: &str = "id, generation, mode, role, control, provider, provider_sid, \
     source, created_by, gone_at, resume_count, interrupted_note_pending, session_key, \
     machine_id, state, status, cwd, name, pid, started_at, created_at, last_seen, parent_id, \
     state_reason";

/// Read one row selected with `AGENT_COLUMNS`.
pub fn agent_row(r: &rusqlite::Row) -> rusqlite::Result<AgentRow> {
    Ok(AgentRow {
        id: r.get(0)?,
        generation: r.get(1)?,
        mode: r.get(2)?,
        role: r.get(3)?,
        control: r.get(4)?,
        provider: r.get(5)?,
        provider_sid: r.get(6)?,
        source: r.get(7)?,
        created_by: r.get(8)?,
        gone_at: r.get(9)?,
        resume_count: r.get(10)?,
        interrupted_note_pending: r.get::<_, i64>(11)? != 0,
        session_key: r.get(12)?,
        machine_id: r.get(13)?,
        state: r.get(14)?,
        status: r.get(15)?,
        cwd: r.get(16)?,
        name: r.get(17)?,
        pid: r.get(18)?,
        started_at: r.get(19)?,
        created_at: r.get(20)?,
        last_seen: r.get(21)?,
        parent_id: r.get(22)?,
        state_reason: r.get(23)?,
    })
}

/// The `agents` rows that pass `filter`, in id (creation) order.
pub fn agent_rows(conn: &Connection, filter: AgentFilter) -> rusqlite::Result<Vec<AgentRow>> {
    let mut st = conn.prepare(&format!(
        "SELECT {} FROM agents WHERE {} ORDER BY id",
        AGENT_COLUMNS,
        filter.sql()
    ))?;
    let rows = st.query_map([], agent_row)?;
    rows.collect()
}

/// Open `path` and run `quick_check`. `Ok(None)` means the file is corrupt
/// or not a db. Other failures (permissions, I/O) are errors: moving a
/// healthy db aside would lose it.
fn open_checked(path: &Path) -> Result<Option<Connection>, String> {
    let conn =
        Connection::open(path).map_err(|e| format!("Failed to open db {:?}: {}", path, e))?;
    // Owner-only, like everything in the home. Covers a db created just now.
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .map_err(|e| format!("Failed to chmod 0600 {:?}: {}", path, e))?;
    match quick_check(&conn) {
        Ok(true) => Ok(Some(conn)),
        Ok(false) => Ok(None),
        Err(e) if is_corruption(&e) => Ok(None),
        Err(e) => Err(format!("quick_check on {:?} failed: {}", path, e)),
    }
}

fn quick_check(conn: &Connection) -> rusqlite::Result<bool> {
    let mut st = conn.prepare("PRAGMA quick_check")?;
    let rows: Vec<String> = st
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows.len() == 1 && rows[0] == "ok")
}

fn is_corruption(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(ErrorCode::DatabaseCorrupt) | Some(ErrorCode::NotADatabase)
    )
}

/// Rename the db and its WAL and shm files to `dest`, `dest-wal`,
/// `dest-shm`. The WAL holds committed pages, so it goes with the db.
fn move_aside(path: &Path, dest: &Path) -> Result<(), String> {
    for suffix in ["", "-wal", "-shm"] {
        let from = with_suffix(path, suffix);
        let to = with_suffix(dest, suffix);
        match std::fs::rename(&from, &to) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !suffix.is_empty() => {}
            Err(e) => return Err(format!("Failed to move {:?} to {:?}: {}", from, to, e)),
        }
    }
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn configure(conn: &Connection) -> Result<(), String> {
    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .map_err(|e| format!("Failed to enable WAL: {}", e))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(format!("Failed to enable WAL: journal_mode is {}", mode));
    }
    // FULL, not NORMAL: in WAL mode NORMAL skips the fsync on commit, so a
    // power loss could drop events that were already broadcast, and the
    // next boot would hand their seqs to new events in the same epoch.
    // Kept on purpose (D11): the engine writes little (state changes and
    // their events, not every read), so the fsync per commit costs little
    // and data safety comes first.
    conn.execute_batch("PRAGMA synchronous = FULL; PRAGMA foreign_keys = ON;")
        .map_err(|e| format!("Failed to set db pragmas: {}", e))?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|e| format!("Failed to set busy_timeout: {}", e))
}

/// Read the epoch, or mint one for a new db. Random, so a recreated db never
/// reuses the epoch of the one it replaced.
fn ensure_epoch(conn: &Connection) -> Result<String, String> {
    let fresh = uuid::Uuid::new_v4().simple().to_string()[..16].to_string();
    conn.execute(
        "INSERT OR IGNORE INTO meta (key, value) VALUES ('epoch', ?1)",
        [&fresh],
    )
    .map_err(|e| format!("Failed to write the epoch: {}", e))?;
    conn.query_row("SELECT value FROM meta WHERE key = 'epoch'", [], |r| {
        r.get(0)
    })
    .map_err(|e| format!("Failed to read the epoch: {}", e))
}

/// Make `machine_id` the only `is_self` row. A machine key that is new to
/// this db (first boot, or the key file was replaced) gets an audit row.
fn register_self(conn: &mut Connection, machine_id: &str) -> Result<(), String> {
    let fail = |e: rusqlite::Error| format!("Failed to register machine {}: {}", machine_id, e);
    let tx = conn.transaction().map_err(fail)?;
    tx.execute(
        "UPDATE machines SET is_self = 0 WHERE is_self = 1 AND id != ?1",
        [machine_id],
    )
    .map_err(fail)?;
    let added = tx
        .execute(
            "INSERT INTO machines (id, is_self) VALUES (?1, 1) \
             ON CONFLICT (id) DO NOTHING",
            [machine_id],
        )
        .map_err(fail)?;
    if added == 0 {
        tx.execute(
            "UPDATE machines SET is_self = 1 WHERE id = ?1",
            params![machine_id],
        )
        .map_err(fail)?;
    } else {
        audit::append(
            &tx,
            now_ms(),
            &AuditEntry {
                principal: audit::SYSTEM,
                method: "machine.register",
                target: Some(machine_id),
                decision: "allow",
                reason: Some("new machine key"),
                ..Default::default()
            },
        )
        .map_err(fail)?;
    }
    tx.commit().map_err(fail)
}

/// A new agent id, `agt_<ulid>`: 48 bits of unix ms, then 80 random bits,
/// in Crockford base32. Ids sort by the ms they were made in.
pub fn new_agent_id() -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let random: u128 = rand::random::<u128>() & ((1u128 << 80) - 1);
    let v = ((now_ms().max(0) as u128 & ((1u128 << 48) - 1)) << 80) | random;
    let mut id = String::with_capacity(30);
    id.push_str("agt_");
    // 26 digits of 5 bits cover 130 bits; the top two are always 0.
    for i in 0..26 {
        id.push(ALPHABET[((v >> (125 - 5 * i)) & 31) as usize] as char);
    }
    id
}

/// Unix time in ms.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// How long a `dedupe` row is kept. A sender never retries after its
/// own deadline (0.5 s for a hook), so an hour is plenty.
pub const DEDUPE_WINDOW_MS: i64 = 60 * 60 * 1000;

/// Delete `dedupe` rows older than `DEDUPE_WINDOW_MS`. Returns how many
/// went.
pub fn prune_dedupe(conn: &Connection, now: i64) -> rusqlite::Result<usize> {
    conn.execute("DELETE FROM dedupe WHERE ts < ?1", [now - DEDUPE_WINDOW_MS])
}

#[cfg(test)]
mod tests {
    use super::migrate::{user_version, DB_NEWER_THAN_ENGINE};
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// A machine id for tests; real ones come from `engine::machine`.
    const M: &str = "m0";

    fn home() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let h = t.path().join("engine");
        (t, h)
    }

    /// Every schema object, in a stable order, plus the version.
    fn schema_dump(conn: &Connection) -> String {
        let mut st = conn
            .prepare(
                "SELECT type, name, sql FROM sqlite_schema \
                 WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
            )
            .unwrap();
        let mut out = format!("-- user_version {}\n", user_version(conn).unwrap());
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .unwrap();
        for row in rows {
            let (ty, name, sql) = row.unwrap();
            out.push_str(&format!("-- {} {}\n{};\n", ty, name, sql));
        }
        out
    }

    const GOLDEN: &str = "src/engine/db/schema.golden.sql";

    /// The schema a fresh db gets. A change must be a new migration plus a
    /// reviewed golden update: `C9_UPDATE_GOLDEN=1 cargo test ... golden`.
    #[test]
    fn golden_schema_dump() {
        let (_t, h) = home();
        let db = open(&h, M).unwrap();
        let dump = schema_dump(&db.reader().unwrap());
        let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
        if std::env::var_os("C9_UPDATE_GOLDEN").is_some() {
            fs::write(&golden, &dump).unwrap();
        }
        let want = fs::read_to_string(&golden).unwrap();
        assert_eq!(dump, want, "schema drifted from {}", GOLDEN);
    }

    #[test]
    fn schema_has_the_c1b_m2_tables_and_no_token_hash() {
        let (_t, h) = home();
        let db = open(&h, M).unwrap();
        let r = db.reader().unwrap();
        let tables: Vec<String> = r
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            tables,
            [
                "agent_state",
                "agents",
                "annotation_rates",
                "annotation_sources",
                "annotations",
                "audit",
                "dedupe",
                "events",
                "gateway_clients",
                "legacy_events",
                "machines",
                "mail_pause",
                "message_status",
                "messages",
                "meta",
                "ops",
                "session_alias"
            ]
        );
        let cols: Vec<String> = r
            .prepare("SELECT name FROM pragma_table_info('agents')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            cols,
            [
                "id",
                "generation",
                "mode",
                "role",
                "control",
                "provider",
                "provider_sid",
                "source",
                "created_by",
                "gone_at",
                "resume_count",
                "interrupted_note_pending",
                "session_key",
                "machine_id",
                "state",
                "status",
                "cwd",
                "name",
                "pid",
                "started_at",
                "created_at",
                "last_seen",
                "parent_id",
                "state_reason",
                "resume_argv"
            ]
        );
    }

    #[test]
    fn open_creates_a_private_wal_db_and_keeps_its_epoch() {
        let (_t, h) = home();
        let db = open(&h, M).unwrap();
        let epoch = db.epoch().to_string();
        assert_eq!(epoch.len(), 16);
        assert!(db.moved_aside().is_none());
        let mode: String = db
            .reader()
            .unwrap()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let sync: i64 = db
            .writer()
            .call_blocking(|c| c.query_row("PRAGMA synchronous", [], |r| r.get(0)))
            .unwrap()
            .unwrap();
        assert_eq!(sync, 2, "the writer syncs every commit (FULL)");
        let perm = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(perm(&db_path(&h)), 0o600);
        assert_eq!(perm(&backups_dir(&h)), 0o700);
        drop(db);

        let db = open(&h, M).unwrap();
        assert_eq!(db.epoch(), epoch);
    }

    #[test]
    fn writes_go_through_the_writer_and_readers_see_them() {
        let (_t, h) = home();
        let db = open(&h, M).unwrap();
        db.writer()
            .call_blocking(|c| {
                c.execute(
                    "INSERT INTO agents (id, role, provider, source) \
                     VALUES ('agt_1', 'observed', 'claude', 'detected')",
                    [],
                )
            })
            .unwrap()
            .unwrap();
        let n: i64 = db
            .reader()
            .unwrap()
            .query_row("SELECT count(*) FROM agents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        // Readers are read-only.
        assert!(db
            .reader()
            .unwrap()
            .execute("DELETE FROM agents", [])
            .is_err());
        // Foreign keys are on for the writer.
        let fk = db
            .writer()
            .call_blocking(|c| {
                c.execute(
                    "INSERT INTO agent_state (agent_id, gen) VALUES ('agt_missing', 1)",
                    [],
                )
            })
            .unwrap();
        assert!(fk.is_err());
    }

    #[test]
    fn downgrade_is_refused_and_the_db_is_kept() {
        let (_t, h) = home();
        drop(open(&h, M).unwrap());
        {
            let conn = Connection::open(db_path(&h)).unwrap();
            conn.pragma_update(None, "user_version", latest_version() + 1)
                .unwrap();
        }
        let before = fs::read(db_path(&h)).unwrap();
        let err = open(&h, M).map(drop).unwrap_err();
        assert!(err.starts_with(DB_NEWER_THAN_ENGINE), "{}", err);
        // Not moved aside, not migrated, not written at all.
        assert_eq!(fs::read(db_path(&h)).unwrap(), before);
        let conn = Connection::open(db_path(&h)).unwrap();
        assert_eq!(user_version(&conn).unwrap(), latest_version() + 1);
        assert_eq!(fs::read_dir(backups_dir(&h)).unwrap().count(), 0);
    }

    /// A newer engine's schema that this SQLite cannot parse makes
    /// `quick_check` fail as corrupt; the version check must come first.
    #[test]
    fn a_newer_db_this_sqlite_cannot_parse_is_refused_not_moved_aside() {
        let (_t, h) = home();
        drop(open(&h, M).unwrap());
        {
            let conn = Connection::open(db_path(&h)).unwrap();
            conn.pragma_update(None, "user_version", latest_version() + 1)
                .unwrap();
            conn.execute_batch(
                "PRAGMA writable_schema = ON; \
                 INSERT INTO sqlite_schema (type, name, tbl_name, rootpage, sql) \
                 VALUES ('table', 'future', 'future', 0, 'CREATE TABLE future (x FUTURE(((');",
            )
            .unwrap();
        }
        let before = fs::read(db_path(&h)).unwrap();
        let err = open(&h, M).map(drop).unwrap_err();
        assert!(err.starts_with(DB_NEWER_THAN_ENGINE), "{}", err);
        assert_eq!(fs::read(db_path(&h)).unwrap(), before);
        assert_eq!(fs::read_dir(backups_dir(&h)).unwrap().count(), 0);
    }

    /// Damage the db so that `quick_check` reports errors while the header
    /// and schema page stay readable: fill every page after the first with
    /// garbage.
    fn corrupt_pages(path: &Path) {
        let mut bytes = fs::read(path).unwrap();
        let page = 4096;
        assert!(bytes.len() > 2 * page, "db too small to corrupt");
        for b in &mut bytes[page..] {
            *b = 0xA5;
        }
        fs::write(path, bytes).unwrap();
    }

    fn assert_new_epoch_after(h: &Path, old_epoch: &str) {
        let db = open(h, M).unwrap();
        assert_ne!(db.epoch(), old_epoch);
        let moved = db.moved_aside().expect("db moved aside").to_path_buf();
        assert!(moved.starts_with(backups_dir(h)), "{:?}", moved);
        assert!(moved.exists());
        // The new db is complete and empty.
        let r = db.reader().unwrap();
        assert_eq!(user_version(&r).unwrap(), latest_version());
        let n: i64 = r
            .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn quick_check_failure_gives_a_new_epoch() {
        let (_t, h) = home();
        let db = open(&h, M).unwrap();
        let old = db.epoch().to_string();
        let ep = old.clone();
        db.writer()
            .call_blocking(move |c| {
                let tx = c.transaction().unwrap();
                for i in 0..2000 {
                    tx.execute(
                        "INSERT INTO events (epoch, topic, payload, ts) VALUES (?1, 't', ?2, ?3)",
                        rusqlite::params![ep, "x".repeat(100), i],
                    )
                    .unwrap();
                }
                tx.commit().unwrap();
            })
            .unwrap();
        // Dropping the db closes the writer, which checkpoints the WAL.
        drop(db);
        corrupt_pages(&db_path(&h));
        assert_new_epoch_after(&h, &old);
    }

    #[test]
    fn not_a_db_gives_a_new_epoch() {
        let (_t, h) = home();
        let old = open(&h, M).unwrap().epoch().to_string();
        fs::write(db_path(&h), vec![0x5Au8; 8192]).unwrap();
        assert_new_epoch_after(&h, &old);
    }

    fn self_rows(db: &Db) -> Vec<(String, i64)> {
        db.reader()
            .unwrap()
            .prepare("SELECT id, is_self FROM machines ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn audit_rows(db: &Db) -> Vec<(String, String, String)> {
        db.reader()
            .unwrap()
            .prepare("SELECT principal, method, target FROM audit ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn the_machine_key_id_is_the_self_row_and_new_keys_are_audited() {
        let (_t, h) = home();
        let db = open(&h, M).unwrap();
        assert_eq!(db.machine_id(), M);
        assert_eq!(self_rows(&db), [(M.to_string(), 1)]);
        let reg = |id: &str| {
            (
                "system".to_string(),
                "machine.register".to_string(),
                id.to_string(),
            )
        };
        assert_eq!(audit_rows(&db), [reg(M)]);
        drop(db);

        // Same key again: nothing new.
        let db = open(&h, M).unwrap();
        assert_eq!(audit_rows(&db), [reg(M)]);
        drop(db);

        // A replaced key file: a new self row, the old one stays as a peer.
        let db = open(&h, "m1").unwrap();
        assert_eq!(self_rows(&db), [(M.to_string(), 0), ("m1".to_string(), 1)]);
        assert_eq!(audit_rows(&db), [reg(M), reg("m1")]);
        drop(db);

        let db = open(&h, M).unwrap();
        assert_eq!(self_rows(&db), [(M.to_string(), 1), ("m1".to_string(), 0)]);
        assert_eq!(audit_rows(&db).len(), 2);
    }

    fn agent_event(id: &str) -> NewEvent {
        NewEvent {
            topic: "agent.upsert".into(),
            subject: Some(format!("{}/{}", M, id)),
            origin: None,
            data: serde_json::json!({ "id": id }),
        }
    }

    fn insert_agent(tx: &Tx, id: &str) -> rusqlite::Result<usize> {
        tx.execute(
            "INSERT INTO agents (id, role, provider, source) \
             VALUES (?1, 'observed', 'claude', 'detected')",
            [id],
        )
    }

    /// `Tx::after_commit`: hooks run after a commit, in order, and a
    /// panicking one does not hide the commit; a failed closure or a failed
    /// commit drops them unrun.
    #[test]
    fn after_commit_hooks_run_only_after_a_commit() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (_t, h) = home();
        let db = open(&h, M).unwrap();
        let ran = Arc::new(AtomicUsize::new(0));
        fn hook(tx: &mut Tx, ran: &Arc<AtomicUsize>) {
            let ran = ran.clone();
            tx.after_commit(move || {
                ran.fetch_add(1, Ordering::SeqCst);
            });
        }
        // A statement fails: rolled back, the hook never runs.
        let r = ran.clone();
        let err = db.write_blocking(move |tx| {
            hook(tx, &r);
            tx.execute("INSERT INTO no_such_table VALUES (1)", [])
        });
        assert!(err.is_err());
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        // The commit fails (a deferred foreign key): the hook never runs.
        let r = ran.clone();
        let err = db.write_blocking(move |tx| {
            hook(tx, &r);
            tx.execute_batch(
                "PRAGMA defer_foreign_keys = ON; \
                 INSERT INTO agent_state (agent_id, gen) VALUES ('nobody', 1);",
            )
        });
        assert!(err.unwrap_err().contains("FOREIGN KEY"));
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        let rows: i64 = db
            .reader()
            .unwrap()
            .query_row("SELECT count(*) FROM agent_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
        // A commit: the hooks run; one that panics is skipped, and the
        // caller still gets its result.
        let mut head = db.head.subscribe();
        let r = ran.clone();
        let (out, cur) = db
            .write_blocking(move |tx| {
                hook(tx, &r);
                tx.after_commit(|| panic!("hook"));
                hook(tx, &r);
                insert_agent(tx, "a1")?;
                tx.emit(&agent_event("a1"))?;
                Ok(7)
            })
            .unwrap();
        assert_eq!(out, 7);
        assert_eq!(cur, Some(db.cursor_at(1)));
        assert_eq!(ran.load(Ordering::SeqCst), 2);
        // Subscribers still wake on the commit.
        assert!(head.has_changed().unwrap());
        assert_eq!(*head.borrow_and_update(), 1);
        // The writer still works.
        db.write_blocking(|tx| tx.execute("DELETE FROM agents WHERE id = 'zz'", []))
            .unwrap();
    }

    #[test]
    fn an_event_commits_or_rolls_back_with_its_state_change() {
        let (_t, h) = home();
        let db = open(&h, M).unwrap();
        let mut head = db.head.subscribe();
        assert_eq!(
            db.head_cursor().to_string(),
            format!("{}:{}:0", M, db.epoch())
        );

        let (_, cur) = db
            .write_blocking(|tx| {
                insert_agent(tx, "a1")?;
                tx.emit(&agent_event("a1"))
            })
            .unwrap();
        assert_eq!(cur, Some(db.cursor_at(1)));
        assert!(head.has_changed().unwrap());
        assert_eq!(*head.borrow_and_update(), 1);

        // The state change fails after the emit: neither row stays, and no
        // head is broadcast.
        let err = db
            .write_blocking(|tx| {
                tx.emit(&agent_event("a1"))?;
                insert_agent(tx, "a1")
            })
            .unwrap_err();
        assert!(err.contains("UNIQUE"), "{}", err);
        assert!(!head.has_changed().unwrap());
        let r = db.reader().unwrap();
        let count = |sql: &str| -> i64 { r.query_row(sql, [], |row| row.get(0)).unwrap() };
        assert_eq!(count("SELECT count(*) FROM agents"), 1);
        assert_eq!(count("SELECT count(*) FROM events"), 1);

        // A write without events moves no cursor.
        let (_, cur) = db.write_blocking(|tx| insert_agent(tx, "a2")).unwrap();
        assert_eq!(cur, None);
        assert!(!head.has_changed().unwrap());

        // The next event is seq 2: the rolled-back one took no seq.
        let (_, cur) = db.write_blocking(|tx| tx.emit(&agent_event("a2"))).unwrap();
        assert_eq!(cur.unwrap().seq, 2);
    }

    #[test]
    fn the_snapshot_cursor_matches_its_state_and_survives_a_reopen() {
        let (_t, h) = home();
        let db = open(&h, M).unwrap();
        db.write_blocking(|tx| {
            insert_agent(tx, "a1")?;
            tx.emit(&agent_event("a1"))
        })
        .unwrap();
        let (cur, agents) = db.snapshot().unwrap();
        assert_eq!(cur, db.cursor_at(1));
        assert_eq!(agents.len(), 1);
        assert_eq!(
            agents[0],
            AgentRow {
                id: "a1".into(),
                generation: 0,
                mode: None,
                role: "observed".into(),
                control: "none".into(),
                provider: "claude".into(),
                provider_sid: None,
                source: "detected".into(),
                created_by: None,
                gone_at: None,
                resume_count: 0,
                interrupted_note_pending: false,
                session_key: None,
                machine_id: None,
                state: None,
                status: None,
                cwd: None,
                name: None,
                pid: None,
                started_at: None,
                created_at: None,
                last_seen: None,
                parent_id: None,
                state_reason: None,
            }
        );
        assert_eq!(db.check_cursor(&cur), Ok(1));
        db.close();
        assert!(db.write_blocking(|tx| insert_agent(tx, "a2")).is_err());
        drop(db);

        let db = open(&h, M).unwrap();
        assert_eq!(db.head_cursor(), cur);
        assert_eq!(db.check_cursor(&cur), Ok(1));
    }

    /// The address of the `sqlite3` handle behind a connection, to tell
    /// whether the pool handed back the same one.
    fn handle(c: &Connection) -> usize {
        unsafe { c.handle() as usize }
    }

    #[test]
    fn pooled_readers_are_reused_and_see_new_writes() {
        let (_t, h) = home();
        let db = open(&h, M).unwrap();
        let first = handle(&db.pooled_reader().unwrap());
        let again = db.pooled_reader().unwrap();
        assert_eq!(handle(&again), first, "an idle reader is reused");
        // A second reader while one is out is a new connection.
        let other = db.pooled_reader().unwrap();
        assert_ne!(handle(&other), first);
        drop(other);
        // A reused reader sees writes committed after it was opened.
        db.write_blocking(|tx| insert_agent(tx, "a1")).unwrap();
        let n: i64 = again
            .query_row("SELECT count(*) FROM agents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        // `&PooledReader` coerces to `&Connection`.
        assert_eq!(agent_rows(&again, AgentFilter::All).unwrap().len(), 1);
    }

    #[test]
    fn a_reader_left_in_a_transaction_is_not_reused() {
        let (_t, h) = home();
        let db = open(&h, M).unwrap();
        let r = db.pooled_reader().unwrap();
        r.execute_batch("BEGIN").unwrap();
        drop(r);
        assert!(db.readers.lock().unwrap().is_empty(), "closed, not pooled");
        let ok = db.pooled_reader().unwrap();
        drop(ok);
        assert_eq!(db.readers.lock().unwrap().len(), 1);
        assert!(db.pooled_reader().unwrap().is_autocommit());
    }

    #[test]
    fn the_pool_keeps_at_most_its_idle_limit_and_survives_close() {
        let (_t, h) = home();
        let db = open(&h, M).unwrap();
        let many: Vec<_> = (0..READER_POOL_IDLE + 3)
            .map(|_| db.pooled_reader().unwrap())
            .collect();
        drop(many);
        assert_eq!(db.readers.lock().unwrap().len(), READER_POOL_IDLE);
        db.close();
        let r = db.pooled_reader().unwrap();
        let n: i64 = r
            .query_row("SELECT count(*) FROM machines", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn audit_retention_runs_at_boot_and_on_demand() {
        let (_t, h) = home();
        let db = open(&h, M).unwrap();
        db.write_blocking(|tx| {
            let old = AuditEntry {
                principal: audit::SYSTEM,
                method: "old",
                target: Some("t"),
                decision: "allow",
                ..Default::default()
            };
            audit::append(tx, 1, &old)?;
            audit::append(tx, 2, &old)?;
            let fresh = AuditEntry {
                method: "fresh",
                ..old
            };
            audit::append(tx, now_ms(), &fresh)
        })
        .unwrap();
        assert_eq!(audit_rows(&db).len(), 4);
        drop(db);

        // The default keeps a year: the two rows from 1970 go at boot.
        let db = open(&h, M).unwrap();
        let methods: Vec<String> = audit_rows(&db).into_iter().map(|r| r.1).collect();
        assert_eq!(methods, ["machine.register", "fresh"]);
        drop(db);

        // A row cap, applied by `prune_audit` on the writer.
        let opts = OpenOptions {
            audit_retention: AuditRetention {
                max_rows: 2,
                ..Default::default()
            },
        };
        let db = open_with(&h, M, opts).unwrap();
        db.write_blocking(|tx| {
            for m in ["a", "b", "c"] {
                let e = AuditEntry {
                    principal: audit::SYSTEM,
                    method: m,
                    target: Some("t"),
                    decision: "allow",
                    ..Default::default()
                };
                audit::append(tx, now_ms(), &e)?;
            }
            Ok(())
        })
        .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(rt.block_on(db.prune_audit()).unwrap(), 3);
        let methods: Vec<String> = audit_rows(&db).into_iter().map(|r| r.1).collect();
        assert_eq!(methods, ["b", "c"]);
    }
}
