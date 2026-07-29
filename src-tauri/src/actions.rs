use std::process::Command;

/// Open a session by focusing its terminal or IDE window
///
/// This finds the parent application of the Claude process and activates it.
/// Works with Terminal, iTerm2, Zed, VS Code, Cursor, and other applications.
pub fn open_session(pid: u32, project_path: String) -> Result<(), String> {
    if pid == 0 {
        return Err("This session does not expose a focusable terminal process".to_string());
    }
    // Find the parent application by walking up the process tree
    let app_name = find_parent_app(pid)?;

    // Extract project name from path for window matching
    let project_name = std::path::Path::new(&project_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    crate::debug_log::log_info(&format!(
        "[open_session] App: {}, Project: {}, Path: {}",
        app_name, project_name, project_path
    ));

    // Supacode: the session's env carries its worktree/tab/surface IDs,
    // so we can focus the exact surface via the bundled `supacode` CLI (macOS only)
    #[cfg(target_os = "macos")]
    if app_name == "supacode" {
        return focus_supacode_session(pid);
    }

    // iTerm2: use tty matching to focus the correct tab (macOS only)
    #[cfg(target_os = "macos")]
    if app_name == "iTerm" || app_name == "iTerm2" {
        return focus_iterm2_session(pid);
    }

    // macOS Terminal.app: use tty matching to focus the correct tab (macOS only)
    #[cfg(target_os = "macos")]
    if app_name == "Terminal" {
        return focus_terminal_session(pid);
    }

    // JetBrains IDEs: use URL scheme to focus the correct project window
    if is_jetbrains_ide(&app_name) {
        return focus_jetbrains_window(&app_name, &project_path);
    }

    // Try to use app-specific CLI to open/focus the correct window
    if let Some(cli_path) = get_app_cli(&app_name) {
        crate::debug_log::log_info(&format!(
            "[open_session] Using CLI: {} to open: {}",
            cli_path, project_path
        ));

        // VS Code family uses -r flag to reuse window, -g to not open new if exists
        let output =
            if app_name == "Visual Studio Code" || app_name == "Cursor" || app_name == "Windsurf" {
                Command::new(&cli_path)
                    .arg("-r") // Reuse existing window
                    .arg("-g") // Don't grab focus for new file (but we want focus)
                    .arg(&project_path)
                    .output()
            } else {
                // Zed and others just take the path
                Command::new(&cli_path).arg(&project_path).output()
            };

        match output {
            Ok(out) => {
                if out.status.success() {
                    crate::debug_log::log_info("[open_session] CLI succeeded");
                    return Ok(());
                } else {
                    let error = String::from_utf8_lossy(&out.stderr);
                    crate::debug_log::log_error(&format!("[open_session] CLI error: {}", error));
                }
            }
            Err(e) => {
                crate::debug_log::log_error(&format!("[open_session] Failed to run CLI: {}", e));
            }
        }
    }

    // Platform-specific fallback to activate the app
    activate_app_fallback(&app_name)?;

    Ok(())
}

/// Get the controlling tty of a process via `ps -o tty=`
#[cfg(target_os = "macos")]
fn get_process_tty(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .arg("-o")
        .arg("tty=")
        .arg("-p")
        .arg(pid.to_string())
        .output()
        .ok()?;
    let tty = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if tty.is_empty() || tty == "??" {
        None
    } else {
        Some(tty)
    }
}

/// Walk up the process tree to find a tty (Claude may be a child process)
#[cfg(target_os = "macos")]
fn get_session_tty(pid: u32) -> Option<String> {
    let mut current_pid = pid;
    for _ in 0..10 {
        if let Some(tty) = get_process_tty(current_pid) {
            return Some(tty);
        }
        let ppid_output = Command::new("ps")
            .arg("-o")
            .arg("ppid=")
            .arg("-p")
            .arg(current_pid.to_string())
            .output()
            .ok()?;
        let ppid: u32 = String::from_utf8_lossy(&ppid_output.stdout)
            .trim()
            .parse()
            .ok()?;
        if ppid <= 1 {
            break;
        }
        current_pid = ppid;
    }
    None
}

/// Read a single environment variable from a running process via `ps eww`.
/// Only visible for processes owned by the current user, which is the case
/// for the Claude sessions c9watch monitors.
#[cfg(target_os = "macos")]
fn get_process_env(pid: u32, key: &str) -> Option<String> {
    let output = Command::new("ps")
        .arg("eww")
        .arg("-o")
        .arg("command=")
        .arg("-p")
        .arg(pid.to_string())
        .output()
        .ok()?;
    let raw = String::from_utf8_lossy(&output.stdout);
    let prefix = format!("{}=", key);
    // `ps eww` appends env as space-separated KEY=VALUE words. Supacode IDs are
    // URL-encoded / UUIDs, so they never contain spaces.
    raw.split_whitespace()
        .find_map(|word| word.strip_prefix(prefix.as_str()).map(|v| v.to_string()))
}

/// Locate the `supacode` CLI bundled with the app
#[cfg(target_os = "macos")]
fn get_supacode_cli() -> Option<String> {
    let paths = [
        "/Applications/supacode.app/Contents/Resources/bin/supacode",
        "/usr/local/bin/supacode",
    ];
    paths
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(|p| p.to_string())
}

/// Focus the exact Supacode tab + surface owning a Claude session.
///
/// Supacode injects SUPACODE_WORKTREE_ID / SUPACODE_TAB_ID / SUPACODE_SURFACE_ID
/// into every surface's shell, so the Claude process's own environment says
/// exactly where it lives. The bundled `supacode` CLI does the focusing.
#[cfg(target_os = "macos")]
fn focus_supacode_session(pid: u32) -> Result<(), String> {
    let worktree = get_process_env(pid, "SUPACODE_WORKTREE_ID");
    let tab = get_process_env(pid, "SUPACODE_TAB_ID");
    let surface = get_process_env(pid, "SUPACODE_SURFACE_ID");

    crate::debug_log::log_info(&format!(
        "[open_session] Supacode coords for PID {}: worktree={:?} tab={:?} surface={:?}",
        pid, worktree, tab, surface
    ));

    // Bring the app to the front first so the focused surface is visible
    activate_app_fallback("supacode")?;

    let (Some(worktree), Some(tab)) = (worktree, tab) else {
        // Env not visible (or an older Supacode without the vars) —
        // app-level activation is the best we can do
        return Ok(());
    };

    let Some(cli) = get_supacode_cli() else {
        crate::debug_log::log_error("[open_session] Supacode CLI not found");
        return Ok(());
    };

    let mut tab_cmd = Command::new(&cli);
    tab_cmd
        .arg("tab")
        .arg("focus")
        .arg("--worktree")
        .arg(&worktree)
        .arg("--tab")
        .arg(&tab)
        .arg("--timeout")
        .arg("5");
    if let Err(e) = tab_cmd.output() {
        crate::debug_log::log_error(&format!("[open_session] supacode tab focus failed: {}", e));
        return Ok(());
    }

    if let Some(surface) = surface {
        let mut surface_cmd = Command::new(&cli);
        surface_cmd
            .arg("surface")
            .arg("focus")
            .arg("--worktree")
            .arg(&worktree)
            .arg("--tab")
            .arg(&tab)
            .arg("--surface")
            .arg(&surface)
            .arg("--timeout")
            .arg("5");
        if let Err(e) = surface_cmd.output() {
            crate::debug_log::log_error(&format!(
                "[open_session] supacode surface focus failed: {}",
                e
            ));
        }
    }

    Ok(())
}

/// Focus the correct iTerm2 tab/session by matching tty
#[cfg(target_os = "macos")]
fn focus_iterm2_session(pid: u32) -> Result<(), String> {
    let tty = get_session_tty(pid);
    crate::debug_log::log_info(&format!("[open_session] iTerm2 tty for PID {}: {:?}", pid, tty));

    let Some(tty) = tty else {
        // No tty found — just activate iTerm2
        let _ = Command::new("osascript")
            .arg("-e")
            .arg(r#"tell application "iTerm2" to activate"#)
            .output();
        return Ok(());
    };

    // AppleScript: iterate all iTerm2 sessions, match by tty, focus it
    let script = format!(
        r#"
        tell application "iTerm2"
            activate
            repeat with w in windows
                repeat with t in tabs of w
                    repeat with s in sessions of t
                        if tty of s ends with "{tty}" then
                            select s
                            select t
                            set index of w to 1
                            return "found"
                        end if
                    end repeat
                end repeat
            end repeat
            return "not found"
        end tell
        "#,
        tty = tty
    );

    let output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("Failed to run AppleScript: {}", e))?;

    let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
    crate::debug_log::log_info(&format!("[open_session] iTerm2 tty match result: {}", result));

    Ok(())
}

/// Focus the correct macOS Terminal.app tab by matching tty
///
/// Terminal.app tabs expose their tty (e.g. "/dev/ttys004") in the scripting
/// dictionary, so the exact tab can be selected instead of just activating
/// the app. Unlike iTerm2 there is no session layer — windows contain tabs.
/// `miniaturized` is a native window property here, so no Accessibility
/// permission is needed to restore a Dock-minimized window.
///
/// Ordering matters: select the tab and raise its window (referenced by a
/// stable window id, not the loop variable) BEFORE calling `activate`. Calling
/// `activate` first surfaces whatever window was already frontmost, and a
/// subsequent raise of a background window does not reliably re-order the
/// display — so a session in a non-frontmost window would be missed.
#[cfg(target_os = "macos")]
fn focus_terminal_session(pid: u32) -> Result<(), String> {
    let tty = get_session_tty(pid);
    crate::debug_log::log_info(&format!(
        "[open_session] Terminal tty for PID {}: {:?}",
        pid, tty
    ));

    let Some(tty) = tty else {
        // No tty found — fall back to app activation
        return activate_app_fallback("Terminal");
    };

    // AppleScript: find the tab by tty, select it, raise its window by id,
    // then activate the app last.
    let script = format!(
        r#"
        tell application "Terminal"
            set targetWinId to missing value
            repeat with w in windows
                repeat with t in tabs of w
                    if tty of t ends with "{tty}" then
                        set selected of t to true
                        set targetWinId to id of w
                        exit repeat
                    end if
                end repeat
                if targetWinId is not missing value then exit repeat
            end repeat
            if targetWinId is missing value then return "not found"
            try
                if miniaturized of (window id targetWinId) then set miniaturized of (window id targetWinId) to false
            end try
            set index of (window id targetWinId) to 1
            set frontmost of (window id targetWinId) to true
            activate
            return "found"
        end tell
        "#,
        tty = tty
    );

    let output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("Failed to run AppleScript: {}", e))?;

    let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
    crate::debug_log::log_info(&format!(
        "[open_session] Terminal tty match result: {}",
        result
    ));

    if result != "found" {
        // Tab no longer exists (or tty changed) — fall back to app activation
        return activate_app_fallback("Terminal");
    }

    Ok(())
}

