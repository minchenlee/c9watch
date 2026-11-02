//! `c9watch send <to> <message>` (C1b-M1-10).
//!
//! Mails one engine agent through `msg.send`. The engine queues the message
//! and pastes it into the agent's input box when the agent is ready; the
//! agent reads it as its next prompt. `to` is an agent id (`agt_<ulid>`),
//! `<machine>/<id>` or a name. The legacy daemon's `send` is gone: a
//! `claude -p` or bg worker takes its prompt only at spawn.
//!
//! The command prints `{"msg": <result>}`, whose `cursor` can be passed to
//! `wait --event turn_ended --since`. An engine error prints
//! `{"error": {code, msg, hint, data?}}` on stderr and exits 1.
//!
//! `--wait` follows `msg.state` and `agent.turn_end` of the target from that
//! cursor. It waits for the message to be `delivered`, then for the next
//! turn end: the agent's answer to the message. It prints a second line and
//! exits 0, or exits 1 when the message ends `failed` or `uncertain`, or 2
//! on timeout. When the stream ends (an `engine restart`) it connects again
//! and goes on from the last cursor.
//!
//! A caller inside an engine agent (`C9_AGENT_ID` set) connects only to
//! `C9_ENGINE_SOCK` and never starts the engine. It may not `--wait`: the
//! wait would hold its Bash call open while the answer to it, a message to
//! this agent, waits for the agent to be ready.

use super::engine_client::{error_line, EngineConn, ENV_AGENT_ID};
use super::engine_sessions::{RECONNECT_EVERY, RESTART_GRACE};
use super::engine_wait::{connect, target, Target};
use crate::engine::proto::{
    ErrorBody, EventFrame, MsgKind, MsgSendParams, MsgSendResult, MsgStateEvent, SubscribeParams,
};
use std::ffi::OsString;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Exit code: the message ended `failed` or `uncertain`.
pub const EXIT_UNDELIVERED: i32 = 1;
/// Exit code: `--wait` timed out.
pub const EXIT_TIMEOUT: i32 = 2;

/// How long `--wait` tries to reach the engine again after the event stream
/// ended: an `engine restart` drains for up to 90 s.
const RECONNECT_FOR: Duration =
    Duration::from_secs(crate::engine::drain::HARD_DEADLINE.as_secs() + 30);

/// `send --kind`: what the message asks of its reader.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SendKind {
    /// The reader should answer
    Request,
    /// For the reader's information
    #[default]
    Info,
}

impl From<SendKind> for MsgKind {
    fn from(k: SendKind) -> Self {
        match k {
            SendKind::Request => MsgKind::Request,
            SendKind::Info => MsgKind::Info,
        }
    }
}

/// `c9watch send` arguments. Exactly one body source is required.
#[derive(clap::Args, Debug)]
#[command(group(
    clap::ArgGroup::new("body")
        .required(true)
        .args(["message", "message_flag", "stdin", "file"])
))]
pub struct SendArgs {
    /// The agent: `agt_<id>`, `<machine>/<id>`, or a name
    pub to: String,
    /// The message text
    pub message: Option<String>,
    /// The message text, as a flag
    #[arg(long = "message", id = "message_flag", value_name = "TEXT")]
    pub message_flag: Option<String>,
    /// Read the message from stdin
    #[arg(long)]
    pub stdin: bool,
    /// Read the message from this file
    #[arg(long)]
    pub file: Option<String>,
    /// What the message asks of its reader
    #[arg(long, value_enum, default_value_t)]
    pub kind: SendKind,
    /// A key that makes a resend safe: the same sender, key and message get
    /// the first message back
    #[arg(long)]
    pub idem: Option<String>,
    /// The id of the message this one answers
    #[arg(long, value_name = "MSG_ID")]
    pub reply_to: Option<String>,
    /// Wait until the agent has read the message and its turn has ended
    /// (exit 1 if it was not delivered, 2 on timeout). Not for agents
    #[arg(long)]
    pub wait: bool,
    /// Give up `--wait` after this many seconds
    #[arg(long, default_value = "300")]
    pub timeout: u64,
}

