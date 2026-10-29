// The tauri_nspanel macro expands panel_event! in a way that requires `-> ()` syntax,
// which clippy flags as unused_unit. Suppress it since we cannot change the macro invocation.
#![cfg_attr(target_os = "macos", allow(clippy::unused_unit))]

// ── Core modules (always compiled) ──────────────────────────────────
pub mod actions;
pub mod claude_hooks;
pub mod claude_path;
pub mod claude_usage;
pub mod debug_log;
pub mod session;

// ── GUI-only modules ────────────────────────────────────────────────
#[cfg(all(not(mobile), feature = "gui"))]
pub mod auth;
#[cfg(all(not(mobile), feature = "gui"))]
mod blocking;
#[cfg(all(not(mobile), feature = "gui"))]
pub mod notifications;
#[cfg(all(not(mobile), feature = "gui"))]
pub mod polling;
#[cfg(all(not(mobile), feature = "gui"))]
pub mod remote_access;
#[cfg(all(not(mobile), feature = "gui"))]
pub mod subscription_usage;
#[cfg(all(not(mobile), feature = "gui"))]
pub mod web_server;
#[cfg(all(not(mobile), feature = "gui"))]
pub mod ws_auth;

// ── CLI module ──────────────────────────────────────────────────────
#[cfg(feature = "cli")]
pub mod cli;

#[cfg(feature = "gui")]
use actions::open_session as open_session_action;
#[cfg(feature = "gui")]
use polling::{start_polling, Session};
#[cfg(feature = "gui")]
use session::conversation::{Conversation, ConversationProgress};
// Re-export for web_server.rs which uses crate::get_conversation_data
#[cfg(feature = "gui")]
pub use session::conversation::{
    get_conversation_data, get_conversation_data_for_provider,
    get_conversation_data_for_provider_with_progress, get_conversation_data_with_progress,
};
#[cfg(feature = "gui")]
use std::sync::{Arc, Mutex};
#[cfg(feature = "gui")]
use std::time::Duration;
#[cfg(feature = "gui")]
use tauri::{
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, PhysicalPosition,
};
#[cfg(feature = "gui")]
use tauri::{AppHandle, Manager};
#[cfg(all(target_os = "macos", feature = "gui"))]
use tauri_nspanel::{
    tauri_panel, CollectionBehavior, ManagerExt as PanelManagerExt, PanelLevel, StyleMask,
    WebviewWindowExt as PanelExt,
};

// ── GUI-only: Tauri commands ─────────────────────────────────────────

#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn get_subscription_usage() -> Vec<subscription_usage::SubscriptionUsage> {
    subscription_usage::get_subscription_usage().await
}

#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn get_sessions(
    detector: tauri::State<'_, Arc<Mutex<session::DetectorState>>>,
) -> Result<Vec<Session>, String> {
    let detector = Arc::clone(detector.inner());
    blocking::scan(&blocking::DISCOVERY, move || {
        detect_snapshot(&detector).and_then(|snapshot| snapshot)
    })
    .await
}

/// Detect and enrich the current sessions using the shared detector.
///
/// The outer `Err` means the detector lock is poisoned; the inner `Err`
/// means detection or enrichment failed. Callers decide which errors to
/// propagate. The lock guard is dropped before the enrichment pass.
#[cfg(all(not(mobile), feature = "gui"))]
fn detect_snapshot(
    detector: &Mutex<session::DetectorState>,
) -> Result<Result<Vec<Session>, String>, String> {
    let detect_result = {
        let mut state = detector
            .lock()
            .map_err(|e| format!("Detector lock poisoned: {}", e))?;
        state.detect()
    };
    Ok(detect_result
        .map_err(|e| format!("Detect failed: {}", e))
        .and_then(|(detected, diag)| {
            session::enrichment::enrich_detected_sessions(detected, diag)
                .map(|(sessions, _)| sessions)
        }))
}

