//! Schema migrations, versioned by `PRAGMA user_version`.
//!
//! Migration N moves the db from version N-1 to N in one transaction, so a
//! crash leaves the db at N-1 or N, never in between. Before each one runs
//! on a db that already has a schema, the db is copied with `VACUUM INTO
//! <backups>/pre-<N>.db` (A:497). A db newer than this engine is refused
//! with `db_newer_than_engine` and left untouched; `db::open` checks the
//! file header first, before anything opens the db.

use rusqlite::Connection;
use std::path::Path;

/// Error code: the db was written by a newer engine.
pub const DB_NEWER_THAN_ENGINE: &str = "db_newer_than_engine";

/// One schema step. `version` is the `user_version` after it runs.
pub struct Migration {
    pub version: u32,
    pub sql: &'static str,
}

/// Every migration, in order. Versions start at 1 and have no gaps.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("migrations/0002_detection.sql"),
    },
];

/// The schema version this engine writes.
pub fn latest_version() -> u32 {
    MIGRATIONS.last().map_or(0, |m| m.version)
}

pub fn user_version(conn: &Connection) -> Result<u32, String> {
    conn.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
        .map_err(|e| format!("Failed to read user_version: {}", e))
}

/// Fail with `db_newer_than_engine` when the db is ahead of `migrations`.
pub fn check_not_newer(conn: &Connection, migrations: &[Migration]) -> Result<u32, String> {
    let have = user_version(conn)?;
    let known = migrations.last().map_or(0, |m| m.version);
    if have > known {
        return Err(newer_than_engine(have, known));
    }
    Ok(have)
}

/// The `db_newer_than_engine` error for a db at `have`.
pub fn newer_than_engine(have: u32, known: u32) -> String {
    format!(
        "{}: db is at schema version {}, this engine knows up to {}",
        DB_NEWER_THAN_ENGINE, have, known
    )
}

/// `user_version` read from the file header (bytes 60..64, big-endian),
/// without opening the db. `None` when the file is missing, short or not
/// a SQLite db. A version committed only to the WAL is not seen here.
pub fn header_user_version(path: &Path) -> Result<Option<u32>, String> {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Failed to open db {:?}: {}", path, e)),
    };
    let mut head = Vec::with_capacity(64);
    f.by_ref()
        .take(64)
        .read_to_end(&mut head)
        .map_err(|e| format!("Failed to read db header {:?}: {}", path, e))?;
    if head.len() < 64 || !head.starts_with(b"SQLite format 3\0") {
        return Ok(None);
    }
    Ok(Some(u32::from_be_bytes([
        head[60], head[61], head[62], head[63],
    ])))
}

/// Bring the db up to the last of `migrations`. Returns the versions that
/// ran. `backups` must exist and be private.
pub fn migrate(
    conn: &mut Connection,
    migrations: &[Migration],
    backups: &Path,
) -> Result<Vec<u32>, String> {
    let mut ran = Vec::new();
    let mut have = check_not_newer(conn, migrations)?;
    let start = have;
    for m in migrations.iter().filter(|m| m.version > start) {
        if m.version != have + 1 {
            return Err(format!(
                "Migration gap: db at {}, next migration is {}",
                have, m.version
            ));
        }
        // A db that started at version 0 was an empty file: there is
        // nothing to keep.
        if start > 0 {
            backup(conn, &backups.join(format!("pre-{}.db", m.version)))?;
        }
        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to begin migration {}: {}", m.version, e))?;
        tx.execute_batch(m.sql)
            .map_err(|e| format!("Migration {} failed: {}", m.version, e))?;
        // user_version lives in the db header, which this transaction owns.
        tx.pragma_update(None, "user_version", m.version)
            .map_err(|e| format!("Failed to set user_version {}: {}", m.version, e))?;
        tx.commit()
            .map_err(|e| format!("Failed to commit migration {}: {}", m.version, e))?;
        have = m.version;
        ran.push(m.version);
    }
    Ok(ran)
}