/// The message text from the one source given, with CRLF line ends made LF.
pub fn body(
    args: &SendArgs,
    read_stdin: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    let text = if let Some(m) = args.message.as_ref().or(args.message_flag.as_ref()) {
        m.clone()
    } else if args.stdin {
        read_stdin()?
    } else if let Some(path) = &args.file {
        std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read message file {:?}: {}", path, e))?
    } else {
        return Err("bad_args: give MESSAGE, --message, --stdin or --file".into());
    };
    Ok(text.replace("\r\n", "\n"))
}

/// Fail when an agent (`C9_AGENT_ID` set) asks to `--wait`.
pub fn check_wait(wait: bool, env: &dyn Fn(&str) -> Option<OsString>) -> Result<(), String> {
    if wait && env(ENV_AGENT_ID).is_some_and(|v| !v.is_empty()) {
        return Err(format!(
            "bad_args: --wait is not for agents ({} is set); end your turn; \
             the reply arrives as a new message",
            ENV_AGENT_ID
        ));
    }
    Ok(())
}

/// The `msg.send` params of `args` with `body`.
pub fn send_params(args: &SendArgs, body: String) -> serde_json::Value {
    serde_json::to_value(MsgSendParams {
        to: args.to.clone(),
        body,
        kind: args.kind.into(),
        reply_to: args.reply_to.clone(),
    })
    .expect("MsgSendParams serializes")
}

/// The `{"error": ...}` line of an engine error response.
pub fn error_json(error: Option<&ErrorBody>) -> serde_json::Value {
    match error {
        Some(e) => serde_json::json!({ "error": e }),
        None => serde_json::json!({ "error": "the engine sent ok:false without an error" }),
    }
}

/// How a `--wait` ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Delivered, and the agent's turn ended after it: the `msg.state`
    /// `delivered` data and the turn end's cursor.
    Answered(MsgStateEvent, String),
    /// The message ended `failed` or `uncertain`: that `msg.state` data.
    Undelivered(MsgStateEvent),
}

/// What `--wait` has seen of one message so far.
#[derive(Debug, Default)]
pub struct Watch {
    delivered: Option<MsgStateEvent>,
    /// A turn end of the target since the last `delivering`, before
    /// `delivered`: the cursor.
    turn_end: Option<String>,
}

impl Watch {
    /// Judge one frame for message `msg_id` to agent `agent_id`.
    pub fn judge(&mut self, msg_id: &str, agent_id: &str, frame: &EventFrame) -> Option<Outcome> {
        match frame.topic.as_str() {
            crate::engine::mail::TOPIC => {
                let ev = serde_json::from_value::<MsgStateEvent>(frame.data.clone()).ok()?;
                if ev.msg_id != msg_id {
                    return None;
                }
                if ev.state == "delivering"
                    && ev.detail.as_deref().is_some_and(|d| d.starts_with("held:"))
                {
                    eprintln!("held by {}'s Claude Code; waiting up to 15 min", agent_id);
                }
                match ev.state.as_str() {
                    "queued" | "delivering" => self.turn_end = None,
                    // Without the prompt hook, the engine marks a message
                    // delivered only once the turn is open, some seconds
                    // after the paste: a short turn may have ended by then.
                    "delivered" => match self.turn_end.take() {
                        Some(cursor) if ev.detail.as_deref() == Some("turn_open") => {
                            return Some(Outcome::Answered(ev, cursor));
                        }
                        _ => self.delivered = Some(ev),
                    },
                    "failed" | "uncertain" | "rejected" | "expired" | "cancelled" => {
                        return Some(Outcome::Undelivered(ev))
                    }
                    _ => {}
                }
                None
            }
            "agent.turn_end" if frame.data.get("id").and_then(|v| v.as_str()) == Some(agent_id) => {
                match self.delivered.take() {
                    Some(ev) => Some(Outcome::Answered(ev, frame.cursor.clone())),
                    None => {
                        self.turn_end = Some(frame.cursor.clone());
                        None
                    }
                }
            }
            _ => None,
        }
    }
}

