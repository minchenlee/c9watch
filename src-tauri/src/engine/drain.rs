//! Drain (plan C1a-27, A:228-237, D:184): what the engine does between a
//! SIGTERM and its exit, so the next engine can resume its agents (C1a-28).
//!
//! C1a has no `engine.drain` method (§1.1): only SIGTERM starts a drain
//! (`c9watch engine restart`, `kill -TERM`). SIGINT and `run_until`'s stop
//! still stop at once, and boot reconcile marks what they leave.
//!
//! 1. The `DrainSwitch` goes on, and from then on nothing is injected and
//!    nothing is spawned:
//!    - `agent.create` answers `draining`. A create that has its row but no
//!      PTY yet spawns nothing and leaves its op to boot reconcile.
//!    - `hook.session_start` is still taken but answers with no context and
//!      keeps the interrupted note pending (`engine::standing`).
//!    - A PTY write (C1a: the trust-dialog keys) is not sent.
//!
//!    Each blocked injection is an audit row `drain.blocked_injection` with
//!    reason `path=hook` or `path=pty`.
//! 2. Each agent this engine supervises gets what the state table
//!    (`action`) says:
//!    - `ready`: its supervisor kills it (SIGTERM first) and the row becomes
//!      `paused` with reason `drain` and its `resume_argv`.
//!    - Mid-turn (`starting`, `working`, `blocked`, `refreshing`,
//!      `retiring`): wait up to `TURN_WAIT` for the turn end; a turn that
//!      ends meanwhile is `ready` and goes the same way. After that the
//!      supervisor kills it and the row becomes `interrupted` with reason
//!      `drain`, its `resume_argv` and `interrupted_note_pending`.
//!    - An agent whose create op is still open (`creating`, `starting`
//!      before `bound`): left as it is. The engine kills its PTY when it
//!      stops, and boot reconcile fails it (A:231).
//!    - `paused`, `interrupted`, `failed`, `retired` and `orphaned` rows
//!      have no supervisor here and are not touched.
//! 3. `HARD_DEADLINE` after the start the engine exits, whatever is left;
//!    boot reconcile handles it. `run` keeps `SHUTDOWN_RESERVE` of it for
//!    the final kills.
//!
//! The kill runs before the write, outside any transaction, and the write
//! decides from the state it reads: a turn that ended just before the kill
//! is `paused`, not `interrupted`.

use super::adapter;
use super::create::Spawner;
use super::db::audit::{self, AuditEntry};
use super::db::{now_ms, Db, Tx};
use super::driver::{KILL_GRACE, KILL_VERIFY};
use super::generation::emit_upsert;
use super::ops;
use rusqlite::{params, OptionalExtension};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long a mid-turn agent may take to end its turn (A:231).
pub const TURN_WAIT: Duration = Duration::from_secs(60);

/// From the SIGTERM to the exit (A:233).
pub const HARD_DEADLINE: Duration = Duration::from_secs(90);

/// The part of `HARD_DEADLINE` kept for the final kills: one `kill` of a
/// stuck process group.
pub const SHUTDOWN_RESERVE: Duration =
    Duration::from_secs(KILL_GRACE.as_secs() + KILL_VERIFY.as_secs());

/// How often the drain looks at the agents again.
const TICK: Duration = Duration::from_millis(250);

/// `agents.state_reason` of the rows a drain left.
pub const REASON_DRAIN: &str = "drain";

/// The audit method of a blocked injection.
pub const BLOCKED_INJECTION: &str = "drain.blocked_injection";

/// The audit method of a row a drain settled.
pub const METHOD_DRAIN: &str = "engine.drain";

/// The engine-wide drain switch. Once on, it stays on until the engine
/// exits.
#[derive(Debug, Default)]
pub struct DrainSwitch(AtomicBool);

impl DrainSwitch {
    pub fn new() -> DrainSwitch {
        DrainSwitch::default()
    }

    /// Turn the switch on. Returns whether it was off.
    pub fn start(&self) -> bool {
        !self.0.swap(true, Ordering::SeqCst)
    }

    pub fn on(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The two injection paths (A:228).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionPath {
    /// SessionStart `additionalContext`.
    Hook,
    /// Bytes written to the agent's terminal.
    Pty,
}

impl InjectionPath {
    pub fn as_str(self) -> &'static str {
        match self {
            InjectionPath::Hook => "hook",
            InjectionPath::Pty => "pty",
        }
    }
}

/// Record an injection the drain blocked: `drain.blocked_injection` with
/// reason `path=<path>`.
pub fn audit_blocked(
    tx: &Tx,
    principal: &str,
    agent_id: &str,
    path: InjectionPath,
    now: i64,
) -> rusqlite::Result<()> {
    let reason = format!("path={}", path.as_str());
    audit::append(
        tx,
        now,
        &AuditEntry {
            principal,
            method: BLOCKED_INJECTION,
            target: Some(agent_id),
            decision: "deny",
            reason: Some(&reason),
            ..Default::default()
        },
    )?;
    Ok(())
}

/// What the drain does with one supervised agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Idle at the prompt: stop it now, `paused(drain)`.
    Pause,
    /// Mid-turn: wait for the turn end, up to `TURN_WAIT`.
    Wait,
    /// Not the drain's: an open create op, or not live.
    Leave,
}

