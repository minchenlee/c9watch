//! The ops journal and boot reconcile (plan C1a-26, D:188).
//!
//! A free `agent.create` is one `ops` row of kind `agent.create` that walks
//! the phases `row` → `pty` → `bound` (the `worktree` phase of D:188 is for
//! managed agents and is skipped). Each phase is written in the same
//! transaction as the state it records:
//! - `row`: the `agents` row (`creating`), its first generation and the op.
//! - `pty`: the PTY child's pid and start time, in the payload and in
//!   `agent_state`; the row moves to `starting`.
//! - `bound`: the agent is `ready`; the op is done.
//!
//! On boot, before the socket is bound and so before any mutation, `reconcile`
//! undoes what a stopped engine left (A:231):
//! - An open create op: its PTY child, if one was recorded, is killed by pid
//!   **and** start time (`pty::kill_orphan`; a reused pid is never
//!   signalled), and the agent ends `failed` with `create_interrupted`.
//! - An engine agent that was live (`starting`, `ready`, `working`,
//!   `blocked`, `refreshing`): its PTY child is killed the same way, since
//!   no engine can reach its terminal any more, and it is marked
//!   `interrupted` with the note pending (C1a-25). Resuming it is M6.
//! - An agent left `retiring` ends `retired`; one left `creating` without an
//!   open op ends `failed`.
//!
//! `paused`, `interrupted`, `retired`, `failed` and `orphaned` rows are left
//! alone. The kills run before the write, outside any transaction.

use super::adapter::cc::profile::settings_path_of;
use super::db::audit::{self, AuditEntry};
use super::db::outbox::NewEvent;
use super::db::{new_agent_id, now_ms, Db, Tx};
use super::driver::ChildId;
use super::generation::emit_upsert;
use super::pty::{kill_orphan, Orphan};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// `ops.kind` of a free create.
pub const KIND_CREATE: &str = "agent.create";

/// Why boot failed an agent whose create op was still open.
pub const CREATE_INTERRUPTED: &str = "create_interrupted";

/// The phases of a free create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Row,
    Pty,
    Bound,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Row => "row",
            Phase::Pty => "pty",
            Phase::Bound => "bound",
        }
    }
}

/// The payload of a create op.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePayload {
    pub agent_id: String,
    pub generation: i64,
    pub session_id: String,
    pub cwd: String,
    /// The PTY child, from phase `pty` on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    /// How the op ended: `bound`, or the failure reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

impl CreatePayload {
    /// The recorded PTY child.
    pub fn child(&self) -> Option<ChildId> {
        Some(ChildId {
            pid: self.pid?.try_into().ok()?,
            start: self.start?,
        })
    }
}

/// A new op id, `op_<ulid>`.
pub fn new_op_id() -> String {
    format!("op_{}", &new_agent_id()["agt_".len()..])
}

fn to_json(p: &CreatePayload) -> String {
    serde_json::to_string(p).expect("CreatePayload serializes")
}

/// Open a create op in phase `row`.
pub fn open_create(tx: &Tx, op_id: &str, p: &CreatePayload, now: i64) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO ops (id, kind, phase, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![op_id, KIND_CREATE, Phase::Row.as_str(), to_json(p), now],
    )?;
    Ok(())
}

/// Move an open op to `phase` with payload `p`. Returns whether the op was
/// still open.
pub fn set_phase(tx: &Tx, op_id: &str, phase: Phase, p: &CreatePayload) -> rusqlite::Result<bool> {
    let n = tx.execute(
        "UPDATE ops SET phase = ?2, payload = ?3 WHERE id = ?1 AND done_at IS NULL",
        params![op_id, phase.as_str(), to_json(p)],
    )?;
    Ok(n > 0)
}

