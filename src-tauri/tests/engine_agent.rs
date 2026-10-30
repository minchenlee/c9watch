//! `c9watch-agent` against a real engine (C1a-21): the engine's own copy,
//! the exe and ancestry checks, and the hook's exit-0 promise.
//!
//! The engine runs in this process, in a temp engine home whose path holds
//! a space (like `Application Support`), with detection off. The agent
//! source comes in through `EngineConfig`, never through the env. Agents
//! are `/bin/sh` PTY children that the test registers in the engine's live
//! table; no real Claude Code runs.

#![cfg(all(unix, feature = "engine"))]

use c9watch_lib::engine::agent_bin::installed_path;
use c9watch_lib::engine::agents::LiveTable;
use c9watch_lib::engine::db::db_path;
use c9watch_lib::engine::driver::{Driver, SpawnSpec};
use c9watch_lib::engine::home::{sock_path, ENGINE_HOME_ENV};
use c9watch_lib::engine::machine;
use c9watch_lib::engine::pty::NativePty;
use c9watch_lib::engine::run::{run_until, EngineConfig};
use c9watch_lib::engine::standing::{header, mail_rules};
use c9watch_lib::engine_proto::client::Conn;
use c9watch_lib::engine_proto::wire::HOOK_METHODS;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_c9watch-agent");
const MAIN: &str = env!("CARGO_BIN_EXE_c9watch");
const AGENT_ID: &str = "agt_t";

