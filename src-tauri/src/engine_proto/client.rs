//! A small blocking c9p/1 client for callers built without the `engine`
//! feature (the dev GUI's shadow diff).
//!
//! It only connects to an engine that is already running: it never starts
//! one. The server must run under our euid, as in `cli/engine_client.rs`.
//! Every read and write has a timeout, so a stuck engine costs the caller
//! at most that long.

use super::peer::{check_peer_uid, engine_uid, peer_cred};
use super::wire::{
    AgentRow, ClientInfo, HelloParams, HelloResult, Request, Response, SessionListResult,
    API_MAJOR, MAX_FRAME,
};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// Why a call did not return a result.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientError {
    /// No engine answers on the socket.
    Unreachable(String),
    /// The engine answered, but with an error or a frame we cannot read.
    Failed(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Unreachable(m) => write!(f, "unreachable: {}", m),
            ClientError::Failed(m) => write!(f, "failed: {}", m),
        }
    }
}

/// One c9p/1 connection after a successful `hello`.
pub struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    pub hello: HelloResult,
}

impl Conn {
    /// Connect to `sock`, check the server's uid and send `hello` with
    /// `kind` as the client kind (logged by the engine, never trusted).
    pub fn connect(sock: &Path, kind: &str, timeout: Duration) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(sock)
            .map_err(|e| ClientError::Unreachable(format!("{:?}: {}", sock, e)))?;
        let server = peer_cred(stream.as_raw_fd()).map_err(ClientError::Failed)?;
        check_peer_uid(&server, engine_uid()).map_err(ClientError::Failed)?;
        let _ = stream.set_read_timeout(Some(timeout));
        let _ = stream.set_write_timeout(Some(timeout));
        let writer = stream
            .try_clone()
            .map_err(|e| ClientError::Unreachable(format!("Failed to clone socket: {}", e)))?;
        let mut conn = Conn {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
            hello: HelloResult {
                api_major: 0,
                api_minor: 0,
                engine_version: String::new(),
                machine_id: String::new(),
                principal: String::new(),
                capabilities: Vec::new(),
                cursor: None,
            },
        };
        let params = HelloParams {
            api_major: Some(API_MAJOR),
            client: Some(ClientInfo {
                kind: Some(kind.to_string()),
                name: Some("c9watch".into()),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            want: Vec::new(),
        };
        let params = serde_json::to_value(params)
            .map_err(|e| ClientError::Failed(format!("Failed to encode hello: {}", e)))?;
        let result = conn.call("hello", params)?;
        conn.hello = serde_json::from_value(result)
            .map_err(|e| ClientError::Failed(format!("bad hello result: {}", e)))?;
        Ok(conn)
    }

    /// Send one request and return its `result`. An `ok:false` response is
    /// `Failed` with `"<code>: <msg>"`.
    pub fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        let id = self.next_id.to_string();
        self.next_id += 1;
        let req = Request {
            v: Some(API_MAJOR),
            id: Some(id.clone()),
            method: Some(method.to_string()),
            params,
            idem: None,
        };
        let mut line = serde_json::to_vec(&req)
            .map_err(|e| ClientError::Failed(format!("Failed to encode the request: {}", e)))?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .map_err(|e| ClientError::Unreachable(format!("write: {}", e)))?;
        let mut buf = String::new();
        let n = (&mut self.reader)
            .take(MAX_FRAME as u64 + 1)
            .read_line(&mut buf)
            .map_err(|e| ClientError::Unreachable(format!("read: {}", e)))?;
        if n == 0 {
            return Err(ClientError::Unreachable(
                "the engine closed the connection".into(),
            ));
        }
        if !buf.ends_with('\n') {
            return Err(ClientError::Failed("response frame too large".into()));
        }
        let resp: Response = serde_json::from_str(buf.trim_end())
            .map_err(|e| ClientError::Failed(format!("bad response: {}", e)))?;
        if resp.id.as_deref() != Some(id.as_str()) {
            return Err(ClientError::Failed(format!(
                "response id {:?} does not match request {}",
                resp.id, id
            )));
        }
        match (resp.ok, resp.result, resp.error) {
            (true, Some(result), _) => Ok(result),
            (true, None, _) => Ok(serde_json::Value::Null),
            (false, _, Some(e)) => Err(ClientError::Failed(format!("{}: {}", e.code, e.msg))),
            (false, _, None) => Err(ClientError::Failed(
                "the engine sent ok:false without an error".into(),
            )),
        }
    }
}