/// Close an open op with `outcome`. Returns whether it was still open.
pub fn finish(tx: &Tx, op_id: &str, outcome: &str, now: i64) -> rusqlite::Result<bool> {
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload FROM ops WHERE id = ?1 AND done_at IS NULL",
            [op_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(payload) = payload else {
        return Ok(false);
    };
    let mut p: CreatePayload = serde_json::from_str(&payload).unwrap_or_default();
    p.outcome = Some(outcome.to_string());
    tx.execute(
        "UPDATE ops SET payload = ?2, done_at = ?3 WHERE id = ?1",
        params![op_id, to_json(&p), now],
    )?;
    Ok(true)
}

/// Mark engine agent `agent_id` `failed` and gone, forget its PTY child,
/// and emit `agent.upsert` and `agent.failed` with `reason` and `detail`.
/// Does not touch a row that is already retired or failed.
pub fn fail_agent(
    tx: &mut Tx,
    machine: &str,
    agent_id: &str,
    reason: &str,
    detail: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    let n = tx.execute(
        "UPDATE agents SET state = 'failed', gone_at = ?2 WHERE id = ?1 \
         AND source = 'engine' AND state NOT IN ('retired', 'failed')",
        params![agent_id, now],
    )?;
    tx.execute("DELETE FROM agent_state WHERE agent_id = ?1", [agent_id])?;
    if n == 0 {
        return Ok(false);
    }
    let why = format!("{}: {}", reason, detail);
    audit::append(
        tx,
        now,
        &AuditEntry {
            principal: audit::SYSTEM,
            method: KIND_CREATE,
            target: Some(agent_id),
            decision: "failed",
            reason: Some(&why),
            ..Default::default()
        },
    )?;
    emit_upsert(tx, machine, agent_id)?;
    tx.emit(&NewEvent {
        topic: "agent.failed".into(),
        subject: Some(format!("{}/{}", machine, agent_id)),
        origin: None,
        data: serde_json::json!({ "id": agent_id, "reason": reason, "detail": detail }),
    })?;
    Ok(true)
}

/// What `reconcile` did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// Agents of open create ops, now `failed`.
    pub failed: Vec<String>,
    /// Live agents of the old engine, now `interrupted`.
    pub interrupted: Vec<String>,
    /// Agents left `retiring`, now `retired`.
    pub retired: Vec<String>,
    /// PTY children killed.
    pub killed: usize,
    /// Recorded PTY children whose pid now belongs to another process.
    pub pid_reused: usize,
    /// Kills that failed, with the error.
    pub kill_errors: Vec<String>,
}

impl Reconciled {
    pub fn is_empty(&self) -> bool {
        *self == Reconciled::default()
    }
}

/// What one row of the old engine becomes.
#[derive(Debug, Clone)]
enum Fix {
    /// An open create op and its agent.
    Create { op_id: String, p: CreatePayload },
    /// A live agent without an open op.
    Live {
        agent_id: String,
        gen: i64,
        state: String,
        child: Option<ChildId>,
    },
}

/// Read what the old engine left.
fn plan(db: &Db) -> Result<Vec<Fix>, String> {
    let conn = db.reader()?;
    let fail = |e: rusqlite::Error| format!("Failed to read the ops journal: {}", e);
    let mut fixes = Vec::new();
    let mut open = Vec::new();
    {
        let mut st = conn
            .prepare(
                "SELECT id, payload FROM ops WHERE done_at IS NULL AND kind = ?1 \
                 ORDER BY created_at, id",
            )
            .map_err(fail)?;
        let rows = st
            .query_map([KIND_CREATE], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .map_err(fail)?;
        for row in rows {
            let (op_id, payload) = row.map_err(fail)?;
            let p: CreatePayload = payload
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            open.push(p.agent_id.clone());
            fixes.push(Fix::Create { op_id, p });
        }
    }
    let mut st = conn
        .prepare(
            "SELECT a.id, a.generation, a.state, s.pty_child_pid, s.pty_child_start \
             FROM agents a LEFT JOIN agent_state s ON s.agent_id = a.id \
             WHERE a.source = 'engine' AND a.control = 'pty' AND a.gone_at IS NULL \
             AND a.state IN ('creating', 'starting', 'ready', 'working', 'blocked', \
             'refreshing', 'retiring') ORDER BY a.id",
        )
        .map_err(fail)?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })
        .map_err(fail)?;
    for row in rows {
        let (agent_id, gen, state, pid, start) = row.map_err(fail)?;
        if open.contains(&agent_id) {
            continue;
        }
        let child = pid.zip(start).map(|(pid, start)| ChildId {
            pid: pid as libc::pid_t,
            start: start as u64,
        });
        fixes.push(Fix::Live {
            agent_id,
            gen,
            state,
            child,
        });
    }
    Ok(fixes)
}

