//! c9p/1 wire types and NDJSON framing.
//!
//! These serde types are the single source of truth for the protocol
//! (A:245): one JSON object per line, a request/response envelope, typed
//! error codes with a hint, and the `hello` handshake. Within major 1 every
//! change is additive, so unknown fields are ignored on input.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// Protocol major version. It is also in the socket name (`engine-v1.sock`).
pub const API_MAJOR: u32 = 1;
/// Protocol minor version. Raised for additive changes.
pub const API_MINOR: u32 = 0;
/// Largest frame the engine reads, without the newline.
pub const MAX_FRAME: usize = 1 << 20;

/// A request frame. Every field is optional on the wire, so that a frame
/// with a missing field still gets an error that echoes its `id`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Request {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idem: Option<String>,
}

/// A response frame: `ok:true` with `result`, or `ok:false` with `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Response {
    pub v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
    /// Cursor of the event this write produced (from M2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<String>,
}

impl Response {
    pub fn ok(id: Option<String>, result: serde_json::Value) -> Self {
        Self {
            v: API_MAJOR,
            id,
            ok: true,
            result: Some(result),
            error: None,
            seq: None,
        }
    }

    pub fn err(id: Option<String>, error: ApiError) -> Self {
        Self {
            v: API_MAJOR,
            id,
            ok: false,
            result: None,
            error: Some(error.into()),
            seq: None,
        }
    }
}

/// The `error` object of a failed response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub msg: String,
    pub retryable: bool,
    pub hint: String,
}

/// Typed error codes. New codes are additive; a client that does not know a
/// code still has `msg`, `retryable` and `hint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The client speaks another major version.
    UpgradeRequired,
    /// The first frame was not `hello`.
    HelloRequired,
    /// No `hello` arrived in time.
    HelloTimeout,
    /// A frame is larger than `MAX_FRAME`.
    FrameTooLarge,
    /// The frame is not valid JSON or misses a required field.
    BadRequest,
    /// The engine has no such method.
    UnknownMethod,
    /// The peer runs under another uid.
    PeerUidMismatch,
    /// The cursor's events are gone: pruned, another epoch or machine, or
    /// ahead of the log.
    CursorExpired,
    /// An engine-side failure.
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::UpgradeRequired => "upgrade_required",
            ErrorCode::HelloRequired => "hello_required",
            ErrorCode::HelloTimeout => "hello_timeout",
            ErrorCode::FrameTooLarge => "frame_too_large",
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::UnknownMethod => "unknown_method",
            ErrorCode::PeerUidMismatch => "peer_uid_mismatch",
            ErrorCode::CursorExpired => "cursor_expired",
            ErrorCode::Internal => "internal",
        }
    }

    /// Whether the same request can succeed when it is sent again unchanged.
    pub fn retryable(self) -> bool {
        matches!(self, ErrorCode::HelloTimeout | ErrorCode::Internal)
    }

    /// What the caller should do next.
    pub fn hint(self) -> &'static str {
        match self {
            ErrorCode::UpgradeRequired => {
                "update the client, or restart the engine with `c9watch engine restart`"
            }
            ErrorCode::HelloRequired => {
                "send {\"v\":1,\"id\":\"1\",\"method\":\"hello\",\"params\":{}} as the first frame"
            }
            ErrorCode::HelloTimeout => "reconnect and send `hello` within 5 s",
            ErrorCode::FrameTooLarge => "keep each frame under 1 MiB",
            ErrorCode::BadRequest => {
                "send one JSON object per line with `v`, `id`, `method` and `params`"
            }
            ErrorCode::UnknownMethod => "see `capabilities` in the hello result",
            ErrorCode::PeerUidMismatch => "connect as the user that runs the engine",
            ErrorCode::CursorExpired => {
                "call `snapshot.get`, then `events.subscribe` with the snapshot's cursor"
            }
            ErrorCode::Internal => "retry; if it persists, check the engine log",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A typed error, before it goes on the wire.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub code: ErrorCode,
    pub msg: String,
}

