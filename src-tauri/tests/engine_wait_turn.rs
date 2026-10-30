//! `c9watch wait --event turn_ended --agent X` (C1a-37) against a fake
//! engine: a Unix socket in a temp dir that answers `hello`,
//! `snapshot.get` and `events.subscribe` and then sends scripted event
//! frames. The caller is an agent (`C9_AGENT_ID` and `C9_ENGINE_SOCK`), so
//! the command never auto-starts an engine; the tests check that it does
//! not create the (temp) engine home either.

#![cfg(all(unix, feature = "engine"))]

use c9watch_lib::engine::home::ENGINE_HOME_ENV;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_c9watch");
const MACHINE: &str = "m1";
const SNAP_CURSOR: &str = "m1:e1:3";

/// What one connection does after its `events.subscribe` answer.
#[derive(Clone)]
enum After {
    /// Send these frames, then keep the connection open.
    Send(Vec<Value>),
    /// Send these frames, then close the connection.
    SendAndClose(Vec<Value>),
    /// Answer the first `events.subscribe` with `cursor_expired`; then
    /// `snapshot.get` returns `agents` at `EXPIRED_CURSOR`, and the next
    /// subscribe goes on as `then`.
    Expired { agents: Value, then: Box<After> },
    /// Answer nothing, not even `hello`.
    Mute,
}

const EXPIRED_CURSOR: &str = "m1:e1:40";

/// The fake engine. Connection `n` follows `script[n]` (the last entry
/// repeats). Every request is recorded with its connection number.
struct FakeEngine {
    _dir: tempfile::TempDir,
    sock: PathBuf,
    home: PathBuf,
    engine_home: PathBuf,
    requests: Arc<Mutex<Vec<(usize, Value)>>>,
}

fn agent_row(id: &str, name: &str, state: &str) -> Value {
    json!({
        "id": id, "generation": 1, "mode": "free", "role": "human",
        "control": "pty", "provider": "claude", "source": "engine",
        "resumeCount": 0, "interruptedNotePending": false,
        "machineId": MACHINE, "state": state, "name": name, "cwd": "/w/x",
    })
}

fn observed_row(id: &str) -> Value {
    json!({
        "id": id, "generation": 1, "role": "observed", "control": "none",
        "provider": "claude", "source": "detected", "resumeCount": 0,
        "interruptedNotePending": false, "sessionKey": "claudeCode:s1",
    })
}

fn event(seq: u32, topic: &str, id: &str, data: Value) -> Value {
    json!({
        "v": 1, "type": "event", "cursor": format!("m1:e1:{}", seq),
        "topic": topic, "subject": format!("{}/{}", MACHINE, id), "ts": 0,
        "data": data,
    })
}

fn turn_end(seq: u32, id: &str) -> Value {
    event(
        seq,
        "agent.turn_end",
        id,
        json!({"id": id, "generation": 1, "by": "hook", "at": 1234}),
    )
}

fn write_line(s: &mut UnixStream, v: &Value) -> bool {
    let mut line = serde_json::to_vec(v).unwrap();
    line.push(b'\n');
    s.write_all(&line).is_ok()
}

