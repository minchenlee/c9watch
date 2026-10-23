//! JSON shapes the CLI prints for sessions, history entries and search hits.

use super::read_session_tasks;
use crate::session;
use crate::session::sanitize::strip_system_tags;

// ── Session formatters ──────────────────────────────────────────────

/// Compact session: minimal fields for status polling.
pub(super) fn compact_session(s: &session::enrichment::Session) -> serde_json::Value {
    let mut json = serde_json::json!({
        "id": s.id,
        "pid": s.pid,
        "status": s.status,
        "projectPath": s.project_path,
        "sessionName": s.session_name,
    });
    insert_session_contract(&mut json, s);

    // Only include non-null optional fields
    if let Some(ref tool) = s.pending_tool_name {
        json.as_object_mut()
            .unwrap()
            .insert("pendingToolName".to_string(), serde_json::json!(tool));
    }

    json
}

/// Full session: all fields, sanitized, with null fields omitted.
pub(super) fn full_session(s: session::enrichment::Session) -> serde_json::Value {
    // Re-read full first prompt so we can sanitize BEFORE truncation.
    let first_prompt = (s.provider == session::SessionProvider::ClaudeCode)
        .then(|| find_first_prompt_raw_for_session(&s.id))
        .flatten()
        // A prompt that was entirely a system tag yields "", not the fallback.
        .map(|full| clean_first_prompt(&full).unwrap_or_default())
        .unwrap_or_else(|| {
            let cleaned = strip_system_tags(&s.first_prompt);
            cleaned.trim().to_string()
        });

    let latest_message = strip_system_tags(&s.latest_message);

    let mut json = serde_json::json!({
        "id": s.id,
        "pid": s.pid,
        "sessionName": s.session_name,
        "projectPath": s.project_path,
        "firstPrompt": first_prompt,
        "messageCount": s.message_count,
        "modified": s.modified,
        "status": s.status,
    });
    insert_session_contract(&mut json, &s);

    let obj = json.as_object_mut().unwrap();

    // Only include non-null/non-empty optional fields to save tokens
    if let Some(ref title) = s.custom_title {
        obj.insert("customTitle".to_string(), serde_json::json!(title));
    }
    if let Some(ref branch) = s.git_branch {
        obj.insert("gitBranch".to_string(), serde_json::json!(branch));
    }
    if let Some(ref summary) = s.summary {
        obj.insert("summary".to_string(), serde_json::json!(summary));
    }
    if !latest_message.trim().is_empty() {
        obj.insert(
            "latestMessage".to_string(),
            serde_json::json!(latest_message),
        );
    }
    if let Some(ref tool) = s.pending_tool_name {
        obj.insert("pendingToolName".to_string(), serde_json::json!(tool));
    }
    if let Some(input) = s.pending_tool_input {
        obj.insert("pendingToolInput".to_string(), input);
    }
    if s.provider == session::SessionProvider::ClaudeCode {
        if let Some(summary) = get_task_summary(&s.id) {
            obj.insert("taskProgress".to_string(), summary);
        }
    }

    json
}

