//! c9p/1 wire types.
//!
//! These serde types are the single source of truth for the protocol:
//! one JSON object per line, a request/response envelope, typed
//! error codes with a hint, and the `hello` handshake. Unknown input fields
//! are ignored; preview clients detect breaking changes through discovery.
//!
//! They live outside the `engine` feature, so the GUI can read the engine
//! without being built with it. The JSON Schema derive exists only in
//! engine builds, which generate `docs/api/c9p-1.json`.

use serde::{Deserialize, Serialize};

/// Protocol major version. It is also in the socket name (`engine-v1.sock`).
pub const API_MAJOR: u32 = 1;
/// Protocol minor version. Preview breaks also raise method revisions.
pub const API_MINOR: u32 = 11;
/// Largest frame the engine reads, without the newline.
pub const MAX_FRAME: usize = 1 << 20;

/// A request frame. Every field is optional on the wire, so that a frame
/// with a missing field still gets an error that echoes its `id`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub msg: String,
    pub retryable: bool,
    pub hint: String,
    /// Facts about this error for the caller to act on, such as the
    /// `nearest` name for `unknown_address`. Absent for most errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Typed error codes. New codes are additive; a client that does not know a
/// code still has `msg`, `retryable` and `hint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
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
    /// The caller is over a limit, such as the event streams per
    /// connection.
    RateLimited,
    /// A hook-class method (`hook.*`, `task.done`, `agent.handoff`) from a
    /// caller that is not a live agent running the engine's own
    /// `c9watch-agent` copy, or that names another agent. Also a request
    /// that needs the caller's identity on a connection whose process core
    /// could not verify once session detection was ready (it exited, or its
    /// start time is unreadable); connect again.
    PeerNotAuthorized,
    /// `task.done` from an agent without a task (every free agent).
    NoTask,
    /// The operation is part of the wire format but is not supported yet.
    Unsupported,
    /// The engine has no `c9watch-agent` copy: none was next to the engine
    /// binary when it started, or the copy is gone.
    AgentBinMissing,
    /// The engine's `c9watch-agent` copy no longer has the sha256 it had
    /// when the engine installed it.
    AgentBinMismatch,
    /// The principal may not call this method (`agent.create`,
    /// `agent.retire` and `agent.resume` take only a local client).
    NotAllowed,
    /// A widening permission change needs a fresh native confirmation.
    ConfirmationRequired,
    /// The canonical cwd of a free agent is inside the engine home or a
    /// managed worktree home.
    CwdCollision,
    /// The agent process exited, showed a vendor refusal (usage limit,
    /// login) or was not ready in time. The engine does not retry.
    SpawnFailed,
    /// No engine agent has this id.
    UnknownAgent,
    /// The engine is draining after a SIGTERM: no spawn and no injection
    /// until the next engine is up. Retry against the new engine.
    Draining,
    /// `msg.send`: no agent answers to `to`, its machine is not this one,
    /// or a name matches more than one agent. `data` has `nearest` or
    /// `candidates`.
    UnknownAddress,
    /// `msg.send`: the engine does not drive the target's terminal (an
    /// observed session), or the target is retiring or gone. `data` has
    /// `state` and `control`.
    NotControlled,
    /// `msg.send`: a managed worker takes mail only from its parent and
    /// the human, and sends only to its parent. `data` may have `parent`.
    NotOwner,
    /// `msg.send`: the `idem` key was used for another message, or the same
    /// body went to the same agent within 10 minutes. `data` has `msgId`
    /// and `at`. `agent.create`, `agent.retire`, `session.stop`: the `idem`
    /// key was used for a request with other params.
    Duplicate,
    /// `session.stop`: no live session has this key.
    UnknownSession,
    HopLimit,
    NotCancellable,
    /// `session.stop` or `session.rename` was refused or failed on the
    /// session itself: it cannot be stopped, it runs as another process
    /// now, or the write failed. `agent.resume`: the agent is not `paused`
    /// or `interrupted`, already runs, or has nothing to resume. `msg`
    /// says which.
    ActionFailed,
    /// The engine is at a limit that frees up soon: the connection cap, a
    /// scan of the same kind that is still running, every `idem` key in
    /// use, or, while the engine starts, a request that needs the caller's
    /// identity before session detection has finished a pass. The request
    /// has its own `id`; the connection stays open. Retry shortly.
    Busy,
    /// An annotation count, size or write-rate limit was exceeded.
    LimitExceeded,
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
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::PeerNotAuthorized => "peer_not_authorized",
            ErrorCode::NoTask => "no_task",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::AgentBinMissing => "agent_bin_missing",
            ErrorCode::AgentBinMismatch => "agent_bin_mismatch",
            ErrorCode::NotAllowed => "not_allowed",
            ErrorCode::ConfirmationRequired => "confirmation_required",
            ErrorCode::CwdCollision => "cwd_collision",
            ErrorCode::SpawnFailed => "spawn_failed",
            ErrorCode::UnknownAgent => "unknown_agent",
            ErrorCode::Draining => "draining",
            ErrorCode::UnknownAddress => "unknown_address",
            ErrorCode::NotControlled => "not_controlled",
            ErrorCode::NotOwner => "not_owner",
            ErrorCode::Duplicate => "duplicate",
            ErrorCode::HopLimit => "hop_limit",
            ErrorCode::NotCancellable => "not_cancellable",
            ErrorCode::UnknownSession => "unknown_session",
            ErrorCode::ActionFailed => "action_failed",
            ErrorCode::Busy => "busy",
            ErrorCode::LimitExceeded => "limit_exceeded",
            ErrorCode::Internal => "internal",
        }
    }

    /// Whether the same request can succeed when it is sent again unchanged.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            ErrorCode::HelloTimeout | ErrorCode::Draining | ErrorCode::Busy | ErrorCode::Internal
        )
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
            ErrorCode::UnknownMethod => "see `hello.result.methods`",
            ErrorCode::PeerUidMismatch => "connect as the user that runs the engine",
            ErrorCode::CursorExpired => {
                "call `snapshot.get`, then `events.subscribe` with the snapshot's cursor"
            }
            ErrorCode::RateLimited => {
                "use fewer event streams per connection: one subscription with \
                 topic and subject filters, or another connection"
            }
            ErrorCode::PeerNotAuthorized => {
                "only an engine agent may call this, through `c9watch-agent` from the \
                 engine's own copy"
            }
            ErrorCode::NoTask => "free agents have no task; just end the turn",
            ErrorCode::Unsupported => "this engine does not support it yet",
            ErrorCode::AgentBinMissing => {
                "install c9watch-agent next to the c9watch binary, then \
                 `c9watch engine restart`"
            }
            ErrorCode::AgentBinMismatch => {
                "`c9watch engine restart` copies c9watch-agent again; check what changed \
                 the engine's copy"
            }
            ErrorCode::NotAllowed => "inspect auth.can for the required authority",
            ErrorCode::ConfirmationRequired => {
                "confirm the exact permission change in the native GUI dialog"
            }
            ErrorCode::CwdCollision => {
                "pick a cwd outside the engine home and outside `.c9watch/worktrees`"
            }
            ErrorCode::SpawnFailed => {
                "read the vendor text in `msg` (usage limit, login), fix it, then create again"
            }
            ErrorCode::UnknownAgent => "see `agent.list` with `all` for the agent ids",
            ErrorCode::Draining => {
                "the engine is stopping; reconnect once the next engine is up and send it again"
            }
            ErrorCode::UnknownAddress => "see `agent.list` for the agent ids and names",
            ErrorCode::NotControlled => "take over first",
            ErrorCode::NotOwner => "managed workers take mail only from their parent and the human",
            ErrorCode::Duplicate => "pass a new idem to send it again",
            ErrorCode::HopLimit => "the thread is too deep; a person must start a new one",
            ErrorCode::NotCancellable => "only queued mail can be cancelled",
            ErrorCode::UnknownSession => "see `session.views` for the live session keys",
            ErrorCode::ActionFailed => "read `msg`; `session.views` shows the session now",
            ErrorCode::Busy => "retry in a second",
            ErrorCode::LimitExceeded => {
                "reduce annotation keys or wait for the write budget to refill"
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
    /// Goes on the wire as `error.data`.
    pub data: Option<serde_json::Value>,
    /// Replaces the code's fixed hint when the next step depends on the
    /// case, such as `did you mean <nearest>?`.
    pub hint: Option<String>,
}