/// Detect the current sessions with the shared detector, without the
/// enrichment pass. Stop resolves its target from this, so it waits for the
/// lock instead of failing when a poll's scan is running.
#[cfg(all(not(mobile), feature = "gui"))]
pub(crate) fn detect_sessions(
    detector: &Mutex<session::DetectorState>,
) -> Result<Vec<session::DetectedSession>, String> {
    let mut state = detector
        .lock()
        .map_err(|e| format!("Detector lock poisoned: {}", e))?;
    state
        .detect()
        .map(|(detected, _)| detected)
        .map_err(|e| format!("Detect failed: {}", e))
}

/// Emit a fresh `sessions-updated` snapshot after a session action.
/// Lock poison is returned; detect and enrich errors are ignored.
#[cfg(all(not(mobile), feature = "gui"))]
fn emit_sessions_snapshot(
    app: &AppHandle,
    detector: &Mutex<session::DetectorState>,
) -> Result<(), String> {
    if let Ok(sessions) = detect_snapshot(detector)? {
        let _ = app.emit("sessions-updated", &sessions);
    }
    Ok(())
}

#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
fn get_detection_diagnostics(
    latest: tauri::State<'_, polling::LatestDiagnostics>,
) -> session::DetectionDiagnostics {
    latest.0.lock().map(|d| d.clone()).unwrap_or_default()
}

#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn get_conversation(
    app: tauri::AppHandle,
    session_id: String,
    provider: Option<session::SessionProvider>,
    include_tools: Option<bool>,
) -> Result<Conversation, String> {
    let include_tools = include_tools.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
        let emit_id = session_id.clone();
        get_conversation_data_for_provider_with_progress(
            &session_id,
            provider,
            include_tools,
            &mut |bytes_read, bytes_total| {
                let _ = app.emit(
                    "conversation-progress",
                    ConversationProgress {
                        session_id: emit_id.clone(),
                        provider,
                        request_id: None,
                        bytes_read,
                        bytes_total,
                    },
                );
            },
        )
    })
    .await
    .map_err(|error| format!("Failed to load conversation: {error}"))?
}

/// Away-activity counts for the requested sessions. Reads the poll-maintained
/// index only; it never parses a conversation.
#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn get_away_activity(
    requests: Vec<session::away::AwayRequest>,
) -> Result<std::collections::HashMap<String, session::away::AwayActivity>, String> {
    tauri::async_runtime::spawn_blocking(move || session::away::away_activity(&requests))
        .await
        .map_err(|error| format!("Failed to read away activity: {error}"))
}

#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn get_session_history() -> Result<Vec<session::HistoryEntry>, String> {
    // History scans provider archives and must not run on a Tokio worker used
    // by the Tauri IPC runtime. In particular, Cursor's large composer DB is
    // intentionally warmed in the background, but the remaining bounded disk
    // reads and JSON metadata parsing are still blocking work.
    tauri::async_runtime::spawn_blocking(session::get_history)
        .await
        .map_err(|error| format!("Failed to load history: {error}"))?
}

#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn deep_search_sessions(
    query: String,
    #[allow(non_snake_case)] caseSensitive: Option<bool>,
    #[allow(non_snake_case)] wholeWord: Option<bool>,
) -> Result<Vec<session::DeepSearchHit>, String> {
    if query.trim().is_empty() {
        return Ok(vec![]);
    }
    session::deep_search(
        &query,
        caseSensitive.unwrap_or(false),
        wholeWord.unwrap_or(false),
    )
}

#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn get_cost_data() -> Result<session::CostData, String> {
    session::get_cost_data()
}

#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn get_memory_files() -> Result<Vec<session::ProjectMemory>, String> {
    session::get_memory_files()
}