/// The second line `--wait` prints: the send result with the message's
/// final state, plus the turn end's cursor when it was answered.
pub fn outcome_line(sent: &MsgSendResult, outcome: &Outcome) -> serde_json::Value {
    let (ev, turn_end) = match outcome {
        Outcome::Answered(ev, cursor) => (ev, Some(cursor)),
        Outcome::Undelivered(ev) => (ev, None),
    };
    let mut msg = serde_json::to_value(sent).expect("MsgSendResult serializes");
    msg["state"] = ev.state.clone().into();
    msg["detail"] = ev.detail.clone().into();
    let mut line = serde_json::json!({ "msg": msg });
    if let Some(cursor) = turn_end {
        line["turnEnd"] = cursor.clone().into();
    }
    line
}

fn subscribe_params(cursor: &str, subject: &str) -> serde_json::Value {
    serde_json::to_value(SubscribeParams {
        since: Some(cursor.to_string()),
        topics: vec![crate::engine::mail::TOPIC.into(), "agent.turn_end".into()],
        subjects: vec![subject.to_string()],
    })
    .unwrap_or_default()
}

/// Why one subscription stopped without an outcome.
enum Stop {
    /// The connection ended: connect again.
    Lost,
    /// The engine refused (`cursor_expired`, for one): waiting does not help.
    Refused(String),
}

/// Follow the message from `cursor` on `conn` and on new connections after
/// it, until an outcome.
fn follow(target: &Target, mut conn: EngineConn, sent: &MsgSendResult) -> Result<Outcome, String> {
    let mut cursor = sent.cursor.clone();
    let mut watch = Watch::default();
    loop {
        let opened_at = Instant::now();
        let stop = (|| {
            let resp = conn
                .call("events.subscribe", subscribe_params(&cursor, &sent.to.addr))
                .map_err(|_| Stop::Lost)?;
            if !resp.ok {
                return Err(Stop::Refused(error_line(&resp)));
            }
            let frames = conn.into_frames().map_err(|_| Stop::Lost)?;
            for line in frames.lines.iter() {
                // A response frame here is the subscription's error:
                // subscribe again from the last cursor.
                let Ok(frame) = serde_json::from_str::<EventFrame>(&line) else {
                    break;
                };
                cursor = frame.cursor.clone();
                if let Some(outcome) = watch.judge(&sent.msg_id, &sent.to.id, &frame) {
                    return Ok(outcome);
                }
            }
            Err(Stop::Lost)
        })();
        match stop {
            Ok(outcome) => return Ok(outcome),
            Err(Stop::Refused(e)) => return Err(e),
            Err(Stop::Lost) => {}
        }
        // A stream that ends at once is not reopened in a busy loop.
        if opened_at.elapsed() < RECONNECT_EVERY {
            std::thread::sleep(RECONNECT_EVERY);
        }
        // For the first `RESTART_GRACE` only connect, so an `engine
        // restart` starts the new engine, not this command.
        let lost_at = Instant::now();
        conn = loop {
            match connect(target, lost_at.elapsed() >= RESTART_GRACE) {
                Ok(conn) => break conn,
                Err(e) if lost_at.elapsed() >= RECONNECT_FOR => return Err(e),
                Err(_) => std::thread::sleep(RECONNECT_EVERY),
            }
        };
    }
}