impl ApiError {
    pub fn new(code: ErrorCode, msg: impl Into<String>) -> Self {
        Self {
            code,
            msg: msg.into(),
            data: None,
            hint: None,
        }
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl From<ApiError> for ErrorBody {
    fn from(e: ApiError) -> Self {
        ErrorBody {
            code: e.code,
            msg: e.msg,
            retryable: e.code.retryable(),
            hint: e.hint.unwrap_or_else(|| e.code.hint().to_string()),
            data: e.data,
        }
    }
}

/// `hello` params. `client` is the caller's own claim. The engine only logs
/// it as unverified; it never decides the principal.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HelloParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_major: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub want: Vec<String>,
    /// The minor the client was built against. `hello.result.breakingSince`
    /// lists the preview methods that changed shape after it. Without it,
    /// the engine assumes minor 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_minor: Option<u32>,
}

/// What a client says about itself in `hello`. Unverified.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct ClientInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// `hello` result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
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
    /// Every method this engine serves (minor 6).
    #[serde(default)]
    pub methods: std::collections::BTreeMap<String, MethodInfo>,
    /// The preview methods whose shape changed after the client's
    /// `apiMinor`. A client refuses to call them; it does not guess.
    #[serde(default)]
    pub breaking_since: Vec<String>,
    /// `preview` until the protocol is declared stable.
    #[serde(default)]
    pub stability: String,
    /// The event topics this connection can receive.
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub limits: Limits,
}

