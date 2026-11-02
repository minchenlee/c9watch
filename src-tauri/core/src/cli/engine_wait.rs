//! `c9watch wait --event turn_ended --agent X` (C1a-37, A:957).
//!
//! Waits for the next `agent.turn_end` event of one engine agent. A turn
//! end means only that the agent is idle (D22.3); it says nothing about
//! whether its work is done.
//!
//! The command reads `snapshot.get` to find the agent, then follows
//! `events.subscribe` from the snapshot's cursor, so a turn that ends
//! between the two calls is not lost. A turn that ended before the snapshot
//! does not count: a caller that sent a prompt and then waits passes the
//! cursor it had before the send (`--since`, for example the `cursor` of
//! the last line `wait` printed), or the wait goes on until the next turn
//! ends. When the stream ends (an `engine restart`), it connects again and
//! subscribes from the last cursor it saw. `--timeout` covers the whole
//! command, the engine auto-start included.
//!
//! A caller inside an engine agent (`C9_AGENT_ID` set) connects only to
//! `C9_ENGINE_SOCK`. It never auto-starts the engine, and an engine that
//! does not answer there is an error. Other callers auto-start the engine.

use super::engine_client::{current_exe, ensure_engine, error_line, EngineConn, ENV_AGENT_ID};
use super::engine_sessions::{
    DETECTED_READ_TIMEOUT, ENGINE_UNREACHABLE, RECONNECT_EVERY, RESTART_GRACE,
};
use super::print_line;
use super::task::ENV_SOCK;
use super::watch;
use crate::engine::home::{engine_home, sock_path};
use crate::engine::proto::{AgentRow, EventFrame, SnapshotResult, SubscribeParams};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The engine event `wait --event` waits for.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitEvent {
    /// The agent's turn ended: it is idle
    #[value(name = "turn_ended")]
    TurnEnded,
}

/// The event name in the line `wait` prints.
pub const TURN_ENDED: &str = "turn_ended";
/// The event name when the agent stopped before its turn ended.
pub const AGENT_ENDED: &str = "agent_ended";

/// How long the command tries to reach the engine again after the event
/// stream ended: an `engine restart` drains for up to 90 s.
const RECONNECT_FOR: Duration =
    Duration::from_secs(crate::engine::drain::HARD_DEADLINE.as_secs() + 30);

/// Engine lifecycle states after which no turn of the agent ends.
const FINAL_STATES: &[&str] = &["retiring", "retired", "failed", "orphaned"];

/// Where the command connects.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// Inside an engine agent: this socket only, with no auto-start.
    AgentSock(PathBuf),
    /// Auto-start the engine of this user, then connect.
    AutoStart,
}

/// Pick the target from the environment: `C9_AGENT_ID` set to anything but
/// "" means an agent caller, which needs `C9_ENGINE_SOCK`.
pub fn target(env: &dyn Fn(&str) -> Option<OsString>) -> Result<Target, String> {
    let set = |k: &str| env(k).filter(|v| !v.is_empty());
    if set(ENV_AGENT_ID).is_none() {
        return Ok(Target::AutoStart);
    }
    match set(ENV_SOCK) {
        Some(sock) => Ok(Target::AgentSock(PathBuf::from(sock))),
        None => Err(format!(
            "{}: {} is set but {} is not; inside an agent, c9watch connects \
             only through {} and never starts the engine",
            ENGINE_UNREACHABLE, ENV_AGENT_ID, ENV_SOCK, ENV_SOCK
        )),
    }
}

/// Connect to `target`; with `auto_start`, an `AutoStart` target starts
/// the engine first if none runs.
pub(crate) fn connect(target: &Target, auto_start: bool) -> Result<EngineConn, String> {
    let unreachable = |e: String| format!("{}: {}", ENGINE_UNREACHABLE, e);
    let sock = match target {
        Target::AgentSock(sock) => sock.clone(),
        Target::AutoStart => {
            let home = engine_home().map_err(unreachable)?;
            if auto_start {
                ensure_engine(&home, &current_exe()?).map_err(unreachable)?;
            }
            sock_path(&home)
        }
    };
    let mut conn = EngineConn::connect(&sock).map_err(|e| unreachable(e.into_msg()))?;
    conn.set_read_timeout(DETECTED_READ_TIMEOUT);
    Ok(conn)
}

fn call(
    conn: &mut EngineConn,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let resp = conn.call(method, params)?;
    if resp.ok {
        Ok(resp.result.unwrap_or_default())
    } else {
        Err(error_line(&resp))
    }
}

