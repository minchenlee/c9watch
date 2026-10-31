use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

use super::codex::CodexRolloutSummary;
use super::cursor::CursorTranscriptSummary;
use super::pi::PiTranscriptSummary;

#[derive(Error, Debug)]
pub enum SessionDetectorError {
    #[error("Failed to read directory: {0}")]
    DirectoryRead(#[from] std::io::Error),
    #[error("Failed to get home directory")]
    HomeDirectoryNotFound,
    #[error("Failed to refresh process information")]
    ProcessRefreshError,
    #[error("CLI subprocess failed: {0}")]
    CliFailed(String),
    #[error("CLI subprocess timed out after {0}ms")]
    Timeout(u128),
    #[error("Failed to parse CLI output: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    Interactive,
    Background,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CliActivity {
    Busy,
    Idle,
    /// Claude Code is blocked on the user: a permission prompt, dialog,
    /// elicitation or other input request.
    Waiting,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum SessionProvider {
    #[default]
    ClaudeCode,
    Codex,
    Cursor,
    Pi,
    Opencode,
}

impl SessionProvider {
    /// Every provider, in declaration order.
    pub(crate) const ALL: [SessionProvider; 5] = [
        Self::ClaudeCode,
        Self::Codex,
        Self::Cursor,
        Self::Pi,
        Self::Opencode,
    ];

    /// Wire key, identical to the serde encoding (`claudeCode`, `codex`, ...).
    pub(crate) fn as_key(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claudeCode",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Pi => "pi",
            Self::Opencode => "opencode",
        }
    }

    /// Inverse of [`SessionProvider::as_key`]. Unknown keys return `None`.
    pub(crate) fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|provider| provider.as_key() == key)
    }

    /// Human-readable product name for user-facing text.
    // Only notifications.rs (GUI build) calls this.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Cursor => "Cursor",
            Self::Pi => "Pi",
            Self::Opencode => "OpenCode",
        }
    }
}

/// Health of the source observation used to produce a session card.
///
/// This is deliberately separate from `SessionStatus`: a session can be
/// waiting for input while its source is fresh, partial, or unavailable.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum SourceHealth {
    Fresh,
    Stale,
    Partial,
    Unavailable,
    Unknown,
}

/// Classify a successfully read source by the age of its last event.
///
/// `stale_after_secs` should be the provider's own retention/freshness
/// window for the current lifecycle: an observation older than the window in
/// which the detector still surfaces the session may no longer reflect
/// reality, so it is `Stale` rather than `Fresh`.
pub fn health_from_last_timestamp(last_timestamp: &str, stale_after_secs: u64) -> SourceHealth {
    if last_timestamp.is_empty() {
        return SourceHealth::Partial;
    }
    let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(last_timestamp) else {
        return SourceHealth::Partial;
    };
    let age_secs = chrono::Utc::now()
        .signed_duration_since(timestamp.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0) as u64;
    if age_secs > stale_after_secs {
        SourceHealth::Stale
    } else {
        SourceHealth::Fresh
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum SessionSurface {
    #[default]
    ClaudeCode,
    App,
    Cli,
    Exec,
    Integration,
    Cursor,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum AgentKind {
    #[default]
    Root,
    Subagent,
    Internal,
}

impl SessionSurface {
    /// Wire name, identical to the serde encoding.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claudeCode",
            Self::App => "app",
            Self::Cli => "cli",
            Self::Exec => "exec",
            Self::Integration => "integration",
            Self::Cursor => "cursor",
            Self::Unknown => "unknown",
        }
    }
}

impl AgentKind {
    /// Wire name, identical to the serde encoding.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Subagent => "subagent",
            Self::Internal => "internal",
        }
    }
}

/// Stable identity for a detected session.
///
/// A provider owns the namespace of its session IDs. Keeping the provider in
/// the key prevents a Claude, Codex, and Cursor session with the same opaque
/// ID from sharing deduplication, status, or overlay state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIdentity {
    pub provider: SessionProvider,
    pub session_id: String,
}

impl SessionIdentity {
    pub fn new(provider: SessionProvider, session_id: impl Into<String>) -> Self {
        Self {
            provider,
            session_id: session_id.into(),
        }
    }

    /// Opaque UI/storage key. The provider prefix is explicit so callers do
    /// not have to infer a namespace from a raw session ID.
    pub fn key(&self) -> String {
        format!("{}:{}", self.provider.as_key(), self.session_id)
    }
}