/// One method in `hello.result.methods`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct MethodInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<AuthNeeds>,
    /// The minor that added the method.
    pub since: u32,
    /// Goes up by one on each breaking change to the method's shape.
    pub rev: u32,
    /// `preview`, `internal` or `deprecated`.
    pub stability: String,
}

/// A sensitivity ceiling. Ordering is also the egress ordering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Ceiling {
    Public,
    #[default]
    Metadata,
    Conversation,
}

/// Additional authority a caller would need.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AuthNeeds {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling: Option<Ceiling>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<bool>,
}

/// Ask about the caller's own authority. Params are inspected, never executed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AuthCanParams {
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AuthCanResult {
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs: Option<AuthNeeds>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AuthRulesResult {
    pub principal: String,
    pub scopes: Vec<String>,
    pub ceiling: Ceiling,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_expires_at: Option<i64>,
    pub methods: std::collections::BTreeMap<String, MethodInfo>,
    pub topics: Vec<String>,
}

/// The engine's limits, in `hello.result.limits`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    /// Bytes in one frame, newline included.
    pub max_frame: u64,
    /// Event streams on one connection.
    pub max_streams: u64,
    /// Connections the socket serves at once.
    pub max_connections: u64,
    /// Hours the outbox keeps an event.
    pub retention_hours: u64,
    /// Events the outbox keeps at most.
    pub retention_events: u64,
}

/// `events.unsubscribe` params: the `id` of the `events.subscribe` request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct UnsubscribeParams {
    pub sub: String,
}

/// `events.unsubscribe` result. No event frame of the stream follows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct UnsubscribeResult {
    /// `false` when the connection held no stream with that `sub`.
    pub ended: bool,
}

/// `ping` params. There are none.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct PingParams {}

/// `ping` result: the engine's clock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct PingResult {
    /// Unix milliseconds.
    pub ts: i64,
}

/// `snapshot.get` params. There are none yet; the engine ignores any it
/// gets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct SnapshotParams {}

/// `snapshot.get` result: the state and the cursor it is consistent with.
/// Subscribing with `since` = `cursor` continues with no gap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SnapshotResult {
    pub cursor: String,
    pub machine_id: String,
    pub agents: Vec<AgentRow>,
    /// Unix ms of the last detection pass that was written: the
    /// `lastOkAt` of `detectorHealth`, read before the rows, so the rows
    /// are never older than it. Absent in an engine without detection, or
    /// before its first good pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<i64>,
    /// How detection is doing (the `detect.health` data). Absent in an
    /// engine without detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detector_health: Option<DetectorHealth>,
}

