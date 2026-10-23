//! Wire types for the CC daemon control.sock JSON-RPC protocol.
//!
//! Protocol: newline-delimited JSON over Unix socket. Every request and reply
//! is one JSON object terminated by `\n`. Confirmed working against CC 2.1.150.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Request {
    Subscribe { short: String },
    Reply { short: String, text: String },
    Kill { short: String },
    Nudge { short: String },
    Ping,
}

impl Request {
    /// Wrap with `proto:1` and serialize as one JSON line (NO trailing newline —
    /// caller appends `\n` before send).
    pub fn to_wire(&self) -> String {
        let mut v = serde_json::to_value(self).expect("Request always serializes");
        v.as_object_mut()
            .unwrap()
            .insert("proto".to_string(), serde_json::json!(1));
        serde_json::to_string(&v).expect("Value always serializes")
    }
}

#[derive(Debug, Deserialize)]
pub struct OneShotReply {
    pub ok: bool,
    pub op: Option<String>,
    pub error: Option<String>,
    pub code: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SubscribeEvent {
    Snapshot {
        record: serde_json::Value,
        #[serde(default)]
        stream_tail: Vec<String>,
    },
    State {
        patch: StatePatch,
    },
    Stream {
        #[allow(dead_code)]
        line: String,
    },
}

impl SubscribeEvent {
    /// Decode the `StatePatch` payloads carried by this event, in the order
    /// they must be applied to a settle detector.
    ///
    /// - `Snapshot`: the nested `record.currentState` patch first, then the
    ///   whole record read as a patch. Either is dropped if it fails to parse.
    /// - `State`: its single patch.
    /// - `Stream`: none.
    pub fn state_patches(&self) -> Vec<StatePatch> {
        match self {
            SubscribeEvent::Snapshot { record, .. } => {
                let nested = record
                    .get("currentState")
                    .and_then(|v| serde_json::from_value::<StatePatch>(v.clone()).ok());
                let from_record = serde_json::from_value::<StatePatch>(record.clone()).ok();
                nested.into_iter().chain(from_record).collect()
            }
            SubscribeEvent::State { patch } => vec![patch.clone()],
            SubscribeEvent::Stream { .. } => Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct StatePatch {
    pub state: Option<String>,
    pub tempo: Option<String>,
    pub needs: Option<String>,
    pub detail: Option<String>,
    pub pid: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_serializes_with_proto() {
        let req = Request::Subscribe {
            short: "abc12345".to_string(),
        };
        let wire = req.to_wire();
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["proto"], 1);
        assert_eq!(v["op"], "subscribe");
        assert_eq!(v["short"], "abc12345");
    }

    #[test]
    fn reply_uses_text_field() {
        let req = Request::Reply {
            short: "abc12345".to_string(),
            text: "go".to_string(),
        };
        let wire = req.to_wire();
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["op"], "reply");
        assert_eq!(v["text"], "go");
        // Negative: must NOT serialize as "message" / "prompt" / "input"
        // — those were rejected by the daemon during probing.
        assert!(v.get("message").is_none());
    }

    #[test]
    fn parse_kill_ok_reply() {
        let reply: OneShotReply = serde_json::from_str(r#"{"ok":true,"op":"kill"}"#).unwrap();
        assert!(reply.ok);
        assert_eq!(reply.op.as_deref(), Some("kill"));
    }

    #[test]
    fn parse_error_reply() {
        let reply: OneShotReply = serde_json::from_str(
            r#"{"ok":false,"error":"malformed request: Invalid input","code":"EUNKNOWN"}"#,
        )
        .unwrap();
        assert!(!reply.ok);
        assert_eq!(
            reply.error.as_deref(),
            Some("malformed request: Invalid input")
        );
    }

    #[test]
    fn parse_state_event_with_blocked() {
        let raw = r#"{"type":"state","patch":{"state":"blocked","tempo":"blocked","needs":"user input"}}"#;
        let ev: SubscribeEvent = serde_json::from_str(raw).unwrap();
        match ev {
            SubscribeEvent::State { patch } => {
                assert_eq!(patch.state.as_deref(), Some("blocked"));
                assert_eq!(patch.needs.as_deref(), Some("user input"));
            }
            _ => panic!("expected State event"),
        }
    }

    #[test]
    fn parse_pid_only_state_event() {
        // From Phase 0: mid-turn the daemon sometimes emits pid-only patches.
        // Must parse without error; state/tempo will be None.
        let raw = r#"{"type":"state","patch":{"pid":28418}}"#;
        let ev: SubscribeEvent = serde_json::from_str(raw).unwrap();
        match ev {
            SubscribeEvent::State { patch } => {
                assert_eq!(patch.pid, Some(28418));
                assert!(patch.state.is_none());
            }
            _ => panic!("expected State event"),
        }
    }

    #[test]
    fn parse_stream_event_discarded() {
        let raw = r#"{"type":"stream","line":"[2J[H"}"#;
        let ev: SubscribeEvent = serde_json::from_str(raw).unwrap();
        assert!(matches!(ev, SubscribeEvent::Stream { .. }));
    }

    #[test]
    fn snapshot_state_patches_nested_first_then_record() {
        let raw = r#"{"type":"snapshot","record":{"tempo":"idle","pid":7,"currentState":{"state":"working","tempo":"active"}}}"#;
        let ev: SubscribeEvent = serde_json::from_str(raw).unwrap();
        let patches = ev.state_patches();
        assert_eq!(patches.len(), 2);
        assert_eq!(patches[0].state.as_deref(), Some("working"));
        assert_eq!(patches[0].tempo.as_deref(), Some("active"));
        assert_eq!(patches[0].pid, None);
        assert_eq!(patches[1].tempo.as_deref(), Some("idle"));
        assert_eq!(patches[1].pid, Some(7));
    }

    #[test]
    fn snapshot_without_current_state_gives_record_patch_only() {
        let raw = r#"{"type":"snapshot","record":{"tempo":"idle"}}"#;
        let ev: SubscribeEvent = serde_json::from_str(raw).unwrap();
        let patches = ev.state_patches();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].tempo.as_deref(), Some("idle"));
    }

    #[test]
    fn snapshot_drops_unparseable_patches() {
        let raw = r#"{"type":"snapshot","record":{"currentState":"bogus","pid":"not-a-number"}}"#;
        let ev: SubscribeEvent = serde_json::from_str(raw).unwrap();
        assert!(ev.state_patches().is_empty());
    }

    #[test]
    fn state_event_gives_one_patch() {
        let raw = r#"{"type":"state","patch":{"state":"blocked","needs":"approve"}}"#;
        let ev: SubscribeEvent = serde_json::from_str(raw).unwrap();
        let patches = ev.state_patches();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].state.as_deref(), Some("blocked"));
        assert_eq!(patches[0].needs.as_deref(), Some("approve"));
    }

    #[test]
    fn stream_event_gives_no_patches() {
        let raw = r#"{"type":"stream","line":"hello"}"#;
        let ev: SubscribeEvent = serde_json::from_str(raw).unwrap();
        assert!(ev.state_patches().is_empty());
    }
}