/// Returns a map of parent_session_id -> subagent invocations detected by
/// parsing each session's JSONL transcript for Agent/Task tool_use entries.
///
/// `session_ids` is the frontend's already-known set of *raw Claude Code*
/// session IDs (from its own `sessions` store, which is what triggers this
/// call in the first place). It is not a set of provider-scoped UI keys such
/// as `claudeCode:<id>` and it must not include Codex/Cursor/Pi/OpenCode IDs.
/// Passing this set in lets the scan skip a stat/cache-lookup entirely for any
/// session file that isn't live and has nothing relevant cached, without
/// needing a second `claude agents --json` call on this side to re-derive the
/// same live set the main polling loop already has. The WebSocket protocol is
/// desktop-session-only for this store and does not expose this Tauri command;
/// no provider-scoped live-id payload is inferred there.
#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn get_subagents(
    session_ids: Vec<String>,
) -> Result<std::collections::HashMap<String, Vec<session::SubagentInfo>>, String> {
    // Transcript scans perform blocking disk I/O and JSON parsing. Running them
    // directly on Tokio workers can starve subscription IPC, pipes and timers.
    // Hold the permit inside the blocking job so cancellation cannot overlap scans.
    blocking::scan(&blocking::SUBAGENTS, move || {
        let live: std::collections::HashSet<String> = session_ids.into_iter().collect();
        Ok(session::all_subagents_by_session(&live))
    })
    .await
}

/// Returns the prompt + final result (plus usage stats when available) for a
/// single Agent/Task tool_use inside the given parent session's JSONL.
#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn get_subagent_transcript(
    parent_session_id: String,
    subagent_id: String,
) -> Result<session::SubagentTranscript, String> {
    blocking::scan(&blocking::SUBAGENT_TRANSCRIPT, move || {
        session::get_subagent_transcript(&parent_session_id, &subagent_id).ok_or_else(|| {
            format!(
                "subagent {} not found in session {}",
                subagent_id, parent_session_id
            )
        })
    })
    .await
}

/// Returns the parsed TodoWrite tasks for a session, sorted by numeric `id`.
/// Reads `~/.claude/tasks/<session_id>/*.json`. Returns an empty list when
/// the directory does not exist; silently skips malformed JSON files.
#[cfg(all(not(mobile), feature = "gui", feature = "cli"))]
#[tauri::command]
async fn get_session_tasks(session_id: String) -> Result<Vec<serde_json::Value>, String> {
    cli::read_session_tasks(&session_id)
}

/// Save base64-encoded PNG data to a temp file and return the path.
/// Used by the token distance visualizer to share canvas screenshots.
#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn save_temp_image(data: String) -> Result<String, String> {
    use std::fs;

    let temp_dir = std::env::temp_dir();
    let file_path = temp_dir.join("c9watch-token-journey.png");

    // data is base64-encoded PNG (no data URL prefix)
    let bytes = data.strip_prefix("data:image/png;base64,").unwrap_or(&data);

    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(bytes)
        .map_err(|e| format!("Failed to decode base64: {}", e))?;

    fs::write(&file_path, decoded).map_err(|e| format!("Failed to write temp file: {}", e))?;

    Ok(file_path.to_string_lossy().to_string())
}

/// Open a directory in the system file manager (Finder on macOS)
#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn reveal_in_file_manager(path: String) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(&path)
        .spawn()
        .map_err(|e| format!("Failed to open directory: {}", e))?;
    Ok(())
}

#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn stop_session(
    app: AppHandle,
    detector: tauri::State<'_, Arc<Mutex<session::DetectorState>>>,
    session_key: String,
    pid: Option<u32>,
) -> Result<(), String> {
    let shared = Arc::clone(detector.inner());
    tauri::async_runtime::spawn_blocking(move || {
        actions::stop_session_by_key(
            &session_key,
            pid,
            || detect_sessions(&shared),
            actions::process_start_secs,
        )
    })
    .await
    .map_err(|e| format!("Stop failed: {}", e))??;
    tokio::time::sleep(Duration::from_millis(300)).await;

    emit_sessions_snapshot(&app, detector.inner())
}

#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn open_session(pid: u32, project_path: String) -> Result<(), String> {
    open_session_action(pid, project_path)
}

#[cfg(all(not(mobile), feature = "gui"))]
pub(crate) fn run_validated_rename<F>(
    provider: Option<session::SessionProvider>,
    session_id: &str,
    owners: &session::ProviderSourceOwners,
    write: F,
) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String>,
{
    session::validate_rename_request(provider, session_id, owners)?;
    write()
}