/// One row of the `agents` table, as `snapshot.get`,
/// `session.list` and `agent.list` send it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentRow {
    /// `agt_<ulid>`.
    pub id: String,
    /// +1 on spawn, resume, refresh and takeover.
    pub generation: i64,
    /// `managed` or `free`; `null` for an observed agent.
    pub mode: Option<String>,
    /// `human`, `orch`, `worker` or `observed`.
    pub role: String,
    /// `pty` or `none`.
    pub control: String,
    /// The adapter kind: `claude`, `codex`, ...
    pub provider: String,
    /// The provider session id of the current generation.
    pub provider_sid: Option<String>,
    /// `detected`, `engine` or `legacy_import`.
    pub source: String,
    /// The principal that created the agent.
    pub created_by: Option<String>,
    /// Unix ms; `null` while the agent is alive.
    pub gone_at: Option<i64>,
    pub resume_count: i64,
    pub interrupted_note_pending: bool,
    /// `SessionIdentity::key()` (`<provider>:<session id>`) of the session
    /// detection matched to this agent.
    pub session_key: Option<String>,
    /// The machine the agent runs on.
    pub machine_id: Option<String>,
    /// Engine lifecycle state (`creating`, `starting`, `ready`, `working`,
    /// `blocked`, `paused`, `refreshing`, `interrupted`, `retiring`,
    /// `retired`, `failed`, `orphaned`); `null` for an observed agent.
    /// `refreshing` and `retiring` are reserved: the engine does not emit
    /// them today, but clients must accept them. `orphaned` means no kill
    /// (retire, exit cleanup or boot reconcile) could confirm that the
    /// agent's process ended; the agent is not resumable, `goneAt` stays
    /// `null`, and `agent.retire` tries the kill again.
    pub state: Option<String>,
    /// The detected session status: `working`, `needs_attention`,
    /// `waiting_for_input` or `connecting`.
    pub status: Option<String>,
    /// The working directory; for an observed agent, the project path.
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub pid: Option<i64>,
    /// Unix ms, as the provider reports it.
    pub started_at: Option<i64>,
    /// Unix ms.
    pub created_at: Option<i64>,
    /// Unix ms of a detection tick that saw the agent. While nothing else
    /// in the row changes, it moves at most once a minute.
    pub last_seen: Option<i64>,
    /// The agent that owns this one; for an imported legacy worker, the PM
    /// that adopted it.
    pub parent_id: Option<String>,
    /// Why the agent is in its `state`: `drain` for a `paused` or
    /// `interrupted` agent that an engine drain stopped; `resume_failed`
    /// for a `paused` or `interrupted` agent whose last resume failed
    /// (`agent.resume` tries again); for a `failed` agent the failure
    /// reason, such as `spawn_failed`, `resume_failed` (nothing to
    /// resume), `exited` or `create_interrupted`; `kill_unconfirmed` for
    /// an `orphaned` agent.
    pub state_reason: Option<String>,
}

/// `session.list` params. There are none yet; the engine ignores any it
/// gets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct SessionListParams {}

/// `session.list` result: the agents whose session detection sees now
/// (`goneAt` is `null`), in creation order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SessionListResult {
    pub sessions: Vec<AgentRow>,
    /// As in `snapshot.get`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<i64>,
    /// As in `snapshot.get`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detector_health: Option<DetectorHealth>,
}

/// `session.views` params. There are none yet; the engine ignores any it
/// gets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct SessionViewsParams {}

/// `session.views` result: the sessions of the last detection tick, as the
/// GUI shows them, in detection order, and the cursor they are consistent
/// with. Subscribing to `session.*` with `since` = `cursor` continues with
/// no gap: `session.view` carries a new or changed session in full,
/// `session.gone` (`SessionGoneEvent`) one that ended.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct SessionViewsResult {
    pub cursor: String,
    pub sessions: Vec<crate::session::Session>,
}

/// `session.gone` data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SessionGoneEvent {
    /// `<provider>:<session id>`.
    pub session_key: String,
    /// The provider session id.
    pub id: String,
}

/// How session detection is doing: the `detect.health` data, and the
/// `detectorHealth` of `snapshot.get` and `session.list`. A failed
/// detection writes nothing, so the rows keep what the last good pass saw;
/// this says how old that is. While passes are written, `detect.health`
/// repeats every `staleAfterMs / 2` with a newer `lastOkAt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DetectorHealth {
    /// `ok`: the last pass was written. `degraded`: it was written, but
    /// the providers in `failedProviders` did not answer, and their rows
    /// keep what an earlier pass saw. `failed`: the last pass wrote
    /// nothing. `starting`: no pass has finished yet. Clients must accept
    /// other values.
    pub state: String,
    /// Why the state is not `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Provider keys (`claudeCode`, `codex`, ...) that did not answer the
    /// last written pass.
    #[serde(default)]
    pub failed_providers: Vec<String>,
    /// Unix ms of the last pass that was written (`observedAt`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ok_at: Option<i64>,
    /// Unix ms when the last pass finished, written or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<i64>,
    /// No pass was written for `staleAfterMs`: every detected session row
    /// may be out of date. Rows whose provider is in `failedProviders` may
    /// be out of date even when this is false.
    pub stale: bool,
    pub stale_after_ms: i64,
}

