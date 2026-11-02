//! c9watch core: session readers, the engine, the c9watch CLI and the
//! gateway. It never depends on tauri (D32); the Tauri GUI shell in
//! `src-tauri` is one client of it. CI fails if a `tauri*` crate enters
//! this crate's dependency tree.
//!
//! The `gui` feature builds the parts that the GUI shell calls (blocking
//! readers, subscription usage, the gateway). It adds no tauri.

pub mod actions;
pub mod claude_hooks;
pub mod claude_path;
pub mod claude_usage;
pub mod debug_log;
pub mod log_line;
pub mod session;

#[cfg(any(feature = "gui", all(unix, feature = "engine")))]
pub mod gateway;

#[cfg(feature = "gui")]
pub mod auth;

// ── CLI module ──────────────────────────────────────────────────────
#[cfg(feature = "cli")]
pub mod cli;

// ── Engine module (opt-in) ──────────────────────────────────────────
#[cfg(feature = "engine")]
pub mod engine;

// c9p/1 client side (wire types, engine home, blocking client). Not behind
// the `engine` feature: the dev channel GUI reads the engine through it.
#[cfg(all(unix, any(feature = "gui", feature = "engine")))]
pub mod engine_proto;

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