/// Focus the correct JetBrains IDE project window using the IDE's URL scheme.
///
/// JetBrains IDEs register custom URL schemes (e.g., phpstorm://, idea://) that
/// can open files and focus the correct project window. Using the system URL
/// opener brings the right window to front without requiring Accessibility or
/// Screen Recording permissions.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn focus_jetbrains_window(app_name: &str, project_path: &str) -> Result<(), String> {
    let scheme = jetbrains_url_scheme(app_name)
        .unwrap_or("idea");
    let root = find_jetbrains_project_root(project_path);

    crate::debug_log::log_info(&format!(
        "[open_session] JetBrains URL scheme: {}://open?file={} (from: {})",
        scheme, root, project_path
    ));

    let encoded_path = encode_path_for_url(&root);
    let url = format!("{}://open?file={}", scheme, encoded_path);
    let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let output = Command::new(cmd)
        .arg(&url)
        .output()
        .map_err(|e| format!("Failed to open JetBrains URL: {}", e))?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        crate::debug_log::log_error(&format!("[open_session] URL scheme failed: {}", error));
        return activate_app_fallback(app_name);
    }

    crate::debug_log::log_info("[open_session] JetBrains URL scheme succeeded");
    Ok(())
}

/// Get the iTerm2 session title for a process by matching its tty
#[cfg(target_os = "macos")]
pub fn get_iterm2_session_title(pid: u32) -> Option<String> {
    let tty = get_session_tty(pid)?;

    let script = format!(
        r#"
        tell application "System Events"
            if not (exists process "iTerm2") then return ""
        end tell
        tell application "iTerm2"
            repeat with w in windows
                repeat with t in tabs of w
                    repeat with s in sessions of t
                        if tty of s ends with "{tty}" then
                            return name of s
                        end if
                    end repeat
                end repeat
            end repeat
            return ""
        end tell
        "#,
        tty = tty
    );

    let output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .ok()?;

    let title = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

/// Platform-specific fallback to activate/focus an application
#[cfg(target_os = "macos")]
fn activate_app_fallback(app_name: &str) -> Result<(), String> {
    // Activate the app and restore any minimized windows.
    // macOS `activate` alone does NOT un-minimize windows from the Dock,
    // so we use System Events to set AXMinimized to false.
    //
    // This fallback is used by terminals without a dedicated code path or CLI:
    // Ghostty, Alacritty, kitty, Warp, Hyper, WezTerm, etc.
    // (iTerm2 and macOS Terminal have their own tty-matching focus functions;
    // VS Code/Cursor/Zed use CLI.)
    //
    // Notes:
    // - Unminimizes ALL windows of the app, not just the target one.
    // - AXMinimized requires Accessibility permission and may throw;
    //   wrapped in try/end try so `activate` always succeeds regardless.
    let script = format!(
        r#"
        tell application "{app}" to activate
        try
            tell application "System Events"
                tell process "{app}"
                    repeat with w in windows
                        if value of attribute "AXMinimized" of w is true then
                            set value of attribute "AXMinimized" of w to false
                        end if
                    end repeat
                end tell
            end tell
        end try
        "#,
        app = app_name
    );
    let output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("Failed to execute osascript: {}", e))?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        crate::debug_log::log_error(&format!("[open_session] AppleScript error: {}", error));
    }
    Ok(())
}