/// `agent.attention` data: an agent needs the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentAttentionEvent {
    pub id: String,
    pub generation: i64,
    /// Why: a permission prompt, a notification, `mail_uncertain: <msg>`.
    pub reason: String,
    /// The message whose delivery is uncertain, for `mail_uncertain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    /// The hook's `tool_use_id` (may be `null`), for a hook's attention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<serde_json::Value>,
}

/// `agent.turn_end` data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AgentTurnEndEvent {
    pub id: String,
    pub generation: i64,
    /// What saw the turn end: `hook` or `screen`.
    pub by: String,
    /// Unix milliseconds.
    pub at: i64,
}

/// `agent.gone` data: the session behind an observed agent ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentGoneEvent {
    pub id: String,
    pub session_key: String,
    /// Unix milliseconds.
    pub gone_at: i64,
}

/// `agent.failed` data: a managed agent could not be started or kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AgentFailedEvent {
    pub id: String,
    pub reason: String,
    pub detail: String,
}

/// `notification.needs_attention` and `notification.waiting_for_input`
/// data: what the GUI shows in its system notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct NotificationEvent {
    /// The agent id of the session.
    pub id: String,
    pub session_key: String,
    /// `needs_attention` or `waiting_for_input`.
    pub status: String,
    pub name: String,
    pub pending_tool: Option<String>,
    /// Can carry conversation text (the first prompt, a session title).
    pub title: String,
    /// `title` without conversation text: the project folder name, else the
    /// provider's name (e.g. `Claude Code`). Never a session title or name.
    pub redacted_title: String,
    pub body: String,
    pub pid: u32,
    pub provider: crate::session::SessionProvider,
}

/// `legacy.inbox` data: one imported worker inbox line, as it was.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct LegacyInboxEvent {
    pub legacy: bool,
    /// The worker's agent id, when the import found its row.
    pub id: Option<String>,
    /// The original inbox line.
    pub event: serde_json::Value,
}

/// `session.away` params: one request per session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct SessionAwayParams {
    #[serde(default)]
    pub requests: Vec<crate::session::away::AwayRequest>,
}

/// `session.away` result: what each session did after its `sinceMs`,
/// keyed by session key. A session without an index (not Claude Code, or
/// not seen by a detection tick yet) is absent.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct SessionAwayResult {
    pub activity: std::collections::HashMap<String, crate::session::away::AwayActivity>,
}

/// `detect.recheck` params. There are none yet; the engine ignores any it
/// gets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct DetectRecheckParams {}

/// `detect.recheck`, `session.stop` and `session.rename` result. The call
/// ran one detection tick before it answered: its `session.view` and
/// `session.gone` events are at or before `cursor`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TickResult {
    pub cursor: String,
    /// The detection backend after the tick: `cli` or `legacy`.
    pub backend: String,
}

/// `session.stop` params: stop an observed Claude Code session with
/// SIGTERM. The pid comes from the engine's own detection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SessionStopParams {
    /// `<provider>:<session id>`.
    pub session_key: String,
    /// When given, detection must still see the session as this pid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// `session.rename` params: set a Claude Code session's custom title.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SessionRenameParams {
    /// The provider session id.
    pub session_id: String,
    pub new_name: String,
    /// Only `claudeCode`. Without it, an id that another provider also
    /// has is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<crate::session::SessionProvider>,
}

/// `conversation.get` params: one session's transcript, as the GUI's
/// `get_conversation` reads it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ConversationGetParams {
    /// `<provider>:<session id>`. A bare session id (no known provider
    /// prefix) is looked up in every provider, or in `provider`.
    pub session_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<crate::session::SessionProvider>,
    /// Keep tool-use and tool-result messages. Default false, as in the GUI.
    #[serde(default)]
    pub include_tools: bool,
    /// Without it, the first page (`offset` 0, the default `limit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<PageParams>,
}

/// The default `PageParams::limit`.
pub const PAGE_DEFAULT_LIMIT: usize = 200;
/// The largest `PageParams::limit`; a larger one is treated as this.
pub const PAGE_MAX_LIMIT: usize = 1000;
/// The serialized size a page's items stay under, well below
/// `MAX_FRAME` so that the response envelope fits too.
pub const PAGE_BUDGET: usize = MAX_FRAME / 2;

