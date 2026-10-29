//! c9p/1 connection handling: one reader and one writer task per
//! connection, the `hello` handshake, and method dispatch.
//!
//! The first frame must be `hello` and must arrive within 5 s of the accept;
//! blank lines do not extend that deadline. A major version mismatch gets
//! `upgrade_required` and the connection closes. So do a missing or late
//! `hello`, an oversized frame and a peer under another uid. After `hello`, a
//! frame that is not valid JSON or lacks `v` gets `bad_request`, and the
//! connection stays open, because the line framing is still intact.
//!
//! `events.subscribe` starts a stream task on the connection. It reads the
//! event log from SQLite at its own pace and writes event frames to the
//! same write queue as responses, so a slow client only slows its own
//! stream, never the db writer. A connection holds at most `MAX_STREAMS`
//! live streams; one more gets `rate_limited`.
//!
//! Hook-class methods (`hook.*`, `task.done`, `agent.handoff`) need two
//! facts (C1a-21): the principal is `agent:<id>` for the agent the params
//! name, and the peer's exe realpath is the engine's `c9watch-agent` copy
//! with the sha256 the engine recorded. Anything else is
//! `peer_not_authorized`, and the refusal goes to the audit log. The main
//! binary's `c9watch api` can therefore never send one. An authorized
//! `hook.*` event then goes through `engine::hooks::intake` (kind,
//! generation, dedupe, state merge; C1a-22). `hook.session_start` also
//! answers with the standing orders (`engine::standing`, C1a-25).
//!
//! `agent.create` and `agent.retire` take only a local client
//! (`local-unverified`); an agent gets `not_allowed`, which is audited. The
//! work is in `engine::create` (C1a-26).

use super::agent_bin::AgentBin;
use super::create::Spawner;
use super::db::audit::{self, AuditEntry};
use super::db::outbox::{Cursor, Event, ReadError, Subscription, READ_BATCH};
use super::db::{now_ms, AgentFilter, Db};
use super::hooks::{self, HookEvent, Outcome};
use super::merge::MergeTable;
use super::peer::{self, PeerCred};
use super::principal::{self, LiveAgents, NoLiveAgents, Principal};
use super::proto::{
    encode, encode_event, is_hook_class, read_frame, AgentCreateParams, AgentCreateResult,
    AgentListParams, AgentListResult, AgentRetireParams, AgentRetireResult, ApiError, ErrorCode,
    EventFrame, EventType, FrameError, HandoffParams, HandoffResult, HelloParams, HelloResult,
    HookParams, HookResult, Request, Response, SessionListParams, SessionListResult,
    SnapshotParams, SnapshotResult, SubscribeParams, SubscribeResult, TaskDoneParams,
    TaskDoneResult, API_MAJOR, API_MINOR, HOOK_METHODS, MAX_FRAME,
};
use super::standing;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// How long a new connection has to send `hello`.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// Encoded frames (responses and events) queued for one connection's
/// writer task.
const WRITE_QUEUE: usize = 64;

/// Live `events.subscribe` streams one connection may hold. Each holds a
/// SQLite reader (and its file descriptors) until the connection closes, so
/// without a cap one client could exhaust the engine's descriptors.
pub const MAX_STREAMS: usize = 16;

/// Longest `seq` a hook-class call may carry. A ns clock has 19 digits.
const MAX_SEQ: usize = 32;

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
    /// The engine's `c9watch-agent` copy. Without one, every hook-class
    /// call is `peer_not_authorized`.
    pub agent_bin: Option<Arc<AgentBin>>,
    /// The store. `None` only in tests of the connection layer.
    pub db: Option<Arc<Db>>,
    /// The state merge of every live agent (C1a-22).
    pub merge: Arc<MergeTable>,
    /// `<home>/standing-orders.md`, sent on `hook.session_start`. `None`
    /// sends no orders.
    pub standing_orders: Option<PathBuf>,
    /// Free-agent create and retire (C1a-26). `None` answers
    /// `experimental_disabled` to `agent.create`.
    pub spawner: Option<Arc<Spawner>>,
}

impl ServerCtx {
    pub fn new(machine_id: String) -> Self {
        Self {
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            machine_id,
            hello_timeout: HELLO_TIMEOUT,
            // `run` sets the engine's live table (`with_agents`).
            agents: Arc::new(NoLiveAgents),
            engine_uid: peer::engine_uid(),
            agent_bin: None,
            db: None,
            merge: Arc::new(MergeTable::new()),
            standing_orders: None,
            spawner: None,
        }
    }

    /// Stamp `agent:<id>` from the engine's live-agent table (C1a-33).
    pub fn with_agents(mut self, agents: Arc<dyn LiveAgents>) -> Self {
        self.agents = agents;
        self
    }

    pub fn with_agent_bin(mut self, bin: Arc<AgentBin>) -> Self {
        self.agent_bin = Some(bin);
        self
    }

    pub fn with_db(mut self, db: Arc<Db>) -> Self {
        self.db = Some(db);
        self
    }

    pub fn with_standing_orders(mut self, path: PathBuf) -> Self {
        self.standing_orders = Some(path);
        self
    }

    pub fn with_spawner(mut self, spawner: Arc<Spawner>) -> Self {
        self.spawner = Some(spawner);
        self
    }