/// Linux fallback: try xdg-open or xdotool to raise window
#[cfg(target_os = "linux")]
fn activate_app_fallback(app_name: &str) -> Result<(), String> {
    // Try xdotool to find and activate a window by name
    let search_name = match app_name {
        "Visual Studio Code" => "Visual Studio Code",
        "Cursor" => "Cursor",
        "Windsurf" => "Windsurf",
        "Zed" => "Zed",
        "Sublime Text" => "Sublime Text",
        "PhpStorm" => "PhpStorm",
        "IntelliJ IDEA" | "IntelliJ IDEA CE" => "IntelliJ IDEA",
        "WebStorm" => "WebStorm",
        "PyCharm" | "PyCharm CE" => "PyCharm",
        "GoLand" => "GoLand",
        "CLion" => "CLion",
        "Rider" => "Rider",
        "RubyMine" => "RubyMine",
        "DataGrip" => "DataGrip",
        "Android Studio" => "Android Studio",
        "Aqua" => "Aqua",
        "Fleet" => "Fleet",
        "RustRover" => "RustRover",
        _ => app_name,
    };

    let output = Command::new("xdotool")
        .arg("search")
        .arg("--name")
        .arg(search_name)
        .arg("windowactivate")
        .output();

    match output {
        Ok(out) => {
            if out.status.success() {
                crate::debug_log::log_info(&format!(
                    "[open_session] xdotool activated window for: {}",
                    search_name
                ));
                return Ok(());
            }
            crate::debug_log::log_warn(&format!(
                "[open_session] xdotool failed, window not found for: {}",
                search_name
            ));
        }
        Err(_) => {
            crate::debug_log::log_warn("[open_session] xdotool not available");
        }
    }

    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn activate_app_fallback(_app_name: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn focus_jetbrains_window(_app_name: &str, _project_path: &str) -> Result<(), String> {
    Ok(())
}

/// Percent-encode a file path for use in a URL query parameter
fn encode_path_for_url(path: &str) -> String {
    path.chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '#' => "%23".to_string(),
            '%' => "%25".to_string(),
            '&' => "%26".to_string(),
            '?' => "%3F".to_string(),
            _ => c.to_string(),
        })
        .collect()
}

/// Walk up from a directory to find the JetBrains project root (contains `.idea/`).
///
/// When a Claude session starts in a subfolder (e.g., `/monorepo/backend`),
/// the actual JetBrains project root may be an ancestor containing `.idea/`.
/// In monorepos, multiple directories may have `.idea/`, so we use the topmost
/// one to match the root project that JetBrains has open.
/// Returns the original path if no `.idea/` is found.
fn find_jetbrains_project_root(path: &str) -> String {
    let mut current = std::path::PathBuf::from(path);
    let mut topmost: Option<String> = None;
    loop {
        if current.join(".idea").is_dir() {
            topmost = Some(current.to_string_lossy().to_string());
        }
        if !current.pop() {
            break;
        }
    }
    topmost.unwrap_or_else(|| path.to_string())
}