/// An engine running on its own thread. Dropping it stops the engine.
struct Engine {
    dir: tempfile::TempDir,
    home: PathBuf,
    agents: Arc<LiveTable>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl Engine {
    fn start() -> Engine {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("engine home");
        let agents = Arc::new(LiveTable::new());
        let config = EngineConfig {
            home: home.clone(),
            agent_source: PathBuf::from(AGENT),
            agents: agents.clone(),
            detection: false,
            claude: None,
        };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(run_until(&config, async {
                let _ = stopped.await;
            }))
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while Conn::connect(&sock_path(&home), "test", Duration::from_secs(1)).is_err() {
            assert!(Instant::now() < deadline, "the engine did not come up");
            std::thread::sleep(Duration::from_millis(50));
        }
        Engine {
            dir,
            home,
            agents,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    fn sock(&self) -> PathBuf {
        sock_path(&self.home)
    }

    fn copy(&self) -> PathBuf {
        installed_path(&self.home)
    }

    /// Audit rows the engine wrote for refused hook-class calls.
    fn denials(&self) -> Vec<(String, String)> {
        let conn = rusqlite::Connection::open(db_path(&self.home)).unwrap();
        let mut stmt = conn
            .prepare("SELECT principal, method FROM audit WHERE decision = 'deny' ORDER BY seq")
            .unwrap();
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        rows.map(Result::unwrap).collect()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(t) = self.thread.take() {
            t.join().unwrap().unwrap();
        }
    }
}

/// Single-quote `p` for `/bin/sh`.
fn q(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

/// Run `cmd` in `/bin/sh` behind a PTY with the agent env, as an engine
/// agent would. With `register`, the PTY child is the live generation 1 of
/// `AGENT_ID` before the command starts. Returns the output and the exit
/// code.
fn as_agent(engine: &Engine, cmd: &str, register: bool) -> (String, i32) {
    let script = format!("read go; {}; echo \"rc=$?.\"", cmd);
    let mut spec = SpawnSpec::new(
        vec!["/bin/sh".into(), "-c".into(), script.into()],
        engine.dir.path().to_path_buf(),
    );
    spec.env("HOME", engine.dir.path());
    spec.env("C9_ENGINE_SOCK", engine.sock());
    spec.env("C9_AGENT_ID", AGENT_ID);
    let mut pty = NativePty::spawn(&spec).unwrap();
    if register {
        engine.agents.register(AGENT_ID, 1, pty.child()).unwrap();
    }
    pty.write(b"go\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = String::new();
    let rc = loop {
        let out = pty.output(Duration::from_millis(100));
        seen.push_str(&String::from_utf8_lossy(&out.bytes));
        if let Some(i) = seen.rfind("rc=") {
            if let Some(end) = seen[i..].find('.') {
                break seen[i + 3..i + end].parse().unwrap();
            }
        }
        assert!(!out.eof, "EOF; got {:?}", seen);
        assert!(Instant::now() < deadline, "no rc; got {:?}", seen);
    };
    pty.kill().unwrap();
    if register {
        engine.agents.unregister(AGENT_ID, 1);
    }
    (seen, rc)
}

#[test]
fn the_engine_installs_its_own_copy_0555_with_the_source_sha() {
    let engine = Engine::start();
    let copy = engine.copy();
    let meta = std::fs::metadata(&copy).unwrap();
    assert_eq!(meta.permissions().mode() & 0o7777, 0o555);
    assert_eq!(
        Sha256::digest(std::fs::read(&copy).unwrap()),
        Sha256::digest(std::fs::read(AGENT).unwrap())
    );
    assert_ne!(
        meta.ino(),
        std::fs::metadata(AGENT).unwrap().ino(),
        "a copy, not a link"
    );
}

#[test]
fn an_agent_running_the_copy_is_authorized() {
    let engine = Engine::start();
    let bin = q(&engine.copy());
    // Authorized, and every C1a agent is free: no task.
    let (out, rc) = as_agent(&engine, &format!("{} task done --report r", bin), true);
    assert_eq!(rc, 1, "{}", out);
    assert!(out.contains("no_task"), "{}", out);
    let (out, rc) = as_agent(&engine, &format!("echo n | {} handoff --note -", bin), true);
    assert_eq!(rc, 1, "{}", out);
    assert!(out.contains("unsupported_in_c1a"), "{}", out);
    // The hook goes through quietly.
    let (out, rc) = as_agent(
        &engine,
        &format!("echo '{{\"session_id\":\"s\"}}' | {} hook stop", bin),
        true,
    );
    assert_eq!(rc, 0, "{}", out);
    // Nothing on stdout but the terminal's echo of our input.
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != "go")
        .collect();
    assert_eq!(lines, ["rc=0."], "{:?}", out);
    assert!(engine.denials().is_empty(), "{:?}", engine.denials());
}

/// X10: an authorized hook moves the agent row through the state merge
/// (C1a-22); a hook of another kind is dropped and audited.
#[test]
fn an_accepted_hook_moves_the_agent_row() {
    let engine = Engine::start();
    let db = || {
        let c = rusqlite::Connection::open(db_path(&engine.home)).unwrap();
        c.busy_timeout(Duration::from_secs(5)).unwrap();
        c
    };
    db().execute(
        "INSERT INTO agents (id, generation, mode, role, control, provider, provider_sid, \
         source, state) VALUES (?1, 1, 'free', 'worker', 'pty', 'claudeCode', 's1', \
         'engine', 'working')",
        [AGENT_ID],
    )
    .unwrap();
    let state = || -> String {
        db().query_row("SELECT state FROM agents WHERE id = ?1", [AGENT_ID], |r| {
            r.get(0)
        })
        .unwrap()
    };
    let bin = q(&engine.copy());
    let (out, rc) = as_agent(
        &engine,
        &format!("{} hook permission-request </dev/null", bin),
        true,
    );
    assert_eq!(rc, 0, "{}", out);
    // The hook waits for the engine's answer, but it gives up after 0.5 s
    // and still exits 0: wait for the row, and show the hook's output.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while state() != "blocked" && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(state(), "blocked", "{}", out);
    let (out, rc) = as_agent(
        &engine,
        &format!("{} hook stop --kind grok </dev/null", bin),
        true,
    );
    assert_eq!(rc, 0, "{}", out);
    let drops = || -> Vec<String> {
        let c = db();
        let mut st = c
            .prepare("SELECT reason FROM audit WHERE decision = 'drop'")
            .unwrap();
        let rows = st.query_map([], |r| r.get(0)).unwrap();
        rows.map(Result::unwrap).collect()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while drops().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(state(), "blocked");
    let drops = drops();
    assert_eq!(drops.len(), 1, "{:?} {}", drops, out);
    assert!(drops[0].starts_with("kind_mismatch"), "{:?}", drops);
    assert!(engine.denials().is_empty(), "{:?}", engine.denials());
}

/// C1a-25: `hook session-start` prints the standing orders from
/// `<home>/standing-orders.md` as CC's `additionalContext`, and after an
/// interrupt the note goes out once.
#[test]
fn session_start_prints_the_standing_orders() {
    let engine = Engine::start();
    let orders = engine.home.join("standing-orders.md");
    let default = std::fs::read_to_string(&orders).unwrap();
    assert!(default.contains("{{agent_id}}"), "{}", default);
    std::fs::write(&orders, "Orders for {{agent_id}}.\n").unwrap();
    let db = rusqlite::Connection::open(db_path(&engine.home)).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute(
        "INSERT INTO agents (id, generation, mode, role, control, provider, provider_sid, \
         source, state, interrupted_note_pending) VALUES (?1, 1, 'free', 'worker', 'pty', \
         'claudeCode', 's1', 'engine', 'ready', 1)",
        [AGENT_ID],
    )
    .unwrap();
    let pending = || -> i64 {
        db.query_row(
            "SELECT interrupted_note_pending FROM agents WHERE id = ?1",
            [AGENT_ID],
            |r| r.get(0),
        )
        .unwrap()
    };
    warm_up_agent();
    let bin = q(&engine.copy());
    // The hook gives up after 4 s, and a slow runner could still miss
    // that: run it until it prints. Each run is a new event (a new seq).
    // Returns the output, the context and how many runs it took.
    let context = |source: &str| -> (String, Value, usize) {
        let cmd = format!(
            "echo '{{\"session_id\":\"s1\",\"source\":\"{}\"}}' | {} hook session-start",
            source, bin
        );
        let mut last = String::new();
        for run in 1..=10 {
            let (out, rc) = as_agent(&engine, &cmd, true);
            assert_eq!(rc, 0, "{}", out);
            if let Some(line) = out.lines().find(|l| l.contains("hookSpecificOutput")) {
                let v: Value = serde_json::from_str(line.trim()).unwrap();
                let ctx = v["hookSpecificOutput"]["additionalContext"].clone();
                assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
                return (out, ctx, run);
            }
            last = out;
        }
        panic!("no context in 10 runs; last {:?}", last);
    };
    // A compaction does not take the note; the next start does, once.
    // Every block names generation 1 and replaces earlier ones (D24).
    // Then the mail rules with the agent's header token (C1b-M1-7).
    let token = machine::load_or_create(&engine.home)
        .unwrap()
        .header_key()
        .token(AGENT_ID);
    let want = format!(
        "{}\n\nOrders for agt_t.\n\n{}",
        header(1),
        mail_rules(Some(&token))
    );
    let (out, ctx, _) = context("compact");
    assert_eq!(ctx, want, "{}", out);
    assert_eq!(pending(), 1);
    let (out, ctx, runs) = context("resume");
    assert_eq!(pending(), 0, "{}", out);
    // Always on the first run that prints: an earlier run that timed out
    // would have taken the note without printing it.
    assert_eq!(
        ctx,
        format!(
            "{}\nYou were interrupted; check `git status` first.\n",
            want
        ),
        "{} run(s): {}",
        runs,
        out
    );
    let (out, ctx, _) = context("startup");
    assert_eq!(ctx, want, "{}", out);
    let (out, ctx, _) = context("compact");
    assert_eq!(ctx, want, "{}", out);
    assert!(engine.denials().is_empty(), "{:?}", engine.denials());
}

#[test]
fn the_copy_outside_a_live_agent_is_refused() {
    let engine = Engine::start();
    let bin = q(&engine.copy());
    let (out, rc) = as_agent(&engine, &format!("{} task done --report r", bin), false);
    assert_eq!(rc, 1, "{}", out);
    assert!(out.contains("peer_not_authorized"), "{}", out);
    // The hook still exits 0; the engine logs the refusal.
    let (_, rc) = as_agent(&engine, &format!("{} hook stop </dev/null", bin), false);
    assert_eq!(rc, 0);
    let denials = engine.denials();
    assert_eq!(
        denials,
        vec![
            ("local-unverified".to_string(), "task.done".to_string()),
            ("local-unverified".to_string(), "hook.stop".to_string()),
        ]
    );
}

#[test]
fn an_agent_running_another_binary_is_refused() {
    let engine = Engine::start();
    // The source next to the main binary: same bytes, not the engine's copy.
    let (out, rc) = as_agent(
        &engine,
        &format!("{} task done --report r", q(Path::new(AGENT))),
        true,
    );
    assert_eq!(rc, 1, "{}", out);
    assert!(out.contains("peer_not_authorized"), "{}", out);
    // The main binary's raw `api` escape hatch, from inside the agent, for
    // `task.done`, `agent.handoff` and every hook method (X17).
    let mut calls = vec![
        (
            "task.done",
            r#"{"agentId":"agt_t","report":"r","seq":"1"}"#.to_string(),
        ),
        (
            "agent.handoff",
            r#"{"agentId":"agt_t","note":"n","seq":"2"}"#.to_string(),
        ),
    ];
    for (i, method) in HOOK_METHODS.iter().enumerate() {
        let params = format!(
            r#"{{"agentId":"agt_t","kind":"claude","seq":"{}","input":{{}}}}"#,
            i + 3
        );
        calls.push((*method, params));
    }
    for (method, params) in &calls {
        let cmd = format!(
            "env -u C9_AGENT_ID {}={} {} api {} '{}'",
            ENGINE_HOME_ENV,
            q(&engine.home),
            q(Path::new(MAIN)),
            method,
            params
        );
        let (out, rc) = as_agent(&engine, &cmd, true);
        assert_eq!(rc, 1, "{}: {}", method, out);
        assert!(out.contains("peer_not_authorized"), "{}: {}", method, out);
    }
    let agent = format!("agent:{}", AGENT_ID);
    let mut want = vec![(agent.clone(), "task.done".to_string())];
    want.extend(calls.iter().map(|(m, _)| (agent.clone(), m.to_string())));
    assert_eq!(engine.denials(), want);
}

/// Security review H1: `local-unverified` may retire (and create), but not
/// from deeper than the stamp looks under a live agent, nor from a sandbox.
#[test]
fn a_nested_or_sandboxed_local_peer_may_not_retire() {
    let engine = Engine::start();
    let api = format!(
        "env -u C9_AGENT_ID {}={} {} api agent.retire '{{\"id\":\"agt_none\"}}'",
        ENGINE_HOME_ENV,
        q(&engine.home),
        q(Path::new(MAIN))
    );
    // Outside any agent: the call goes through to the unknown id.
    let (out, rc) = as_agent(&engine, &api, false);
    assert_eq!(rc, 1, "{}", out);
    assert!(out.contains("unknown_agent"), "{}", out);
    // 14 shells under the live agent's PTY child: the stamp walks only 10
    // levels and says local-unverified, the mutation check walks them all.
    let nest = engine.dir.path().join("nest.sh");
    std::fs::write(
        &nest,
        format!(
            "if [ \"$1\" -gt 0 ]; then /bin/sh \"$0\" $(($1 - 1)); rc=$?; exit $rc; fi\n{}\n",
            api
        ),
    )
    .unwrap();
    let (out, rc) = as_agent(&engine, &format!("/bin/sh {} 14", q(&nest)), true);
    assert_eq!(rc, 1, "{}", out);
    assert!(out.contains("not_allowed"), "{}", out);
    assert!(out.contains("agent:agt_t"), "{}", out);
    let mut want = 1;
    // A sandboxed peer, as an agent's Bash or anything it left behind.
    if cfg!(target_os = "macos") {
        let (out, rc) = as_agent(
            &engine,
            &format!(
                "/usr/bin/sandbox-exec -p '(version 1)(allow default)' /bin/sh -c {}",
                q(Path::new(&api))
            ),
            false,
        );
        assert_eq!(rc, 1, "{}", out);
        if out.contains("not_allowed") {
            assert!(out.contains("sandbox"), "{}", out);
            want += 1;
        } else {
            // sandbox-exec cannot nest inside a sandboxed test run.
            eprintln!("skip: sandbox-exec did not run: {}", out);
        }
    }
    let denials = engine.denials();
    assert_eq!(denials.len(), want, "{:?}", denials);
    assert!(denials
        .iter()
        .all(|(p, m)| p == "local-unverified" && m == "agent.retire"));
}

#[test]
fn a_changed_copy_is_refused() {
    let engine = Engine::start();
    let copy = engine.copy();
    // Same inode, other bytes: the agent binary with a trailing byte, which
    // still runs.
    let mut bytes = std::fs::read(&copy).unwrap();
    bytes.push(0);
    std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(&copy, &bytes).unwrap();
    let (out, rc) = as_agent(&engine, &format!("{} task done --report r", q(&copy)), true);
    assert_eq!(rc, 1, "{}", out);
    assert!(out.contains("peer_not_authorized"), "{}", out);
    assert!(out.contains("agent_bin_mismatch"), "{}", out);
}

/// Run the agent binary once, untimed: the first exec of a freshly linked
/// binary can wait on the OS's malware scan, which is not the hook's cost.
fn warm_up_agent() {
    let out = Command::new(AGENT).arg("--version").output().unwrap();
    assert!(out.status.success(), "{:?}", out);
}

#[test]
fn a_hook_exits_0_without_an_engine_or_with_bad_args() {
    warm_up_agent();
    let dir = tempfile::tempdir().unwrap();
    let hook = |args: &[&str]| {
        let t = Instant::now();
        let out = Command::new(AGENT)
            .args(args)
            .env("C9_ENGINE_SOCK", dir.path().join("none.sock"))
            .env("C9_AGENT_ID", AGENT_ID)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out, t.elapsed())
    };
    for args in [
        &["hook", "stop"][..],
        &["hook", "session-start"],
        &["hook", "no-such-event"],
        &["hook"],
        &["hook", "stop", "--bogus"],
    ] {
        let (out, took) = hook(args);
        assert_eq!(out.status.code(), Some(0), "{:?}: {:?}", args, out);
        assert!(out.stdout.is_empty(), "{:?}: {:?}", args, out);
        assert!(took < Duration::from_secs(3), "{:?} took {:?}", args, took);
    }
    // `task done` is not a hook: it fails loudly.
    let (out, _) = hook(&["task", "done", "--report", "r"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot reach the engine"));
    // No `api`.
    let (out, _) = hook(&["api", "hello"]);
    assert_eq!(out.status.code(), Some(2));
}

/// The engine accepts the connection but never answers: the hook gives up
/// after its 0.5 s budget and still exits 0.
#[test]
fn a_hook_against_a_silent_engine_exits_0_in_time() {
    warm_up_agent();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("e.sock");
    let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let held = std::thread::spawn(move || {
        let c = l.accept().unwrap();
        std::thread::sleep(Duration::from_secs(3));
        drop(c);
    });
    let t = Instant::now();
    let out = Command::new(AGENT)
        .args(["hook", "stop"])
        .env("C9_ENGINE_SOCK", &path)
        .env("C9_AGENT_ID", AGENT_ID)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let took = t.elapsed();
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    assert!(out.stdout.is_empty());
    // Well under the 3 s the engine holds the connection.
    assert!(took < Duration::from_millis(2500), "took {:?}", took);
    held.join().unwrap();
}

/// `cargo run` picks the main binary (`default-run`), and the agent binary
/// exists only with the engine (`required-features`). The Linux CI job also
/// runs `cargo run ... -- --version` and builds `c9watch-agent` by name.
#[test]
fn the_manifest_keeps_c9watch_the_default_binary() {
    let manifest = include_str!("../Cargo.toml");
    assert!(manifest.contains("\ndefault-run = \"c9watch\"\n"));
    let bin = manifest.split("[[bin]]").nth(1).unwrap();
    assert!(bin.contains("name = \"c9watch-agent\""));
    assert!(bin.contains("required-features = [\"engine\"]"));
}
