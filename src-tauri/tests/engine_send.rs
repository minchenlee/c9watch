//! `c9watch send` (C1b-M1-10) against a fake engine: a Unix socket at
//! `sock_path(<engine home>)` that answers `hello`, `msg.send` and
//! `events.subscribe` and then sends scripted event frames. A plain caller
//! finds it there through the auto-start fast path; an agent caller
//! (`C9_AGENT_ID`) reaches it through `C9_ENGINE_SOCK`.

#![cfg(all(unix, feature = "engine"))]

use c9watch_lib::engine::home::{sock_path, ENGINE_HOME_ENV};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_c9watch");
const MACHINE: &str = "m1";
const SENT_CURSOR: &str = "m1:e1:10";

/// What one connection does after its `events.subscribe` answer.
#[derive(Clone)]
enum After {
    /// Send these frames, then keep the connection open.
    Send(Vec<Value>),
    /// Send these frames, then close the connection.
    SendAndClose(Vec<Value>),
}

/// The fake engine. Connection `n` follows `script[n]` (the last entry
/// repeats). `msg.send` answers `send_reply`. Every request is recorded.
struct FakeEngine {
    _dir: tempfile::TempDir,
    home: PathBuf,
    engine_home: PathBuf,
    requests: Arc<Mutex<Vec<Value>>>,
}

fn sent() -> Value {
    json!({
        "msgId": "msg_1", "state": "queued", "cursor": SENT_CURSOR, "replayed": false,
        "to": {"id": "agt_1", "name": "w1", "addr": format!("{}/agt_1", MACHINE)},
    })
}

fn event(seq: u32, topic: &str, data: Value) -> Value {
    json!({
        "v": 1, "type": "event", "cursor": format!("m1:e1:{}", seq),
        "topic": topic, "subject": format!("{}/agt_1", MACHINE), "ts": 0,
        "data": data,
    })
}

fn msg_state(seq: u32, state: &str, detail: Option<&str>) -> Value {
    event(
        seq,
        "msg.state",
        json!({
            "msgId": "msg_1", "from": "local-unverified", "to": "agt_1",
            "state": state, "at": 1, "attempts": 1, "detail": detail,
        }),
    )
}

fn turn_end(seq: u32, id: &str) -> Value {
    event(
        seq,
        "agent.turn_end",
        json!({"id": id, "generation": 1, "by": "hook", "at": 1234}),
    )
}

fn write_line(s: &mut UnixStream, v: &Value) -> bool {
    let mut line = serde_json::to_vec(v).unwrap();
    line.push(b'\n');
    s.write_all(&line).is_ok()
}

fn serve(stream: UnixStream, send_reply: Value, after: After, requests: Arc<Mutex<Vec<Value>>>) {
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let req: Value = serde_json::from_str(line.trim_end()).unwrap();
        requests.lock().unwrap().push(req.clone());
        let id = req["id"].clone();
        let ok = |result: Value| json!({"v": 1, "id": id, "ok": true, "result": result});
        let resp = match req["method"].as_str().unwrap_or("") {
            "hello" => ok(json!({
                "apiMajor": 1, "apiMinor": 0, "engineVersion": "fake",
                "machineId": MACHINE, "principal": "local-unverified",
                "capabilities": [], "cursor": "m1:e1:1",
            })),
            "msg.send" => {
                let mut r = send_reply.clone();
                r["id"] = id.clone();
                r
            }
            "events.subscribe" => {
                let since = req["params"]["since"].clone();
                if !write_line(&mut writer, &ok(json!({ "cursor": since }))) {
                    return;
                }
                let (frames, close) = match &after {
                    After::Send(f) => (f, false),
                    After::SendAndClose(f) => (f, true),
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
    fn start(send_reply: Value, script: Vec<After>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        // The engine home is opened without following symlinks, and the
        // temp dir of macOS sits under one (/var).
        let root = dir.path().canonicalize().unwrap();
        let home = root.join("h");
        std::fs::create_dir(&home).unwrap();
        let engine_home = root.join("e");
        std::fs::create_dir(&engine_home).unwrap();
        std::fs::set_permissions(&engine_home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let listener = UnixListener::bind(sock_path(&engine_home)).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().enumerate() {
                let Ok(stream) = stream else { return };
                let after = script[n.min(script.len() - 1)].clone();
                let (reply, recorded) = (send_reply.clone(), recorded.clone());
                std::thread::spawn(move || serve(stream, reply, after, recorded));
            }
        });
        FakeEngine {
            _dir: dir,
            home,
            engine_home,
            requests,
        }
    }

    fn ok() -> Value {
        json!({"v": 1, "ok": true, "seq": SENT_CURSOR, "result": sent()})
    }

    /// `c9watch send` as a plain caller: it finds the engine through the
    /// auto-start fast path.
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.arg("send")
            .args(args)
            .env("HOME", &self.home)
            .env(ENGINE_HOME_ENV, &self.engine_home)
            .env_remove("C9_AGENT_ID")
            .env_remove("C9_ENGINE_SOCK")
            .env_remove("XDG_STATE_HOME")
            .stdin(Stdio::null());
        c
    }

    /// `c9watch send` from inside agent `agt_me`.
    fn agent_cmd(&self, args: &[&str]) -> Command {
        let mut c = self.cmd(args);
        c.env("C9_AGENT_ID", "agt_me")
            .env("C9_ENGINE_SOCK", sock_path(&self.engine_home));
        c
    }

    fn requests(&self, method: &str) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == method)
            .cloned()
            .collect()
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
            panic!("send did not exit in 60 s: {:?}", out);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

fn stdout_lines(out: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn stderr_json(out: &Output) -> Value {
    let text = String::from_utf8_lossy(&out.stderr);
    serde_json::from_str(text.lines().last().unwrap_or_default()).unwrap()
}

#[test]
fn an_agent_sends_through_its_socket_with_the_idem_key() {
    let fake = FakeEngine::start(FakeEngine::ok(), vec![After::Send(vec![])]);
    let out = run_bounded(fake.agent_cmd(&[
        "w1",
        "--message",
        "hi\r\nthere",
        "--kind",
        "request",
        "--idem",
        "k1",
    ]));
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    assert_eq!(stdout_lines(&out), vec![json!({ "msg": sent() })]);
    let sends = fake.requests("msg.send");
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0]["idem"], "k1");
    assert_eq!(
        sends[0]["params"],
        json!({"to": "w1", "body": "hi\nthere", "kind": "request"})
    );
    // Only the fake socket is in the engine home: the agent started nothing.
    let names: Vec<_> = std::fs::read_dir(&fake.engine_home)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        names,
        vec![sock_path(&fake.engine_home).file_name().unwrap()]
    );
}

