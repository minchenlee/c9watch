//! c9p/1 connection handling: one reader and one writer task per
//! connection, the `hello` handshake, and method dispatch.
//!
//! The first frame must be `hello` and must arrive within 5 s of the accept;
//! blank lines do not extend that deadline. A major version mismatch gets
//! `upgrade_required` and the connection closes. So do a missing or late
//! `hello`, an oversized frame and a peer under another uid. After `hello`, a
//! frame that is not valid JSON or lacks `v` gets `bad_request`, and the
//! connection stays open, because the line framing is still intact.

use super::peer::{self, PeerCred};
use super::principal::{self, LiveAgents, NoLiveAgents, Principal};
use super::proto::{
    encode, read_frame, ApiError, ErrorCode, FrameError, HelloParams, HelloResult, Request,
    Response, API_MAJOR, API_MINOR, MAX_FRAME,
};
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

/// How long a new connection has to send `hello`.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// Responses queued for one connection's writer task.
const WRITE_QUEUE: usize = 64;

/// Engine-wide facts every connection needs.
pub struct ServerCtx {
    pub engine_version: String,
    /// The machine key id (`engine::machine`).
    pub machine_id: String,
    pub hello_timeout: Duration,
    /// Live agents, for `agent:<id>` stamping.
    pub agents: Arc<dyn LiveAgents>,
    /// The only uid a peer may have: the engine's own euid.
    pub engine_uid: libc::uid_t,
}

impl ServerCtx {
    pub fn new(machine_id: String) -> Self {
        Self {
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            machine_id,
            hello_timeout: HELLO_TIMEOUT,
            // C1a-33 replaces this with the engine's agent table.
            agents: Arc::new(NoLiveAgents),
            engine_uid: peer::engine_uid(),
        }
    }
}

/// Accept connections until the listener fails. Each connection runs in its
/// own task. The caller stops the engine by dropping this future (for
/// example in a `select!` with `ShutdownSignals::recv`).
pub async fn serve(listener: UnixListener, ctx: Arc<ServerCtx>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(serve_conn(stream, ctx.clone()));
            }
            Err(e) => {
                eprintln!("[engine] Accept error: {}", e);
                // EMFILE and friends: back off instead of spinning.
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// The connection's state after `hello`.
struct Conn {
    principal: Principal,
}

/// What the reader does after handling a frame.
enum Next {
    Continue,
    Close,
}

/// Serve one connection until the peer closes it or a fatal error.
pub async fn serve_conn(stream: UnixStream, ctx: Arc<ServerCtx>) {
    let hello_deadline = tokio::time::Instant::now() + ctx.hello_timeout;
    // Read the clock before the peer facts: a peer that started after this
    // reused the pid of the process that connected.
    let accepted = peer::start_token_now();
    let cred = peer::peer_cred(stream.as_raw_fd());
    let (read, mut write) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<Response>(WRITE_QUEUE);
    let writer = tokio::spawn(async move {
        while let Some(resp) = rx.recv().await {
            if write.write_all(&encode(&resp)).await.is_err() {
                break;
            }
        }
        let _ = write.shutdown().await;
    });

    let cred = match cred.and_then(|c| peer::check_peer_uid(&c, ctx.engine_uid).map(|_| c)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[engine] Rejected connection: {}", e);
            let _ = tx
                .send(Response::err(
                    None,
                    ApiError::new(ErrorCode::PeerUidMismatch, e),
                ))
                .await;
            drop(tx);
            let _ = writer.await;
            return;
        }
    };

    // Stamp at accept, not at `hello`: the peer pid is then as fresh as it
    // gets, and a late `hello` cannot wait for the pid to be reused.
    let principal = principal::stamp_peer(cred.pid, accepted, ctx.agents.as_ref());

    let mut reader = BufReader::new(read);
    let mut conn: Option<Conn> = None;
    loop {
        let frame = if conn.is_none() {
            match tokio::time::timeout_at(hello_deadline, read_frame(&mut reader, MAX_FRAME)).await
            {
                Ok(f) => f,
                Err(_) => {
                    let _ = tx
                        .send(Response::err(
                            None,
                            ApiError::new(ErrorCode::HelloTimeout, "no hello within the timeout"),
                        ))
                        .await;
                    break;
                }
            }
        } else {
            read_frame(&mut reader, MAX_FRAME).await
        };
        let frame = match frame {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(FrameError::TooLarge) => {
                let _ = tx
                    .send(Response::err(
                        None,
                        ApiError::new(
                            ErrorCode::FrameTooLarge,
                            format!("frame exceeds {} bytes", MAX_FRAME),
                        ),
                    ))
                    .await;
                break;
            }
            Err(FrameError::Io(_)) => break,
        };
        if frame.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let (resp, next) = handle_frame(&frame, &ctx, cred, &principal, &mut conn);
        if tx.send(resp).await.is_err() {
            break;
        }
        if let Next::Close = next {
            break;
        }
    }
    drop(tx);
    let _ = writer.await;
}