#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DetectionDiagnostics {
    pub claude_processes_found: u32,
    pub processes_with_cwd: u32,
    pub fda_likely_needed: bool,
    /// Live Claude Code sessions that Claude Code itself keeps out of its
    /// registry, so c9watch can count them but not show them.
    #[serde(default)]
    pub hidden_claude_sessions: Vec<super::hidden_sessions::HiddenClaudeSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedSession {
    pub pid: u32,
    pub cwd: PathBuf,
    pub project_path: PathBuf,
    pub session_id: Option<String>,
    pub project_name: String,
    pub kind: SessionKind,
    /// Raw `entrypoint` from `~/.claude/sessions/<pid>.json` (e.g. "cli", "sdk-cli",
    /// "claude-vscode", "mcp", "remote_desktop", ...). Only populated by the CLI
    /// backend; other providers/backends leave this `None`.
    #[serde(default)]
    pub entrypoint: Option<String>,
    pub started_at_ms: Option<i64>,
    pub official_name: Option<String>,
    pub cli_activity: Option<CliActivity>,
    /// Whether this Claude Code version reports every user-blocking prompt as
    /// `CliActivity::Waiting`, so `Busy`/`Idle` also mean no prompt is open.
    #[serde(default)]
    pub cli_reports_prompts: bool,
    #[serde(default)]
    pub provider: SessionProvider,
    #[serde(default)]
    pub surface: SessionSurface,
    #[serde(default)]
    pub agent_kind: AgentKind,
    #[serde(default)]
    pub parent_thread_id: Option<String>,
    #[serde(default)]
    pub root_session_id: Option<String>,
    #[serde(default)]
    pub agent_path: Option<String>,
    #[serde(default)]
    pub agent_nickname: Option<String>,
    #[serde(default)]
    pub agent_role: Option<String>,
    #[serde(default)]
    pub internal_kind: Option<String>,
    #[serde(default = "default_true")]
    pub can_open: bool,
    #[serde(default = "default_true")]
    pub can_stop: bool,
    #[serde(default = "default_true")]
    pub can_rename: bool,
    #[serde(skip)]
    pub codex_summary: Option<CodexRolloutSummary>,
    #[serde(skip)]
    pub cursor_summary: Option<CursorTranscriptSummary>,
    #[serde(skip)]
    pub pi_summary: Option<PiTranscriptSummary>,
    #[serde(skip)]
    pub opencode_summary: Option<super::opencode::OpenCodeSummary>,
    /// Start time (seconds since the epoch) of `pid` when it was detected.
    /// Stop re-reads it before signalling, so a pid the OS has since handed
    /// to another process is never killed. `None` when the backend has no
    /// process behind the session.
    #[serde(skip)]
    pub process_start_secs: Option<u64>,
}

fn default_true() -> bool {
    true
}

impl DetectedSession {
    pub fn identity(&self) -> Option<SessionIdentity> {
        self.session_id
            .as_ref()
            .map(|id| SessionIdentity::new(self.provider, id.clone()))
    }

    pub fn identity_key(&self) -> Option<String> {
        self.identity().map(|identity| identity.key())
    }

    /// Legacy backend doesn't fill the new (Phase 2) fields. Helper enforces the defaults.
    pub fn with_legacy_defaults(
        pid: u32,
        cwd: PathBuf,
        project_path: PathBuf,
        session_id: Option<String>,
        project_name: String,
    ) -> Self {
        Self {
            pid,
            cwd,
            project_path,
            session_id,
            project_name,
            kind: SessionKind::Interactive,
            entrypoint: None,
            started_at_ms: None,
            official_name: None,
            cli_activity: None,
            cli_reports_prompts: false,
            provider: SessionProvider::ClaudeCode,
            surface: SessionSurface::ClaudeCode,
            agent_kind: AgentKind::Root,
            parent_thread_id: None,
            root_session_id: None,
            agent_path: None,
            agent_nickname: None,
            agent_role: None,
            internal_kind: None,
            can_open: true,
            can_stop: true,
            can_rename: true,
            codex_summary: None,
            cursor_summary: None,
            pi_summary: None,
            opencode_summary: None,
            process_start_secs: None,
        }
    }

    /// Session found from files or an HTTP snapshot rather than a process c9watch
    /// controls: pid 0, `project_path` equal to `cwd`, and no open, stop or
    /// rename. Callers set provider-specific fields with struct-update syntax.
    pub fn read_only(
        provider: SessionProvider,
        surface: SessionSurface,
        cwd: PathBuf,
        session_id: String,
        project_name: String,
    ) -> Self {
        Self {
            provider,
            surface,
            can_open: false,
            can_stop: false,
            can_rename: false,
            ..Self::with_legacy_defaults(0, cwd.clone(), cwd, Some(session_id), project_name)
        }
    }
}

