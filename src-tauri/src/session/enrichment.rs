use crate::session::cache::FileVersion;
use crate::session::codex::CodexLifecycle;
use crate::session::cursor::CursorLifecycle;
use crate::session::owners::global_provider_source_owners;
use crate::session::pi::PiLifecycle;
use crate::session::source::{
    AgentKind, CliActivity, DetectedSession, DetectionDiagnostics, SessionIdentity,
    SessionProvider, SessionSource, SessionSurface,
};
use crate::session::{
    determine_status, get_pending_tool_input, get_pending_tool_name, parse_last_n_entries,
    parse_sessions_index, SessionStatus,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::{LazyLock, Mutex};

/// Combined session information
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    /// Provider-scoped identity used by frontend selection and backend maps.
    pub session_key: String,
    pub pid: u32,
    pub session_name: String,
    pub custom_title: Option<String>,
    /// Codex's current UI thread name from ~/.codex/session_index.jsonl.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_title: Option<String>,
    /// Cursor's current UI composer name from state.vscdb.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_title: Option<String>,
    pub project_path: String,
    pub git_branch: Option<String>,
    pub first_prompt: String,
    pub summary: Option<String>,
    pub message_count: u32,
    pub modified: String,
    pub status: SessionStatus,
    pub latest_message: String,
    pub pending_tool_name: Option<String>,
    /// The input/arguments of the pending tool (when status is NeedsPermission)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_tool_input: Option<serde_json::Value>,
    /// Session ID of the PM that spawned this session (if it's a c9watch worker).
    /// Populated in polling.rs by overlay from `~/.claude/c9watch/workers/*/meta.json`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_of: Option<String>,
    /// User-set name from `claude agents` (Ctrl+T background-pinned sessions).
    /// Only populated by the CLI backend; legacy backend leaves this None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub official_name: Option<String>,
    /// Session start time in ms since epoch (from `claude agents --json`).
    /// Only populated by the CLI backend; legacy backend leaves this None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    pub provider: SessionProvider,
    pub surface: SessionSurface,
    pub agent_kind: AgentKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub internal_kind: Option<String>,
    pub can_open: bool,
    pub can_stop: bool,
    pub can_rename: bool,
}

/// Cache for native custom titles, keyed by file path.
/// The stamp includes size and, on Unix, change time and inode.  Mtime alone
/// can be coarse or unchanged for an in-place rewrite, which would otherwise
/// let a stale title survive in the history view.
type NativeTitleStamp = FileVersion;

/// Stores the file stamp and cached title to avoid re-scanning JSONL files
/// every poll cycle while still falling back safely on weak metadata systems.
static NATIVE_TITLE_CACHE: LazyLock<
    Mutex<HashMap<std::path::PathBuf, (NativeTitleStamp, Option<String>)>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn native_title_stamp(path: &Path) -> Option<NativeTitleStamp> {
    FileVersion::read(path).ok()
}

fn claude_custom_name(
    provider: SessionProvider,
    names: &crate::session::CustomNames,
    session_id: &str,
) -> Option<String> {
    (provider == SessionProvider::ClaudeCode)
        .then(|| names.get(session_id).cloned())
        .flatten()
}

fn claude_custom_title(
    provider: SessionProvider,
    titles: &crate::session::CustomTitles,
    session_id: &str,
) -> Option<String> {
    (provider == SessionProvider::ClaudeCode)
        .then(|| titles.get(session_id).cloned())
        .flatten()
}

/// Look up the native custom title for a session JSONL, using a strong file
/// stamp when the platform provides one and a safe re-scan otherwise.
pub(crate) fn get_cached_native_title(path: &Path) -> Option<String> {
    let Some(stamp) = native_title_stamp(path) else {
        if let Ok(mut cache) = NATIVE_TITLE_CACHE.lock() {
            cache.remove(path);
        }
        return None;
    };

    if let Ok(mut cache) = NATIVE_TITLE_CACHE.lock() {
        if let Some((cached_stamp, cached_title)) = cache.get(path) {
            if *cached_stamp == stamp && stamp.supports_unchanged_fast_path() {
                return cached_title.clone();
            }
        }
        // Cache miss or stale — re-scan
        let title = crate::session::parser::get_native_custom_title_from_file(path);
        cache.insert(path.to_path_buf(), (stamp, title.clone()));
        title
    } else {
        // Mutex poisoned — fallback to direct read
        crate::session::parser::get_native_custom_title_from_file(path)
    }
}

