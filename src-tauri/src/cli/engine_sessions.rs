//! `list`, `status`, `self`, `watch` and `wait` as engine clients (C1a-36).
//!
//! The engine detects sessions (C1a-15). These commands auto-start it, then
//! read `session.list`; `watch` and `wait` read `snapshot.get` and follow
//! `events.subscribe` from its cursor, so no change between the two is
//! lost. Each session's payload keeps the `--direct` field names that the
//! engine can fill, and carries the engine's row (c9p/1 `AgentRow`) under
//! `engine` (owner, 2026-10-30: scripts that read `id`, `sessionName` or
//! `projectPath` keep working).
//!
//! `--direct` keeps in-process detection. A caller inside an engine agent
//! (`C9_AGENT_ID` set) gets `--direct` without asking: agents may not
//! auto-start the engine, and `c9watch list` in an agent's Bash must keep
//! working (N5). Without `C9_AGENT_ID`, an engine that cannot be reached is
//! an error whose hint names `--direct`.

use super::engine_client::{
    current_exe, ensure_engine, error_line, EngineConn, Frames, ENV_AGENT_ID,
};
use super::{print_json, print_line, watch, Poll};
use crate::engine::detect::status_name;
use crate::engine::home::{engine_home, sock_path};
use crate::engine::proto::{
    AgentRow, EventFrame, SessionListResult, SnapshotResult, SubscribeParams,
};
use crate::session::SessionStatus;
use std::ffi::OsString;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Error code: no engine answers and none could be started.
pub const ENGINE_UNREACHABLE: &str = "engine_unreachable";

/// How long `watch` and `wait` try to reach the engine again after their
/// event stream ends: an `engine restart` drains for up to 90 s.
const RECONNECT_FOR: Duration =
    Duration::from_secs(crate::engine::drain::HARD_DEADLINE.as_secs() + 30);
pub(crate) const RECONNECT_EVERY: Duration = Duration::from_secs(1);
/// For this long after an event stream ends, `watch` and `wait` only
/// connect and do not start an engine: an `engine restart` starts its own,
/// with its binary and its environment (reviews M2).
pub(crate) const RESTART_GRACE: Duration = Duration::from_secs(15);
/// Read timeout of a call that may wait for detection's first pass
/// (`session.list`, `snapshot.get`): the engine waits up to
/// `detect::FIRST_TICK_WAIT` for it.
pub(crate) const DETECTED_READ_TIMEOUT: Duration =
    Duration::from_secs(crate::engine::detect::FIRST_TICK_WAIT.as_secs() + 5);

/// Whether a detection command reads sessions from the engine: not with
/// `--direct`, and not when `C9_AGENT_ID` is set to anything but "".
pub fn use_engine(direct: bool, env: &dyn Fn(&str) -> Option<OsString>) -> bool {
    !direct && env(ENV_AGENT_ID).filter(|v| !v.is_empty()).is_none()
}

fn unreachable(e: impl std::fmt::Display) -> String {
    format!(
        "{}: {}; run with --direct to detect sessions in this process",
        ENGINE_UNREACHABLE, e
    )
}

/// Auto-start the engine and connect to it.
fn connect() -> Result<EngineConn, String> {
    connect_with(true)
}

/// Connect to the engine; with `auto_start`, start one first if none runs.
fn connect_with(auto_start: bool) -> Result<EngineConn, String> {
    let home = engine_home().map_err(unreachable)?;
    if auto_start {
        ensure_engine(&home, &current_exe()?).map_err(unreachable)?;
    }
    let mut conn = EngineConn::connect(&sock_path(&home)).map_err(|e| unreachable(e.into_msg()))?;
    conn.set_read_timeout(DETECTED_READ_TIMEOUT);
    Ok(conn)
}

/// One request; an `ok:false` response is `"<code>: <msg>"`.
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