/// Handle one frame and say whether the connection stays open.
fn handle_frame(
    frame: &[u8],
    ctx: &ServerCtx,
    cred: PeerCred,
    principal: &Principal,
    conn: &mut Option<Conn>,
) -> (Response, Next) {
    // A bad frame before `hello` closes; after it the connection stays.
    let soft = if conn.is_some() {
        Next::Continue
    } else {
        Next::Close
    };
    let req: Request = match serde_json::from_slice(frame) {
        Ok(r) => r,
        Err(e) => {
            return (
                Response::err(
                    None,
                    ApiError::new(ErrorCode::BadRequest, format!("invalid JSON: {}", e)),
                ),
                soft,
            );
        }
    };
    let id = req.id.clone();
    let Some(v) = req.v else {
        return (
            Response::err(id, ApiError::new(ErrorCode::BadRequest, "missing v")),
            soft,
        );
    };
    if v != API_MAJOR {
        return (
            Response::err(
                id,
                ApiError::new(
                    ErrorCode::UpgradeRequired,
                    format!("envelope v={}, this engine speaks c9p/{}", v, API_MAJOR),
                ),
            ),
            Next::Close,
        );
    }
    let Some(method) = req.method.as_deref() else {
        return (
            Response::err(id, ApiError::new(ErrorCode::BadRequest, "missing method")),
            soft,
        );
    };
    if method == "hello" {
        return hello(req, ctx, cred, principal, conn);
    }
    if conn.is_none() {
        return (
            Response::err(
                id,
                ApiError::new(
                    ErrorCode::HelloRequired,
                    format!("got {:?} before hello", method),
                ),
            ),
            Next::Close,
        );
    }
    let resp = match dispatch(method, &req) {
        Ok(result) => Response::ok(id, result),
        Err(e) => Response::err(id, e),
    };
    (resp, Next::Continue)
}

/// Methods after `hello`. The method table grows from M2 on.
fn dispatch(method: &str, _req: &Request) -> Result<serde_json::Value, ApiError> {
    Err(ApiError::new(
        ErrorCode::UnknownMethod,
        format!("unknown method {:?}", method),
    ))
}

/// Capabilities this engine offers. Empty until the first optional feature.
const CAPABILITIES: &[&str] = &[];

