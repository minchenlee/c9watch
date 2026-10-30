//! Contract tests for `c9watch engine`, `engine restart` and `api`, run
//! against the real binary in a temp HOME and a temp engine home. They never
//! touch `~/Library/Application Support/c9watch` or `~/.claude`.

#![cfg(all(unix, feature = "engine"))]

use c9watch_lib::engine::home::{lock_path, sock_path, ENGINE_HOME_ENV};
use c9watch_lib::engine::lock::{engine_lock_held, engine_pid};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_c9watch");

/// A temp HOME (`h`) and engine home (`e`). Dropping it stops the engine.
struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    engine: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("h");
        std::fs::create_dir(&home).unwrap();
        let engine = dir.path().join("e");
        Sandbox {
            _dir: dir,
            home,
            engine,
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("HOME", &self.home)
            .env(ENGINE_HOME_ENV, &self.engine)
            .env_remove("C9_AGENT_ID")
            .env_remove("C9_ENGINE_SOCK")
            .env_remove("XDG_STATE_HOME")
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn pid(&self) -> Option<libc::pid_t> {
        engine_pid(&lock_path(&self.engine)).unwrap_or(None)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.engine.join("engine.log")).unwrap_or_default()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let Some(pid) = self.pid() else { return };
        unsafe { libc::kill(pid, libc::SIGTERM) };
        if !wait_for(Duration::from_secs(5), || {
            !engine_lock_held(&lock_path(&self.engine)).unwrap_or(false)
        }) {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

fn wait_for(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}

fn alive(pid: libc::pid_t) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn stdout_json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}): {:?} / stderr {:?}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn mode(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn api_auto_starts_the_engine_and_round_trips() {
    let sb = Sandbox::new();
    let out = sb.run(&["api", "hello", "{}"]);
    assert!(out.status.success(), "{:?}", out);
    let resp = stdout_json(&out);
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["result"]["apiMajor"], 1);
    assert_eq!(resp["result"]["principal"], "local-unverified");

    let pid = sb.pid().expect("an engine holds the lock");
    assert!(alive(pid));
    assert_eq!(mode(&sb.engine), 0o700);
    assert_eq!(mode(&sock_path(&sb.engine)), 0o600);
    assert_eq!(mode(&sb.engine.join("engine.log")), 0o600);

    // An error response is printed and exits 1; the engine is reused.
    let out = sb.run(&["api", "no.such.method", "{}"]);
    assert_eq!(out.status.code(), Some(1));
    let resp = stdout_json(&out);
    assert_eq!(resp["ok"], false);
    assert_eq!(resp["error"]["code"], "unknown_method");
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown_method"));
    assert_eq!(sb.pid(), Some(pid));

    // Bad params fail locally.
    let out = sb.run(&["api", "hello", "{not json"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not JSON"));
}

#[test]
fn concurrent_clis_start_one_engine() {
    let sb = Sandbox::new();
    let children: Vec<_> = (0..4)
        .map(|_| {
            sb.cmd(&["api", "hello"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for c in children {
        let out = c.wait_with_output().unwrap();
        assert!(out.status.success(), "{:?}", out);
        assert_eq!(stdout_json(&out)["ok"], true);
    }
    assert!(sb.pid().is_some());
    let log = sb.log();
    assert_eq!(log.matches("Listening on").count(), 1, "log: {log}");
    assert!(!log.contains("engine_already_running"), "log: {log}");
}

#[test]
fn engine_restart_replaces_the_engine() {
    let sb = Sandbox::new();
    assert!(sb.run(&["api", "hello"]).status.success());
    let old = sb.pid().unwrap();

    let out = sb.run(&["engine", "restart"]);
    assert!(out.status.success(), "{:?}", out);
    assert_eq!(stdout_json(&out)["apiMajor"], 1);
    let new = sb.pid().unwrap();
    assert_ne!(old, new);
    assert!(wait_for(Duration::from_secs(5), || !alive(old)));

    let out = sb.run(&["api", "hello"]);
    assert!(out.status.success(), "{:?}", out);
    assert_eq!(sb.pid(), Some(new));
}

#[test]
fn engine_restart_with_no_engine_starts_one() {
    let sb = Sandbox::new();
    let out = sb.run(&["engine", "restart"]);
    assert!(out.status.success(), "{:?}", out);
    assert!(sb.pid().is_some());
}

#[test]
fn foreground_engine_holds_the_lock_and_cleans_up_on_sigterm() {
    use wait_timeout::ChildExt;
    let sb = Sandbox::new();
    let mut child = sb
        .cmd(&["engine"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let sock = sock_path(&sb.engine);
    assert!(wait_for(Duration::from_secs(10), || sock.exists()));

    // A second engine in the same home fails; clients use the first one.
    let out = sb.run(&["engine"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("engine_already_running"));
    assert!(sb.run(&["api", "hello"]).status.success());
    assert_eq!(sb.pid(), Some(child.id() as libc::pid_t));

    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .unwrap()
        .expect("engine exits on SIGTERM");
    assert!(status.success(), "{:?}", status);
    assert!(!sock.exists());
    assert!(sb.pid().is_none());
}

#[test]
fn agent_callers_cannot_run_or_start_the_engine() {
    let sb = Sandbox::new();
    for args in [
        &["engine"][..],
        &["engine", "restart"][..],
        &["api", "hello"][..],
    ] {
        let out = sb.cmd(args).env("C9_AGENT_ID", "a1").output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("agent_caller_refused"),
            "{args:?}: {:?}",
            out
        );
    }
    assert!(!sock_path(&sb.engine).exists());
    assert!(sb.pid().is_none());
}

/// The engine coexists with the legacy daemon: starting it leaves the
/// daemon, its socket and its worker dirs alone (no RPC, no retire).
#[cfg(feature = "pm-orchestration")]
#[test]
fn the_engine_leaves_the_legacy_daemon_alone() {
    let sb = Sandbox::new();
    let legacy = sb.home.join(".claude").join("c9watch");
    let mut daemon = sb
        .cmd(&["daemon"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let daemon_sock = legacy.join("daemon.sock");
    assert!(wait_for(Duration::from_secs(10), || daemon_sock.exists()));

    let mut sleeper = Command::new("sleep").arg("60").spawn().unwrap();
    let worker = legacy.join("workers").join("fixture-1");
    std::fs::create_dir_all(&worker).unwrap();
    let meta = serde_json::json!({
        "sessionId": "fixture-1",
        "pid": sleeper.id(),
        "name": "fixture",
        "cwd": "/",
        "spawnedAt": "2026-10-29T00:00:00Z",
        "spawnedBy": "test",
        "spawnArgs": {"permissionMode": "default", "addDirs": []},
        "stoppedAt": null,
    });
    std::fs::write(worker.join("meta.json"), meta.to_string()).unwrap();

    let out = sb.run(&["api", "hello"]);
    assert!(out.status.success(), "{:?}", out);
    assert!(sb.pid().is_some());
    // Give a misbehaving engine time to act on the daemon.
    std::thread::sleep(Duration::from_millis(500));

    assert!(daemon.try_wait().unwrap().is_none(), "the daemon exited");
    assert!(alive(daemon.id() as libc::pid_t));
    assert!(daemon_sock.exists());
    assert!(worker.join("meta.json").exists());
    assert!(sleeper.try_wait().unwrap().is_none(), "the worker pid died");

    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = sleeper.kill();
    let _ = sleeper.wait();
}

#[test]
fn a_db_from_a_newer_engine_fails_the_auto_start_with_its_code() {
    let sb = Sandbox::new();
    std::fs::create_dir(&sb.engine).unwrap();
    std::fs::set_permissions(&sb.engine, std::fs::Permissions::from_mode(0o700)).unwrap();
    let db = sb.engine.join("engine.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE t (x INTEGER); PRAGMA user_version = 9999;")
            .unwrap();
    }
    let before = std::fs::read(&db).unwrap();
    let out = sb.run(&["api", "hello"]);
    assert!(!out.status.success(), "{:?}", out);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("db_newer_than_engine"), "stderr: {err}");
    assert!(sb.pid().is_none());
    // Refused, not moved aside or changed.
    assert_eq!(std::fs::read(&db).unwrap(), before);
}

/// A stand-in for `claude` (never the real one). Detection's `--version`
/// gets a version; any other call without `--settings` fails at once. An
/// agent spawn logs its argv, sends the SessionStart hook through the
/// engine's `c9watch-agent` copy as CC would (source `resume` with
/// `--resume`), shows CC's idle title and a prompt, and waits to be killed.
/// A `ready-delay` file next to it holds seconds to wait first, as a slow
/// CC start would.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
case "$1" in --version) echo "2.1.312 (Claude Code)"; exit 0 ;; esac
dir=$(dirname "$0")
args="$*"
sid=""; source="startup"; settings=""
while [ $# -gt 0 ]; do
  case "$1" in
    --session-id) sid="$2"; shift 2 ;;
    --resume) sid="$2"; source="resume"; shift 2 ;;
    --settings) settings="$2"; shift 2 ;;
    *) shift ;;
  esac
done
[ -n "$settings" ] || exit 1
echo "$args" >> "$dir/argv.log"
[ -f "$dir/ready-delay" ] && sleep "$(cat "$dir/ready-delay")"
home=$(dirname "$(dirname "$settings")")
printf '{"session_id":"%s","source":"%s","hook_event_name":"SessionStart"}' "$sid" "$source" \
  | "$home/bin/c9watch-agent" hook session-start --kind claude > "$dir/ctx-$C9_AGENT_GEN.out"
printf '\033]0;\342\234\263 Claude Code\007\342\235\257 \r\n'
while :; do sleep 1; done
"#;

/// C1a-28 end to end: `engine restart` with a live agent drains it and the
/// next engine resumes it on the same session, as generation 2, with
/// `--resume`; the restart does not count as a resume.
#[test]
fn engine_restart_with_a_live_agent_drains_then_resumes_it() {
    let sb = Sandbox::new();
    let root = sb.home.parent().unwrap().to_path_buf();
    let bin = root.join("fakebin");
    std::fs::create_dir(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, FAKE_CLAUDE).unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cwd = root.join("work");
    std::fs::create_dir(&cwd).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = |args: &[&str]| -> Output {
        sb.cmd(args)
            .env("PATH", &path)
            .env("C9WATCH_EXPERIMENTAL_CREATE", "1")
            .output()
            .unwrap()
    };
    let list = || -> Vec<Value> {
        let out = run(&["api", "agent.list", r#"{"all":true}"#]);
        assert!(out.status.success(), "{:?}", out);
        stdout_json(&out)["result"]["agents"]
            .as_array()
            .unwrap()
            .clone()
    };

    let params = serde_json::json!({ "mode": "free", "cwd": cwd }).to_string();
    let out = run(&["api", "agent.create", &params]);
    assert!(out.status.success(), "{:?} / log {}", out, sb.log());
    let created = stdout_json(&out)["result"]["agent"].clone();
    assert_eq!(created["state"], "ready", "{}", created);
    assert_eq!(created["generation"], 1);
    let id = created["id"].as_str().unwrap().to_string();
    let sid = created["providerSid"].as_str().unwrap().to_string();
    let old = sb.pid().unwrap();

    let out = run(&["engine", "restart"]);
    assert!(out.status.success(), "{:?} / log {}", out, sb.log());
    assert_ne!(sb.pid(), Some(old));
    let mut last = Value::Null;
    let resumed = wait_for(Duration::from_secs(30), || {
        last = list()
            .into_iter()
            .find(|a| a["id"] == id.as_str())
            .unwrap_or_default();
        last["state"] == "ready" && last["generation"] == 2
    });
    assert!(resumed, "{} / log {}", last, sb.log());
    assert_eq!(last["providerSid"], sid.as_str());
    assert_eq!(last["resumeCount"], 0);
    assert!(last["stateReason"].is_null(), "{}", last);
    let argv = std::fs::read_to_string(bin.join("argv.log")).unwrap();
    let lines: Vec<&str> = argv.lines().collect();
    assert_eq!(lines.len(), 2, "{}", argv);
    assert!(lines[0].starts_with(&format!("--session-id {} --settings ", sid)));
    assert!(lines[1].starts_with(&format!("--resume {} --settings ", sid)));
    assert!(lines[1].contains(&format!("{}-2.json", id)), "{}", lines[1]);
    assert!(lines[1].contains("--permission-mode auto"), "{}", lines[1]);
    // D24: each generation's SessionStart context names its generation
    // and replaces the earlier blocks (reviews L4).
    for gen in [1, 2] {
        let ctx = std::fs::read_to_string(bin.join(format!("ctx-{}.out", gen))).unwrap();
        let header = format!(
            "c9watch context, generation {} (replaces any earlier c9watch context)",
            gen
        );
        assert!(ctx.contains(&header), "ctx-{}.out: {}", gen, ctx);
    }

    let out = run(&["api", "agent.retire", &format!(r#"{{"id":"{}"}}"#, id)]);
    assert!(out.status.success(), "{:?}", out);
    // Nothing of the fake agent is left running.
    let claude_s = claude.display().to_string();
    let gone = wait_for(Duration::from_secs(10), || {
        let out = Command::new("pgrep")
            .args(["-f", &claude_s])
            .output()
            .unwrap();
        out.stdout.is_empty()
    });
    assert!(gone, "a fake claude still runs");
}

// ── C1a-36: list, status, self, watch and wait through the engine ────

const DETECT_SID: &str = "22222222-3333-4444-5555-666666666666";

/// A stand-in `claude` for detection only (never the real one):
/// `--version` gets a version CLI detection accepts, `agents --json`
/// prints `agents.json` next to the script, anything else fails.
const DETECT_CLAUDE: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "2.1.312 (Claude Code)"; exit 0 ;;
  agents) cat "$(dirname "$0")/agents.json"; exit 0 ;;
esac
exit 1
"#;

/// One detected session: a `sleep` that `claude agents --json` reports,
/// in `<root>/work/proj`. `path` puts the stand-in first on PATH.
struct Detected {
    path: String,
    sleeper: std::process::Child,
}

impl Detected {
    fn new(sb: &Sandbox) -> Self {
        let root = sb.home.parent().unwrap().to_path_buf();
        let bin = root.join("detectbin");
        std::fs::create_dir(&bin).unwrap();
        let claude = bin.join("claude");
        std::fs::write(&claude, DETECT_CLAUDE).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cwd = root.join("work/proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let sleeper = Command::new("sleep").arg("300").spawn().unwrap();
        let started_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 1000;
        let agents = serde_json::json!([{
            "pid": sleeper.id(), "cwd": cwd, "kind": "interactive",
            "startedAt": started_ms, "sessionId": DETECT_SID, "status": "busy",
        }]);
        std::fs::write(bin.join("agents.json"), agents.to_string()).unwrap();
        Detected {
            path: format!("{}:/usr/bin:/bin", bin.display()),
            sleeper,
        }
    }

    fn cmd(&self, sb: &Sandbox, args: &[&str]) -> Command {
        let mut c = sb.cmd(args);
        c.env("PATH", &self.path);
        c
    }

    fn run(&self, sb: &Sandbox, args: &[&str]) -> Output {
        self.cmd(sb, args).output().unwrap()
    }

    fn end_session(&mut self) {
        let _ = self.sleeper.kill();
        let _ = self.sleeper.wait();
    }
}

impl Drop for Detected {
    fn drop(&mut self) {
        self.end_session();
    }
}

/// Run `list` until the engine's detection lists the session.
fn engine_lists_the_session(sb: &Sandbox, d: &Detected) -> Value {
    let mut last = Value::Null;
    let listed = wait_for(Duration::from_secs(30), || {
        let out = d.run(sb, &["list"]);
        assert!(out.status.success(), "{:?} / log {}", out, sb.log());
        last = stdout_json(&out);
        last["sessions"].as_array().is_some_and(|s| !s.is_empty())
    });
    assert!(listed, "{} / log {}", last, sb.log());
    last
}

/// A child that is killed and reaped when the test ends, pass or fail, so
/// a `watch` cannot outlive its sandbox and start engines in it.
struct Reap(std::process::Child);

impl std::ops::Deref for Reap {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Reap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Lines a child prints, read on a thread so a caller can wait with a
/// deadline without losing a line.
fn lines_of(child: &mut std::process::Child) -> std::sync::mpsc::Receiver<String> {
    use std::io::BufRead;
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    rx
}

#[test]
fn list_and_status_auto_start_the_engine_and_read_its_sessions() {
    let sb = Sandbox::new();
    let d = Detected::new(&sb);
    let key = format!("claudeCode:{}", DETECT_SID);

    let listed = engine_lists_the_session(&sb, &d);
    assert!(sb.pid().is_some(), "list did not auto-start the engine");
    // The payload is the engine's row, and there are no diagnostics.
    assert!(listed.get("diagnostics").is_none(), "{}", listed);
    let row = &listed["sessions"][0];
    assert_eq!(row["sessionKey"], key.as_str());
    assert_eq!(row["providerSid"], DETECT_SID);
    assert_eq!(row["role"], "observed");
    assert_eq!(row["status"], "connecting");
    assert!(row["id"].as_str().unwrap().starts_with("agt_"), "{}", row);
    assert!(
        row["cwd"].as_str().unwrap().ends_with("work/proj"),
        "{}",
        row
    );

    // Filters apply to the rows; `--status` takes either spelling.
    for (args, n) in [
        (&["list", "--status", "Connecting"][..], 1),
        (&["list", "--status", "connecting"][..], 1),
        (&["list", "--status", "Working"][..], 0),
        (&["list", "--project", "proj"][..], 1),
        (&["list", "--project", "nope"][..], 0),
    ] {
        let out = d.run(&sb, args);
        assert!(out.status.success(), "{args:?}: {:?}", out);
        let got = stdout_json(&out)["sessions"].as_array().unwrap().len();
        assert_eq!(got, n, "{args:?}");
    }
    let out = d.run(&sb, &["list", "--compact"]);
    let compact = stdout_json(&out)["sessions"][0].clone();
    assert_eq!(compact["sessionKey"], key.as_str());
    assert!(compact.get("role").is_none(), "{}", compact);

    let out = d.run(&sb, &["status"]);
    assert!(out.status.success(), "{:?}", out);
    let status = stdout_json(&out);
    assert_eq!(status["total"], 1);
    assert_eq!(status["byStatus"]["Connecting"], 1);
    assert_eq!(status["byProject"]["proj"], 1);

    // `--direct` keeps in-process detection and its shape.
    let out = d.run(&sb, &["list", "--direct"]);
    assert!(out.status.success(), "{:?}", out);
    let direct = stdout_json(&out);
    assert!(direct.get("diagnostics").is_some(), "{}", direct);
    assert_eq!(direct["sessions"][0]["sessionKey"], key.as_str());
    assert_eq!(direct["sessions"][0]["status"], "Connecting");
}

#[test]
fn watch_and_wait_follow_the_engine_events() {
    use wait_timeout::ChildExt;
    let sb = Sandbox::new();
    let mut d = Detected::new(&sb);
    engine_lists_the_session(&sb, &d);
    let key = format!("claudeCode:{}", DETECT_SID);

    // `done` is met at once: the session is not working.
    let out = d.run(
        &sb,
        &[
            "wait",
            &DETECT_SID[..8],
            "--until",
            "done",
            "--timeout",
            "20",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let line = stdout_json(&out);
    assert_eq!(line["event"], "started");
    assert_eq!(line["status"], "Connecting");
    assert_eq!(line["session"]["sessionKey"], key.as_str());
    // No live session matches: exit 2, as with `--direct`.
    let out = d.run(&sb, &["wait", "ffff", "--until", "done", "--timeout", "20"]);
    assert_eq!(out.status.code(), Some(2), "{:?}", out);
    // A timeout is exit 124.
    let out = d.run(
        &sb,
        &[
            "wait",
            &DETECT_SID[..8],
            "--until",
            "ended",
            "--timeout",
            "1",
        ],
    );
    assert_eq!(out.status.code(), Some(124), "{:?}", out);

    let mut watch = d
        .cmd(&sb, &["watch", "--compact"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map(Reap)
        .unwrap();
    let lines = lines_of(&mut watch);
    let mut wait = d
        .cmd(
            &sb,
            &["wait", DETECT_SID, "--until", "ended", "--timeout", "60"],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map(Reap)
        .unwrap();
    let waited = lines_of(&mut wait);
    let started: Value =
        serde_json::from_str(&lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
    assert_eq!(started["event"], "started");
    assert_eq!(started["sessionKey"], key.as_str());
    assert_eq!(started["session"]["status"], "connecting");
    // Let `wait` take its snapshot before the session ends.
    std::thread::sleep(Duration::from_secs(2));

    d.end_session();
    let stopped: Value =
        serde_json::from_str(&lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
    assert_eq!(stopped["event"], "stopped", "{}", stopped);
    assert_eq!(stopped["sessionKey"], key.as_str());
    assert_eq!(stopped["previousStatus"], "Connecting");

    let status = wait.wait_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!(status.and_then(|s| s.code()), Some(0));
    let line: Value =
        serde_json::from_str(&waited.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
    assert_eq!(line["event"], "stopped");
}

#[test]
fn an_agent_caller_detects_directly_and_starts_no_engine() {
    let sb = Sandbox::new();
    let d = Detected::new(&sb);
    for args in [
        &["list"][..],
        &["status"][..],
        &[
            "wait",
            &DETECT_SID[..8],
            "--until",
            "done",
            "--timeout",
            "20",
        ][..],
    ] {
        let out = d
            .cmd(&sb, args)
            .env("C9_AGENT_ID", "agt_x")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{args:?}: {:?}", out);
        assert!(out.stderr.is_empty(), "{args:?}: {:?}", out);
    }
    // The in-process shape, with the session.
    let out = d
        .cmd(&sb, &["list"])
        .env("C9_AGENT_ID", "agt_x")
        .output()
        .unwrap();
    let list = stdout_json(&out);
    assert!(list.get("diagnostics").is_some(), "{}", list);
    assert_eq!(list["sessions"][0]["status"], "Connecting");
    assert!(
        !sb.engine.exists(),
        "an agent caller touched the engine home"
    );
    assert!(sb.pid().is_none());
}

#[test]
fn an_unreachable_engine_fails_with_a_hint_to_direct() {
    let sb = Sandbox::new();
    let d = Detected::new(&sb);
    // An engine refuses to start on a db from a newer engine.
    std::fs::create_dir(&sb.engine).unwrap();
    std::fs::set_permissions(&sb.engine, std::fs::Permissions::from_mode(0o700)).unwrap();
    let conn = rusqlite::Connection::open(sb.engine.join("engine.db")).unwrap();
    conn.execute_batch("CREATE TABLE t (x INTEGER); PRAGMA user_version = 9999;")
        .unwrap();
    drop(conn);
    for args in [
        &["list"][..],
        &["status"][..],
        &["watch"][..],
        &["wait", "x", "--until", "done"][..],
    ] {
        let out = d.run(&sb, args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {:?}", out);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("engine_unreachable"), "{args:?}: {err}");
        assert!(err.contains("--direct"), "{args:?}: {err}");
        assert!(out.stdout.is_empty(), "{args:?}: {:?}", out);
    }
    assert!(sb.pid().is_none());
    // `--direct` still works.
    let out = d.run(&sb, &["list", "--direct"]);
    assert!(out.status.success(), "{:?}", out);
}

#[test]
fn watch_follows_a_new_engine_after_a_restart() {
    let sb = Sandbox::new();
    let mut d = Detected::new(&sb);
    engine_lists_the_session(&sb, &d);
    let mut watch = d
        .cmd(&sb, &["watch"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map(Reap)
        .unwrap();
    let lines = lines_of(&mut watch);
    let started: Value =
        serde_json::from_str(&lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
    assert_eq!(started["event"], "started");
    let old = sb.pid().unwrap();

    let out = d.run(&sb, &["engine", "restart"]);
    assert!(out.status.success(), "{:?}", out);
    assert_ne!(sb.pid(), Some(old));
    // The same session on the new engine is no change.
    assert!(
        lines.recv_timeout(Duration::from_secs(5)).is_err(),
        "watch printed a line for an unchanged session"
    );

    d.end_session();
    let stopped: Value =
        serde_json::from_str(&lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
    assert_eq!(stopped["event"], "stopped", "{}", stopped);
}

/// `wait --event turn_ended` outside an agent auto-starts the engine
/// (C1a-37); an agent it does not know exits 2.
#[test]
fn wait_for_a_turn_end_auto_starts_the_engine() {
    let sb = Sandbox::new();
    let out = sb.run(&[
        "wait",
        "--event",
        "turn_ended",
        "--agent",
        "agt_nope",
        "--timeout",
        "20",
    ]);
    assert_eq!(out.status.code(), Some(2), "{:?}", out);
    assert!(out.stdout.is_empty(), "{:?}", out);
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert!(err["error"].as_str().unwrap().contains("agt_nope"), "{err}");
    assert!(sb.pid().is_some_and(alive), "no engine was started");
}

// ── C1a-38: spawn --mode free through agent.create ──────────────────

/// A fake `claude` (FAKE_CLAUDE) first on PATH and a cwd for free agents.
struct FreeAgents {
    bin: PathBuf,
    cwd: PathBuf,
    path: String,
}

impl FreeAgents {
    fn new(sb: &Sandbox) -> Self {
        let root = sb.home.parent().unwrap().to_path_buf();
        let bin = root.join("fakebin");
        std::fs::create_dir(&bin).unwrap();
        let claude = bin.join("claude");
        std::fs::write(&claude, FAKE_CLAUDE).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cwd = root.join("work");
        std::fs::create_dir(&cwd).unwrap();
        FreeAgents {
            path: format!("{}:/usr/bin:/bin", bin.display()),
            bin,
            cwd,
        }
    }

    /// `c9watch <args>` in `<root>`, with the create switch on or off.
    fn run(&self, sb: &Sandbox, args: &[&str], create: bool) -> Output {
        let mut c = sb.cmd(args);
        c.env("PATH", &self.path)
            .current_dir(self.cwd.parent().unwrap());
        if create {
            c.env("C9WATCH_EXPERIMENTAL_CREATE", "1");
        } else {
            c.env_remove("C9WATCH_EXPERIMENTAL_CREATE");
        }
        c.output().unwrap()
    }

    /// Wait until no fake agent runs.
    fn all_gone(&self) -> bool {
        let claude = self.bin.join("claude").display().to_string();
        wait_for(Duration::from_secs(10), || {
            let out = Command::new("pgrep")
                .args(["-f", &claude])
                .output()
                .unwrap();
            out.stdout.is_empty()
        })
    }
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The contract round trip: `spawn --mode free` creates a ready free agent
/// (waiting past the 5 s read timeout of other calls for a slow start),
/// `agent.list` shows it, `agent.retire` leaves its tombstone.
#[test]
fn spawn_free_creates_an_agent_that_lists_and_retires() {
    let sb = Sandbox::new();
    let fa = FreeAgents::new(&sb);
    std::fs::write(fa.bin.join("ready-delay"), "7").unwrap();
    let started = Instant::now();
    // A relative --cwd is taken against the caller's directory.
    let out = fa.run(
        &sb,
        &["spawn", "--mode", "free", "--cwd", "work", "--name", "ann"],
        true,
    );
    assert!(out.status.success(), "{:?} / log {}", out, sb.log());
    assert!(started.elapsed() >= Duration::from_secs(7));
    let agent = stdout_json(&out)["agent"].clone();
    assert_eq!(agent["state"], "ready", "{}", agent);
    assert_eq!(agent["mode"], "free", "{}", agent);
    assert_eq!(agent["name"], "ann", "{}", agent);
    assert_eq!(agent["generation"], 1, "{}", agent);
    let cwd = std::fs::canonicalize(&fa.cwd).unwrap();
    assert_eq!(
        agent["cwd"],
        cwd.display().to_string().as_str(),
        "{}",
        agent
    );
    let id = agent["id"].as_str().unwrap().to_string();
    let argv = std::fs::read_to_string(fa.bin.join("argv.log")).unwrap();
    assert_eq!(argv.lines().count(), 1, "{}", argv);

    let list = |all: bool| -> Vec<Value> {
        let params = format!(r#"{{"all":{}}}"#, all);
        let out = fa.run(&sb, &["api", "agent.list", &params], true);
        assert!(out.status.success(), "{:?}", out);
        stdout_json(&out)["result"]["agents"]
            .as_array()
            .unwrap()
            .clone()
    };
    let live = list(false);
    let row = live.iter().find(|a| a["id"] == id.as_str()).unwrap();
    assert_eq!(row["state"], "ready", "{}", row);

    let out = fa.run(
        &sb,
        &["api", "agent.retire", &format!(r#"{{"id":"{}"}}"#, id)],
        true,
    );
    assert!(out.status.success(), "{:?}", out);
    assert_eq!(stdout_json(&out)["result"]["agent"]["state"], "retired");
    assert!(!list(false).iter().any(|a| a["id"] == id.as_str()));
    let all = list(true);
    let tomb = all.iter().find(|a| a["id"] == id.as_str()).unwrap();
    assert_eq!(tomb["state"], "retired", "{}", tomb);
    assert!(fa.all_gone(), "a fake claude still runs");
}

/// Before C1a-35, an engine without `C9WATCH_EXPERIMENTAL_CREATE=1`
/// answers `experimental_disabled`, and nothing is spawned.
#[test]
fn spawn_free_without_the_switch_is_experimental_disabled() {
    let sb = Sandbox::new();
    let fa = FreeAgents::new(&sb);
    let out = fa.run(&sb, &["spawn", "--mode", "free", "--cwd", "work"], false);
    assert_eq!(out.status.code(), Some(1), "{:?}", out);
    let err = stderr_of(&out);
    assert!(err.contains("experimental_disabled"), "{err}");
    assert!(err.contains("C9WATCH_EXPERIMENTAL_CREATE=1"), "{err}");
    assert!(out.stdout.is_empty(), "{:?}", out);
    assert!(sb.pid().is_some(), "the engine was not auto-started");
    assert!(!fa.bin.join("argv.log").exists());
}

/// An agent (`C9_AGENT_ID` set) gets `not_authorized` without reaching
/// the engine, and a managed-only flag with `--mode free` is refused
/// before anything starts.
#[test]
fn spawn_free_refuses_agents_and_managed_flags_locally() {
    let sb = Sandbox::new();
    let fa = FreeAgents::new(&sb);
    let mut c = sb.cmd(&["spawn", "--mode", "free", "--cwd", "work"]);
    let out = c
        .env("PATH", &fa.path)
        .env("C9_AGENT_ID", "agt_x")
        .env("C9WATCH_EXPERIMENTAL_CREATE", "1")
        .current_dir(fa.cwd.parent().unwrap())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{:?}", out);
    let err = stderr_of(&out);
    assert!(err.contains("not_authorized"), "{err}");
    assert!(err.contains("agt_x"), "{err}");

    for flag in [
        &["--prompt", "hi"][..],
        &["--model", "x"],
        &["--permission-mode", "plan"],
        &["--add-dir", "/tmp"],
        &["--append-system-prompt", "x"],
    ] {
        let args = [&["spawn", "--mode", "free"][..], flag].concat();
        let out = fa.run(&sb, &args, true);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {:?}", out);
        let err = stderr_of(&out);
        assert!(err.contains("bad_args"), "{args:?}: {err}");
        assert!(err.contains(flag[0]), "{args:?}: {err}");
    }
    assert!(
        !sb.engine.exists(),
        "a refused spawn touched the engine home"
    );
    assert!(sb.pid().is_none());
}

/// An engine build without `pm-orchestration` has no legacy workers:
/// `spawn` (managed by default) fails and names `--mode free`.
#[cfg(not(feature = "pm-orchestration"))]
#[test]
fn spawn_managed_needs_the_pm_build() {
    let sb = Sandbox::new();
    let fa = FreeAgents::new(&sb);
    for args in [&["spawn"][..], &["spawn", "--mode", "managed"]] {
        let out = fa.run(&sb, args, true);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {:?}", out);
        let err = stderr_of(&out);
        assert!(err.contains("pm-orchestration"), "{args:?}: {err}");
        assert!(err.contains("--mode free"), "{args:?}: {err}");
    }
    assert!(!sb.engine.exists());
}