/// Map JetBrains IDE app names to their registered URL scheme.
/// Returns `None` for non-JetBrains IDEs.
fn jetbrains_url_scheme(app_name: &str) -> Option<&'static str> {
    match app_name {
        "PhpStorm" => Some("phpstorm"),
        "IntelliJ IDEA" | "IntelliJ IDEA CE" => Some("idea"),
        "WebStorm" => Some("webstorm"),
        "PyCharm" | "PyCharm CE" => Some("pycharm"),
        "GoLand" => Some("goland"),
        "CLion" => Some("clion"),
        "Rider" => Some("rider"),
        "RubyMine" => Some("rubymine"),
        "DataGrip" => Some("datagrip"),
        "Android Studio" => Some("studio"),
        "Aqua" => Some("aqua"),
        "Fleet" => Some("fleet"),
        "RustRover" => Some("rustrover"),
        _ => None,
    }
}

/// Get the JetBrains Toolbox scripts directory
#[cfg(target_os = "macos")]
fn get_jetbrains_toolbox_scripts_dir() -> Option<String> {
    dirs::home_dir().map(|home| {
        home.join("Library/Application Support/JetBrains/Toolbox/scripts")
            .to_string_lossy()
            .to_string()
    })
}

/// Get the CLI path for an application if available
#[cfg(target_os = "macos")]
fn get_app_cli(app_name: &str) -> Option<String> {
    let cli_paths: &[(&str, &[&str])] = &[
        ("Zed", &["/Applications/Zed.app/Contents/MacOS/cli"]),
        (
            "Visual Studio Code",
            &[
                "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code",
                "/usr/local/bin/code",
            ],
        ),
        (
            "Cursor",
            &[
                "/Applications/Cursor.app/Contents/Resources/app/bin/cursor",
                "/Applications/Cursor.app/Contents/Resources/app/bin/code",
                "/usr/local/bin/cursor",
            ],
        ),
        (
            "Windsurf",
            &[
                "/Applications/Windsurf.app/Contents/Resources/app/bin/windsurf",
                "/Applications/Windsurf.app/Contents/Resources/app/bin/code",
            ],
        ),
    ];

    for (name, paths) in cli_paths {
        if *name == app_name {
            for path in *paths {
                if std::path::Path::new(path).exists() {
                    return Some(path.to_string());
                }
            }
        }
    }

    // JetBrains IDEs: check Toolbox scripts dir, then ~/Applications, then /Applications
    let jetbrains_cli: Option<(&str, &str)> = match app_name {
        "PhpStorm" => Some(("phpstorm", "PhpStorm")),
        "IntelliJ IDEA" | "IntelliJ IDEA CE" => Some(("idea", "IntelliJ IDEA")),
        "WebStorm" => Some(("webstorm", "WebStorm")),
        "PyCharm" | "PyCharm CE" => Some(("pycharm", "PyCharm")),
        "GoLand" => Some(("goland", "GoLand")),
        "CLion" => Some(("clion", "CLion")),
        "Rider" => Some(("rider", "Rider")),
        "RubyMine" => Some(("rubymine", "RubyMine")),
        "DataGrip" => Some(("datagrip", "DataGrip")),
        "Android Studio" => Some(("studio", "Android Studio")),
        "Aqua" => Some(("aqua", "Aqua")),
        "Fleet" => Some(("fleet", "Fleet")),
        "RustRover" => Some(("rustrover", "RustRover")),
        _ => None,
    };

    if let Some((bin_name, app_dir_name)) = jetbrains_cli {
        // 1. JetBrains Toolbox scripts directory
        if let Some(scripts_dir) = get_jetbrains_toolbox_scripts_dir() {
            let toolbox_path = format!("{}/{}", scripts_dir, bin_name);
            if std::path::Path::new(&toolbox_path).exists() {
                return Some(toolbox_path);
            }
        }

        // 2. ~/Applications (Toolbox install location)
        if let Some(home) = dirs::home_dir() {
            let user_app_path = home
                .join(format!(
                    "Applications/{}.app/Contents/MacOS/{}",
                    app_dir_name, bin_name
                ))
                .to_string_lossy()
                .to_string();
            if std::path::Path::new(&user_app_path).exists() {
                return Some(user_app_path);
            }
        }

        // 3. /Applications (manual install location)
        let system_app_path = format!(
            "/Applications/{}.app/Contents/MacOS/{}",
            app_dir_name, bin_name
        );
        if std::path::Path::new(&system_app_path).exists() {
            return Some(system_app_path);
        }
    }

    None
}

/// Get the JetBrains Toolbox scripts directory on Linux
#[cfg(target_os = "linux")]
fn get_jetbrains_toolbox_scripts_dir() -> Option<String> {
    dirs::home_dir().map(|home| {
        home.join(".local/share/JetBrains/Toolbox/scripts")
            .to_string_lossy()
            .to_string()
    })
}