/// The state table (see the module doc). `create_open`: the agent's create
/// op is still open.
pub fn action(state: Option<&str>, create_open: bool) -> Action {
    if create_open {
        return Action::Leave;
    }
    match state {
        Some("ready") => Action::Pause,
        Some("starting" | "working" | "blocked" | "refreshing" | "retiring") => Action::Wait,
        _ => Action::Leave,
    }
}

/// The argv that resumes `session_id` of an agent of `provider` (A:1038).
pub fn resume_argv(provider: &str, session_id: &str) -> Option<Vec<String>> {
    match adapter::descriptor_for_provider(provider)?.kind {
        "claude" => Some(adapter::claude_resume_argv(session_id)),
        _ => None,
    }
}

/// The write after the supervisor killed a drained agent: `ready` becomes
/// `paused`, a live mid-turn state `interrupted` with the note pending;
/// both get reason `drain` and their `resume_argv`. Any other row is left
/// alone. Returns the new state.
pub fn settle(tx: &mut Tx, machine: &str, agent_id: &str) -> rusqlite::Result<Option<String>> {
    let row: Option<(Option<String>, String, Option<String>)> = tx
        .query_row(
            "SELECT state, provider, provider_sid FROM agents \
             WHERE id = ?1 AND source = 'engine' AND gone_at IS NULL",
            [agent_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((state, provider, sid)) = row else {
        return Ok(None);
    };
    let new = match action(state.as_deref(), false) {
        Action::Pause => "paused",
        Action::Wait => "interrupted",
        Action::Leave => return Ok(None),
    };
    let argv = sid
        .as_deref()
        .and_then(|sid| resume_argv(&provider, sid))
        .map(|a| serde_json::to_string(&a).expect("argv serializes"));
    tx.execute(
        "UPDATE agents SET state = ?2, state_reason = ?3, resume_argv = ?4, \
         interrupted_note_pending = CASE WHEN ?2 = 'interrupted' THEN 1 \
         ELSE interrupted_note_pending END WHERE id = ?1",
        params![agent_id, new, REASON_DRAIN, argv],
    )?;
    tx.execute("DELETE FROM agent_state WHERE agent_id = ?1", [agent_id])?;
    let reason = format!("drain from {}", state.as_deref().unwrap_or("null"));
    audit::append(
        tx,
        now_ms(),
        &AuditEntry {
            principal: audit::SYSTEM,
            method: METHOD_DRAIN,
            target: Some(agent_id),
            decision: new,
            reason: Some(&reason),
            ..Default::default()
        },
    )?;
    emit_upsert(tx, machine, agent_id)?;
    Ok(Some(new.to_string()))
}

/// The drain's waits. `Default` is the engine's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub turn_wait: Duration,
    /// When the drain gives up on what is left.
    pub deadline: Duration,
    /// Real time between two looks.
    pub tick: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            turn_wait: TURN_WAIT,
            deadline: HARD_DEADLINE - SHUTDOWN_RESERVE,
            tick: TICK,
        }
    }
}

/// What a drain did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Now `paused(drain)`.
    pub paused: Vec<String>,
    /// Now `interrupted`, with the note pending.
    pub interrupted: Vec<String>,
    /// Left to boot reconcile: an open create op, or no live state.
    pub left: Vec<String>,
    /// Stopped but not settled by the deadline (or settled otherwise,
    /// such as an exit of its own).
    pub unsettled: Vec<String>,
    /// The deadline came first.
    pub timed_out: bool,
}