#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn rename_session(
    app: AppHandle,
    detector: tauri::State<'_, Arc<Mutex<session::DetectorState>>>,
    session_id: String,
    new_name: String,
    provider: Option<session::SessionProvider>,
) -> Result<(), String> {
    let owners = session::global_provider_source_owners();
    run_validated_rename(provider, &session_id, &owners, || {
        write_session_title(&session_id, &new_name)
    })?;

    emit_sessions_snapshot(&app, detector.inner())
}

/// Persist a session rename for both the Tauri command and the WebSocket
/// handler: Claude Code's native JSONL entry (primary) plus c9watch's own
/// custom titles file (fallback for the history view).
#[cfg(all(not(mobile), feature = "gui"))]
pub(crate) fn write_session_title(session_id: &str, new_name: &str) -> Result<(), String> {
    write_native_custom_title(session_id, new_name);

    let mut custom_titles = session::CustomTitles::load();
    custom_titles.set(session_id.to_string(), new_name.to_string());
    custom_titles.save()
}

/// Append a `custom-title` entry to the session's JSONL file in Claude Code's native format.
/// This makes the rename visible to Claude Code itself (and persists across c9watch reinstalls).
pub fn write_native_custom_title(session_id: &str, title: &str) {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };
    write_native_custom_title_under(&home.join(".claude").join("projects"), session_id, title);
}

fn write_native_custom_title_under(projects_dir: &std::path::Path, session_id: &str, title: &str) {
    use std::io::Write;

    // An unreadable projects root is a silent no-op (no warning), as before.
    if std::fs::read_dir(projects_dir).is_err() {
        return;
    }

    let Some(jsonl_path) =
        session::claude_files::find_session_file_in_projects_dir(projects_dir, session_id)
    else {
        debug_log::log_warn(&format!(
            "Could not find JSONL file for session {} to write native custom-title",
            session_id
        ));
        return;
    };

    let entry = serde_json::json!({
        "type": "custom-title",
        "customTitle": title,
        "sessionId": session_id,
    });
    match std::fs::OpenOptions::new().append(true).open(&jsonl_path) {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{}", entry) {
                debug_log::log_warn(&format!(
                    "Failed to write native custom-title for {}: {}",
                    session_id, e
                ));
            }
        }
        Err(e) => {
            debug_log::log_warn(&format!(
                "Failed to open JSONL for native custom-title {}: {}",
                session_id, e
            ));
        }
    }
}

#[cfg(test)]
mod native_custom_title_tests {
    use super::write_native_custom_title_under;

    const SESSION_ID: &str = "ffffffff-ffff-ffff-ffff-ffffffffffff";

    #[test]
    fn appends_custom_title_entry_to_matching_project_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let other_dir = temp.path().join("-other-project");
        let project_dir = temp.path().join("-synthetic-project");
        std::fs::create_dir_all(&other_dir).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();
        let jsonl = project_dir.join(format!("{SESSION_ID}.jsonl"));
        std::fs::write(&jsonl, "{\"type\":\"user\"}\n").unwrap();

        write_native_custom_title_under(temp.path(), SESSION_ID, "synthetic title");

        let content = std::fs::read_to_string(&jsonl).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "{\"type\":\"user\"}");
        let entry: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(
            entry,
            serde_json::json!({
                "type": "custom-title",
                "customTitle": "synthetic title",
                "sessionId": SESSION_ID,
            })
        );
        assert_eq!(std::fs::read_dir(&other_dir).unwrap().count(), 0);
    }

    #[test]
    fn missing_session_jsonl_creates_no_file() {
        let temp = tempfile::tempdir().unwrap();
        let project_dir = temp.path().join("-synthetic-project");
        std::fs::create_dir_all(&project_dir).unwrap();

        write_native_custom_title_under(temp.path(), SESSION_ID, "synthetic title");

        assert_eq!(std::fs::read_dir(&project_dir).unwrap().count(), 0);
    }

    #[test]
    fn missing_projects_root_is_a_no_op() {
        let temp = tempfile::tempdir().unwrap();
        write_native_custom_title_under(&temp.path().join("absent"), SESSION_ID, "synthetic title");
        assert!(!temp.path().join("absent").exists());
    }
}