fn snapshot(conn: &mut EngineConn) -> Result<SnapshotResult, String> {
    serde_json::from_value(call(conn, "snapshot.get", serde_json::json!({}))?)
        .map_err(|e| format!("Bad snapshot.get result from the engine: {}", e))
}

/// Whether no turn of `row` can end any more.
fn finished(row: &AgentRow) -> bool {
    row.gone_at.is_some()
        || row
            .state
            .as_deref()
            .is_some_and(|s| FINAL_STATES.contains(&s))
}

/// Whether the engine runs `row` in a PTY: only such agents have turn ends.
fn runs_in_pty(row: &AgentRow) -> bool {
    row.source == "engine" && row.control == "pty"
}

/// The live agent that `reference` names: its id, else the name of an
/// agent the engine runs. A detected session often has the same name (its
/// project directory), so a name names one of those only when no engine
/// agent has it.
pub fn resolve<'a>(rows: &'a [AgentRow], reference: &str) -> Result<&'a AgentRow, String> {
    let live = || rows.iter().filter(|r| !finished(r));
    let named = |r: &&AgentRow| r.name.as_deref() == Some(reference);
    let row = live()
        .find(|r| r.id == reference)
        .or_else(|| live().filter(|r| runs_in_pty(r)).find(named))
        .or_else(|| live().find(named))
        .ok_or_else(|| format!("No live engine agent matches '{}'", reference))?;
    if !runs_in_pty(row) {
        return Err(format!(
            "'{}' is not an agent the engine runs, so it has no turn ends; \
             use wait <session> --until done",
            reference
        ));
    }
    Ok(row)
}

/// What one event means for the wait on agent `id`.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The agent's turn ended (the `agent.turn_end` frame).
    TurnEnded(EventFrame),
    /// The agent stopped first: `why` is its final state, `gone` or
    /// `failed`; `cursor` is where the engine said so.
    Ended { why: String, cursor: String },
}

fn names(frame: &EventFrame, id: &str) -> bool {
    frame.data.get("id").and_then(|v| v.as_str()) == Some(id)
}

/// Judge one event frame. Frames about other agents are `None`.
pub fn judge(id: &str, frame: EventFrame) -> Option<Outcome> {
    let ended = |why: &str, frame: EventFrame| {
        Some(Outcome::Ended {
            why: why.to_string(),
            cursor: frame.cursor,
        })
    };
    match frame.topic.as_str() {
        "agent.turn_end" if names(&frame, id) => Some(Outcome::TurnEnded(frame)),
        "agent.gone" if names(&frame, id) => ended("gone", frame),
        "agent.failed" if names(&frame, id) => ended("failed", frame),
        "agent.upsert" => {
            let row = serde_json::from_value::<AgentRow>(frame.data.clone()).ok()?;
            if row.id != id || !finished(&row) {
                return None;
            }
            let why = row.state.unwrap_or_else(|| "gone".into());
            ended(&why, frame)
        }
        _ => None,
    }
}

/// The line printed for `outcome`.
pub fn outcome_line(agent: &AgentRow, outcome: &Outcome, timestamp: &str) -> serde_json::Value {
    let mut line = serde_json::json!({
        "schema": watch::EVENT_SCHEMA,
        "agentId": agent.id,
        "name": agent.name,
        "timestamp": timestamp,
    });
    let obj = line.as_object_mut().expect("json object");
    match outcome {
        Outcome::TurnEnded(frame) => {
            obj.insert("event".into(), TURN_ENDED.into());
            for key in ["generation", "by", "at"] {
                obj.insert(key.into(), frame.data.get(key).cloned().unwrap_or_default());
            }
            obj.insert("cursor".into(), frame.cursor.clone().into());
        }
        Outcome::Ended { why, cursor } => {
            obj.insert("event".into(), AGENT_ENDED.into());
            obj.insert("reason".into(), why.clone().into());
            obj.insert("cursor".into(), cursor.clone().into());
        }
    }
    line
}

/// What the waiting thread sends.
enum Feed {
    Done(Box<AgentRow>, Outcome),
    /// No live engine agent matches: the message.
    NotFound(String),
    Failed(String),
}

/// Whether `state` is a turn in progress.
fn mid_turn(state: Option<&str>) -> bool {
    matches!(state, Some("working" | "blocked"))
}

