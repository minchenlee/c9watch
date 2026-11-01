//! The `SessionView` wire type: one session as `session.views` and the
//! `session.view` event carry it.
//!
//! The engine builds a `SessionView` from the internal `Session` at the API
//! boundary, so the internal struct can change without changing the c9p
//! contract. The schema (`docs/api/c9p-1.json`) is generated from this type,
//! not from `Session`. A field added to `Session` reaches the wire only when
//! it is added here too.

use crate::actions::OpenTarget;
use crate::session::away::WaitingFor;
use crate::session::source::SourceHealth;
use crate::session::{
    AgentKind, Session, SessionKind, SessionProvider, SessionStatus, SessionSurface,
};
use serde::Serialize;

/// Combined session information
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: String,
    /// Provider-scoped identity used by frontend selection and backend maps.
    pub session_key: String,
    pub pid: u32,
    pub session_name: String,
    pub custom_title: Option<String>,
    /// Codex's current UI thread name from ~/.codex/session_index.jsonl.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_title: Option<String>,
    /// Cursor's current UI composer name from state.vscdb.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_title: Option<String>,
    /// Claude Code's auto-generated title (`ai-title` in the session JSONL).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_title: Option<String>,
    pub project_path: String,
    pub git_branch: Option<String>,
    pub first_prompt: String,
    pub summary: Option<String>,
    pub message_count: u32,
    pub modified: String,
    pub status: SessionStatus,
    /// Health of the source observation, independent of the session status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_health: Option<SourceHealth>,
    pub latest_message: String,
    pub pending_tool_name: Option<String>,
    /// The input/arguments of the pending tool (when status is NeedsPermission)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_tool_input: Option<serde_json::Value>,
    /// What the session needs from the user (tool name only, never arguments).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<WaitingFor>,
    /// One-line, tag-free preview of the last assistant text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
    /// Session ID of the PM that spawned this session (if it's a c9watch worker).
    /// Set by the workers overlay from `~/.claude/c9watch/workers/*/meta.json`
    /// (`session::workers`), after detection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_of: Option<String>,
    /// User-set name from `claude agents` (Ctrl+T background-pinned sessions).
    /// Only populated by the CLI backend; legacy backend leaves this None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub official_name: Option<String>,
    /// Session start time in ms since epoch (from `claude agents --json`).
    /// Only populated by the CLI backend; legacy backend leaves this None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    pub provider: SessionProvider,
    pub surface: SessionSurface,
    /// Interactive vs. background-pinned (`claude agents --json`'s `kind`).
    /// Providers other than Claude Code always report `Interactive`.
    pub kind: SessionKind,
    /// Raw `entrypoint` from `~/.claude/sessions/<pid>.json` (e.g. "cli", "sdk-cli",
    /// "claude-vscode", "mcp", "remote_desktop"...). Only the CLI backend populates
    /// this; `None` covers the legacy backend and non-Claude-Code providers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    pub agent_kind: AgentKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub internal_kind: Option<String>,
    /// Native target guarantee; absent means no verified exact native target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_target: Option<OpenTarget>,
    pub can_open: bool,
    pub can_stop: bool,
    pub can_rename: bool,
}