/// The engine's `session.list`.
fn session_rows() -> Result<Vec<AgentRow>, String> {
    let mut conn = connect()?;
    let result = call(&mut conn, "session.list", serde_json::json!({}))?;
    serde_json::from_value::<SessionListResult>(result)
        .map(|r| r.sessions)
        .map_err(|e| format!("Bad session.list result from the engine: {}", e))
}

// ── Rows as the CLI shows them ────────────────────────────────────────

const STATUSES: [SessionStatus; 4] = [
    SessionStatus::Working,
    SessionStatus::NeedsAttention,
    SessionStatus::WaitingForInput,
    SessionStatus::Connecting,
];

/// The detected status of a row, as `--direct` names it. A row without
/// one (or with a value this CLI does not know) is `Connecting`.
pub fn row_status(row: &AgentRow) -> SessionStatus {
    row.status
        .as_deref()
        .and_then(|s| STATUSES.into_iter().find(|x| status_name(x) == s))
        .unwrap_or(SessionStatus::Connecting)
}

fn status_label(s: &SessionStatus) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// `--status` matches the `--direct` name (`WaitingForInput`) or the
/// engine's (`waiting_for_input`), ignoring case.
fn status_matches(row: &AgentRow, filter: &str) -> bool {
    status_label(&row_status(row)).eq_ignore_ascii_case(filter)
        || row
            .status
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case(filter))
}

fn project_matches(row: &AgentRow, filter: Option<&str>) -> bool {
    filter.is_none_or(|f| row.cwd.as_deref().is_some_and(|c| c.contains(f)))
}