/// A turn end read off a snapshot, for a turn whose `agent.turn_end` event
/// the engine no longer keeps (`by` is `snapshot`, `at` is null).
fn snapshot_turn_end(row: &AgentRow, subject: &str, cursor: &str) -> EventFrame {
    EventFrame {
        v: 1,
        kind: crate::engine::proto::EventType::Event,
        sub: None,
        cursor: cursor.to_string(),
        topic: "agent.turn_end".into(),
        subject: Some(subject.to_string()),
        ts: chrono::Utc::now().timestamp_millis(),
        origin: None,
        data: serde_json::json!({
            "id": row.id, "generation": row.generation, "by": "snapshot", "at": null,
        }),
    }
}

fn subscribe_params(cursor: &str, subject: &str) -> serde_json::Value {
    let params = SubscribeParams {
        since: Some(cursor.to_string()),
        topics: vec![
            "agent.turn_end".into(),
            "agent.upsert".into(),
            "agent.gone".into(),
            "agent.failed".into(),
        ],
        subjects: vec![subject.to_string()],
    };
    serde_json::to_value(params).unwrap_or_default()
}

/// Follow the agent's events from `cursor` until one decides the wait,
/// on `conn` and on new connections after it.
struct Follower {
    target: Target,
    cursor: String,
    id: String,
    subject: String,
    /// Whether the agent was mid-turn since the wait began (or the caller
    /// gave `--since`): then an idle agent after a `cursor_expired` means
    /// the turn ended in the gap.
    turn_seen: bool,
}

/// Why one subscription stopped without an outcome.
enum Stop {
    /// The connection ended: connect again.
    Lost,
    /// The engine refused: waiting does not help.
    Refused(String),
}

impl Follower {
    /// One subscription on `conn`. A cursor the engine no longer keeps
    /// (`cursor_expired`) is replaced by a new snapshot's cursor, and the
    /// agent must still be live in that snapshot. If the agent was mid-turn
    /// and is `ready` in it, the turn ended in the events that were pruned.
    fn subscribe(&mut self, mut conn: EngineConn) -> Result<Outcome, Stop> {
        loop {
            let resp = conn
                .call(
                    "events.subscribe",
                    subscribe_params(&self.cursor, &self.subject),
                )
                .map_err(|_| Stop::Lost)?;
            if resp.ok {
                break;
            }
            let e = error_line(&resp);
            if !e.starts_with("cursor_expired") {
                return Err(Stop::Refused(e));
            }
            let snap = snapshot(&mut conn).map_err(|_| Stop::Lost)?;
            let Some(row) = snap.agents.iter().find(|r| r.id == self.id && !finished(r)) else {
                return Ok(Outcome::Ended {
                    why: "gone".into(),
                    cursor: snap.cursor,
                });
            };
            if self.turn_seen && row.state.as_deref() == Some("ready") {
                return Ok(Outcome::TurnEnded(snapshot_turn_end(
                    row,
                    &self.subject,
                    &snap.cursor,
                )));
            }
            self.turn_seen |= mid_turn(row.state.as_deref());
            self.cursor = snap.cursor;
        }
        let frames = conn.into_frames().map_err(|_| Stop::Lost)?;
        for line in frames.lines.iter() {
            // A response frame here is the subscription's error: subscribe
            // again from the last cursor.
            let Ok(frame) = serde_json::from_str::<EventFrame>(&line) else {
                break;
            };
            self.cursor = frame.cursor.clone();
            if frame.topic == "agent.upsert" && names(&frame, &self.id) {
                self.turn_seen |= mid_turn(frame.data.get("state").and_then(|v| v.as_str()));
            }
            if let Some(outcome) = judge(&self.id, frame) {
                return Ok(outcome);
            }
        }
        Err(Stop::Lost)
    }

    /// Subscribe until an outcome. After a lost connection, connect again
    /// for up to `RECONNECT_FOR` and go on from the last cursor; for the
    /// first `RESTART_GRACE` only connect, so an `engine restart` starts
    /// the new engine, not this command.
    fn run(mut self, mut conn: EngineConn) -> Result<Outcome, String> {
        loop {
            let opened_at = Instant::now();
            match self.subscribe(conn) {
                Ok(outcome) => return Ok(outcome),
                Err(Stop::Refused(e)) => return Err(e),
                Err(Stop::Lost) => {}
            }
            // A stream that ends at once is not reopened in a busy loop.
            if opened_at.elapsed() < RECONNECT_EVERY {
                std::thread::sleep(RECONNECT_EVERY);
            }
            let lost_at = Instant::now();
            conn = loop {
                match connect(&self.target, lost_at.elapsed() >= RESTART_GRACE) {
                    Ok(conn) => break conn,
                    Err(e) if lost_at.elapsed() >= RECONNECT_FOR => return Err(e),
                    Err(_) => std::thread::sleep(RECONNECT_EVERY),
                }
            };
        }
    }
}