fn serve(
    stream: UnixStream,
    n: usize,
    agents: Value,
    after: After,
    requests: Arc<Mutex<Vec<(usize, Value)>>>,
) {
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let (mut agents, mut after, mut cursor) = (agents, after, SNAP_CURSOR);
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let req: Value = serde_json::from_str(line.trim_end()).unwrap();
        requests.lock().unwrap().push((n, req.clone()));
        let id = req["id"].clone();
        if matches!(after, After::Mute) {
            continue;
        }
        let ok = |result: Value| json!({"v": 1, "id": id, "ok": true, "result": result});
        let resp = match req["method"].as_str().unwrap_or("") {
            "hello" => ok(json!({
                "apiMajor": 1, "apiMinor": 0, "engineVersion": "fake",
                "machineId": MACHINE, "principal": "agent:agt_me",
                "capabilities": [], "cursor": SNAP_CURSOR,
            })),
            "snapshot.get" => ok(json!({
                "cursor": cursor, "machineId": MACHINE, "agents": agents,
            })),
            "events.subscribe" if matches!(after, After::Expired { .. }) => {
                let After::Expired { agents: a, then } = after else {
                    unreachable!()
                };
                (agents, after, cursor) = (a, *then, EXPIRED_CURSOR);
                json!({"v": 1, "id": id, "ok": false,
                       "error": {"code": "cursor_expired", "msg": "pruned",
                                 "retryable": true, "hint": "snapshot.get"}})
            }
            "events.subscribe" => {
                let since = req["params"]["since"].clone();
                if !write_line(&mut writer, &ok(json!({ "cursor": since }))) {
                    return;
                }
                let (frames, close) = match &after {
                    After::Send(f) => (f, false),
                    After::SendAndClose(f) => (f, true),
                    After::Expired { .. } | After::Mute => unreachable!(),
                };
                for f in frames {
                    let mut f = f.clone();
                    f["sub"] = id.clone();
                    if !write_line(&mut writer, &f) {
                        return;
                    }
                }
                if close {
                    return;
                }
                continue;
            }
            m => json!({"v": 1, "id": id, "ok": false,
                        "error": {"code": "unknown_method", "msg": m,
                                  "retryable": false, "hint": ""}}),
        };
        if !write_line(&mut writer, &resp) {
            return;
        }
    }
}

impl FakeEngine {
    fn start(agents: Value, script: Vec<After>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("fake.sock");
        let home = dir.path().join("h");
        std::fs::create_dir(&home).unwrap();
        let engine_home = dir.path().join("e");
        let listener = UnixListener::bind(&sock).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().enumerate() {
                let Ok(stream) = stream else { return };
                let after = script[n.min(script.len() - 1)].clone();
                let (agents, recorded) = (agents.clone(), recorded.clone());
                std::thread::spawn(move || serve(stream, n, agents, after, recorded));
            }
        });
        FakeEngine {
            _dir: dir,
            sock,
            home,
            engine_home,
            requests,
        }
    }

    fn cmd(&self, sock: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.arg("wait")
            .args(args)
            .env("HOME", &self.home)
            .env(ENGINE_HOME_ENV, &self.engine_home)
            .env("C9_AGENT_ID", "agt_me")
            .env("C9_ENGINE_SOCK", sock)
            .env_remove("XDG_STATE_HOME")
            .stdin(Stdio::null());
        c
    }

    fn wait(&self, args: &[&str]) -> Output {
        run_bounded(self.cmd(&self.sock, args))
    }

    /// The `events.subscribe` params, in order, with their connection.
    fn subscribes(&self) -> Vec<(usize, Value)> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, r)| r["method"] == "events.subscribe")
            .map(|(n, r)| (*n, r["params"].clone()))
            .collect()
    }

    fn assert_no_engine_home(&self) {
        assert!(
            !self.engine_home.exists(),
            "an agent caller touched the engine home"
        );
    }
}

/// Run `cmd` and kill it if it is still running after 60 s.
fn run_bounded(mut cmd: Command) -> Output {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!("wait did not exit in 60 s: {:?}", out);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

fn stdout_line(out: &Output) -> Value {
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "one line expected: {:?}", out);
    serde_json::from_str(lines[0]).unwrap()
}

fn agents() -> Value {
    json!([
        agent_row("agt_other", "other", "working"),
        agent_row("agt_1", "w1", "working"),
        observed_row("agt_obs"),
    ])
}

