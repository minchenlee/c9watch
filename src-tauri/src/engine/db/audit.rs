//! The audit log: one row per decision the engine made on a request.
//!
//! Append-only. Nothing prunes it in v1: event retention (24 h / 50k) only
//! ever touches `events`. A row is written in the same transaction as the
//! change it records, so the log never claims a change that rolled back.

use rusqlite::{params, Connection};

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
