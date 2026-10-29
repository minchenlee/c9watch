//! The engine store: `<home>/engine.db`, SQLite in WAL mode (A:340).
//!
//! Boot order in `open`:
//! 1. `PRAGMA quick_check`. A db that fails it (or is not a db at all) is
//!    moved to `<home>/backups/corrupt-<ms>.db` and a new one is created,
//!    which starts a new epoch (A:497). Observed agents are rebuilt from
//!    provider data by detection (M3).
//! 2. A db newer than this engine is refused with `db_newer_than_engine`
//!    and left as it is.
//! 3. Migrations, each after a `VACUUM INTO` backup (see `migrate`).
//! 4. The writable connection moves to the writer thread (see `writer`).
//!
//! Call `open` only while holding the engine lock: it may move the db.

pub mod migrate;
pub mod writer;

use super::fsutil::ensure_private_dir;
use migrate::{migrate, MIGRATIONS};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use std::path::{Path, PathBuf};
use std::time::Duration;
use writer::Writer;

/// The db file inside the engine home.
pub const DB_NAME: &str = "engine.db";
/// Migration backups and moved-aside dbs.
pub const BACKUPS_DIR: &str = "backups";

/// How long a reader waits for a lock (only the WAL checkpoint takes one).
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

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
    epoch: String,
    moved_aside: Option<PathBuf>,
    writer: Writer,
}

impl Db {
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

    /// A new read-only connection. WAL lets readers run beside the writer.
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

/// Open (or create) the store in `home`.
pub fn open(home: &Path) -> Result<Db, String> {
    ensure_private_dir(home)?;
    let backups = backups_dir(home);
    ensure_private_dir(&backups)?;
    let path = db_path(home);

    let mut moved_aside = None;
    let mut conn = match open_checked(&path)? {
        Some(conn) => conn,
        None => {
            let dest = backups.join(format!("corrupt-{}.db", now_ms()));
            move_aside(&path, &dest)?;
            eprintln!(
                "[engine] {:?} failed quick_check; moved to {:?}, starting a new epoch",
                path, dest
            );
            moved_aside = Some(dest);
            open_checked(&path)?.ok_or_else(|| format!("New db {:?} failed quick_check", path))?
        }
    };

    configure(&conn)?;
    migrate(&mut conn, MIGRATIONS, &backups)?;
    let epoch = ensure_epoch(&conn)?;
    Ok(Db {
        path,
        epoch,
        moved_aside,
        writer: Writer::spawn(conn)?,
    })
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
    conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")
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

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

#[cfg(test)]
mod tests {
    use super::migrate::{latest_version, user_version, DB_NEWER_THAN_ENGINE};
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

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
        let db = open(&h).unwrap();
        let dump = schema_dump(&db.reader().unwrap());
        let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
        if std::env::var_os("C9_UPDATE_GOLDEN").is_some() {
            fs::write(&golden, &dump).unwrap();
        }
        let want = fs::read_to_string(&golden).unwrap();
        assert_eq!(dump, want, "schema drifted from {}", GOLDEN);
    }

    #[test]
    fn schema_has_the_c1a_tables_and_no_token_hash() {
        let (_t, h) = home();
        let db = open(&h).unwrap();
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
                "audit",
                "dedupe",
                "events",
                "machines",
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
                "interrupted_note_pending"
            ]
        );
    }

    #[test]
    fn open_creates_a_private_wal_db_and_keeps_its_epoch() {
        let (_t, h) = home();
        let db = open(&h).unwrap();
        let epoch = db.epoch().to_string();
        assert_eq!(epoch.len(), 16);
        assert!(db.moved_aside().is_none());
        let mode: String = db
            .reader()
            .unwrap()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let perm = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(perm(&db_path(&h)), 0o600);
        assert_eq!(perm(&backups_dir(&h)), 0o700);
        drop(db);

        let db = open(&h).unwrap();
        assert_eq!(db.epoch(), epoch);
    }

    #[test]
    fn writes_go_through_the_writer_and_readers_see_them() {
        let (_t, h) = home();
        let db = open(&h).unwrap();
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
        drop(open(&h).unwrap());
        {
            let conn = Connection::open(db_path(&h)).unwrap();
            conn.pragma_update(None, "user_version", latest_version() + 1)
                .unwrap();
        }
        let err = open(&h).map(drop).unwrap_err();
        assert!(err.starts_with(DB_NEWER_THAN_ENGINE), "{}", err);
        // Not moved aside, not migrated.
        let conn = Connection::open(db_path(&h)).unwrap();
        assert_eq!(user_version(&conn).unwrap(), latest_version() + 1);
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
        let db = open(h).unwrap();
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
        let db = open(&h).unwrap();
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
        let old = open(&h).unwrap().epoch().to_string();
        fs::write(db_path(&h), vec![0x5Au8; 8192]).unwrap();
        assert_new_epoch_after(&h, &old);
    }
}