/// A window of a list: at most `limit` items from `offset`.
///
/// A page also stops before its items would pass a byte budget (half of
/// `limits.maxFrame`), so it can hold fewer than `limit` items. A page from
/// an `offset` before the end always holds at least one item, so
/// `nextOffset` always moves forward. An item too large for the budget on
/// its own comes alone, with its large text replaced by a short note and
/// `truncated: true`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PageParams {
    #[serde(default)]
    pub offset: usize,
    /// At most this many items. Default 200; a value over 1000 is treated
    /// as 1000; 0 is `bad_request`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "engine", schemars(range(min = 1)))]
    pub limit: Option<usize>,
}

/// `conversation.get` result. `messages` are the page asked for, in
/// transcript order (see `PageParams` for the page size). Each call reads
/// the transcript again.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ConversationGetResult {
    pub session_id: String,
    pub provider: crate::session::SessionProvider,
    pub messages: Vec<crate::session::ConversationMessage>,
    /// Messages in the whole conversation.
    pub total: usize,
    pub offset: usize,
    /// `offset` of the next page; `null` after the last one.
    pub next_offset: Option<usize>,
}

/// A `progress` frame: how far a read for request `id` has got. Sent only
/// to a connection that asked for the `conversation.progress` capability,
/// before that request's response. Never stored; a slow client may miss
/// some.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProgressFrame {
    pub v: u32,
    #[serde(rename = "type")]
    pub kind: ProgressType,
    /// The `id` of the request it reports on.
    pub id: Option<String>,
    pub bytes_read: u64,
    pub bytes_total: u64,
}

/// The `type` of a progress frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub enum ProgressType {
    #[serde(rename = "progress")]
    Progress,
}

/// `history.list` params.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HistoryListParams {
    /// Without it, the first page (`offset` 0, the default `limit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<PageParams>,
}

/// `history.list` result: one page of past sessions of every provider,
/// newest first, as the GUI's `get_session_history` lists them.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HistoryListResult {
    pub entries: Vec<crate::session::HistoryEntry>,
    /// Entries in the whole history.
    pub total: usize,
    pub offset: usize,
    /// `offset` of the next page; `null` after the last one.
    pub next_offset: Option<usize>,
}

/// `history.search` params: a phrase search over Claude Code transcripts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HistorySearchParams {
    pub query: String,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default)]
    pub whole_word: bool,
}

/// `history.search` result: one hit per matching session. A blank query
/// has none.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct HistorySearchResult {
    pub hits: Vec<crate::session::DeepSearchHit>,
}

/// `cost.get` params. There are none yet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct CostGetParams {}

/// `memory.list` params.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MemoryListParams {
    /// Without it, the first page (`offset` 0, the default `limit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<PageParams>,
}

/// `memory.list` result: one page of memory files grouped by project, as
/// the GUI's `get_memory_files` reads them. A page item is a project.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MemoryListResult {
    pub projects: Vec<crate::session::ProjectMemory>,
    /// Projects in the whole list.
    pub total: usize,
    pub offset: usize,
    /// `offset` of the next page; `null` after the last one.
    pub next_offset: Option<usize>,
}

/// `subagent.list` params: the raw Claude Code session ids the caller
/// shows (not session keys).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SubagentListParams {
    #[serde(default)]
    pub session_ids: Vec<String>,
}

/// `subagent.list` result: parent session id → its subagent invocations.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct SubagentListResult {
    pub subagents: std::collections::HashMap<String, Vec<crate::session::subagents::SubagentInfo>>,
}

/// `subagent.transcript` params.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SubagentTranscriptParams {
    pub parent_session_id: String,
    /// The Agent tool's `tool_use_id`.
    pub subagent_id: String,
}

/// `session.tasks` params.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SessionTasksParams {
    /// The raw Claude Code session id.
    pub session_id: String,
}

/// `session.tasks` result: the session's TodoWrite tasks as Claude Code
/// wrote them, sorted by numeric `id`.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct SessionTasksResult {
    pub tasks: Vec<serde_json::Value>,
}

/// `usage.subscriptions` params. There are none yet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct UsageSubscriptionsParams {}

/// `usage.subscriptions` result: one snapshot per provider plan. The
/// engine reads the providers at most once a minute.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct UsageSubscriptionsResult {
    pub usage: Vec<crate::session::subscription_usage::SubscriptionUsage>,
}

/// `provider.opencode.status` params. There are none yet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct OpencodeStatusParams {}

