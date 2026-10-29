//! Schema migrations, versioned by `PRAGMA user_version`.
//!
//! Migration N moves the db from version N-1 to N in one transaction, so a
//! crash leaves the db at N-1 or N, never in between. Before each one runs
//! on a db that already has a schema, the db is copied with `VACUUM INTO
//! <backups>/pre-<N>.db` (A:497). A db newer than this engine is refused
//! with `db_newer_than_engine` and left untouched.

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
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: include_str!("migrations/0001_init.sql"),
}];

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
        return Err(format!(
            "{}: db is at schema version {}, this engine knows up to {}",
            DB_NEWER_THAN_ENGINE, have, known
        ));
    }
    Ok(have)
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
        // Version 0 is an empty file: there is nothing to keep.
        if have > 0 {
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const V2: Migration = Migration {
        version: 2,
        sql: "CREATE TABLE extra (x INTEGER) STRICT;",
    };

    fn two_step() -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                sql: MIGRATIONS[0].sql,
            },
            V2,
        ]
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
        assert_eq!(migrate(&mut conn, MIGRATIONS, t.path()).unwrap(), vec![1]);
        assert_eq!(user_version(&conn).unwrap(), latest_version());
        assert!(!t.path().join("pre-1.db").exists());
        // Idempotent.
        assert!(migrate(&mut conn, MIGRATIONS, t.path()).unwrap().is_empty());
    }

    #[test]
    fn each_later_migration_backs_up_the_previous_version() {
        let t = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(t.path().join("e.db")).unwrap();
        migrate(&mut conn, &two_step()[..1], t.path()).unwrap();
        conn.execute(
            "INSERT INTO machines (id, label, is_self) VALUES ('k', 'me', 1)",
            [],
        )
        .unwrap();

        assert_eq!(migrate(&mut conn, &two_step(), t.path()).unwrap(), vec![2]);
        assert_eq!(user_version(&conn).unwrap(), 2);

        let backup = Connection::open(t.path().join("pre-2.db")).unwrap();
        assert_eq!(user_version(&backup).unwrap(), 1);
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
        migrate(&mut conn, &two_step()[..1], t.path()).unwrap();
        std::fs::write(t.path().join("pre-2.db"), b"stale").unwrap();
        migrate(&mut conn, &two_step(), t.path()).unwrap();
        let backup = Connection::open(t.path().join("pre-2.db")).unwrap();
        assert_eq!(user_version(&backup).unwrap(), 1);
    }

    #[test]
    fn failed_migration_rolls_back() {
        let t = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(t.path().join("e.db")).unwrap();
        let bad = [
            Migration {
                version: 1,
                sql: MIGRATIONS[0].sql,
            },
            Migration {
                version: 2,
                sql: "CREATE TABLE half (x INTEGER); NOT SQL;",
            },
        ];
        migrate(&mut conn, &bad[..1], t.path()).unwrap();
        assert!(migrate(&mut conn, &bad, t.path()).is_err());
        assert_eq!(user_version(&conn).unwrap(), 1);
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
        migrate(&mut conn, &two_step(), t.path()).unwrap();
        let err = migrate(&mut conn, MIGRATIONS, t.path()).unwrap_err();
        assert!(err.starts_with(DB_NEWER_THAN_ENGINE), "{}", err);
        assert_eq!(user_version(&conn).unwrap(), 2);
    }
}