/// Get the CLI path for an application on Linux
#[cfg(target_os = "linux")]
fn get_app_cli(app_name: &str) -> Option<String> {
    let cli_paths: &[(&str, &[&str])] = &[
        ("Zed", &["/usr/bin/zed", "/usr/local/bin/zed"]),
        (
            "Visual Studio Code",
            &["/usr/bin/code", "/usr/local/bin/code", "/snap/bin/code"],
        ),
        ("Cursor", &["/usr/bin/cursor", "/usr/local/bin/cursor"]),
        (
            "Windsurf",
            &["/usr/bin/windsurf", "/usr/local/bin/windsurf"],
        ),
        (
            "Sublime Text",
            &["/usr/bin/subl", "/usr/local/bin/subl", "/snap/bin/subl"],
        ),
    ];

    for (name, paths) in cli_paths {
        if *name == app_name {
            for path in *paths {
                if std::path::Path::new(path).exists() {
                    return Some(path.to_string());
                }
            }
        }
    }

    // JetBrains IDEs: check Toolbox scripts dir, then standard paths
    let jetbrains_bin = match app_name {
        "PhpStorm" => Some("phpstorm"),
        "IntelliJ IDEA" | "IntelliJ IDEA CE" => Some("idea"),
        "WebStorm" => Some("webstorm"),
        "PyCharm" | "PyCharm CE" => Some("pycharm"),
        "GoLand" => Some("goland"),
        "CLion" => Some("clion"),
        "Rider" => Some("rider"),
        "RubyMine" => Some("rubymine"),
        "DataGrip" => Some("datagrip"),
        "Android Studio" => Some("studio"),
        "Aqua" => Some("aqua"),
        "Fleet" => Some("fleet"),
        "RustRover" => Some("rustrover"),
        _ => None,
    };

    if let Some(bin_name) = jetbrains_bin {
        // 1. JetBrains Toolbox scripts directory
        if let Some(scripts_dir) = get_jetbrains_toolbox_scripts_dir() {
            let toolbox_path = format!("{}/{}", scripts_dir, bin_name);
            if std::path::Path::new(&toolbox_path).exists() {
                return Some(toolbox_path);
            }
        }

        // 2. Standard paths
        for prefix in &["/usr/local/bin", "/snap/bin", "/usr/bin"] {
            let path = format!("{}/{}", prefix, bin_name);
            if std::path::Path::new(&path).exists() {
                return Some(path);
            }
        }
    }

    // Fallback: try to find the binary via `which`
    let bin_name = match app_name {
        "Zed" => Some("zed"),
        "Visual Studio Code" => Some("code"),
        "Cursor" => Some("cursor"),
        "Windsurf" => Some("windsurf"),
        "Sublime Text" => Some("subl"),
        "PhpStorm" => Some("phpstorm"),
        "IntelliJ IDEA" | "IntelliJ IDEA CE" => Some("idea"),
        "WebStorm" => Some("webstorm"),
        "PyCharm" | "PyCharm CE" => Some("pycharm"),
        "GoLand" => Some("goland"),
        "CLion" => Some("clion"),
        "Rider" => Some("rider"),
        "RubyMine" => Some("rubymine"),
        "DataGrip" => Some("datagrip"),
        "Android Studio" => Some("studio"),
        "Aqua" => Some("aqua"),
        "Fleet" => Some("fleet"),
        "RustRover" => Some("rustrover"),
        _ => None,
    };

    if let Some(name) = bin_name {
        if let Ok(output) = Command::new("which").arg(name).output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Some(path);
                }
            }
        }
    }

    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn get_app_cli(_app_name: &str) -> Option<String> {
    None
}