/// Combines the existing heuristic-based SessionStatus with the CLI activity signal.
///
/// CLI activity NEVER overrides NeedsAttention or Connecting — those stay driven by
/// JSONL content + PermissionChecker. It only refines Working vs WaitingForInput.
pub fn merge_cli_activity(
    heuristic: SessionStatus,
    cli: Option<CliActivity>,
    has_pending_tool: bool,
) -> SessionStatus {
    match (heuristic, cli) {
        (SessionStatus::NeedsAttention, _) => SessionStatus::NeedsAttention,
        (SessionStatus::Connecting, _) => SessionStatus::Connecting,
        (heuristic, None) => heuristic,
        (SessionStatus::WaitingForInput, Some(CliActivity::Busy)) => SessionStatus::Working,
        (SessionStatus::Working, Some(CliActivity::Idle)) if !has_pending_tool => {
            SessionStatus::WaitingForInput
        }
        (heuristic, _) => heuristic,
    }
}

/// Detect sessions and enrich them with status and conversation data.
/// Uses the configured backend (CLI or Legacy) via the factory.
pub fn detect_and_enrich_sessions() -> Result<(Vec<Session>, DetectionDiagnostics), String> {
    let mut source = crate::session::create_session_source();
    let claude_result = source.detect();
    let owners = global_provider_source_owners();
    let codex_result = owners.detect_codex();
    let cursor_result = owners.detect_cursor();
    let pi_result = owners.detect_pi();
    match claude_result {
        Ok((mut detected, diagnostics)) => {
            if let Some((mut codex_sessions, _)) = codex_result {
                detected.append(&mut codex_sessions);
            }
            if let Some((mut cursor_sessions, _)) = cursor_result {
                detected.append(&mut cursor_sessions);
            }
            if let Some((mut pi_sessions, _)) = pi_result {
                detected.append(&mut pi_sessions);
            }
            enrich_detected_sessions(detected, diagnostics)
        }
        Err(error) => {
            let mut fallback = Vec::new();
            let mut diagnostics = DetectionDiagnostics::default();
            if let Some((sessions, extra)) = codex_result {
                fallback.extend(sessions);
                diagnostics = extra;
            }
            if let Some((sessions, extra)) = cursor_result {
                fallback.extend(sessions);
                if diagnostics.claude_processes_found == 0 {
                    diagnostics = extra;
                }
            }
            if let Some((sessions, extra)) = pi_result {
                fallback.extend(sessions);
                if diagnostics.claude_processes_found == 0 {
                    diagnostics = extra;
                }
            }
            if !fallback.is_empty() {
                return enrich_detected_sessions(fallback, diagnostics);
            }
            Err(format!("Failed to detect sessions: {error}"))
        }
    }
}

/// Detect sessions using a SessionSource trait object and enrich them.
pub fn detect_and_enrich_sessions_with_source(
    source: &mut dyn SessionSource,
) -> Result<(Vec<Session>, DetectionDiagnostics), String> {
    let (detected, diag) = source
        .detect()
        .map_err(|e| format!("Failed to detect sessions: {}", e))?;
    enrich_detected_sessions(detected, diag)
}