/// The state and whether the create op is still open, for `agent_id`.
fn look(db: &Db, agent_id: &str) -> Result<(Option<String>, bool), String> {
    let conn = db.reader()?;
    let fail = |e: rusqlite::Error| format!("Failed to read agent {}: {}", agent_id, e);
    let state: Option<String> = conn
        .query_row(
            "SELECT state FROM agents WHERE id = ?1 AND gone_at IS NULL",
            [agent_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(fail)?
        .flatten();
    let open = conn
        .query_row(
            "SELECT 1 FROM ops WHERE kind = ?1 AND done_at IS NULL \
             AND json_extract(payload, '$.agentId') = ?2",
            params![ops::KIND_CREATE, agent_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(fail)?
        .is_some();
    Ok((state, open))
}

/// Drain the agents of `sp` (see the module doc). The caller has turned
/// `sp`'s switch on at `start`, by `sp`'s clock (`Spawner::now`). Blocking;
/// returns when every agent is settled or left, or at `start` +
/// `timing.deadline`.
pub fn run(sp: &Spawner, timing: Timing, start: Instant) -> Report {
    let mut stopped: Vec<String> = Vec::new();
    let mut left: HashSet<String> = HashSet::new();
    let mut timed_out = false;
    loop {
        let elapsed = sp.now().saturating_duration_since(start);
        let live = sp.supervised();
        let mut waiting = 0;
        for id in &live {
            if stopped.contains(id) {
                waiting += 1;
                continue;
            }
            let (state, open) = match look(sp.db(), id) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[engine] Drain: {}", e);
                    waiting += 1;
                    continue;
                }
            };
            let stop = match action(state.as_deref(), open) {
                Action::Pause => true,
                Action::Wait => elapsed >= timing.turn_wait,
                Action::Leave => {
                    left.insert(id.clone());
                    continue;
                }
            };
            left.remove(id);
            waiting += 1;
            if stop && sp.drain_agent(id) {
                stopped.push(id.clone());
            }
        }
        if waiting == 0 {
            break;
        }
        if elapsed >= timing.deadline {
            timed_out = true;
            break;
        }
        std::thread::sleep(timing.tick);
    }
    let mut report = Report {
        timed_out,
        ..Default::default()
    };
    let still = sp.supervised();
    for id in stopped {
        let state = look(sp.db(), &id).ok().and_then(|(s, _)| s);
        match state.as_deref() {
            _ if still.contains(&id) => report.unsettled.push(id),
            Some("paused") => report.paused.push(id),
            Some("interrupted") => report.interrupted.push(id),
            _ => report.unsettled.push(id),
        }
    }
    report.left = left.into_iter().filter(|id| still.contains(id)).collect();
    report.left.sort();
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::generation::tests::{db_with_agent, MACHINE};

    const LIVE: &[&str] = &["starting", "working", "blocked", "refreshing", "retiring"];
    const UNTOUCHED: &[&str] = &[
        "creating",
        "paused",
        "interrupted",
        "failed",
        "retired",
        "orphaned",
    ];

    #[test]
    fn the_state_table() {
        assert_eq!(action(Some("ready"), false), Action::Pause);
        for s in LIVE {
            assert_eq!(action(Some(s), false), Action::Wait, "{}", s);
        }
        for s in UNTOUCHED {
            assert_eq!(action(Some(s), false), Action::Leave, "{}", s);
        }
        assert_eq!(action(None, false), Action::Leave);
        // An open create op is boot reconcile's, whatever the state.
        for s in ["creating", "starting", "ready", "working"] {
            assert_eq!(action(Some(s), true), Action::Leave, "{}", s);
        }
    }

    /// (state, state_reason, resume_argv, interrupted_note_pending)
    fn facts(db: &Db) -> (Option<String>, Option<String>, Option<String>, i64) {
        db.reader()
            .unwrap()
            .query_row(
                "SELECT state, state_reason, resume_argv, interrupted_note_pending \
                 FROM agents WHERE id = 'a1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
    }

    fn settle_from(
        state: &str,
    ) -> (
        Option<String>,
        (Option<String>, Option<String>, Option<String>, i64),
    ) {
        let (_t, db) = db_with_agent("a1");
        let st = state.to_string();
        let (new, _) = db
            .write_blocking(move |tx| {
                tx.execute(
                    "UPDATE agents SET state = ?1, provider = 'claudeCode', provider_sid = 's-1', \
                     source = 'engine', control = 'pty' WHERE id = 'a1'",
                    [&st],
                )?;
                tx.execute(
                    "INSERT INTO agent_state (agent_id, gen, pty_child_pid, pty_child_start) \
                     VALUES ('a1', 1, 10, 100)",
                    [],
                )?;
                settle(tx, MACHINE, "a1")
            })
            .unwrap();
        (new, facts(&db))
    }

    #[test]
    fn settle_follows_the_state_table() {
        let argv = Some(r#"["claude","--resume","s-1"]"#.to_string());
        let drain = Some(REASON_DRAIN.to_string());
        assert_eq!(
            settle_from("ready"),
            (
                Some("paused".into()),
                (Some("paused".into()), drain.clone(), argv.clone(), 0)
            )
        );
        for s in LIVE {
            assert_eq!(
                settle_from(s),
                (
                    Some("interrupted".into()),
                    (Some("interrupted".into()), drain.clone(), argv.clone(), 1)
                ),
                "{}",
                s
            );
        }
        for s in UNTOUCHED {
            assert_eq!(
                settle_from(s),
                (None, (Some(s.to_string()), None, None, 0)),
                "{}",
                s
            );
        }
    }

    #[test]
    fn the_switch_turns_on_once() {
        let d = DrainSwitch::new();
        assert!(!d.on());
        assert!(d.start());
        assert!(!d.start());
        assert!(d.on());
    }

    #[test]
    fn the_hard_deadline_keeps_room_for_the_final_kills() {
        let t = Timing::default();
        assert_eq!(t.turn_wait, Duration::from_secs(60));
        assert_eq!(t.deadline + SHUTDOWN_RESERVE, Duration::from_secs(90));
        assert!(t.turn_wait + SHUTDOWN_RESERVE < t.deadline);
        assert_eq!(
            resume_argv("claudeCode", "s"),
            Some(vec!["claude".into(), "--resume".into(), "s".into()])
        );
        assert_eq!(resume_argv("codex", "s"), None);
    }
}