#[cfg(all(test, not(mobile), feature = "gui"))]
mod detect_snapshot_tests {
    use super::detect_snapshot;
    use crate::session::source::{
        DetectedSession, DetectionDiagnostics, SessionDetectorError, SessionSource,
    };
    use crate::session::{BackendMode, DetectorState};
    use std::sync::{Arc, Mutex};

    struct StubSource {
        fail: bool,
    }

    impl SessionSource for StubSource {
        fn detect(
            &mut self,
        ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
            if self.fail {
                Err(SessionDetectorError::CliFailed("synthetic".into()))
            } else {
                Ok((Vec::new(), DetectionDiagnostics::default()))
            }
        }
        fn backend_name(&self) -> &'static str {
            "stub"
        }
    }

    fn detector(fail: bool) -> Mutex<DetectorState> {
        Mutex::new(DetectorState::for_test(
            Box::new(StubSource { fail }),
            BackendMode::ForceCli,
        ))
    }

    #[test]
    fn empty_detection_yields_empty_snapshot() {
        let sessions = detect_snapshot(&detector(false)).unwrap().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn detect_failure_is_the_inner_error() {
        let message = detect_snapshot(&detector(true)).unwrap().unwrap_err();
        assert!(message.starts_with("Detect failed: "), "{message}");
    }

    #[test]
    fn lock_poison_is_the_outer_error() {
        let detector = Arc::new(detector(false));
        let poisoner = Arc::clone(&detector);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the detector lock");
        })
        .join();

        let message = detect_snapshot(&detector).unwrap_err();
        assert!(message.starts_with("Detector lock poisoned: "), "{message}");
    }
}

/// Provider-owner fixtures shared by the Tauri and WebSocket rename tests.
/// Each one plants a synthetic `session_id` transcript under `temp`.
#[cfg(all(test, not(mobile), feature = "gui"))]
pub(crate) mod rename_test_fixtures {
    use crate::session;
    use tempfile::TempDir;

    pub(crate) fn synthetic_cursor_owner(
        temp: &TempDir,
        session_id: &str,
    ) -> session::ProviderSourceOwners {
        let transcript_dir = temp
            .path()
            .join("synthetic-project")
            .join("agent-transcripts")
            .join(session_id);
        std::fs::create_dir_all(&transcript_dir).unwrap();
        std::fs::write(
            transcript_dir.join(format!("{session_id}.jsonl")),
            b"{\"role\":\"user\",\"message\":{\"content\":[]}}\n",
        )
        .unwrap();

        session::ProviderSourceOwners::from_test_sources(
            None,
            Some(session::cursor::CursorSessionSource::at_root(
                temp.path().to_path_buf(),
            )),
            None,
        )
    }

    pub(crate) fn synthetic_codex_owner(
        temp: &TempDir,
        session_id: &str,
    ) -> session::ProviderSourceOwners {
        let rollout_dir = temp.path().join("2026").join("08").join("24");
        std::fs::create_dir_all(&rollout_dir).unwrap();
        std::fs::write(
            rollout_dir.join(format!("rollout-synthetic-{session_id}.jsonl")),
            b"{}\n",
        )
        .unwrap();

        session::ProviderSourceOwners::from_test_sources(
            Some(session::codex::CodexSessionSource::at_root(
                temp.path().to_path_buf(),
            )),
            None,
            None,
        )
    }
}

#[cfg(all(test, not(mobile), feature = "gui"))]
mod rename_collision_tests {
    use super::rename_test_fixtures::{synthetic_codex_owner, synthetic_cursor_owner};
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SESSION_ID: &str = "dddddddd-dddd-dddd-dddd-dddddddddddd";

