//! Hook event intake (plan C1a-22): what the engine does with a `hook.*`
//! call after `api::authorize` let it in.
//!
//! In order, in one write transaction:
//! 1. The agent row: a live engine row (`source=engine`, `control=pty`).
//!    Anything else is dropped as `unknown_agent`.
//! 2. **Kind before dedupe** (A:1056): the hook's `kind` must be one the
//!    row's adapter accepts. A Grok agent running the claude hook script
//!    sends `kind=claude` with Grok's agent id; it is dropped here, before
//!    it can take a `seq` slot of the real agent.
//! 3. **Generation** (A:474, A:1051): the event's generation is the one the
//!    engine stamped on the connection from its live table (C1a-33), never
//!    a value the agent sends. An event from a generation that is no longer
//!    the live one, or no longer the row's, is dropped.
//! 4. **Dedupe** by `agent_id + seq` in the `dedupe` table, for
//!    `db::DEDUPE_WINDOW_MS`.
//! 5. The event goes into the state merge (`engine::merge`), and the
//!    session binding for SessionStart (`engine::generation`).
//!
//! Drops for 1-3 go to the audit log with decision `drop`; a duplicate does
//! not (a retry is not a finding). Every drop still answers `ok:true` with
//! `accepted:false`, so a hook never fails.

use super::adapter;
use super::db::audit::{self, AuditEntry};
use super::db::outbox::NewEvent;
use super::db::Tx;
use super::generation;
use super::merge::{Activity, Change, Claim, MergeTable, Source};
use super::proto::HookParams;
use rusqlite::{params, OptionalExtension};

/// Why an event was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    UnknownAgent,
    KindMismatch,
    StaleGeneration,
    Duplicate,
}

impl DropReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DropReason::UnknownAgent => "unknown_agent",
            DropReason::KindMismatch => "kind_mismatch",
            DropReason::StaleGeneration => "stale_generation",
            DropReason::Duplicate => "duplicate",
        }
    }
}

/// The outcome of one hook event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Accepted,
    Dropped(DropReason),
}

/// One `hook.*` call, as the engine sees it.
pub struct HookEvent<'a> {
    /// `hook.stop`, ...
    pub method: &'a str,
    /// The principal, as stamped (`agent:<id>`).
    pub principal: &'a str,
    /// The generation stamped on the connection.
    pub gen: i64,
    /// The live table's current generation of the agent now, if it is live.
    pub live_gen: Option<i64>,
    pub params: &'a HookParams,
}