/// Kill `child` if it is still the recorded process.
fn kill(child: Option<ChildId>, done: &mut Reconciled) -> Option<String> {
    let child = child?;
    match kill_orphan(child) {
        Ok(Orphan::Killed) => {
            done.killed += 1;
            None
        }
        Ok(Orphan::PidReused) => {
            done.pid_reused += 1;
            None
        }
        Ok(Orphan::Gone) => None,
        Err(e) => {
            let e = e.to_string();
            done.kill_errors.push(e.clone());
            Some(e)
        }
    }
}

/// Undo or finish everything a stopped engine left (see the module doc).
/// Blocking. Run it once at boot, before the socket is bound. `home` is the
/// engine home, for the per-spawn settings files of the agents that end.
pub fn reconcile(db: &Db, home: &Path) -> Result<Reconciled, String> {
    let mut done = Reconciled::default();
    let machine = db.machine_id().to_string();
    for fix in plan(db)? {
        match fix {
            Fix::Create { op_id, p } => {
                let killed = kill(p.child(), &mut done);
                let phase = if p.child().is_some() { "pty" } else { "row" };
                let detail = match killed {
                    None => format!("the engine stopped in phase {}", phase),
                    Some(e) => format!("the engine stopped in phase {}; {}", phase, e),
                };
                let (m, id) = (machine.clone(), p.agent_id.clone());
                db.write_blocking(move |tx| {
                    let now = now_ms();
                    finish(tx, &op_id, CREATE_INTERRUPTED, now)?;
                    fail_agent(tx, &m, &id, CREATE_INTERRUPTED, &detail, now)
                })?;
                let _ = std::fs::remove_file(settings_path_of(home, &p.agent_id, p.generation));
                done.failed.push(p.agent_id);
            }
            Fix::Live {
                agent_id,
                gen,
                state,
                child,
            } => {
                let killed = kill(child, &mut done);
                let m = machine.clone();
                let id = agent_id.clone();
                let st = state.clone();
                db.write_blocking(move |tx| live_fix(tx, &m, &id, &st, killed.as_deref()))?;
                match state.as_str() {
                    "retiring" => {
                        let _ = std::fs::remove_file(settings_path_of(home, &agent_id, gen));
                        done.retired.push(agent_id);
                    }
                    "creating" => {
                        let _ = std::fs::remove_file(settings_path_of(home, &agent_id, gen));
                        done.failed.push(agent_id);
                    }
                    _ => done.interrupted.push(agent_id),
                }
            }
        }
    }
    Ok(done)
}

/// The write for one live agent of the old engine.
fn live_fix(
    tx: &mut Tx,
    machine: &str,
    agent_id: &str,
    state: &str,
    kill_error: Option<&str>,
) -> rusqlite::Result<()> {
    let now = now_ms();
    if state == "creating" {
        let detail = "no open create op";
        fail_agent(tx, machine, agent_id, CREATE_INTERRUPTED, detail, now)?;
        return Ok(());
    }
    let (new_state, reason) = if state == "retiring" {
        ("retired", "the engine stopped while retiring")
    } else {
        ("interrupted", "the engine stopped while the agent ran")
    };
    if new_state == "retired" {
        tx.execute(
            "UPDATE agents SET state = 'retired', gone_at = ?2 WHERE id = ?1",
            params![agent_id, now],
        )?;
    } else {
        tx.execute(
            "UPDATE agents SET state = 'interrupted', interrupted_note_pending = 1 \
             WHERE id = ?1",
            [agent_id],
        )?;
    }
    tx.execute("DELETE FROM agent_state WHERE agent_id = ?1", [agent_id])?;
    let reason = match kill_error {
        Some(e) => format!("{}; {}", reason, e),
        None => reason.to_string(),
    };
    audit::append(
        tx,
        now,
        &AuditEntry {
            principal: audit::SYSTEM,
            method: "engine.reconcile",
            target: Some(agent_id),
            decision: new_state,
            reason: Some(&reason),
            ..Default::default()
        },
    )?;
    emit_upsert(tx, machine, agent_id)
}