/// Find the parent GUI application for a given process ID
fn find_parent_app(pid: u32) -> Result<String, String> {
    let mut current_pid = pid;

    crate::debug_log::log_info(&format!("[open_session] Starting with PID: {}", pid));

    // Walk up the process tree to find a GUI application
    for i in 0..20 {
        // Get the command/path for current process
        let comm_output = Command::new("ps")
            .arg("-o")
            .arg("comm=")
            .arg("-p")
            .arg(current_pid.to_string())
            .output()
            .map_err(|e| format!("Failed to execute ps: {}", e))?;

        let comm = String::from_utf8_lossy(&comm_output.stdout)
            .trim()
            .to_string();
        crate::debug_log::log_info(&format!(
            "[open_session] Step {}: PID {} -> comm: {}",
            i, current_pid, comm
        ));

        // Check if this is a known GUI application
        if let Some(app_name) = get_app_name(&comm) {
            crate::debug_log::log_info(&format!("[open_session] Found app: {}", app_name));
            return Ok(app_name.to_string());
        }

        // Get parent PID
        let ppid_output = Command::new("ps")
            .arg("-o")
            .arg("ppid=")
            .arg("-p")
            .arg(current_pid.to_string())
            .output()
            .map_err(|e| format!("Failed to execute ps: {}", e))?;

        let ppid_str = String::from_utf8_lossy(&ppid_output.stdout)
            .trim()
            .to_string();
        let ppid: u32 = ppid_str.parse().unwrap_or(1);
        crate::debug_log::log_info(&format!("[open_session] Parent PID: {}", ppid));

        // Move to parent
        if ppid <= 1 {
            crate::debug_log::log_info("[open_session] Reached root, checking current comm one more time");
            // Check current process one more time before giving up
            if let Some(app_name) = get_app_name(&comm) {
                crate::debug_log::log_info(&format!("[open_session] Found app at root: {}", app_name));
                return Ok(app_name.to_string());
            }
            break;
        }
        current_pid = ppid;
    }

    // Platform-specific fallback
    #[cfg(target_os = "macos")]
    {
        crate::debug_log::log_warn("[open_session] Falling back to Terminal");
        Ok("Terminal".to_string())
    }
    #[cfg(target_os = "linux")]
    {
        crate::debug_log::log_warn("[open_session] Falling back to xterm");
        Ok("xterm".to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok("Terminal".to_string())
    }
}

/// Map process command names to application names
fn get_app_name(comm: &str) -> Option<&'static str> {
    // macOS: Check for .app bundle paths (e.g., /Applications/Zed.app/Contents/MacOS/zed)
    #[cfg(target_os = "macos")]
    {
        let comm_lower = comm.to_lowercase();
        if comm_lower.contains(".app/") || comm_lower.contains(".app") {
            if comm_lower.contains("zed.app") {
                return Some("Zed");
            }
            // Supacode's surfaces run under a zmx helper that re-parents to
            // launchd, but its binary path still lives inside supacode.app.
            // Must be checked BEFORE VS Code: "supacode.app" contains "code.app".
            if comm_lower.contains("supacode.app") {
                return Some("supacode");
            }
            if comm_lower.contains("visual studio code.app") || comm_lower.contains("code.app") {
                return Some("Visual Studio Code");
            }
            if comm_lower.contains("cursor.app") {
                return Some("Cursor");
            }
            if comm_lower.contains("windsurf.app") {
                return Some("Windsurf");
            }
            if comm_lower.contains("iterm.app") || comm_lower.contains("iterm2.app") {
                return Some("iTerm");
            }
            if comm_lower.contains("terminal.app") {
                return Some("Terminal");
            }
            if comm_lower.contains("alacritty.app") {
                return Some("Alacritty");
            }
            if comm_lower.contains("kitty.app") {
                return Some("kitty");
            }
            if comm_lower.contains("ghostty.app") {
                return Some("Ghostty");
            }
            if comm_lower.contains("warp.app") {
                return Some("Warp");
            }
            if comm_lower.contains("hyper.app") {
                return Some("Hyper");
            }
            if comm_lower.contains("sublime text.app") {
                return Some("Sublime Text");
            }
            // JetBrains IDEs (check CE variants before non-CE to avoid false matches)
            if comm_lower.contains("intellij idea ce.app") {
                return Some("IntelliJ IDEA CE");
            }
            if comm_lower.contains("intellij idea.app") {
                return Some("IntelliJ IDEA");
            }
            if comm_lower.contains("pycharm ce.app") {
                return Some("PyCharm CE");
            }
            if comm_lower.contains("pycharm.app") {
                return Some("PyCharm");
            }
            if comm_lower.contains("phpstorm.app") {
                return Some("PhpStorm");
            }
            if comm_lower.contains("webstorm.app") {
                return Some("WebStorm");
            }
            if comm_lower.contains("goland.app") {
                return Some("GoLand");
            }
            if comm_lower.contains("clion.app") {
                return Some("CLion");
            }
            if comm_lower.contains("rider.app") {
                return Some("Rider");
            }
            if comm_lower.contains("rubymine.app") {
                return Some("RubyMine");
            }
            if comm_lower.contains("datagrip.app") {
                return Some("DataGrip");
            }
            if comm_lower.contains("android studio.app") {
                return Some("Android Studio");
            }
            if comm_lower.contains("aqua.app") {
                return Some("Aqua");
            }
            if comm_lower.contains("fleet.app") {
                return Some("Fleet");
            }
            if comm_lower.contains("rustrover.app") {
                return Some("RustRover");
            }
        }
    }

    // macOS: iTerm2 uses a server process like iTermServer-3.6.6
    // The path looks like: ~/Library/Application Support/iTerm2/iTermServer-X.Y.Z
    #[cfg(target_os = "macos")]
    {
        let comm_lower = comm.to_lowercase();
        if comm_lower.contains("itermserver") || comm_lower.contains("/iterm2/") {
            return Some("iTerm");
        }
    }

    // Extract the base name from the path
    let base_name = comm.rsplit('/').next().unwrap_or(comm);

    match base_name.to_lowercase().as_str() {
        // Terminals (cross-platform names)
        "terminal" => Some("Terminal"),
        "iterm2" | "iterm" => Some("iTerm"),
        "alacritty" => Some("Alacritty"),
        "kitty" => Some("kitty"),
        "warp" => Some("Warp"),
        "hyper" => Some("Hyper"),
        "gnome-terminal-server" | "gnome-terminal" => Some("GNOME Terminal"),
        "konsole" => Some("Konsole"),
        "xfce4-terminal" => Some("Xfce Terminal"),
        "xterm" => Some("xterm"),
        "foot" => Some("foot"),
        "wezterm" | "wezterm-gui" => Some("WezTerm"),
        "tilix" => Some("Tilix"),
        "terminator" => Some("Terminator"),
        "ghostty" => Some("Ghostty"),

        // IDEs
        "zed" | "zed-editor" => Some("Zed"),
        "code" | "code helper" | "electron" => Some("Visual Studio Code"),
        "cursor" => Some("Cursor"),
        "windsurf" => Some("Windsurf"),

        // JetBrains IDEs
        "phpstorm" => Some("PhpStorm"),
        "idea" => Some("IntelliJ IDEA"),
        "webstorm" => Some("WebStorm"),
        "pycharm" => Some("PyCharm"),
        "goland" => Some("GoLand"),
        "clion" => Some("CLion"),
        "rider" => Some("Rider"),
        "rubymine" => Some("RubyMine"),
        "datagrip" => Some("DataGrip"),
        "studio" => Some("Android Studio"),
        "aqua" => Some("Aqua"),
        "fleet" => Some("Fleet"),
        "rustrover" => Some("RustRover"),

        // Other editors
        "sublime_text" | "subl" => Some("Sublime Text"),
        "atom" => Some("Atom"),

        _ => None,
    }
}

/// Returns true if the app_name is a JetBrains IDE
fn is_jetbrains_ide(app_name: &str) -> bool {
    jetbrains_url_scheme(app_name).is_some()
}