impl From<&Session> for SessionView {
    fn from(s: &Session) -> Self {
        SessionView {
            id: s.id.clone(),
            session_key: s.session_key.clone(),
            pid: s.pid,
            session_name: s.session_name.clone(),
            custom_title: s.custom_title.clone(),
            codex_title: s.codex_title.clone(),
            cursor_title: s.cursor_title.clone(),
            ai_title: s.ai_title.clone(),
            project_path: s.project_path.clone(),
            git_branch: s.git_branch.clone(),
            first_prompt: s.first_prompt.clone(),
            summary: s.summary.clone(),
            message_count: s.message_count,
            modified: s.modified.clone(),
            status: s.status.clone(),
            source_health: s.source_health,
            latest_message: s.latest_message.clone(),
            pending_tool_name: s.pending_tool_name.clone(),
            pending_tool_input: s.pending_tool_input.clone(),
            waiting_for: s.waiting_for.clone(),
            last_assistant_message: s.last_assistant_message.clone(),
            worker_of: s.worker_of.clone(),
            official_name: s.official_name.clone(),
            started_at_ms: s.started_at_ms,
            provider: s.provider,
            surface: s.surface,
            kind: s.kind,
            entrypoint: s.entrypoint.clone(),
            agent_kind: s.agent_kind,
            parent_thread_id: s.parent_thread_id.clone(),
            root_session_id: s.root_session_id.clone(),
            agent_path: s.agent_path.clone(),
            agent_nickname: s.agent_nickname.clone(),
            agent_role: s.agent_role.clone(),
            internal_kind: s.internal_kind.clone(),
            open_target: s.open_target,
            can_open: s.can_open,
            can_stop: s.can_stop,
            can_rename: s.can_rename,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SessionView;
    use crate::session::away::WaitingFor;
    use crate::session::source::SourceHealth;
    use crate::session::{
        AgentKind, Session, SessionKind, SessionProvider, SessionStatus, SessionSurface,
    };

    /// A session with every field set, so that every wire field shows.
    pub(crate) fn full() -> Session {
        let mut s = Session::fixture("full", SessionStatus::NeedsAttention);
        s.custom_title = Some("custom".into());
        s.codex_title = Some("codex".into());
        s.cursor_title = Some("cursor".into());
        s.ai_title = Some("ai".into());
        s.git_branch = Some("main".into());
        s.summary = Some("summary".into());
        s.source_health = Some(SourceHealth::Stale);
        s.latest_message = "latest".into();
        s.notification_preview = Some("internal preview".into());
        s.pending_tool_name = Some("Bash".into());
        s.pending_tool_input = Some(serde_json::json!({"command": "make"}));
        s.waiting_for = Some(WaitingFor::Approval {
            tool: "Bash".into(),
        });
        s.last_assistant_message = Some("last".into());
        s.transcript_path = Some("/internal/transcript.jsonl".into());
        s.worker_of = Some("pm-1".into());
        s.official_name = Some("official".into());
        s.provider = SessionProvider::Codex;
        s.surface = SessionSurface::App;
        s.kind = SessionKind::Background;
        s.entrypoint = Some("cli".into());
        s.agent_kind = AgentKind::Subagent;
        s.parent_thread_id = Some("parent".into());
        s.root_session_id = Some("root".into());
        s.agent_path = Some("a/b".into());
        s.agent_nickname = Some("nick".into());
        s.agent_role = Some("role".into());
        s.internal_kind = Some("internal".into());
        s.open_target = Some(crate::actions::OpenTarget::Terminal);
        s.can_open = true;
        s.can_stop = true;
        s.can_rename = true;
        s
    }

    fn wire(s: &Session) -> serde_json::Value {
        serde_json::to_value(SessionView::from(s)).unwrap()
    }

    /// The `SessionView` bytes of `full()` and of a bare fixture equal
    /// `testdata/session_view.golden.json`, recorded from the wire before
    /// the DTO split (the serialized `Session`). With `C9P_GOLDEN_WRITE=1`
    /// the test rewrites the file.
    #[test]
    fn session_view_bytes_match_the_golden_file() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/engine_proto/testdata/session_view.golden.json");
        let bare = Session::fixture("bare", SessionStatus::Working);
        let want = serde_json::to_string_pretty(&serde_json::json!({
            "full": wire(&full()),
            "bare": wire(&bare),
        }))
        .unwrap()
            + "\n";
        if std::env::var_os("C9P_GOLDEN_WRITE").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, want).unwrap();
            return;
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(have, want, "SessionView wire bytes changed");
    }

    /// Fields of `Session` that never go on the wire. Each is set in
    /// `full()`, so a leak would show in its bytes.
    const INTERNAL_ONLY: &[&str] = &["notificationPreview", "transcriptPath"];

    #[test]
    fn internal_only_fields_are_not_on_the_wire() {
        let view = wire(&full());
        for name in INTERNAL_ONLY {
            assert!(view.get(*name).is_none(), "{name} leaked into SessionView");
        }
    }

    /// The internal `Session` has no JSON Schema, so the published schema
    /// cannot be generated from it. This fails to compile if `Session`
    /// derives `JsonSchema` again.
    #[cfg(feature = "engine")]
    #[allow(dead_code)]
    const _: fn() = || {
        trait NoSchema<A> {
            fn check() {}
        }
        impl<T: ?Sized> NoSchema<()> for T {}
        struct HasSchema;
        impl<T: ?Sized + schemars::JsonSchema> NoSchema<HasSchema> for T {}
        // Ambiguous, and so a compile error, when `Session: JsonSchema`.
        <Session as NoSchema<_>>::check();
    };

    /// The schema's `SessionView` is the DTO: its properties are exactly
    /// the fields a fully set view sends, no internal-only field appears
    /// anywhere in the schema, and there is no `Session` definition.
    #[cfg(feature = "engine")]
    #[test]
    fn schema_session_view_is_the_dto() {
        let schema = crate::engine::api::schema();
        let defs = schema["$defs"].as_object().unwrap();
        assert!(!defs.contains_key("Session"));
        let props: std::collections::BTreeSet<&str> = defs["SessionView"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let view = wire(&full());
        let sent: std::collections::BTreeSet<&str> = view
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(props, sent);
        let text = schema.to_string();
        for name in INTERNAL_ONLY {
            assert!(!text.contains(name), "{name} leaked into the schema");
        }
    }
}