/// The last component of the row's working directory, as `--direct`'s
/// `sessionName` of a session without a custom name.
fn project_name(row: &AgentRow) -> String {
    row.cwd
        .as_deref()
        .and_then(|c| std::path::Path::new(c).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Fields of the `engine` object in a `--compact` row.
const COMPACT_FIELDS: &[&str] = &[
    "id",
    "sessionKey",
    "provider",
    "providerSid",
    "status",
    "state",
    "cwd",
    "pid",
];

/// The engine's row as the `engine` object: the whole row, or with
/// `compact` its `COMPACT_FIELDS` that are not null.
fn engine_json(row: &AgentRow, compact: bool) -> serde_json::Value {
    let full = serde_json::to_value(row).unwrap_or_default();
    if !compact {
        return full;
    }
    let mut out = serde_json::Map::new();
    if let serde_json::Value::Object(o) = full {
        for k in COMPACT_FIELDS {
            if let Some(v) = o.get(*k).filter(|v| !v.is_null()) {
                out.insert((*k).to_string(), v.clone());
            }
        }
    }
    serde_json::Value::Object(out)
}

/// A session payload in the `--direct` shape: `id` is the provider's
/// session id, not the engine's `agt_` id, and `status` uses the
/// `--direct` name. Fields the engine does not know (`canStop`,
/// `firstPrompt`, `modified`, ...) are left out; the engine's row is under
/// `engine`.
pub fn row_json(row: &AgentRow, compact: bool) -> serde_json::Value {
    let (key_provider, key_sid) = match row.session_key.as_deref() {
        Some(k) => match k.split_once(':') {
            Some((p, s)) => (Some(p), Some(s)),
            None => (None, None),
        },
        None => (None, None),
    };
    let name = row.name.clone().unwrap_or_else(|| project_name(row));
    serde_json::json!({
        "id": row.provider_sid.as_deref().or(key_sid),
        "sessionKey": row.session_key,
        "sessionName": name,
        "projectPath": row.cwd,
        "status": status_label(&row_status(row)),
        "pid": row.pid,
        "provider": key_provider.unwrap_or(&row.provider),
        "engine": engine_json(row, compact),
    })
}

/// What `list` prints for `rows`.
pub fn list_output(
    rows: &[AgentRow],
    project: Option<&str>,
    status: Option<&str>,
    compact: bool,
) -> serde_json::Value {
    let sessions: Vec<serde_json::Value> = rows
        .iter()
        .filter(|r| project_matches(r, project))
        .filter(|r| status.is_none_or(|f| status_matches(r, f)))
        .map(|r| row_json(r, compact))
        .collect();
    serde_json::json!({ "sessions": sessions })
}

/// What `status` prints for `rows`: the `--direct` shape. The engine keeps
/// no pending tool, so `pendingToolName` is null.
pub fn status_output(rows: &[AgentRow], project: Option<&str>) -> serde_json::Value {
    let rows: Vec<&AgentRow> = rows
        .iter()
        .filter(|r| project_matches(r, project))
        .collect();
    let mut by_status = std::collections::HashMap::new();
    let mut by_project = std::collections::HashMap::new();
    for r in &rows {
        *by_status
            .entry(status_label(&row_status(r)))
            .or_insert(0u32) += 1;
        *by_project.entry(project_name(r)).or_insert(0u32) += 1;
    }
    let needs_permission: Vec<serde_json::Value> = rows
        .iter()
        .filter(|r| row_status(r) == SessionStatus::NeedsAttention)
        .map(|r| {
            serde_json::json!({
                "id": session_id(r),
                "sessionKey": r.session_key,
                "provider": r.provider,
                "sessionName": project_name(r),
                "pendingToolName": null,
            })
        })
        .collect();
    serde_json::json!({
        "total": rows.len(),
        "byStatus": by_status,
        "byProject": by_project,
        "needsPermission": needs_permission,
    })
}

/// The provider's session id of a row: `providerSid`, else the part of the
/// session key after `<provider>:`.
fn session_id(row: &AgentRow) -> String {
    row.provider_sid.clone().unwrap_or_else(|| {
        row.session_key
            .as_deref()
            .and_then(|k| k.split_once(':'))
            .map(|(_, id)| id.to_string())
            .unwrap_or_default()
    })
}

// ── Commands ──────────────────────────────────────────────────────────

/// `c9watch list` through the engine.
pub fn cmd_list(
    project: Option<&str>,
    status: Option<&str>,
    compact: bool,
    pretty: bool,
) -> Result<(), String> {
    let rows = session_rows()?;
    print_json(&list_output(&rows, project, status, compact), pretty)
}

/// `c9watch status` through the engine.
pub fn cmd_status(project: Option<&str>, pretty: bool) -> Result<(), String> {
    let rows = session_rows()?;
    print_json(&status_output(&rows, project), pretty)
}

/// `c9watch self` through the engine: the row of the session whose pid is
/// `claude_pid`, the caller's nearest `claude` ancestor.
pub fn cmd_self(claude_pid: u32, pretty: bool) -> Result<(), String> {
    let rows = session_rows()?;
    print_json(&self_output(&rows, claude_pid)?, pretty)
}

/// What `self` prints: the row whose pid is `claude_pid`, with the
/// `--direct` error when there is none.
pub fn self_output(rows: &[AgentRow], claude_pid: u32) -> Result<serde_json::Value, String> {
    rows.iter()
        .find(|r| r.pid == Some(i64::from(claude_pid)))
        .map(|r| row_json(r, false))
        .ok_or(format!(
            "Found claude process (PID {}) but no matching session",
            claude_pid
        ))
}

// ── watch and wait ────────────────────────────────────────────────────

/// The live sessions among `rows`, in their order, as `watch` snapshots.
pub fn view(rows: &[AgentRow], project: Option<&str>) -> (Vec<watch::Snapshot>, Vec<AgentRow>) {
    let live: Vec<AgentRow> = rows
        .iter()
        .filter(|r| r.gone_at.is_none() && r.session_key.is_some())
        .filter(|r| project_matches(r, project))
        .cloned()
        .collect();
    let snapshots = live
        .iter()
        .map(|r| watch::Snapshot {
            key: r.session_key.clone().unwrap_or_default(),
            id: session_id(r),
            status: row_status(r),
            pending_tool: None,
        })
        .collect();
    (snapshots, live)
}

/// Apply one event frame to `rows`: `agent.upsert` carries the whole row,
/// `agent.gone` the id of a row whose session is gone.
pub fn apply(rows: &mut Vec<AgentRow>, frame: EventFrame) {
    match frame.topic.as_str() {
        "agent.upsert" => {
            let Ok(row) = serde_json::from_value::<AgentRow>(frame.data) else {
                return;
            };
            match rows.iter_mut().find(|r| r.id == row.id) {
                Some(old) => *old = row,
                None => rows.push(row),
            }
        }
        "agent.gone" => {
            if let Some(id) = frame.data.get("id").and_then(|v| v.as_str()) {
                rows.retain(|r| r.id != id);
            }
        }
        _ => {}
    }
}

/// `snapshot.get`, then `events.subscribe` from its cursor for the row
/// events, on one connection.
fn open_feed(auto_start: bool) -> Result<(Vec<AgentRow>, Frames), String> {
    let mut conn = connect_with(auto_start)?;
    let snap: SnapshotResult =
        serde_json::from_value(call(&mut conn, "snapshot.get", serde_json::json!({}))?)
            .map_err(|e| format!("Bad snapshot.get result from the engine: {}", e))?;
    let params = SubscribeParams {
        since: Some(snap.cursor),
        topics: vec!["agent.upsert".into(), "agent.gone".into()],
        subjects: Vec::new(),
    };
    let params = serde_json::to_value(params).map_err(|e| e.to_string())?;
    call(&mut conn, "events.subscribe", params)?;
    Ok((snap.agents, conn.into_frames()?))
}

/// Follow the engine's sessions on a thread: one `Poll::Live` for the
/// snapshot and one after each event. When the stream ends (the engine
/// restarted, or the subscription failed), it reads a new snapshot and
/// goes on; the receiver's diff hides what did not change. It sends
/// `Poll::Failed` and stops when the engine cannot be reached at first, or
/// for `RECONNECT_FOR` after a stream ended. For `RESTART_GRACE` after a
/// stream ended it only connects, so an `engine restart` starts the new
/// engine, not this command.
pub(crate) fn spawn_feed(project: Option<String>) -> mpsc::Receiver<Poll<Vec<AgentRow>>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let project = project.as_deref();
        // When the last stream ended; `None` before the first one opened.
        let mut lost_at: Option<Instant> = None;
        loop {
            let auto_start = lost_at.is_none_or(|t| t.elapsed() >= RESTART_GRACE);
            let (mut rows, frames) = match open_feed(auto_start) {
                Ok(feed) => feed,
                Err(e) => {
                    if lost_at.is_none_or(|t| t.elapsed() >= RECONNECT_FOR) {
                        let _ = tx.send(Poll::Failed(e));
                        return;
                    }
                    std::thread::sleep(RECONNECT_EVERY);
                    continue;
                }
            };
            let opened_at = Instant::now();
            let send = |rows: &[AgentRow]| {
                let (snapshots, live) = view(rows, project);
                tx.send(Poll::Live(snapshots, live)).is_ok()
            };
            if !send(&rows) {
                return;
            }
            for line in frames.lines.iter() {
                // A response frame here is the subscription's error: read a
                // new snapshot.
                let Ok(frame) = serde_json::from_str::<EventFrame>(&line) else {
                    break;
                };
                apply(&mut rows, frame);
                if !send(&rows) {
                    return;
                }
            }
            lost_at = Some(Instant::now());
            // A stream that ends at once is not reopened in a busy loop.
            if opened_at.elapsed() < RECONNECT_EVERY {
                std::thread::sleep(RECONNECT_EVERY);
            }
        }
    });
    rx
}

