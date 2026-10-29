//! The agent side of the c9 engine, as `c9watch-agent` runs it (C1a-21,
//! D28 Q6=B): `hook <event>`, `task done --report <text|->` and `handoff
//! --note <text|->`.
//!
//! The engine sets `C9_ENGINE_SOCK` and `C9_AGENT_ID` on the agent's PTY
//! child. Neither is a secret: the engine knows who calls from the peer's
//! ancestry and exe (`engine::agents`, `engine::agent_bin`). Each command
//! opens one c9p/1 connection, says `hello` and sends one call.
//!
//! A hook never fails the vendor's hook: it exits 0, prints nothing but
//! the `additionalContext` of `SessionStart`, and gives up after
//! `HOOK_TIMEOUT` (A:1054). Only the engine writes logs (A:1167).
//!
//! Library code: everything here except the `cmd_*` wrappers is free of
//! stdin/stdout side effects.

use crate::engine_proto::client::{ClientError, Conn};
use crate::engine_proto::wire::{
    HandoffParams, HookParams, HookResult, TaskDoneParams, TaskDoneResult,
};
use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const ENV_SOCK: &str = "C9_ENGINE_SOCK";
pub const ENV_AGENT_ID: &str = "C9_AGENT_ID";

/// How long a hook waits for the engine, all in (A:1054).
pub const HOOK_TIMEOUT: Duration = Duration::from_millis(500);
/// How long `hook session-start` waits instead. Its answer carries the
/// standing orders and, once after an interrupt, a note the engine hands
/// out only once (C1a-25); CC does not run the hook again, so a reply lost
/// to a busy engine would lose the note. It runs once per generation, and
/// stays under CC's own hook timeout (`engine::adapter::CC_HOOK_TIMEOUT_S`).
pub const SESSION_START_TIMEOUT: Duration = Duration::from_secs(4);
/// How long `task done` and `handoff` wait for each read and write.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest hook stdin a hook forwards. A larger one goes as `null`
/// with `truncated: true`, so the event still arrives.
pub const MAX_HOOK_INPUT: usize = 256 * 1024;

/// The adapter kind `hook` sends unless told otherwise.
pub const DEFAULT_HOOK_KIND: &str = "claude";

#[derive(Debug, Clone, PartialEq)]
pub struct EngineEnv {
    pub sock: String,
    pub agent_id: String,
}

/// Read the engine env. Both are required: a session the engine did not
/// spawn has neither, and `task done` must fail loudly there.
pub fn engine_env(get: impl Fn(&str) -> Option<String>) -> Result<EngineEnv, String> {
    let missing: Vec<&str> = [ENV_SOCK, ENV_AGENT_ID]
        .into_iter()
        .filter(|k| get(k).map_or(true, |v| v.is_empty()))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} not set; this command works only inside an agent that the c9 engine spawned",
            missing.join(", ")
        ));
    }
    Ok(EngineEnv {
        sock: get(ENV_SOCK).unwrap_or_default(),
        agent_id: get(ENV_AGENT_ID).unwrap_or_default(),
    })
}

/// The sender's clock in ns, as a string: with the agent id it dedupes.
fn seq_now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .to_string()
}

/// One call on a new connection. `"<code>: <msg>"` on an engine error.
fn call(env: &EngineEnv, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
    let mut conn = Conn::connect(Path::new(&env.sock), "agent", timeout).map_err(|e| match e {
        ClientError::Unreachable(m) => format!("cannot reach the engine at {}: {}", env.sock, m),
        ClientError::Failed(m) => m,
    })?;
    conn.call(method, params).map_err(|e| match e {
        ClientError::Unreachable(m) => format!("no reply from the engine: {}", m),
        ClientError::Failed(m) => m,
    })
}

// ── task done ─────────────────────────────────────────────────────────

/// The `task.done` params.
pub fn done_params(env: &EngineEnv, report: &str, seq: String) -> TaskDoneParams {
    TaskDoneParams {
        agent_id: env.agent_id.clone(),
        report: report.to_string(),
        seq,
    }
}

