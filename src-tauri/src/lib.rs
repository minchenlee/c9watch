// The tauri_nspanel macro expands panel_event! in a way that requires `-> ()` syntax,
// which clippy flags as unused_unit. Suppress it since we cannot change the macro invocation.
#![cfg_attr(target_os = "macos", allow(clippy::unused_unit))]

// Desktop-only modules
#[cfg(not(mobile))]
pub mod actions;
#[cfg(not(mobile))]
pub mod auth;
#[cfg(not(mobile))]
pub mod polling;
#[cfg(not(mobile))]
pub mod web_server;
#[cfg(not(mobile))]
pub mod debug_log;

// Shared modules (types used by both desktop and mobile builds)
pub mod session;

#[cfg(not(mobile))]
use actions::{open_session as open_session_action, stop_session as stop_session_action};
#[cfg(not(mobile))]
use polling::{detect_and_enrich_sessions, start_polling, Session};
use serde::Serialize;
use session::{extract_messages, parse_all_entries, ImageBlock, MessageType};
#[cfg(not(mobile))]
use std::sync::Arc;
#[cfg(not(mobile))]
use std::time::Duration;
#[cfg(not(mobile))]
use tauri::{
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, PhysicalPosition,
};
use tauri::{AppHandle, Manager};
#[cfg(target_os = "macos")]
use tauri_nspanel::{
    tauri_panel, CollectionBehavior, ManagerExt as PanelManagerExt, PanelLevel, StyleMask,
    WebviewWindowExt as PanelExt,
};

// ── Shared types ────────────────────────────────────────────────────

/// Conversation structure for the frontend
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub session_id: String,
    pub messages: Vec<ConversationMessage>,
}

/// Individual message in a conversation
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    pub timestamp: String,
    pub message_type: MessageType,
    pub content: String,
    /// Images attached to this message (screenshots pasted by the user)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageBlock>,
}

// ── Desktop-only commands ───────────────────────────────────────────

#[cfg(not(mobile))]
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg(not(mobile))]
#[tauri::command]
async fn get_sessions() -> Result<Vec<Session>, String> {
    polling::detect_and_enrich_sessions().map(|(sessions, _)| sessions)
}

/// Core logic for getting conversation data (shared by Tauri command and WS handler)
#[cfg(not(mobile))]
pub fn get_conversation_data(session_id: &str) -> Result<Conversation, String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    let claude_projects_dir = home_dir.join(".claude").join("projects");

    let entries = std::fs::read_dir(&claude_projects_dir)
        .map_err(|e| format!("Failed to read projects directory: {}", e))?;

    let session_filename = format!("{}.jsonl", session_id);

    for entry in entries.flatten() {
        let project_path = entry.path();
        if !project_path.is_dir() {
            continue;
        }

        let session_file = project_path.join(&session_filename);
        if session_file.exists() {
            let entries = parse_all_entries(&session_file)
                .map_err(|e| format!("Failed to parse session file: {}", e))?;

            let messages = extract_messages(&entries);

            let conversation_messages: Vec<ConversationMessage> = messages
                .into_iter()
                .map(|(timestamp, msg_type, content, images)| ConversationMessage {
                    timestamp,
                    message_type: msg_type,
                    content,
                    images,
                })
                .collect();

            return Ok(Conversation {
                session_id: session_id.to_string(),
                messages: conversation_messages,
            });
        }
    }

    Err(format!(
        "Session {} not found in any project directory",
        session_id
    ))
}

#[cfg(not(mobile))]
#[tauri::command]
async fn get_conversation(session_id: String) -> Result<Conversation, String> {
    get_conversation_data(&session_id)
}

#[cfg(not(mobile))]
#[tauri::command]
async fn get_session_history() -> Result<Vec<session::HistoryEntry>, String> {
    session::get_history()
}

#[cfg(not(mobile))]
#[tauri::command]
async fn deep_search_sessions(query: String) -> Result<Vec<session::DeepSearchHit>, String> {
    if query.trim().is_empty() {
        return Ok(vec![]);
    }
    session::deep_search(&query)
}

#[cfg(not(mobile))]
#[tauri::command]
async fn get_cost_data() -> Result<session::CostData, String> {
    session::get_cost_data()
}

#[cfg(not(mobile))]
#[tauri::command]
async fn get_memory_files() -> Result<Vec<session::ProjectMemory>, String> {
    session::get_memory_files()
}

/// Save base64-encoded PNG data to a temp file and return the path.
/// Used by the token distance visualizer to share canvas screenshots.
#[cfg(not(mobile))]
#[tauri::command]
async fn save_temp_image(data: String) -> Result<String, String> {
    use std::fs;

    let temp_dir = std::env::temp_dir();
    let file_path = temp_dir.join("c9watch-token-journey.png");

    // data is base64-encoded PNG (no data URL prefix)
    let bytes = data
        .strip_prefix("data:image/png;base64,")
        .unwrap_or(&data);

    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(bytes)
        .map_err(|e| format!("Failed to decode base64: {}", e))?;

    fs::write(&file_path, decoded).map_err(|e| format!("Failed to write temp file: {}", e))?;

    Ok(file_path.to_string_lossy().to_string())
}