fn hello(
    req: Request,
    ctx: &ServerCtx,
    cred: PeerCred,
    principal: &Principal,
    conn: &mut Option<Conn>,
) -> (Response, Next) {
    let id = req.id.clone();
    let params: HelloParams = if req.params.is_null() {
        HelloParams::default()
    } else {
        match serde_json::from_value(req.params) {
            Ok(p) => p,
            Err(e) => {
                let next = if conn.is_some() {
                    Next::Continue
                } else {
                    Next::Close
                };
                return (
                    Response::err(
                        id,
                        ApiError::new(ErrorCode::BadRequest, format!("bad hello params: {}", e)),
                    ),
                    next,
                );
            }
        }
    };
    if let Some(major) = params.api_major.filter(|m| *m != API_MAJOR) {
        return (
            Response::err(
                id,
                ApiError::new(
                    ErrorCode::UpgradeRequired,
                    format!(
                        "client speaks c9p/{}, this engine speaks c9p/{}",
                        major, API_MAJOR
                    ),
                ),
            ),
            Next::Close,
        );
    }
    let principal = match conn {
        Some(c) => c.principal.clone(),
        None => {
            *conn = Some(Conn {
                principal: principal.clone(),
            });
            principal.clone()
        }
    };
    // The client's claim about itself is logged as unverified and nothing
    // else: it never decides the principal (N1).
    if let Some(client) = &params.client {
        eprintln!(
            "[engine] hello pid={} principal={} unverified client claim kind={:?} name={:?} version={:?}",
            cred.pid, principal, client.kind, client.name, client.version
        );
    }
    let capabilities = params
        .want
        .iter()
        .filter(|w| CAPABILITIES.contains(&w.as_str()))
        .cloned()
        .collect();
    let result = HelloResult {
        api_major: API_MAJOR,
        api_minor: API_MINOR,
        engine_version: ctx.engine_version.clone(),
        machine_id: ctx.machine_id.clone(),
        principal: principal.to_string(),
        capabilities,
        cursor: None,
    };
    let result = serde_json::to_value(result).expect("HelloResult serializes");
    (Response::ok(id, result), Next::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};

    fn ctx(hello_timeout: Duration) -> Arc<ServerCtx> {
        ctx_with(hello_timeout, Arc::new(NoLiveAgents))
    }

    fn ctx_with(hello_timeout: Duration, agents: Arc<dyn LiveAgents>) -> Arc<ServerCtx> {
        Arc::new(ServerCtx {
            engine_version: "9.9.9".into(),
            machine_id: "m-test".into(),
            hello_timeout,
            agents,
            engine_uid: peer::engine_uid(),
        })
    }

    /// A connected client stream, served by `serve_conn` on the other end.
    fn connect(ctx: Arc<ServerCtx>) -> UnixStream {
        let (client, server) = UnixStream::pair().unwrap();
        tokio::spawn(serve_conn(server, ctx));
        client
    }

    async fn send(c: &mut UnixStream, v: Value) {
        let mut line = serde_json::to_vec(&v).unwrap();
        line.push(b'\n');
        c.write_all(&line).await.unwrap();
    }

    /// Read every response until the engine closes the connection.
    async fn read_all(c: UnixStream) -> Vec<Value> {
        let mut out = Vec::new();
        let mut lines = BufReader::new(c).lines();
        while let Some(l) = lines.next_line().await.unwrap() {
            out.push(serde_json::from_str(&l).unwrap());
        }
        out
    }

    async fn read_one(c: &mut UnixStream) -> Value {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            c.read_exact(&mut byte).await.unwrap();
            if byte[0] == b'\n' {
                return serde_json::from_slice(&buf).unwrap();
            }
            buf.push(byte[0]);
        }
    }

    fn hello_req(id: &str, params: Value) -> Value {
        json!({"v": 1, "id": id, "method": "hello", "params": params})
    }

    #[tokio::test]
    async fn hello_returns_the_engine_facts() {
        let mut c = connect(ctx(HELLO_TIMEOUT));
        send(
            &mut c,
            hello_req("1", json!({"apiMajor": 1, "want": ["nope"]})),
        )
        .await;
        let r = read_one(&mut c).await;
        assert_eq!(r["id"], "1");
        assert_eq!(r["ok"], true, "{}", r);
        let res = &r["result"];
        assert_eq!(res["apiMajor"], 1);
        assert_eq!(res["apiMinor"], API_MINOR);
        assert_eq!(res["engineVersion"], "9.9.9");
        assert_eq!(res["machineId"], "m-test");
        assert_eq!(res["principal"], "local-unverified");
        assert_eq!(res["capabilities"], json!([]));
        assert!(res["cursor"].is_null());

        // After hello the connection stays open for more frames.
        send(&mut c, json!({"v": 1, "id": "2", "method": "no.such"})).await;
        let r = read_one(&mut c).await;
        assert_eq!(r["id"], "2");
        assert_eq!(r["error"]["code"], "unknown_method");
    }

    #[tokio::test]
    async fn major_mismatch_is_upgrade_required() {
        let mut c = connect(ctx(HELLO_TIMEOUT));
        send(&mut c, hello_req("1", json!({"apiMajor": 2}))).await;
        let got = read_all(c).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["error"]["code"], "upgrade_required");

        // A v2 envelope is a mismatch too.
        let mut c = connect(ctx(HELLO_TIMEOUT));
        send(
            &mut c,
            json!({"v": 2, "id": "1", "method": "hello", "params": {}}),
        )
        .await;
        let got = read_all(c).await;
        assert_eq!(got[0]["error"]["code"], "upgrade_required");
        assert_eq!(got[0]["id"], "1");
    }

    #[tokio::test]
    async fn no_hello_in_time_closes_the_connection() {
        let c = connect(ctx(Duration::from_millis(100)));
        let got = tokio::time::timeout(Duration::from_secs(5), read_all(c))
            .await
            .expect("engine closes the connection");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["error"]["code"], "hello_timeout");
        assert_eq!(got[0]["error"]["retryable"], true);
    }

    #[tokio::test]
    async fn blank_lines_do_not_extend_the_hello_deadline() {
        let c = connect(ctx(Duration::from_millis(200)));
        let (read, mut write) = c.into_split();
        let ticker = tokio::spawn(async move {
            // Ends once the engine closes and the write fails.
            while write.write_all(b"\n").await.is_ok() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        let mut lines = BufReader::new(read).lines();
        let first = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("engine answers within the hello deadline")
            .unwrap()
            .expect("one error frame");
        let v: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(v["error"]["code"], "hello_timeout");
        ticker.abort();
    }

    #[tokio::test]
    async fn a_peer_under_another_uid_is_rejected() {
        let mut ctx = ServerCtx::new("m".into());
        ctx.engine_uid = peer::engine_uid().wrapping_add(1);
        // The engine answers before any frame, then closes.
        let c = connect(Arc::new(ctx));
        let got = read_all(c).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["error"]["code"], "peer_uid_mismatch");
    }

    #[tokio::test]
    async fn missing_v_after_hello_is_a_bad_request() {
        let mut c = connect(ctx(HELLO_TIMEOUT));
        send(&mut c, hello_req("1", json!({}))).await;
        assert_eq!(read_one(&mut c).await["ok"], true);
        send(&mut c, json!({"id": "2", "method": "x"})).await;
        let r = read_one(&mut c).await;
        assert_eq!(r["id"], "2");
        assert_eq!(r["error"]["code"], "bad_request");
        // Still open; an explicit v2 then closes it.
        send(&mut c, json!({"v": 2, "id": "3", "method": "x"})).await;
        let got = read_all(c).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["error"]["code"], "upgrade_required");
    }

    #[tokio::test]
    async fn first_frame_must_be_hello() {
        let mut c = connect(ctx(HELLO_TIMEOUT));
        send(&mut c, json!({"v": 1, "id": "1", "method": "session.list"})).await;
        let got = read_all(c).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["error"]["code"], "hello_required");
    }

    #[tokio::test]
    async fn oversized_frame_closes_the_connection() {
        let mut c = connect(ctx(HELLO_TIMEOUT));
        send(&mut c, hello_req("1", json!({}))).await;
        assert_eq!(read_one(&mut c).await["ok"], true);
        let big = format!(
            "{{\"v\":1,\"id\":\"2\",\"method\":\"x\",\"params\":\"{}\"}}\n",
            "a".repeat(MAX_FRAME)
        );
        // The engine may close while we still write; ignore that error.
        let _ = c.write_all(big.as_bytes()).await;
        let got = read_all(c).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["error"]["code"], "frame_too_large");
    }

    #[tokio::test]
    async fn unknown_fields_and_bad_json_after_hello() {
        let mut c = connect(ctx(HELLO_TIMEOUT));
        send(
            &mut c,
            json!({"v": 1, "id": "1", "method": "hello", "later": [1, 2],
                   "params": {"client": {"kind": "cli", "name": "c9watch", "new": 1}, "x": {}}}),
        )
        .await;
        assert_eq!(read_one(&mut c).await["ok"], true);
        c.write_all(b"{not json\n").await.unwrap();
        assert_eq!(read_one(&mut c).await["error"]["code"], "bad_request");
        // Still open: a second hello answers again with the same principal.
        send(&mut c, hello_req("3", json!({}))).await;
        let r = read_one(&mut c).await;
        assert_eq!(r["id"], "3");
        assert_eq!(r["result"]["principal"], "local-unverified");
    }

    #[tokio::test]
    async fn serve_accepts_on_a_real_socket() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("e.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(serve(listener, ctx(HELLO_TIMEOUT)));
        let mut c = UnixStream::connect(&path).await.unwrap();
        send(&mut c, hello_req("1", json!({"apiMajor": 1}))).await;
        assert_eq!(read_one(&mut c).await["result"]["apiMajor"], 1);
        server.abort();
    }

    /// Hello with `params` from the test process, and the stamped principal.
    async fn principal_for(agents: Arc<dyn LiveAgents>, params: Value) -> Value {
        let mut c = connect(ctx_with(HELLO_TIMEOUT, agents));
        send(&mut c, hello_req("1", params)).await;
        let r = read_one(&mut c).await;
        assert_eq!(r["ok"], true, "{}", r);
        r
    }

    #[tokio::test]
    async fn cli_and_gui_connections_are_local_unverified() {
        for kind in ["cli", "gui"] {
            let r = principal_for(
                Arc::new(NoLiveAgents),
                json!({"client": {"kind": kind, "name": "c9watch"}}),
            )
            .await;
            assert_eq!(r["result"]["principal"], "local-unverified", "{}", kind);
        }
    }

    #[tokio::test]
    async fn gateway_claim_is_local_unverified() {
        let r = principal_for(
            Arc::new(NoLiveAgents),
            json!({"client": {"kind": "gateway", "name": "x"}, "clientId": "x"}),
        )
        .await;
        assert_eq!(r["result"]["principal"], "local-unverified");
        assert!(!r.to_string().contains("client:"), "{}", r);
    }

    #[tokio::test]
    async fn descendant_of_a_live_pty_child_is_the_agent() {
        // The peer is this test process; pretend its parent is the PTY child.
        let me = std::process::id() as libc::pid_t;
        let parent = peer::proc_info(me).unwrap().ppid;
        let start = peer::proc_info(parent).unwrap().start;
        let agents = principal::tests::FakeAgents::one("a7", parent, start);
        let r = principal_for(Arc::new(agents), json!({"client": {"kind": "cli"}})).await;
        assert_eq!(r["result"]["principal"], "agent:a7");

        // Same pid, other start time: a reused pid is not the agent.
        let agents = principal::tests::FakeAgents::one("a7", parent, start + 1);
        let r = principal_for(Arc::new(agents), json!({})).await;
        assert_eq!(r["result"]["principal"], "local-unverified");
    }
}