/// Take one hook event (see the module doc). `now_ms` is the engine clock;
/// a hook claim is timed by its arrival.
pub fn intake(
    tx: &mut Tx,
    machine: &str,
    merge: &MergeTable,
    ev: &HookEvent,
    now_ms: i64,
) -> rusqlite::Result<Outcome> {
    let p = ev.params;
    let id = p.agent_id.as_str();
    let row: Option<(String, i64, Option<String>)> = tx
        .query_row(
            "SELECT provider, generation, state FROM agents WHERE id = ?1 \
             AND source = 'engine' AND control = 'pty' AND gone_at IS NULL",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((provider, row_gen, state)) = row else {
        let why = format!("no live engine agent {}", id);
        return dropped(tx, ev, DropReason::UnknownAgent, &why, now_ms);
    };
    let kind_ok =
        adapter::descriptor_for_provider(&provider).is_some_and(|d| d.accepts_hook_kind(&p.kind));
    if !kind_ok {
        let why = format!("hook kind {:?} for a {} agent", p.kind, provider);
        return dropped(tx, ev, DropReason::KindMismatch, &why, now_ms);
    }
    if ev.live_gen != Some(ev.gen) || row_gen != ev.gen {
        let why = format!(
            "event of generation {}, live generation {}, row generation {}",
            ev.gen,
            ev.live_gen.map_or("none".to_string(), |g| g.to_string()),
            row_gen
        );
        return dropped(tx, ev, DropReason::StaleGeneration, &why, now_ms);
    }
    let fresh = tx.execute(
        "INSERT OR IGNORE INTO dedupe (peer, msg_id, ts) VALUES (?1, ?2, ?3)",
        params![id, p.seq, now_ms],
    )? > 0;
    if !fresh {
        return Ok(Outcome::Dropped(DropReason::Duplicate));
    }
    apply(tx, machine, merge, ev, state.as_deref(), now_ms)?;
    Ok(Outcome::Accepted)
}

/// Feed an accepted event into the session binding and the merge, and
/// write what changed.
fn apply(
    tx: &mut Tx,
    machine: &str,
    merge: &MergeTable,
    ev: &HookEvent,
    state: Option<&str>,
    now_ms: i64,
) -> rusqlite::Result<()> {
    let p = ev.params;
    let id = p.agent_id.as_str();
    let claim = |a: Activity| Some(Claim::new(Source::Hook, a, now_ms));
    let call = p.input["tool_use_id"].as_str().map(str::to_string);
    let (claim, attention) = match ev.method {
        "hook.session_start" => {
            if let Some(sid) = p.input["session_id"].as_str() {
                let path = p.input["transcript_path"].as_str();
                generation::bind_session(tx, machine, id, ev.gen, sid, path, now_ms)?;
            }
            (None, None)
        }
        // D24.6: the Stop hook is the first turn-end signal.
        "hook.stop" => (claim(Activity::Ready), None),
        // D24.5: confirms the screen's `blocked`.
        "hook.permission_request" => (claim(Activity::Blocked), None),
        // Turn start: the hook tells `working` before the screen does
        // (Orca's pattern).
        "hook.user_prompt_submit" => (claim(Activity::Working), None),
        // Work goes on after a permission prompt: only the PostToolUse of
        // the call that waited, from the main thread, and never a turn
        // start (see below). A sub-agent's calls carry `agent_id`.
        "hook.post_tool_use" => (claim(Activity::Working), None),
        // An auto-mode denial: a human should look (O1 = A).
        "hook.permission_denied" => {
            let reason = p.input["reason"]
                .as_str()
                .unwrap_or("auto mode classifier denied");
            (None, Some(format!("auto_mode_denied: {}", reason)))
        }
        // Information only; the screen and transcript tell the state.
        _ => (None, None),
    };
    let from_subagent = p.input["agent_id"].as_str().is_some_and(|a| !a.is_empty());
    let (change, row_state, attention) = merge.with(id, ev.gen, |m| {
        let claim = match ev.method {
            "hook.post_tool_use" => claim.filter(|_| {
                !from_subagent
                    && m.turn_open()
                    && call.as_deref().is_some_and(|c| m.take_blocked_call(c))
            }),
            "hook.permission_request" => {
                m.set_blocked_call(call.clone());
                claim
            }
            "hook.user_prompt_submit" | "hook.stop" => {
                m.set_blocked_call(None);
                claim
            }
            _ => claim,
        };
        let change = claim.map(|c| m.claim(c)).unwrap_or_default();
        if let Some(a) = &attention {
            m.attention(a, now_ms);
        }
        let row_state = m.row_state(now_ms);
        m.written(row_state);
        (change, row_state, attention)
    });
    write_change(tx, machine, id, ev.gen, state, &change, row_state)?;
    if let Some(reason) = attention {
        tx.emit(&NewEvent {
            topic: "agent.attention".into(),
            subject: Some(format!("{}/{}", machine, id)),
            origin: None,
            data: serde_json::json!({
                "id": id, "generation": ev.gen, "reason": reason,
                "toolUseId": p.input["tool_use_id"],
            }),
        })?;
    }
    Ok(())
}

/// What a merge claim changed, into the row: the new `state` and the turn
/// end event.
fn write_change(
    tx: &mut Tx,
    machine: &str,
    id: &str,
    gen: i64,
    state: Option<&str>,
    change: &Change,
    row_state: Option<&'static str>,
) -> rusqlite::Result<()> {
    // Lifecycle states (starting, paused, retiring, ...) belong to the
    // spawn, drain and retire paths; the merge moves only these.
    let mergeable = matches!(state, Some("ready" | "working" | "blocked"));
    if let Some(new) = row_state.filter(|s| mergeable && state != Some(*s)) {
        tx.execute(
            "UPDATE agents SET state = ?2 WHERE id = ?1",
            params![id, new],
        )?;
        generation::emit_upsert(tx, machine, id)?;
    }
    if let Some(end) = change.turn_end {
        tx.emit(&NewEvent {
            topic: "agent.turn_end".into(),
            subject: Some(format!("{}/{}", machine, id)),
            origin: None,
            data: serde_json::json!({
                "id": id, "generation": gen, "by": end.by.as_str(), "at": end.at_ms,
            }),
        })?;
    }
    Ok(())
}

/// A claim of the agent's own screen, from its supervisor after `ready`:
/// into the merge, then the row state and turn end that follow. The
/// `UserPromptSubmit` and `PostToolUse` hooks claim `working` first; the
/// screen is the fallback when a hook does not arrive, and the drain waits
/// for `working` either way (reviews H1). Ignored unless `gen` is the
/// row's generation. Returns whether it was taken.
pub fn screen_claim(
    tx: &mut Tx,
    machine: &str,
    merge: &MergeTable,
    id: &str,
    gen: i64,
    activity: Activity,
    now_ms: i64,
) -> rusqlite::Result<bool> {
    let row: Option<(i64, Option<String>)> = tx
        .query_row(
            "SELECT generation, state FROM agents WHERE id = ?1 \
             AND source = 'engine' AND control = 'pty' AND gone_at IS NULL",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((row_gen, state)) = row.filter(|(g, _)| *g == gen) else {
        return Ok(false);
    };
    let (change, row_state) = merge.with(id, gen, |m| {
        let change = m.claim(Claim::new(Source::Screen, activity, now_ms));
        let row_state = m.row_state(now_ms);
        m.written(row_state);
        (change, row_state)
    });
    write_change(
        tx,
        machine,
        id,
        row_gen,
        state.as_deref(),
        &change,
        row_state,
    )?;
    Ok(true)
}

/// Write the merge's row state when a hold ran out with no new claim
/// (`AgentMerge::stale_row_state`): a hook `working` held a later screen
/// `ready` for `SETTLE_MS`, and nothing else arrives after an Esc
/// interrupt. No claim, so no turn starts or ends here. Ignored unless
/// `gen` is the row's generation. Returns whether it wrote.
pub fn expire_hold(
    tx: &mut Tx,
    machine: &str,
    merge: &MergeTable,
    id: &str,
    gen: i64,
    now_ms: i64,
) -> rusqlite::Result<bool> {
    let row: Option<Option<String>> = tx
        .query_row(
            "SELECT state FROM agents WHERE id = ?1 AND generation = ?2 \
             AND source = 'engine' AND control = 'pty' AND gone_at IS NULL",
            params![id, gen],
            |r| r.get(0),
        )
        .optional()?;
    let row_state = merge
        .peek(id, gen, |m| {
            let s = m.row_state(now_ms);
            m.written(s);
            s
        })
        .flatten();
    // No live row of this generation: noted as written above, so the
    // supervisor does not ask again every tick.
    let Some(state) = row else {
        return Ok(false);
    };
    write_change(
        tx,
        machine,
        id,
        gen,
        state.as_deref(),
        &Change::default(),
        row_state,
    )?;
    Ok(true)
}

/// Audit a dropped event (not a duplicate) and say so.
fn dropped(
    tx: &mut Tx,
    ev: &HookEvent,
    why: DropReason,
    detail: &str,
    now_ms: i64,
) -> rusqlite::Result<Outcome> {
    let reason = format!("{}: {}", why.as_str(), detail);
    audit::append(
        tx,
        now_ms,
        &AuditEntry {
            principal: ev.principal,
            method: ev.method,
            target: Some(&ev.params.agent_id),
            decision: "drop",
            reason: Some(&reason),
            ..Default::default()
        },
    )?;
    Ok(Outcome::Dropped(why))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::db::{Db, DEDUPE_WINDOW_MS};
    use crate::engine::generation::tests::{db_with_agent, MACHINE};
    use crate::engine::generation::{begin_generation, GenReason};
    use serde_json::{json, Value};
    use std::sync::Arc;

    fn params(kind: &str, seq: &str, input: Value) -> HookParams {
        HookParams {
            agent_id: "a1".into(),
            kind: kind.into(),
            seq: seq.into(),
            input,
            truncated: false,
        }
    }

    /// A store with agent a1 at generation 1, state `ready`.
    fn setup() -> (tempfile::TempDir, Db, Arc<MergeTable>) {
        let (t, db) = db_with_agent("a1");
        db.write_blocking(|tx| {
            begin_generation(tx, MACHINE, "a1", GenReason::Spawn, "s1", None, 1)?;
            tx.execute("UPDATE agents SET state = 'ready' WHERE id = 'a1'", [])
        })
        .unwrap();
        (t, db, Arc::new(MergeTable::new()))
    }

    fn send(
        db: &Db,
        merge: &Arc<MergeTable>,
        method: &'static str,
        gen: i64,
        live_gen: Option<i64>,
        p: HookParams,
        now: i64,
    ) -> Outcome {
        let merge = merge.clone();
        db.write_blocking(move |tx| {
            let ev = HookEvent {
                method,
                principal: "agent:a1",
                gen,
                live_gen,
                params: &p,
            };
            intake(tx, MACHINE, &merge, &ev, now)
        })
        .unwrap()
        .0
    }

    fn audit(db: &Db) -> Vec<(String, String, String)> {
        let conn = db.reader().unwrap();
        let mut st = conn
            .prepare("SELECT method, decision, reason FROM audit ORDER BY seq")
            .unwrap();
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        rows.map(Result::unwrap)
            .filter(|(m, _, _): &(String, String, String)| m.starts_with("hook."))
            .collect()
    }

    fn state(db: &Db) -> String {
        let conn = db.reader().unwrap();
        conn.query_row("SELECT state FROM agents", [], |r| r.get(0))
            .unwrap()
    }

    fn topics(db: &Db) -> Vec<String> {
        let conn = db.reader().unwrap();
        let mut st = conn
            .prepare("SELECT topic FROM events ORDER BY seq")
            .unwrap();
        let rows = st.query_map([], |r| r.get(0)).unwrap();
        rows.map(Result::unwrap).collect()
    }

    #[test]
    fn stale_generation_events_are_dropped_and_audited() {
        let (_t, db, merge) = setup();
        // The connection was stamped with generation 1; the agent was
        // resumed since, so the live table and the row are at 2.
        db.write_blocking(|tx| {
            begin_generation(tx, MACHINE, "a1", GenReason::Resume, "s1", None, 2)
        })
        .unwrap();
        let p = params("claude", "1", json!({}));
        let out = send(&db, &merge, "hook.stop", 1, Some(2), p, 10);
        assert_eq!(out, Outcome::Dropped(DropReason::StaleGeneration));
        // The agent is gone from the live table: also stale.
        let p = params("claude", "2", json!({}));
        let out = send(&db, &merge, "hook.stop", 2, None, p, 10);
        assert_eq!(out, Outcome::Dropped(DropReason::StaleGeneration));
        let rows = audit(&db);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "hook.stop");
        assert_eq!(rows[0].1, "drop");
        assert_eq!(
            rows[0].2,
            "stale_generation: event of generation 1, live generation 2, row generation 2"
        );
        assert!(rows[1].2.contains("live generation none"));
        // A drop does not take the seq: the current generation's event
        // with seq 1 still goes in.
        let p = params("claude", "1", json!({}));
        assert_eq!(
            send(&db, &merge, "hook.stop", 2, Some(2), p, 10),
            Outcome::Accepted
        );
    }

    #[test]
    fn kind_mismatch_is_dropped_before_dedupe() {
        let (_t, db, merge) = setup();
        // A hook of another kind under this agent's id (A:1056).
        let p = params("grok", "7", json!({}));
        assert_eq!(
            send(&db, &merge, "hook.permission_request", 1, Some(1), p, 10),
            Outcome::Dropped(DropReason::KindMismatch)
        );
        let rows = audit(&db);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].2,
            "kind_mismatch: hook kind \"grok\" for a claudeCode agent"
        );
        assert_eq!(state(&db), "ready");
        // The mismatch did not use seq 7.
        let p = params("claude", "7", json!({}));
        assert_eq!(
            send(&db, &merge, "hook.permission_request", 1, Some(1), p, 10),
            Outcome::Accepted
        );
        assert_eq!(state(&db), "blocked");
        // The same seq again is a duplicate, and not audited.
        let p = params("claude", "7", json!({}));
        assert_eq!(
            send(&db, &merge, "hook.stop", 1, Some(1), p, 20),
            Outcome::Dropped(DropReason::Duplicate)
        );
        assert_eq!(audit(&db).len(), 1);
        assert_eq!(state(&db), "blocked");
        // Past the dedupe window the row is pruned.
        let pruned = db
            .write_blocking(|tx| crate::engine::db::prune_dedupe(tx, 10 + DEDUPE_WINDOW_MS))
            .unwrap()
            .0;
        assert_eq!(pruned, 0);
        let pruned = db
            .write_blocking(|tx| crate::engine::db::prune_dedupe(tx, 11 + DEDUPE_WINDOW_MS))
            .unwrap()
            .0;
        assert_eq!(pruned, 1);
    }

    #[test]
    fn an_unknown_or_observed_agent_is_dropped() {
        let (_t, db, merge) = setup();
        db.write_blocking(|tx| {
            tx.execute(
                "UPDATE agents SET source = 'detected', control = 'none' WHERE id = 'a1'",
                [],
            )
        })
        .unwrap();
        let p = params("claude", "1", json!({}));
        assert_eq!(
            send(&db, &merge, "hook.stop", 1, Some(1), p, 10),
            Outcome::Dropped(DropReason::UnknownAgent)
        );
        assert_eq!(audit(&db)[0].1, "drop");
    }

    #[test]
    fn accepted_events_drive_the_state_and_the_session_binding() {
        let (_t, db, merge) = setup();
        // SessionStart binds the transcript.
        let p = params(
            "claude",
            "1",
            json!({"session_id": "s1", "transcript_path": "/t/s1.jsonl", "source": "startup"}),
        );
        assert_eq!(
            send(&db, &merge, "hook.session_start", 1, Some(1), p, 10),
            Outcome::Accepted
        );
        let path = db
            .write_blocking(|tx| generation::transcript_path(tx, "a1", 1))
            .unwrap()
            .0;
        assert_eq!(path.as_deref(), Some("/t/s1.jsonl"));
        // The screen saw a turn start; the Stop hook ends it.
        merge.with("a1", 1, |m| {
            m.claim(Claim::new(Source::Screen, Activity::Working, 100))
        });
        let p = params("claude", "2", json!({}));
        assert_eq!(
            send(&db, &merge, "hook.stop", 1, Some(1), p, 1_000),
            Outcome::Accepted
        );
        assert_eq!(state(&db), "ready");
        assert!(topics(&db).contains(&"agent.turn_end".to_string()));
        // A denial raises attention and does not move the state.
        let p = params(
            "claude",
            "3",
            json!({"tool_use_id": "toolu_1", "reason": "[Code from External]"}),
        );
        send(&db, &merge, "hook.permission_denied", 1, Some(1), p, 1_100);
        assert_eq!(state(&db), "ready");
        let conn = db.reader().unwrap();
        let data: String = conn
            .query_row(
                "SELECT payload FROM events WHERE topic = 'agent.attention'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let data: Value = serde_json::from_str(&data).unwrap();
        assert_eq!(data["reason"], "auto_mode_denied: [Code from External]");
        assert_eq!(data["toolUseId"], "toolu_1");
        // A lifecycle state is left alone.
        db.write_blocking(|tx| tx.execute("UPDATE agents SET state = 'paused'", []))
            .unwrap();
        let p = params("claude", "4", json!({}));
        send(&db, &merge, "hook.permission_request", 1, Some(1), p, 9_000);
        assert_eq!(state(&db), "paused");
    }

    #[test]
    fn prompt_submit_and_post_tool_use_claim_working() {
        let (_t, db, merge) = setup();
        // UserPromptSubmit starts the turn before the screen shows it.
        let p = params("claude", "1", json!({"prompt": "hi"}));
        assert_eq!(
            send(&db, &merge, "hook.user_prompt_submit", 1, Some(1), p, 1_000),
            Outcome::Accepted
        );
        assert_eq!(state(&db), "working");
        // A permission prompt, then PostToolUse after the approval.
        let p = params("claude", "2", json!({"tool_use_id": "toolu_1"}));
        send(&db, &merge, "hook.permission_request", 1, Some(1), p, 2_000);
        assert_eq!(state(&db), "blocked");
        // A sub-agent's call, or another call, does not end the wait.
        let p = params(
            "claude",
            "3",
            json!({"tool_use_id": "toolu_1", "agent_id": "sub1"}),
        );
        send(&db, &merge, "hook.post_tool_use", 1, Some(1), p, 3_000);
        assert_eq!(state(&db), "blocked");
        let p = params("claude", "4", json!({"tool_use_id": "toolu_2"}));
        send(&db, &merge, "hook.post_tool_use", 1, Some(1), p, 4_000);
        assert_eq!(state(&db), "blocked");
        let p = params(
            "claude",
            "5",
            json!({"tool_use_id": "toolu_1", "tool_name": "Bash"}),
        );
        send(&db, &merge, "hook.post_tool_use", 1, Some(1), p, 9_000);
        assert_eq!(state(&db), "working");
        // Stop ends the one turn.
        let p = params("claude", "6", json!({}));
        send(&db, &merge, "hook.stop", 1, Some(1), p, 10_000);
        assert_eq!(state(&db), "ready");
        // A late PostToolUse does not open a new turn.
        let p = params("claude", "7", json!({"tool_use_id": "toolu_1"}));
        send(&db, &merge, "hook.post_tool_use", 1, Some(1), p, 11_000);
        assert_eq!(state(&db), "ready");
        let ends = topics(&db)
            .iter()
            .filter(|t| *t == "agent.turn_end")
            .count();
        assert_eq!(ends, 1);
    }

    #[test]
    fn an_expired_hold_is_written_without_a_new_claim() {
        let (_t, db, merge) = setup();
        let p = params("claude", "1", json!({}));
        send(&db, &merge, "hook.user_prompt_submit", 1, Some(1), p, 1_000);
        assert_eq!(state(&db), "working");
        // Esc: the screen goes idle within the hook's hold, and no Stop
        // hook comes. The hold keeps the row `working` for now.
        let m2 = merge.clone();
        let held = db
            .write_blocking(move |tx| {
                screen_claim(tx, MACHINE, &m2, "a1", 1, Activity::Ready, 1_500)
            })
            .unwrap()
            .0;
        assert!(held);
        assert_eq!(state(&db), "working");
        assert_eq!(merge.with("a1", 1, |m| m.stale_row_state(2_000)), None);
        // The hold runs out: the merge says `ready`, and expire_hold
        // writes it, with no new claim.
        let after = 1_000 + crate::engine::merge::SETTLE_MS;
        assert_eq!(
            merge.with("a1", 1, |m| m.stale_row_state(after)),
            Some("ready")
        );
        let m2 = merge.clone();
        let wrote = db
            .write_blocking(move |tx| expire_hold(tx, MACHINE, &m2, "a1", 1, after))
            .unwrap()
            .0;
        assert!(wrote);
        assert_eq!(state(&db), "ready");
        assert_eq!(merge.with("a1", 1, |m| m.stale_row_state(after + 1)), None);
        // Another generation is ignored.
        let m2 = merge.clone();
        let other = db
            .write_blocking(move |tx| expire_hold(tx, MACHINE, &m2, "a1", 2, after))
            .unwrap()
            .0;
        assert!(!other);
    }

    #[test]
    fn expire_hold_on_a_gone_row_is_asked_once() {
        let (_t, db, merge) = setup();
        let p = params("claude", "1", json!({}));
        send(&db, &merge, "hook.user_prompt_submit", 1, Some(1), p, 1_000);
        merge.with("a1", 1, |m| {
            m.claim(Claim::new(Source::Screen, Activity::Ready, 1_500))
        });
        db.write_blocking(|tx| tx.execute("UPDATE agents SET gone_at = 1", []))
            .unwrap();
        let after = 1_000 + crate::engine::merge::SETTLE_MS;
        assert_eq!(
            merge.with("a1", 1, |m| m.stale_row_state(after)),
            Some("ready")
        );
        let m2 = merge.clone();
        let wrote = db
            .write_blocking(move |tx| expire_hold(tx, MACHINE, &m2, "a1", 1, after))
            .unwrap()
            .0;
        assert!(!wrote);
        // Noted as written: the supervisor does not ask on the next tick.
        assert_eq!(merge.with("a1", 1, |m| m.stale_row_state(after + 1)), None);
    }
}
