//! macOS Now Playing metadata bridge.
//!
//! The system owns the final UI. c9watch supplies the selected AI session as
//! title/status/project metadata and uses the app icon as square artwork.

use crate::polling::Session;
use crate::session::SessionStatus;
use serde::Serialize;
use std::ffi::CString;

const WIDGET_GROUP: &str = "group.com.minchenlee.c9watch";
const WIDGET_BUNDLE_ID: &str = "com.minchenlee.c9watch.widget";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WidgetSnapshot<'a> {
    has_session: bool,
    provider: &'a str,
    title: &'a str,
    status: &'a str,
    project: &'a str,
    latest_message: &'a str,
    working_count: usize,
    approval_count: usize,
    waiting_count: usize,
}

fn write_widget_snapshot(snapshot: &WidgetSnapshot<'_>) {
    let Some(home) = dirs::home_dir() else { return };
    let Ok(data) = serde_json::to_vec(snapshot) else { return };

    let destinations = [
        home.join("Library/Group Containers")
            .join(WIDGET_GROUP)
            .join("widget.json"),
        home.join("Library/Containers")
            .join(WIDGET_BUNDLE_ID)
            .join("Data/Library/Application Support/c9watch/widget.json"),
    ];
    for path in destinations {
        let Some(directory) = path.parent() else { continue };
        if let Err(error) = std::fs::create_dir_all(directory)
            .and_then(|()| std::fs::write(&path, &data))
        {
            crate::debug_log::log_warn(&format!(
                "Failed to update desktop widget data at {}: {error}",
                path.display()
            ));
        }
    }
}

unsafe extern "C" {
    fn c9watch_update_now_playing(
        title: *const std::ffi::c_char,
        status: *const std::ffi::c_char,
        project: *const std::ffi::c_char,
        latest_message: *const std::ffi::c_char,
        is_playing: bool,
    );
    fn c9watch_clear_now_playing();
}

fn c_string(value: &str) -> CString {
    CString::new(value.replace('\0', " ")).unwrap_or_default()
}

fn status_priority(status: &SessionStatus) -> u8 {
    match status {
        SessionStatus::NeedsAttention => 0,
        SessionStatus::Working => 1,
        SessionStatus::Connecting => 2,
        SessionStatus::WaitingForInput => 3,
    }
}

fn selected_session<'a>(sessions: &'a [Session]) -> Option<&'a Session> {
    let codex = sessions
        .iter()
        .filter(|session| session.provider == crate::session::SessionProvider::Codex)
        .collect::<Vec<_>>();
    let candidates = if codex.is_empty() {
        sessions.iter().collect::<Vec<_>>()
    } else {
        codex
    };

    candidates.into_iter().min_by(|a, b| {
        status_priority(&a.status)
            .cmp(&status_priority(&b.status))
            .then_with(|| b.modified.cmp(&a.modified))
    })
}

fn status_counts(sessions: &[Session]) -> (usize, usize, usize) {
    sessions.iter().fold((0, 0, 0), |mut counts, session| {
        match &session.status {
            SessionStatus::Working | SessionStatus::Connecting => counts.0 += 1,
            SessionStatus::NeedsAttention => counts.1 += 1,
            SessionStatus::WaitingForInput => counts.2 += 1,
        }
        counts
    })
}

pub fn publish(sessions: &[Session]) {
    let (working_count, approval_count, waiting_count) = status_counts(sessions);

    let Some(session) = selected_session(sessions) else {
        write_widget_snapshot(&WidgetSnapshot {
            has_session: false,
            provider: "c9watch",
            title: "No active session",
            status: "Waiting",
            project: "",
            latest_message: "Start a Codex or Claude Code session.",
            working_count,
            approval_count,
            waiting_count,
        });
        crate::debug_log::log_info("Now Playing cleared: no active AI sessions");
        // SAFETY: The bridge is compiled and linked only on macOS.
        unsafe { c9watch_clear_now_playing() };
        return;
    };

    let title = session
        .custom_title
        .as_deref()
        .or(session.summary.as_deref())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&session.session_name);
    let status = match &session.status {
        SessionStatus::NeedsAttention => session
            .pending_tool_name
            .as_deref()
            .map(|tool| format!("Needs your input · {tool}"))
            .unwrap_or_else(|| "Needs your input".to_string()),
        SessionStatus::Working => "Working".to_string(),
        SessionStatus::Connecting => "Connecting".to_string(),
        SessionStatus::WaitingForInput => "Waiting for input".to_string(),
    };
    let project = std::path::Path::new(&session.project_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("AI session");
    let provider = if session.provider == crate::session::SessionProvider::Codex {
        "CHATGPT / CODEX"
    } else {
        "CLAUDE CODE"
    };
    write_widget_snapshot(&WidgetSnapshot {
        has_session: true,
        provider,
        title,
        status: &status,
        project,
        latest_message: &session.latest_message,
        working_count,
        approval_count,
        waiting_count,
    });
    let title = c_string(title);
    let status = c_string(&status);
    let project = c_string(project);
    let latest_message = c_string(&session.latest_message);
    let is_playing = session.status == SessionStatus::Working;
    crate::debug_log::log_info(&format!(
        "Now Playing published: provider={:?}, status={:?}",
        session.provider, session.status
    ));

    // SAFETY: All pointers remain valid for the duration of the synchronous
    // Objective-C call and the bridge copies the strings into Foundation objects.
    unsafe {
        c9watch_update_now_playing(
            title.as_ptr(),
            status.as_ptr(),
            project.as_ptr(),
            latest_message.as_ptr(),
            is_playing,
        );
    }
}