fn insert_session_contract(json: &mut serde_json::Value, session: &session::enrichment::Session) {
    let Some(object) = json.as_object_mut() else {
        return;
    };
    object.insert(
        "sessionKey".to_string(),
        serde_json::json!(session.session_key),
    );
    object.insert("provider".to_string(), serde_json::json!(session.provider));
    object.insert("surface".to_string(), serde_json::json!(session.surface));
    object.insert("kind".to_string(), serde_json::json!(session.kind));
    if let Some(entrypoint) = &session.entrypoint {
        object.insert("entrypoint".to_string(), serde_json::json!(entrypoint));
    }
    object.insert(
        "agentKind".to_string(),
        serde_json::json!(session.agent_kind),
    );
    object.insert("canOpen".to_string(), serde_json::json!(session.can_open));
    object.insert("canStop".to_string(), serde_json::json!(session.can_stop));
    object.insert(
        "canRename".to_string(),
        serde_json::json!(session.can_rename),
    );
    if let Some(health) = &session.source_health {
        object.insert("sourceHealth".to_string(), serde_json::json!(health));
    }
    for (key, value) in [
        ("parentThreadId", session.parent_thread_id.as_ref()),
        ("rootSessionId", session.root_session_id.as_ref()),
        ("agentPath", session.agent_path.as_ref()),
        ("agentNickname", session.agent_nickname.as_ref()),
        ("agentRole", session.agent_role.as_ref()),
        ("internalKind", session.internal_kind.as_ref()),
    ] {
        if let Some(value) = value {
            object.insert(key.to_string(), serde_json::json!(value));
        }
    }
    if let Some(title) = &session.codex_title {
        object.insert("codexTitle".to_string(), serde_json::json!(title));
    }
    if let Some(title) = &session.cursor_title {
        object.insert("cursorTitle".to_string(), serde_json::json!(title));
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

pub(super) fn enrich_history_entry(entry: session::HistoryEntry) -> serde_json::Value {
    // Only Claude Code owns ~/.claude/projects/*.jsonl. Looking up a Codex or
    // Cursor ID in that namespace can return an unrelated transcript when the
    // opaque IDs happen to collide.
    let first_prompt = entry
        .provider
        .eq_ignore_ascii_case("claudeCode")
        .then(|| find_first_prompt_raw_for_session(&entry.session_id))
        .flatten()
        .and_then(|full| clean_first_prompt(&full))
        .unwrap_or_else(|| strip_system_tags(&entry.display));

    let date = chrono::DateTime::from_timestamp_millis(entry.timestamp as i64)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();

    let mut json = serde_json::json!({
        "sessionId": entry.session_id,
        "firstPrompt": first_prompt,
        "display": strip_system_tags(&entry.display),
        "date": date,
        "timestamp": entry.timestamp,
        "project": entry.project,
        "projectName": entry.project_name,
        "provider": entry.provider,
        "surface": entry.surface,
        "agentKind": entry.agent_kind,
    });

    if let Some(title) = entry.custom_title {
        json.as_object_mut()
            .unwrap()
            .insert("customTitle".to_string(), serde_json::json!(title));
    }
    if let Some(title) = entry.codex_title {
        json.as_object_mut()
            .unwrap()
            .insert("codexTitle".to_string(), serde_json::json!(title));
    }
    if let Some(title) = entry.cursor_title {
        json.as_object_mut()
            .unwrap()
            .insert("cursorTitle".to_string(), serde_json::json!(title));
    }

    json
}

pub(super) fn enrich_search_hit(hit: session::DeepSearchHit) -> serde_json::Value {
    // pi hits already carry decoded project metadata; never probe Claude's
    // project directories with a pi UUID (it can only mismatch).
    if hit.provider.eq_ignore_ascii_case("pi") {
        let project_path = hit.project_path.clone();
        let modified = hit.modified.clone();
        return format_search_hit(hit, project_path, modified);
    }
    let (project_path, modified) = if hit.provider.eq_ignore_ascii_case("codex")
        || hit.provider.eq_ignore_ascii_case("cursor")
    {
        (hit.project_path.clone(), hit.modified.clone())
    } else {
        let (project_path_encoded, modified) = find_session_metadata(&hit.session_id);

        // Decode the encoded project path back to a real filesystem path
        // e.g. "-Users-liminchen-Documents-GitHub-c9watch" -> "/Users/liminchen/Documents/GitHub/c9watch"
        (
            project_path_encoded.map(|encoded| decode_project_path(&encoded)),
            modified,
        )
    };

    format_search_hit(hit, project_path, modified)
}

pub(super) fn search_hit_matches_project(
    hit: &serde_json::Value,
    project_filter: Option<&str>,
) -> bool {
    match project_filter {
        Some(filter) => hit
            .get("projectPath")
            .and_then(|path| path.as_str())
            .map(|path| path.contains(filter))
            .unwrap_or(false),
        None => true,
    }
}

fn format_search_hit(
    hit: session::DeepSearchHit,
    project_path: Option<String>,
    modified: Option<String>,
) -> serde_json::Value {
    serde_json::json!({
        "sessionId": hit.session_id,
        "snippet": strip_system_tags(&hit.snippet),
        "projectPath": project_path,
        "modified": modified,
        "provider": hit.provider,
        "surface": hit.surface,
        "agentKind": hit.agent_kind,
    })
}

/// Decode Claude Code's encoded project directory name back to a real path.
/// Claude Code encodes paths by replacing every non-alphanumeric char with `-`.
///
/// Strategy: try sessions-index.json first (authoritative), then use the active
/// session's CWD from enrichment data, then fall back to heuristic decoding.
fn decode_project_path(encoded: &str) -> String {
    // 1. Try sessions-index.json
    if let Some(home_dir) = dirs::home_dir() {
        let project_dir = home_dir.join(".claude").join("projects").join(encoded);
        let index_path = project_dir.join("sessions-index.json");
        if let Ok(index) = session::parse_sessions_index(&index_path) {
            if let Some(entry) = index.entries.first() {
                return entry.project_path.to_string_lossy().to_string();
            }
        }
    }

    // 2. Heuristic: the encoded form is a path with non-alphanumeric chars replaced by `-`.
    //    We can't perfectly reverse it, but common patterns work:
    //    "-Users-liminchen-Documents-GitHub-c9watch" -> "/Users/liminchen/Documents/GitHub/c9watch"
    //    This works because most path components are alphanumeric.
    if encoded.starts_with('-') {
        let decoded = encoded.replacen('-', "/", 1); // First `-` -> `/`
                                                     // Replace remaining `-` with `/`, but this is lossy for hyphenated dirs
        return decoded.replace('-', "/");
    }

    encoded.to_string()
}

fn find_first_prompt_raw_for_session(session_id: &str) -> Option<String> {
    let session_file = session::find_claude_session_file(session_id)?;
    session::enrichment::get_first_prompt_from_jsonl_raw(&session_file)
}

/// Strip system tags from a raw first prompt, trim it and truncate it to 100
/// characters. Returns `None` when nothing is left.
fn clean_first_prompt(raw: &str) -> Option<String> {
    let clean = strip_system_tags(raw);
    let trimmed = clean.trim();
    (!trimmed.is_empty()).then(|| session::enrichment::truncate_string(trimmed, 100))
}

fn find_session_metadata(session_id: &str) -> (Option<String>, Option<String>) {
    let Some(session_file) = session::find_claude_session_file(session_id) else {
        return (None, None);
    };
    let project_path = session_file
        .parent()
        .and_then(|dir| dir.file_name())
        .and_then(|n| n.to_str())
        .map(|s| s.to_string());

    let modified = std::fs::metadata(&session_file)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| {
            let datetime: chrono::DateTime<chrono::Utc> = t.into();
            datetime.to_rfc3339()
        });

    (project_path, modified)
}

fn get_task_summary(session_id: &str) -> Option<serde_json::Value> {
    let tasks = read_session_tasks(session_id).ok()?;
    if tasks.is_empty() {
        return None;
    }

    let total = tasks.len();
    let completed = tasks
        .iter()
        .filter(|t| t.get("status").and_then(|s| s.as_str()) == Some("completed"))
        .count();
    let in_progress = tasks
        .iter()
        .filter(|t| t.get("status").and_then(|s| s.as_str()) == Some("in_progress"))
        .count();

    let current_task = tasks.iter().find_map(|t| {
        if t.get("status").and_then(|s| s.as_str()) == Some("in_progress") {
            t.get("subject").and_then(|s| s.as_str()).map(String::from)
        } else {
            None
        }
    });

    Some(serde_json::json!({
        "total": total,
        "completed": completed,
        "inProgress": in_progress,
        "currentTask": current_task,
    }))
}

#[cfg(test)]
mod session_formatter_tests {
    use super::*;
    use crate::cli::cmd_search_output;
    use crate::session::source::{AgentKind, SessionKind, SessionProvider, SessionSurface};
    use crate::session::SessionStatus;

    #[test]
    fn clean_first_prompt_system_tag_only_is_none() {
        assert_eq!(
            clean_first_prompt("<system-reminder>internal</system-reminder>"),
            None
        );
    }

    #[test]
    fn clean_first_prompt_whitespace_only_is_none() {
        assert_eq!(clean_first_prompt("  \n\t "), None);
    }

    #[test]
    fn clean_first_prompt_trims_normal_prompt() {
        assert_eq!(
            clean_first_prompt("  fix the bug  ").as_deref(),
            Some("fix the bug")
        );
    }

    #[test]
    fn clean_first_prompt_truncates_long_prompt_at_100_chars() {
        let long = "a".repeat(150);
        assert_eq!(
            clean_first_prompt(&long),
            Some(format!("{}...", "a".repeat(100)))
        );
        let exact = "b".repeat(100);
        assert_eq!(clean_first_prompt(&exact), Some(exact.clone()));
    }

    fn codex_subagent() -> session::enrichment::Session {
        session::enrichment::Session {
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
            source_health: None,
            notification_preview: None,
            latest_message: String::new(),
            pending_tool_name: None,
            pending_tool_input: None,
            waiting_for: None,
            last_assistant_message: None,
            transcript_path: None,
            worker_of: None,
            official_name: None,
            started_at_ms: Some(1_752_364_800_000),
            provider: SessionProvider::Codex,
            surface: SessionSurface::App,
            kind: SessionKind::Interactive,
            entrypoint: None,
            agent_kind: AgentKind::Subagent,
            parent_thread_id: Some("parent-thread".to_string()),
            root_session_id: Some("root-thread".to_string()),
            agent_path: Some("/root/investigator".to_string()),
            agent_nickname: Some("Scout".to_string()),
            agent_role: Some("investigator".to_string()),
            internal_kind: Some("spawned".to_string()),
            open_target: None,
            can_open: false,
            can_stop: false,
            can_rename: false,
        }
    }

    #[test]
    fn session_formatters_preserve_kind_and_optional_raw_entrypoint() {
        for (kind, expected) in [
            (SessionKind::Interactive, "interactive"),
            (SessionKind::Background, "background"),
            (SessionKind::Unknown, "unknown"),
        ] {
            let mut session = codex_subagent();
            session.kind = kind;
            session.entrypoint = Some("sdk-cli".to_string());
            for value in [compact_session(&session), full_session(session.clone())] {
                assert_eq!(value["kind"], expected);
                assert_eq!(value["entrypoint"], "sdk-cli");
            }
            session.entrypoint = None;
            assert!(compact_session(&session).get("entrypoint").is_none());
            assert!(full_session(session).get("entrypoint").is_none());
        }
    }

    fn assert_codex_contract(value: &serde_json::Value) {
        assert_eq!(value["sessionKey"], "codex:child-thread");
        assert_eq!(value["codexTitle"], "Codex thread title");
        assert_eq!(value["provider"], "codex");
        assert_eq!(value["surface"], "app");
        assert_eq!(value["agentKind"], "subagent");
        assert_eq!(value["parentThreadId"], "parent-thread");
        assert_eq!(value["rootSessionId"], "root-thread");
        assert_eq!(value["agentPath"], "/root/investigator");
        assert_eq!(value["agentNickname"], "Scout");
        assert_eq!(value["agentRole"], "investigator");
        assert_eq!(value["internalKind"], "spawned");
        assert_eq!(value["canOpen"], false);
        assert_eq!(value["canStop"], false);
        assert_eq!(value["canRename"], false);
    }

    #[test]
    fn search_hit_includes_provider_surface_and_agent_kind() {
        let value = enrich_search_hit(session::DeepSearchHit {
            session_id: "search-session".to_string(),
            snippet: "matching text".to_string(),
            project_path: Some("/tmp/codex-project".to_string()),
            modified: Some("2026-07-13T02:03:04+00:00".to_string()),
            provider: "codex".to_string(),
            surface: Some("cli".to_string()),
            agent_kind: "subagent".to_string(),
        });

        assert_eq!(value["provider"], "codex");
        assert_eq!(value["surface"], "cli");
        assert_eq!(value["agentKind"], "subagent");
    }

    #[test]
    fn codex_rollout_search_wiring_attaches_metadata_and_filters_project() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join(".codex/sessions/2026/07/13");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("rollout-codex-search.jsonl"),
            concat!(
                r#"{"timestamp":"2026-07-13T02:03:04Z","type":"session_meta","payload":{"id":"codex-search","cwd":"/tmp/codex-project","source":"cli","originator":"codex-tui"}}"#,
                "\n",
                r#"{"timestamp":"2026-07-13T02:03:04Z","type":"event_msg","payload":{"type":"user_message","message":"search wiring marker"}}"#,
            ),
        )
        .unwrap();

        let matching = cmd_search_output(
            home.path(),
            "search wiring marker",
            Some("codex-project"),
            20,
            false,
            false,
        )
        .unwrap();
        let hits = matching["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["sessionId"], "codex-search");
        assert_eq!(hits[0]["projectPath"], "/tmp/codex-project");
        assert_eq!(hits[0]["modified"], "2026-07-13T02:03:04+00:00");

        let nonmatching = cmd_search_output(
            home.path(),
            "search wiring marker",
            Some("other-project"),
            20,
            false,
            false,
        )
        .unwrap();
        assert!(nonmatching["hits"].as_array().unwrap().is_empty());
    }

    #[test]
    fn cursor_search_wiring_preserves_metadata_and_filters_project() {
        let home = tempfile::tempdir().unwrap();
        let session_id = "dddddddd-dddd-dddd-dddd-dddddddddddd";
        let project_path = home.path().join("cursor-project");
        std::fs::create_dir_all(&project_path).unwrap();
        let project_key = format!(
            "{}-cursor-project",
            home.path()
                .strip_prefix("/")
                .unwrap()
                .to_string_lossy()
                .replace('/', "-")
        );
        let transcript_dir = home
            .path()
            .join(".cursor/projects")
            .join(project_key)
            .join("agent-transcripts")
            .join(session_id);
        std::fs::create_dir_all(&transcript_dir).unwrap();
        std::fs::write(
            transcript_dir.join(format!("{session_id}.jsonl")),
            r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>cursor search wiring marker</user_query>"}]}}
"#,
        )
        .unwrap();

        let matching = cmd_search_output(
            home.path(),
            "cursor search wiring marker",
            Some("cursor-project"),
            20,
            false,
            false,
        )
        .unwrap();
        let hits = matching["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["sessionId"], session_id);
        assert_eq!(hits[0]["provider"], "cursor");
        assert_eq!(hits[0]["surface"], "cursor");
        assert!(hits[0]["projectPath"]
            .as_str()
            .unwrap()
            .ends_with("cursor-project"));

        let nonmatching = cmd_search_output(
            home.path(),
            "cursor search wiring marker",
            Some("other-project"),
            20,
            false,
            false,
        )
        .unwrap();
        assert!(nonmatching["hits"].as_array().unwrap().is_empty());
    }

    #[test]
    fn compact_session_includes_provider_hierarchy_and_capabilities() {
        assert_codex_contract(&compact_session(&codex_subagent()));
    }

    #[test]
    fn full_session_includes_provider_hierarchy_and_capabilities() {
        assert_codex_contract(&full_session(codex_subagent()));
    }
}