/// Enrichment-only: takes already-detected sessions and adds JSONL-derived fields.
/// Split out so callers can drop a lock before doing slow JSONL parsing.
pub fn enrich_detected_sessions(
    detected_sessions: Vec<DetectedSession>,
    diagnostics: DetectionDiagnostics,
) -> Result<(Vec<Session>, DetectionDiagnostics), String> {
    let custom_names = crate::session::CustomNames::load();
    let custom_titles = crate::session::CustomTitles::load();
    let mut sessions = Vec::new();
    let mut seen_ids: HashSet<SessionIdentity> = HashSet::new();

    for detected in detected_sessions {
        // Get session ID - if not found, skip this session
        let identity = match detected.identity() {
            Some(identity) => identity,
            None => {
                continue;
            }
        };
        let session_id = identity.session_id.clone();

        // Skip duplicate provider-scoped identities (the same session can
        // appear in multiple project dirs without collapsing other providers).
        if !seen_ids.insert(identity) {
            continue;
        }

        if let Some(summary) = detected.codex_summary.as_ref() {
            let first_prompt = summary
                .first_prompt()
                .map(|prompt| truncate_string(prompt, 100))
                .unwrap_or_else(|| "(No conversation yet)".to_string());
            let latest_message = summary
                .latest_message()
                .map(|message| truncate_string(message, 200))
                .unwrap_or_default();
            let session_name = claude_custom_name(detected.provider, &custom_names, &session_id)
                .or_else(|| detected.agent_nickname.clone())
                .unwrap_or_else(|| detected.project_name.clone());
            let codex_title = crate::session::codex::get_cached_codex_thread_title(&session_id);
            let modified = if summary.last_timestamp.is_empty() {
                detected
                    .started_at_ms
                    .and_then(DateTime::<Utc>::from_timestamp_millis)
                    .map(|timestamp| timestamp.to_rfc3339())
                    .unwrap_or_default()
            } else {
                summary.last_timestamp.clone()
            };
            sessions.push(Session {
                id: session_id.clone(),
                session_key: SessionIdentity::new(detected.provider, session_id.clone()).key(),
                pid: detected.pid,
                session_name,
                custom_title: claude_custom_title(detected.provider, &custom_titles, &session_id),
                codex_title,
                cursor_title: None,
                project_path: detected.cwd.to_string_lossy().to_string(),
                git_branch: None,
                first_prompt,
                summary: None,
                message_count: summary.messages.len() as u32,
                modified,
                status: if summary.lifecycle == CodexLifecycle::Working {
                    SessionStatus::Working
                } else {
                    SessionStatus::WaitingForInput
                },
                latest_message,
                pending_tool_name: None,
                pending_tool_input: None,
                worker_of: None,
                official_name: detected.official_name.clone(),
                started_at_ms: detected.started_at_ms,
                provider: detected.provider,
                surface: detected.surface,
                agent_kind: detected.agent_kind,
                parent_thread_id: detected.parent_thread_id.clone(),
                root_session_id: detected.root_session_id.clone(),
                agent_path: detected.agent_path.clone(),
                agent_nickname: detected.agent_nickname.clone(),
                agent_role: detected.agent_role.clone(),
                internal_kind: detected.internal_kind.clone(),
                can_open: detected.can_open,
                can_stop: detected.can_stop,
                can_rename: detected.can_rename,
            });
            continue;
        }

        if let Some(summary) = detected.cursor_summary.as_ref() {
            let first_prompt = summary
                .first_prompt()
                .map(|prompt| truncate_string(prompt, 100))
                .unwrap_or_else(|| "(No conversation yet)".to_string());
            let latest_message = summary
                .latest_message()
                .map(|message| truncate_string(message, 200))
                .unwrap_or_default();
            let session_name = claude_custom_name(detected.provider, &custom_names, &session_id)
                .or_else(|| detected.agent_nickname.clone())
                .unwrap_or_else(|| detected.project_name.clone());
            let modified = if summary.last_timestamp.is_empty() {
                detected
                    .started_at_ms
                    .and_then(DateTime::<Utc>::from_timestamp_millis)
                    .map(|timestamp| timestamp.to_rfc3339())
                    .unwrap_or_default()
            } else {
                summary.last_timestamp.clone()
            };
            let status = if summary.lifecycle == CursorLifecycle::Working {
                if summary.empty {
                    SessionStatus::Connecting
                } else {
                    SessionStatus::Working
                }
            } else {
                SessionStatus::WaitingForInput
            };
            sessions.push(Session {
                id: session_id.clone(),
                session_key: SessionIdentity::new(detected.provider, session_id.clone()).key(),
                pid: detected.pid,
                session_name,
                custom_title: claude_custom_title(detected.provider, &custom_titles, &session_id)
                    .or_else(|| detected.official_name.clone()),
                codex_title: None,
                cursor_title: summary.cursor_title.clone(),
                project_path: detected.cwd.to_string_lossy().to_string(),
                git_branch: None,
                first_prompt,
                summary: None,
                message_count: summary.messages.len() as u32,
                modified,
                status,
                latest_message,
                pending_tool_name: None,
                pending_tool_input: None,
                worker_of: None,
                official_name: detected.official_name.clone(),
                started_at_ms: detected.started_at_ms,
                provider: detected.provider,
                surface: detected.surface,
                agent_kind: detected.agent_kind,
                parent_thread_id: detected.parent_thread_id.clone(),
                root_session_id: detected.root_session_id.clone(),
                agent_path: detected.agent_path.clone(),
                agent_nickname: detected.agent_nickname.clone(),
                agent_role: detected.agent_role.clone(),
                internal_kind: detected.internal_kind.clone(),
                can_open: detected.can_open,
                can_stop: detected.can_stop,
                can_rename: detected.can_rename,
            });
            continue;
        }

        if let Some(summary) = detected.pi_summary.as_ref() {
            let first_prompt = summary
                .first_prompt
                .as_deref()
                .map(|prompt| truncate_string(prompt, 100))
                .filter(|prompt| !prompt.trim().is_empty())
                .unwrap_or_else(|| "(No conversation yet)".to_string());
            let latest_message = summary
                .latest_snippet
                .as_deref()
                .map(|message| truncate_string(message, 200))
                .unwrap_or_default();
            let session_name = claude_custom_name(detected.provider, &custom_names, &session_id)
                .or_else(|| detected.agent_nickname.clone())
                .unwrap_or_else(|| detected.project_name.clone());
            let modified = if summary.last_timestamp.is_empty() {
                detected
                    .started_at_ms
                    .and_then(DateTime::<Utc>::from_timestamp_millis)
                    .map(|timestamp| timestamp.to_rfc3339())
                    .unwrap_or_default()
            } else {
                summary.last_timestamp.clone()
            };
            let status = if summary.lifecycle == PiLifecycle::Working {
                if summary.empty {
                    SessionStatus::Connecting
                } else {
                    SessionStatus::Working
                }
            } else {
                SessionStatus::WaitingForInput
            };
            sessions.push(Session {
                id: session_id.clone(),
                session_key: SessionIdentity::new(detected.provider, session_id.clone()).key(),
                pid: detected.pid,
                session_name,
                custom_title: claude_custom_title(detected.provider, &custom_titles, &session_id),
                codex_title: None,
                cursor_title: None,
                project_path: detected.cwd.to_string_lossy().to_string(),
                git_branch: None,
                first_prompt,
                summary: None,
                message_count: summary.message_count as u32,
                modified,
                status,
                latest_message,
                pending_tool_name: summary.pending_tool_name.clone(),
                pending_tool_input: None,
                worker_of: None,
                official_name: detected.official_name.clone(),
                started_at_ms: detected.started_at_ms,
                provider: detected.provider,
                surface: detected.surface,
                agent_kind: detected.agent_kind,
                parent_thread_id: detected.parent_thread_id.clone(),
                root_session_id: detected.root_session_id.clone(),
                agent_path: detected.agent_path.clone(),
                agent_nickname: detected.agent_nickname.clone(),
                agent_role: detected.agent_role.clone(),
                internal_kind: detected.internal_kind.clone(),
                can_open: detected.can_open,
                can_stop: detected.can_stop,
                can_rename: detected.can_rename,
            });
            continue;
        }

        // Try to parse sessions-index.json to get basic info (optional)
        let index_path = detected.project_path.join("sessions-index.json");
        let sessions_index = parse_sessions_index(&index_path).ok();

        // Find the matching entry in the index (if index exists)
        let session_entry = sessions_index.as_ref().and_then(|index| {
            index
                .entries
                .iter()
                .find(|entry| entry.session_id == session_id)
        });

        let (first_prompt, summary, message_count, modified, git_branch) = match session_entry {
            Some(entry) => {
                // Guard: if sessions-index first_prompt is a system command, try JSONL fallback
                let fp = if crate::session::parser::is_system_content(&entry.first_prompt) {
                    let session_file_path =
                        detected.project_path.join(format!("{}.jsonl", session_id));
                    get_first_prompt_from_jsonl(&session_file_path)
                        .unwrap_or_else(|| entry.first_prompt.clone())
                } else {
                    entry.first_prompt.clone()
                };
                (
                    fp,
                    entry.summary.clone(),
                    entry.message_count,
                    entry.modified.clone(),
                    Some(entry.git_branch.clone()),
                )
            }
            None => {
                // Session not in index or index doesn't exist - use fallback values
                let session_file_path = detected.project_path.join(format!("{}.jsonl", session_id));

                // Try to get first prompt from JSONL file
                let first_prompt =
                    get_first_prompt_from_jsonl(&session_file_path).unwrap_or_else(|| {
                        if detected.started_at_ms.is_some() {
                            "(No conversation yet)".to_string()
                        } else {
                            "(Active session)".to_string()
                        }
                    });

                // Count messages in the file
                let message_count = count_messages_in_jsonl(&session_file_path);

                // Get file modification time. Fall back to started_at_ms for
                // CLI-sourced placeholder sessions (no JSONL yet) so the frontend
                // doesn't choke on `new Date("").getTime()` → NaN when sorting.
                let modified = std::fs::metadata(&session_file_path)
                    .and_then(|m| m.modified())
                    .ok()
                    .map(|t| {
                        let datetime: DateTime<Utc> = t.into();
                        datetime.to_rfc3339()
                    })
                    .or_else(|| {
                        detected.started_at_ms.and_then(|ms| {
                            DateTime::<Utc>::from_timestamp_millis(ms).map(|dt| dt.to_rfc3339())
                        })
                    })
                    .unwrap_or_default();

                (first_prompt, None, message_count, modified, None)
            }
        };

        // Parse the session JSONL file to determine status and get latest message
        let session_file_path = detected.project_path.join(format!("{}.jsonl", session_id));
        let entries = match parse_last_n_entries(&session_file_path, 20) {
            Ok(entries) => entries,
            Err(e) => {
                crate::debug_log::log_warn(&format!(
                    "Failed to parse session file for {}: {}",
                    session_id, e
                ));
                vec![]
            }
        };

        let pending_tool_name = get_pending_tool_name(&entries);

        let heuristic_status = if entries.is_empty() {
            SessionStatus::Connecting
        } else {
            let raw_status = determine_status(&entries);
            // Override WaitingForInput if the JSONL file was recently modified.
            if raw_status == SessionStatus::WaitingForInput
                && is_file_recently_modified(&session_file_path, 8)
            {
                SessionStatus::Working
            } else {
                raw_status
            }
        };
        let status = merge_cli_activity(
            heuristic_status,
            detected.cli_activity,
            pending_tool_name.is_some(),
        );

        let latest_message = get_latest_message_from_entries(&entries);
        let pending_tool_input = get_pending_tool_input(&entries);

        // Skip empty sessions (0 messages) UNLESS CLI-sourced — `claude agents --json`
        // confirms the agent is live even when no project JSONL exists (SDK-based
        // interactive sessions never write to ~/.claude/projects/). Render a
        // placeholder card so the dashboard count matches `claude agents --json`.
        let cli_sourced = detected.started_at_ms.is_some();
        if message_count == 0 && !cli_sourced {
            continue;
        }

        // Use custom name if available, otherwise use detected project name
        let session_name = claude_custom_name(detected.provider, &custom_names, &session_id)
            .unwrap_or(detected.project_name);

        // Get custom title: Claude Code native /rename takes priority over c9watch's own.
        // Uses a static cache keyed by path plus a strong file stamp when available.
        let native_title = get_cached_native_title(&session_file_path);
        let custom_title = native_title
            .or_else(|| claude_custom_title(detected.provider, &custom_titles, &session_id));

        sessions.push(Session {
            id: session_id.clone(),
            session_key: SessionIdentity::new(detected.provider, session_id.clone()).key(),
            pid: detected.pid,
            session_name,
            custom_title,
            codex_title: None,
            cursor_title: None,
            project_path: detected.cwd.to_string_lossy().to_string(),
            git_branch,
            first_prompt,
            summary,
            message_count,
            modified,
            status,
            latest_message,
            pending_tool_name,
            pending_tool_input,
            worker_of: None,
            official_name: detected.official_name.clone(),
            started_at_ms: detected.started_at_ms,
            provider: detected.provider,
            surface: detected.surface,
            agent_kind: detected.agent_kind,
            parent_thread_id: detected.parent_thread_id.clone(),
            root_session_id: detected.root_session_id.clone(),
            agent_path: detected.agent_path.clone(),
            agent_nickname: detected.agent_nickname.clone(),
            agent_role: detected.agent_role.clone(),
            internal_kind: detected.internal_kind.clone(),
            can_open: detected.can_open,
            can_stop: detected.can_stop,
            can_rename: detected.can_rename,
        });
    }

    Ok((sessions, diagnostics))
}