/// `provider.opencode.connect` params: the OpenCode server detection
/// reads. An empty `url` disconnects. The engine keeps the credentials in
/// memory only and never returns them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct OpencodeConnectParams {
    pub url: String,
    #[serde(default)]
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// `diag.detection` params. There are none yet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct DiagDetectionParams {}

/// `agent.list` params.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AgentListParams {
    /// Also return gone, retired and failed agents (tombstones included).
    #[serde(default)]
    pub all: bool,
}

/// `agent.list` result, in creation order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AgentListResult {
    pub agents: Vec<AgentRow>,
}

/// `agent.create` params. Only free agents are supported: no brief,
/// no worktree, the given cwd.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentCreateParams {
    /// The adapter kind. Only `claude` is supported.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// `free`. `managed` is not supported and answers `unsupported`.
    pub mode: String,
    /// An absolute directory. The engine uses its realpath.
    pub cwd: String,
    /// A display name: letters, digits, `.`, `_` and `-`, unique among
    /// the live engine agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

fn default_kind() -> String {
    "claude".into()
}

/// `agent.create` result: the agent once it is `ready`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AgentCreateResult {
    pub agent: AgentRow,
}

/// `agent.retire` params.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AgentRetireParams {
    /// The agent id (`agt_<ulid>`).
    pub id: String,
    /// End an `orphaned` agent even when the kill cannot confirm that
    /// every process is gone: core kills by pid and start time as
    /// always, then writes the tombstone and an audit row that names the
    /// forced retire. The local client only, as `agent.retire` is. No
    /// effect on an agent that is not `orphaned`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
}

/// `agent.retire` result: the tombstone row. Retiring a retired or
/// failed agent changes nothing and returns its row. When the kill cannot
/// be confirmed, the row is `orphaned` instead; retire it again to retry
/// the kill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AgentRetireResult {
    pub agent: AgentRow,
}

/// `agent.resume` params.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AgentResumeParams {
    /// The agent id (`agt_<ulid>`) of a `paused` or `interrupted` engine
    /// agent.
    pub id: String,
}

/// `agent.resume` result: the agent once it is `ready`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AgentResumeResult {
    pub agent: AgentRow,
}

/// What a message asks of its reader. A caller sends `request` or
/// `info`; `result` and `system` are written by the engine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MsgKind {
    Request,
    #[default]
    Info,
    Result,
    System,
}

impl MsgKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MsgKind::Request => "request",
            MsgKind::Info => "info",
            MsgKind::Result => "result",
            MsgKind::System => "system",
        }
    }
}

/// `msg.send` params. The envelope's `idem` makes a resend safe: the same
/// sender, key and request get the first message back.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MsgSendParams {
    /// An agent id (`agt_<ulid>`), a name, or `<machine>/<id>` with this
    /// machine's id.
    pub to: String,
    /// UTF-8 text. No control characters other than `\n` and `\t`.
    pub body: String,
    /// `request` or `info`.
    #[serde(default)]
    pub kind: MsgKind,
    /// The id of the message this one answers. Stored, not checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

/// The agent a message went to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MsgAddress {
    pub id: String,
    pub name: Option<String>,
    /// `<machine>/<id>`.
    pub addr: String,
}

/// `msg.send` result. The response's `seq` is the same cursor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MsgSendResult {
    /// `msg_<ulid>`.
    pub msg_id: String,
    /// The message's state now: `queued` for a new message.
    pub state: String,
    pub to: MsgAddress,
    /// Cursor of the message's `msg.state` `queued` event. For a replay it
    /// is the first send's cursor, which may be past event retention.
    pub cursor: String,
    /// The same sender sent the same request with this `idem` before; no
    /// new message was written.
    pub replayed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
}

/// `data` of a `msg.state` event: one per state change of a message. The
/// event's subject is the target's `<machine>/<id>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MsgStateEvent {
    pub msg_id: String,
    /// The sender's principal: `agent:<id>` or `local-unverified`.
    pub from: String,
    /// The target's agent id.
    pub to: String,
    /// `queued`, `delivering`, `delivered`, `failed`, `uncertain`, `rejected`, `expired` or `cancelled`.
    pub state: String,
    /// Unix ms.
    pub at: i64,
    /// Delivery attempts so far.
    pub attempts: i64,
    /// Why: for `delivered`, `prompt_hook`, `peer_jsonl`, `late_evidence` or `turn_open`; for `failed` and
    /// `uncertain`, what went wrong.
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<MsgFrom>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<MsgAddress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// `events.subscribe` params. Empty filters match everything.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SubscribeResult {
    /// Where the stream starts: events after this cursor follow.
    pub cursor: String,
}