#[test]
fn an_agent_may_not_wait() {
    let fake = FakeEngine::start(FakeEngine::ok(), vec![After::Send(vec![])]);
    let out = run_bounded(fake.agent_cmd(&["w1", "hi", "--wait"]));
    assert_eq!(out.status.code(), Some(1), "{:?}", out);
    let err = stderr_json(&out)["error"].as_str().unwrap().to_string();
    assert!(
        err.contains("end your turn; the reply arrives as a new message"),
        "{}",
        err
    );
    assert!(fake.requests("msg.send").is_empty());
}

#[test]
fn an_engine_error_prints_its_body() {
    let refused = json!({"v": 1, "ok": false, "error": {
        "code": "unknown_address", "msg": "no agent is named 'w2'", "retryable": false,
        "hint": "check the name", "data": {"nearest": "w1"},
    }});
    let fake = FakeEngine::start(refused, vec![After::Send(vec![])]);
    let out = run_bounded(fake.cmd(&["w2", "hi"]));
    assert_eq!(out.status.code(), Some(1), "{:?}", out);
    assert!(out.stdout.is_empty());
    let err = stderr_json(&out);
    assert_eq!(err["error"]["code"], "unknown_address");
    assert_eq!(err["error"]["data"]["nearest"], "w1");
}

#[test]
fn wait_ends_on_the_turn_end_after_delivery() {
    let fake = FakeEngine::start(
        FakeEngine::ok(),
        vec![After::Send(vec![
            // The turn the agent was in when the message was queued.
            turn_end(11, "agt_1"),
            msg_state(12, "delivering", None),
            msg_state(13, "delivered", Some("prompt_hook")),
            turn_end(14, "agt_other"),
            turn_end(15, "agt_1"),
        ])],
    );
    let out = run_bounded(fake.cmd(&["w1", "hi", "--wait"]));
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let lines = stdout_lines(&out);
    assert_eq!(lines.len(), 2, "{:?}", out);
    assert_eq!(lines[0], json!({ "msg": sent() }));
    assert_eq!(lines[1]["msg"]["state"], "delivered");
    assert_eq!(lines[1]["msg"]["detail"], "prompt_hook");
    assert_eq!(lines[1]["turnEnd"], "m1:e1:15");
    let subs = fake.requests("events.subscribe");
    assert_eq!(subs[0]["params"]["since"], SENT_CURSOR);
    assert_eq!(subs[0]["params"]["subjects"], json!(["m1/agt_1"]));
}

#[test]
fn an_uncertain_message_exits_1() {
    let fake = FakeEngine::start(
        FakeEngine::ok(),
        vec![After::Send(vec![
            msg_state(12, "delivering", None),
            msg_state(13, "uncertain", Some("paste not seen in input box")),
        ])],
    );
    let out = run_bounded(fake.cmd(&["w1", "hi", "--wait"]));
    assert_eq!(out.status.code(), Some(1), "{:?}", out);
    let lines = stdout_lines(&out);
    assert_eq!(lines.len(), 2, "{:?}", out);
    assert_eq!(lines[1]["msg"]["state"], "uncertain");
    assert_eq!(lines[1]["msg"]["detail"], "paste not seen in input box");
    assert!(lines[1].get("turnEnd").is_none());
}

#[test]
fn wait_goes_on_from_the_last_cursor_after_the_stream_ends() {
    let fake = FakeEngine::start(
        FakeEngine::ok(),
        vec![
            // The auto-start probe: `hello` only.
            After::Send(vec![]),
            After::SendAndClose(vec![msg_state(13, "delivered", Some("prompt_hook"))]),
            After::Send(vec![turn_end(16, "agt_1")]),
        ],
    );
    let out = run_bounded(fake.cmd(&["w1", "hi", "--wait"]));
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    assert_eq!(stdout_lines(&out)[1]["turnEnd"], "m1:e1:16");
    let since: Vec<Value> = fake
        .requests("events.subscribe")
        .iter()
        .map(|r| r["params"]["since"].clone())
        .collect();
    assert_eq!(since, vec![json!(SENT_CURSOR), json!("m1:e1:13")]);
}

#[test]
fn wait_times_out_with_exit_2() {
    let fake = FakeEngine::start(
        FakeEngine::ok(),
        vec![After::Send(vec![msg_state(12, "delivering", None)])],
    );
    let out = run_bounded(fake.cmd(&["w1", "hi", "--wait", "--timeout", "1"]));
    assert_eq!(out.status.code(), Some(2), "{:?}", out);
    assert_eq!(stdout_lines(&out).len(), 1);
    assert!(stderr_json(&out)["error"]
        .as_str()
        .unwrap()
        .starts_with("timeout:"));
}
