//! The `SessionView` wire type: one session as `session.views` and the
//! `session.view` event carry it.

#[cfg(test)]
mod tests {
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
        s.waiting_for = Some(WaitingFor::Approval { tool: "Bash".into() });
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
        serde_json::to_value(s).unwrap()
    }

    /// The `SessionView` bytes of `full()` and of a bare fixture equal
    /// `testdata/session_view.golden.json`, recorded from the wire before
    /// the DTO split. With `C9P_GOLDEN_WRITE=1` the test rewrites the file.
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
}