/// `c9watch send`. Returns the exit code: 0 sent (with `--wait`: answered),
/// 1 an engine error or (with `--wait`) not delivered, 2 `--wait` timed out.
pub fn cmd_send(args: &SendArgs, pretty: bool) -> Result<i32, String> {
    let env = |k: &str| std::env::var_os(k);
    check_wait(args.wait, &env)?;
    let body = body(args, || {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("Failed to read from stdin: {}", e))?;
        Ok(buf)
    })?;
    let target = target(&env)?;
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    let mut conn = connect(&target, true)?;
    let resp = conn.call_idem("msg.send", send_params(args, body), args.idem.as_deref())?;
    if !resp.ok {
        eprintln!("{}", error_json(resp.error.as_ref()));
        return Ok(1);
    }
    let sent: MsgSendResult = serde_json::from_value(resp.result.unwrap_or_default())
        .map_err(|e| format!("Bad msg.send result from the engine: {}", e))?;
    super::print_json(&serde_json::json!({ "msg": sent }), pretty)?;
    if !args.wait {
        return Ok(0);
    }

    let (tx, rx) = mpsc::channel();
    let followed = sent.clone();
    std::thread::spawn(move || {
        let _ = tx.send(follow(&target, conn, &followed));
    });
    let outcome = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(outcome) => outcome?,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            eprintln!(
                "{}",
                serde_json::json!({ "error": format!(
                    "timeout: {} was not answered within {} s", sent.msg_id, args.timeout
                ) })
            );
            return Ok(EXIT_TIMEOUT);
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Err("The engine feed stopped".to_string())
        }
    };
    super::print_json(&outcome_line(&sent, &outcome), pretty)?;
    Ok(match outcome {
        Outcome::Answered(..) => 0,
        Outcome::Undelivered(_) => EXIT_UNDELIVERED,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::collections::HashMap;

    #[derive(Parser, Debug)]
    struct Cmd {
        #[command(flatten)]
        args: SendArgs,
    }

    fn parse(argv: &[&str]) -> Result<SendArgs, clap::Error> {
        Cmd::try_parse_from(std::iter::once("send").chain(argv.iter().copied())).map(|c| c.args)
    }

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let m: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |k| m.get(k).cloned()
    }

    fn no_stdin() -> Result<String, String> {
        panic!("stdin is not read")
    }

    #[test]
    fn exactly_one_body_source_is_taken() {
        let a = parse(&["agt_1", "hi"]).unwrap();
        assert_eq!(body(&a, no_stdin).unwrap(), "hi");
        let a = parse(&["agt_1", "--message", "hi"]).unwrap();
        assert_eq!(body(&a, no_stdin).unwrap(), "hi");
        let a = parse(&["agt_1", "--stdin"]).unwrap();
        assert_eq!(body(&a, || Ok("a\r\nb\n".into())).unwrap(), "a\nb\n");

        for argv in [
            &["agt_1"][..],
            &["agt_1", "hi", "--message", "ho"],
            &["agt_1", "hi", "--stdin"],
            &["agt_1", "--message", "hi", "--file", "f"],
            &["agt_1", "--stdin", "--file", "f"],
        ] {
            assert!(parse(argv).is_err(), "{:?} parses", argv);
        }
    }

    #[test]
    fn the_flags_parse() {
        let a = parse(&[
            "pm/agt_1",
            "hi",
            "--kind",
            "request",
            "--idem",
            "k1",
            "--reply-to",
            "msg_9",
            "--wait",
            "--timeout",
            "5",
        ])
        .unwrap();
        assert_eq!(a.kind, SendKind::Request);
        assert_eq!(a.idem.as_deref(), Some("k1"));
        assert!(a.wait);
        assert_eq!(a.timeout, 5);
        let p = send_params(&a, "hi".into());
        assert_eq!(
            p,
            serde_json::json!({"to": "pm/agt_1", "body": "hi", "kind": "request", "replyTo": "msg_9"})
        );
        let a = parse(&["helper", "hi"]).unwrap();
        assert_eq!((a.kind, a.timeout, a.wait), (SendKind::Info, 300, false));
        assert!(parse(&["helper", "hi", "--kind", "result"]).is_err());
    }

    #[test]
    fn an_agent_may_not_wait() {
        let agent = env_of(&[(ENV_AGENT_ID, "agt_1")]);
        let e = check_wait(true, &agent).unwrap_err();
        assert!(e.starts_with("bad_args:"), "{}", e);
        assert!(
            e.contains("end your turn; the reply arrives as a new message"),
            "{}",
            e
        );
        assert!(check_wait(false, &agent).is_ok());
        assert!(check_wait(true, &env_of(&[])).is_ok());
        assert!(check_wait(true, &env_of(&[(ENV_AGENT_ID, "")])).is_ok());
    }

    #[test]
    fn an_engine_error_keeps_its_data() {
        let e: ErrorBody = serde_json::from_value(serde_json::json!({
            "code": "unknown_address", "msg": "no agent", "retryable": false,
            "hint": "check the name", "data": {"nearest": "helper"},
        }))
        .unwrap();
        let v = error_json(Some(&e));
        assert_eq!(v["error"]["code"], "unknown_address");
        assert_eq!(v["error"]["data"]["nearest"], "helper");
    }

    fn state(msg: &str, state: &str, detail: Option<&str>) -> EventFrame {
        frame(
            crate::engine::mail::TOPIC,
            serde_json::json!({
                "msgId": msg, "from": "local-unverified", "to": "agt_1", "state": state,
                "at": 1, "attempts": 1, "detail": detail,
            }),
        )
    }

    fn frame(topic: &str, data: serde_json::Value) -> EventFrame {
        EventFrame {
            v: 1,
            kind: crate::engine::proto::EventType::Event,
            sub: Some("2".into()),
            cursor: format!("m:e:{}", topic.len()),
            topic: topic.into(),
            subject: Some("m/agt_1".into()),
            ts: 0,
            origin: None,
            data,
        }
    }

    fn turn_end(id: &str) -> EventFrame {
        frame(
            "agent.turn_end",
            serde_json::json!({"id": id, "generation": 1}),
        )
    }

    #[test]
    fn only_a_turn_end_after_delivery_answers() {
        let mut w = Watch::default();
        let judge = |w: &mut Watch, f: EventFrame| w.judge("msg_1", "agt_1", &f);
        assert_eq!(judge(&mut w, turn_end("agt_1")), None);
        assert_eq!(judge(&mut w, state("msg_1", "delivering", None)), None);
        assert_eq!(
            judge(&mut w, state("msg_1", "delivered", Some("prompt_hook"))),
            None
        );
        assert_eq!(judge(&mut w, state("msg_2", "failed", None)), None);
        assert_eq!(judge(&mut w, turn_end("agt_2")), None);
        match judge(&mut w, turn_end("agt_1")) {
            Some(Outcome::Answered(ev, _)) => assert_eq!(ev.detail.as_deref(), Some("prompt_hook")),
            o => panic!("{:?}", o),
        }
    }

    #[test]
    fn a_turn_open_delivery_takes_a_turn_end_since_the_paste() {
        let mut w = Watch::default();
        let judge = |w: &mut Watch, f: EventFrame| w.judge("msg_1", "agt_1", &f);
        assert_eq!(judge(&mut w, turn_end("agt_1")), None);
        assert_eq!(judge(&mut w, state("msg_1", "delivering", None)), None);
        assert_eq!(judge(&mut w, turn_end("agt_1")), None);
        assert!(matches!(
            judge(&mut w, state("msg_1", "delivered", Some("turn_open"))),
            Some(Outcome::Answered(..))
        ));
    }

    #[test]
    fn an_uncertain_or_failed_message_ends_the_wait() {
        for s in ["uncertain", "failed"] {
            let mut w = Watch::default();
            let o = w.judge("msg_1", "agt_1", &state("msg_1", s, Some("pty gone")));
            let Some(Outcome::Undelivered(ev)) = o else {
                panic!("{:?}", o)
            };
            let sent = MsgSendResult {
                msg_id: "msg_1".into(),
                state: "queued".into(),
                to: crate::engine::proto::MsgAddress {
                    id: "agt_1".into(),
                    name: None,
                    addr: "m/agt_1".into(),
                },
                cursor: "m:e:1".into(),
                replayed: false,
                paused: None,
            };
            let line = outcome_line(&sent, &Outcome::Undelivered(ev));
            assert_eq!(line["msg"]["state"], s);
            assert_eq!(line["msg"]["detail"], "pty gone");
            assert!(line.get("turnEnd").is_none());
        }
    }
}

#[cfg(test)]
mod m2_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn held_waits_and_rejected_expired_cancelled_exit_one() {
        for state in ["rejected", "expired", "cancelled", "delivering"] {
            let data = json!({"msgId":"msg_one","from":"agent:a","to":"b","state":state,"at":1,"attempts":1,"detail":"held: test"});
            let frame: EventFrame = serde_json::from_value(
                json!({"v":1,"type":"event","topic":"msg.state","cursor":"m:e:1","ts":1,"data":data}),
            )
            .unwrap();
            let outcome = Watch::default().judge("msg_one", "b", &frame);
            if state == "delivering" {
                assert!(outcome.is_none());
            } else {
                assert!(matches!(outcome, Some(Outcome::Undelivered(_))));
            }
        }
    }
}