/// Stop a session by sending SIGTERM to the process
///
/// This gracefully terminates the Claude process by sending a SIGTERM signal.
/// SIGTERM is preferred over SIGINT as Claude Code may trap SIGINT for its own use.
pub fn stop_session(pid: u32) -> Result<(), String> {
    if pid == 0 {
        return Err("This session cannot be stopped through c9watch".to_string());
    }
    crate::debug_log::log_info(&format!("[stop_session] Stopping PID: {}", pid));

    // First try SIGTERM (signal 15) - graceful termination
    let output = Command::new("kill")
        .arg("-15") // SIGTERM
        .arg(pid.to_string())
        .output()
        .map_err(|e| format!("Failed to execute kill command: {}", e))?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        crate::debug_log::log_error(&format!("[stop_session] SIGTERM failed: {}", error));

        // If SIGTERM fails, the process might not exist or we don't have permission
        return Err(format!("Failed to stop process {}: {}", pid, error));
    }

    crate::debug_log::log_info("[stop_session] SIGTERM sent successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stop_session_invalid_pid() {
        // Try to stop a non-existent process
        let result = stop_session(999999);
        assert!(result.is_err());
    }

    #[test]
    #[ignore] // This test requires manual verification
    fn test_open_session() {
        // Use current process PID for testing
        let result = open_session(std::process::id(), "/tmp".to_string());
        println!("Result: {:?}", result);
    }

    #[test]
    fn test_get_app_name_terminals() {
        assert_eq!(get_app_name("alacritty"), Some("Alacritty"));
        assert_eq!(get_app_name("kitty"), Some("kitty"));
        assert_eq!(get_app_name("/usr/bin/kitty"), Some("kitty"));
        assert_eq!(get_app_name("ghostty"), Some("Ghostty"));
    }

    #[test]
    fn test_get_app_name_ides() {
        assert_eq!(get_app_name("code"), Some("Visual Studio Code"));
        assert_eq!(get_app_name("zed"), Some("Zed"));
        assert_eq!(get_app_name("cursor"), Some("Cursor"));
    }

    #[test]
    fn test_get_app_name_all_terminals() {
        assert_eq!(get_app_name("warp"), Some("Warp"));
        assert_eq!(get_app_name("hyper"), Some("Hyper"));
        assert_eq!(get_app_name("iterm2"), Some("iTerm"));
        assert_eq!(get_app_name("iterm"), Some("iTerm"));
        assert_eq!(get_app_name("terminal"), Some("Terminal"));
        assert_eq!(get_app_name("wezterm"), Some("WezTerm"));
        assert_eq!(get_app_name("wezterm-gui"), Some("WezTerm"));
        assert_eq!(get_app_name("foot"), Some("foot"));
        assert_eq!(get_app_name("gnome-terminal"), Some("GNOME Terminal"));
        assert_eq!(get_app_name("gnome-terminal-server"), Some("GNOME Terminal"));
        assert_eq!(get_app_name("konsole"), Some("Konsole"));
        assert_eq!(get_app_name("xfce4-terminal"), Some("Xfce Terminal"));
        assert_eq!(get_app_name("xterm"), Some("xterm"));
        assert_eq!(get_app_name("tilix"), Some("Tilix"));
        assert_eq!(get_app_name("terminator"), Some("Terminator"));
        assert_eq!(get_app_name("/usr/bin/warp"), Some("Warp"));
    }

    #[test]
    fn test_get_app_name_all_ides() {
        assert_eq!(get_app_name("windsurf"), Some("Windsurf"));
        assert_eq!(get_app_name("zed-editor"), Some("Zed"));
        assert_eq!(get_app_name("sublime_text"), Some("Sublime Text"));
        assert_eq!(get_app_name("subl"), Some("Sublime Text"));
        assert_eq!(get_app_name("atom"), Some("Atom"));
        assert_eq!(get_app_name("code helper"), Some("Visual Studio Code"));
        assert_eq!(get_app_name("electron"), Some("Visual Studio Code"));
    }

    #[test]
    fn test_get_app_name_jetbrains_binary_names() {
        assert_eq!(get_app_name("phpstorm"), Some("PhpStorm"));
        assert_eq!(get_app_name("idea"), Some("IntelliJ IDEA"));
        assert_eq!(get_app_name("webstorm"), Some("WebStorm"));
        assert_eq!(get_app_name("pycharm"), Some("PyCharm"));
        assert_eq!(get_app_name("goland"), Some("GoLand"));
        assert_eq!(get_app_name("clion"), Some("CLion"));
        assert_eq!(get_app_name("rider"), Some("Rider"));
        assert_eq!(get_app_name("rubymine"), Some("RubyMine"));
        assert_eq!(get_app_name("datagrip"), Some("DataGrip"));
        assert_eq!(get_app_name("studio"), Some("Android Studio"));
        assert_eq!(get_app_name("aqua"), Some("Aqua"));
        assert_eq!(get_app_name("fleet"), Some("Fleet"));
        assert_eq!(get_app_name("rustrover"), Some("RustRover"));
    }

    #[test]
    fn test_get_app_name_unknown_returns_none() {
        assert_eq!(get_app_name("unknown_editor"), None);
        assert_eq!(get_app_name(""), None);
        assert_eq!(get_app_name("vim"), None);
        assert_eq!(get_app_name("emacs"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_get_app_name_macos_app_paths() {
        assert_eq!(
            get_app_name("/Applications/Zed.app/Contents/MacOS/zed"),
            Some("Zed")
        );
        assert_eq!(
            get_app_name("/Applications/Visual Studio Code.app/Contents/MacOS/Electron"),
            Some("Visual Studio Code")
        );
        assert_eq!(
            get_app_name("/Applications/Cursor.app/Contents/MacOS/Cursor"),
            Some("Cursor")
        );
        assert_eq!(
            get_app_name("/Applications/Windsurf.app/Contents/MacOS/Windsurf"),
            Some("Windsurf")
        );
        assert_eq!(
            get_app_name("/Applications/iTerm.app/Contents/MacOS/iTerm2"),
            Some("iTerm")
        );
        assert_eq!(
            get_app_name("/Applications/Alacritty.app/Contents/MacOS/alacritty"),
            Some("Alacritty")
        );
        assert_eq!(
            get_app_name("/Applications/kitty.app/Contents/MacOS/kitty"),
            Some("kitty")
        );
        assert_eq!(
            get_app_name("/Applications/Warp.app/Contents/MacOS/warp"),
            Some("Warp")
        );
        assert_eq!(
            get_app_name("/Applications/Hyper.app/Contents/MacOS/Hyper"),
            Some("Hyper")
        );
        assert_eq!(
            get_app_name("/Applications/Sublime Text.app/Contents/MacOS/sublime_text"),
            Some("Sublime Text")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_get_app_name_jetbrains_app_paths() {
        assert_eq!(
            get_app_name("/Applications/PhpStorm.app/Contents/MacOS/phpstorm"),
            Some("PhpStorm")
        );
        assert_eq!(
            get_app_name("/Applications/WebStorm.app/Contents/MacOS/webstorm"),
            Some("WebStorm")
        );
        assert_eq!(
            get_app_name("/Applications/GoLand.app/Contents/MacOS/goland"),
            Some("GoLand")
        );
        assert_eq!(
            get_app_name("/Applications/CLion.app/Contents/MacOS/clion"),
            Some("CLion")
        );
        assert_eq!(
            get_app_name("/Applications/Rider.app/Contents/MacOS/rider"),
            Some("Rider")
        );
        assert_eq!(
            get_app_name("/Applications/RubyMine.app/Contents/MacOS/rubymine"),
            Some("RubyMine")
        );
        assert_eq!(
            get_app_name("/Applications/DataGrip.app/Contents/MacOS/datagrip"),
            Some("DataGrip")
        );
        assert_eq!(
            get_app_name("/Applications/Android Studio.app/Contents/MacOS/studio"),
            Some("Android Studio")
        );
        assert_eq!(
            get_app_name("/Applications/Aqua.app/Contents/MacOS/aqua"),
            Some("Aqua")
        );
        assert_eq!(
            get_app_name("/Applications/Fleet.app/Contents/MacOS/fleet"),
            Some("Fleet")
        );
        assert_eq!(
            get_app_name("/Applications/RustRover.app/Contents/MacOS/rustrover"),
            Some("RustRover")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_get_app_name_ce_ordering_guard() {
        // Without this ordering guard, a path containing "IntelliJ IDEA CE.app"
        // would match the "intellij idea.app" check first (since CE path is a superset),
        // incorrectly returning "IntelliJ IDEA" instead of "IntelliJ IDEA CE".
        assert_eq!(
            get_app_name("/Applications/IntelliJ IDEA CE.app/Contents/MacOS/idea"),
            Some("IntelliJ IDEA CE")
        );
        assert_eq!(
            get_app_name("/Applications/IntelliJ IDEA.app/Contents/MacOS/idea"),
            Some("IntelliJ IDEA")
        );
        assert_eq!(
            get_app_name("/Applications/PyCharm CE.app/Contents/MacOS/pycharm"),
            Some("PyCharm CE")
        );
        assert_eq!(
            get_app_name("/Applications/PyCharm.app/Contents/MacOS/pycharm"),
            Some("PyCharm")
        );
    }

    #[test]
    fn test_is_jetbrains_ide() {
        assert!(is_jetbrains_ide("PhpStorm"));
        assert!(is_jetbrains_ide("IntelliJ IDEA"));
        assert!(is_jetbrains_ide("IntelliJ IDEA CE"));
        assert!(is_jetbrains_ide("WebStorm"));
        assert!(is_jetbrains_ide("PyCharm"));
        assert!(is_jetbrains_ide("PyCharm CE"));
        assert!(is_jetbrains_ide("GoLand"));
        assert!(is_jetbrains_ide("CLion"));
        assert!(is_jetbrains_ide("Rider"));
        assert!(is_jetbrains_ide("RubyMine"));
        assert!(is_jetbrains_ide("DataGrip"));
        assert!(is_jetbrains_ide("Android Studio"));
        assert!(is_jetbrains_ide("Aqua"));
        assert!(is_jetbrains_ide("Fleet"));
        assert!(is_jetbrains_ide("RustRover"));

        assert!(!is_jetbrains_ide("Visual Studio Code"));
        assert!(!is_jetbrains_ide("Cursor"));
        assert!(!is_jetbrains_ide("Zed"));
        assert!(!is_jetbrains_ide("iTerm"));
        assert!(!is_jetbrains_ide("Terminal"));
    }

    #[test]
    fn test_encode_path_for_url() {
        assert_eq!(encode_path_for_url("/Users/foo/project"), "/Users/foo/project");
        assert_eq!(encode_path_for_url("/Users/John Smith/My Project"), "/Users/John%20Smith/My%20Project");
        assert_eq!(encode_path_for_url("/path/with#hash"), "/path/with%23hash");
        assert_eq!(encode_path_for_url("/path/with&amp"), "/path/with%26amp");
    }

    #[test]
    fn test_find_jetbrains_project_root() {
        // No .idea anywhere — returns original path
        assert_eq!(find_jetbrains_project_root("/tmp/nonexistent/sub"), "/tmp/nonexistent/sub");

        // Root path — returns as-is
        assert_eq!(find_jetbrains_project_root("/"), "/");
    }

    #[test]
    fn test_jetbrains_url_scheme() {
        assert_eq!(jetbrains_url_scheme("PhpStorm"), Some("phpstorm"));
        assert_eq!(jetbrains_url_scheme("IntelliJ IDEA"), Some("idea"));
        assert_eq!(jetbrains_url_scheme("IntelliJ IDEA CE"), Some("idea"));
        assert_eq!(jetbrains_url_scheme("WebStorm"), Some("webstorm"));
        assert_eq!(jetbrains_url_scheme("PyCharm"), Some("pycharm"));
        assert_eq!(jetbrains_url_scheme("PyCharm CE"), Some("pycharm"));
        assert_eq!(jetbrains_url_scheme("GoLand"), Some("goland"));
        assert_eq!(jetbrains_url_scheme("CLion"), Some("clion"));
        assert_eq!(jetbrains_url_scheme("Rider"), Some("rider"));
        assert_eq!(jetbrains_url_scheme("RubyMine"), Some("rubymine"));
        assert_eq!(jetbrains_url_scheme("DataGrip"), Some("datagrip"));
        assert_eq!(jetbrains_url_scheme("Android Studio"), Some("studio"));
        assert_eq!(jetbrains_url_scheme("Aqua"), Some("aqua"));
        assert_eq!(jetbrains_url_scheme("Fleet"), Some("fleet"));
        assert_eq!(jetbrains_url_scheme("RustRover"), Some("rustrover"));
        assert_eq!(jetbrains_url_scheme("Visual Studio Code"), None);
        assert_eq!(jetbrains_url_scheme("Unknown IDE"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_get_app_name_iterm_server_process() {
        assert_eq!(
            get_app_name("/Users/user/Library/Application Support/iTerm2/iTermServer-3.6.6"),
            Some("iTerm")
        );
        assert_eq!(
            get_app_name("iTermServer-3.5.0"),
            Some("iTerm")
        );
    }
}