/// Checks if a file was modified within the last N seconds
pub fn is_file_recently_modified(path: &Path, seconds: u64) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .map(|modified| {
            modified
                .elapsed()
                .map(|elapsed| elapsed.as_secs() < seconds)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// Extract the first user prompt from a session JSONL file (truncated to 100 chars).
pub fn get_first_prompt_from_jsonl(path: &Path) -> Option<String> {
    get_first_prompt_from_jsonl_raw(path).map(|s| truncate_string(&s, 100))
}

/// Extract the first user prompt from a session JSONL file (full text, no truncation).
pub fn get_first_prompt_from_jsonl_raw(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines().map_while(Result::ok).take(50) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
            if value.get("type").and_then(|t| t.as_str()) == Some("user") {
                if let Some(message) = value.get("message") {
                    if let Some(content) = message.get("content") {
                        if let Some(text) = content.as_str() {
                            // Skip system-generated local command messages
                            if crate::session::parser::is_system_content(text) {
                                continue;
                            }
                            return Some(text.to_string());
                        } else if let Some(arr) = content.as_array() {
                            for item in arr {
                                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                        return Some(text.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

/// Truncate a string to a maximum length (character-safe for UTF-8)
pub fn truncate_string(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}

/// Extract the latest message content from session entries
pub fn get_latest_message_from_entries(entries: &[crate::session::parser::SessionEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }

    for entry in entries.iter().rev() {
        match entry {
            crate::session::parser::SessionEntry::User { message, .. } => {
                // Skip tool result entries and system-generated command messages
                if message.is_tool_result
                    || crate::session::parser::is_system_content(&message.content)
                {
                    continue;
                }
                return truncate_string(&message.content, 200);
            }
            crate::session::parser::SessionEntry::Assistant { message, .. } => {
                for content in message.content.iter().rev() {
                    match content {
                        crate::session::parser::MessageContent::Text { text } => {
                            return truncate_string(text, 200);
                        }
                        crate::session::parser::MessageContent::Thinking { thinking, .. } => {
                            return truncate_string(thinking, 200);
                        }
                        crate::session::parser::MessageContent::ToolUse { name, .. } => {
                            return format!("Executing {}...", name);
                        }
                        _ => continue,
                    }
                }
            }
            _ => continue,
        }
    }

    String::new()
}

/// Count user/assistant messages in a JSONL file.
/// Skips system-injected user messages (local commands, slash commands, etc.)
pub fn count_messages_in_jsonl(path: &Path) -> u32 {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    let reader = BufReader::new(file);
    let mut count = 0u32;

    for line in reader.lines().map_while(Result::ok) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(msg_type) = value.get("type").and_then(|t| t.as_str()) {
                match msg_type {
                    "assistant" => count += 1,
                    "user" => {
                        // Skip system-injected user messages
                        if let Some(content) = value
                            .get("message")
                            .and_then(|m| m.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            if !crate::session::parser::is_system_content(content) {
                                count += 1;
                            }
                        } else {
                            // Array content (tool results) — still count them
                            count += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_string_no_truncation() {
        assert_eq!(truncate_string("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_string_exact_boundary() {
        assert_eq!(truncate_string("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_string_over_boundary() {
        assert_eq!(truncate_string("hello world", 5), "hello...");
    }

    #[test]
    fn test_truncate_string_empty() {
        assert_eq!(truncate_string("", 10), "");
    }

    #[test]
    fn test_truncate_string_zero_max() {
        assert_eq!(truncate_string("hello", 0), "...");
        assert!(!truncate_string("hello", 0).contains('h'));
    }

    #[test]
    fn test_truncate_string_single_char_limit() {
        assert_eq!(truncate_string("hello", 1), "h...");
    }

    #[test]
    fn test_truncate_string_utf8_accented() {
        assert_eq!(truncate_string("héllo", 3), "hél...");
    }

    #[test]
    fn test_truncate_string_utf8_cjk() {
        assert_eq!(truncate_string("你好世界", 2), "你好...");
    }

    #[test]
    fn test_truncate_string_utf8_emoji() {
        assert_eq!(truncate_string("Hello 👋 World", 7), "Hello 👋...");
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;
    use crate::session::source::CliActivity;
    use crate::session::SessionStatus;

    #[test]
    fn merge_preserves_needs_attention_when_busy() {
        let merged = merge_cli_activity(
            SessionStatus::NeedsAttention,
            Some(CliActivity::Busy),
            false,
        );
        assert_eq!(merged, SessionStatus::NeedsAttention);
    }

    #[test]
    fn merge_preserves_needs_attention_when_idle() {
        let merged = merge_cli_activity(
            SessionStatus::NeedsAttention,
            Some(CliActivity::Idle),
            false,
        );
        assert_eq!(merged, SessionStatus::NeedsAttention);
    }

    #[test]
    fn merge_preserves_connecting() {
        let merged = merge_cli_activity(SessionStatus::Connecting, Some(CliActivity::Busy), false);
        assert_eq!(merged, SessionStatus::Connecting);
    }

    #[test]
    fn merge_busy_promotes_waiting_to_working() {
        let merged = merge_cli_activity(
            SessionStatus::WaitingForInput,
            Some(CliActivity::Busy),
            false,
        );
        assert_eq!(merged, SessionStatus::Working);
    }

    #[test]
    fn merge_idle_downgrades_working_without_pending_tool() {
        let merged = merge_cli_activity(SessionStatus::Working, Some(CliActivity::Idle), false);
        assert_eq!(merged, SessionStatus::WaitingForInput);
    }

    #[test]
    fn merge_idle_keeps_working_when_pending_tool() {
        let merged = merge_cli_activity(SessionStatus::Working, Some(CliActivity::Idle), true);
        assert_eq!(merged, SessionStatus::Working);
    }

    #[test]
    fn merge_none_returns_heuristic_unchanged() {
        let merged = merge_cli_activity(SessionStatus::Working, None, false);
        assert_eq!(merged, SessionStatus::Working);
        let merged = merge_cli_activity(SessionStatus::WaitingForInput, None, true);
        assert_eq!(merged, SessionStatus::WaitingForInput);
    }
}

#[cfg(test)]
mod placeholder_tests {
    use super::*;
    use crate::session::source::{DetectedSession, DetectionDiagnostics, SessionKind};
    use std::io::Write;
    use std::path::PathBuf;

    #[test]
    fn cli_placeholder_session_has_valid_modified_from_started_at_ms() {
        // CLI-sourced session with no JSONL → must still produce parseable RFC3339
        // `modified` so `new Date(modified).getTime()` doesn't yield NaN on the
        // frontend (which would scramble sort/group ordering).
        let tmp = tempfile::tempdir().unwrap();
        let detected = DetectedSession {
            pid: 12345,
            cwd: PathBuf::from("/tmp/nonexistent"),
            project_path: tmp.path().join("never-existed"),
            session_id: Some("11111111-2222-3333-4444-555555555555".to_string()),
            project_name: "nonexistent".to_string(),
            kind: SessionKind::Interactive,
            started_at_ms: Some(1_700_000_000_000),
            official_name: None,
            cli_activity: None,
            provider: SessionProvider::ClaudeCode,
            surface: SessionSurface::ClaudeCode,
            agent_kind: AgentKind::Root,
            parent_thread_id: None,
            root_session_id: None,
            agent_path: None,
            agent_nickname: None,
            agent_role: None,
            internal_kind: None,
            can_open: true,
            can_stop: true,
            can_rename: true,
            codex_summary: None,
            cursor_summary: None,
            pi_summary: None,
        };
        let (sessions, _) =
            enrich_detected_sessions(vec![detected], DetectionDiagnostics::default()).unwrap();
        assert_eq!(sessions.len(), 1, "CLI placeholder should be emitted");
        let s = &sessions[0];
        assert!(!s.modified.is_empty(), "modified must not be empty");
        let parsed = DateTime::parse_from_rfc3339(&s.modified);
        assert!(
            parsed.is_ok(),
            "modified must parse as RFC3339, got: {}",
            s.modified
        );
        assert_eq!(s.started_at_ms, Some(1_700_000_000_000));
    }

    #[test]
    fn legacy_session_without_jsonl_still_skipped() {
        // Legacy backend leaves started_at_ms = None. Without JSONL, no message
        // count, no real data — keep dropping these to avoid the legacy ghost-pid bug.
        let tmp = tempfile::tempdir().unwrap();
        let detected = DetectedSession::with_legacy_defaults(
            42,
            PathBuf::from("/tmp/legacy"),
            tmp.path().join("legacy-empty"),
            Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string()),
            "legacy".to_string(),
        );
        let (sessions, _) =
            enrich_detected_sessions(vec![detected], DetectionDiagnostics::default()).unwrap();
        assert!(
            sessions.is_empty(),
            "legacy empty-JSONL session must be skipped"
        );
    }

    #[test]
    fn provider_scoped_identity_keeps_same_id_from_codex_and_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let same_id = "same-provider-id";

        let mut codex = DetectedSession::with_legacy_defaults(
            0,
            PathBuf::from("/tmp/codex"),
            tmp.path().join("codex"),
            Some(same_id.to_string()),
            "codex".to_string(),
        );
        codex.provider = SessionProvider::Codex;
        codex.surface = SessionSurface::App;
        let mut codex_summary = crate::session::codex::CodexRolloutSummary::default();
        codex_summary.thread_id = same_id.to_string();
        codex.codex_summary = Some(codex_summary);

        let mut cursor = DetectedSession::with_legacy_defaults(
            0,
            PathBuf::from("/tmp/cursor"),
            tmp.path().join("cursor"),
            Some(same_id.to_string()),
            "cursor".to_string(),
        );
        cursor.provider = SessionProvider::Cursor;
        cursor.surface = SessionSurface::Cursor;
        let mut cursor_summary = crate::session::cursor::CursorTranscriptSummary::default();
        cursor_summary.session_id = same_id.to_string();
        cursor.cursor_summary = Some(cursor_summary);

        let mut pi = DetectedSession::with_legacy_defaults(
            0,
            PathBuf::from("/tmp/pi"),
            tmp.path().join("pi"),
            Some(same_id.to_string()),
            "pi".to_string(),
        );
        pi.provider = SessionProvider::Pi;
        pi.surface = SessionSurface::Cli;
        let mut pi_summary = crate::session::pi::PiTranscriptSummary::default();
        pi_summary.session_id = same_id.to_string();
        pi_summary.first_prompt = Some("pi prompt".to_string());
        pi.pi_summary = Some(pi_summary);

        let (sessions, _) =
            enrich_detected_sessions(vec![codex, cursor, pi], DetectionDiagnostics::default())
                .unwrap();

        assert_eq!(sessions.len(), 3);
        assert_eq!(sessions[0].session_key, "codex:same-provider-id");
        assert_eq!(sessions[1].session_key, "cursor:same-provider-id");
        assert_eq!(sessions[2].session_key, "pi:same-provider-id");
        assert_eq!(sessions[2].provider, SessionProvider::Pi);
    }

    #[test]
    fn native_title_cache_invalidates_on_transcript_change_and_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("synthetic-claude.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"type":"custom-title","customTitle":"first","sessionId":"synthetic"}}"#
        )
        .unwrap();
        drop(file);

        assert_eq!(get_cached_native_title(&path).as_deref(), Some("first"));
        assert_eq!(get_cached_native_title(&path).as_deref(), Some("first"));

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            r#"{{"type":"custom-title","customTitle":"second","sessionId":"synthetic"}}"#
        )
        .unwrap();
        drop(file);

        assert_eq!(get_cached_native_title(&path).as_deref(), Some("second"));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(get_cached_native_title(&path), None);
    }
}