/// The event line for `change`, with the engine's row when it is live.
fn change_line(change: &watch::Change, rows: &[AgentRow], compact: bool) -> serde_json::Value {
    let live = rows
        .iter()
        .find(|r| r.session_key.as_deref() == Some(change.key.as_str()));
    watch::event_json(
        change,
        live.map(|r| serde_json::json!(r.provider)),
        live.map(|r| row_json(r, compact)),
        &chrono::Utc::now().to_rfc3339(),
    )
}

/// `c9watch watch` through the engine. `--interval` does not apply: lines
/// follow the engine's events.
pub fn cmd_watch(project: Option<&str>, compact: bool, changes_only: bool) -> Result<(), String> {
    let feed = spawn_feed(project.map(str::to_string));
    let mut prev = std::collections::HashMap::new();
    for poll in feed.iter() {
        match poll {
            Poll::Live(snapshots, rows) => {
                for change in watch::diff_sessions(&mut prev, snapshots, !changes_only) {
                    print_line(&change_line(&change, &rows, compact));
                }
            }
            Poll::Missed => {}
            Poll::Failed(e) => return Err(e),
        }
    }
    Err("The engine feed stopped".to_string())
}

/// `c9watch wait` through the engine; see `super::wait_loop`.
pub fn cmd_wait(
    reference: &str,
    until: watch::Until,
    timeout_secs: Option<u64>,
) -> Result<i32, String> {
    super::wait_loop(
        reference,
        until,
        timeout_secs,
        spawn_feed(None),
        |c, rows| change_line(c, rows, true),
    )
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

    fn at(key: &str, status: &str, cwd: &str) -> AgentRow {
        let mut r = row(key, status);
        r.cwd = Some(cwd.into());
        r.pid = Some(42);
        r
    }

    #[test]
    fn direct_or_an_agent_caller_skips_the_engine() {
        assert!(use_engine(false, &env_of(&[])));
        assert!(use_engine(false, &env_of(&[("C9_AGENT_ID", "")])));
        assert!(!use_engine(true, &env_of(&[])));
        assert!(!use_engine(false, &env_of(&[("C9_AGENT_ID", "agt_1")])));
        assert!(!use_engine(true, &env_of(&[("C9_AGENT_ID", "agt_1")])));
    }

    #[test]
    fn engine_statuses_read_as_the_direct_names() {
        for (engine, direct) in [
            ("working", "Working"),
            ("needs_attention", "NeedsAttention"),
            ("waiting_for_input", "WaitingForInput"),
            ("connecting", "Connecting"),
            ("something_new", "Connecting"),
        ] {
            assert_eq!(
                status_label(&row_status(&row("claudeCode:a", engine))),
                direct
            );
        }
        let mut r = row("claudeCode:a", "working");
        r.status = None;
        assert_eq!(row_status(&r), SessionStatus::Connecting);
    }

    #[test]
    fn list_filters_by_project_and_either_status_spelling() {
        let rows = vec![
            at("claudeCode:a", "working", "/w/alpha"),
            at("claudeCode:b", "waiting_for_input", "/w/beta"),
        ];
        let ids = |v: serde_json::Value| -> Vec<String> {
            v["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["sessionKey"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(ids(list_output(&rows, None, None, false)).len(), 2);
        assert_eq!(
            ids(list_output(&rows, Some("alp"), None, false)),
            ["claudeCode:a"]
        );
        assert_eq!(
            ids(list_output(&rows, None, Some("WaitingForInput"), false)),
            ["claudeCode:b"]
        );
        assert_eq!(
            ids(list_output(&rows, None, Some("WAITING_FOR_INPUT"), false)),
            ["claudeCode:b"]
        );
        assert!(ids(list_output(&rows, None, Some("NeedsAttention"), false)).is_empty());

        // The payload has the --direct names, and the c9p/1 row under
        // `engine`; compact keeps a few set fields of that row.
        let full = list_output(&rows, Some("alpha"), None, false);
        let s = &full["sessions"][0];
        assert_eq!(s["engine"], serde_json::to_value(&rows[0]).unwrap());
        assert_eq!(s["id"], "a");
        assert_eq!(s["sessionKey"], "claudeCode:a");
        assert_eq!(s["provider"], "claudeCode");
        assert_eq!(s["projectPath"], "/w/alpha");
        assert_eq!(s["sessionName"], "alpha");
        assert_eq!(s["status"], "Working");
        let compact = list_output(&rows, Some("alpha"), None, true);
        let keys: Vec<&String> = compact["sessions"][0]["engine"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(
            keys,
            [
                "cwd",
                "id",
                "pid",
                "provider",
                "providerSid",
                "sessionKey",
                "status"
            ]
        );
    }

    #[test]
    fn status_keeps_the_direct_shape() {
        let rows = vec![
            at("claudeCode:a", "working", "/w/alpha"),
            at("claudeCode:b", "needs_attention", "/w/alpha"),
            at("claudeCode:c", "needs_attention", "/w/beta"),
        ];
        let out = status_output(&rows, None);
        assert_eq!(out["total"], 3);
        assert_eq!(out["byStatus"]["Working"], 1);
        assert_eq!(out["byStatus"]["NeedsAttention"], 2);
        assert_eq!(out["byProject"]["alpha"], 2);
        assert_eq!(out["needsPermission"][0]["id"], "b");
        assert_eq!(out["needsPermission"][0]["sessionName"], "alpha");
        assert!(out["needsPermission"][0]["pendingToolName"].is_null());
        let out = status_output(&rows, Some("beta"));
        assert_eq!(out["total"], 1);
    }

    #[test]
    fn self_is_the_row_of_the_claude_pid() {
        let mut rows = vec![at("claudeCode:a", "working", "/w/alpha")];
        rows[0].pid = Some(7);
        let me = self_output(&rows, 7).unwrap();
        assert_eq!(me["id"], "a");
        assert_eq!(me["pid"], 7);
        assert_eq!(me["engine"], serde_json::to_value(&rows[0]).unwrap());
        assert_eq!(
            self_output(&rows, 8).unwrap_err(),
            "Found claude process (PID 8) but no matching session"
        );
    }

    fn frame(topic: &str, data: serde_json::Value) -> EventFrame {
        EventFrame {
            v: 1,
            kind: crate::engine::proto::EventType::Event,
            sub: Some("2".into()),
            cursor: "m:e:1".into(),
            topic: topic.into(),
            subject: None,
            ts: 0,
            origin: None,
            data,
        }
    }

    #[test]
    fn events_update_the_view() {
        let mut rows = vec![at("claudeCode:a", "working", "/w/alpha")];
        let (snaps, _) = view(&rows, None);
        assert_eq!(snaps[0].status, SessionStatus::Working);
        assert_eq!(snaps[0].id, "a");

        let mut changed = rows[0].clone();
        changed.status = Some("waiting_for_input".into());
        apply(
            &mut rows,
            frame("agent.upsert", serde_json::to_value(&changed).unwrap()),
        );
        let new = at("claudeCode:b", "working", "/w/beta");
        apply(
            &mut rows,
            frame("agent.upsert", serde_json::to_value(&new).unwrap()),
        );
        let (snaps, live) = view(&rows, None);
        assert_eq!(snaps.len(), 2);
        assert_eq!(snaps[0].status, SessionStatus::WaitingForInput);
        assert_eq!(live[1].session_key.as_deref(), Some("claudeCode:b"));
        // The project filter applies to the view.
        assert_eq!(view(&rows, Some("beta")).0.len(), 1);

        apply(
            &mut rows,
            frame(
                "agent.gone",
                serde_json::json!({"id": "agt_claudeCode:a", "goneAt": 1}),
            ),
        );
        let (snaps, _) = view(&rows, None);
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].key, "claudeCode:b");

        // A pty row that gave its session back is not a session.
        let mut handed = rows[0].clone();
        handed.session_key = None;
        apply(
            &mut rows,
            frame("agent.upsert", serde_json::to_value(&handed).unwrap()),
        );
        assert!(view(&rows, None).0.is_empty());
    }
}