/// Connect to `sock` as `kind` and return the engine's `session.list`.
pub fn session_list(
    sock: &Path,
    kind: &str,
    timeout: Duration,
) -> Result<Vec<AgentRow>, ClientError> {
    let mut conn = Conn::connect(sock, kind, timeout)?;
    let result = conn.call("session.list", serde_json::json!({}))?;
    let list: SessionListResult = serde_json::from_value(result)
        .map_err(|e| ClientError::Failed(format!("bad session.list result: {}", e)))?;
    Ok(list.sessions)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    /// A fake engine on `<dir>/engine-v1.sock` that answers `hello`, then
    /// `session.list` with `rows`, for `conns` connections.
    pub(crate) fn fake_engine(
        dir: &Path,
        rows: Vec<AgentRow>,
        conns: usize,
    ) -> (PathBuf, std::thread::JoinHandle<()>) {
        let sock = dir.join("engine-v1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let handle = std::thread::spawn(move || {
            for _ in 0..conns {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                let mut line = String::new();
                while {
                    line.clear();
                    reader.read_line(&mut line).unwrap() > 0
                } {
                    let req: Request = serde_json::from_str(line.trim_end()).unwrap();
                    let result = match req.method.as_deref() {
                        Some("hello") => serde_json::json!({
                            "apiMajor": 1, "apiMinor": 1, "engineVersion": "0",
                            "machineId": "m", "principal": "local-unverified",
                            "capabilities": [], "cursor": null
                        }),
                        Some("session.list") => serde_json::json!({ "sessions": rows }),
                        _ => serde_json::Value::Null,
                    };
                    let mut out = serde_json::to_vec(&Response::ok(req.id, result)).unwrap();
                    out.push(b'\n');
                    writer.write_all(&out).unwrap();
                }
            }
        });
        (sock, handle)
    }

    pub(crate) fn row(key: &str, status: &str) -> AgentRow {
        AgentRow {
            id: format!("agt_{}", key),
            generation: 1,
            mode: None,
            role: "observed".into(),
            control: "none".into(),
            provider: "claude".into(),
            provider_sid: key.split(':').nth(1).map(str::to_string),
            source: "detected".into(),
            created_by: None,
            gone_at: None,
            resume_count: 0,
            interrupted_note_pending: false,
            session_key: Some(key.into()),
            machine_id: Some("m".into()),
            state: None,
            status: Some(status.into()),
            cwd: None,
            name: None,
            pid: None,
            started_at: None,
            created_at: None,
            last_seen: None,
        }
    }

    #[test]
    fn session_list_reads_a_running_engine() {
        let dir = tempfile::tempdir().unwrap();
        let rows = vec![row("claudeCode:a", "working")];
        let (sock, server) = fake_engine(dir.path(), rows.clone(), 1);
        let got = session_list(&sock, "test", Duration::from_secs(2)).unwrap();
        assert_eq!(got, rows);
        server.join().unwrap();
    }

    #[test]
    fn missing_socket_is_unreachable_and_nothing_is_started() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("engine-v1.sock");
        let err = session_list(&sock, "test", Duration::from_millis(200)).unwrap_err();
        assert!(matches!(err, ClientError::Unreachable(_)), "{:?}", err);
        // Nothing was created: the client never auto-starts an engine.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn silent_engine_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("engine-v1.sock");
        // Accepts (the kernel backlog does) but never answers.
        let _listener = UnixListener::bind(&sock).unwrap();
        let err = session_list(&sock, "test", Duration::from_millis(100)).unwrap_err();
        assert!(matches!(err, ClientError::Unreachable(_)), "{:?}", err);
    }
}