    fn assert_no_write_for_collision(owners: session::ProviderSourceOwners) {
        let writes = AtomicUsize::new(0);
        let result = run_validated_rename(None, SESSION_ID, &owners, || {
            writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        assert!(result.is_err());
        assert_eq!(writes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn tauri_rename_collision_rejects_before_custom_title_write_for_cursor() {
        let temp = tempfile::tempdir().unwrap();
        assert_no_write_for_collision(synthetic_cursor_owner(&temp, SESSION_ID));
    }

    #[test]
    fn tauri_rename_collision_rejects_before_custom_title_write_for_codex() {
        let temp = tempfile::tempdir().unwrap();
        assert_no_write_for_collision(synthetic_codex_owner(&temp, SESSION_ID));
    }
}

/// Show and focus the main application window
#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn show_main_window(app: AppHandle) -> Result<(), String> {
    // On macOS the popover panel auto-hides via window_did_resign_key
    // when the main window takes focus. No need to explicitly hide it here.
    // (Calling panel.hide() here would deadlock the panel manager mutex
    // because resign_key fires synchronously and also calls get_webview_panel.)
    #[cfg(not(target_os = "macos"))]
    if let Some(popover) = app.get_webview_window("popover") {
        let _ = popover.hide();
    }

    if let Some(window) = app.get_webview_window("main") {
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Server connection info for the MOBILE modal and Settings. It carries no
/// credential: a phone gets its token by redeeming a pairing code.
#[cfg(all(not(mobile), feature = "gui"))]
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub port: u16,
    /// The address a phone uses: the default-route IP in LAN mode,
    /// `127.0.0.1` otherwise.
    pub local_ip: String,
    pub lan_enabled: bool,
    /// `"loopback"` or `"lan"`.
    pub bind: &'static str,
    /// This Mac's interface IPs, read now. Empty unless LAN is on.
    pub lan_addresses: Vec<String>,
}

#[cfg(all(not(mobile), feature = "gui"))]
fn server_info(server: &web_server::WsServer) -> ServerInfo {
    let lan_enabled = server.remote().lan_enabled;
    let (local_ip, lan_addresses) = if lan_enabled {
        let ips = remote_access::local_interface_ips()
            .into_iter()
            .map(|ip| ip.to_string())
            .collect();
        (auth::get_local_ip(), ips)
    } else {
        ("127.0.0.1".to_string(), Vec::new())
    };
    let port = server.port();
    ServerInfo {
        port,
        local_ip,
        lan_enabled,
        bind: if lan_enabled { "lan" } else { "loopback" },
        lan_addresses,
    }
}

#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn get_server_info(
    server: tauri::State<'_, Arc<web_server::WsServer>>,
) -> Result<ServerInfo, String> {
    Ok(server_info(&server))
}

/// A new one-time pairing code for the MOBILE modal. The previous code
/// stops working.
#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn create_pairing_code(
    server: tauri::State<'_, Arc<web_server::WsServer>>,
) -> Result<web_server::PairingCode, String> {
    server.create_pairing_code()
}

/// Forget every paired device and close their sockets.
#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn forget_paired_clients(
    server: tauri::State<'_, Arc<web_server::WsServer>>,
) -> Result<(), String> {
    server.forget_paired_clients()
}

#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn get_remote_access(
    server: tauri::State<'_, Arc<web_server::WsServer>>,
) -> Result<remote_access::RemoteAccess, String> {
    Ok(server.remote())
}

/// Save the remote-access settings and rebind the WS server. Open sockets
/// close; turning LAN off drops LAN clients.
#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn set_remote_access(
    server: tauri::State<'_, Arc<web_server::WsServer>>,
    remote: remote_access::RemoteAccess,
) -> Result<remote_access::RemoteAccess, String> {
    server.update(remote).await
}

#[cfg(all(not(mobile), feature = "gui"))]
#[tauri::command]
async fn get_debug_logs() -> Result<Vec<debug_log::LogEntry>, String> {
    Ok(debug_log::get_logs())
}

// ── NSPanel definition for macOS popover ────────────────────────────
#[cfg(all(target_os = "macos", feature = "gui"))]
tauri_panel! {
    panel!(PopoverPanel {
        config: {
            can_become_key_window: true,
            is_floating_panel: true
        }
    })

    panel_event!(PopoverEventHandler {
        window_did_resign_key(notification: &NSNotification) -> ()
    })
}

// ── App entry point (GUI only) ──────────────────────────────────────