#[test]
fn wait_exits_on_the_agents_turn_end_only() {
    let mut working = agent_row("agt_1", "w1", "working");
    working["status"] = json!("working");
    let fake = FakeEngine::start(
        agents(),
        vec![After::Send(vec![
            turn_end(4, "agt_other"),
            event(5, "agent.upsert", "agt_1", working),
            turn_end(6, "agt_1"),
        ])],
    );
    for reference in ["agt_1", "w1"] {
        let out = fake.wait(&["--event", "turn_ended", "--agent", reference]);
        assert_eq!(out.status.code(), Some(0), "{:?}", out);
        let line = stdout_line(&out);
        assert_eq!(line["event"], "turn_ended");
        assert_eq!(line["agentId"], "agt_1");
        assert_eq!(line["name"], "w1");
        assert_eq!(line["by"], "hook");
        assert_eq!(line["generation"], 1);
        assert_eq!(line["at"], 1234);
        assert_eq!(line["cursor"], "m1:e1:6");
        assert_eq!(line["schema"], 1);
    }
    // It subscribed from the snapshot's cursor, to this agent only.
    let subs = fake.subscribes();
    assert_eq!(subs.len(), 2);
    for (_, params) in subs {
        assert_eq!(params["since"], SNAP_CURSOR);
        assert_eq!(params["subjects"], json!(["m1/agt_1"]));
        let topics = params["topics"].as_array().unwrap();
        assert!(topics.contains(&json!("agent.turn_end")));
    }
    fake.assert_no_engine_home();
}

#[test]
fn a_lost_stream_resumes_from_the_last_cursor() {
    let fake = FakeEngine::start(
        agents(),
        vec![
            After::SendAndClose(vec![turn_end(8, "agt_other")]),
            After::Send(vec![turn_end(9, "agt_1")]),
        ],
    );
    let out = fake.wait(&["--event", "turn_ended", "--agent", "agt_1"]);
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    assert_eq!(stdout_line(&out)["cursor"], "m1:e1:9");
    let subs = fake.subscribes();
    assert_eq!(subs.len(), 2, "{subs:?}");
    assert_eq!(subs[0].1["since"], SNAP_CURSOR);
    // A new connection, from the last frame the first one carried.
    assert_ne!(subs[0].0, subs[1].0);
    assert_eq!(subs[1].1["since"], "m1:e1:8");
    fake.assert_no_engine_home();
}

#[test]
fn an_agent_that_stops_first_exits_3() {
    let cases = [
        (
            event(
                5,
                "agent.upsert",
                "agt_1",
                agent_row("agt_1", "w1", "retired"),
            ),
            "retired",
        ),
        (
            event(
                5,
                "agent.gone",
                "agt_1",
                json!({"id": "agt_1", "goneAt": 9}),
            ),
            "gone",
        ),
        (
            event(
                5,
                "agent.failed",
                "agt_1",
                json!({"id": "agt_1", "reason": "exited"}),
            ),
            "failed",
        ),
    ];
    for (frame, why) in cases {
        let fake = FakeEngine::start(agents(), vec![After::Send(vec![frame])]);
        let out = fake.wait(&["--event", "turn_ended", "--agent", "agt_1"]);
        assert_eq!(out.status.code(), Some(3), "{why}: {:?}", out);
        let line = stdout_line(&out);
        assert_eq!(line["event"], "agent_ended", "{why}");
        assert_eq!(line["reason"], why);
    }
}

#[test]
fn no_matching_engine_agent_exits_2_and_silence_times_out() {
    let fake = FakeEngine::start(agents(), vec![After::Send(Vec::new())]);
    for reference in ["nope", "agt_obs"] {
        let out = fake.wait(&["--event", "turn_ended", "--agent", reference]);
        assert_eq!(out.status.code(), Some(2), "{reference}: {:?}", out);
        assert!(out.stdout.is_empty());
        let err: Value = serde_json::from_slice(&out.stderr).unwrap();
        assert!(err["error"].as_str().unwrap().contains(reference));
    }
    let start = Instant::now();
    let out = fake.wait(&[
        "--event",
        "turn_ended",
        "--agent",
        "agt_1",
        "--timeout",
        "1",
    ]);
    assert_eq!(out.status.code(), Some(124), "{:?}", out);
    assert!(start.elapsed() < Duration::from_secs(10));
    assert!(out.stdout.is_empty());
    fake.assert_no_engine_home();
}

