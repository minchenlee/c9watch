//! c9p/1 NDJSON framing. The wire types live in `engine_proto::wire`
//! (outside the `engine` feature, so the GUI can use them) and are
//! re-exported here.

pub use crate::engine_proto::wire::*;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

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

fn encode_line<T: serde::Serialize>(v: &T) -> Vec<u8> {
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
            ErrorCode::RateLimited,
            ErrorCode::PeerNotAuthorized,
            ErrorCode::NoTask,
            ErrorCode::UnsupportedInC1a,
            ErrorCode::AgentBinMissing,
            ErrorCode::AgentBinMismatch,
            ErrorCode::ExperimentalDisabled,
            ErrorCode::NotAllowed,
            ErrorCode::CwdCollision,
            ErrorCode::SpawnFailed,
            ErrorCode::UnknownAgent,
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
