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
    pub fk_off: bool,
}

/// Every migration, in order. Versions start at 1 and have no gaps.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("migrations/0001_init.sql"),
        fk_off: false,
    },
    Migration {
        version: 2,
        sql: include_str!("migrations/0002_detection.sql"),
        fk_off: false,
    },
    Migration {
        version: 3,
        sql: include_str!("migrations/0003_legacy_import.sql"),
        fk_off: false,
    },
    Migration {
        version: 4,
        sql: include_str!("migrations/0004_drain.sql"),
        fk_off: false,
    },
    Migration {
        version: 5,
        sql: include_str!("migrations/0005_messages.sql"),
        fk_off: false,
    },
    Migration {
        version: 6,
        sql: include_str!("migrations/0006_mail_socket.sql"),
        fk_off: true,
    },
    Migration {
        version: 7,
        sql: include_str!("migrations/0007_gateway_tokens.sql"),
        fk_off: false,
    },
    Migration {
        version: 8,
        sql: include_str!("migrations/0008_annotations.sql"),
        fk_off: false,
    },
    Migration {
        version: 9,
        sql: include_str!("migrations/0009_kill_survivors.sql"),
        fk_off: false,
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
        if m.fk_off {
            conn.pragma_update(None, "foreign_keys", false)
                .map_err(|e| e.to_string())?;
        }
        let applied = (|| -> Result<(), String> {
            let tx = conn.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(m.sql)
                .map_err(|e| format!("Migration {} failed: {}", m.version, e))?;
            if m.fk_off {
                let bad = tx
                    .prepare("PRAGMA foreign_key_check")
                    .map_err(|e| e.to_string())?
                    .exists([])
                    .map_err(|e| e.to_string())?;
                if bad {
                    return Err(format!("Migration {} failed: foreign_key_check", m.version));
                }
            }
            tx.pragma_update(None, "user_version", m.version)
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())
        })();
        // Always restore, including SQL, FK-check and commit failures.
        let restored = if m.fk_off {
            conn.pragma_update(None, "foreign_keys", true)
                .map_err(|e| e.to_string())
        } else {
            Ok(())
        };
        restored?;
        applied?;
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
                fk_off: m.fk_off,
            })
            .chain(std::iter::once(Migration {
                version: latest_version() + 1,
                sql,
                fk_off: false,
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
    fn a_version_1_db_moves_to_the_latest_and_keeps_its_rows() {
        let t = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(t.path().join("e.db")).unwrap();
        migrate(&mut conn, &MIGRATIONS[..1], t.path()).unwrap();
        conn.execute(
            "INSERT INTO agents (id, role, provider, source) \
             VALUES ('agt_1', 'observed', 'claudeCode', 'detected')",
            [],
        )
        .unwrap();
        let later: Vec<u32> = (2..=latest_version()).collect();
        assert_eq!(migrate(&mut conn, MIGRATIONS, t.path()).unwrap(), later);
        assert!(t.path().join("pre-2.db").exists());
        assert!(t.path().join("pre-3.db").exists());
        let (id, key): (String, Option<String>) = conn
            .query_row("SELECT id, session_key FROM agents", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((id.as_str(), key), ("agt_1", None));
    }

    #[test]
    fn a_version_4_db_gains_the_mail_tables_with_a_backup() {
        let t = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(t.path().join("e.db")).unwrap();
        migrate(&mut conn, &MIGRATIONS[..4], t.path()).unwrap();
        conn.execute(
            "INSERT INTO agents (id, role, provider, source) \
             VALUES ('agt_1', 'worker', 'claude', 'engine')",
            [],
        )
        .unwrap();
        assert_eq!(
            migrate(&mut conn, MIGRATIONS, t.path()).unwrap(),
            [5, 6, 7, 8, 9]
        );
        assert_eq!(user_version(&conn).unwrap(), latest_version());
        let backup = Connection::open(t.path().join("pre-5.db")).unwrap();
        assert_eq!(user_version(&backup).unwrap(), 4);
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema \
                 WHERE name IN ('messages', 'message_status')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
        let id: String = conn
            .query_row("SELECT id FROM agents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(id, "agt_1");
    }

    #[test]
    fn idem_is_unique_per_sender_and_null_never_collides() {
        let t = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(t.path().join("e.db")).unwrap();
        migrate(&mut conn, MIGRATIONS, t.path()).unwrap();
        conn.execute(
            "INSERT INTO agents (id, role, provider, source) \
             VALUES ('agt_1', 'worker', 'claude', 'engine')",
            [],
        )
        .unwrap();
        let put = |id: &str, from: &str, idem: Option<&str>| {
            conn.execute(
                "INSERT INTO messages (id, from_principal, to_agent, to_addr, \
                 origin_kind, provenance, body, body_hash, req_hash, idem, state, \
                 state_at, created_at, wire_id) \
                 VALUES (?1, ?2, 'agt_1', 'm/agt_1', 'local-unverified', 'p', 'b', \
                 'h', 'r', ?3, 'queued', 0, 0, ?4)",
                rusqlite::params![id, from, idem, uuid::Uuid::new_v4().to_string()],
            )
        };
        put("msg_1", "local-unverified", None).unwrap();
        put("msg_2", "local-unverified", None).unwrap();
        put("msg_3", "local-unverified", Some("k")).unwrap();
        // Another sender may use the same key.
        put("msg_4", "agent:agt_9", Some("k")).unwrap();
        assert!(put("msg_5", "local-unverified", Some("k")).is_err());
    }

    /// D2 (P1-3): a version 8 db gains `agent_state.survivors`, NULL on
    /// its rows, after a `VACUUM INTO` backup of version 8.
    #[test]
    fn a_version_8_db_gains_kill_survivors_with_a_backup() {
        let t = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(t.path().join("e.db")).unwrap();
        migrate(&mut conn, &MIGRATIONS[..8], t.path()).unwrap();
        conn.execute_batch(
            "INSERT INTO agents (id, role, provider, source) \
             VALUES ('agt_1', 'worker', 'claude', 'engine'); \
             INSERT INTO agent_state (agent_id, gen, pty_child_pid, pty_child_start) \
             VALUES ('agt_1', 1, 4242, 7);",
        )
        .unwrap();
        assert_eq!(migrate(&mut conn, MIGRATIONS, t.path()).unwrap(), [9]);
        assert_eq!(user_version(&conn).unwrap(), latest_version());
        let backup = Connection::open(t.path().join("pre-9.db")).unwrap();
        assert_eq!(user_version(&backup).unwrap(), 8);
        let row: (i64, Option<String>) = conn
            .query_row(
                "SELECT pty_child_pid, survivors FROM agent_state WHERE agent_id = 'agt_1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (4242, None));
        conn.execute(
            "UPDATE agent_state SET survivors = '[]' WHERE agent_id = 'agt_1'",
            [],
        )
        .unwrap();
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
    #[test]
    fn v5_rows_statuses_uuid_and_foreign_keys_survive_the_rebuild() {
        let t = tempfile::tempdir().unwrap();
        let mut c = Connection::open(t.path().join("db")).unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        migrate(&mut c, &MIGRATIONS[..5], t.path()).unwrap();
        c.execute("INSERT INTO agents(id,role,provider,source) VALUES ('a','worker','claudeCode','engine')",[]).unwrap();
        for (n, state) in ["queued", "delivered", "uncertain"].iter().enumerate() {
            c.execute("INSERT INTO messages(id,from_principal,to_agent,to_addr,origin_kind,provenance,body,body_hash,req_hash,state,state_at,created_at) VALUES (?1,'local-unverified','a','m/a','local-unverified','p','b','h','h',?2,1,1)",rusqlite::params![format!("msg_{}",n),state]).unwrap();
            c.execute(
                "INSERT INTO message_status(msg_id,state,at) VALUES (?1,?2,1)",
                rusqlite::params![format!("msg_{}", n), state],
            )
            .unwrap();
        }
        migrate(&mut c, MIGRATIONS, t.path()).unwrap();
        let wires = c
            .prepare("SELECT wire_id FROM messages")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(wires.len(), 3);
        for wire in &wires {
            assert_eq!(uuid::Uuid::parse_str(wire).unwrap().get_version_num(), 4);
        }
        assert_eq!(
            wires.iter().collect::<std::collections::HashSet<_>>().len(),
            3
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM message_status", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert!(!c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
        assert!(c
            .query_row("PRAGMA foreign_keys", [], |r| r.get::<_, bool>(0))
            .unwrap());
    }
    #[test]
    fn a_bad_v5_foreign_key_rolls_back_and_restores_the_pragma() {
        let t = tempfile::tempdir().unwrap();
        let mut c = Connection::open(t.path().join("db")).unwrap();
        migrate(&mut c, &MIGRATIONS[..5], t.path()).unwrap();
        c.pragma_update(None, "foreign_keys", false).unwrap();
        c.execute(
            "INSERT INTO message_status(msg_id,state,at) VALUES ('missing','queued',1)",
            [],
        )
        .unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        assert!(migrate(&mut c, MIGRATIONS, t.path())
            .unwrap_err()
            .contains("foreign_key_check"));
        assert_eq!(user_version(&c).unwrap(), 5);
        assert!(c
            .query_row("PRAGMA foreign_keys", [], |r| r.get::<_, bool>(0))
            .unwrap());
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM pragma_table_info('messages') WHERE name='wire_id'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}
