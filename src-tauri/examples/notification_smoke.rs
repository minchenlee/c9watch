//! Opt-in native macOS smoke test. Run only inside a separate QA bundle.
//! Emits exactly three notifications; disabled and duplicate attempts must stay silent.
use c9watch_lib::{
    notifications::{self, Preferences},
    session::{Session, SessionProvider, SessionStatus},
};
use std::time::Duration;
use tauri::Manager;
fn fixture() -> Session {
    Session {
        id: "child-thread".to_string(),
        session_key: "codex:child-thread".to_string(),
        pid: 0,
        session_name: "child".to_string(),
        custom_title: None,
        codex_title: Some("Codex thread title".to_string()),
        cursor_title: None,
        project_path: "/tmp/project".to_string(),
        git_branch: None,
        first_prompt: "investigate".to_string(),
        summary: None,
        message_count: 3,
        modified: "2026-07-13T00:00:00Z".to_string(),
        status: SessionStatus::Working,
        notification_preview: None,
        latest_message: String::new(),
        pending_tool_name: None,
        pending_tool_input: None,
        worker_of: None,
        official_name: None,
        started_at_ms: Some(1_752_364_800_000),
        provider: SessionProvider::Codex,
        surface: c9watch_lib::session::SessionSurface::App,
        agent_kind: c9watch_lib::session::AgentKind::Subagent,
        parent_thread_id: Some("parent-thread".to_string()),
        root_session_id: Some("root-thread".to_string()),
        agent_path: Some("/root/investigator".to_string()),
        agent_nickname: Some("Scout".to_string()),
        agent_role: Some("investigator".to_string()),
        internal_kind: Some("spawned".to_string()),
        can_open: false,
        can_stop: false,
        can_rename: false,
    }
}

fn main() {
    assert!(
        std::env::args().any(|arg| arg == "--native-smoke"),
        "Pass --native-smoke to explicitly send test notifications"
    );
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            notifications::initialize(app.handle()).map_err(std::io::Error::other)?;
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let run = || -> Result<(), String> {
                    let mut session = fixture();
                    session.session_key = "codex:notification-native-smoke".into();
                    session.status = SessionStatus::WaitingForInput;
                    session.notification_preview = Some("Native reply preview: the requested change is ready for review.".into());
                    session.custom_title = Some("Notification QA · MUST NOT APPEAR".into());
                    let mut prefs = Preferences { enabled: false, ..Preferences::default() };
                    notifications::save_notification_preferences(handle.state(), prefs.clone())?;
                    notifications::notify(&handle, &session);
                    prefs.enabled = true; prefs.reply_ready = false;
                    notifications::save_notification_preferences(handle.state(), prefs.clone())?;
                    notifications::notify(&handle, &session);
                    prefs.reply_ready = true;
                    notifications::save_notification_preferences(handle.state(), prefs.clone())?;
                    session.custom_title = Some("Notification QA · Reply".into());
                    notifications::notify(&handle, &session);
                    notifications::notify(&handle, &session); // same event: suppressed
                    std::thread::sleep(Duration::from_secs(7));
                    session.status = SessionStatus::NeedsAttention;
                    session.pending_tool_name = Some("AskUserQuestion".into());
                    session.pending_tool_input = Some(serde_json::json!({"questions":[{"question":"Which branch should I use?"}]}));
                    session.custom_title = Some("Notification QA · Question".into());
                    notifications::notify(&handle, &session); // distinct event: no reply cooldown
                    notifications::notify(&handle, &session);
                    std::thread::sleep(Duration::from_secs(7));
                    session.pending_tool_name = Some("Bash".into());
                    session.pending_tool_input = Some(serde_json::json!({"command":"cargo test --lib"}));
                    session.custom_title = Some("Notification QA · Permission".into());
                    notifications::notify(&handle, &session);
                    notifications::notify(&handle, &session);
                    prefs.questions = false; prefs.permissions = false;
                    notifications::save_notification_preferences(handle.state(), prefs)?;
                    session.session_key = "codex:notification-filter-smoke".into(); // no existing cooldown
                    session.custom_title = Some("Notification QA · MUST NOT APPEAR".into());
                    notifications::notify(&handle, &session);
                    session.pending_tool_name = Some("AskUserQuestion".into());
                    notifications::notify(&handle, &session);
                    notifications::save_notification_preferences(handle.state(), Preferences::default())?;
                    std::thread::sleep(Duration::from_secs(7));
                    Ok(())
                };
                match run() {
                    Ok(()) => { println!("SMOKE COMPLETE: expect exactly Reply, Question, Permission; no duplicates or MUST NOT APPEAR"); handle.exit(0); }
                    Err(error) => { eprintln!("SMOKE FAILED: {error}"); handle.exit(1); }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("native smoke app failed");
}