    fn db(&self) -> Result<&Arc<Db>, ApiError> {
        self.db
            .as_ref()
            .ok_or_else(|| ApiError::new(ErrorCode::Internal, "the engine has no store"))
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

/// The process on the other end, as read at accept.
#[derive(Debug, Clone, Copy)]
struct Peer {
    cred: PeerCred,
    /// Start time of `cred.pid`; `None` when it could not be read.
    start: Option<u64>,
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
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(WRITE_QUEUE);
    let writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if write.write_all(&line).await.is_err() {
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
                .send(encode(&Response::err(
                    None,
                    ApiError::new(ErrorCode::PeerUidMismatch, e),
                )))
                .await;
            drop(tx);
            let _ = writer.await;
            return;
        }
    };

    // Stamp at accept, not at `hello`: the peer pid is then as fresh as it
    // gets, and a late `hello` cannot wait for the pid to be reused.
    // The peer's start time comes from the same read as the principal, so
    // a later exe check reads the process the principal came from.
    let (principal, start) = principal::stamp_peer(cred.pid, accepted, ctx.agents.as_ref());
    let peer = Peer { cred, start };

    let mut reader = BufReader::new(read);
    let mut conn: Option<Conn> = None;
    let mut streams: Vec<JoinHandle<()>> = Vec::new();
    loop {
        let frame = if conn.is_none() {
            match tokio::time::timeout_at(hello_deadline, read_frame(&mut reader, MAX_FRAME)).await
            {
                Ok(f) => f,
                Err(_) => {
                    let _ = tx
                        .send(encode(&Response::err(
                            None,
                            ApiError::new(ErrorCode::HelloTimeout, "no hello within the timeout"),
                        )))
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
                    .send(encode(&Response::err(
                        None,
                        ApiError::new(
                            ErrorCode::FrameTooLarge,
                            format!("frame exceeds {} bytes", MAX_FRAME),
                        ),
                    )))
                    .await;
                break;
            }
            Err(FrameError::Io(_)) => break,
        };
        if frame.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        // A stream that ended (expired, or the store closed) frees its slot.
        streams.retain(|h| !h.is_finished());
        let (resp, next, stream) =
            handle_frame(&frame, &ctx, peer, &principal, &mut conn, streams.len()).await;
        if tx.send(encode(&resp)).await.is_err() {
            break;
        }
        // Start the stream only after its subscribe response is queued, so
        // that response comes first.
        if let Some(s) = stream {
            streams.push(tokio::spawn(s.run(tx.clone())));
        }
        if let Next::Close = next {
            break;
        }
    }
    for s in &streams {
        s.abort();
    }
    drop(tx);
    let _ = writer.await;
}

/// Handle one frame and say whether the connection stays open, and which
/// event stream to start after the response.
async fn handle_frame(
    frame: &[u8],
    ctx: &ServerCtx,
    peer: Peer,
    principal: &Principal,
    conn: &mut Option<Conn>,
    streams: usize,
) -> (Response, Next, Option<EventStream>) {
    match check_frame(frame, ctx, peer.cred, principal, conn) {
        Checked::Done(resp, next) => (resp, next, None),
        Checked::Call(req, method) if is_hook_class(&method) => {
            let id = req.id.clone();
            match hook_class(&method, req, ctx, peer, principal).await {
                Ok(result) => (Response::ok(id, result), Next::Continue, None),
                Err(e) => (Response::err(id, e), Next::Continue, None),
            }
        }
        Checked::Call(req, method) => {
            let id = req.id.clone();
            match dispatch(&method, req, ctx, peer, principal, streams).await {
                Ok((result, stream)) => (Response::ok(id, result), Next::Continue, stream),
                Err(e) => (Response::err(id, e), Next::Continue, None),
            }
        }
    }
}

/// A frame after the envelope checks.
enum Checked {
    /// Answered already (an error or `hello`).
    Done(Response, Next),
    /// A method call after `hello`.
    Call(Request, String),
}

/// Envelope checks and `hello`. Never touches the store.
fn check_frame(
    frame: &[u8],
    ctx: &ServerCtx,
    cred: PeerCred,
    principal: &Principal,
    conn: &mut Option<Conn>,
) -> Checked {
    // A bad frame before `hello` closes; after it the connection stays.
    let soft = if conn.is_some() {
        Next::Continue
    } else {
        Next::Close
    };
    let req: Request = match serde_json::from_slice(frame) {
        Ok(r) => r,
        Err(e) => {
            return Checked::Done(
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
        return Checked::Done(
            Response::err(id, ApiError::new(ErrorCode::BadRequest, "missing v")),
            soft,
        );
    };
    if v != API_MAJOR {
        return Checked::Done(
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
    let Some(method) = req.method.clone() else {
        return Checked::Done(
            Response::err(id, ApiError::new(ErrorCode::BadRequest, "missing method")),
            soft,
        );
    };
    if method == "hello" {
        let (resp, next) = hello(req, ctx, cred, principal, conn);
        return Checked::Done(resp, next);
    }
    if conn.is_none() {
        return Checked::Done(
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
    Checked::Call(req, method)
}

/// Every c9p/1 method, as in `docs/api/c9p-1.json`. `hello` is handled
/// before dispatch; `dispatch` serves the rest.
pub const METHODS: &[&str] = &[
    "hello",
    "snapshot.get",
    "events.subscribe",
    "session.list",
    "agent.list",
    "agent.create",
    "agent.retire",
    "hook.session_start",
    "hook.stop",
    "hook.permission_request",
    "hook.permission_denied",
    "hook.notification",
    "task.done",
    "agent.handoff",
];

/// The c9p/1 JSON Schema (A:245): the frame types, a `methods` table with
/// each method's params and result, and every wire type under `$defs`.
/// `docs/api/c9p-1.json` is this value, and `src/lib/c9p-1.ts` is generated
/// from that file by `scripts/gen-c9p-types.mjs`.
pub fn schema() -> serde_json::Value {
    use schemars::generate::SchemaSettings;
    // Params and the request frame are what the engine reads; results and
    // the other frames are what it writes. Each side gets its own contract,
    // so an `Option` param may be left out while an `Option` result field
    // is always sent.
    let mut de = SchemaSettings::draft2020_12()
        .for_deserialize()
        .into_generator();
    let mut ser = SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator();
    let frames = serde_json::json!({
        "request": de.subschema_for::<Request>(),
        "response": ser.subschema_for::<Response>(),
        "event": ser.subschema_for::<EventFrame>(),
    });
    let methods = serde_json::json!({
        "hello": {
            "params": de.subschema_for::<HelloParams>(),
            "result": ser.subschema_for::<HelloResult>(),
        },
        "snapshot.get": {
            "params": de.subschema_for::<SnapshotParams>(),
            "result": ser.subschema_for::<SnapshotResult>(),
        },
        "events.subscribe": {
            "params": de.subschema_for::<SubscribeParams>(),
            "result": ser.subschema_for::<SubscribeResult>(),
            "events": ser.subschema_for::<EventFrame>(),
        },
        "session.list": {
            "params": de.subschema_for::<SessionListParams>(),
            "result": ser.subschema_for::<SessionListResult>(),
        },
        "agent.list": {
            "params": de.subschema_for::<AgentListParams>(),
            "result": ser.subschema_for::<AgentListResult>(),
        },
        "agent.create": {
            "params": de.subschema_for::<AgentCreateParams>(),
            "result": ser.subschema_for::<AgentCreateResult>(),
        },
        "agent.retire": {
            "params": de.subschema_for::<AgentRetireParams>(),
            "result": ser.subschema_for::<AgentRetireResult>(),
        },
        "task.done": {
            "params": de.subschema_for::<TaskDoneParams>(),
            "result": ser.subschema_for::<TaskDoneResult>(),
        },
        "agent.handoff": {
            "params": de.subschema_for::<HandoffParams>(),
            "result": ser.subschema_for::<HandoffResult>(),
        },
    });
    let mut methods = methods;
    for m in HOOK_METHODS {
        methods[*m] = serde_json::json!({
            "params": de.subschema_for::<HookParams>(),
            "result": ser.subschema_for::<HookResult>(),
        });
    }
    let mut defs = de.take_definitions(true);
    for (name, def) in ser.take_definitions(true) {
        match defs.get(&name) {
            // A type on both sides (ErrorCode) must read the same way.
            Some(other) => assert_eq!(other, &def, "{} differs between contracts", name),
            None => {
                defs.insert(name, def);
            }
        }
    }
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "c9p/1",
        "description": "The c9watch engine protocol: one JSON object per line over \
            the engine socket. Within a major version every change is additive.",
        "apiMajor": API_MAJOR,
        "apiMinor": API_MINOR,
        "frames": frames,
        "methods": methods,
        "$defs": defs,
    })
}

/// Methods after `hello`, except the hook-class ones. Both C1a principals,
/// L and A, may call every method here except `agent.create` and
/// `agent.retire`, which take L only (§1.1). `streams` is how many event
/// streams the connection holds.
async fn dispatch(
    method: &str,
    req: Request,
    ctx: &ServerCtx,
    peer: Peer,
    principal: &Principal,
    streams: usize,
) -> Result<(serde_json::Value, Option<EventStream>), ApiError> {
    match method {
        "agent.create" => Ok((
            agent_lifecycle(method, req, ctx, peer, principal).await?,
            None,
        )),
        "agent.retire" => Ok((
            agent_lifecycle(method, req, ctx, peer, principal).await?,
            None,
        )),
        "snapshot.get" => Ok((snapshot_get(ctx).await?, None)),
        "session.list" => Ok((session_list(ctx).await?, None)),
        "agent.list" => Ok((agent_list(req, ctx).await?, None)),
        "events.subscribe" => {
            if streams >= MAX_STREAMS {
                return Err(ApiError::new(
                    ErrorCode::RateLimited,
                    format!("this connection already holds {} event streams", streams),
                ));
            }
            let (result, stream) = events_subscribe(req, ctx).await?;
            Ok((result, Some(stream)))
        }
        _ => Err(ApiError::new(
            ErrorCode::UnknownMethod,
            format!("unknown method {:?}", method),
        )),
    }
}

/// `agent.create` and `agent.retire` (C1a-26, `engine::create`). An agent
/// principal's call is refused and audited. So is a `local-unverified`
/// peer that fails `principal::check_mutating_peer`: an ancestry that does
/// not reach init, a live agent's PTY child at any depth, or a sandbox.
async fn agent_lifecycle(
    method: &str,
    req: Request,
    ctx: &ServerCtx,
    peer: Peer,
    principal: &Principal,
) -> Result<serde_json::Value, ApiError> {
    let r = match check_local_peer(method, ctx, peer, principal).await {
        Ok(()) => spawn_or_retire(method, req, ctx, principal).await,
        Err(e) => Err(e),
    };
    if let Err(e) = &r {
        if e.code == ErrorCode::NotAllowed {
            audit_refusal(ctx, principal, method, "", e).await;
        }
    }
    r
}

/// Who may call `agent.create` or `agent.retire`: never an agent, also
/// with the experimental switch off, so its attempt is audited; and a
/// `local-unverified` peer only after the mutation check.
async fn check_local_peer(
    method: &str,
    ctx: &ServerCtx,
    peer: Peer,
    principal: &Principal,
) -> Result<(), ApiError> {
    if *principal != Principal::LocalUnverified {
        return Err(ApiError::new(
            ErrorCode::NotAllowed,
            format!("{} may not call {} in C1a", principal, method),
        ));
    }
    let agents = ctx.agents.clone();
    let checked = blocking(move || {
        Ok(principal::check_mutating_peer(
            peer.cred.pid,
            peer.start,
            agents.as_ref(),
        ))
    })
    .await?;
    checked.map_err(|why| {
        ApiError::new(
            ErrorCode::NotAllowed,
            format!("{} refused for this peer: {}", method, why),
        )
    })
}

async fn spawn_or_retire(
    method: &str,
    req: Request,
    ctx: &ServerCtx,
    principal: &Principal,
) -> Result<serde_json::Value, ApiError> {
    let Some(spawner) = &ctx.spawner else {
        return Err(match method {
            "agent.create" => ApiError::new(
                ErrorCode::ExperimentalDisabled,
                "this engine cannot create agents",
            ),
            _ => ApiError::new(ErrorCode::Internal, "this engine runs no agents"),
        });
    };
    if method == "agent.create" {
        let agent = spawner.create(principal, req.params).await?;
        Ok(
            serde_json::to_value(AgentCreateResult { agent })
                .expect("AgentCreateResult serializes"),
        )
    } else {
        let agent = spawner.retire(principal, req.params).await?;
        Ok(
            serde_json::to_value(AgentRetireResult { agent })
                .expect("AgentRetireResult serializes"),
        )
    }
}

/// Hook-class methods (§1.1): `hook.*`, `task.done` and `agent.handoff`.
async fn hook_class(
    method: &str,
    req: Request,
    ctx: &ServerCtx,
    peer: Peer,
    principal: &Principal,
) -> Result<serde_json::Value, ApiError> {
    if method.starts_with("hook.") && !HOOK_METHODS.contains(&method) {
        return Err(ApiError::new(
            ErrorCode::UnknownMethod,
            format!("unknown method {:?}", method),
        ));
    }
    let claimed = req
        .params
        .get("agentId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let checked = {
        let bin = ctx.agent_bin.clone();
        let principal = principal.clone();
        let claimed = claimed.clone();
        blocking(move || Ok(authorize(bin.as_deref(), peer, &principal, &claimed))).await?
    };
    if let Err(e) = checked {
        audit_refusal(ctx, principal, method, &claimed, &e).await;
        return Err(e);
    }
    let bad = |e: serde_json::Error| {
        ApiError::new(
            ErrorCode::BadRequest,
            format!("bad {} params: {}", method, e),
        )
    };
    match method {
        "task.done" => {
            serde_json::from_value::<TaskDoneParams>(req.params).map_err(bad)?;
            // Every C1a agent is free, and free agents have no task.
            Err(ApiError::new(
                ErrorCode::NoTask,
                format!("{} is a free agent and has no task", claimed),
            ))
        }
        "agent.handoff" => {
            serde_json::from_value::<HandoffParams>(req.params).map_err(bad)?;
            Err(ApiError::new(
                ErrorCode::UnsupportedInC1a,
                "agent.handoff comes with refresh in C1b",
            ))
        }
        _ => {
            let params = serde_json::from_value::<HookParams>(req.params).map_err(bad)?;
            // A sender's `seq` is a clock in ns; a long one only fills dedupe.
            if params.seq.len() > MAX_SEQ {
                return Err(ApiError::new(
                    ErrorCode::BadRequest,
                    format!("bad {} params: seq is longer than {}", method, MAX_SEQ),
                ));
            }
            let Principal::Agent { gen, .. } = principal else {
                // `authorize` lets only an agent through.
                return Err(ApiError::new(
                    ErrorCode::PeerNotAuthorized,
                    format!("{} is not an agent", principal),
                ));
            };
            let (outcome, additional_context) =
                hook_intake(ctx, method, principal, *gen, params).await?;
            let result = HookResult {
                accepted: outcome == Outcome::Accepted,
                additional_context,
            };
            Ok(serde_json::to_value(result).expect("HookResult serializes"))
        }
    }
}

/// Run an authorized `hook.*` event through the intake. Returns the
/// outcome, and for `hook.session_start` the context to send; a dropped
/// event is not an error (the hook must not fail).
async fn hook_intake(
    ctx: &ServerCtx,
    method: &str,
    principal: &Principal,
    gen: i64,
    params: HookParams,
) -> Result<(Outcome, Option<String>), ApiError> {
    let db = ctx.db()?.clone();
    let machine = ctx.machine_id.clone();
    let merge = ctx.merge.clone();
    // The file is read before the write: no I/O inside the transaction.
    let orders = match (&ctx.standing_orders, method) {
        (Some(path), "hook.session_start") => {
            let path = path.clone();
            blocking(move || Ok(standing::read(&path))).await?
        }
        _ => None,
    };
    // Read now, not at accept: a resume since then makes the event stale.
    let live_gen = ctx.agents.current_gen(&params.agent_id);
    let agent_id = params.agent_id.clone();
    let is_start = method == "hook.session_start";
    let (method, principal) = (method.to_string(), principal.to_string());
    let (answer, _) = db
        .write(move |tx| {
            let ev = HookEvent {
                method: &method,
                principal: &principal,
                gen,
                live_gen,
                params: &params,
            };
            if method == "hook.session_start" {
                standing::session_start(tx, &machine, &merge, &ev, orders.as_deref(), now_ms())
            } else {
                hooks::intake(tx, &machine, &merge, &ev, now_ms()).map(|o| (o, None))
            }
        })
        .await
        .map_err(internal)?;
    // A duplicate is the hook's retry: the start still happened.
    let started = matches!(
        answer.0,
        Outcome::Accepted | Outcome::Dropped(hooks::DropReason::Duplicate)
    );
    if let (true, true, Some(spawner)) = (is_start, started, &ctx.spawner) {
        spawner.session_started(&agent_id, gen);
    }
    Ok(answer)
}

/// The two facts a hook-class call needs: the principal is the agent the
/// params name, and the peer runs the engine's `c9watch-agent` copy. The
/// exe check reads the peer pid again, so it must still be the process that
/// was stamped at accept. Blocking.
fn authorize(
    bin: Option<&AgentBin>,
    peer: Peer,
    principal: &Principal,
    claimed: &str,
) -> Result<(), ApiError> {
    let denied = |m: String| ApiError::new(ErrorCode::PeerNotAuthorized, m);
    let Principal::Agent { id, .. } = principal else {
        return Err(denied(format!("{} is not an agent", principal)));
    };
    if claimed != id {
        return Err(denied(format!(
            "the call names agent {:?}, the caller is {}",
            claimed, principal
        )));
    }
    let Some(bin) = bin else {
        return Err(denied("the engine has no c9watch-agent copy".into()));
    };
    let exe = peer::peer_exe_path(peer.cred.pid).map_err(denied)?;
    let start = peer::proc_info(peer.cred.pid).map(|p| p.start);
    if start.is_none() || start != peer.start {
        return Err(denied(format!(
            "pid {} is no longer the process that connected",
            peer.cred.pid
        )));
    }
    bin.check_peer_exe(&exe)
}

/// Record a refused hook-class call. A failed write only logs.
async fn audit_refusal(
    ctx: &ServerCtx,
    principal: &Principal,
    method: &str,
    claimed: &str,
    e: &ApiError,
) {
    let Some(db) = ctx.db.clone() else {
        return;
    };
    let (principal, method, claimed) = (
        principal.to_string(),
        method.to_string(),
        claimed.to_string(),
    );
    let reason = format!("{}: {}", e.code, e.msg);
    let wrote = db
        .write(move |tx| {
            audit::append(
                tx,
                now_ms(),
                &AuditEntry {
                    principal: &principal,
                    method: &method,
                    target: (!claimed.is_empty()).then_some(claimed.as_str()),
                    decision: "deny",
                    reason: Some(&reason),
                    ..Default::default()
                },
            )
        })
        .await;
    if let Err(e) = wrote {
        eprintln!("[engine] Failed to audit a refused hook-class call: {}", e);
    }
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, e.to_string())
}

fn read_error(e: ReadError) -> ApiError {
    match e {
        ReadError::Expired(m) => ApiError::new(ErrorCode::CursorExpired, m),
        ReadError::Db(m) => internal(m),
    }
}

/// Run blocking store work off the async workers.
async fn blocking<R, F>(f: F) -> Result<R, ApiError>
where
    F: FnOnce() -> Result<R, ApiError> + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(internal)?
}

async fn snapshot_get(ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let db = ctx.db()?.clone();
    let (cursor, agents) = blocking(move || db.snapshot().map_err(internal)).await?;
    let result = SnapshotResult {
        cursor: cursor.to_string(),
        machine_id: ctx.machine_id.clone(),
        agents,
    };
    Ok(serde_json::to_value(result).expect("SnapshotResult serializes"))
}

/// The agents whose session detection sees now (C1a-15).
async fn session_list(ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let db = ctx.db()?.clone();
    let sessions = blocking(move || db.agents(AgentFilter::Sessions).map_err(internal)).await?;
    Ok(serde_json::to_value(SessionListResult { sessions }).expect("SessionListResult serializes"))
}

/// Live agents, or with `all` every row (N4).
async fn agent_list(req: Request, ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let params: AgentListParams = if req.params.is_null() {
        AgentListParams::default()
    } else {
        serde_json::from_value(req.params).map_err(|e| {
            ApiError::new(
                ErrorCode::BadRequest,
                format!("bad agent.list params: {}", e),
            )
        })?
    };
    let filter = if params.all {
        AgentFilter::All
    } else {
        AgentFilter::Live
    };
    let db = ctx.db()?.clone();
    let agents = blocking(move || db.agents(filter).map_err(internal)).await?;
    Ok(serde_json::to_value(AgentListResult { agents }).expect("AgentListResult serializes"))
}

async fn events_subscribe(
    req: Request,
    ctx: &ServerCtx,
) -> Result<(serde_json::Value, EventStream), ApiError> {
    let params: SubscribeParams = if req.params.is_null() {
        SubscribeParams::default()
    } else {
        serde_json::from_value(req.params).map_err(|e| {
            ApiError::new(
                ErrorCode::BadRequest,
                format!("bad events.subscribe params: {}", e),
            )
        })?
    };
    let db = ctx.db()?.clone();
    let since = match &params.since {
        Some(s) => Some(Cursor::parse(s).map_err(|e| ApiError::new(ErrorCode::BadRequest, e))?),
        None => None,
    };
    let (start, sub) = blocking(move || {
        let after = match since {
            Some(c) => db.check_cursor(&c).map_err(read_error)?,
            None => db.head_cursor().seq,
        };
        let sub = db.subscribe(after, READ_BATCH).map_err(internal)?;
        Ok((db.cursor_at(after), sub))
    })
    .await?;
    let result = SubscribeResult {
        cursor: start.to_string(),
    };
    let stream = EventStream {
        sub,
        id: req.id,
        machine: start.machine,
        epoch: start.epoch,
        topics: params.topics,
        subjects: params.subjects,
    };
    Ok((
        serde_json::to_value(result).expect("SubscribeResult serializes"),
        stream,
    ))
}

/// One `events.subscribe` on a connection.
struct EventStream {
    sub: Subscription,
    /// The subscribe request's `id`, set on every frame of the stream.
    id: Option<String>,
    machine: String,
    epoch: String,
    topics: Vec<String>,
    subjects: Vec<String>,
}

impl EventStream {
    fn wants(&self, e: &Event) -> bool {
        let topic_ok = self.topics.is_empty()
            || self.topics.iter().any(|t| match t.strip_suffix(".*") {
                Some(prefix) => e
                    .topic
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.')),
                None => *t == e.topic,
            });
        let subject_ok = self.subjects.is_empty()
            || e.subject
                .as_ref()
                .is_some_and(|s| self.subjects.contains(s));
        topic_ok && subject_ok
    }

    fn frame(&self, e: Event) -> EventFrame {
        EventFrame {
            v: API_MAJOR,
            kind: EventType::Event,
            sub: self.id.clone(),
            cursor: format!("{}:{}:{}", self.machine, self.epoch, e.seq),
            topic: e.topic,
            subject: e.subject,
            ts: e.ts,
            origin: e.origin,
            data: e.data,
        }
    }

    /// Send events until the connection closes or the subscription fails.
    /// A full write queue only parks this task.
    async fn run(mut self, tx: mpsc::Sender<Vec<u8>>) {
        loop {
            let batch = match self.sub.next().await {
                Ok(Some(batch)) => batch,
                Ok(None) => return,
                Err(e) => {
                    let _ = tx
                        .send(encode(&Response::err(self.id.clone(), read_error(e))))
                        .await;
                    return;
                }
            };
            for e in batch {
                if !self.wants(&e) {
                    continue;
                }
                if tx.send(encode_event(&self.frame(e))).await.is_err() {
                    return;
                }
            }
        }
    }
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
        cursor: ctx.db.as_ref().map(|db| db.head_cursor().to_string()),
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
            agent_bin: None,
            db: None,
            merge: Arc::new(MergeTable::new()),
            standing_orders: None,
            spawner: None,
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

    // ── Store-backed methods (C1a-13) ──────────────────────────────────

    use crate::engine::db::outbox::{self, NewEvent};
    use tokio::io::Lines;
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

    const MACHINE: &str = "m-test";

    /// `docs/api/c9p-1.json` must equal `schema()`. With
    /// `C9P_SCHEMA_WRITE=1` the test rewrites the file instead.
    #[test]
    fn schema_file_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/api/c9p-1.json");
        let want = serde_json::to_string_pretty(&schema()).unwrap() + "\n";
        if std::env::var_os("C9P_SCHEMA_WRITE").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, want).unwrap();
            return;
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            have == want,
            "docs/api/c9p-1.json is stale. Run `C9P_SCHEMA_WRITE=1 cargo test --no-default-features \
             --features engine schema_file_is_current`, then `node scripts/gen-c9p-types.mjs`."
        );
    }

    #[test]
    fn schema_lists_every_method_and_resolves_every_ref() {
        let s = schema();
        let methods: Vec<&str> = s["methods"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut want = METHODS.to_vec();
        want.sort();
        assert_eq!(methods, want);
        let text = s.to_string();
        let defs = s["$defs"].as_object().unwrap();
        for r in text.split("\"$ref\":\"#/$defs/").skip(1) {
            let name = &r[..r.find('"').unwrap()];
            assert!(defs.contains_key(name), "dangling $ref {}", name);
        }
        let codes = serde_json::to_string(&s["$defs"]["ErrorCode"]).unwrap();
        for code in ["upgrade_required", "cursor_expired", "internal"] {
            assert!(codes.contains(code), "ErrorCode lacks {}", code);
        }
    }

    /// Every method in the schema is served: none answers `unknown_method`.
    #[tokio::test]
    async fn every_schema_method_is_served() {
        let (_t, ctx, _db) = ctx_db(Arc::new(NoLiveAgents));
        let (mut c, _) = Client::new(ctx).await;
        for (i, m) in METHODS.iter().enumerate() {
            let r = c.call(&i.to_string(), m, json!({})).await;
            if is_hook_class(m) {
                // Served, but only to an agent (C1a-21).
                assert_eq!(r["error"]["code"], "peer_not_authorized", "{}: {}", m, r);
            } else if *m == "agent.create" {
                // Off without the experimental switch (C1a-26).
                assert_eq!(r["error"]["code"], "experimental_disabled", "{}: {}", m, r);
            } else if *m == "agent.retire" {
                assert_eq!(r["error"]["code"], "internal", "{}: {}", m, r);
            } else {
                assert_eq!(r["ok"], true, "{}: {}", m, r);
            }
        }
        let r = c.call("x", "no.such", json!({})).await;
        assert_eq!(r["error"]["code"], "unknown_method");
    }

    /// A server context with a fresh store, and the store.
    fn ctx_db(agents: Arc<dyn LiveAgents>) -> (tempfile::TempDir, Arc<ServerCtx>, Arc<Db>) {
        let t = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::engine::db::open(&t.path().join("engine"), MACHINE).unwrap());
        let ctx = Arc::new(ServerCtx {
            engine_version: "9.9.9".into(),
            machine_id: MACHINE.into(),
            hello_timeout: HELLO_TIMEOUT,
            agents,
            engine_uid: peer::engine_uid(),
            agent_bin: None,
            db: Some(db.clone()),
            merge: Arc::new(MergeTable::new()),
            standing_orders: None,
            spawner: None,
        });
        (t, ctx, db)
    }

    struct Client {
        lines: Lines<BufReader<OwnedReadHalf>>,
        write: OwnedWriteHalf,
    }

    impl Client {
        /// Connect and say `hello`; returns the hello result too.
        async fn new(ctx: Arc<ServerCtx>) -> (Client, Value) {
            let (read, write) = connect(ctx).into_split();
            let mut c = Client {
                lines: BufReader::new(read).lines(),
                write,
            };
            let r = c.call("h", "hello", json!({})).await;
            assert_eq!(r["ok"], true, "{}", r);
            (c, r["result"].clone())
        }

        async fn send(&mut self, id: &str, method: &str, params: Value) {
            let mut line =
                serde_json::to_vec(&json!({"v": 1, "id": id, "method": method, "params": params}))
                    .unwrap();
            line.push(b'\n');
            self.write.write_all(&line).await.unwrap();
        }

        async fn next(&mut self) -> Value {
            let line = tokio::time::timeout(Duration::from_secs(10), self.lines.next_line())
                .await
                .expect("a frame within 10 s")
                .unwrap()
                .expect("connection open");
            serde_json::from_str(&line).unwrap()
        }

        async fn call(&mut self, id: &str, method: &str, params: Value) -> Value {
            self.send(id, method, params).await;
            self.next().await
        }
    }

    fn event(topic: &str, subject: &str, n: usize, pad: usize) -> NewEvent {
        NewEvent {
            topic: topic.into(),
            subject: Some(subject.into()),
            origin: Some(json!({"kind": "system"})),
            data: json!({"n": n, "pad": "x".repeat(pad)}),
        }
    }

    /// Emit `n` events, one write transaction each.
    async fn emit(db: &Db, n: usize, pad: usize) {
        for i in 0..n {
            let e = event("agent.upsert", "m-test/a1", i, pad);
            db.write(move |tx| tx.emit(&e)).await.unwrap();
        }
    }

    fn seq_of(frame: &Value) -> i64 {
        Cursor::parse(frame["cursor"].as_str().unwrap())
            .unwrap()
            .seq
    }

    #[tokio::test]
    async fn hello_and_snapshot_return_the_head_cursor() {
        let (_t, ctx, db) = ctx_db(Arc::new(NoLiveAgents));
        emit(&db, 3, 0).await;
        let head = format!("{}:{}:3", MACHINE, db.epoch());
        let (mut c, hello) = Client::new(ctx).await;
        assert_eq!(hello["cursor"], head);
        let r = c.call("1", "snapshot.get", json!({})).await;
        assert_eq!(r["ok"], true, "{}", r);
        assert_eq!(r["result"]["cursor"], head);
        assert_eq!(r["result"]["machineId"], MACHINE);
        assert_eq!(r["result"]["agents"], json!([]));
    }

    #[tokio::test]
    async fn snapshot_agents_are_typed_camel_case_rows() {
        let (_t, ctx, db) = ctx_db(Arc::new(NoLiveAgents));
        db.write(|tx| {
            tx.execute(
                "INSERT INTO agents (id, role, provider, provider_sid, source, \
                 interrupted_note_pending) \
                 VALUES ('a1', 'observed', 'claude', 'sid-1', 'detected', 1)",
                [],
            )
        })
        .await
        .unwrap();
        let (mut c, _) = Client::new(ctx).await;
        let r = c.call("1", "snapshot.get", json!({})).await;
        assert_eq!(
            r["result"]["agents"],
            json!([{
                "id": "a1",
                "generation": 0,
                "mode": null,
                "role": "observed",
                "control": "none",
                "provider": "claude",
                "providerSid": "sid-1",
                "source": "detected",
                "createdBy": null,
                "goneAt": null,
                "resumeCount": 0,
                "interruptedNotePending": true,
                "sessionKey": null,
                "machineId": null,
                "state": null,
                "status": null,
                "cwd": null,
                "name": null,
                "pid": null,
                "startedAt": null,
                "createdAt": null,
                "lastSeen": null,
                "parentId": null
            }])
        );
    }

    #[tokio::test]
    async fn slow_subscriber_does_not_block_the_writer_and_catches_up_without_gaps() {
        let (_t, ctx, db) = ctx_db(Arc::new(NoLiveAgents));
        let (mut c, _) = Client::new(ctx).await;
        let r = c.call("s", "events.subscribe", json!({})).await;
        assert_eq!(r["ok"], true, "{}", r);
        assert_eq!(seq_of(&r["result"]), 0);

        // The client reads nothing while ~6 MB of events are written: far
        // more than the socket buffer and the write queue hold.
        const N: usize = 3000;
        tokio::time::timeout(Duration::from_secs(30), emit(&db, N, 2000))
            .await
            .expect("the writer is not blocked by the slow subscriber");

        let mut seqs = Vec::with_capacity(N);
        while seqs.len() < N {
            let f = c.next().await;
            assert_eq!(f["type"], "event", "{}", f);
            assert_eq!(f["sub"], "s");
            assert_eq!(f["data"]["n"], seqs.len());
            seqs.push(seq_of(&f));
        }
        assert_eq!(seqs, (1..=N as i64).collect::<Vec<_>>());

        // Caught up, the stream is live again.
        emit(&db, 1, 0).await;
        assert_eq!(seq_of(&c.next().await), N as i64 + 1);
    }

    #[tokio::test]
    async fn expired_cursor_errors_and_the_snapshot_cursor_resumes() {
        let (_t, ctx, db) = ctx_db(Arc::new(NoLiveAgents));
        emit(&db, 5, 0).await;
        db.writer()
            .call(|c| outbox::prune_with(c, 0, 2))
            .await
            .unwrap()
            .unwrap();
        let (mut c, _) = Client::new(ctx).await;
        let epoch = db.epoch().to_string();

        // Events 2 and 3 are gone.
        let r = c
            .call(
                "1",
                "events.subscribe",
                json!({"since": format!("{}:{}:1", MACHINE, epoch)}),
            )
            .await;
        assert_eq!(r["error"]["code"], "cursor_expired", "{}", r);
        assert!(r["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("snapshot.get"));
        // An old epoch.
        let r = c
            .call(
                "2",
                "events.subscribe",
                json!({"since": format!("{}:old:5", MACHINE)}),
            )
            .await;
        assert_eq!(r["error"]["code"], "cursor_expired", "{}", r);
        // Not a cursor.
        let r = c
            .call("3", "events.subscribe", json!({"since": "nope"}))
            .await;
        assert_eq!(r["error"]["code"], "bad_request", "{}", r);

        let snap = c.call("4", "snapshot.get", json!({})).await;
        let cursor = snap["result"]["cursor"].as_str().unwrap().to_string();
        let r = c
            .call("5", "events.subscribe", json!({"since": cursor}))
            .await;
        assert_eq!(r["ok"], true, "{}", r);
        assert_eq!(r["result"]["cursor"], cursor);
        emit(&db, 1, 0).await;
        let f = c.next().await;
        assert_eq!(f["sub"], "5");
        assert_eq!(f["cursor"], format!("{}:{}:6", MACHINE, epoch));
        assert_eq!(f["topic"], "agent.upsert");
        assert_eq!(f["subject"], "m-test/a1");
        assert_eq!(f["origin"]["kind"], "system");
    }

    #[tokio::test]
    async fn subscribe_replays_from_since_and_filters() {
        let (_t, ctx, db) = ctx_db(Arc::new(NoLiveAgents));
        for (topic, subject) in [
            ("agent.upsert", "m/a1"),
            ("session.seen", "m/a1"),
            ("agent.gone", "m/a2"),
            ("agentx.y", "m/a2"),
        ] {
            let e = event(topic, subject, 0, 0);
            db.write(move |tx| tx.emit(&e)).await.unwrap();
        }
        let since = format!("{}:{}:0", MACHINE, db.epoch());
        let (mut c, _) = Client::new(ctx).await;
        let r = c
            .call(
                "t",
                "events.subscribe",
                json!({"since": since, "topics": ["agent.*"]}),
            )
            .await;
        assert_eq!(r["ok"], true, "{}", r);
        // Events of "t" may arrive before the response to "s".
        c.send(
            "s",
            "events.subscribe",
            json!({"since": since, "subjects": ["m/a2"]}),
        )
        .await;
        let mut got = Vec::new();
        while got.len() < 4 {
            let f = c.next().await;
            if f["type"] == "event" {
                got.push((f["sub"].as_str().unwrap().to_string(), seq_of(&f)));
            } else {
                assert_eq!((&f["id"], &f["ok"]), (&json!("s"), &json!(true)), "{}", f);
            }
        }
        got.sort();
        assert_eq!(
            got,
            [
                ("s".to_string(), 3),
                ("s".to_string(), 4),
                ("t".to_string(), 1),
                ("t".to_string(), 3)
            ]
        );
    }

    #[tokio::test]
    async fn an_agent_principal_may_read_the_store() {
        let me = std::process::id() as libc::pid_t;
        let parent = peer::proc_info(me).unwrap().ppid;
        let start = peer::proc_info(parent).unwrap().start;
        let agents = principal::tests::FakeAgents::one("a7", parent, start);
        let (_t, ctx, _db) = ctx_db(Arc::new(agents));
        let (mut c, hello) = Client::new(ctx).await;
        assert_eq!(hello["principal"], "agent:a7");
        assert_eq!(c.call("1", "snapshot.get", json!({})).await["ok"], true);
        assert_eq!(c.call("2", "events.subscribe", json!({})).await["ok"], true);
        assert_eq!(c.call("3", "session.list", json!({})).await["ok"], true);
        assert_eq!(c.call("4", "agent.list", json!({})).await["ok"], true);
    }

    /// A detected session's row never makes its processes an agent: only
    /// the engine's live table does (C1a-33, A:186).
    #[tokio::test]
    async fn an_observed_row_is_never_the_agent() {
        use crate::engine::agents::LiveTable;
        use crate::engine::driver::ChildId;
        let me = std::process::id() as libc::pid_t;
        let parent = peer::proc_info(me).unwrap();
        let parent = peer::proc_info(parent.ppid).unwrap();
        let table = Arc::new(LiveTable::new());
        let (_t, ctx, db) = ctx_db(table.clone());
        db.write(move |tx| {
            tx.execute(
                "INSERT INTO agents (id, role, provider, source, control, pid, started_at) \
                 VALUES ('obs', 'observed', 'claude', 'detected', 'none', ?1, 1)",
                [parent.pid],
            )
        })
        .await
        .unwrap();
        let (_c, hello) = Client::new(ctx.clone()).await;
        assert_eq!(hello["principal"], "local-unverified");
        // The same parent as the PTY child of a live engine agent.
        table
            .register(
                "a7",
                2,
                ChildId {
                    pid: parent.pid,
                    start: parent.start,
                },
            )
            .unwrap();
        let (_c, hello) = Client::new(ctx).await;
        assert_eq!(hello["principal"], "agent:a7");
    }

    // ── Hook-class methods (C1a-21) ────────────────────────────────────

    const HOOK_CLASS: &[&str] = &[
        "hook.session_start",
        "hook.stop",
        "hook.permission_request",
        "hook.permission_denied",
        "hook.notification",
        "task.done",
        "agent.handoff",
    ];

    fn hook_params(agent: &str) -> Value {
        json!({"agentId": agent, "kind": "claude", "seq": "1", "report": "r", "note": "n",
               "input": {}})
    }

    fn audit_rows(db: &Db) -> Vec<(String, String, String, String)> {
        let conn = db.reader().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT principal, method, decision, reason FROM audit \
                 WHERE decision = 'deny' ORDER BY seq",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap();
        rows.map(Result::unwrap).collect()
    }

    /// The test process as the descendant of a live agent's PTY child.
    fn as_agent(id: &str) -> Arc<dyn LiveAgents> {
        let me = std::process::id() as libc::pid_t;
        let parent = peer::proc_info(me).unwrap().ppid;
        let start = peer::proc_info(parent).unwrap().start;
        Arc::new(principal::tests::FakeAgents::one(id, parent, start))
    }

    /// A store-backed context whose agent copy is a fake file: no test
    /// process runs it.
    fn ctx_with_bin(agents: Arc<dyn LiveAgents>) -> (tempfile::TempDir, Arc<ServerCtx>, Arc<Db>) {
        let (t, ctx, db) = ctx_db(agents);
        let source = t.path().join("c9watch-agent");
        std::fs::write(&source, b"fake agent").unwrap();
        let bin = Arc::new(AgentBin::install(&t.path().join("engine"), &source));
        let mut ctx = Arc::try_unwrap(ctx).ok().unwrap();
        ctx.agent_bin = Some(bin);
        (t, Arc::new(ctx), db)
    }

    #[tokio::test]
    async fn hook_class_calls_from_a_local_peer_are_refused_and_audited() {
        let (_t, ctx, db) = ctx_with_bin(Arc::new(NoLiveAgents));
        let (mut c, hello) = Client::new(ctx).await;
        assert_eq!(hello["principal"], "local-unverified");
        for (i, m) in HOOK_CLASS.iter().enumerate() {
            let r = c.call(&i.to_string(), m, hook_params("a7")).await;
            assert_eq!(r["error"]["code"], "peer_not_authorized", "{}: {}", m, r);
        }
        let rows = audit_rows(&db);
        assert_eq!(rows.len(), HOOK_CLASS.len());
        for (row, m) in rows.iter().zip(HOOK_CLASS) {
            assert_eq!(row.0, "local-unverified");
            assert_eq!(row.1, *m);
            assert_eq!(row.2, "deny");
            assert!(row.3.starts_with("peer_not_authorized"), "{:?}", row);
        }
    }

    /// An agent's process that is not the engine's `c9watch-agent` copy
    /// (here the test binary; in life, the main binary's `c9watch api`)
    /// is refused.
    #[tokio::test]
    async fn an_agent_running_another_binary_is_refused() {
        let (_t, ctx, db) = ctx_with_bin(as_agent("a7"));
        let (mut c, hello) = Client::new(ctx).await;
        assert_eq!(hello["principal"], "agent:a7");
        for (i, m) in HOOK_CLASS.iter().enumerate() {
            let r = c.call(&i.to_string(), m, hook_params("a7")).await;
            assert_eq!(r["error"]["code"], "peer_not_authorized", "{}: {}", m, r);
            assert!(
                r["error"]["msg"]
                    .as_str()
                    .unwrap()
                    .contains("not the engine's"),
                "{}",
                r
            );
        }
        assert_eq!(audit_rows(&db)[0].0, "agent:a7");
        // Without any copy, the refusal says so.
        let (_t, ctx, _db) = ctx_db(as_agent("a7"));
        let (mut c, _) = Client::new(ctx).await;
        let r = c.call("1", "hook.stop", hook_params("a7")).await;
        assert_eq!(r["error"]["code"], "peer_not_authorized");
        assert!(r["error"]["msg"]
            .as_str()
            .unwrap()
            .contains("no c9watch-agent"));
    }

    /// Reviews: an agent's create or retire is `not_allowed` and audited,
    /// also on an engine that cannot create.
    #[tokio::test]
    async fn an_agent_may_not_create_or_retire_even_with_the_switch_off() {
        let (_t, ctx, db) = ctx_db(as_agent("a7"));
        let (mut c, _) = Client::new(ctx).await;
        for (i, m) in ["agent.create", "agent.retire"].iter().enumerate() {
            let r = c.call(&i.to_string(), m, json!({})).await;
            assert_eq!(r["error"]["code"], "not_allowed", "{}: {}", m, r);
        }
        let rows: Vec<(String, String)> = audit_rows(&db)
            .into_iter()
            .map(|(p, m, _, _)| (p, m))
            .collect();
        assert_eq!(
            rows,
            [
                ("agent:a7".to_string(), "agent.create".to_string()),
                ("agent:a7".to_string(), "agent.retire".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn an_agent_naming_another_agent_is_refused() {
        let (_t, ctx, db) = ctx_with_bin(as_agent("a7"));
        let (mut c, _) = Client::new(ctx).await;
        for params in [hook_params("a8"), json!({})] {
            let r = c.call("1", "task.done", params).await;
            assert_eq!(r["error"]["code"], "peer_not_authorized", "{}", r);
            assert!(r["error"]["msg"].as_str().unwrap().contains("names agent"));
        }
        assert_eq!(audit_rows(&db).len(), 2);
    }

    #[tokio::test]
    async fn an_unknown_hook_method_is_unknown() {
        let (_t, ctx, db) = ctx_with_bin(as_agent("a7"));
        let (mut c, _) = Client::new(ctx).await;
        let r = c.call("1", "hook.pre_tool_use", hook_params("a7")).await;
        assert_eq!(r["error"]["code"], "unknown_method");
        assert!(audit_rows(&db).is_empty());
    }

    // ── Detection-backed lists (C1a-15) ────────────────────────────────

    fn ids(rows: &Value) -> Vec<String> {
        rows.as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn session_list_and_agent_list_read_the_agents_table() {
        use crate::engine::detect::{apply, Observed};
        let (_t, ctx, db) = ctx_db(Arc::new(NoLiveAgents));
        let seen = |sid: &str| Observed {
            key: format!("claudeCode:{}", sid),
            provider: "claudeCode".into(),
            sid: sid.into(),
            status: "working",
            cwd: "/p".into(),
            name: sid.into(),
            pid: 1,
            started_at: None,
        };
        let (a, b) = (seen("a"), seen("b"));
        let both = vec![a.clone(), b];
        db.write(move |tx| apply(tx, MACHINE, &both, 1, &Default::default()))
            .await
            .unwrap();
        db.write(move |tx| {
            apply(tx, MACHINE, &[a], 2, &Default::default())?;
            tx.execute(
                "INSERT INTO agents (id, role, provider, source, state) \
                 VALUES ('agt_zretired', 'worker', 'claudeCode', 'engine', 'retired')",
                [],
            )
        })
        .await
        .unwrap();
        let rows = db.agents(AgentFilter::All).unwrap();
        let id_of = |sid: &str| {
            rows.iter()
                .find(|r| r.provider_sid.as_deref() == Some(sid))
                .unwrap()
                .id
                .clone()
        };

        let (mut c, _) = Client::new(ctx).await;
        let r = c.call("1", "session.list", json!({})).await;
        assert_eq!(r["ok"], true, "{}", r);
        let sessions = &r["result"]["sessions"];
        assert_eq!(ids(sessions), [id_of("a")]);
        assert_eq!(sessions[0]["sessionKey"], "claudeCode:a");
        assert_eq!(sessions[0]["status"], "working");
        assert_eq!(sessions[0]["machineId"], MACHINE);

        let r = c.call("2", "agent.list", Value::Null).await;
        assert_eq!(ids(&r["result"]["agents"]), [id_of("a")]);
        let r = c.call("3", "agent.list", json!({"all": false})).await;
        assert_eq!(ids(&r["result"]["agents"]), [id_of("a")]);
        let r = c.call("4", "agent.list", json!({"all": true})).await;
        let mut want = vec![id_of("a"), id_of("b"), "agt_zretired".to_string()];
        want.sort();
        assert_eq!(ids(&r["result"]["agents"]), want);
        let gone = &r["result"]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id_of("b"))
            .unwrap()["goneAt"];
        assert_eq!(gone, 2);

        let r = c.call("5", "agent.list", json!({"all": "yes"})).await;
        assert_eq!(r["error"]["code"], "bad_request", "{}", r);
    }

    #[tokio::test]
    async fn a_connection_holds_at_most_max_streams() {
        let (_t, ctx, db) = ctx_db(Arc::new(NoLiveAgents));
        let (mut c, _) = Client::new(ctx).await;
        for i in 0..MAX_STREAMS {
            let r = c.call(&i.to_string(), "events.subscribe", json!({})).await;
            assert_eq!(r["ok"], true, "{}", r);
        }
        let r = c.call("over", "events.subscribe", json!({})).await;
        assert_eq!(r["error"]["code"], "rate_limited", "{}", r);
        // The connection and its streams keep working.
        assert_eq!(c.call("snap", "snapshot.get", json!({})).await["ok"], true);
        emit(&db, 1, 0).await;
        let f = c.next().await;
        assert_eq!(f["type"], "event", "{}", f);
    }

    #[tokio::test]
    async fn store_methods_need_hello_first() {
        let (_t, ctx, _db) = ctx_db(Arc::new(NoLiveAgents));
        let mut c = connect(ctx);
        send(&mut c, json!({"v": 1, "id": "1", "method": "snapshot.get"})).await;
        let got = read_all(c).await;
        assert_eq!(got[0]["error"]["code"], "hello_required");
    }
}
