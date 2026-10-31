//! Real c9p dispatcher, isolated provider fixtures, no GUI or paid provider.
#![cfg(all(unix, feature = "engine"))]

use c9watch_lib::engine::{
    api::{self, ServerCtx},
    db::{self, outbox::NewEvent},
    home::sock_path,
};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

const BIN: &str = env!("CARGO_BIN_EXE_c9watch");

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    engine: PathBuf,
    db: Arc<db::Db>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("h");
        let engine = dir.path().join("e");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir(&engine).unwrap();
        let history: String = (0..205).map(|n| format!("{}\n", json!({
            "sessionId": format!("s{n}"), "display": "private prompt", "timestamp": 1793491200000u64 + n,
            "project": "/example/project"
        }))).collect();
        std::fs::write(home.join(".claude/history.jsonl"), history).unwrap();
        let project = home.join(".claude/projects/-example-project");
        std::fs::create_dir_all(&project).unwrap();
        for n in 0..205 {
            std::fs::write(project.join(format!("s{n}.jsonl")), "{}\n").unwrap();
        }
        let db = Arc::new(db::open(&engine, "m-docs").unwrap());
        let mut ctx = ServerCtx::new("m-docs".into())
            .with_db(db.clone())
            .with_cost_cache(engine.join("cost-cache.json"));
        ctx.user_home = Some(home.clone());
        let listener = tokio::net::UnixListener::bind(sock_path(&engine)).unwrap();
        let server = tokio::spawn(api::serve(listener, Arc::new(ctx)));
        Self {
            _dir: dir,
            home,
            engine,
            db,
            server,
        }
    }
    fn cmd(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut c = Command::new(program);
        c.env("HOME", &self.home)
            .env("C9WATCH_ENGINE_HOME", &self.engine)
            .env_remove("C9_AGENT_ID")
            .env_remove("C9_ENGINE_SOCK");
        c
    }
    fn run(&self, args: &[&str]) -> Output {
        self.cmd(BIN).args(args).output().unwrap()
    }
    fn emit(&self, topic: &str, data: Value) {
        let topic = topic.to_string();
        self.db
            .write_blocking(move |tx| {
                tx.emit(&NewEvent {
                    topic,
                    subject: Some("m-docs/claudeCode:s1".into()),
                    origin: None,
                    data,
                })
            })
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        self.db.close();
    }
}
fn lines(out: &Output) -> Vec<Value> {
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exports_page_history_redact_events_and_preserve_output_files() {
    let f = Fixture::new();
    f.emit("session.view", json!({"sessionKey":"claudeCode:s1", "id":"s1", "firstPrompt":"secret", "futureField":"secret"}));
    f.emit("legacy.inbox", json!({"raw":"secret"}));
    let history = lines(&f.run(&["export", "history"]));
    assert_eq!(history.len(), 205, "must read all pages");
    assert!(history.iter().all(|v| v.get("display").is_none()));
    assert_eq!(
        lines(&f.run(&["export", "history", "--include-conversation"]))[0]["display"],
        "private prompt"
    );
    let filtered = lines(&f.run(&[
        "export",
        "history",
        "--since",
        "2026-11-01",
        "--until",
        "2026-11-02",
    ]));
    assert_eq!(filtered.len(), 205);
    let events = lines(&f.run(&["export", "events"]));
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]["data"],
        json!({"id":"s1", "sessionKey":"claudeCode:s1"})
    );
    assert_eq!(
        lines(&f.run(&["export", "events", "--include-conversation"]))[0]["data"]["firstPrompt"],
        "secret"
    );
    let cursor = f.db.head_cursor().to_string();
    assert!(lines(&f.run(&["export", "events", "--since", &cursor])).is_empty());
    let expired = f.run(&["export", "events", "--since", "other:epoch:0"]);
    assert!(!expired.status.success());
    assert!(String::from_utf8_lossy(&expired.stderr).contains("cursor_expired"));
    let file = f.engine.join("export.csv");
    let out = f.run(&[
        "export",
        "history",
        "--format",
        "csv",
        "--out",
        file.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{:?}", out);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let saved = std::fs::read(&file).unwrap();
    assert!(!String::from_utf8_lossy(&saved).contains("private prompt"));
    assert!(!f
        .run(&["export", "history", "--out", file.to_str().unwrap()])
        .status
        .success());
    assert_eq!(std::fs::read(&file).unwrap(), saved);
    assert!(lines(&f.run(&["export", "sessions"])).is_empty());
    assert!(lines(&f.run(&["export", "cost"])).is_empty());
    // L1 is available starting at 1.9; an empty fixture is a successful list.
    assert!(lines(&f.run(&["export", "annotations"])).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn documented_examples_run_against_a_fixture_engine() {
    // Enabled by public-api.yml after the SDK and example TS are compiled.
    if std::env::var_os("C9P_DOC_EXAMPLES").is_none() {
        return;
    }
    let f = Fixture::new();
    f.emit(
        "session.view",
        json!({"id":"s1", "sessionKey":"claudeCode:s1"}),
    );
    let start = f.db.cursor_at(0).to_string();
    let out = f
        .cmd("python3")
        .arg(root().join("examples/python-watch/watch.py"))
        .args(["--since", &start, "--max-events", "1"])
        .output()
        .unwrap();
    let events = lines(&out);
    assert_eq!(events[0]["topic"], "session.view");
    let mut web = f
        .cmd("node")
        .arg(root().join("examples/ts-web-board/smoke.mjs"))
        .arg(sock_path(&f.engine))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(web.stdout.take().unwrap());
    let mut ready = String::new();
    stdout.read_line(&mut ready).unwrap();
    assert_eq!(ready.trim(), "web board fixture: ready");
    f.emit(
        "session.view",
        json!({"id":"s1", "sessionKey":"claudeCode:s1"}),
    );
    f.emit(
        "session.gone",
        json!({"id":"s1", "sessionKey":"claudeCode:s1"}),
    );
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    assert!(web.wait().unwrap().success());
    assert!(rest.contains("web board fixture: ok"));
    let out = f
        .cmd("node")
        .arg(root().join("scripts/test-public-schema.mjs"))
        .arg(BIN)
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
}

#[test]
fn export_auto_starts_core_in_its_isolated_home() {
    use c9watch_lib::engine::{
        home::lock_path,
        lock::{engine_lock_held, engine_pid},
    };
    struct StartedCore(PathBuf);
    impl Drop for StartedCore {
        fn drop(&mut self) {
            let lock = lock_path(&self.0);
            if engine_lock_held(&lock).unwrap_or(false) {
                if let Some(pid) = engine_pid(&lock).unwrap_or(None) {
                    // This test owns the core in its private temporary home.
                    unsafe {
                        libc::kill(pid, libc::SIGINT);
                    }
                    for _ in 0..250 {
                        if !engine_lock_held(&lock).unwrap_or(false) {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    panic!("fixture core did not stop");
                }
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("h");
    std::fs::create_dir(&home).unwrap();
    let engine = dir.path().join("e");
    let _core = StartedCore(engine.clone());
    let out = Command::new(BIN)
        .args(["export", "sessions"])
        .env("HOME", &home)
        .env("C9WATCH_ENGINE_HOME", &engine)
        .env_remove("C9_AGENT_ID")
        .env_remove("C9_ENGINE_SOCK")
        .env_remove("XDG_STATE_HOME")
        .output()
        .unwrap();
    for row in lines(&out) {
        assert!(row["sessionKey"].is_string());
    }
    assert!(engine_lock_held(&lock_path(&engine)).unwrap());
    assert!(engine_pid(&lock_path(&engine)).unwrap().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_annotations_cli_and_event_replay_share_the_fixture_engine() {
    let f = Fixture::new();
    f.db.write_blocking(|tx| {
        tx.execute("INSERT INTO agents(id,role,provider,source,session_key) VALUES ('annotation-session','observed','claudeCode','detected','claudeCode:annotation-session')", [])?;
        Ok(())
    }).unwrap();
    let script = r#"
import json, os, socket
s = socket.socket(socket.AF_UNIX)
s.settimeout(10)
s.connect(os.path.join(os.environ['C9WATCH_ENGINE_HOME'], 'engine-v1.sock'))
f = s.makefile('rwb')
def send(i, method, params):
    f.write((json.dumps(dict(v=1, id=i, method=method, params=params))+'\n').encode())
    f.flush()
def read():
    return json.loads(f.readline())
send('hello', 'hello', dict(apiMajor=1, apiMinor=9))
assert read()['ok']
send('before', 'annotation.list', {})
cursor = read()['result']['cursor']
send('set', 'annotation.patch', dict(target=dict(kind='session', sessionKey='claudeCode:annotation-session'), source='ci', seq=1, values=dict(tests=dict(type='number', v=42), note=dict(type='string', v='secret annotation'))))
assert read()['result']['applied']
send('events', 'events.subscribe', dict(topics=['annotation.changed'], since=cursor))
assert read()['ok']
event = read()
assert event['topic'] == 'annotation.changed'
assert event['data']['values']['tests']['value']['v'] == 42
print('python annotation patch and event replay: ok')
"#;
    let out = f.cmd("python3").args(["-c", script]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "python annotation patch and event replay: ok"
    );
    let listed = lines(&f.run(&[
        "annotate",
        "list",
        "--session-key",
        "claudeCode:annotation-session",
    ]));
    assert_eq!(listed[0]["annotations"].as_array().unwrap().len(), 2);
    assert!(listed[0]["cursor"].is_string());
    assert!(
        listed[0].get("result").is_none(),
        "annotate prints the result, not a response frame"
    );
    let exported = lines(&f.run(&["export", "annotations"]));
    assert_eq!(exported.len(), 2);
    assert!(!serde_json::to_string(&exported)
        .unwrap()
        .contains("secret annotation"));
    assert!(serde_json::to_string(&lines(&f.run(&[
        "export",
        "annotations",
        "--include-conversation"
    ])))
    .unwrap()
    .contains("secret annotation"));
    assert!(f
        .run(&[
            "annotate",
            "set",
            "claudeCode:annotation-session",
            "ci/tests",
            r#"{"type":"number","v":43}"#
        ])
        .status
        .success());
    let deleted = lines(&f.run(&[
        "annotate",
        "delete",
        "claudeCode:annotation-session",
        "ci/tests",
    ]));
    assert_eq!(deleted[0]["deleted"], true);
    assert!(deleted[0].get("result").is_none());
}