#[cfg(feature = "gui")]
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_opener::init());

    // Desktop: full setup with all plugins and commands
    #[cfg(not(mobile))]
    let builder = builder
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_sharekit::init());

    // macOS: NSPanel plugin for popover (must appear above fullscreen apps)
    #[cfg(target_os = "macos")]
    let builder = builder.plugin(tauri_nspanel::init());

    #[cfg(not(mobile))]
    let builder = builder
        .setup(|app| {
            notifications::initialize(app.handle()).map_err(std::io::Error::other)?;
            // ── WebSocket server ────────────────────────────────
            let port = web_server::WS_PORT;

            let (sessions_tx, _rx) = tokio::sync::broadcast::channel::<String>(16);
            let (notifications_tx, _nrx) = tokio::sync::broadcast::channel::<String>(16);

            // ── Shared detector state ───────────────────────────
            let detector_state: Arc<Mutex<session::DetectorState>> =
                Arc::new(Mutex::new(session::DetectorState::new()));
            app.manage(detector_state.clone());

            let ws_state = Arc::new(web_server::WsState::new(
                sessions_tx.clone(),
                notifications_tx.clone(),
                detector_state.clone(),
            ));
            let remote_path = app
                .path()
                .app_config_dir()
                .map_err(std::io::Error::other)?
                .join(remote_access::FILE_NAME);
            let remote = remote_access::load(&remote_path);
            let ws_server = Arc::new(web_server::WsServer::new(ws_state, port, remote_path));
            app.manage(ws_server.clone());
            tauri::async_runtime::spawn(async move {
                if let Err(e) = ws_server.apply(remote).await {
                    debug_log::log_error(&format!("[ws-server] {}", e));
                }
            });
            app.manage(polling::LatestDiagnostics::default());

            // ── Polling loop ────────────────────────────────────
            start_polling(
                app.handle().clone(),
                detector_state.clone(),
                sessions_tx,
                notifications_tx,
            );

            // ── Main window: hide on close + recheck backend on focus ─────────
            // hide-on-close keeps "Open Dashboard" working from the popover.
            // Focused(true) triggers DetectorState::recheck_and_maybe_swap so a
            // user upgrading Claude Code mid-session can pick up the new
            // backend the next time they refocus the app.
            // NOTE: on_window_event in Tauri 2 REPLACES (not appends) the
            // handler — both behaviors must live in this single closure.
            #[cfg(not(mobile))]
            if let Some(main_win) = app.get_webview_window("main") {
                let main_win_clone = main_win.clone();
                let detector_for_focus = detector_state.clone();
                main_win.on_window_event(move |event| match event {
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        let _ = main_win_clone.hide();
                    }
                    tauri::WindowEvent::Focused(true) => {
                        if let Ok(mut state) = detector_for_focus.lock() {
                            state.recheck_and_maybe_swap();
                        }
                    }
                    _ => {}
                });
            }

            // ── Popover panel: convert NSWindow to NSPanel for fullscreen support ──
            // NSPanel can appear above fullscreen apps, unlike regular NSWindow.
            #[cfg(target_os = "macos")]
            if let Some(popover) = app.get_webview_window("popover") {
                match popover.to_panel::<PopoverPanel>() {
                    Err(e) => {
                        debug_log::log_warn(&format!("Failed to convert popover to NSPanel: {e}. Fullscreen support unavailable."));
                        // Do not return early — tray icon setup must still proceed below.
                    }
                    Ok(panel) => {
                        // Status level (25) = same as macOS menu bar
                        panel.set_level(PanelLevel::Status.value());

                        // NonactivatingPanel: won't steal focus from the fullscreen app
                        panel.set_style_mask(StyleMask::empty().nonactivating_panel().into());

                        // Allow in all Spaces including fullscreen
                        panel.set_collection_behavior(
                            CollectionBehavior::new()
                                .full_screen_auxiliary()
                                .can_join_all_spaces()
                                .stationary()
                                .into(),
                        );

                        // Don't hide when app is deactivated (when fullscreen app is active)
                        panel.set_hides_on_deactivate(false);

                        // Rounded corners at the native window level
                        panel.set_corner_radius(10.0);

                        // Click-outside dismiss: hide panel when it loses key window status
                        let handler = PopoverEventHandler::new();
                        let handle = app.handle().clone();
                        handler.window_did_resign_key(move |_notification| {
                            if let Ok(p) = handle.get_webview_panel("popover") {
                                p.hide();
                            }
                        });
                        panel.set_event_handler(Some(handler.as_ref()));
                    }
                }
            }

            // ── Tray icon ───────────────────────────────────────
            let app_handle = app.handle().clone();
            TrayIconBuilder::new()
                .icon(tauri::include_image!("icons/tray-icon.png"))
                .icon_as_template(true)
                .tooltip("c9watch")
                .on_tray_icon_event(move |_tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        rect,
                        ..
                    } = event
                    {
                        // Use NSPanel via tauri-nspanel on macOS for fullscreen support
                        #[cfg(target_os = "macos")]
                        {
                            if let Ok(panel) = app_handle.get_webview_panel("popover") {
                                if panel.is_visible() {
                                    panel.hide();
                                } else {
                                    // Position below the tray icon, centered horizontally
                                    if let Some(popover) = app_handle.get_webview_window("popover")
                                    {
                                        let scale = popover
                                            .current_monitor()
                                            .ok()
                                            .flatten()
                                            .map(|m| m.scale_factor())
                                            .unwrap_or(1.0);

                                        let pos = rect.position.to_physical::<f64>(scale);
                                        let size = rect.size.to_physical::<f64>(scale);

                                        // Align panel left edge with tray icon left edge
                                        let x = pos.x;
                                        let y = pos.y + size.height + 4.0;

                                        let _ = popover.set_position(PhysicalPosition::new(
                                            x.round() as i32,
                                            y.round() as i32,
                                        ));
                                    }
                                    panel.show_and_make_key();
                                }
                            }
                        }

                        // Non-macOS: use regular window
                        #[cfg(not(target_os = "macos"))]
                        {
                            if let Some(popover) = app_handle.get_webview_window("popover") {
                                if popover.is_visible().unwrap_or(false) {
                                    let _ = popover.hide();
                                } else {
                                    let scale = popover
                                        .current_monitor()
                                        .ok()
                                        .flatten()
                                        .map(|m| m.scale_factor())
                                        .unwrap_or(1.0);
                                    let pos = rect.position.to_physical::<f64>(scale);
                                    let size = rect.size.to_physical::<f64>(scale);
                                    let popover_physical_width = popover
                                        .outer_size()
                                        .map(|s| s.width as f64)
                                        .unwrap_or(320.0);

                                    let x =
                                        pos.x + (size.width / 2.0) - (popover_physical_width / 2.0);
                                    let y = pos.y + size.height + 4.0;

                                    let _ = popover.set_position(PhysicalPosition::new(
                                        x.round() as i32,
                                        y.round() as i32,
                                    ));
                                    let _ = popover.show();
                                    let _ = popover.set_focus();
                                }
                            }
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            session::opencode::opencode_connection_status,
            session::opencode::opencode_connect,
            notifications::get_notification_preferences,
            notifications::save_notification_preferences,
            notifications::test_native_notification,
            get_sessions,
            get_detection_diagnostics,
            get_subscription_usage,
            get_conversation,
            get_away_activity,
            get_session_history,
            deep_search_sessions,
            get_cost_data,
            get_memory_files,
            get_subagents,
            get_subagent_transcript,
            get_session_tasks,
            save_temp_image,
            reveal_in_file_manager,
            stop_session,
            open_session,
            rename_session,
            show_main_window,
            get_server_info,
            create_pairing_code,
            forget_paired_clients,
            get_remote_access,
            set_remote_access,
            get_debug_logs
        ]);

    // Mobile: minimal shell (all communication via WebSocket from the frontend)
    #[cfg(mobile)]
    let builder = builder.setup(|_app| Ok(()));

    builder
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // Prevent the app from exiting when all windows are closed.
            // This is essential for tray/menu bar apps — the app stays alive
            // in the background with the tray icon even when no windows are visible.
            // Guard for desktop only: on mobile the OS controls the app lifecycle.
            #[cfg(not(mobile))]
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                api.prevent_exit();
            }
        });
}