impl ApiError {
    pub fn new(code: ErrorCode, msg: impl Into<String>) -> Self {
        Self {
            code,
            msg: msg.into(),
        }
    }
}

impl From<ApiError> for ErrorBody {
    fn from(e: ApiError) -> Self {
        ErrorBody {
            code: e.code,
            msg: e.msg,
            retryable: e.code.retryable(),
            hint: e.code.hint().to_string(),
        }
    }
}

/// `hello` params. `client` is the caller's own claim. The engine only logs
/// it as unverified; it never decides the principal (N1).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HelloParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_major: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub want: Vec<String>,
}

/// What a client says about itself in `hello`. Unverified.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ClientInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// `hello` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HelloResult {
    pub api_major: u32,
    pub api_minor: u32,
    pub engine_version: String,
    pub machine_id: String,
    /// Stamped by the engine from connection facts.
    pub principal: String,
    pub capabilities: Vec<String>,
    /// Cursor of the newest committed event. `null` only without a store.
    pub cursor: Option<String>,
}

/// `snapshot.get` params. There are none yet; the engine ignores any it
/// gets.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotParams {}

/// `snapshot.get` result: the state and the cursor it is consistent with.
/// Subscribing with `since` = `cursor` continues with no gap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotResult {
    pub cursor: String,
    pub machine_id: String,
    /// Rows of the `agents` table, keyed by column name.
    pub agents: Vec<serde_json::Value>,
}

/// `events.subscribe` params (A:218). Empty filters match everything.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeParams {
    /// The last cursor the client has seen. Without it, the stream starts at
    /// the current head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// Exact topics, or a prefix ending in `.*` (`agent.*`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
    /// Exact subjects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subjects: Vec<String>,
}

/// `events.subscribe` result. Event frames follow it on the same
/// connection until the connection closes, or until an error frame with
/// the subscribe request's `id` (`cursor_expired` when the subscriber fell
/// behind retention).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeResult {
    /// Where the stream starts: events after this cursor follow.
    pub cursor: String,
}

/// One event on the wire (A:203). `sub` is the `id` of the subscribe
/// request, so a connection can carry several subscriptions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventFrame {
    pub v: u32,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
    pub cursor: String,
    pub topic: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub ts: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<serde_json::Value>,
    pub data: serde_json::Value,
}

impl EventFrame {
    pub const KIND: &'static str = "event";
}

/// Why `read_frame` could not return a frame.
#[derive(Debug)]
pub enum FrameError {
    /// The frame grew past the limit before its newline.
    TooLarge,
    Io(std::io::Error),
}

/// Read one NDJSON frame without its line ending. Returns `None` at end of
/// stream. A last frame without a trailing newline still counts. Never
/// buffers more than `max` bytes plus one read: past the limit it fails with
/// `TooLarge`, and the caller must close the connection.
pub async fn read_frame<R: AsyncBufRead + Unpin>(
    r: &mut R,
    max: usize,
) -> Result<Option<Vec<u8>>, FrameError> {
    let mut frame = Vec::new();
    loop {
        let buf = r.fill_buf().await.map_err(FrameError::Io)?;
        if buf.is_empty() {
            return Ok((!frame.is_empty()).then_some(frame));
        }
        let (chunk, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..i], i + 1),
            None => (buf, buf.len()),
        };
        let found_newline = done != chunk.len();
        if frame.len() + chunk.len() > max {
            return Err(FrameError::TooLarge);
        }
        frame.extend_from_slice(chunk);
        r.consume(done);
        if found_newline {
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            return Ok(Some(frame));
        }
    }
}

/// Serialize `resp` as one NDJSON line.
pub fn encode(resp: &Response) -> Vec<u8> {
    encode_line(resp)
}

/// Serialize an event frame as one NDJSON line.
pub fn encode_event(ev: &EventFrame) -> Vec<u8> {
    encode_line(ev)
}