/// Connect, find the agent, and follow it until an outcome.
fn wait_turn(target: Target, reference: &str, since: Option<&str>) -> Feed {
    let mut conn = match connect(&target, true) {
        Ok(conn) => conn,
        Err(e) => return Feed::Failed(e),
    };
    let snap = match snapshot(&mut conn) {
        Ok(snap) => snap,
        Err(e) => return Feed::Failed(e),
    };
    let agent = match resolve(&snap.agents, reference) {
        Ok(agent) => agent.clone(),
        Err(e) => return Feed::NotFound(e),
    };
    let follower = Follower {
        target,
        cursor: since.map_or(snap.cursor, str::to_string),
        subject: format!("{}/{}", snap.machine_id, agent.id),
        id: agent.id.clone(),
        turn_seen: since.is_some() || mid_turn(agent.state.as_deref()),
    };
    match follower.run(conn) {
        Ok(outcome) => Feed::Done(Box::new(agent), outcome),
        Err(e) => Feed::Failed(e),
    }
}

/// `c9watch wait --event turn_ended --agent X [--since <cursor>]`. Returns
/// the exit code: 0 the turn ended, 2 no live engine agent matches, 3 the
/// agent stopped first, 124 timeout. The timeout counts from the start, so
/// it covers connecting and the engine auto-start too.
pub fn cmd_wait_turn(
    reference: &str,
    since: Option<&str>,
    timeout_secs: Option<u64>,
) -> Result<i32, String> {
    let deadline = timeout_secs.map(|t| Instant::now() + Duration::from_secs(t));
    let target = target(&|k| std::env::var_os(k))?;
    let (tx, rx) = mpsc::channel();
    let (reference, since) = (reference.to_string(), since.map(str::to_string));
    std::thread::spawn(move || {
        let _ = tx.send(wait_turn(target, &reference, since.as_deref()));
    });
    let stopped = || "The engine feed stopped".to_string();
    let feed = match deadline {
        Some(deadline) => {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(feed) => feed,
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(watch::EXIT_TIMEOUT),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(stopped()),
            }
        }
        None => rx.recv().map_err(|_| stopped())?,
    };
    let (agent, outcome) = match feed {
        Feed::Done(agent, outcome) => (agent, outcome),
        Feed::NotFound(e) => {
            eprintln!("{}", serde_json::json!({ "error": e }));
            return Ok(watch::EXIT_NOT_FOUND);
        }
        Feed::Failed(e) => return Err(e),
    };
    print_line(&outcome_line(
        &agent,
        &outcome,
        &chrono::Utc::now().to_rfc3339(),
    ));
    Ok(match outcome {
        Outcome::TurnEnded(_) => 0,
        Outcome::Ended { .. } => watch::EXIT_ENDED_FIRST,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_proto::client::tests::row;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let m: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |k| m.get(k).cloned()
    }

    fn engine_agent(id: &str, name: &str) -> AgentRow {
        let mut r = row("claudeCode:s", "working");
        r.id = id.into();
        r.name = Some(name.into());
        r.source = "engine".into();
        r.control = "pty".into();
        r.state = Some("working".into());
        r
    }

    fn frame(topic: &str, data: serde_json::Value) -> EventFrame {
        EventFrame {
            v: 1,
            kind: crate::engine::proto::EventType::Event,
            sub: Some("2".into()),
            cursor: "m:e:7".into(),
            topic: topic.into(),
            subject: Some("m/agt_1".into()),
            ts: 0,
            origin: None,
            data,
        }
    }

    #[test]
    fn an_agent_caller_uses_only_its_engine_sock() {
        assert_eq!(target(&env_of(&[])).unwrap(), Target::AutoStart);
        assert_eq!(
            target(&env_of(&[("C9_AGENT_ID", ""), ("C9_ENGINE_SOCK", "/s")])).unwrap(),
            Target::AutoStart
        );
        assert_eq!(
            target(&env_of(&[
                ("C9_AGENT_ID", "agt_1"),
                ("C9_ENGINE_SOCK", "/s")
            ]))
            .unwrap(),
            Target::AgentSock("/s".into())
        );
        for env in [
            env_of(&[("C9_AGENT_ID", "agt_1")]),
            env_of(&[("C9_AGENT_ID", "agt_1"), ("C9_ENGINE_SOCK", "")]),
        ] {
            let e = target(&env).unwrap_err();
            assert!(e.starts_with(ENGINE_UNREACHABLE), "{e}");
            assert!(e.contains("C9_ENGINE_SOCK"), "{e}");
        }
    }

    #[test]
    fn the_agent_is_found_by_id_or_name_among_live_engine_agents() {
        let mut gone = engine_agent("agt_0", "w");
        gone.gone_at = Some(1);
        let live = engine_agent("agt_1", "w");
        let mut observed = row("claudeCode:o", "working");
        observed.name = Some("obs".into());
        let rows = vec![gone, live, observed];
        assert_eq!(resolve(&rows, "agt_1").unwrap().id, "agt_1");
        assert_eq!(resolve(&rows, "w").unwrap().id, "agt_1");
        assert!(resolve(&rows, "agt_0").is_err());
        assert!(resolve(&rows, "nope").unwrap_err().contains("No live"));
        assert!(resolve(&rows, "obs").unwrap_err().contains("--until done"));
        // A detected session with the same name, listed first, does not
        // hide the engine agent.
        let mut same = row("claudeCode:p", "working");
        same.name = Some("w".into());
        let rows = vec![same, engine_agent("agt_3", "w")];
        assert_eq!(resolve(&rows, "w").unwrap().id, "agt_3");
        let mut retiring = engine_agent("agt_2", "r");
        retiring.state = Some("retiring".into());
        assert!(resolve(&[retiring], "agt_2").is_err());
    }

    #[test]
    fn only_this_agents_turn_end_or_end_decides() {
        let turn = |id: &str| {
            frame(
                "agent.turn_end",
                serde_json::json!({"id": id, "generation": 1, "by": "hook", "at": 5}),
            )
        };
        assert!(judge("agt_1", turn("agt_2")).is_none());
        assert!(matches!(
            judge("agt_1", turn("agt_1")),
            Some(Outcome::TurnEnded(_))
        ));

        // A row that is still running does not decide; a final one does.
        let mut r = engine_agent("agt_1", "w");
        let up = |r: &AgentRow| frame("agent.upsert", serde_json::to_value(r).unwrap());
        assert!(judge("agt_1", up(&r)).is_none());
        r.state = Some("paused".into());
        assert!(judge("agt_1", up(&r)).is_none());
        r.state = Some("retired".into());
        let Some(Outcome::Ended { why, .. }) = judge("agt_1", up(&r)) else {
            panic!("a retired row ends the wait");
        };
        assert_eq!(why, "retired");
        assert!(judge("agt_9", up(&r)).is_none());

        for (topic, why) in [("agent.gone", "gone"), ("agent.failed", "failed")] {
            let f = frame(topic, serde_json::json!({"id": "agt_1"}));
            assert!(matches!(judge("agt_1", f), Some(Outcome::Ended { why: w, .. }) if w == why));
            assert!(judge("agt_2", frame(topic, serde_json::json!({"id": "agt_1"}))).is_none());
        }
    }

    #[test]
    fn the_line_names_the_turn_or_why_the_agent_ended() {
        let agent = engine_agent("agt_1", "w");
        let f = frame(
            "agent.turn_end",
            serde_json::json!({"id": "agt_1", "generation": 2, "by": "screen", "at": 9}),
        );
        let line = outcome_line(&agent, &Outcome::TurnEnded(f.clone()), "t");
        assert_eq!(
            line,
            serde_json::json!({
                "schema": watch::EVENT_SCHEMA, "event": "turn_ended",
                "agentId": "agt_1", "name": "w", "generation": 2,
                "by": "screen", "at": 9, "cursor": "m:e:7", "timestamp": "t",
            })
        );
        let ended = Outcome::Ended {
            why: "failed".into(),
            cursor: f.cursor,
        };
        let line = outcome_line(&agent, &ended, "t");
        assert_eq!(line["event"], "agent_ended");
        assert_eq!(line["reason"], "failed");
    }

    #[test]
    fn the_event_value_is_spelled_turn_ended() {
        use clap::ValueEnum;
        let v = WaitEvent::TurnEnded.to_possible_value().unwrap();
        assert_eq!(v.get_name(), TURN_ENDED);
    }
}
