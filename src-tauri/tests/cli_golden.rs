//! Golden tests for the in-process detection commands (C1a-36): `list`,
//! `status`, `watch` and `wait` print what they printed before the
//! CLI became an engine client. An engine build runs them with `--direct`;
//! any other build (the release CLI) runs them as they are. Both compare
//! against the same fixtures in `tests/fixtures/cli-golden/`.
//!
//! Each run uses a temp HOME and a PATH of a stand-in `claude` (never the
//! real one) whose `agents --json` lists one session on a live `sleep`.
//! `UPDATE_GOLDEN=1` rewrites the fixtures. `self` is left out: its
//! output depends on whether the test runs under a Claude Code process.

#![cfg(unix)]

use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_c9watch");
const SID: &str = "11111111-2222-3333-4444-555555555555";

/// `--version` gets a version CLI detection accepts, `agents --json` prints
/// `agents.json` next to the script, and anything else fails.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "2.1.312 (Claude Code)"; exit 0 ;;
  agents) cat "$(dirname "$0")/agents.json"; exit 0 ;;
esac
exit 1
"#;

struct Golden {
    _dir: tempfile::TempDir,
    root: PathBuf,
    sleeper: Child,
}

impl Golden {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        for d in ["h", "bin", "work/proj"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let claude = root.join("bin/claude");
        std::fs::write(&claude, FAKE_CLAUDE).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let sleeper = Command::new("sleep").arg("300").spawn().unwrap();
        // A start time at or after the process's own keeps the pid-reuse
        // check quiet.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 1000;
        let agents = serde_json::json!([{
            "pid": sleeper.id(),
            "cwd": root.join("work/proj"),
            "kind": "interactive",
            "startedAt": now_ms,
            "sessionId": SID,
            "name": "golden",
            "status": "busy",
        }]);
        std::fs::write(root.join("bin/agents.json"), agents.to_string()).unwrap();
        Golden {
            _dir: dir,
            root,
            sleeper,
        }
    }

    /// `args` with `--direct` after the subcommand in an engine build.
    fn cmd(&self, args: &[&str]) -> Command {
        let mut args: Vec<&str> = args.to_vec();
        if cfg!(feature = "engine") {
            args.insert(1, "--direct");
        }
        let mut c = Command::new(BIN);
        c.args(&args)
            .env_clear()
            .env("HOME", self.root.join("h"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("C9WATCH_ENGINE_HOME", self.root.join("e"))
            .current_dir(&self.root)
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// Replace what differs between runs: the temp root, pids and times.
    fn normalize(&self, v: Value) -> Value {
        let root = self.root.display().to_string();
        match v {
            Value::String(s) => Value::String(s.replace(&root, "<ROOT>")),
            Value::Array(a) => Value::Array(a.into_iter().map(|x| self.normalize(x)).collect()),
            Value::Object(o) => Value::Object(
                o.into_iter()
                    .map(|(k, x)| {
                        let x = match k.as_str() {
                            "pid" => Value::String("<PID>".into()),
                            "modified" | "timestamp" => Value::String("<TIME>".into()),
                            _ => self.normalize(x),
                        };
                        (k, x)
                    })
                    .collect(),
            ),
            other => other,
        }
    }

    /// One command's result as `{"exit", "stdout", "stderr"}`, each stream
    /// parsed as JSON lines.
    fn result(&self, out: &Output) -> Value {
        let lines = |b: &[u8]| -> Value {
            String::from_utf8_lossy(b)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    self.normalize(
                        serde_json::from_str(l).unwrap_or_else(|_| Value::String(l.to_string())),
                    )
                })
                .collect()
        };
        serde_json::json!({
            "exit": out.status.code(),
            "stdout": lines(&out.stdout),
            "stderr": lines(&out.stderr),
        })
    }

    /// The engine home stays untouched: `--direct` never starts an engine.
    fn assert_no_engine(&self) {
        assert!(
            !self.root.join("e").exists(),
            "a direct command touched the engine home"
        );
    }
}

impl Drop for Golden {
    fn drop(&mut self) {
        let _ = self.sleeper.kill();
        let _ = self.sleeper.wait();
    }
}

fn check(name: &str, got: &Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cli-golden")
        .join(format!("{}.json", name));
    let text = serde_json::to_string_pretty(got).unwrap() + "\n";
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden {:?} ({}); run with UPDATE_GOLDEN=1",
            path, e
        )
    });
    assert_eq!(text, want, "golden {} differs", name);
}

#[test]
fn list_status_and_wait_match_the_golden_output() {
    let g = Golden::new();
    let cases: &[(&str, &[&str])] = &[
        ("list", &["list"]),
        ("list-compact", &["list", "--compact"]),
        ("list-status-match", &["list", "--status", "connecting"]),
        ("list-status-none", &["list", "--status", "Working"]),
        (
            "list-project-none",
            &["list", "--project", "no-such-project"],
        ),
        ("status", &["status"]),
        ("status-project", &["status", "--project", "proj"]),
        (
            "wait-done",
            &["wait", "1111", "--until", "done", "--timeout", "30"],
        ),
        (
            "wait-missing",
            &["wait", "9999", "--until", "done", "--timeout", "30"],
        ),
    ];
    for (name, args) in cases {
        let out = g.run(args);
        check(name, &g.result(&out));
    }
    g.assert_no_engine();
}

#[test]
fn watch_matches_the_golden_output() {
    let g = Golden::new();
    let mut child = g
        .cmd(&["watch", "--compact"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(60));
    let _ = child.kill();
    let _ = child.wait();
    let line = line.expect("watch printed no line in 60 s");
    let got = g.normalize(serde_json::from_str(&line).unwrap());
    check("watch-started", &got);
    g.assert_no_engine();
}
