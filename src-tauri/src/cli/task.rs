//! `c9watch task done --report <text|->`: the managed worker's completion
//! call to the c9 engine (ported from the C0 spike, D23.12 / D24.7).
//!
//! The engine sets `C9_ENGINE_SOCK`, `C9_AGENT_ID` and `C9_AGENT_TOKEN` on
//! the worker's PTY child. [`report_done`] sends one newline-delimited JSON
//! message on that unix socket and reads one JSON line back. The wire
//! message is the same one the spike's `c0-done --report` sends.
//!
//! Library code: [`engine_env`], [`done_message`] and [`report_done`] have
//! no stdin/stdout side effects, so any binary that links `c9watch_lib`
//! can call them. [`cmd_task_done`] is the CLI wrapper.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const ENV_SOCK: &str = "C9_ENGINE_SOCK";
pub const ENV_AGENT_ID: &str = "C9_AGENT_ID";
pub const ENV_TOKEN: &str = "C9_AGENT_TOKEN";

#[derive(Debug, Clone, PartialEq)]
pub struct EngineEnv {
    pub sock: String,
    pub agent_id: String,
    pub token: String,
}

/// Read the engine env. All three are required: a session the engine did
/// not spawn has none of them, and the command must fail loudly there.
pub fn engine_env(get: impl Fn(&str) -> Option<String>) -> Result<EngineEnv, String> {
    let missing: Vec<&str> = [ENV_SOCK, ENV_AGENT_ID, ENV_TOKEN]
        .into_iter()
        .filter(|k| get(k).map_or(true, |v| v.is_empty()))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "task done: {} not set; this command works only inside a worker that the c9 engine spawned",
            missing.join(", ")
        ));
    }
    Ok(EngineEnv {
        sock: get(ENV_SOCK).unwrap_or_default(),
        agent_id: get(ENV_AGENT_ID).unwrap_or_default(),
        token: get(ENV_TOKEN).unwrap_or_default(),
    })
}

/// The `task.done` wire message. `seq` is the sender's clock in ns, as a
/// string (same as c0-done).
pub fn done_message(env: &EngineEnv, report: &str, seq: u128, pid: u32) -> Value {
    json!({
        "method": "task.done",
        "report": report,
        "agent_id": env.agent_id,
        "token": env.token,
        "seq": seq.to_string(),
        "pid": pid,
    })
}

fn send(sock: &str, msg: &Value) -> Result<Value, String> {
    let mut s = UnixStream::connect(sock)
        .map_err(|e| format!("task done: cannot reach the engine at {sock}: {e}"))?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    s.write_all(format!("{msg}\n").as_bytes())
        .map_err(|e| format!("task done: write to {sock} failed: {e}"))?;
    let mut line = String::new();
    BufReader::new(s)
        .read_line(&mut line)
        .map_err(|e| format!("task done: no reply from the engine: {e}"))?;
    serde_json::from_str(line.trim())
        .map_err(|_| format!("task done: bad reply from the engine: {:?}", line.trim()))
}

/// Send `report` to the engine as `task.done` and return its reply.
/// A reply without `"ok": true` is an error.
pub fn report_done(env: &EngineEnv, report: &str) -> Result<Value, String> {
    if report.trim().is_empty() {
        return Err("task done: the report is empty".into());
    }
    let seq = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let reply = send(
        &env.sock,
        &done_message(env, report, seq, std::process::id()),
    )?;
    if reply["ok"] != true {
        return Err(format!("task done: the engine refused the report: {reply}"));
    }
    Ok(reply)
}

/// `c9watch task done --report <text|->`: read the env and the report
/// (`-` reads stdin), send it, and print the engine's reply.
pub fn cmd_task_done(report: &str) -> Result<(), String> {
    let env = engine_env(|k| std::env::var(k).ok())?;
    let report = if report == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| format!("task done: cannot read the report from stdin: {e}"))?;
        s
    } else {
        report.to_string()
    };
    let reply = report_done(&env, &report)?;
    println!("{reply}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn message_matches_c0_done_wire_format() {
        let env = EngineEnv {
            sock: "/s".into(),
            agent_id: "agt_x".into(),
            token: "tok".into(),
        };
        let m = done_message(&env, "all 3 steps done", 42, 7);
        assert_eq!(
            m,
            json!({"method": "task.done", "report": "all 3 steps done", "agent_id": "agt_x",
                   "token": "tok", "seq": "42", "pid": 7})
        );
        // One line on the wire: the report's newlines stay escaped.
        let m = done_message(&env, "a\nb", 1, 1);
        assert!(!m.to_string().contains('\n'));
    }

    #[test]
    fn missing_env_is_an_error_that_names_the_vars() {
        let e = engine_env(env_of(&[])).unwrap_err();
        assert!(
            e.contains("C9_ENGINE_SOCK")
                && e.contains("C9_AGENT_ID")
                && e.contains("C9_AGENT_TOKEN"),
            "{e}"
        );
        let e = engine_env(env_of(&[
            ("C9_ENGINE_SOCK", "/s"),
            ("C9_AGENT_ID", "a"),
            ("C9_AGENT_TOKEN", ""),
        ]))
        .unwrap_err();
        assert!(
            e.contains("C9_AGENT_TOKEN") && !e.contains("C9_ENGINE_SOCK"),
            "{e}"
        );
        let ok = engine_env(env_of(&[
            ("C9_ENGINE_SOCK", "/s"),
            ("C9_AGENT_ID", "a"),
            ("C9_AGENT_TOKEN", "t"),
        ]))
        .unwrap();
        assert_eq!(
            ok,
            EngineEnv {
                sock: "/s".into(),
                agent_id: "a".into(),
                token: "t".into()
            }
        );
    }

    #[test]
    fn refused_socket_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("none.sock");
        let e = send(sock.to_str().unwrap(), &json!({})).unwrap_err();
        assert!(e.contains("cannot reach the engine"), "{e}");
    }

    #[test]
    fn round_trip_over_a_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("e.sock");
        let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let h = std::thread::spawn(move || {
            let (c, _) = l.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&c).read_line(&mut line).unwrap();
            let mut w = &c;
            writeln!(w, "{}", json!({"ok": true})).unwrap();
            serde_json::from_str::<Value>(line.trim()).unwrap()
        });
        let env = EngineEnv {
            sock: path.display().to_string(),
            agent_id: "a".into(),
            token: "t".into(),
        };
        let reply = send(&env.sock, &done_message(&env, "r", 1, 2)).unwrap();
        assert_eq!(reply["ok"], true);
        assert_eq!(h.join().unwrap()["method"], "task.done");
    }
}
