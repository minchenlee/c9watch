//! Contract tests for `c9watch engine`, `engine restart` and `api`, run
//! against the real binary in a temp HOME and a temp engine home. They never
//! touch `~/Library/Application Support/c9watch` or `~/.claude`.

#![cfg(all(unix, feature = "engine"))]

use c9watch_lib::engine::home::{lock_path, sock_path, ENGINE_HOME_ENV};
use c9watch_lib::engine::lock::{engine_lock_held, engine_pid};
use c9watch_lib::engine::peer::proc_entry;
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
        let socks = dir.path().join("socks");
        std::fs::create_dir(&socks).unwrap();
        std::fs::set_permissions(&socks, std::fs::Permissions::from_mode(0o700)).unwrap();
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
            .env("C9_CC_SOCK_DIR", self.home.parent().unwrap().join("socks"))
            .env(
                "C9_CC_SESSIONS_DIR",
                self.home.parent().unwrap().join("cc-sessions"),
            )
            .env_remove("C9_AGENT_ID")
            .env_remove("C9_ENGINE_SOCK")
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_RUNTIME_DIR")
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

/// Whether `pid` runs something. A zombie does not: a container whose PID 1
/// reaps nothing keeps a killed orphan as one.
fn alive(pid: libc::pid_t) -> bool {
    proc_entry(pid).is_some_and(|e| !e.zombie)
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

/// Exercise actual Linux resolution in separate CLI/engine processes,
/// without changing the test runner's environment or touching the real home.
#[cfg(target_os = "linux")]
#[test]
fn linux_default_home_auto_start_restart_and_runtime_fallback() {
    use c9watch_lib::engine_proto::home::{linux_sock_path, Channel};
    for runtime_value in [None, Some("relative"), Some("runtime")] {
        let mut sb = Sandbox::new();
        sb.engine = sb
            .home
            .join(".local/state/c9watch")
            .join(match Channel::current() {
                Channel::Prod => "engine",
                Channel::Dev => "engine-dev",
            });
        let runtime = sb.home.parent().unwrap().join("runtime");
        std::fs::create_dir(&runtime).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let run = |args: &[&str]| {
            let mut cmd = sb.cmd(args);
            cmd.env_remove(ENGINE_HOME_ENV);
            match runtime_value {
                Some("runtime") => {
                    cmd.env("XDG_RUNTIME_DIR", &runtime);
                }
                Some(value) => {
                    cmd.env("XDG_RUNTIME_DIR", value);
                }
                None => {}
            }
            cmd.output().unwrap()
        };
        let sock = linux_sock_path(
            Channel::current(),
            &sb.engine,
            runtime_value.map(|v| {
                if v == "runtime" {
                    runtime.as_os_str()
                } else {
                    std::ffi::OsStr::new(v)
                }
            }),
        );
        let out = run(&["api", "hello"]);
        assert!(out.status.success(), "{out:?} / log {}", sb.log());
        assert_eq!(mode(&sock), 0o600);
        assert_eq!(mode(sock.parent().unwrap()), 0o700);
        assert!(sb.engine.join("engine.db").exists());
        let old = sb.pid().unwrap();
        let out = run(&["engine", "restart"]);
        assert!(out.status.success(), "{out:?} / log {}", sb.log());
        assert_ne!(sb.pid(), Some(old));
        assert_eq!(mode(&sock), 0o600);
        assert!(run(&["api", "session.views"]).status.success());
        if runtime_value == Some("runtime") {
            assert!(!sb.engine.join("engine-v1.sock").exists());
        }
        drop(sb);
        assert!(!sock.exists(), "SIGTERM must remove the selected socket");
    }
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
/// `--resume`), shows CC's idle title and CC's layout (a rule, the input
/// box with its placeholder, a rule, two status lines) and runs until it
/// is killed. A `ready-delay` file next to it holds seconds to wait first,
/// as a slow CC start would; with a `hold` file the box shows a half-typed
/// draft, so no mail goes in.
///
/// Its input is a mail paste (C1b-M1-11): once `ESC[201~` ends the paste
/// the box shows its first line; a CR then submits it as CC would. The
/// prompt goes to `in-<agent id>.log` under a `--- gen <n>` line, the
/// UserPromptSubmit hook carries it, and a Stop hook ends the turn after
/// `linger <secs>` (from a body line, default 0.3 s). A body line
/// `forward to <to>` makes it mail `<to>` with the `c9watch` next to it.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
if [ "${FAKE_CC_SOCKET:-}" = 1 ] || [ -f "$(dirname "$0")/socket-enabled" ]; then exec /usr/bin/env python3 "$(dirname "$0")/fake-socket-cc.py" "$@"; fi
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
hook() { "$home/bin/c9watch-agent" hook "$1" --kind claude; }
printf '{"session_id":"%s","source":"%s","hook_event_name":"SessionStart"}' "$sid" "$source" \
  | hook session-start > "$dir/ctx-$C9_AGENT_GEN.out"
rule="────────────────────────────────────────"
draw() {
  printf '\033[2J\033[H%s\r\n\342\235\257 %s\r\n%s\r\n  auto mode on\r\n  1 agent\r\n' \
    "$rule" "$1" "$rule"
}
printf '\033]0;\342\234\263 Claude Code\007'
if [ -f "$dir/hold" ]; then draw "a half-typed draft"; else draw 'Try "fix lint errors"'; fi
esc=$(printf '\033'); cr=$(printf '\r')
stty -icanon -echo -icrnl min 0 time 2
buf=""
while :; do
  chunk=$(dd bs=4096 count=1 2>/dev/null; echo .)
  chunk=${chunk%.}
  if [ -z "$chunk" ]; then sleep 0.2; continue; fi
  buf="$buf$chunk"
  case "$buf" in
    *"$esc[201~"*"$cr"*) ;;
    *"$esc[201~"*) draw "$(printf '%s\n' "${buf#*"$esc[200~"}" | head -n 1)"; continue ;;
    *) continue ;;
  esac
  p=${buf#*"$esc[200~"}; p=${p%%"$esc[201~"*}; buf=""
  printf '\033]0;\342\227\220 Claude Code\007'
  draw ""
  printf '%s\n' "--- gen $C9_AGENT_GEN" "$p" >> "$dir/in-$C9_AGENT_ID.log"
  json=$(printf '%s\n' "$p" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' \
    | awk '{ printf "%s%s", (NR > 1 ? "\\n" : ""), $0 }')
  printf '{"session_id":"%s","hook_event_name":"UserPromptSubmit","prompt":"%s"}' "$sid" "$json" \
    | hook prompt-submit > /dev/null
  to=$(printf '%s\n' "$p" | sed -n 's/^forward to //p' | head -n 1)
  [ -z "$to" ] || "$dir/c9watch" send "$to" "forwarded by $C9_AGENT_ID" \
    >> "$dir/fwd-$C9_AGENT_ID.out" 2>&1
  linger=$(printf '%s\n' "$p" | sed -n 's/^linger //p' | head -n 1)
  sleep "${linger:-0.3}"
  printf '{"session_id":"%s","hook_event_name":"Stop"}' "$sid" | hook stop > /dev/null
  printf '\033]0;\342\234\263 Claude Code\007'
done
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
    let run = |args: &[&str]| -> Output { sb.cmd(args).env("PATH", &path).output().unwrap() };
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
  agents)
    [ -f "$(dirname "$0")/agents-delay" ] && sleep "$(cat "$(dirname "$0")/agents-delay")"
    cat "$(dirname "$0")/agents.json"; exit 0 ;;
esac
exit 1
"#;

/// One detected session: a `sleep` that `claude agents --json` reports,
/// in `<root>/work/proj`. `path` puts the stand-in first on PATH.
struct Detected {
    path: String,
    bin: PathBuf,
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
            bin,
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
    // The payload keeps the --direct names, with the engine's row under
    // `engine`; there are no diagnostics.
    assert!(listed.get("diagnostics").is_none(), "{}", listed);
    let s = &listed["sessions"][0];
    assert_eq!(s["id"], DETECT_SID);
    assert_eq!(s["sessionKey"], key.as_str());
    assert_eq!(s["status"], "Connecting");
    assert_eq!(s["sessionName"], "proj");
    assert!(
        s["projectPath"].as_str().unwrap().ends_with("work/proj"),
        "{}",
        s
    );
    let row = &s["engine"];
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

/// The first `list` after a cold start already has the session: the engine
/// answers once its first detection pass is written (reviews H1), even when
/// `claude agents --json` is slow.
#[test]
fn the_first_list_after_a_cold_start_has_the_session() {
    let sb = Sandbox::new();
    let d = Detected::new(&sb);
    std::fs::write(d.bin.join("agents-delay"), "1").unwrap();
    let out = d.run(&sb, &["list"]);
    assert!(out.status.success(), "{:?} / log {}", out, sb.log());
    let list = stdout_json(&out);
    assert_eq!(
        list["sessions"].as_array().map(Vec::len),
        Some(1),
        "{} / log {}",
        list,
        sb.log()
    );
    let out = d.run(&sb, &["engine", "restart"]);
    assert!(out.status.success(), "{:?}", out);
    let out = d.run(&sb, &["status"]);
    assert_eq!(stdout_json(&out)["total"], 1, "{:?}", out);
}

/// M-a1: `session.views` answers the GUI's session JSON, and a subscriber
/// sees the `session.view` that put it there and the `session.gone` when
/// the session ends.
#[test]
fn session_views_and_their_events_through_the_engine() {
    use std::io::{BufRead, BufReader, Write};
    let sb = Sandbox::new();
    let mut d = Detected::new(&sb);
    let cwd = sb.home.parent().unwrap().join("work/proj");
    let project: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let project_dir = sb.home.join(".claude/projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();
    let transcript = project_dir.join(format!("{DETECT_SID}.jsonl"));
    let message = |text: &str| {
        serde_json::json!({
            "type": "user", "sessionId": DETECT_SID, "uuid": uuid::Uuid::new_v4().to_string(),
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "message": {"role": "user", "content": text},
        })
        .to_string()
            + "\n"
    };
    std::fs::write(&transcript, message("before fixture change")).unwrap();
    engine_lists_the_session(&sb, &d);
    let key = format!("claudeCode:{}", DETECT_SID);

    let out = d.run(&sb, &["api", "session.views", "{}"]);
    assert!(out.status.success(), "{:?} / log {}", out, sb.log());
    let resp = stdout_json(&out);
    assert_eq!(resp["ok"], true, "{}", resp);
    let views = &resp["result"];
    let sessions = views["sessions"].as_array().expect("sessions");
    assert_eq!(sessions.len(), 1, "{}", views);
    assert_eq!(sessions[0]["id"], DETECT_SID);
    assert_eq!(sessions[0]["sessionKey"], key.as_str());
    assert_eq!(sessions[0]["latestMessage"], "before fixture change");
    let cursor = views["cursor"].as_str().expect("cursor").to_string();

    // Replay from the start of the epoch: the first tick's view is there.
    let (prefix, _) = cursor.rsplit_once(':').unwrap();
    let since = format!("{}:0", prefix);
    let mut sock = std::os::unix::net::UnixStream::connect(sock_path(&sb.engine)).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    for req in [
        serde_json::json!({"v": 1, "id": "h", "method": "hello", "params": {}}),
        serde_json::json!({"v": 1, "id": "s", "method": "events.subscribe",
            "params": {"since": since, "topics": ["session.*"]}}),
    ] {
        writeln!(sock, "{}", req).unwrap();
    }
    let mut lines = BufReader::new(sock.try_clone().unwrap()).lines();
    let mut next_event = |topic: &str| -> Value {
        loop {
            let line = lines
                .next()
                .unwrap_or_else(|| panic!("no {topic} / log {}", sb.log()))
                .unwrap();
            let f: Value = serde_json::from_str(&line).unwrap();
            if f.get("ok") == Some(&Value::Bool(false)) {
                panic!("error frame {f}");
            }
            if f["type"] == "event" && f["topic"] == topic {
                return f;
            }
        }
    };
    let view = next_event("session.view");
    assert_eq!(view["data"]["id"], DETECT_SID);
    assert_eq!(view["data"]["sessionKey"], key.as_str());
    assert!(view["subject"].as_str().unwrap().ends_with(&key));

    // A transcript change must reach the live subscriber, not just replay
    // the initial detection event. The same CLI read sees the changed view.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    file.write_all(message("after fixture transcript change").as_bytes())
        .unwrap();
    loop {
        let changed = next_event("session.view");
        if changed["data"]["latestMessage"] == "after fixture transcript change" {
            break;
        }
    }
    let out = d.run(&sb, &["api", "session.views"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        stdout_json(&out)["result"]["sessions"][0]["latestMessage"],
        "after fixture transcript change"
    );

    d.end_session();
    let gone = next_event("session.gone");
    assert_eq!(gone["data"]["sessionKey"], key.as_str());
    assert_eq!(gone["data"]["id"], DETECT_SID);
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
        .stderr(Stdio::piped())
        .spawn()
        .map(Reap)
        .unwrap();
    let waited = lines_of(&mut wait);
    let started: Value =
        serde_json::from_str(&lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
    assert_eq!(started["event"], "started");
    assert_eq!(started["sessionKey"], key.as_str());
    assert_eq!(started["session"]["status"], "Connecting");
    assert_eq!(started["session"]["id"], DETECT_SID);
    assert_eq!(started["session"]["engine"]["status"], "connecting");
    // Let `wait` take its snapshot before the session ends. The engine is
    // warm, so this is one local round trip; the margin is for a loaded CI
    // host, and a miss shows `wait`'s stderr below.
    std::thread::sleep(Duration::from_secs(5));

    d.end_session();
    let stopped: Value =
        serde_json::from_str(&lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
    assert_eq!(stopped["event"], "stopped", "{}", stopped);
    assert_eq!(stopped["sessionKey"], key.as_str());
    assert_eq!(stopped["previousStatus"], "Connecting");

    let status = wait.wait_timeout(Duration::from_secs(30)).unwrap();
    let mut err = String::new();
    if let Some(mut e) = wait.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut err);
    }
    assert_eq!(status.and_then(|s| s.code()), Some(0), "wait stderr: {err}");
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
    // Exactly one line after the restart: no `stopped` (or `started`) for
    // the unchanged session came late and stood in for this one.
    let extra: Vec<String> =
        std::iter::from_fn(|| lines.recv_timeout(Duration::from_secs(3)).ok()).collect();
    assert!(extra.is_empty(), "extra lines: {extra:?}");
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

/// A fake `claude` (FAKE_CLAUDE) first on PATH, with this `c9watch` next
/// to it, and a cwd for free agents.
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
        std::fs::write(
            bin.join("fake-socket-cc.py"),
            include_str!("fixtures/fake_socket_cc.py"),
        )
        .unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(BIN, bin.join("c9watch")).unwrap();
        let cwd = root.join("work");
        std::fs::create_dir(&cwd).unwrap();
        FreeAgents {
            path: format!("{}:/usr/bin:/bin", bin.display()),
            bin,
            cwd,
        }
    }

    /// `c9watch <args>` in `<root>`.
    fn cmd(&self, sb: &Sandbox, args: &[&str]) -> Command {
        let mut c = sb.cmd(args);
        c.env("PATH", &self.path)
            .current_dir(self.cwd.parent().unwrap());
        c
    }

    fn run(&self, sb: &Sandbox, args: &[&str]) -> Output {
        self.cmd(sb, args).output().unwrap()
    }

    /// `spawn --mode free --name <name>` in `work` (live agents share one
    /// cwd); the ready agent's id.
    fn spawn(&self, sb: &Sandbox, name: &str) -> String {
        let out = self.run(
            sb,
            &["spawn", "--mode", "free", "--cwd", "work", "--name", name],
        );
        assert!(out.status.success(), "{:?} / log {}", out, sb.log());
        let agent = stdout_json(&out)["agent"].clone();
        assert_eq!(agent["state"], "ready", "{}", agent);
        agent["id"].as_str().unwrap().to_string()
    }

    /// The prompts agent `id` was given, as its `in-<id>.log` has them.
    fn prompts(&self, id: &str) -> String {
        std::fs::read_to_string(self.bin.join(format!("in-{}.log", id))).unwrap_or_default()
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
        let out = fa.run(&sb, &["api", "agent.list", &params]);
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
    );
    assert!(out.status.success(), "{:?}", out);
    assert_eq!(stdout_json(&out)["result"]["agent"]["state"], "retired");
    assert!(!list(false).iter().any(|a| a["id"] == id.as_str()));
    let all = list(true);
    let tomb = all.iter().find(|a| a["id"] == id.as_str()).unwrap();
    assert_eq!(tomb["state"], "retired", "{}", tomb);
    assert!(fa.all_gone(), "a fake claude still runs");
}

/// An agent (`C9_AGENT_ID` set) gets `not_allowed` without reaching
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
        .current_dir(fa.cwd.parent().unwrap())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{:?}", out);
    let err = stderr_of(&out);
    assert!(err.contains("not_allowed"), "{err}");
    assert!(err.contains("agt_x"), "{err}");

    for flag in [
        &["--prompt", "hi"][..],
        &["--model", "x"],
        &["--permission-mode", "plan"],
        // The managed default, given by hand, is refused too.
        &["--permission-mode", "bypassPermissions"],
        &["--add-dir", "/tmp"],
        &["--append-system-prompt", "x"],
    ] {
        let args = [&["spawn", "--mode", "free"][..], flag].concat();
        let out = fa.run(&sb, &args);
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
        let out = fa.run(&sb, args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {:?}", out);
        let err = stderr_of(&out);
        assert!(err.contains("pm-orchestration"), "{args:?}: {err}");
        assert!(err.contains("--mode free"), "{args:?}: {err}");
    }
    assert!(!sb.engine.exists());
}

// ── C1b-M1-11: send mail through the engine ─────────────────────────

/// The `message_status` rows of `msg_id`, oldest first: (state, detail).
fn mail_states(sb: &Sandbox, msg_id: &str) -> Vec<(String, Option<String>)> {
    use c9watch_lib::engine::db::db_path;
    let conn = rusqlite::Connection::open(db_path(&sb.engine)).unwrap();
    let mut st = conn
        .prepare("SELECT state, detail FROM message_status WHERE msg_id = ?1 ORDER BY seq")
        .unwrap();
    let rows = st
        .query_map([msg_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    rows.map(Result::unwrap).collect()
}

/// Wait until `msg_id` is in `state`; its rows.
fn wait_mail(sb: &Sandbox, msg_id: &str, state: &str) -> Vec<(String, Option<String>)> {
    let mut rows = Vec::new();
    let reached = wait_for(Duration::from_secs(30), || {
        rows = mail_states(sb, msg_id);
        rows.last().is_some_and(|(s, _)| s == state)
    });
    assert!(reached, "{msg_id} not {state}: {rows:?} / log {}", sb.log());
    rows
}

/// `c9watch send` that succeeded: its `msg`.
fn sent(out: &Output) -> Value {
    assert!(out.status.success(), "{:?}", out);
    let msg = stdout_json(out)["msg"].clone();
    assert!(
        msg["msgId"].as_str().unwrap().starts_with("msg_"),
        "{}",
        msg
    );
    msg
}

/// The `error` of the JSON line a failed engine command prints on stderr.
fn stderr_error(out: &Output) -> Value {
    let err = stderr_of(out);
    let line = err
        .lines()
        .rev()
        .find(|l| l.starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON on stderr: {err}"));
    serde_json::from_str::<Value>(line).unwrap()["error"].clone()
}

fn states_only(rows: &[(String, Option<String>)]) -> Vec<&str> {
    rows.iter().map(|(s, _)| s.as_str()).collect()
}

/// The terminal (L) mails a free agent by name. The agent's input box gets
/// the fence (header with the token, provenance, body, end line) as one
/// prompt, and the prompt hook confirms it: `queued → delivering →
/// delivered(prompt_hook)`.
#[test]
fn send_from_the_terminal_reaches_a_free_agent() {
    let sb = Sandbox::new();
    let fa = FreeAgents::new(&sb);
    let ann = fa.spawn(&sb, "ann");
    let msg = sent(&fa.run(&sb, &["send", "ann", "hello\nworld"]));
    assert_eq!(msg["state"], "queued", "{}", msg);
    assert_eq!(msg["to"]["id"], ann.as_str(), "{}", msg);
    assert_eq!(msg["replayed"], false, "{}", msg);
    let id = msg["msgId"].as_str().unwrap();
    let rows = wait_mail(&sb, id, "delivered");
    assert_eq!(
        states_only(&rows),
        ["queued", "delivering", "delivered"],
        "{rows:?}"
    );
    assert_eq!(rows[2].1.as_deref(), Some("prompt_hook"), "{rows:?}");

    let prompts = fa.prompts(&ann);
    let header = format!("[c9watch mail {} \u{b7} tok ", id);
    let fence = prompts
        .split("--- gen 1\n")
        .nth(1)
        .unwrap_or_else(|| panic!("no prompt: {prompts}"));
    assert!(fence.starts_with(&header), "{prompts}");
    assert!(
        fence.contains("\n[from terminal (local-unverified) for "),
        "{prompts}"
    );
    assert!(
        fence.contains(&format!("\nhello\nworld\n[end mail {}]", id)),
        "{prompts}"
    );
    // One prompt: the paste went in once and was submitted once.
    assert_eq!(prompts.matches("--- gen").count(), 1, "{prompts}");

    let out = fa.run(
        &sb,
        &["api", "agent.retire", &format!(r#"{{"id":"{}"}}"#, ann)],
    );
    assert!(out.status.success(), "{:?}", out);
    assert!(fa.all_gone(), "a fake claude still runs");
}

/// `send --wait` returns once the message is delivered and the agent's
/// turn after it has ended, with a second line naming that turn end.
#[test]
fn send_wait_returns_after_the_reply_turn() {
    let sb = Sandbox::new();
    let fa = FreeAgents::new(&sb);
    let ann = fa.spawn(&sb, "ann");
    let out = fa.run(
        &sb,
        &["send", "ann", "linger 2", "--wait", "--timeout", "60"],
    );
    assert!(out.status.success(), "{:?} / log {}", out, sb.log());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    let id = lines[0]["msg"]["msgId"].as_str().unwrap();
    let done = &lines[1];
    assert_eq!(done["msg"]["msgId"], id, "{done}");
    assert_eq!(done["msg"]["state"], "delivered", "{done}");
    assert_eq!(done["msg"]["detail"], "prompt_hook", "{done}");
    assert!(
        cursor_seq(&done["turnEnd"]) > cursor_seq(&lines[0]["msg"]["cursor"]),
        "{stdout}"
    );
    // The turn that ended is the one the message started: its Stop came
    // after the prompt, once the linger was over.
    assert!(fa.prompts(&ann).contains(id));
}

/// A name no agent has is `unknown_address`, with the nearest open name.
#[test]
fn send_to_an_unknown_name_suggests_the_nearest() {
    let sb = Sandbox::new();
    let fa = FreeAgents::new(&sb);
    fa.spawn(&sb, "ann");
    let out = fa.run(&sb, &["send", "anne", "hi"]);
    assert_eq!(out.status.code(), Some(1), "{:?}", out);
    let err = stderr_error(&out);
    assert_eq!(err["code"], "unknown_address", "{err}");
    assert_eq!(err["data"]["nearest"], "ann", "{err}");
}

/// A detected session (`control='none'`) takes no mail: `not_controlled`,
/// and no message row.
#[test]
fn send_to_an_observed_session_is_not_controlled() {
    use c9watch_lib::engine::db::db_path;
    let sb = Sandbox::new();
    let d = Detected::new(&sb);
    let listed = engine_lists_the_session(&sb, &d);
    let id = listed["sessions"][0]["engine"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let out = d.run(&sb, &["send", &id, "hi"]);
    assert_eq!(out.status.code(), Some(1), "{:?}", out);
    let err = stderr_error(&out);
    assert_eq!(err["code"], "not_controlled", "{err}");
    let conn = rusqlite::Connection::open(db_path(&sb.engine)).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

/// E12: mail queued while the agent's box is busy (a half-typed draft)
/// stays queued through `engine restart` and goes to generation 2 after
/// the resume.
#[test]
fn mail_queued_across_an_engine_restart_is_delivered_after_resume() {
    let sb = Sandbox::new();
    let fa = FreeAgents::new(&sb);
    std::fs::write(fa.bin.join("hold"), "").unwrap();
    let ann = fa.spawn(&sb, "ann");
    let msg = sent(&fa.run(&sb, &["send", "ann", "after the restart"]));
    let id = msg["msgId"].as_str().unwrap();
    // A box with text in it takes no paste.
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(states_only(&mail_states(&sb, id)), ["queued"]);
    assert_eq!(fa.prompts(&ann), "");

    std::fs::remove_file(fa.bin.join("hold")).unwrap();
    let out = fa.run(&sb, &["engine", "restart"]);
    assert!(out.status.success(), "{:?} / log {}", out, sb.log());
    let rows = wait_mail(&sb, id, "delivered");
    assert_eq!(rows.last().unwrap().1.as_deref(), Some("prompt_hook"));
    assert!(!rows.iter().any(|(s, _)| s == "uncertain"), "{rows:?}");
    let prompts = fa.prompts(&ann);
    assert!(prompts.starts_with("--- gen 2\n"), "{prompts}");
    assert!(prompts.contains(id), "{prompts}");
}

/// An agent mails another agent with `c9watch send` (no `--wait`) from
/// inside its turn: the engine stamps it with the sender agent, and the
/// second agent gets it.
#[test]
fn an_agent_can_mail_another_agent() {
    let sb = Sandbox::new();
    let fa = FreeAgents::new(&sb);
    let ann = fa.spawn(&sb, "ann");
    let bob = fa.spawn(&sb, "bob");
    let msg = sent(&fa.run(&sb, &["send", "ann", "forward to bob"]));
    wait_mail(&sb, msg["msgId"].as_str().unwrap(), "delivered");

    let mut prompts = String::new();
    let got = wait_for(Duration::from_secs(30), || {
        prompts = fa.prompts(&bob);
        prompts.contains(&format!("forwarded by {}", ann))
    });
    let fwd = std::fs::read_to_string(fa.bin.join(format!("fwd-{}.out", ann)));
    assert!(got, "{prompts} / send: {fwd:?} / log {}", sb.log());
    assert!(
        prompts.contains(&format!("/{} (free) for ", ann)),
        "{prompts}"
    );
    let fwd: Value = serde_json::from_str(fwd.unwrap().trim()).unwrap();
    let id = fwd["msg"]["msgId"].as_str().unwrap();
    assert_eq!(fwd["msg"]["to"]["id"], bob.as_str(), "{fwd}");
    let rows = wait_mail(&sb, id, "delivered");
    assert_eq!(rows.last().unwrap().1.as_deref(), Some("prompt_hook"));
}

/// X5, the mail half (Q1=A): a send during the drain is taken and stays
/// `queued`; the draining engine writes nothing to the agent, and the
/// resumed generation gets it.
#[test]
fn no_mail_is_delivered_while_draining() {
    let sb = Sandbox::new();
    let fa = FreeAgents::new(&sb);
    let ann = fa.spawn(&sb, "ann");
    // A turn that lasts, so the drain waits for it.
    let first = sent(&fa.run(&sb, &["send", "ann", "linger 6"]));
    let first = first["msgId"].as_str().unwrap().to_string();
    assert!(
        wait_for(Duration::from_secs(30), || fa
            .prompts(&ann)
            .contains(&first)),
        "log {}",
        sb.log()
    );
    let mut restart = fa
        .cmd(&sb, &["engine", "restart"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(Reap)
        .unwrap();
    assert!(
        wait_for(Duration::from_secs(10), || sb
            .log()
            .contains("SIGTERM: draining")),
        "log {}",
        sb.log()
    );
    let msg = sent(&fa.run(&sb, &["send", "ann", "during the drain"]));
    let id = msg["msgId"].as_str().unwrap();
    assert_eq!(msg["state"], "queued", "{}", msg);
    assert!(restart.wait().unwrap().success(), "log {}", sb.log());

    let rows = wait_mail(&sb, id, "delivered");
    assert_eq!(
        states_only(&rows),
        ["queued", "delivering", "delivered"],
        "{rows:?}"
    );
    let prompts = fa.prompts(&ann);
    let (gen1, gen2) = prompts
        .split_once("--- gen 2\n")
        .unwrap_or_else(|| panic!("no generation 2 prompt: {prompts}"));
    assert!(gen1.contains(&first), "{prompts}");
    assert!(!gen1.contains(id), "delivered while draining: {prompts}");
    assert!(gen2.contains(id), "{prompts}");
}

// ── C1a exit criteria X2, X9, X15 ───────────────────────────────────

/// The `seq` of a `<machine>:<epoch>:<seq>` cursor.
fn cursor_seq(cursor: &Value) -> i64 {
    let c = cursor
        .as_str()
        .unwrap_or_else(|| panic!("no cursor: {cursor}"));
    c.rsplit(':').next().unwrap().parse().unwrap()
}

/// X2: a subscriber process stopped with SIGSTOP while the engine writes a
/// burst of events gets every one of them after SIGCONT, with contiguous
/// seqs. The exit criterion stops it for 10 minutes; the stop here lasts
/// seconds. What a long stop adds is a full socket buffer and a full write
/// queue (64 frames), and the burst (~160 events of ~800 bytes from 40
/// free agents created and retired) fills both, so the stream parks as it
/// would over 10 minutes. `nc -U` is the subscriber: a plain process the
/// test can stop.
#[test]
fn a_stopped_subscriber_gets_every_event_after_it_continues() {
    let sb = Sandbox::new();
    let fa = FreeAgents::new(&sb);
    assert!(fa.run(&sb, &["api", "hello"]).status.success());
    let mut nc = Command::new("nc")
        .arg("-U")
        .arg(sock_path(&sb.engine))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map(Reap)
        .expect("nc is on PATH");
    {
        use std::io::Write;
        let stdin = nc.stdin.as_mut().unwrap();
        stdin
            .write_all(
                b"{\"v\":1,\"id\":\"h\",\"method\":\"hello\",\"params\":{}}\n\
                  {\"v\":1,\"id\":\"s\",\"method\":\"events.subscribe\",\"params\":{}}\n",
            )
            .unwrap();
    }
    let lines = lines_of(&mut nc);
    let next = |d: Duration| -> Option<Value> {
        let line = lines.recv_timeout(d).ok()?;
        Some(serde_json::from_str(&line).unwrap())
    };
    let hello = next(Duration::from_secs(10)).expect("hello answers");
    assert_eq!(hello["ok"], true, "{hello}");
    let sub = next(Duration::from_secs(10)).expect("subscribe answers");
    assert_eq!(sub["id"], "s", "{sub}");
    assert_eq!(sub["ok"], true, "{sub}");
    let start = cursor_seq(&sub["result"]["cursor"]);

    let pid = nc.id() as libc::pid_t;
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    // Each step runs 20 CLIs at once: 5 rounds of `spawn`, then a retire of
    // every agent. That is more than a Linux socket buffer (~208 KB) plus
    // the 64-frame write queue, so the stream parks.
    let all = |args: Vec<Vec<String>>| -> Vec<Output> {
        let children: Vec<_> = args
            .iter()
            .map(|a| {
                let a: Vec<&str> = a.iter().map(String::as_str).collect();
                fa.cmd(&sb, &a)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        children
            .into_iter()
            .map(|c| c.wait_with_output().unwrap())
            .collect()
    };
    let spawn: Vec<String> = ["spawn", "--mode", "free", "--cwd", "work"]
        .map(String::from)
        .into();
    let mut ids = Vec::new();
    for _ in 0..5 {
        for out in all(vec![spawn.clone(); 20]) {
            assert!(out.status.success(), "{:?} / log {}", out, sb.log());
            ids.push(
                stdout_json(&out)["agent"]["id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
    }
    for chunk in ids.chunks(20) {
        let retire = chunk
            .iter()
            .map(|id| {
                let params = format!(r#"{{"id":"{}"}}"#, id);
                vec!["api".into(), "agent.retire".into(), params]
            })
            .collect();
        for out in all(retire) {
            assert!(out.status.success(), "{:?} / log {}", out, sb.log());
        }
    }
    let out = fa.run(&sb, &["api", "snapshot.get"]);
    assert!(out.status.success(), "{:?}", out);
    let head = cursor_seq(&stdout_json(&out)["result"]["cursor"]);
    assert!(head - start >= 375, "only {} events", head - start);
    unsafe { libc::kill(pid, libc::SIGCONT) };

    let (mut seqs, mut bytes) = (Vec::new(), 0);
    while seqs.last() != Some(&head) {
        let line = lines
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|_| panic!("stream stalled after seq {:?}", seqs.last()));
        bytes += line.len() + 1;
        let f: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(f["type"], "event", "{f}");
        assert_eq!(f["sub"], "s", "{f}");
        seqs.push(cursor_seq(&f["cursor"]));
    }
    assert_eq!(seqs, (start + 1..=head).collect::<Vec<_>>());
    // The backlog was larger than what the kernel and the queue hold.
    assert!(bytes > 256 * 1024, "only {bytes} bytes");
    assert!(fa.all_gone(), "a fake claude still runs");
}

/// Every `*.sock` under `dir`, at any depth.
fn socks_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for e in entries.flatten() {
        let p = e.path();
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            found.extend(socks_under(&p));
        } else if p.extension().is_some_and(|x| x == "sock") {
            found.push(p);
        }
    }
    found
}

/// X15: once its clients are closed (a `watch` here), a foreground engine
/// stopped with SIGTERM, or with SIGINT, exits and leaves no socket in its
/// home. This is measured after the stop, not after a restart.
#[test]
fn a_stopped_engine_leaves_no_socket_once_its_clients_close() {
    use wait_timeout::ChildExt;
    for (name, sig) in [("SIGTERM", libc::SIGTERM), ("SIGINT", libc::SIGINT)] {
        let sb = Sandbox::new();
        let d = Detected::new(&sb);
        let mut engine = d
            .cmd(&sb, &["engine"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(Reap)
            .unwrap();
        let sock = sock_path(&sb.engine);
        assert!(
            wait_for(Duration::from_secs(10), || sock.exists()),
            "{name}: log {}",
            sb.log()
        );

        let mut watch = d
            .cmd(&sb, &["watch", "--compact"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map(Reap)
            .unwrap();
        let lines = lines_of(&mut watch);
        let first: Value = serde_json::from_str(
            &lines
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_else(|_| panic!("{name}: no first frame / log {}", sb.log())),
        )
        .unwrap();
        assert_eq!(first["event"], "started", "{name}: {first}");
        watch.kill().unwrap();
        watch.wait().unwrap();
        assert_eq!(sb.pid(), Some(engine.id() as libc::pid_t), "{name}");

        unsafe { libc::kill(engine.id() as libc::pid_t, sig) };
        let status = engine
            .wait_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap_or_else(|| panic!("the engine did not exit on {name}"));
        assert!(status.success(), "{name}: {:?}", status);
        let socks = socks_under(&sb.engine);
        assert!(socks.is_empty(), "{name}: {socks:?}");
        assert!(sb.pid().is_none(), "{name}");
    }
}

/// X9: a real engine SIGKILLed right after each create phase (the
/// `live-test` failpoint, `engine::live_test::crash_after`) is reconciled
/// by the next boot: every op has `done_at`, and the agent it left running
/// is killed. The fake agent ignores SIGHUP, so the engine's death (which
/// closes the PTY) does not end it and only boot reconcile can. After
/// phase `bound` the agent was `ready`, so the next engine resumes it.
#[cfg(feature = "live-test")]
#[test]
fn an_engine_killed_after_each_create_phase_is_reconciled_on_boot() {
    use c9watch_lib::engine::db::db_path;
    use c9watch_lib::engine::live_test::CRASH_AFTER_ENV;
    use std::os::unix::process::ExitStatusExt;
    use wait_timeout::ChildExt;
    for phase in ["row", "pty", "bound"] {
        let sb = Sandbox::new();
        let fa = FreeAgents::new(&sb);
        let hardy = FAKE_CLAUDE.replacen("#!/bin/sh\n", "#!/bin/sh\ntrap '' HUP\n", 1);
        std::fs::write(fa.bin.join("claude"), hardy).unwrap();
        let mut engine = fa
            .cmd(&sb, &["engine"])
            .env(CRASH_AFTER_ENV, phase)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(Reap)
            .unwrap();
        let sock = sock_path(&sb.engine);
        assert!(
            wait_for(Duration::from_secs(10), || sock.exists()),
            "{phase}: log {}",
            sb.log()
        );
        let out = fa.run(&sb, &["spawn", "--mode", "free", "--cwd", "work"]);
        assert!(!out.status.success(), "{phase}: {:?}", out);
        let status = engine
            .wait_timeout(Duration::from_secs(30))
            .unwrap()
            .unwrap_or_else(|| panic!("{phase}: the engine did not crash"));
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{phase}");

        let ops = || -> Vec<(String, bool, Value)> {
            let conn = rusqlite::Connection::open(db_path(&sb.engine)).unwrap();
            let mut st = conn
                .prepare("SELECT phase, done_at IS NOT NULL, payload FROM ops ORDER BY created_at")
                .unwrap();
            let rows = st
                .query_map([], |r| {
                    let payload: String = r.get(2)?;
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        serde_json::from_str(&payload).unwrap(),
                    ))
                })
                .unwrap();
            rows.map(Result::unwrap).collect()
        };
        let before = ops();
        assert_eq!(before.len(), 1, "{phase}: {before:?}");
        assert_eq!(before[0].0, phase, "{phase}: {before:?}");
        // `bound` finishes the op in its own transaction, so only `row` and
        // `pty` leave it open for boot; at `bound` boot only kills the
        // orphan.
        assert_eq!(before[0].1, phase == "bound", "{phase}: {before:?}");
        // The PTY child the dead engine left, from phase `pty` on.
        let orphan = before[0].2["pid"].as_i64().map(|p| p as libc::pid_t);
        assert_eq!(orphan.is_some(), phase != "row", "{phase}: {before:?}");
        // It ignores SIGHUP: kill it if the test fails before boot does.
        struct KillOnPanic(Option<libc::pid_t>);
        impl Drop for KillOnPanic {
            fn drop(&mut self) {
                if let (Some(pid), true) = (self.0, std::thread::panicking()) {
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
        }
        let _guard = KillOnPanic(orphan);
        if let Some(pid) = orphan {
            assert!(alive(pid), "{phase}: the agent died with the engine");
        }

        // The next engine, auto-started.
        let out = fa.run(&sb, &["api", "hello"]);
        assert!(
            out.status.success(),
            "{phase}: {:?} / log {}",
            out,
            sb.log()
        );
        let done = wait_for(Duration::from_secs(10), || ops().iter().all(|o| o.1));
        assert!(done, "{phase}: {:?} / log {}", ops(), sb.log());
        if let Some(pid) = orphan {
            assert!(
                wait_for(Duration::from_secs(10), || !alive(pid)),
                "{phase}: orphan {pid} still runs"
            );
        }
        // Only phase `bound` left a live (now `interrupted`) agent: it is
        // resumed as generation 2, then retired here.
        let out = fa.run(&sb, &["api", "agent.list", r#"{"all":false}"#]);
        assert!(out.status.success(), "{phase}: {:?}", out);
        let live = stdout_json(&out)["result"]["agents"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(
            live.len(),
            usize::from(phase == "bound"),
            "{phase}: {live:?}"
        );
        for a in &live {
            assert_eq!(a["state"], "ready", "{phase}: {a}");
            assert_eq!(a["generation"], 2, "{phase}: {a}");
            let params = format!(r#"{{"id":{}}}"#, a["id"]);
            let out = fa.run(&sb, &["api", "agent.retire", &params]);
            assert!(out.status.success(), "{phase}: {:?}", out);
        }
        assert!(fa.all_gone(), "{phase}: a fake claude still runs");
    }
}

// C1b-M2 synthetic CC socket acceptance. The fake is a real PTY child,
// runs no model, and writes an explicitly synthetic JSONL transcript.
fn socket_agents() -> (Sandbox, FreeAgents) {
    let sb = Sandbox::new();
    let f = FreeAgents::new(&sb);
    std::fs::write(f.bin.join("socket-enabled"), "").unwrap();
    let out = f
        .cmd(&sb, &["list"])
        .env("FAKE_CC_SOCKET", "1")
        .env("C9_MAIL_SOCKET", "on")
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?} {}", out, sb.log());
    (sb, f)
}
fn socket_spawn(sb: &Sandbox, f: &FreeAgents, name: &str) -> String {
    let out = f
        .cmd(
            sb,
            &["spawn", "--mode", "free", "--cwd", "work", "--name", name],
        )
        .env("FAKE_CC_SOCKET", "1")
        .env("C9_MAIL_SOCKET", "on")
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?} {}", out, sb.log());
    stdout_json(&out)["agent"]["id"].as_str().unwrap().into()
}
fn ledger(sb: &Sandbox) -> rusqlite::Connection {
    rusqlite::Connection::open(sb.engine.join("engine.db")).unwrap()
}
fn socket_wait(sb: &Sandbox, sql: &str, expected: i64) {
    assert!(
        wait_for(Duration::from_secs(15), || ledger(sb)
            .query_row(sql, [], |r| r.get::<_, i64>(0))
            .ok()
            == Some(expected)),
        "{} != {}; log {}",
        sql,
        expected,
        sb.log()
    );
}
fn send_body(sb: &Sandbox, f: &FreeAgents, to: &str, body: &str) -> String {
    let out = f.run(sb, &["send", to, body]);
    assert!(out.status.success(), "{:?} {}", out, sb.log());
    stdout_json(&out)["msg"]["msgId"].as_str().unwrap().into()
}
#[test]
fn agent_to_agent_mail_goes_by_socket_and_a_cli_reply_links_to_the_original() {
    let (sb, f) = socket_agents();
    let a = socket_spawn(&sb, &f, "A");
    let b = socket_spawn(&sb, &f, "B");
    send_body(&sb, &f, &a, &format!("forward to {}", b));
    socket_wait(&sb,"SELECT count(*) FROM messages WHERE origin_kind='agent' AND state='delivered' AND via='socket'",1);
    let c = ledger(&sb);
    let original: String = c
        .query_row(
            "SELECT id FROM messages WHERE origin_kind='agent'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    send_body(&sb, &f, &b, &format!("reply to {} about {}", a, original));
    socket_wait(
        &sb,
        "SELECT count(*) FROM messages WHERE hop=1 AND state='delivered' AND via='socket'",
        1,
    );
    assert_eq!(
        c.query_row("SELECT reply_to FROM messages WHERE hop=1", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        original
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM messages WHERE origin_kind='local-unverified' AND via='pty'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    let out = f.run(&sb, &["mail", "list", "--pretty"]);
    assert!(out.status.success(), "{:?}", out);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Thread msg_"));
    assert!(text.contains("[peer]"));
    assert!(text.contains("[paste]"));
    assert!(text.contains("re:"));
}
#[test]
fn a_sendmessage_reply_is_audited_and_the_replier_is_told_to_use_the_cli() {
    let (sb, f) = socket_agents();
    let a = socket_spawn(&sb, &f, "A");
    let b = socket_spawn(&sb, &f, "B");
    let c = ledger(&sb);
    send_body(
        &sb,
        &f,
        &a,
        &format!("forward to {} with use SendMessage", b),
    );
    socket_wait(
        &sb,
        "SELECT count(*) FROM audit WHERE method='cc.inbound' AND reason='use_cli'",
        2,
    );
    socket_wait(
        &sb,
        "SELECT count(*) FROM messages WHERE kind='system' AND state='delivered'",
        1,
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM messages WHERE kind='system'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert!(f.prompts(&b).contains("Use `c9watch send"));
}
#[test]
fn held_mail_shows_in_msg_state_and_waits_for_evidence() {
    let (sb, f) = socket_agents();
    let a = socket_spawn(&sb, &f, "A");
    let b = socket_spawn(&sb, &f, "B");
    std::fs::write(f.bin.join(format!("socket-mode-{}", b)), "held").unwrap();
    send_body(&sb, &f, &a, &format!("forward to {}", b));
    socket_wait(
        &sb,
        "SELECT count(*) FROM message_status WHERE state='delivering' AND detail LIKE 'held:%'",
        1,
    );
    assert_eq!(
        ledger(&sb)
            .query_row(
                "SELECT count(*) FROM messages WHERE via='socket' AND state='delivered'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let out = f.run(&sb, &["mail", "list", "--pretty"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("held:"));
    std::fs::write(f.bin.join(format!("release-{}", b)), "").unwrap();
    socket_wait(
        &sb,
        "SELECT count(*) FROM messages WHERE via='socket' AND state='delivered'",
        1,
    );
}
#[test]
fn without_peer_protocol_mail_goes_by_paste() {
    let (sb, f) = socket_agents();
    std::fs::write(f.bin.join("no-peer-protocol"), "").unwrap();
    let a = socket_spawn(&sb, &f, "A");
    let b = socket_spawn(&sb, &f, "B");
    send_body(&sb, &f, &a, &format!("forward to {}", b));
    socket_wait(&sb,"SELECT count(*) FROM messages WHERE origin_kind='agent' AND via='pty' AND state='delivered'",1);
}
#[test]
fn cancel_and_pause_work_from_the_cli_and_human_mail_continues() {
    let (sb, f) = socket_agents();
    let a = socket_spawn(&sb, &f, "A");
    let b = socket_spawn(&sb, &f, "B");
    assert!(f.run(&sb, &["mail", "pause", &b]).status.success());
    send_body(&sb, &f, &a, &format!("forward to {}", b));
    socket_wait(
        &sb,
        "SELECT count(*) FROM messages WHERE origin_kind='agent' AND state='queued'",
        1,
    );
    let id: String = ledger(&sb)
        .query_row(
            "SELECT id FROM messages WHERE origin_kind='agent'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let human = send_body(&sb, &f, &b, "human still reaches B");
    socket_wait(
        &sb,
        &format!(
            "SELECT count(*) FROM messages WHERE id='{}' AND state='delivered'",
            human
        ),
        1,
    );
    assert!(f.run(&sb, &["mail", "cancel", &id]).status.success());
    assert!(f.run(&sb, &["mail", "resume", &b]).status.success());
    socket_wait(
        &sb,
        "SELECT count(*) FROM messages WHERE kind='system' AND state='delivered'",
        1,
    );
    let out = f.run(&sb, &["mail", "read", &id]);
    assert!(out.status.success());
    assert_eq!(stdout_json(&out)["state"], "cancelled");
}
#[test]
fn the_engine_socket_is_bound_at_boot_and_removed_on_shutdown() {
    let (sb, _f) = socket_agents();
    let sockdir = sb.home.parent().unwrap().join("socks");
    let sockets: Vec<_> = std::fs::read_dir(&sockdir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(sockets.len(), 1);
    assert!(sockets[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("c9w-"));
    let pid = sb.pid().unwrap();
    unsafe { libc::kill(pid, libc::SIGTERM) };
    assert!(wait_for(Duration::from_secs(10), || !sockets[0].exists()));
}

fn socket_wait_long(sb: &Sandbox, sql: &str, expected: i64, secs: u64) {
    assert!(
        wait_for(Duration::from_secs(secs), || ledger(sb)
            .query_row(sql, [], |r| r.get::<_, i64>(0))
            .ok()
            == Some(expected)),
        "{}; log {}",
        sql,
        sb.log()
    );
}
#[test]
fn an_uncertain_mail_sends_one_delayed_system_mail() {
    let (sb, f) = socket_agents();
    let a = socket_spawn(&sb, &f, "A");
    let b = socket_spawn(&sb, &f, "B");
    std::fs::write(f.bin.join(format!("socket-mode-{}", b)), "no-evidence").unwrap();
    send_body(&sb, &f, &a, &format!("forward to {}", b));
    socket_wait_long(
        &sb,
        "SELECT count(*) FROM messages WHERE origin_kind='agent' AND state='uncertain'",
        1,
        45,
    );
    let c = ledger(&sb);
    assert_eq!(c.query_row("SELECT count(*) FROM messages WHERE kind='system' AND state='queued' AND not_before > created_at",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    socket_wait_long(
        &sb,
        "SELECT count(*) FROM messages WHERE kind='system' AND state='delivered'",
        1,
        80,
    );
    assert!(f.prompts(&a).contains("may have arrived"));
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM messages WHERE kind='system'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}
#[test]
fn a_late_entry_cancels_the_pending_system_mail() {
    let (sb, f) = socket_agents();
    let a = socket_spawn(&sb, &f, "A");
    let b = socket_spawn(&sb, &f, "B");
    // Delay before even enqueuing so the no-evidence timer genuinely fires.
    std::fs::write(f.bin.join(format!("socket-mode-{}", b)), "late-33").unwrap();
    send_body(&sb, &f, &a, &format!("forward to {}", b));
    socket_wait_long(
        &sb,
        "SELECT count(*) FROM messages WHERE kind='system' AND state='cancelled'",
        1,
        50,
    );
    let c = ledger(&sb);
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM messages WHERE origin_kind='agent' AND state='delivered'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM message_status WHERE detail LIKE 'late_evidence:%'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn m2fix_cc_listener_is_absent_when_gate_is_unset_or_off() {
    for gate in [None, Some("off")] {
        let sb = Sandbox::new();
        let mut cmd = sb.cmd(&["list"]);
        if let Some(value) = gate {
            cmd.env("C9_MAIL_SOCKET", value);
        } else {
            cmd.env_remove("C9_MAIL_SOCKET");
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{out:?} {}", sb.log());
        let sockdir = sb.home.parent().unwrap().join("socks");
        assert_eq!(
            std::fs::read_dir(&sockdir).unwrap().count(),
            0,
            "gate={gate:?}"
        );
    }
}

#[test]
fn explicit_engine_stop_is_idempotent_and_never_auto_starts() {
    let sb = Sandbox::new();
    let out = sb.run(&["engine", "stop"]);
    assert!(out.status.success(), "{out:?}");
    assert!(sb.pid().is_none());
    assert!(!sock_path(&sb.engine).exists());
    assert!(sb.run(&["api", "hello"]).status.success());
    let pid = sb.pid().unwrap();
    let out = sb.run(&["engine", "stop"]);
    assert!(out.status.success(), "{out:?}");
    assert!(wait_for(Duration::from_secs(5), || !alive(pid)));
    assert!(sb.pid().is_none());
    assert!(sb.run(&["engine", "stop"]).status.success());
    assert!(sb.pid().is_none());
}

// An inert child fixture, never a GUI or real agent. Invoked only by the
// newer-version test below. It has the same uid, socket and lifetime-lock
// identity checks as a real engine.
#[test]
#[ignore]
fn newer_core_fixture() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    let Some(home) = std::env::var_os("C9_TEST_NEWER_HOME") else {
        return;
    };
    let home = PathBuf::from(home);
    std::fs::create_dir_all(&home).unwrap();
    let _lock = c9watch_lib::engine::lock::acquire_engine_lock(&lock_path(&home)).unwrap();
    let listener = UnixListener::bind(sock_path(&home)).unwrap();
    let version = std::env::var("C9_TEST_NEWER_VERSION").unwrap_or_else(|_| "999.0.0".into());
    let minor: u32 = std::env::var("C9_TEST_NEWER_MINOR")
        .unwrap_or_else(|_| "999".into())
        .parse()
        .unwrap();
    for socket in listener.incoming() {
        let mut socket = socket.unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        while {
            line.clear();
            reader.read_line(&mut line).unwrap_or(0) > 0
        } {
            let request: Value = serde_json::from_str(&line).unwrap();
            writeln!(socket, "{}", serde_json::json!({"v":1,"id":request["id"],"ok":true,"result":{
                "apiMajor":1,"apiMinor":minor,"engineVersion":version,"machineId":"test","principal":"local-unverified","capabilities":[],"cursor":null
            }})).unwrap();
        }
    }
}

#[test]
fn cli_restart_refuses_a_newer_core_without_signalling_it() {
    for (version, minor) in [("999.0.0", "6"), (env!("CARGO_PKG_VERSION"), "999")] {
        let sb = Sandbox::new();
        let mut fixture = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "newer_core_fixture"])
            .env("C9_TEST_NEWER_HOME", &sb.engine)
            .env("C9_TEST_NEWER_VERSION", version)
            .env("C9_TEST_NEWER_MINOR", minor)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        assert!(wait_for(Duration::from_secs(5), || sock_path(&sb.engine).exists()));
        let out = sb.run(&["engine", "restart"]);
        let survived = fixture.try_wait().unwrap().is_none();
        let _ = fixture.kill();
        let _ = fixture.wait();
        assert!(!out.status.success(), "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("engine_newer"),
            "{out:?}"
        );
        assert!(survived, "restart signalled a newer core");
    }
}
