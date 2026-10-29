//! c9p/1 wire types.
//!
//! These serde types are the single source of truth for the protocol
//! (A:245): one JSON object per line, a request/response envelope, typed
//! error codes with a hint, and the `hello` handshake. Within major 1 every
//! change is additive, so unknown fields are ignored on input.
//!
//! They live outside the `engine` feature, so the GUI can read the engine
//! without being built with it. The JSON Schema derive exists only in
//! engine builds, which generate `docs/api/c9p-1.json`.

use serde::{Deserialize, Serialize};

/// Protocol major version. It is also in the socket name (`engine-v1.sock`).
pub const API_MAJOR: u32 = 1;
/// Protocol minor version. Raised for additive changes.
pub const API_MINOR: u32 = 2;
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
    /// `c9watch-agent` copy, or that names another agent.
    PeerNotAuthorized,
    /// `task.done` from an agent without a task (every free agent).
    NoTask,
    /// The method is part of the wire format but does nothing before C1b
    /// (`agent.handoff`).
    UnsupportedInC1a,
    /// The engine has no `c9watch-agent` copy: none was next to the engine
    /// binary when it started, or the copy is gone.
    AgentBinMissing,
    /// The engine's `c9watch-agent` copy no longer has the sha256 it had
    /// when the engine installed it.
    AgentBinMismatch,
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
            ErrorCode::UnsupportedInC1a => "unsupported_in_c1a",
            ErrorCode::AgentBinMissing => "agent_bin_missing",
            ErrorCode::AgentBinMismatch => "agent_bin_mismatch",
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
            ErrorCode::RateLimited => {
                "use fewer event streams per connection: one subscription with \
                 topic and subject filters, or another connection"
            }
            ErrorCode::PeerNotAuthorized => {
                "only an engine agent may call this, through `c9watch-agent` from the \
                 engine's own copy"
            }
            ErrorCode::NoTask => "free agents have no task; just end the turn",
            ErrorCode::UnsupportedInC1a => "this engine does not support it yet",
            ErrorCode::AgentBinMissing => {
                "install c9watch-agent next to the c9watch binary, then \
                 `c9watch engine restart`"
            }
            ErrorCode::AgentBinMismatch => {
                "`c9watch engine restart` copies c9watch-agent again; check what changed \
                 the engine's copy"
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
}

/// One row of the `agents` table (A:340), as `snapshot.get`,
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
pub struct SessionListResult {
    pub sessions: Vec<AgentRow>,
}

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

/// `events.subscribe` params (A:218). Empty filters match everything.
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

/// One event on the wire (A:203). `sub` is the `id` of the subscribe
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

/// The hook-class methods (§1.1). Only a live agent may call them, and only
/// through the engine's own copy of `c9watch-agent` (C1a-21).
pub const HOOK_METHODS: &[&str] = &[
    "hook.session_start",
    "hook.stop",
    "hook.permission_request",
    "hook.permission_denied",
    "hook.notification",
];

/// Whether `method` is hook-class: a `hook.*` method, `task.done` or
/// `agent.handoff`.
pub fn is_hook_class(method: &str) -> bool {
    method.starts_with("hook.") || method == "task.done" || method == "agent.handoff"
}

/// `hook.*` params: one Claude Code hook event, sent by `c9watch-agent
/// hook <event>`. The engine alone writes the log (A:1167).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HookParams {
    /// `C9_AGENT_ID` of the sender. It must be the agent the engine stamped.
    pub agent_id: String,
    /// The adapter kind of the hook (`claude`). The engine compares it with
    /// the agent row (A:1056).
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

/// `task.done` result. In C1a every agent is free, so the call fails with
/// `no_task` instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskDoneResult {
    pub task_id: String,
}

/// `agent.handoff` params (`c9watch-agent handoff --note -`). The wire
/// format is fixed now; the engine answers `unsupported_in_c1a` until C1b.
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

/// `agent.handoff` result (C1b).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HandoffResult {
    pub accepted: bool,
}
