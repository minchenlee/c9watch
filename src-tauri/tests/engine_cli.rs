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