/// Send `report` to the engine as `task.done` and return its result.
pub fn report_done(env: &EngineEnv, report: &str) -> Result<TaskDoneResult, String> {
    if report.trim().is_empty() {
        return Err("task done: the report is empty".into());
    }
    let params = serde_json::to_value(done_params(env, report, seq_now()))
        .map_err(|e| format!("task done: {}", e))?;
    let result =
        call(env, "task.done", params, CALL_TIMEOUT).map_err(|e| format!("task done: {}", e))?;
    serde_json::from_value(result).map_err(|e| format!("task done: bad result: {}", e))
}

/// Read `text`, or stdin when it is `-`.
fn text_or_stdin(text: &str, what: &str) -> Result<String, String> {
    if text != "-" {
        return Ok(text.to_string());
    }
    let mut s = String::new();
    std::io::stdin()
        .read_to_string(&mut s)
        .map_err(|e| format!("cannot read the {} from stdin: {}", what, e))?;
    Ok(s)
}

/// `c9watch-agent task done --report <text|->`: print the engine's result.
pub fn cmd_task_done(report: &str) -> Result<(), String> {
    let env = engine_env(|k| std::env::var(k).ok()).map_err(|e| format!("task done: {}", e))?;
    let report = text_or_stdin(report, "report")?;
    let result = report_done(&env, &report)?;
    println!("{}", serde_json::to_string(&result).unwrap_or_default());
    Ok(())
}

// ── handoff ───────────────────────────────────────────────────────────

/// Send `note` as `agent.handoff`. C1a engines answer
/// `unsupported_in_c1a`.
pub fn handoff(env: &EngineEnv, note: &str) -> Result<Value, String> {
    if note.trim().is_empty() {
        return Err("handoff: the note is empty".into());
    }
    let params = HandoffParams {
        agent_id: env.agent_id.clone(),
        note: note.to_string(),
        seq: seq_now(),
    };
    let params = serde_json::to_value(params).map_err(|e| format!("handoff: {}", e))?;
    call(env, "agent.handoff", params, CALL_TIMEOUT).map_err(|e| format!("handoff: {}", e))
}

/// `c9watch-agent handoff --note <text|->`.
pub fn cmd_handoff(note: &str) -> Result<(), String> {
    let env = engine_env(|k| std::env::var(k).ok()).map_err(|e| format!("handoff: {}", e))?;
    let note = text_or_stdin(note, "note")?;
    let result = handoff(&env, &note)?;
    println!("{}", result);
    Ok(())
}

// ── hook ──────────────────────────────────────────────────────────────

/// Claude Code hook events `hook` knows, and their c9p/1 methods.
pub const HOOK_EVENTS: &[(&str, &str)] = &[
    ("session-start", "hook.session_start"),
    ("stop", "hook.stop"),
    ("permission-request", "hook.permission_request"),
    ("permission-denied", "hook.permission_denied"),
    ("notification", "hook.notification"),
];

/// The c9p/1 method of a hook event (`session-start` → `hook.session_start`).
pub fn hook_method(event: &str) -> Option<&'static str> {
    HOOK_EVENTS
        .iter()
        .find(|(e, _)| *e == event)
        .map(|(_, m)| *m)
}

/// How long `hook <event>` waits for the engine, all in.
pub fn hook_timeout(event: &str) -> Duration {
    if event == "session-start" {
        SESSION_START_TIMEOUT
    } else {
        HOOK_TIMEOUT
    }
}

/// The `hook.*` params for the hook's stdin. Input that is not JSON, or is
/// larger than `MAX_HOOK_INPUT`, goes as `null`.
pub fn hook_params(env: &EngineEnv, kind: &str, input: &[u8], seq: String) -> HookParams {
    let truncated = input.len() > MAX_HOOK_INPUT;
    let input = if truncated {
        Value::Null
    } else {
        serde_json::from_slice(input).unwrap_or(Value::Null)
    };
    HookParams {
        agent_id: env.agent_id.clone(),
        kind: kind.to_string(),
        seq,
        input,
        truncated,
    }
}

