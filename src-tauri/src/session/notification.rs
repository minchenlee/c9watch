//! The text of a status-change notification: what the GUI poll shows and
//! broadcasts to web clients, and what the engine puts on `notification.*`
//! events. Preferences (detail, sound) do not change it; the macOS native
//! notification builds its own text in `notifications.rs`.

use crate::session::enrichment::truncate_string;
use crate::session::{is_question_tool, project_basename, Session, SessionStatus};

/// Notification text for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationText {
    /// The session's custom or provider title, else its first prompt
    /// (conversation text), at most 60 chars.
    pub title: String,
    /// `title` without conversation text: the project name (the last
    /// component of the project path), else the provider's name. Remote
    /// clients that may not see conversation text get this.
    pub redacted_title: String,
    pub body: String,
}

/// The text for a session that needs attention or waits for input; `None`
/// for any other status.
pub fn notification_text(session: &Session) -> Option<NotificationText> {
    let custom_title = session
        .custom_title
        .as_deref()
        .or(session.codex_title.as_deref())
        .or(session.cursor_title.as_deref())
        .or(session.ai_title.as_deref());
    let session_name = &session.session_name;
    let pending_tool_name = session.pending_tool_name.as_deref();
    let body = match session.status {
        SessionStatus::NeedsAttention => {
            if is_question_tool(pending_tool_name) {
                format!("❓ {}: Waiting for your response", session_name)
            } else {
                let tool_name = pending_tool_name.unwrap_or("unknown tool");
                format!("🔐 {}: Needs permission for {}", session_name, tool_name)
            }
        }
        SessionStatus::WaitingForInput => {
            format!("✅ {}: Finished working", session_name)
        }
        _ => return None,
    };
    Some(NotificationText {
        title: truncate_string(custom_title.unwrap_or(&session.first_prompt), 60),
        redacted_title: truncate_string(redacted_title(session), 60),
        body,
    })
}

/// A title with no conversation text: the project folder name, else the
/// provider's name. Never a session name or title (both can be the first
/// prompt).
fn redacted_title(session: &Session) -> &str {
    project_basename(&session.project_path).unwrap_or(session.provider.label())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionProvider;

    #[test]
    fn titles_and_bodies_by_status() {
        let mut s = Session::fixture("a", SessionStatus::WaitingForInput);
        let t = notification_text(&s).unwrap();
        assert_eq!(t.title, "investigate");
        assert_eq!(t.redacted_title, "project");
        assert_eq!(t.body, "✅ name-a: Finished working");

        s.status = SessionStatus::NeedsAttention;
        s.pending_tool_name = Some("Bash".into());
        s.ai_title = Some("Fix the build".into());
        let t = notification_text(&s).unwrap();
        assert_eq!(t.title, "Fix the build");
        assert_eq!(t.redacted_title, "project");
        assert_eq!(t.body, "🔐 name-a: Needs permission for Bash");

        s.pending_tool_name = Some("AskUserQuestion".into());
        assert_eq!(
            notification_text(&s).unwrap().body,
            "❓ name-a: Waiting for your response"
        );

        s.status = SessionStatus::Working;
        assert_eq!(notification_text(&s), None);
    }

    #[test]
    fn redacted_title_is_the_project_name_never_a_title() {
        let mut s = Session::fixture("a", SessionStatus::WaitingForInput);
        s.project_path = "/Users/me/c9watch".into();
        s.session_name = "secret session name".into();
        s.first_prompt = "secret prompt".into();
        s.custom_title = Some("secret custom".into());
        s.codex_title = Some("secret codex".into());
        s.cursor_title = Some("secret cursor".into());
        s.ai_title = Some("secret ai".into());
        let t = notification_text(&s).unwrap();
        assert_eq!(t.title, "secret custom");
        assert_eq!(t.redacted_title, "c9watch");
        assert!(!t.redacted_title.contains("secret"));

        s.custom_title = None;
        s.codex_title = None;
        s.cursor_title = None;
        s.ai_title = None;
        assert_eq!(notification_text(&s).unwrap().redacted_title, "c9watch");
    }

    #[test]
    fn redacted_title_falls_back_to_the_provider_name() {
        let mut s = Session::fixture("a", SessionStatus::WaitingForInput);
        s.ai_title = Some("secret ai".into());
        for path in ["", "/"] {
            s.project_path = path.into();
            s.provider = SessionProvider::ClaudeCode;
            assert_eq!(notification_text(&s).unwrap().redacted_title, "Claude Code");
            s.provider = SessionProvider::Codex;
            assert_eq!(notification_text(&s).unwrap().redacted_title, "Codex");
        }
    }
}
