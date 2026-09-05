use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

use super::codex::CodexRolloutSummary;
use super::cursor::CursorTranscriptSummary;

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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum SessionProvider {
    #[default]
    ClaudeCode,
    Codex,
    Cursor,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq, Hash)]
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
#[serde(rename_all = "camelCase")]
pub enum AgentKind {
    #[default]
    Root,
    Subagent,
    Internal,
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
        format!("{}:{}", provider_key(self.provider), self.session_id)
    }
}

fn provider_key(provider: SessionProvider) -> &'static str {
    match provider {
        SessionProvider::ClaudeCode => "claudeCode",
        SessionProvider::Codex => "codex",
        SessionProvider::Cursor => "cursor",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DetectionDiagnostics {
    pub claude_processes_found: u32,
    pub processes_with_cwd: u32,
    pub fda_likely_needed: bool,
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
    pub started_at_ms: Option<i64>,
    pub official_name: Option<String>,
    pub cli_activity: Option<CliActivity>,
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
            started_at_ms: None,
            official_name: None,
            cli_activity: None,
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

        assert_ne!(claude, codex);
        assert_ne!(codex, cursor);
        assert_eq!(claude.key(), "claudeCode:same-session-id");
        assert_eq!(codex.key(), "codex:same-session-id");
        assert_eq!(cursor.key(), "cursor:same-session-id");
    }
}