#[test]
fn an_agent_caller_without_its_engine_sock_fails_and_starts_nothing() {
    let fake = FakeEngine::start(agents(), vec![After::Send(Vec::new())]);
    let args = ["--event", "turn_ended", "--agent", "agt_1"];
    let missing = fake.engine_home.with_file_name("missing.sock");
    let mut unset = fake.cmd(&fake.sock, &args);
    unset.env_remove("C9_ENGINE_SOCK");
    for (what, cmd) in [("unreachable", fake.cmd(&missing, &args)), ("unset", unset)] {
        let out = run_bounded(cmd);
        assert_eq!(out.status.code(), Some(1), "{what}: {:?}", out);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("engine_unreachable"), "{what}: {err}");
        assert!(out.stdout.is_empty(), "{what}");
    }
    assert!(fake.requests.lock().unwrap().is_empty());
    fake.assert_no_engine_home();
}

#[test]
fn an_expired_cursor_reads_a_new_snapshot() {
    let only = |row: Value| json!([row]);
    // Still mid-turn: subscribe from the new snapshot and go on.
    let fake = FakeEngine::start(
        agents(),
        vec![After::Expired {
            agents: only(agent_row("agt_1", "w1", "working")),
            then: Box::new(After::Send(vec![turn_end(41, "agt_1")])),
        }],
    );
    let out = fake.wait(&["--event", "turn_ended", "--agent", "agt_1"]);
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    assert_eq!(stdout_line(&out)["by"], "hook");
    let subs = fake.subscribes();
    assert_eq!(subs.len(), 2, "{subs:?}");
    assert_eq!(subs[0].1["since"], SNAP_CURSOR);
    assert_eq!(subs[1].1["since"], EXPIRED_CURSOR);

    // Gone from the new snapshot: exit 3, `gone`.
    let fake = FakeEngine::start(
        agents(),
        vec![After::Expired {
            agents: only(agent_row("agt_other", "other", "working")),
            then: Box::new(After::Send(Vec::new())),
        }],
    );
    let out = fake.wait(&["--event", "turn_ended", "--agent", "agt_1"]);
    assert_eq!(out.status.code(), Some(3), "{:?}", out);
    let line = stdout_line(&out);
    assert_eq!(line["event"], "agent_ended");
    assert_eq!(line["reason"], "gone");

    // Mid-turn when the wait began and idle now: the turn ended in the
    // pruned events.
    let fake = FakeEngine::start(
        agents(),
        vec![After::Expired {
            agents: only(agent_row("agt_1", "w1", "ready")),
            then: Box::new(After::Send(Vec::new())),
        }],
    );
    let out = fake.wait(&["--event", "turn_ended", "--agent", "agt_1"]);
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let line = stdout_line(&out);
    assert_eq!(line["event"], "turn_ended");
    assert_eq!(line["by"], "snapshot");
    assert_eq!(line["cursor"], EXPIRED_CURSOR);
    assert_eq!(fake.subscribes().len(), 1);
    fake.assert_no_engine_home();
}

#[test]
fn since_subscribes_from_the_given_cursor() {
    let fake = FakeEngine::start(agents(), vec![After::Send(vec![turn_end(2, "agt_1")])]);
    let out = fake.wait(&[
        "--event",
        "turn_ended",
        "--agent",
        "agt_1",
        "--since",
        "m1:e1:1",
    ]);
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    assert_eq!(stdout_line(&out)["cursor"], "m1:e1:2");
    assert_eq!(fake.subscribes()[0].1["since"], "m1:e1:1");
}

#[test]
fn the_timeout_covers_an_engine_that_does_not_answer() {
    let fake = FakeEngine::start(agents(), vec![After::Mute]);
    let start = Instant::now();
    let out = fake.wait(&[
        "--event",
        "turn_ended",
        "--agent",
        "agt_1",
        "--timeout",
        "1",
    ]);
    assert_eq!(out.status.code(), Some(124), "{:?}", out);
    assert!(
        start.elapsed() < Duration::from_secs(4),
        "{:?}",
        start.elapsed()
    );
    assert!(out.stdout.is_empty());
}
