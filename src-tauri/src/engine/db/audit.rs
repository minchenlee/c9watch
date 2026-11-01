//! The audit log: one row per decision the engine made on a request.
//!
//! Append-only, apart from retention (D10, `prune`): rows older than
//! `AuditRetention::max_age` go, and so do all but the newest
//! `AuditRetention::max_rows`. Event retention (24 h / 50k) is separate and
//! never touches `audit`. A row is written in the same transaction as the
//! change it records, so the log never claims a change that rolled back.
//! Successful reads and hooks write no audit row, which keeps the volume
//! low.

use rusqlite::{params, Connection};
use std::time::Duration;

/// How often the engine runs audit retention after boot. The age check
/// scans the table (there is no `ts` index), so it runs hourly, not with
/// event retention every minute.
pub const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);

/// How long, and how many, audit rows are kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRetention {
    /// Rows older than this go. Default 365 days.
    pub max_age: Duration,
    /// All but the newest `max_rows` go: the size cap. Default 500 000
    /// (a row is a few hundred bytes, so about 100-250 MB at most).
    pub max_rows: u64,
}

impl Default for AuditRetention {
    fn default() -> Self {
        AuditRetention {
            max_age: Duration::from_secs(365 * 24 * 60 * 60),
            max_rows: 500_000,
        }
    }
}

/// Apply `keep` at `now` (unix ms). Returns how many rows went. Runs on the
/// writable connection (boot, or a writer job). The newest row always
/// stays, so `seq` (no AUTOINCREMENT) is never handed out again.
pub fn prune(conn: &Connection, now: i64, keep: &AuditRetention) -> rusqlite::Result<usize> {
    let age_ms = i64::try_from(keep.max_age.as_millis()).unwrap_or(i64::MAX);
    let cutoff = now.saturating_sub(age_ms);
    let max_rows = i64::try_from(keep.max_rows.max(1)).unwrap_or(i64::MAX);
    let old = conn.execute(
        "DELETE FROM audit WHERE ts < ?1 AND seq < (SELECT max(seq) FROM audit)",
        params![cutoff],
    )?;
    // Below the `max_rows`-th newest row; exact even with gaps in `seq`.
    // NULL (fewer rows than the cap) deletes nothing.
    let over = conn.execute(
        "DELETE FROM audit WHERE seq < \
         (SELECT seq FROM audit ORDER BY seq DESC LIMIT 1 OFFSET ?1 - 1)",
        params![max_rows],
    )?;
    Ok(old + over)
}

/// Principal of rows the engine writes on its own behalf.
pub const SYSTEM: &str = "system";

/// One audit row, before `seq` and `ts` are assigned.
#[derive(Debug, Clone, Default)]
pub struct AuditEntry<'a> {
    /// Who asked, as the engine stamped it (`agent:<id>`, `local-unverified`,
    /// or `system`).
    pub principal: &'a str,
    /// The request's `origin` object as JSON, when it has one.
    pub origin: Option<&'a str>,
    pub method: &'a str,
    pub target: Option<&'a str>,
    /// `allow`, `deny`, or a method-specific outcome.
    pub decision: &'a str,
    pub reason: Option<&'a str>,
    pub payload_hash: Option<&'a str>,
}

/// Append `e` at `ts` (unix ms). Returns its `seq`.
pub fn append(conn: &Connection, ts: i64, e: &AuditEntry) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO audit (ts, principal, origin, method, target, decision, reason, payload_hash) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            ts,
            e.principal,
            e.origin,
            e.method,
            e.target,
            e.decision,
            e.reason,
            e.payload_hash
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn_with(rows: &[i64]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE audit (seq INTEGER PRIMARY KEY, ts INTEGER NOT NULL, \
             principal TEXT NOT NULL, origin TEXT, method TEXT NOT NULL, target TEXT, \
             decision TEXT NOT NULL, reason TEXT, payload_hash TEXT)",
        )
        .unwrap();
        for ts in rows {
            let e = AuditEntry {
                principal: SYSTEM,
                method: "m",
                decision: "allow",
                ..Default::default()
            };
            append(&c, *ts, &e).unwrap();
        }
        c
    }

    fn seqs(c: &Connection) -> Vec<i64> {
        let mut st = c.prepare("SELECT seq FROM audit ORDER BY seq").unwrap();
        let rows = st.query_map([], |r| r.get(0)).unwrap();
        rows.map(Result::unwrap).collect()
    }

    const DAY: i64 = 24 * 60 * 60 * 1000;

    #[test]
    fn default_keeps_a_year_and_half_a_million_rows() {
        let d = AuditRetention::default();
        assert_eq!(d.max_age, Duration::from_secs(365 * 86_400));
        assert_eq!(d.max_rows, 500_000);
    }

    #[test]
    fn rows_older_than_max_age_go() {
        let now = 1000 * DAY;
        let c = conn_with(&[now - 400 * DAY, now - 366 * DAY, now - 364 * DAY, now]);
        let n = prune(&c, now, &AuditRetention::default()).unwrap();
        assert_eq!(n, 2);
        assert_eq!(seqs(&c), vec![3, 4]);
    }

    #[test]
    fn only_the_newest_max_rows_stay() {
        let now = 10 * DAY;
        let c = conn_with(&[now; 6]);
        let keep = AuditRetention {
            max_rows: 4,
            ..Default::default()
        };
        assert_eq!(prune(&c, now, &keep).unwrap(), 2);
        assert_eq!(seqs(&c), vec![3, 4, 5, 6]);
        // Idempotent.
        assert_eq!(prune(&c, now, &keep).unwrap(), 0);
    }

    #[test]
    fn the_row_cap_counts_rows_not_seqs() {
        let now = 10 * DAY;
        let c = conn_with(&[now; 6]);
        c.execute("DELETE FROM audit WHERE seq IN (2, 3, 4)", [])
            .unwrap();
        let keep = AuditRetention {
            max_rows: 3,
            ..Default::default()
        };
        assert_eq!(prune(&c, now, &keep).unwrap(), 0);
        assert_eq!(seqs(&c), vec![1, 5, 6]);
        let keep = AuditRetention {
            max_rows: 2,
            ..Default::default()
        };
        assert_eq!(prune(&c, now, &keep).unwrap(), 1);
        assert_eq!(seqs(&c), vec![5, 6]);
    }

    #[test]
    fn the_newest_row_always_stays() {
        let c = conn_with(&[1, 2, 3]);
        let keep = AuditRetention {
            max_age: Duration::from_millis(1),
            max_rows: 0,
        };
        assert_eq!(prune(&c, 1000 * DAY, &keep).unwrap(), 2);
        assert_eq!(seqs(&c), vec![3]);
    }

    #[test]
    fn an_empty_table_and_huge_limits_are_fine() {
        let c = conn_with(&[]);
        let keep = AuditRetention {
            max_age: Duration::MAX,
            max_rows: u64::MAX,
        };
        assert_eq!(prune(&c, 0, &keep).unwrap(), 0);
        let c = conn_with(&[1, 2]);
        assert_eq!(prune(&c, i64::MAX, &keep).unwrap(), 0);
    }
}
