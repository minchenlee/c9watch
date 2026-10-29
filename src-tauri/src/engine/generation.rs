//! Session generations in the store (plan C1a-22, D22.13, A:474).
//!
//! `agents.generation` goes up by one at each spawn and resume (and, from
//! C1b, refresh and takeover). Each new generation adds a `session_alias`
//! row with the session id and transcript it runs on, so the history of an
//! agent's sessions stays when `provider_sid` moves on. A resume counts in
//! `resume_count`, except a resume after an engine restart (A:1010).
//!
//! The SessionStart hook of the current generation may rebind the session
//! (`bind_session`): CC can change the session id on `--resume`, and the
//! hook is where the engine learns the transcript path (A:1051).
//!
//! The engine's live table (`engine::agents`) holds the same generation
//! number with the PTY child; the caller registers it there after the
//! commit.

use super::db::outbox::NewEvent;
use super::db::{Tx, AGENT_COLUMNS};
use rusqlite::{params, OptionalExtension};

/// Why a generation starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenReason {
    Spawn,
    /// `claude --resume` after a crash or on request: counts in
    /// `resume_count`.
    Resume,
    /// The boot resume after a drain (`engine restart`): not counted.
    EngineRestart,
}

/// Start the next generation of engine agent `agent_id` on session
/// `provider_sid`. Returns the new generation. Emits `agent.upsert`.
pub fn begin_generation(
    tx: &mut Tx,
    machine: &str,
    agent_id: &str,
    reason: GenReason,
    provider_sid: &str,
    transcript_path: Option<&str>,
    now_ms: i64,
) -> rusqlite::Result<i64> {
    let counted = i64::from(reason == GenReason::Resume);
    let gen: i64 = tx.query_row(
        "UPDATE agents SET generation = generation + 1, resume_count = resume_count + ?2, \
         provider_sid = ?3 WHERE id = ?1 AND source = 'engine' RETURNING generation",
        params![agent_id, counted, provider_sid],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO session_alias (agent_id, generation, provider_sid, transcript_path, bound_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![agent_id, gen, provider_sid, transcript_path, now_ms],
    )?;
    emit_upsert(tx, machine, agent_id)?;
    Ok(gen)
}

/// Bind generation `gen` of `agent_id` to the session a SessionStart hook
/// reported. The same session id only fills in a missing transcript path;
/// a new id becomes `provider_sid` and gets its own alias row. Returns
/// whether anything changed.
pub fn bind_session(
    tx: &mut Tx,
    machine: &str,
    agent_id: &str,
    gen: i64,
    provider_sid: &str,
    transcript_path: Option<&str>,
    now_ms: i64,
) -> rusqlite::Result<bool> {
    let current: Option<(i64, Option<String>)> = tx
        .query_row(
            "SELECT rowid, transcript_path FROM session_alias \
             WHERE agent_id = ?1 AND generation = ?2 AND provider_sid = ?3 \
             ORDER BY rowid DESC LIMIT 1",
            params![agent_id, gen, provider_sid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let changed = match current {
        Some((_, Some(_))) => false,
        Some((rowid, None)) => {
            transcript_path.is_some()
                && tx.execute(
                    "UPDATE session_alias SET transcript_path = ?2 WHERE rowid = ?1",
                    params![rowid, transcript_path],
                )? > 0
        }
        None => {
            tx.execute(
                "INSERT INTO session_alias \
                 (agent_id, generation, provider_sid, transcript_path, bound_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![agent_id, gen, provider_sid, transcript_path, now_ms],
            )?;
            true
        }
    };
    let moved = tx.execute(
        "UPDATE agents SET provider_sid = ?3 WHERE id = ?1 AND generation = ?2 \
         AND provider_sid IS NOT ?3",
        params![agent_id, gen, provider_sid],
    )? > 0;
    if moved {
        emit_upsert(tx, machine, agent_id)?;
    }
    Ok(changed || moved)
}

/// The transcript path of `agent_id`'s generation `gen`, newest binding
/// first.
pub fn transcript_path(tx: &Tx, agent_id: &str, gen: i64) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        "SELECT transcript_path FROM session_alias WHERE agent_id = ?1 AND generation = ?2 \
         AND transcript_path IS NOT NULL ORDER BY rowid DESC LIMIT 1",
        params![agent_id, gen],
        |r| r.get(0),
    )
    .optional()
}

/// Emit `agent.upsert` with the row as it is now.
pub fn emit_upsert(tx: &mut Tx, machine: &str, agent_id: &str) -> rusqlite::Result<()> {
    let row = tx.query_row(
        &format!("SELECT {} FROM agents WHERE id = ?1", AGENT_COLUMNS),
        [agent_id],
        super::db::agent_row,
    )?;
    tx.emit(&NewEvent {
        topic: "agent.upsert".into(),
        subject: Some(format!("{}/{}", machine, agent_id)),
        origin: None,
        data: serde_json::to_value(&row).expect("AgentRow serializes"),
    })?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::engine::db::{self, Db};

    pub(crate) const MACHINE: &str = "m_test";

    /// A store with one free CC engine agent `id` at generation 0.
    pub(crate) fn db_with_agent(id: &str) -> (tempfile::TempDir, Db) {
        let t = tempfile::tempdir().unwrap();
        let db = db::open(&t.path().join("engine"), MACHINE).unwrap();
        let id = id.to_string();
        db.write_blocking(move |tx| {
            tx.execute(
                "INSERT INTO agents (id, mode, role, control, provider, source, state, \
                 machine_id) VALUES (?1, 'free', 'worker', 'pty', 'claudeCode', 'engine', \
                 'starting', ?2)",
                params![id, MACHINE],
            )
        })
        .unwrap();
        (t, db)
    }

    fn aliases(db: &Db) -> Vec<(i64, String, Option<String>)> {
        let conn = db.reader().unwrap();
        let mut st = conn
            .prepare(
                "SELECT generation, provider_sid, transcript_path FROM session_alias \
                 ORDER BY rowid",
            )
            .unwrap();
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        rows.map(Result::unwrap).collect()
    }

    fn row(db: &Db) -> (i64, i64, Option<String>) {
        let conn = db.reader().unwrap();
        conn.query_row(
            "SELECT generation, resume_count, provider_sid FROM agents",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }

    fn begin(db: &Db, reason: GenReason, sid: &'static str) -> i64 {
        db.write_blocking(move |tx| begin_generation(tx, MACHINE, "a1", reason, sid, None, 7))
            .unwrap()
            .0
    }

    #[test]
    fn each_spawn_and_resume_is_a_new_generation_with_an_alias_row() {
        let (_t, db) = db_with_agent("a1");
        assert_eq!(begin(&db, GenReason::Spawn, "s1"), 1);
        assert_eq!(row(&db), (1, 0, Some("s1".into())));
        assert_eq!(begin(&db, GenReason::Resume, "s1"), 2);
        assert_eq!(row(&db), (2, 1, Some("s1".into())));
        // A boot resume after a drain is not counted.
        assert_eq!(begin(&db, GenReason::EngineRestart, "s2"), 3);
        assert_eq!(row(&db), (3, 1, Some("s2".into())));
        assert_eq!(
            aliases(&db),
            vec![
                (1, "s1".into(), None),
                (2, "s1".into(), None),
                (3, "s2".into(), None)
            ]
        );
        // Each one emitted the row.
        let conn = db.reader().unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE topic = 'agent.upsert'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 3);
        // Only engine rows have generations.
        assert!(db
            .write_blocking(|tx| begin_generation(
                tx,
                MACHINE,
                "nope",
                GenReason::Spawn,
                "s",
                None,
                7
            ))
            .is_err());
    }

    #[test]
    fn session_start_binds_the_transcript_and_rebinds_a_new_session_id() {
        let (_t, db) = db_with_agent("a1");
        begin(&db, GenReason::Spawn, "s1");
        let bind = |sid: &'static str, path: Option<&'static str>| {
            db.write_blocking(move |tx| bind_session(tx, MACHINE, "a1", 1, sid, path, 9))
                .unwrap()
                .0
        };
        assert!(bind("s1", Some("/t/s1.jsonl")));
        assert!(!bind("s1", Some("/t/s1.jsonl")));
        assert!(!bind("s1", None));
        // CC moved to a new session id: a new alias row, and provider_sid.
        assert!(bind("s9", Some("/t/s9.jsonl")));
        assert_eq!(row(&db).2.as_deref(), Some("s9"));
        assert_eq!(
            aliases(&db),
            vec![
                (1, "s1".into(), Some("/t/s1.jsonl".into())),
                (1, "s9".into(), Some("/t/s9.jsonl".into()))
            ]
        );
        let path = db
            .write_blocking(|tx| transcript_path(tx, "a1", 1))
            .unwrap()
            .0;
        assert_eq!(path.as_deref(), Some("/t/s9.jsonl"));
        // An old generation's binding never moves provider_sid.
        begin(&db, GenReason::Resume, "s9");
        db.write_blocking(|tx| bind_session(tx, MACHINE, "a1", 1, "s1", None, 9))
            .unwrap();
        assert_eq!(row(&db).2.as_deref(), Some("s9"));
    }
}