fn encode_line<T: Serialize>(v: &T) -> Vec<u8> {
    // Frames hold only strings, numbers and JSON values, so serialization
    // cannot fail.
    let mut line = serde_json::to_vec(v).expect("frame serializes");
    line.push(b'\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn frames(input: &[u8], max: usize) -> Vec<Result<Vec<u8>, String>> {
        let mut r = tokio::io::BufReader::with_capacity(16, input);
        let mut out = Vec::new();
        loop {
            match read_frame(&mut r, max).await {
                Ok(Some(f)) => out.push(Ok(f)),
                Ok(None) => return out,
                Err(FrameError::TooLarge) => {
                    out.push(Err("too large".into()));
                    return out;
                }
                Err(FrameError::Io(e)) => panic!("{}", e),
            }
        }
    }

    #[tokio::test]
    async fn frames_split_on_newlines_across_reads() {
        let got = frames(b"{\"a\":1}\r\n{\"b\":\"0123456789abcdef\"}\nlast", 64).await;
        assert_eq!(
            got,
            vec![
                Ok(b"{\"a\":1}".to_vec()),
                Ok(b"{\"b\":\"0123456789abcdef\"}".to_vec()),
                Ok(b"last".to_vec()),
            ]
        );
    }

    #[tokio::test]
    async fn frame_over_the_limit_fails() {
        // Exactly at the limit is fine, one byte more is not.
        let ok = [vec![b'x'; 40], b"\n".to_vec()].concat();
        assert_eq!(frames(&ok, 40).await, vec![Ok(vec![b'x'; 40])]);
        let big = [vec![b'x'; 41], b"\n".to_vec()].concat();
        assert_eq!(frames(&big, 40).await, vec![Err("too large".into())]);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let req: Request = serde_json::from_value(json!({
            "v": 1, "id": "7", "method": "hello", "future": {"x": 1},
            "params": {"apiMajor": 1, "client": {"kind": "cli", "extra": true}, "proof": "z"}
        }))
        .unwrap();
        assert_eq!(req.method.as_deref(), Some("hello"));
        let hello: HelloParams = serde_json::from_value(req.params).unwrap();
        assert_eq!(hello.api_major, Some(1));
        assert_eq!(hello.client.unwrap().kind.as_deref(), Some("cli"));
    }

    #[test]
    fn error_wire_format_has_code_and_hint() {
        let resp = Response::err(
            Some("1".into()),
            ApiError::new(ErrorCode::UpgradeRequired, "client speaks c9p/2"),
        );
        let v: serde_json::Value = serde_json::from_slice(&encode(&resp)).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["id"], "1");
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "upgrade_required");
        assert_eq!(v["error"]["retryable"], false);
        assert!(!v["error"]["hint"].as_str().unwrap().is_empty());
        assert!(v.get("result").is_none());
    }

    #[test]
    fn error_code_names_match_serde() {
        for code in [
            ErrorCode::UpgradeRequired,
            ErrorCode::HelloRequired,
            ErrorCode::HelloTimeout,
            ErrorCode::FrameTooLarge,
            ErrorCode::BadRequest,
            ErrorCode::UnknownMethod,
            ErrorCode::PeerUidMismatch,
            ErrorCode::CursorExpired,
            ErrorCode::Internal,
        ] {
            assert_eq!(serde_json::to_value(code).unwrap(), code.as_str());
            assert!(!code.hint().is_empty());
        }
    }

    #[test]
    fn hello_result_uses_camel_case() {
        let r = HelloResult {
            api_major: 1,
            api_minor: 0,
            engine_version: "0.0.0".into(),
            machine_id: "m".into(),
            principal: "local-unverified".into(),
            capabilities: vec![],
            cursor: None,
        };
        let v = serde_json::to_value(r).unwrap();
        for key in [
            "apiMajor",
            "apiMinor",
            "engineVersion",
            "machineId",
            "principal",
            "capabilities",
            "cursor",
        ] {
            assert!(v.get(key).is_some(), "missing {}", key);
        }
    }
}