/// `VACUUM INTO dest`. A leftover `dest` from an earlier attempt that
/// never committed is replaced, since VACUUM INTO refuses an existing file.
fn backup(conn: &Connection, dest: &Path) -> Result<(), String> {
    match std::fs::remove_file(dest) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Failed to remove old backup {:?}: {}", dest, e)),
    }
    let dest_str = dest
        .to_str()
        .ok_or_else(|| format!("Backup path {:?} is not UTF-8", dest))?;
    conn.execute("VACUUM INTO ?1", [dest_str])
        .map_err(|e| format!("Failed to back up the db to {:?}: {}", dest, e))?;
    // VACUUM INTO creates the file with the umask's mode; a backup holds
    // the same data as the db, so it is owner-only too.
    std::fs::set_permissions(dest, std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .map_err(|e| format!("Failed to chmod 0600 {:?}: {}", dest, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A migration one past the newest, as a newer engine would ship it.
    const NEXT_SQL: &str = "CREATE TABLE extra (x INTEGER) STRICT;";

    /// `MIGRATIONS` plus one more step with `sql`.
    fn plus_next(sql: &'static str) -> Vec<Migration> {
        MIGRATIONS
            .iter()
            .map(|m| Migration {
                version: m.version,
                sql: m.sql,
            })
            .chain(std::iter::once(Migration {
                version: latest_version() + 1,
                sql,
            }))
            .collect()
    }

    fn next() -> u32 {
        latest_version() + 1
    }

    #[test]
    fn migrations_are_numbered_from_one_without_gaps() {
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(m.version as usize, i + 1);
        }
    }

    #[test]
    fn fresh_db_migrates_without_backup() {
        let t = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(t.path().join("e.db")).unwrap();
        let all: Vec<u32> = (1..=latest_version()).collect();
        assert_eq!(migrate(&mut conn, MIGRATIONS, t.path()).unwrap(), all);
        assert_eq!(user_version(&conn).unwrap(), latest_version());
        // Not even between the steps of one run.
        assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 1);
        // Idempotent.
        assert!(migrate(&mut conn, MIGRATIONS, t.path()).unwrap().is_empty());
    }

    #[test]
    fn a_version_1_db_moves_to_2_and_keeps_its_rows() {
        let t = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(t.path().join("e.db")).unwrap();
        migrate(&mut conn, &MIGRATIONS[..1], t.path()).unwrap();
        conn.execute(
            "INSERT INTO agents (id, role, provider, source) \
             VALUES ('agt_1', 'observed', 'claudeCode', 'detected')",
            [],
        )
        .unwrap();
        assert_eq!(migrate(&mut conn, MIGRATIONS, t.path()).unwrap(), vec![2]);
        assert!(t.path().join("pre-2.db").exists());
        let (id, key): (String, Option<String>) = conn
            .query_row("SELECT id, session_key FROM agents", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((id.as_str(), key), ("agt_1", None));
    }

    #[test]
    fn each_later_migration_backs_up_the_previous_version() {
        let t = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(t.path().join("e.db")).unwrap();
        migrate(&mut conn, MIGRATIONS, t.path()).unwrap();
        conn.execute(
            "INSERT INTO machines (id, label, is_self) VALUES ('k', 'me', 1)",
            [],
        )
        .unwrap();

        let pre = t.path().join(format!("pre-{}.db", next()));
        assert_eq!(
            migrate(&mut conn, &plus_next(NEXT_SQL), t.path()).unwrap(),
            vec![next()]
        );
        assert_eq!(user_version(&conn).unwrap(), next());

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&pre).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let backup = Connection::open(&pre).unwrap();
        assert_eq!(user_version(&backup).unwrap(), latest_version());
        let id: String = backup
            .query_row("SELECT id FROM machines", [], |r| r.get(0))
            .unwrap();
        assert_eq!(id, "k");
        let extra: i64 = backup
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'extra'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(extra, 0);
    }

    #[test]
    fn stale_backup_file_is_replaced() {
        let t = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(t.path().join("e.db")).unwrap();
        migrate(&mut conn, MIGRATIONS, t.path()).unwrap();
        let pre = t.path().join(format!("pre-{}.db", next()));
        std::fs::write(&pre, b"stale").unwrap();
        migrate(&mut conn, &plus_next(NEXT_SQL), t.path()).unwrap();
        let backup = Connection::open(&pre).unwrap();
        assert_eq!(user_version(&backup).unwrap(), latest_version());
    }

    #[test]
    fn failed_migration_rolls_back() {
        let t = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(t.path().join("e.db")).unwrap();
        migrate(&mut conn, MIGRATIONS, t.path()).unwrap();
        let bad = plus_next("CREATE TABLE half (x INTEGER); NOT SQL;");
        assert!(migrate(&mut conn, &bad, t.path()).is_err());
        assert_eq!(user_version(&conn).unwrap(), latest_version());
        let half: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'half'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(half, 0);
    }

    #[test]
    fn newer_db_is_refused_and_untouched() {
        let t = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(t.path().join("e.db")).unwrap();
        migrate(&mut conn, &plus_next(NEXT_SQL), t.path()).unwrap();
        let err = migrate(&mut conn, MIGRATIONS, t.path()).unwrap_err();
        assert!(err.starts_with(DB_NEWER_THAN_ENGINE), "{}", err);
        assert_eq!(user_version(&conn).unwrap(), next());
    }
}