/// One event on the wire. `sub` is the `id` of the subscribe
/// request, so a connection can carry several subscriptions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct EventFrame {
    pub v: u32,
    #[serde(rename = "type")]
    pub kind: EventType,
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

/// The `type` of an event frame. A response frame has no `type`, so a
/// client tells the two apart by `type === "event"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub enum EventType {
    #[serde(rename = "event")]
    Event,
}

/// The hook-class methods. Only a live agent may call them, and only
/// through the engine's own copy of `c9watch-agent`.
pub const HOOK_METHODS: &[&str] = &[
    "hook.session_start",
    "hook.stop",
    "hook.permission_request",
    "hook.permission_denied",
    "hook.notification",
    "hook.user_prompt_submit",
    "hook.post_tool_use",
];

/// Whether `method` is hook-class: a `hook.*` method, `task.done` or
/// `agent.handoff`.
pub fn is_hook_class(method: &str) -> bool {
    method.starts_with("hook.") || method == "task.done" || method == "agent.handoff"
}

/// `hook.*` params: one Claude Code hook event, sent by `c9watch-agent
/// hook <event>`. The engine alone writes the log.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HookParams {
    /// `C9_AGENT_ID` of the sender. It must be the agent the engine stamped.
    pub agent_id: String,
    /// The adapter kind of the hook (`claude`). The engine compares it with
    /// the agent row.
    pub kind: String,
    /// The sender's clock in ns, as a string. With `agentId` it dedupes.
    pub seq: String,
    /// The hook's stdin JSON as the vendor sent it; `null` when it was not
    /// JSON or too large.
    #[serde(default)]
    pub input: serde_json::Value,
    /// The stdin JSON was larger than the hook forwards.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// `hook.*` result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HookResult {
    /// The engine took the event. A dropped event (an old generation, a
    /// duplicate) still gets `ok:true`, with `accepted:false`.
    pub accepted: bool,
    /// For `hook.session_start`: context the hook prints as
    /// `hookSpecificOutput.additionalContext`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

/// `task.done` params (`c9watch-agent task done --report`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskDoneParams {
    /// `C9_AGENT_ID` of the sender.
    pub agent_id: String,
    pub report: String,
    /// The sender's clock in ns, as a string.
    pub seq: String,
}

/// `task.done` result. Free agents have no task, so the call fails with
/// `no_task` instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskDoneResult {
    pub task_id: String,
}

/// `agent.handoff` params (`c9watch-agent handoff --note -`). The wire
/// format is reserved; the engine currently answers `unsupported`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HandoffParams {
    /// `C9_AGENT_ID` of the sender.
    pub agent_id: String,
    /// What the next generation should know.
    pub note: String,
    /// The sender's clock in ns, as a string.
    pub seq: String,
}

/// `agent.handoff` result (reserved).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HandoffResult {
    pub accepted: bool,
}

/// Mailbox query. Reads never mark a message read by the model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MsgListParams {
    pub agent: Option<String>,
    pub thread: Option<String>,
    pub before: Option<String>,
    pub limit: Option<usize>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MsgIdParams {
    pub msg_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct MailAgentParams {
    pub agent: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MsgFrom {
    pub kind: String,
    pub addr: Option<String>,
    pub name: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MsgSummary {
    pub msg_id: String,
    pub from: MsgFrom,
    pub to: MsgAddress,
    pub kind: String,
    pub state: String,
    pub state_at: i64,
    pub detail: Option<String>,
    pub via: Option<String>,
    pub reply_to: Option<String>,
    pub thread_id: Option<String>,
    pub notice_id: Option<String>,
    pub hop: i64,
    pub created_at: i64,
    pub preview: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct MsgListResult {
    pub items: Vec<MsgSummary>,
    pub next: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct MsgStatus {
    pub state: String,
    pub at: i64,
    pub detail: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct MsgReadResult {
    #[serde(flatten)]
    pub summary: MsgSummary,
    pub body: String,
    pub provenance: String,
    pub attempts: i64,
    pub status: Vec<MsgStatus>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct MailControlResult {
    pub ok: bool,
}

// L1 annotation types are shared with clients outside the engine feature.
pub use super::annotations::*;