/// Open a directory in the system file manager (Finder on macOS)
#[cfg(not(mobile))]
#[tauri::command]
async fn reveal_in_file_manager(path: String) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(&path)
        .spawn()
        .map_err(|e| format!("Failed to open directory: {}", e))?;
    Ok(())
}

#[cfg(not(mobile))]
#[tauri::command]
async fn stop_session(app: AppHandle, pid: u32) -> Result<(), String> {
    stop_session_action(pid)?;
    std::thread::sleep(Duration::from_millis(300));

    if let Ok((sessions, _)) = detect_and_enrich_sessions() {
        let _ = app.emit("sessions-updated", &sessions);
    }
    Ok(())
}

#[cfg(not(mobile))]
#[tauri::command]
async fn open_session(pid: u32, project_path: String) -> Result<(), String> {
    open_session_action(pid, project_path)
}

#[cfg(not(mobile))]
#[tauri::command]
async fn rename_session(
    app: AppHandle,
    session_id: String,
    new_name: String,
) -> Result<(), String> {
    // Write to Claude Code's native JSONL format (primary)
    write_native_custom_title(&session_id, &new_name);

    // Also write to c9watch's own custom titles (fallback for history view)
    let mut custom_titles = session::CustomTitles::load();
    custom_titles.set(session_id, new_name);
    custom_titles.save()?;

    if let Ok((sessions, _)) = detect_and_enrich_sessions() {
        let _ = app.emit("sessions-updated", &sessions);
    }
    Ok(())
}

/// Append a `custom-title` entry to the session's JSONL file in Claude Code's native format.
/// This makes the rename visible to Claude Code itself (and persists across c9watch reinstalls).
pub fn write_native_custom_title(session_id: &str, title: &str) {
    use std::io::Write;

    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };
    let projects_dir = home.join(".claude").join("projects");

    // Search all project directories for the session JSONL
    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let jsonl_path = path.join(format!("{}.jsonl", session_id));
        if jsonl_path.exists() {
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
            return;
        }
    }

    debug_log::log_warn(&format!(
        "Could not find JSONL file for session {} to write native custom-title",
        session_id
    ));
}

/// Get the terminal title for a session (iTerm2 only, macOS)
#[tauri::command]
async fn get_terminal_title(pid: u32) -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    {
        Ok(actions::get_iterm2_session_title(pid))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        Ok(None)
    }
}

/// Show and focus the main application window
#[cfg(not(mobile))]
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

/// Server connection info for the mobile client
#[cfg(not(mobile))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub token: String,
    pub port: u16,
    pub local_ip: String,
    pub ws_url: String,
}

#[cfg(not(mobile))]
#[tauri::command]
async fn get_server_info(info: tauri::State<'_, ServerInfo>) -> Result<ServerInfo, String> {
    Ok(ServerInfo {
        token: info.token.clone(),
        port: info.port,
        local_ip: info.local_ip.clone(),
        ws_url: info.ws_url.clone(),
    })
}

#[cfg(not(mobile))]
#[tauri::command]
async fn get_debug_logs() -> Result<Vec<debug_log::LogEntry>, String> {
    Ok(debug_log::get_logs())
}

// ── NSPanel definition for macOS popover ────────────────────────────
#[cfg(target_os = "macos")]
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

// ── App entry point ─────────────────────────────────────────────────

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
            // ── WebSocket server ────────────────────────────────
            let token = auth::generate_token();
            let local_ip = auth::get_local_ip();
            let port = web_server::WS_PORT;

            let ws_url = format!("ws://{}:{}/ws?token={}", local_ip, port, token);
            let http_url = format!("http://{}:{}/?token={}", local_ip, port, token);

            debug_log::log_info(&format!("Mobile connection ready — URL: {}", http_url));
            qr2term::print_qr(&http_url).ok();
            eprintln!();

            let (sessions_tx, _rx) = tokio::sync::broadcast::channel::<String>(16);
            let (notifications_tx, _nrx) = tokio::sync::broadcast::channel::<String>(16);

            let server_info = ServerInfo {
                token: token.clone(),
                port,
                local_ip: local_ip.clone(),
                ws_url,
            };
            app.manage(server_info);

            let ws_state = Arc::new(web_server::WsState {
                auth_token: token,
                sessions_tx: sessions_tx.clone(),
                notifications_tx: notifications_tx.clone(),
            });
            tauri::async_runtime::spawn(web_server::start_server(ws_state));

            // ── Polling loop ────────────────────────────────────
            start_polling(app.handle().clone(), sessions_tx, notifications_tx);

            // ── Main window: hide on close instead of destroying ──────────────
            // This allows "Open Dashboard" from the popover to re-show it.
            // Without this, closing the window destroys it and show() is a no-op.
            #[cfg(not(mobile))]
            if let Some(main_win) = app.get_webview_window("main") {
                let main_win_clone = main_win.clone();
                main_win.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = main_win_clone.hide();
                    }
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
            greet,
            get_sessions,
            get_conversation,
            get_session_history,
            deep_search_sessions,
            get_cost_data,
            get_memory_files,
            save_temp_image,
            reveal_in_file_manager,
            stop_session,
            open_session,
            rename_session,
            get_terminal_title,
            show_main_window,
            get_server_info,
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