/// What the hook prints for a result: only `SessionStart` context, as
/// Claude Code reads it.
pub fn hook_stdout(method: &str, result: &HookResult) -> Option<String> {
    let ctx = result.additional_context.as_ref()?;
    if method != "hook.session_start" || ctx.is_empty() {
        return None;
    }
    Some(
        serde_json::json!({"hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": ctx,
        }})
        .to_string(),
    )
}

/// Run one hook: read up to `MAX_HOOK_INPUT + 1` bytes of `stdin`, send the
/// event, and return what to print. Gives up after `timeout` in all,
/// stdin included. Never fails: a session the engine did not spawn (no
/// env), an unknown event, a missing or slow engine, and a refusal all
/// return `None`. The work runs on its own thread; a call still stuck at
/// the deadline is left behind and dies with the process.
pub fn run_hook(
    event: &str,
    kind: &str,
    stdin: impl Read + Send + 'static,
    get: impl Fn(&str) -> Option<String>,
    timeout: Duration,
) -> Option<String> {
    let deadline = Instant::now() + timeout;
    let env = engine_env(get).ok()?;
    let method = hook_method(event)?;
    let kind = kind.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("hook".into())
        .spawn(move || {
            let mut input = Vec::new();
            let _ = stdin
                .take(MAX_HOOK_INPUT as u64 + 1)
                .read_to_end(&mut input);
            let params = hook_params(&env, &kind, &input, seq_now());
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            let sent = serde_json::to_value(params)
                .map_err(|e| e.to_string())
                .and_then(|p| call(&env, method, p, left))
                .and_then(|r| serde_json::from_value::<HookResult>(r).map_err(|e| e.to_string()));
            let _ = tx.send(sent);
        })
        .ok()?;
    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(result)) => hook_stdout(method, &result),
        Ok(Err(e)) => {
            eprintln!("c9watch-agent hook {}: {}", event, e);
            None
        }
        Err(_) => {
            eprintln!(
                "c9watch-agent hook {}: no answer within {:?}",
                event, timeout
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_proto::wire::{Request, Response};
    use serde_json::json;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    fn env(sock: &str) -> EngineEnv {
        EngineEnv {
            sock: sock.into(),
            agent_id: "agt_x".into(),
        }
    }

    /// A one-connection c9p/1 server: answers `hello`, then answers the
    /// next call with `reply(method, params)`, and returns that call.
    fn serve_one(
        path: &Path,
        reply: impl Fn(&str) -> Result<Value, (&'static str, &'static str)> + Send + 'static,
    ) -> std::thread::JoinHandle<Request> {
        let l = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let (c, _) = l.accept().unwrap();
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut w = c;
            let mut last = Request::default();
            for _ in 0..2 {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let req: Request = serde_json::from_str(line.trim()).unwrap();
                let method = req.method.clone().unwrap();
                let resp = if method == "hello" {
                    Response::ok(
                        req.id.clone(),
                        json!({"apiMajor": 1, "apiMinor": 2, "engineVersion": "0",
                               "machineId": "m", "principal": "agent:agt_x",
                               "capabilities": [], "cursor": null}),
                    )
                } else {
                    match reply(&method) {
                        Ok(v) => Response::ok(req.id.clone(), v),
                        Err((code, msg)) => serde_json::from_value(json!({
                            "v": 1, "id": req.id, "ok": false,
                            "error": {"code": code, "msg": msg, "retryable": false, "hint": "h"}
                        }))
                        .unwrap(),
                    }
                };
                writeln!(w, "{}", serde_json::to_string(&resp).unwrap()).unwrap();
                last = req;
            }
            last
        })
    }

    #[test]
    fn missing_env_is_an_error_that_names_the_vars() {
        let e = engine_env(env_of(&[])).unwrap_err();
        assert!(
            e.contains("C9_ENGINE_SOCK") && e.contains("C9_AGENT_ID"),
            "{e}"
        );
        // No token: D28 Q6=B.
        assert!(!e.contains("TOKEN"), "{e}");
        let e = engine_env(env_of(&[("C9_ENGINE_SOCK", "/s"), ("C9_AGENT_ID", "")])).unwrap_err();
        assert!(
            e.contains("C9_AGENT_ID") && !e.contains("C9_ENGINE_SOCK"),
            "{e}"
        );
        let ok = engine_env(env_of(&[
            ("C9_ENGINE_SOCK", "/s"),
            ("C9_AGENT_ID", "a"),
            ("C9_AGENT_TOKEN", "ignored"),
        ]))
        .unwrap();
        assert_eq!(
            ok,
            EngineEnv {
                sock: "/s".into(),
                agent_id: "a".into(),
            }
        );
    }

    #[test]
    fn done_params_use_the_wire_names() {
        let p = done_params(&env("/s"), "all 3 steps done", "42".into());
        assert_eq!(
            serde_json::to_value(p).unwrap(),
            json!({"agentId": "agt_x", "report": "all 3 steps done", "seq": "42"})
        );
    }

    #[test]
    fn task_done_round_trips_and_surfaces_engine_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("e.sock");
        let h = serve_one(&path, |_| Ok(json!({"taskId": "t1"})));
        let e = env(path.to_str().unwrap());
        assert_eq!(report_done(&e, "r").unwrap().task_id, "t1");
        let req = h.join().unwrap();
        assert_eq!(req.method.as_deref(), Some("task.done"));
        assert_eq!(req.params["agentId"], "agt_x");
        assert_eq!(req.params["report"], "r");

        let path = dir.path().join("e2.sock");
        let h = serve_one(&path, |_| Err(("no_task", "free agent")));
        let err = report_done(&env(path.to_str().unwrap()), "r").unwrap_err();
        assert!(err.contains("no_task"), "{err}");
        h.join().unwrap();
    }

    #[test]
    fn handoff_sends_the_note() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("e.sock");
        let h = serve_one(&path, |_| Err(("unsupported_in_c1a", "C1b")));
        let err = handoff(&env(path.to_str().unwrap()), "next steps").unwrap_err();
        assert!(err.contains("unsupported_in_c1a"), "{err}");
        let req = h.join().unwrap();
        assert_eq!(req.method.as_deref(), Some("agent.handoff"));
        assert_eq!(req.params["note"], "next steps");
        assert!(handoff(&env("/nonexistent"), " ")
            .unwrap_err()
            .contains("empty"));
    }

    #[test]
    fn refused_socket_and_empty_report_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("none.sock");
        let e = report_done(&env(sock.to_str().unwrap()), "r").unwrap_err();
        assert!(e.contains("cannot reach the engine"), "{e}");
        let e = report_done(&env("/nonexistent/e.sock"), "  \n").unwrap_err();
        assert!(e.contains("empty"), "{e}");
    }

    #[test]
    fn hook_events_map_to_methods() {
        assert_eq!(hook_method("session-start"), Some("hook.session_start"));
        assert_eq!(
            hook_method("permission-denied"),
            Some("hook.permission_denied")
        );
        assert_eq!(hook_method("SessionStart"), None);
        for (_, m) in HOOK_EVENTS {
            assert!(crate::engine_proto::wire::HOOK_METHODS.contains(m), "{m}");
        }
        assert_eq!(
            HOOK_EVENTS.len(),
            crate::engine_proto::wire::HOOK_METHODS.len()
        );
    }

    #[test]
    fn hook_params_carry_the_input_or_null() {
        let e = env("/s");
        let p = hook_params(&e, "claude", br#"{"session_id":"s1"}"#, "7".into());
        assert_eq!(
            serde_json::to_value(p).unwrap(),
            json!({"agentId": "agt_x", "kind": "claude", "seq": "7",
                   "input": {"session_id": "s1"}})
        );
        assert_eq!(
            hook_params(&e, "claude", b"not json", "7".into()).input,
            Value::Null
        );
        let big = vec![b' '; MAX_HOOK_INPUT + 1];
        let p = hook_params(&e, "claude", &big, "7".into());
        assert!(p.truncated && p.input.is_null());
    }

    #[test]
    fn only_session_start_prints_its_context() {
        let r = HookResult {
            accepted: true,
            additional_context: Some("orders".into()),
        };
        let out: Value =
            serde_json::from_str(&hook_stdout("hook.session_start", &r).unwrap()).unwrap();
        assert_eq!(
            out,
            json!({"hookSpecificOutput": {"hookEventName": "SessionStart",
                                          "additionalContext": "orders"}})
        );
        assert_eq!(hook_stdout("hook.stop", &r), None);
        assert_eq!(
            hook_stdout("hook.session_start", &HookResult::default()),
            None
        );
    }

    #[test]
    fn only_session_start_waits_longer() {
        assert_eq!(hook_timeout("session-start"), SESSION_START_TIMEOUT);
        for (event, _) in HOOK_EVENTS {
            if *event != "session-start" {
                assert_eq!(hook_timeout(event), HOOK_TIMEOUT, "{}", event);
            }
        }
    }

    #[test]
    fn hook_sends_the_event_and_returns_the_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("e.sock");
        let h = serve_one(&path, |_| {
            Ok(json!({"accepted": true, "additionalContext": "orders"}))
        });
        let sock = path.to_str().unwrap().to_string();
        let out = run_hook(
            "session-start",
            "claude",
            std::io::Cursor::new(br#"{"source":"startup"}"#.to_vec()),
            env_of(&[("C9_ENGINE_SOCK", &sock), ("C9_AGENT_ID", "agt_x")]),
            Duration::from_secs(5),
        );
        assert!(out.unwrap().contains("orders"));
        let req = h.join().unwrap();
        assert_eq!(req.method.as_deref(), Some("hook.session_start"));
        assert_eq!(req.params["input"]["source"], "startup");
    }

    #[test]
    fn hook_without_env_or_with_an_unknown_event_does_nothing() {
        let none = || std::io::Cursor::new(Vec::new());
        assert_eq!(
            run_hook("stop", "claude", none(), env_of(&[]), HOOK_TIMEOUT),
            None
        );
        let env = [
            ("C9_ENGINE_SOCK", "/nonexistent.sock"),
            ("C9_AGENT_ID", "a"),
        ];
        assert_eq!(
            run_hook("nope", "claude", none(), env_of(&env), HOOK_TIMEOUT),
            None
        );
        // No engine: an error, not a hang.
        let t = Instant::now();
        assert_eq!(
            run_hook("stop", "claude", none(), env_of(&env), HOOK_TIMEOUT),
            None
        );
        assert!(t.elapsed() < HOOK_TIMEOUT + Duration::from_millis(200));
    }

    #[test]
    fn a_silent_engine_or_a_stuck_stdin_cost_at_most_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("e.sock");
        // Accepts, never answers.
        let l = UnixListener::bind(&path).unwrap();
        let held = std::thread::spawn(move || {
            let c = l.accept().unwrap();
            std::thread::sleep(Duration::from_secs(3));
            drop(c);
        });
        let sock = path.to_str().unwrap().to_string();
        let env = [("C9_ENGINE_SOCK", sock.as_str()), ("C9_AGENT_ID", "a")];
        let t = Instant::now();
        let out = run_hook(
            "stop",
            "claude",
            std::io::Cursor::new(Vec::new()),
            env_of(&env),
            HOOK_TIMEOUT,
        );
        assert_eq!(out, None);
        assert!(
            t.elapsed() < HOOK_TIMEOUT + Duration::from_millis(200),
            "{:?}",
            t.elapsed()
        );

        // Stdin that never ends.
        struct Stuck;
        impl Read for Stuck {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                std::thread::sleep(Duration::from_secs(5));
                Ok(0)
            }
        }
        let t = Instant::now();
        assert_eq!(
            run_hook("stop", "claude", Stuck, env_of(&env), HOOK_TIMEOUT),
            None
        );
        assert!(
            t.elapsed() < HOOK_TIMEOUT + Duration::from_millis(200),
            "{:?}",
            t.elapsed()
        );
        held.join().unwrap();
    }
}
