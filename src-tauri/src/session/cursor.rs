//! Cursor Agent session discovery from `~/.cursor/projects/*/agent-transcripts`.
//!
//! JSONL transcripts are the source of truth for discovery and liveness.
//! `state.vscdb` is an optional read-only overlay for title, cwd, and model.

use super::parser::MessageType;
use super::source::{
    AgentKind, DetectedSession, DetectionDiagnostics, SessionDetectorError, SessionKind,
    SessionProvider, SessionSource, SessionSurface,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const IDLE_FRESHNESS_SECS: u64 = 30 * 60;
const WORKING_FRESHNESS_SECS: u64 = 4 * 60 * 60;
const LINKED_PARENT_CEILING_SECS: u64 = 24 * 60 * 60;
const MONITOR_MESSAGE_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CursorLifecycle {
    Working,
    #[default]
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorMessage {
    pub timestamp: String,
    pub role: String,
    pub message_type: MessageType,
    pub content: String,
}

#[derive(Debug, Clone, Default)]
pub struct CursorTranscriptSummary {
    pub session_id: String,
    pub cwd: PathBuf,
    pub agent_kind: AgentKind,
    pub parent_thread_id: Option<String>,
    pub root_session_id: Option<String>,
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
    pub lifecycle: CursorLifecycle,
    pub messages: Vec<CursorMessage>,
    pub started_at_ms: Option<i64>,
    pub last_timestamp: String,
    pub empty: bool,
}

impl CursorTranscriptSummary {
    pub fn first_prompt(&self) -> Option<&str> {
        self.messages
            .iter()
            .find(|message| message.role == "user")
            .map(|message| message.content.as_str())
    }

    pub fn latest_message(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .find(|message| {
                matches!(
                    message.message_type,
                    MessageType::User | MessageType::Assistant
                )
            })
            .map(|message| message.content.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified_nanos: u128,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    stamp: FileStamp,
    offset: u64,
    summary: CursorTranscriptSummary,
}

struct TranscriptRef {
    path: PathBuf,
    session_id: String,
    agent_kind: AgentKind,
    parent_thread_id: Option<String>,
    project_key: String,
}

#[derive(Default)]
pub struct CursorSessionSource {
    projects_root: PathBuf,
    vscdb_path: Option<PathBuf>,
    cache: HashMap<PathBuf, CacheEntry>,
    #[cfg(test)]
    parse_count: u32,
}

impl CursorSessionSource {
    pub fn new() -> Result<Self, SessionDetectorError> {
        let home = dirs::home_dir().ok_or(SessionDetectorError::HomeDirectoryNotFound)?;
        Ok(Self::at_roots(
            home.join(".cursor").join("projects"),
            default_vscdb_path(&home),
        ))
    }

    pub fn at_root(projects_root: PathBuf) -> Self {
        Self::at_roots(projects_root, None)
    }

    fn at_roots(projects_root: PathBuf, vscdb_path: Option<PathBuf>) -> Self {
        Self {
            projects_root,
            vscdb_path,
            cache: HashMap::new(),
            #[cfg(test)]
            parse_count: 0,
        }
    }

    #[cfg(test)]
    fn at_root_with_vscdb(projects_root: PathBuf, vscdb_path: PathBuf) -> Self {
        Self::at_roots(projects_root, Some(vscdb_path))
    }

    fn detect_at(
        &mut self,
        wall_now: SystemTime,
    ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
        let refs = collect_transcript_refs(&self.projects_root);
        let mut live: Vec<(PathBuf, CursorTranscriptSummary, u64)> = Vec::new();
        let mut existing_paths = HashSet::new();

        for transcript in &refs {
            existing_paths.insert(transcript.path.clone());
            let Ok(summary) = self.summary_for(&transcript.path, transcript) else {
                continue;
            };
            let age = file_age_secs(&transcript.path, wall_now);
            live.push((transcript.path.clone(), summary, age));
        }

        let composers = load_composer_map(self.vscdb_path.as_deref());
        for (_path, summary, _age) in &mut live {
            apply_composer_overlay(summary, composers.get(&summary.session_id));
        }

        let working_parent_ids: HashSet<String> = live
            .iter()
            .filter(|(_, summary, age)| {
                summary.agent_kind == AgentKind::Subagent
                    && summary.lifecycle == CursorLifecycle::Working
                    && *age <= WORKING_FRESHNESS_SECS
            })
            .flat_map(|(_, summary, _)| {
                [
                    summary.parent_thread_id.clone(),
                    summary.root_session_id.clone(),
                ]
                .into_iter()
                .flatten()
            })
            .collect();

        self.cache
            .retain(|path, _| existing_paths.contains(path));

        let mut sessions = Vec::new();
        for (_path, summary, age_secs) in live {
            if summary.session_id.is_empty() {
                continue;
            }
            let freshness = if summary.lifecycle == CursorLifecycle::Working {
                WORKING_FRESHNESS_SECS
            } else {
                IDLE_FRESHNESS_SECS
            };
            let linked_parent = working_parent_ids.contains(&summary.session_id)
                && age_secs <= LINKED_PARENT_CEILING_SECS;
            if age_secs > freshness && !linked_parent {
                continue;
            }

            let cwd = if summary.cwd.as_os_str().is_empty() {
                PathBuf::from("Cursor")
            } else {
                summary.cwd.clone()
            };
            let project_name = cwd
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Cursor")
                .to_string();
            sessions.push(DetectedSession {
                pid: 0,
                cwd: cwd.clone(),
                project_path: cwd,
                session_id: Some(summary.session_id.clone()),
                project_name,
                kind: SessionKind::Interactive,
                started_at_ms: summary.started_at_ms,
                official_name: summary.agent_nickname.clone(),
                cli_activity: None,
                provider: SessionProvider::Cursor,
                surface: SessionSurface::Cursor,
                agent_kind: summary.agent_kind,
                parent_thread_id: summary.parent_thread_id.clone(),
                root_session_id: summary.root_session_id.clone(),
                agent_path: None,
                agent_nickname: summary.agent_nickname.clone(),
                agent_role: summary.agent_role.clone(),
                internal_kind: None,
                can_open: false,
                can_stop: false,
                can_rename: false,
                codex_summary: None,
                cursor_summary: Some(summary),
            });
        }
        Ok((sessions, DetectionDiagnostics::default()))
    }

    fn summary_for(
        &mut self,
        path: &Path,
        transcript: &TranscriptRef,
    ) -> Result<CursorTranscriptSummary, SessionDetectorError> {
        let stamp = file_stamp(path)?;
        if let Some(cached) = self.cache.get(path) {
            if cached.stamp == stamp {
                return Ok(cached.summary.clone());
            }
        }
        let (existing, start_offset) = match self.cache.get(path) {
            Some(cached) if stamp.len > cached.offset => {
                (Some(cached.summary.clone()), cached.offset)
            }
            _ => (None, 0),
        };
        #[cfg(test)]
        {
            self.parse_count += 1;
        }
        let (mut summary, offset) =
            parse_transcript_range(path, transcript, true, start_offset, existing)?;
        if summary.cwd.as_os_str().is_empty() {
            summary.cwd = decode_cursor_project_key(&transcript.project_key);
        }
        self.cache.insert(
            path.to_path_buf(),
            CacheEntry {
                stamp,
                offset,
                summary: summary.clone(),
            },
        );
        Ok(summary)
    }
}

impl SessionSource for CursorSessionSource {
    fn detect(
        &mut self,
    ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
        self.detect_at(SystemTime::now())
    }

    fn backend_name(&self) -> &'static str {
        "cursor-transcript"
    }
}

pub fn find_cursor_conversation_with_progress(
    session_id: &str,
    include_tools: bool,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<CursorMessage>, String> {
    let home = dirs::home_dir().ok_or("Failed to get home directory")?;
    find_cursor_conversation_under(
        &home.join(".cursor").join("projects"),
        session_id,
        include_tools,
        on_progress,
    )
}

pub fn find_cursor_conversation_under(
    projects_root: &Path,
    session_id: &str,
    include_tools: bool,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<CursorMessage>, String> {
    let Some(transcript) = collect_transcript_refs(projects_root)
        .into_iter()
        .find(|item| item.session_id == session_id)
    else {
        return Err(format!("Cursor session {session_id} not found"));
    };
    let total = fs::metadata(&transcript.path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    on_progress(0, total);
    let mut summary =
        parse_transcript(&transcript.path, &transcript, false).map_err(|error| error.to_string())?;
    on_progress(total, total);
    if !include_tools {
        summary
            .messages
            .retain(|message| !matches!(message.message_type, MessageType::ToolUse));
    }
    Ok(summary.messages)
}

pub fn cursor_history_entries(home_dir: &Path) -> Vec<crate::session::history::HistoryEntry> {
    let projects_root = home_dir.join(".cursor").join("projects");
    collect_transcript_refs(&projects_root)
        .into_iter()
        .filter(|item| item.agent_kind == AgentKind::Root)
        .filter_map(|item| {
            let summary = parse_transcript(&item.path, &item, true).ok()?;
            let display = summary
                .first_prompt()
                .unwrap_or("(No conversation yet)")
                .to_string();
            let cwd = if summary.cwd.as_os_str().is_empty() {
                decode_cursor_project_key(&item.project_key)
            } else {
                summary.cwd
            };
            let project_name = cwd
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Cursor")
                .to_string();
            let timestamp = fs::metadata(&item.path)
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(system_time_millis)
                .unwrap_or(0)
                .max(0) as u64;
            Some(crate::session::history::HistoryEntry {
                session_id: item.session_id,
                display,
                timestamp,
                project: cwd.to_string_lossy().into_owned(),
                project_name,
                custom_title: summary.agent_nickname,
                provider: "cursor".to_string(),
                surface: Some("cursor".to_string()),
                agent_kind: Some("root".to_string()),
            })
        })
        .collect()
}

pub fn list_session_ids(home_dir: &Path, prefix: &str) -> Vec<String> {
    collect_transcript_refs(&home_dir.join(".cursor").join("projects"))
        .into_iter()
        .map(|item| item.session_id)
        .filter(|id| id.starts_with(prefix))
        .collect()
}

pub fn search_cursor_transcripts(
    home_dir: &Path,
    query_norm: &str,
    case_sensitive: bool,
    whole_word: bool,
    phrase_match: fn(&str, &str, bool) -> bool,
    extract_snippet: fn(&str, &str, &str) -> String,
) -> Vec<crate::session::history::DeepSearchHit> {
    let mut hits = Vec::new();
    let projects_root = home_dir.join(".cursor").join("projects");
    for item in collect_transcript_refs(&projects_root)
        .into_iter()
        .filter(|item| item.agent_kind == AgentKind::Root)
    {
        let Ok(summary) = parse_transcript(&item.path, &item, true) else {
            continue;
        };
        let cwd = if summary.cwd.as_os_str().is_empty() {
            decode_cursor_project_key(&item.project_key)
        } else {
            summary.cwd.clone()
        };
        for message in &summary.messages {
            let text = &message.content;
            let norm = if case_sensitive {
                text.clone()
            } else {
                text.to_lowercase()
            };
            if !phrase_match(&norm, query_norm, whole_word) {
                continue;
            }
            let snippet = extract_snippet(text, &norm, query_norm);
            if snippet.is_empty() {
                continue;
            }
            hits.push(crate::session::history::DeepSearchHit {
                session_id: item.session_id.clone(),
                snippet,
                project_path: Some(cwd.to_string_lossy().into_owned()),
                modified: Some(summary.last_timestamp.clone()),
                provider: "cursor".to_string(),
                surface: Some("cursor".to_string()),
                agent_kind: if item.agent_kind == AgentKind::Subagent {
                    "subagent".to_string()
                } else {
                    "root".to_string()
                },
            });
            break;
        }
    }
    hits
}

fn default_vscdb_path(home: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(
            home.join("Library")
                .join("Application Support")
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb"),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = home;
        None
    }
}

fn collect_transcript_refs(projects_root: &Path) -> Vec<TranscriptRef> {
    let Ok(projects) = fs::read_dir(projects_root) else {
        return Vec::new();
    };
    let mut refs = Vec::new();
    for project in projects.flatten() {
        let project_path = project.path();
        if !project_path.is_dir() {
            continue;
        }
        let Some(project_key) = project_path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let transcripts = project_path.join("agent-transcripts");
        let Ok(sessions) = fs::read_dir(&transcripts) else {
            continue;
        };
        for session_dir in sessions.flatten() {
            let session_path = session_dir.path();
            if !session_path.is_dir() {
                continue;
            }
            let Some(parent_id) = session_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            if !looks_like_uuid(&parent_id) {
                continue;
            }
            let parent_file = session_path.join(format!("{parent_id}.jsonl"));
            if parent_file.is_file() {
                refs.push(TranscriptRef {
                    path: parent_file,
                    session_id: parent_id.clone(),
                    agent_kind: AgentKind::Root,
                    parent_thread_id: None,
                    project_key: project_key.clone(),
                });
            }
            let subagents = session_path.join("subagents");
            let Ok(children) = fs::read_dir(&subagents) else {
                continue;
            };
            for child in children.flatten() {
                let child_path = child.path();
                if child_path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(child_id) = child_path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
                else {
                    continue;
                };
                if !looks_like_uuid(&child_id) {
                    continue;
                }
                refs.push(TranscriptRef {
                    path: child_path,
                    session_id: child_id,
                    agent_kind: AgentKind::Subagent,
                    parent_thread_id: Some(parent_id.clone()),
                    project_key: project_key.clone(),
                });
            }
        }
    }
    refs
}

fn parse_transcript(
    path: &Path,
    transcript: &TranscriptRef,
    compact: bool,
) -> Result<CursorTranscriptSummary, SessionDetectorError> {
    let (summary, _) = parse_transcript_range(path, transcript, compact, 0, None)?;
    Ok(summary)
}

fn parse_transcript_range(
    path: &Path,
    transcript: &TranscriptRef,
    compact: bool,
    start_offset: u64,
    existing: Option<CursorTranscriptSummary>,
) -> Result<(CursorTranscriptSummary, u64), SessionDetectorError> {
    let mut file = File::open(path)?;
    let metadata = fs::metadata(path)?;
    let modified = metadata.modified().ok().and_then(system_time_millis);
    let created = metadata.created().ok().and_then(system_time_millis);
    let last_timestamp = modified
        .and_then(|ms| chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms))
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();
    let mut saw_lifecycle = existing.is_some();
    let mut summary = existing.unwrap_or_else(|| CursorTranscriptSummary {
        session_id: transcript.session_id.clone(),
        agent_kind: transcript.agent_kind,
        parent_thread_id: transcript.parent_thread_id.clone(),
        root_session_id: Some(
            transcript
                .parent_thread_id
                .clone()
                .unwrap_or_else(|| transcript.session_id.clone()),
        ),
        started_at_ms: created.or(modified),
        last_timestamp: last_timestamp.clone(),
        ..CursorTranscriptSummary::default()
    });
    summary.last_timestamp = last_timestamp;

    file.seek(SeekFrom::Start(start_offset))?;
    let mut reader = BufReader::new(file);
    let mut offset = start_offset;
    loop {
        let mut buf = Vec::new();
        let bytes_read = reader.read_until(b'\n', &mut buf)?;
        if bytes_read == 0 {
            break;
        }
        if !buf.ends_with(&[b'\n']) {
            break;
        }
        offset += bytes_read as u64;
        let Ok(line) = std::str::from_utf8(&buf) else {
            continue;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if apply_transcript_line(&mut summary, &value, compact) {
            saw_lifecycle = true;
        }
    }
    summary.empty = summary.messages.is_empty();
    if summary.empty && !saw_lifecycle {
        summary.lifecycle = CursorLifecycle::Working;
    }
    Ok((summary, offset))
}

fn apply_transcript_line(summary: &mut CursorTranscriptSummary, value: &Value, compact: bool) -> bool {
    if value.get("type").and_then(Value::as_str) == Some("turn_ended") {
        summary.lifecycle = CursorLifecycle::Idle;
        return true;
    }
    let Some(role) = value.get("role").and_then(Value::as_str) else {
        return false;
    };
    summary.lifecycle = CursorLifecycle::Working;
    let content_blocks = value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    match role {
        "user" => {
            let raw = collect_text_blocks(&content_blocks);
            let content = extract_user_query(&raw);
            if content.is_empty() {
                return true;
            }
            if summary.agent_kind == AgentKind::Subagent {
                if let Some(role_name) = infer_subagent_role(&content) {
                    if summary.agent_role.is_none() {
                        summary.agent_role = Some(role_name);
                    }
                }
            }
            push_monitor_message(summary, "user", MessageType::User, content, compact);
        }
        "assistant" => {
            let text = collect_text_blocks(&content_blocks);
            if !text.is_empty() {
                push_monitor_message(summary, "assistant", MessageType::Assistant, text, compact);
            }
            for block in &content_blocks {
                if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                    continue;
                }
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                if name == "Task" && summary.agent_kind == AgentKind::Subagent {
                    if let Some(subagent_type) = input.get("subagent_type").and_then(Value::as_str)
                    {
                        if summary.agent_role.is_none() {
                            summary.agent_role = Some(subagent_type.to_string());
                        }
                    }
                }
                let content = format!("{name} {}", compact_json(&input));
                push_monitor_message(summary, "assistant", MessageType::ToolUse, content, compact);
            }
        }
        _ => {}
    }
    true
}

fn push_monitor_message(
    summary: &mut CursorTranscriptSummary,
    role: &str,
    message_type: MessageType,
    content: String,
    compact: bool,
) {
    let content = if compact {
        truncate_chars(&content, MONITOR_MESSAGE_CHARS)
    } else {
        content
    };
    summary.messages.push(CursorMessage {
        timestamp: summary.last_timestamp.clone(),
        role: role.to_string(),
        message_type,
        content,
    });
}

fn collect_text_blocks(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn extract_user_query(text: &str) -> String {
    if let Some(start) = text.find("<user_query>") {
        let rest = &text[start + "<user_query>".len()..];
        if let Some(end) = rest.find("</user_query>") {
            return rest[..end].trim().to_string();
        }
        return rest.trim().to_string();
    }
    if let Some(end) = text.find("</timestamp>") {
        return text[end + "</timestamp>".len()..].trim().to_string();
    }
    text.trim().to_string()
}

fn infer_subagent_role(prompt: &str) -> Option<String> {
    let lower = prompt.to_lowercase();
    if lower.contains("subagent_type") {
        return None;
    }
    for marker in ["explore", "generalpurpose", "general-purpose", "bugbot"] {
        if lower.contains(marker) {
            return Some(marker.replace('-', ""));
        }
    }
    None
}

pub(crate) fn decode_cursor_project_key(encoded: &str) -> PathBuf {
    if encoded.is_empty() {
        return PathBuf::from("/");
    }
    let parts: Vec<&str> = encoded.split('-').collect();
    let mut current = PathBuf::from("/");
    let mut i = 0;
    while i < parts.len() {
        let mut matched = false;
        for j in (i + 1..=parts.len()).rev() {
            let candidate = parts[i..j].join("-");
            let next = current.join(&candidate);
            if next.exists() {
                current = next;
                i = j;
                matched = true;
                break;
            }
        }
        if !matched {
            current = current.join(parts[i..].join("-"));
            break;
        }
    }
    current
}

fn looks_like_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
}

fn file_stamp(path: &Path) -> Result<FileStamp, SessionDetectorError> {
    let metadata = fs::metadata(path)?;
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    Ok(FileStamp {
        len: metadata.len(),
        modified_nanos,
    })
}

fn file_age_secs(path: &Path, wall_now: SystemTime) -> u64 {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| wall_now.duration_since(modified).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn system_time_millis(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as i64)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

#[derive(Debug, Clone, Default)]
struct ComposerOverlay {
    name: Option<String>,
    cwd: Option<PathBuf>,
    agent_role: Option<String>,
    generating: bool,
}

fn apply_composer_overlay(summary: &mut CursorTranscriptSummary, overlay: Option<&ComposerOverlay>) {
    let Some(overlay) = overlay else {
        return;
    };
    if let Some(name) = overlay.name.clone() {
        summary.agent_nickname = Some(name);
    }
    if let Some(cwd) = overlay.cwd.clone() {
        summary.cwd = cwd;
    }
    if let Some(role) = overlay.agent_role.clone() {
        summary.agent_role = Some(role);
    }
    if overlay.generating {
        summary.lifecycle = CursorLifecycle::Working;
    }
}

fn load_composer_map(vscdb_path: Option<&Path>) -> HashMap<String, ComposerOverlay> {
    let Some(path) = vscdb_path else {
        return HashMap::new();
    };
    if !path.is_file() {
        return HashMap::new();
    }
    read_composer_overlays(path).unwrap_or_default()
}

fn read_composer_overlays(path: &Path) -> Result<HashMap<String, ComposerOverlay>, rusqlite::Error> {
    let uri = format!("file:{}?mode=ro", path.display());
    let conn = rusqlite::Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    let _ = conn.busy_timeout(Duration::from_millis(50));
    let mut stmt = conn.prepare("SELECT key, value FROM cursorDiskKV WHERE key LIKE 'composerData:%'")?;
    let rows = stmt.query_map([], |row| {
        let key: String = row.get(0)?;
        let value: String = row.get(1)?;
        Ok((key, value))
    })?;
    let mut map = HashMap::new();
    for row in rows.flatten() {
        let Some(id) = row.0.strip_prefix("composerData:") else {
            continue;
        };
        if let Some(overlay) = parse_composer_overlay(&row.1) {
            map.insert(id.to_string(), overlay);
        }
    }
    Ok(map)
}

#[derive(Debug, Deserialize)]
struct ComposerDataFile {
    name: Option<String>,
    #[serde(rename = "workspaceIdentifier")]
    workspace_identifier: Option<WorkspaceIdentifier>,
    #[serde(rename = "subagentInfo")]
    subagent_info: Option<SubagentInfoFile>,
    #[serde(rename = "generatingBubbleIds")]
    generating_bubble_ids: Option<Vec<Value>>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceIdentifier {
    uri: Option<WorkspaceUri>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceUri {
    #[serde(rename = "fsPath")]
    fs_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SubagentInfoFile {
    #[serde(rename = "subagentTypeName")]
    subagent_type_name: Option<String>,
}

fn parse_composer_overlay(raw: &str) -> Option<ComposerOverlay> {
    let data: ComposerDataFile = serde_json::from_str(raw).ok()?;
    Some(ComposerOverlay {
        name: data.name.filter(|name| !name.is_empty()),
        cwd: data
            .workspace_identifier
            .and_then(|workspace| workspace.uri)
            .and_then(|uri| uri.fs_path)
            .map(PathBuf::from),
        agent_role: data
            .subagent_info
            .and_then(|info| info.subagent_type_name)
            .filter(|name| !name.is_empty()),
        generating: data
            .generating_bubble_ids
            .is_some_and(|ids| !ids.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    const PARENT: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    const CHILD_RUNNING: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
    const CHILD_DONE: &str = "cccccccc-cccc-cccc-cccc-cccccccccccc";

    fn write_jsonl(path: &Path, lines: &[&str]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = File::create(path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    fn user_line(text: &str) -> String {
        format!(
            r#"{{"role":"user","message":{{"content":[{{"type":"text","text":"<timestamp>Wednesday, Aug 19, 2026, 9:31 PM (UTC+8)</timestamp>\n<user_query>\n{text}\n</user_query>"}}]}}}}"#
        )
    }

    fn assistant_line(text: &str) -> String {
        format!(
            r#"{{"role":"assistant","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    fn tool_line(name: &str, extra: &str) -> String {
        format!(
            r#"{{"role":"assistant","message":{{"content":[{{"type":"tool_use","name":"{name}","input":{extra}}}]}}}}"#
        )
    }

    fn layout(root: &Path, project_key: &str) -> PathBuf {
        let transcripts = root
            .join(project_key)
            .join("agent-transcripts")
            .join(PARENT);
        fs::create_dir_all(transcripts.join("subagents")).unwrap();
        transcripts
    }

    #[test]
    fn extracts_user_query_and_strips_wrapper() {
        let raw = "<timestamp>Wednesday, Aug 19, 2026, 9:31 PM (UTC+8)</timestamp>\n<user_query>\nFix the overlay\n</user_query>";
        assert_eq!(extract_user_query(raw), "Fix the overlay");
    }

    #[test]
    fn last_turn_ended_is_idle_even_with_earlier_turns() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        write_jsonl(
            &session_dir.join(format!("{PARENT}.jsonl")),
            &[
                &user_line("first"),
                &assistant_line("working"),
                r#"{"type":"turn_ended","status":"success"}"#,
                &user_line("second"),
                &assistant_line("done"),
                r#"{"type":"turn_ended","status":"success"}"#,
            ],
        );
        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (sessions, _) = source.detect().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].agent_kind, AgentKind::Root);
        assert_eq!(
            sessions[0].cursor_summary.as_ref().unwrap().lifecycle,
            CursorLifecycle::Idle
        );
        assert_eq!(
            sessions[0]
                .cursor_summary
                .as_ref()
                .unwrap()
                .first_prompt()
                .unwrap(),
            "first"
        );
    }

    #[test]
    fn running_subagent_without_turn_ended_is_working_and_pins_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        write_jsonl(
            &session_dir.join(format!("{PARENT}.jsonl")),
            &[
                &user_line("parent prompt"),
                &tool_line("Task", r#"{"subagent_type":"explore","description":"scan"}"#),
                r#"{"type":"turn_ended","status":"success"}"#,
            ],
        );
        write_jsonl(
            &session_dir
                .join("subagents")
                .join(format!("{CHILD_RUNNING}.jsonl")),
            &[
                &user_line("Explore the codebase"),
                &tool_line("Grep", r#"{"pattern":"cursor"}"#),
            ],
        );
        write_jsonl(
            &session_dir
                .join("subagents")
                .join(format!("{CHILD_DONE}.jsonl")),
            &[
                &user_line("Done already"),
                &assistant_line("finished"),
                r#"{"type":"turn_ended","status":"success"}"#,
            ],
        );

        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (sessions, _) = source.detect().unwrap();
        assert_eq!(sessions.len(), 3);
        let running = sessions
            .iter()
            .find(|session| session.session_id.as_deref() == Some(CHILD_RUNNING))
            .unwrap();
        assert_eq!(running.agent_kind, AgentKind::Subagent);
        assert_eq!(running.parent_thread_id.as_deref(), Some(PARENT));
        assert_eq!(
            running.cursor_summary.as_ref().unwrap().lifecycle,
            CursorLifecycle::Working
        );
        let done = sessions
            .iter()
            .find(|session| session.session_id.as_deref() == Some(CHILD_DONE))
            .unwrap();
        assert_eq!(
            done.cursor_summary.as_ref().unwrap().lifecycle,
            CursorLifecycle::Idle
        );
    }

    #[test]
    fn idle_sessions_age_out_but_working_child_keeps_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        write_jsonl(
            &session_dir.join(format!("{PARENT}.jsonl")),
            &[
                &user_line("parent"),
                r#"{"type":"turn_ended","status":"success"}"#,
            ],
        );
        write_jsonl(
            &session_dir
                .join("subagents")
                .join(format!("{CHILD_RUNNING}.jsonl")),
            &[&user_line("still going"), &assistant_line("working")],
        );

        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let stale = SystemTime::now() + Duration::from_secs(IDLE_FRESHNESS_SECS + 60);
        let (sessions, _) = source.detect_at(stale).unwrap();
        let ids: HashSet<_> = sessions
            .iter()
            .filter_map(|session| session.session_id.clone())
            .collect();
        assert!(ids.contains(PARENT), "working child should pin idle parent");
        assert!(ids.contains(CHILD_RUNNING));
    }

    #[test]
    fn conversation_tools_off_drops_tool_use() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        write_jsonl(
            &session_dir.join(format!("{PARENT}.jsonl")),
            &[
                &user_line("hello"),
                &tool_line("Grep", r#"{"pattern":"x"}"#),
                &assistant_line("done"),
            ],
        );
        let with_tools = find_cursor_conversation_under(tmp.path(), PARENT, true, &mut |_, _| {})
            .unwrap();
        let without_tools =
            find_cursor_conversation_under(tmp.path(), PARENT, false, &mut |_, _| {}).unwrap();
        assert!(with_tools
            .iter()
            .any(|message| message.message_type == MessageType::ToolUse));
        assert!(without_tools
            .iter()
            .all(|message| message.message_type != MessageType::ToolUse));
        assert_eq!(without_tools[0].content, "hello");
    }

    #[test]
    fn decode_prefers_existing_path_segments() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("GitHub").join("c9watch");
        fs::create_dir_all(&real).unwrap();
        let encoded = format!(
            "{}-GitHub-c9watch",
            tmp.path()
                .strip_prefix("/")
                .unwrap()
                .to_string_lossy()
                .replace('/', "-")
        );
        let decoded = decode_cursor_project_key(&encoded);
        assert_eq!(decoded, real);
    }

    #[test]
    fn jsonl_only_still_works_when_vscdb_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        write_jsonl(
            &session_dir.join(format!("{PARENT}.jsonl")),
            &[&user_line("no db"), &assistant_line("ok")],
        );
        let mut source = CursorSessionSource::at_root_with_vscdb(
            tmp.path().to_path_buf(),
            tmp.path().join("missing.vscdb"),
        );
        let (sessions, _) = source.detect().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].provider, SessionProvider::Cursor);
        assert!(!sessions[0].can_open);
    }

    #[test]
    fn vscdb_overlay_fills_title_cwd_and_can_promote_working() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        write_jsonl(
            &session_dir.join(format!("{PARENT}.jsonl")),
            &[
                &user_line("named chat"),
                r#"{"type":"turn_ended","status":"success"}"#,
            ],
        );
        let db_path = tmp.path().join("state.vscdb");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        let payload = serde_json::json!({
            "name": "Research Cursor agent sessions",
            "workspaceIdentifier": {"uri": {"fsPath": "/tmp/real-cwd"}},
            "generatingBubbleIds": ["bubble-1"],
            "status": "aborted"
        });
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            rusqlite::params![format!("composerData:{PARENT}"), payload.to_string()],
        )
        .unwrap();
        drop(conn);

        let mut source =
            CursorSessionSource::at_root_with_vscdb(tmp.path().to_path_buf(), db_path);
        let (sessions, _) = source.detect().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].agent_nickname.as_deref(),
            Some("Research Cursor agent sessions")
        );
        assert_eq!(sessions[0].cwd, PathBuf::from("/tmp/real-cwd"));
        assert_eq!(
            sessions[0].cursor_summary.as_ref().unwrap().lifecycle,
            CursorLifecycle::Working
        );
    }

    #[test]
    fn aborted_status_without_generating_ids_does_not_force_working() {
        let overlay = parse_composer_overlay(
            r#"{"name":"x","status":"aborted","generatingBubbleIds":[],"unfinishedRunAt":1}"#,
        )
        .unwrap();
        assert!(!overlay.generating);
        let mut summary = CursorTranscriptSummary {
            lifecycle: CursorLifecycle::Idle,
            ..CursorTranscriptSummary::default()
        };
        apply_composer_overlay(&mut summary, Some(&overlay));
        assert_eq!(summary.lifecycle, CursorLifecycle::Idle);
    }

    #[test]
    fn stale_idle_root_without_working_child_is_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        write_jsonl(
            &session_dir.join(format!("{PARENT}.jsonl")),
            &[
                &user_line("old chat"),
                r#"{"type":"turn_ended","status":"success"}"#,
            ],
        );
        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let stale = SystemTime::now() + Duration::from_secs(IDLE_FRESHNESS_SECS + 60);
        let (sessions, _) = source.detect_at(stale).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn decode_keeps_hyphenated_directory_names() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("my-app");
        fs::create_dir_all(&real).unwrap();
        let encoded = format!(
            "{}-my-app",
            tmp.path()
                .strip_prefix("/")
                .unwrap()
                .to_string_lossy()
                .replace('/', "-")
        );
        assert_eq!(decode_cursor_project_key(&encoded), real);
    }

    #[test]
    fn empty_transcript_is_marked_empty_working() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        write_jsonl(&session_dir.join(format!("{PARENT}.jsonl")), &[]);
        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (sessions, _) = source.detect().unwrap();
        assert_eq!(sessions.len(), 1);
        let summary = sessions[0].cursor_summary.as_ref().unwrap();
        assert!(summary.empty);
        assert_eq!(summary.lifecycle, CursorLifecycle::Working);
        let (enriched, _) = crate::session::enrichment::enrich_detected_sessions(
            sessions,
            DetectionDiagnostics::default(),
        )
        .unwrap();
        assert_eq!(enriched[0].status, crate::session::SessionStatus::Connecting);
    }

    #[test]
    fn history_lists_roots_not_subagents() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = tmp.path().join(".cursor").join("projects");
        let session_dir = layout(&projects, "demo-project");
        write_jsonl(
            &session_dir.join(format!("{PARENT}.jsonl")),
            &[&user_line("root prompt")],
        );
        write_jsonl(
            &session_dir
                .join("subagents")
                .join(format!("{CHILD_RUNNING}.jsonl")),
            &[&user_line("child prompt")],
        );
        let entries = cursor_history_entries(tmp.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, PARENT);
        assert_eq!(entries[0].provider, "cursor");
        assert_eq!(entries[0].display, "root prompt");
        assert!(entries[0].timestamp > 0, "history should sort by last write, not birth");
    }

    #[test]
    fn incremental_cache_reuses_unchanged_file_and_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        let path = session_dir.join(format!("{PARENT}.jsonl"));
        write_jsonl(&path, &[&user_line("hello"), &assistant_line("working")]);
        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (first, _) = source.detect().unwrap();
        assert_eq!(
            first[0].cursor_summary.as_ref().unwrap().lifecycle,
            CursorLifecycle::Working
        );
        let parsed = source.parse_count;
        let _ = source.detect().unwrap();
        assert_eq!(source.parse_count, parsed, "unchanged file must be a cache hit");

        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, r#"{{"type":"turn_ended","status":"success"}}"#).unwrap();
        drop(file);

        let (second, _) = source.detect().unwrap();
        assert!(source.parse_count > parsed);
        let summary = second[0].cursor_summary.as_ref().unwrap();
        assert_eq!(summary.lifecycle, CursorLifecycle::Idle);
        assert_eq!(
            summary
                .messages
                .iter()
                .filter(|message| message.role == "user")
                .count(),
            1,
            "append must not duplicate earlier records"
        );
    }

    #[test]
    fn turn_ended_only_file_stays_idle_not_connecting() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        write_jsonl(
            &session_dir.join(format!("{PARENT}.jsonl")),
            &[r#"{"type":"turn_ended","status":"success"}"#],
        );
        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (sessions, _) = source.detect().unwrap();
        let summary = sessions[0].cursor_summary.as_ref().unwrap();
        assert!(summary.empty);
        assert_eq!(summary.lifecycle, CursorLifecycle::Idle);
        let (enriched, _) = crate::session::enrichment::enrich_detected_sessions(
            sessions,
            DetectionDiagnostics::default(),
        )
        .unwrap();
        assert_eq!(enriched[0].status, crate::session::SessionStatus::WaitingForInput);
    }

    #[test]
    fn rewrite_replaces_cached_messages_instead_of_merging() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        let path = session_dir.join(format!("{PARENT}.jsonl"));
        write_jsonl(&path, &[&user_line("old prompt"), &assistant_line("old")]);
        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (first, _) = source.detect().unwrap();
        assert_eq!(
            first[0].cursor_summary.as_ref().unwrap().first_prompt(),
            Some("old prompt")
        );

        write_jsonl(&path, &[&user_line("replaced")]);
        let (second, _) = source.detect().unwrap();
        let summary = second[0].cursor_summary.as_ref().unwrap();
        assert_eq!(summary.first_prompt(), Some("replaced"));
        assert_eq!(
            summary
                .messages
                .iter()
                .filter(|message| message.role == "user")
                .count(),
            1
        );
    }

    #[test]
    fn parent_task_tool_does_not_become_root_agent_role() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        write_jsonl(
            &session_dir.join(format!("{PARENT}.jsonl")),
            &[
                &user_line("parent prompt"),
                &tool_line("Task", r#"{"subagent_type":"explore","description":"scan"}"#),
                r#"{"type":"turn_ended","status":"success"}"#,
            ],
        );
        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (sessions, _) = source.detect().unwrap();
        assert!(sessions[0].agent_role.is_none());
    }
}