pub trait SessionSource: Send {
    fn detect(
        &mut self,
    ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError>;
    fn backend_name(&self) -> &'static str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_and_agent_kind_as_str_match_serde() {
        for surface in [
            SessionSurface::ClaudeCode,
            SessionSurface::App,
            SessionSurface::Cli,
            SessionSurface::Exec,
            SessionSurface::Integration,
            SessionSurface::Cursor,
            SessionSurface::Unknown,
        ] {
            assert_eq!(
                serde_json::to_value(surface).unwrap(),
                serde_json::Value::from(surface.as_str())
            );
        }
        for kind in [AgentKind::Root, AgentKind::Subagent, AgentKind::Internal] {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::Value::from(kind.as_str())
            );
        }
    }

    #[test]
    fn provider_keys_match_serde_and_round_trip() {
        for provider in SessionProvider::ALL {
            assert_eq!(
                serde_json::to_value(provider).unwrap(),
                serde_json::Value::from(provider.as_key())
            );
            assert_eq!(SessionProvider::from_key(provider.as_key()), Some(provider));
        }
        assert_eq!(SessionProvider::from_key("bogus"), None);
        assert_eq!(SessionProvider::from_key(""), None);
        assert_eq!(SessionProvider::from_key("ClaudeCode"), None);
    }

    /// `ALL` is written by hand. serde's derived error lists every variant
    /// in declaration order, so a variant missing from `ALL` fails here.
    #[test]
    fn provider_all_lists_every_serde_variant() {
        let err = serde_json::from_str::<SessionProvider>("\"__not_a_provider__\"")
            .unwrap_err()
            .to_string();
        let expected = SessionProvider::ALL
            .iter()
            .map(|p| format!("`{}`", p.as_key()))
            .collect::<Vec<_>>()
            .join(", ");
        assert!(
            err.contains(&format!("expected one of {expected} ")),
            "SessionProvider::ALL is out of sync with the enum: {err}"
        );
    }

    #[test]
    fn provider_labels_are_product_names() {
        let labels: Vec<_> = SessionProvider::ALL.iter().map(|p| p.label()).collect();
        assert_eq!(labels, ["Claude Code", "Codex", "Cursor", "Pi", "OpenCode"]);
    }

    #[test]
    fn identity_key_uses_provider_key_prefix() {
        assert_eq!(
            SessionIdentity::new(SessionProvider::ClaudeCode, "abc").key(),
            "claudeCode:abc"
        );
        assert_eq!(
            SessionIdentity::new(SessionProvider::Opencode, "ses_1").key(),
            "opencode:ses_1"
        );
    }

    #[test]
    fn session_kind_serializes_as_lowercase() {
        assert_eq!(
            serde_json::to_string(&SessionKind::Interactive).unwrap(),
            "\"interactive\""
        );
        assert_eq!(
            serde_json::to_string(&SessionKind::Background).unwrap(),
            "\"background\""
        );
        assert_eq!(
            serde_json::to_string(&SessionKind::Unknown).unwrap(),
            "\"unknown\""
        );
    }

    #[test]
    fn cli_activity_serializes_as_lowercase() {
        assert_eq!(
            serde_json::to_string(&CliActivity::Busy).unwrap(),
            "\"busy\""
        );
        assert_eq!(
            serde_json::to_string(&CliActivity::Idle).unwrap(),
            "\"idle\""
        );
    }

    #[test]
    fn health_from_last_timestamp_classifies_age() {
        assert_eq!(
            super::health_from_last_timestamp("", 60),
            super::SourceHealth::Partial
        );
        assert_eq!(
            super::health_from_last_timestamp("not-a-timestamp", 60),
            super::SourceHealth::Partial
        );
        let now = chrono::Utc::now().to_rfc3339();
        assert_eq!(
            super::health_from_last_timestamp(&now, 3600),
            super::SourceHealth::Fresh
        );
        // Comfortable margins on both sides of the threshold so the wall clock
        // cannot flip the assertion mid-test.
        let old = (chrono::Utc::now() - chrono::Duration::hours(3)).to_rfc3339();
        assert_eq!(
            super::health_from_last_timestamp(&old, 3600),
            super::SourceHealth::Stale
        );
        assert_eq!(
            super::health_from_last_timestamp(&old, 4 * 3600),
            super::SourceHealth::Fresh
        );
    }

    #[test]
    fn source_health_serializes_as_camel_case() {
        assert_eq!(
            serde_json::to_string(&SourceHealth::Fresh).unwrap(),
            "\"fresh\""
        );
        assert_eq!(
            serde_json::to_string(&SourceHealth::Unavailable).unwrap(),
            "\"unavailable\""
        );
    }

    #[test]
    fn detected_session_defaults_for_legacy_backend_have_kind_interactive() {
        let d = DetectedSession::with_legacy_defaults(
            42,
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/proj"),
            Some("uuid".to_string()),
            "tmp".to_string(),
        );
        assert!(matches!(d.kind, SessionKind::Interactive));
        assert_eq!(d.started_at_ms, None);
        assert!(d.official_name.is_none());
        assert!(d.cli_activity.is_none());
        assert_eq!(d.provider, SessionProvider::ClaudeCode);
        assert_eq!(d.surface, SessionSurface::ClaudeCode);
        assert_eq!(d.agent_kind, AgentKind::Root);
        assert!(d.can_open && d.can_stop && d.can_rename);
        assert_eq!(d.process_start_secs, None);
    }

    #[test]
    fn process_start_secs_is_not_serialized() {
        let mut d = DetectedSession::with_legacy_defaults(
            42,
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/proj"),
            Some("uuid".to_string()),
            "tmp".to_string(),
        );
        let before = serde_json::to_value(&d).unwrap();
        d.process_start_secs = Some(1_791_984_957);
        assert_eq!(serde_json::to_value(&d).unwrap(), before);
    }

    #[test]
    fn read_only_session_has_pid_zero_cwd_as_project_and_no_actions() {
        let d = DetectedSession::read_only(
            SessionProvider::Pi,
            SessionSurface::Cli,
            PathBuf::from("/tmp/proj"),
            "uuid".to_string(),
            "proj".to_string(),
        );
        let mut expected = DetectedSession::with_legacy_defaults(
            0,
            PathBuf::from("/tmp/proj"),
            PathBuf::from("/tmp/proj"),
            Some("uuid".to_string()),
            "proj".to_string(),
        );
        expected.provider = SessionProvider::Pi;
        expected.surface = SessionSurface::Cli;
        expected.can_open = false;
        expected.can_stop = false;
        expected.can_rename = false;
        assert_eq!(
            serde_json::to_value(&d).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert!(d.codex_summary.is_none() && d.cursor_summary.is_none());
        assert!(d.pi_summary.is_none() && d.opencode_summary.is_none());
        assert_eq!(d.process_start_secs, None);
    }

    #[test]
    fn provider_surface_and_agent_kind_use_frontend_contract_values() {
        assert_eq!(
            serde_json::to_string(&SessionProvider::ClaudeCode).unwrap(),
            "\"claudeCode\""
        );
        assert_eq!(
            serde_json::to_string(&SessionProvider::Codex).unwrap(),
            "\"codex\""
        );
        assert_eq!(
            serde_json::to_string(&SessionProvider::Cursor).unwrap(),
            "\"cursor\""
        );
        assert_eq!(
            serde_json::to_string(&SessionProvider::Pi).unwrap(),
            "\"pi\""
        );
        assert_eq!(
            serde_json::to_string(&SessionSurface::Cursor).unwrap(),
            "\"cursor\""
        );
        assert_eq!(
            serde_json::to_string(&SessionSurface::Integration).unwrap(),
            "\"integration\""
        );
        assert_eq!(
            serde_json::to_string(&AgentKind::Subagent).unwrap(),
            "\"subagent\""
        );
    }

    #[test]
    fn session_identity_namespaces_provider_ids() {
        let id = "same-session-id";
        let claude = SessionIdentity::new(SessionProvider::ClaudeCode, id);
        let codex = SessionIdentity::new(SessionProvider::Codex, id);
        let cursor = SessionIdentity::new(SessionProvider::Cursor, id);
        let pi = SessionIdentity::new(SessionProvider::Pi, id);

        assert_ne!(claude, codex);
        assert_ne!(codex, cursor);
        assert_ne!(cursor, pi);
        assert_eq!(claude.key(), "claudeCode:same-session-id");
        assert_eq!(codex.key(), "codex:same-session-id");
        assert_eq!(cursor.key(), "cursor:same-session-id");
        assert_eq!(pi.key(), "pi:same-session-id");
    }
}
