//! macOS notification preferences and content. Web notifications remain independent.
use crate::session::{Session, SessionProvider, SessionStatus};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
#[cfg(not(target_os = "macos"))]
use tauri_plugin_notification::NotificationExt;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Preferences {
    pub enabled: bool,
    pub reply_ready: bool,
    pub questions: bool,
    pub permissions: bool,
    pub detail: Detail,
    pub sound: bool,
    pub cooldown_seconds: u64,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub enum Detail {
    Brief,
    #[default]
    Detailed,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: true,
            reply_ready: true,
            questions: true,
            permissions: true,
            detail: Detail::Detailed,
            sound: false,
            cooldown_seconds: 30,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Event {
    Reply,
    Question,
    Permission,
}
impl Event {
    fn from_session(s: &Session) -> Option<Self> {
        match s.status {
            SessionStatus::WaitingForInput => Some(Self::Reply),
            SessionStatus::NeedsAttention => Some(
                if matches!(
                    s.pending_tool_name.as_deref(),
                    Some("Question" | "AskUserQuestion")
                ) {
                    Self::Question
                } else {
                    Self::Permission
                },
            ),
            _ => None,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Reply => "Reply ready",
            Self::Question => "Your response needed",
            Self::Permission => "Permission needed",
        }
    }
    fn enabled(self, p: &Preferences) -> bool {
        p.enabled
            && match self {
                Self::Reply => p.reply_ready,
                Self::Question => p.questions,
                Self::Permission => p.permissions,
            }
    }
}
struct StateData {
    prefs: Preferences,
    sent: HashMap<(String, Event), Instant>,
}
pub struct NotificationState {
    path: PathBuf,
    data: Mutex<StateData>,
    #[cfg(target_os = "macos")]
    delivery: std::sync::mpsc::SyncSender<Delivery>,
}
#[cfg(target_os = "macos")]
struct Delivery {
    title: String,
    body: String,
    sound: bool,
}

/// Native delivery must not share the async executor used by transcript scans.
/// A bounded queue also keeps a burst of session events from growing without limit.
#[cfg(target_os = "macos")]
fn delivery_worker(identifier: String) -> Result<std::sync::mpsc::SyncSender<Delivery>, String> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Delivery>(64);
    std::thread::Builder::new()
        .name("native-notifications".into())
        .spawn(move || {
            // macOS permits setting the process application identifier only once.
            if let Err(error) = notify_rust::set_application(&identifier) {
                crate::debug_log::log_error(&format!(
                    "Native notification initialization failed: {error}"
                ));
                return;
            }
            for item in rx {
                let result = (|| -> Result<(), String> {
                    let mut notification = notify_rust::Notification::new();
                    notification.summary(&item.title).body(&item.body);
                    if item.sound {
                        notification.sound_name("Glass");
                    }
                    notification.show().map_err(|e| e.to_string())?;
                    Ok(())
                })();
                if let Err(error) = result {
                    crate::debug_log::log_error(&format!(
                        "Native notification delivery failed: {error}"
                    ));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(tx)
}
pub fn initialize(app: &AppHandle) -> Result<(), String> {
    let path = app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("notifications.json");
    let prefs = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<Preferences>(&bytes)
            .map_err(|e| {
                crate::debug_log::log_error(&format!("Invalid notification preferences: {e}"));
                e
            })
            .unwrap_or_default(),
        Err(_) => Preferences::default(),
    };
    app.manage(NotificationState {
        #[cfg(target_os = "macos")]
        delivery: delivery_worker(app.config().identifier.clone())?,
        path,
        data: Mutex::new(StateData {
            prefs,
            sent: HashMap::new(),
        }),
    });
    Ok(())
}
#[tauri::command]
pub fn get_notification_preferences(
    state: tauri::State<'_, NotificationState>,
) -> Result<Preferences, String> {
    Ok(state.data.lock().map_err(|e| e.to_string())?.prefs.clone())
}
#[tauri::command]
pub fn save_notification_preferences(
    state: tauri::State<'_, NotificationState>,
    preferences: Preferences,
) -> Result<Preferences, String> {
    if preferences.cooldown_seconds > 600 {
        return Err("Cooldown must be between 0 and 600 seconds".into());
    }
    let mut data = state.data.lock().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(state.path.parent().ok_or("Missing preferences directory")?)
        .map_err(|e| e.to_string())?;
    let tmp = state.path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&preferences).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(tmp, &state.path).map_err(|e| e.to_string())?;
    data.prefs = preferences.clone();
    Ok(preferences)
}
fn compact(s: &str, limit: usize) -> String {
    let clean = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut result: String = clean.chars().take(limit).collect();
    if clean.chars().count() > limit {
        result.push('…');
    }
    result
}
fn content(s: &Session, event: Event, detail: Detail) -> (String, String) {
    let name = s
        .custom_title
        .as_deref()
        .or(s.codex_title.as_deref())
        .or(s.cursor_title.as_deref())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(&s.first_prompt);
    let title = compact(
        if name.trim().is_empty() {
            &s.session_name
        } else {
            name
        },
        80,
    );
    let provider = match s.provider {
        SessionProvider::ClaudeCode => "Claude Code",
        SessionProvider::Codex => "Codex",
        SessionProvider::Cursor => "Cursor",
        SessionProvider::Pi => "Pi",
    };
    let project = std::path::Path::new(&s.project_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&s.session_name);
    let mut body = format!(
        "{} · {} · {}",
        event.label(),
        provider,
        compact(project, 40)
    );
    if detail == Detail::Detailed {
        let preview = match event {
            Event::Reply => s.notification_preview.clone().unwrap_or_default(),
            Event::Question => s
                .pending_tool_input
                .as_ref()
                .and_then(|v| v.get("questions"))
                .and_then(|v| v.as_array())
                .map(|qs| {
                    qs.iter()
                        .filter_map(|q| q.get("question")?.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| s.notification_preview.clone().unwrap_or_default()),
            Event::Permission => {
                let input = s
                    .pending_tool_input
                    .as_ref()
                    .and_then(|v| {
                        ["command", "file_path", "path", "description"]
                            .iter()
                            .find_map(|k| v.get(*k).and_then(|v| v.as_str()))
                    })
                    .unwrap_or("");
                format!(
                    "{}{}{}",
                    s.pending_tool_name.as_deref().unwrap_or("Tool approval"),
                    if input.is_empty() { "" } else { ": " },
                    input
                )
            }
        };
        if !preview.trim().is_empty() {
            body.push_str(&format!("\n{}", compact(&preview, 240)));
        }
    }
    (title, body)
}
fn show(app: &AppHandle, title: &str, body: &str, sound: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        app.state::<NotificationState>()
            .delivery
            .try_send(Delivery {
                title: title.into(),
                body: body.into(),
                sound,
            })
            .map_err(|e| format!("Native notification queue unavailable: {e}"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut notification = app.notification().builder().title(title).body(body);
        if sound {
            notification = notification.sound("Glass");
        }
        notification.show().map_err(|e| e.to_string())
    }
}
fn on_cooldown(
    sent: &HashMap<(String, Event), Instant>,
    key: &(String, Event),
    seconds: u64,
) -> bool {
    sent.get(key)
        .is_some_and(|t| t.elapsed() < Duration::from_secs(seconds))
}
pub fn notify(app: &AppHandle, session: &Session) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let Some(event) = Event::from_session(session) else {
        return;
    };
    let state = app.state::<NotificationState>();
    let Ok(mut data) = state.data.lock() else {
        return;
    };
    if !event.enabled(&data.prefs) {
        return;
    }
    data.sent
        .retain(|_, time| time.elapsed() < Duration::from_secs(600));
    let key = (session.session_key.clone(), event);
    if on_cooldown(&data.sent, &key, data.prefs.cooldown_seconds) {
        return;
    }
    let (title, body) = content(session, event, data.prefs.detail);
    match show(app, &title, &body, data.prefs.sound) {
        Ok(()) => {
            data.sent.insert(key, Instant::now());
        }
        Err(e) => crate::debug_log::log_error(&format!("Notification failed: {e}")),
    }
}
#[tauri::command]
pub fn test_native_notification(
    app: AppHandle,
    state: tauri::State<'_, NotificationState>,
) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("Available on macOS only".into());
    }
    let data = state.data.lock().map_err(|e| e.to_string())?;
    let body = if data.prefs.detail == Detail::Detailed {
        "Reply ready · Codex · c9watch\nNotification preview is ready. Your selected sound and detail settings are applied."
    } else {
        "Reply ready · Codex · c9watch"
    };
    show(&app, "c9watch · Test notification", body, data.prefs.sound)
}
#[cfg(test)]
mod tests {
    use super::*;
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
            surface: crate::session::SessionSurface::App,
            agent_kind: crate::session::AgentKind::Subagent,
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

    #[test]
    fn detailed_reply_and_brief_privacy() {
        let mut s = fixture();
        s.status = SessionStatus::WaitingForInput;
        s.notification_preview = Some("Tests passed. Ready for review.".into());
        let (title, body) = content(&s, Event::Reply, Detail::Detailed);
        assert_eq!(title, "Codex thread title");
        assert!(body.contains("Codex · project"));
        assert!(body.contains("Tests passed"));
        assert!(!content(&s, Event::Reply, Detail::Brief)
            .1
            .contains("Tests passed"));
        assert!(!body.contains("Finished working"));
    }
    #[test]
    fn questions_and_permissions_include_actionable_content() {
        let mut s = fixture();
        s.status = SessionStatus::NeedsAttention;
        s.pending_tool_name = Some("AskUserQuestion".into());
        s.pending_tool_input =
            Some(serde_json::json!({"questions":[{"question":"Which branch should I use?"}]}));
        assert_eq!(Event::from_session(&s), Some(Event::Question));
        assert!(content(&s, Event::Question, Detail::Detailed)
            .1
            .contains("Which branch"));
        s.pending_tool_name = Some("Bash".into());
        s.pending_tool_input =
            Some(serde_json::json!({"command":"cargo test", "secret":"never dump unknown fields"}));
        assert_eq!(Event::from_session(&s), Some(Event::Permission));
        let body = content(&s, Event::Permission, Detail::Detailed).1;
        assert!(body.contains("Bash: cargo test"));
        assert!(!body.contains("secret"));
    }
    #[test]
    fn absent_reply_does_not_use_activity_or_old_prompt() {
        let mut s = fixture();
        s.latest_message = "private thinking".into();
        assert!(!content(&s, Event::Reply, Detail::Detailed)
            .1
            .contains("private thinking"));
    }

    #[test]
    fn cooldown_is_per_provider_session_and_event() {
        let sent = HashMap::from([(("codex:same".into(), Event::Reply), Instant::now())]);
        assert!(on_cooldown(&sent, &("codex:same".into(), Event::Reply), 30));
        assert!(!on_cooldown(
            &sent,
            &("codex:same".into(), Event::Permission),
            30
        ));
        assert!(!on_cooldown(
            &sent,
            &("claudeCode:same".into(), Event::Reply),
            30
        ));
        assert!(!on_cooldown(&sent, &("codex:same".into(), Event::Reply), 0));
    }
    #[test]
    fn partial_settings_keep_defaults() {
        let p: Preferences = serde_json::from_str(r#"{"enabled":false}"#).unwrap();
        assert!(!p.enabled);
        assert!(p.questions);
        assert_eq!(p.cooldown_seconds, 30);
    }
    #[test]
    fn unicode_preview_is_bounded() {
        assert_eq!(compact("  你好\n世界 ", 4), "你好 世…");
    }
    #[test]
    fn event_filters_are_independent() {
        let mut p = Preferences::default();
        p.reply_ready = false;
        assert!(!Event::Reply.enabled(&p));
        assert!(Event::Permission.enabled(&p));
        p.enabled = false;
        assert!(!Event::Question.enabled(&p));
    }
}
