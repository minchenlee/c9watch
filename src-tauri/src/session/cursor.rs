//! Cursor Agent session discovery from `~/.cursor/projects/*/agent-transcripts`.
//!
//! JSONL transcripts are the source of truth for discovery and liveness.
//! `state.vscdb` is an optional read-only overlay for title, cwd, and model.

use super::cache::FileVersion;
use super::parser::MessageType;
use super::source::{
    AgentKind, DetectedSession, DetectionDiagnostics, SessionDetectorError, SessionKind,
    SessionProvider, SessionSource, SessionSurface,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const IDLE_FRESHNESS_SECS: u64 = 30 * 60;
const WORKING_FRESHNESS_SECS: u64 = 4 * 60 * 60;
const LINKED_PARENT_CEILING_SECS: u64 = 24 * 60 * 60;
const MONITOR_MESSAGE_CHARS: usize = 200;
const EXACT_PREFIX_VERIFY_LIMIT: u64 = 4 * 1024 * 1024;
const PREFIX_GUARD_BYTES: u64 = 64 * 1024;
const MIN_FULL_VERIFY_GROWTH_BYTES: u64 = 1024 * 1024;
const FULL_VERIFY_GROWTH_DIVISOR: u64 = 8;
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

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
    /// Cursor's current UI composer name from state.vscdb.
    pub cursor_title: Option<String>,
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

type FileStamp = FileVersion;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrefixGuard {
    len: u64,
    head_hash: u64,
    tail_hash: u64,
}

#[derive(Debug, Clone, Copy)]
struct PrefixSnapshot {
    hash: u64,
    guard: PrefixGuard,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    stamp: FileStamp,
    offset: u64,
    /// FNV hash of the transcript bytes in `[0, offset)`. The state can be
    /// extended over an append without re-reading the existing prefix.
    prefix_hash: u64,
    /// Bounded content guard used between full prefix validations on large
    /// transcripts. Small transcripts continue to use exact verification.
    prefix_guard: PrefixGuard,
    /// Large transcripts are fully revalidated after a geometrically bounded
    /// amount of growth. This keeps cumulative validation I/O linear in the
    /// transcript size while bounding the large-file rewrite detection window.
    next_full_verify_offset: u64,
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
    #[cfg(test)]
    prefix_verification_bytes: u64,
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

    #[cfg(test)]
    pub(crate) fn parse_count_for_test(&self) -> u32 {
        self.parse_count
    }

    #[cfg(test)]
    pub(crate) fn prefix_verification_bytes_for_test(&self) -> u64 {
        self.prefix_verification_bytes
    }

    fn at_roots(projects_root: PathBuf, vscdb_path: Option<PathBuf>) -> Self {
        Self {
            projects_root,
            vscdb_path,
            cache: HashMap::new(),
            #[cfg(test)]
            parse_count: 0,
            #[cfg(test)]
            prefix_verification_bytes: 0,
        }
    }

    pub(crate) fn contains_session_id(&self, session_id: &str) -> bool {
        collect_transcript_refs(&self.projects_root)
            .iter()
            .any(|transcript| transcript.session_id == session_id)
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
            let age = file_age_secs(&transcript.path, wall_now);
            if age > LINKED_PARENT_CEILING_SECS {
                self.cache.remove(&transcript.path);
                continue;
            }
            let Ok(summary) = self.summary_for(&transcript.path, transcript) else {
                continue;
            };
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

        let cacheable_paths: HashSet<PathBuf> = live
            .iter()
            .filter(|(_, _, age)| *age <= LINKED_PARENT_CEILING_SECS)
            .map(|(path, _, _)| path.clone())
            .collect();
        self.cache
            .retain(|path, _| existing_paths.contains(path) && cacheable_paths.contains(path));

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
                pi_summary: None,
            });
        }
        Ok((sessions, DetectionDiagnostics::default()))
    }

    fn summary_for(
        &mut self,
        path: &Path,
        transcript: &TranscriptRef,
    ) -> Result<CursorTranscriptSummary, SessionDetectorError> {
        const MAX_READ_ATTEMPTS: usize = 3;

        for _ in 0..MAX_READ_ATTEMPTS {
            let (stamp, summary) = self.summary_for_once(path, transcript)?;
            let after = file_stamp(path)?;
            if stamp == after {
                return Ok(summary);
            }
            self.cache.remove(path);
        }

        Err(SessionDetectorError::Parse(format!(
            "Cursor transcript changed while reading: {}",
            path.display()
        )))
    }

    fn summary_for_once(
        &mut self,
        path: &Path,
        transcript: &TranscriptRef,
    ) -> Result<(FileStamp, CursorTranscriptSummary), SessionDetectorError> {
        let stamp = file_stamp(path)?;
        if let Some(cached) = self.cache.get(path) {
            if cached.stamp == stamp && stamp.supports_unchanged_fast_path() {
                return Ok((stamp, cached.summary.clone()));
            }
        }
        let (
            existing,
            start_offset,
            resumed_hash,
            resumed_full_verification,
            inherited_next_full_verify_offset,
        ) = match self.cache.get(path) {
            Some(cached) if stamp.len > cached.offset => {
                let full_verification = cached.offset <= EXACT_PREFIX_VERIFY_LIMIT
                    || stamp.len >= cached.next_full_verify_offset;
                let verified_bytes = if full_verification {
                    verify_prefix(path, cached.offset, cached.prefix_hash)
                } else {
                    verify_prefix_guard(path, cached.prefix_guard)
                };
                match verified_bytes {
                    Some(_bytes) => {
                        #[cfg(test)]
                        {
                            self.prefix_verification_bytes =
                                self.prefix_verification_bytes.saturating_add(_bytes);
                        }
                        (
                            Some(cached.summary.clone()),
                            cached.offset,
                            Some(cached.prefix_hash),
                            full_verification,
                            Some(cached.next_full_verify_offset),
                        )
                    }
                    None => (None, 0, None, false, None),
                }
            }
            _ => (None, 0, None, false, None),
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
        let full_prefix_validation = resumed_hash.is_none() || resumed_full_verification;
        let snapshot = if let Some(mut hash) = resumed_hash {
            // Extend the verified prefix hash over the appended bytes instead
            // of re-hashing the whole range. Large files use a bounded guard
            // between geometrically scheduled full-prefix validations.
            feed_suffix(path, start_offset, offset, &mut hash).ok_or_else(|| {
                SessionDetectorError::Parse(format!(
                    "Cursor transcript suffix changed while hashing: {}",
                    path.display()
                ))
            })?;
            PrefixSnapshot {
                hash,
                guard: hash_prefix_guard(path, offset)
                    .ok_or_else(|| {
                        SessionDetectorError::Parse(format!(
                            "Cursor transcript prefix guard could not be hashed: {}",
                            path.display()
                        ))
                    })?
                    .0,
            }
        } else {
            hash_file_prefix(path, offset).ok_or_else(|| {
                SessionDetectorError::Parse(format!(
                    "Cursor transcript prefix could not be hashed: {}",
                    path.display()
                ))
            })?
        };
        let next_full_verify_offset = if full_prefix_validation {
            next_full_verify_offset(offset)
        } else {
            inherited_next_full_verify_offset.unwrap_or_else(|| next_full_verify_offset(offset))
        };
        self.cache.insert(
            path.to_path_buf(),
            CacheEntry {
                stamp,
                offset,
                prefix_hash: snapshot.hash,
                prefix_guard: snapshot.guard,
                next_full_verify_offset,
                summary: summary.clone(),
            },
        );
        Ok((stamp, summary))
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
    let mut summary = parse_transcript(&transcript.path, &transcript, false)
        .map_err(|error| error.to_string())?;
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
    let composers = load_composer_map(default_vscdb_path(home_dir).as_deref());
    collect_transcript_refs(&projects_root)
        .into_iter()
        .filter(|item| item.agent_kind == AgentKind::Root)
        .filter_map(|item| {
            let mut summary = parse_transcript(&item.path, &item, true).ok()?;
            apply_composer_overlay(&mut summary, composers.get(&item.session_id));
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
                custom_title: None,
                codex_title: None,
                cursor_title: summary.cursor_title,
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

fn apply_transcript_line(
    summary: &mut CursorTranscriptSummary,
    value: &Value,
    compact: bool,
) -> bool {
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
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
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

fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Feed `[start, end)` into an incremental FNV state. Returns `None` if the
/// file is shorter than `end`, unreadable, or ends prematurely.
fn feed_hash_range(path: &Path, start: u64, end: u64, hash: &mut u64) -> Option<u64> {
    if end < start {
        return None;
    }
    let mut file = File::open(path).ok()?;
    if fs::metadata(path).ok()?.len() < end {
        return None;
    }
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut remaining = end - start;
    let mut bytes_read = 0;
    while remaining > 0 {
        let want = buf.len().min(remaining as usize);
        let read = file.read(&mut buf[..want]).ok()?;
        if read == 0 {
            return None;
        }
        *hash = hash_bytes(*hash, &buf[..read]);
        remaining -= read as u64;
        bytes_read += read as u64;
    }
    Some(bytes_read)
}

fn hash_range(path: &Path, start: u64, end: u64) -> Option<(u64, u64)> {
    let mut hash = FNV_OFFSET_BASIS;
    let bytes_read = feed_hash_range(path, start, end, &mut hash)?;
    Some((hash, bytes_read))
}

/// Verify the complete cached prefix. Small transcripts use this on every
/// append; large transcripts use it at geometrically scheduled checkpoints.
fn verify_prefix(path: &Path, len: u64, expected_hash: u64) -> Option<u64> {
    let (hash, bytes_read) = hash_range(path, 0, len)?;
    (hash == expected_hash).then_some(bytes_read)
}

fn hash_prefix_guard(path: &Path, len: u64) -> Option<(PrefixGuard, u64)> {
    let head_len = len.min(PREFIX_GUARD_BYTES);
    let (head_hash, head_bytes) = hash_range(path, 0, head_len)?;
    if len <= PREFIX_GUARD_BYTES {
        return Some((
            PrefixGuard {
                len,
                head_hash,
                tail_hash: head_hash,
            },
            head_bytes,
        ));
    }

    let tail_start = len.saturating_sub(PREFIX_GUARD_BYTES);
    let (tail_hash, tail_bytes) = hash_range(path, tail_start, len)?;
    Some((
        PrefixGuard {
            len,
            head_hash,
            tail_hash,
        },
        head_bytes + tail_bytes,
    ))
}

/// Verify fixed-size head/tail guards between full prefix checkpoints. This is
/// intentionally bounded I/O: an arbitrary rewrite in the unguarded middle of
/// a very large transcript is detected at the next geometric full checkpoint.
fn verify_prefix_guard(path: &Path, expected: PrefixGuard) -> Option<u64> {
    let (current, bytes_read) = hash_prefix_guard(path, expected.len)?;
    (current == expected).then_some(bytes_read)
}

/// Hash the first `len` bytes and compute the bounded guard in the same cache
/// update. The full hash is needed to extend the incremental state later.
fn hash_file_prefix(path: &Path, len: u64) -> Option<PrefixSnapshot> {
    let (hash, _) = hash_range(path, 0, len)?;
    let (guard, _) = hash_prefix_guard(path, len)?;
    Some(PrefixSnapshot { hash, guard })
}

/// Feed the bytes of `path` in `[start, end)` into `hash`.
///
/// A failure is reported to the caller so an incomplete hash can never be
/// persisted as if it represented the full cached prefix.
fn feed_suffix(path: &Path, start: u64, end: u64, hash: &mut u64) -> Option<()> {
    feed_hash_range(path, start, end, hash).map(|_| ())
}

fn next_full_verify_offset(offset: u64) -> u64 {
    if offset <= EXACT_PREFIX_VERIFY_LIMIT {
        return offset;
    }
    let growth = (offset / FULL_VERIFY_GROWTH_DIVISOR).max(MIN_FULL_VERIFY_GROWTH_BYTES);
    offset.saturating_add(growth)
}

fn file_stamp(path: &Path) -> Result<FileStamp, SessionDetectorError> {
    Ok(FileVersion::read(path)?)
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

fn apply_composer_overlay(
    summary: &mut CursorTranscriptSummary,
    overlay: Option<&ComposerOverlay>,
) {
    let Some(overlay) = overlay else {
        return;
    };
    if let Some(name) = overlay.name.clone() {
        summary.cursor_title = Some(name.clone());
        // Preserve the existing nickname projection for subagent labels and
        // older consumers while exposing the provider title separately.
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

fn read_composer_overlays(
    path: &Path,
) -> Result<HashMap<String, ComposerOverlay>, rusqlite::Error> {
    let uri = format!("file:{}?mode=ro", path.display());
    let conn = rusqlite::Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    let _ = conn.busy_timeout(Duration::from_millis(50));
    let mut stmt =
        conn.prepare("SELECT key, value FROM cursorDiskKV WHERE key LIKE 'composerData:%'")?;
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
                &tool_line(
                    "Task",
                    r#"{"subagent_type":"explore","description":"scan"}"#,
                ),
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
        let with_tools =
            find_cursor_conversation_under(tmp.path(), PARENT, true, &mut |_, _| {}).unwrap();
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

        let mut source = CursorSessionSource::at_root_with_vscdb(tmp.path().to_path_buf(), db_path);
        let (sessions, _) = source.detect().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].agent_nickname.as_deref(),
            Some("Research Cursor agent sessions")
        );
        assert_eq!(
            sessions[0]
                .cursor_summary
                .as_ref()
                .unwrap()
                .cursor_title
                .as_deref(),
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
    fn cache_evicts_transcripts_past_visibility_ceiling() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        let path = session_dir.join(format!("{PARENT}.jsonl"));
        write_jsonl(
            &path,
            &[
                &user_line("old chat"),
                r#"{"type":"turn_ended","status":"success"}"#,
            ],
        );

        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (sessions, _) = source.detect().unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(source.cache.contains_key(&path));

        let stale = SystemTime::now() + Duration::from_secs(LINKED_PARENT_CEILING_SECS + 60);
        let (sessions, _) = source.detect_at(stale).unwrap();
        assert!(sessions.is_empty());
        assert!(source.cache.is_empty());
        let parse_count = source.parse_count;

        let later = stale + Duration::from_secs(60);
        let (sessions, _) = source.detect_at(later).unwrap();
        assert!(sessions.is_empty());
        assert_eq!(source.parse_count, parse_count);
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
        assert_eq!(
            enriched[0].status,
            crate::session::SessionStatus::Connecting
        );
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
        assert!(
            entries[0].timestamp > 0,
            "history should sort by last write, not birth"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn history_applies_cursor_composer_title_overlay() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = tmp.path().join(".cursor").join("projects");
        let session_dir = layout(&projects, "demo-project");
        write_jsonl(
            &session_dir.join(format!("{PARENT}.jsonl")),
            &[&user_line("root prompt")],
        );

        let db_path = default_vscdb_path(tmp.path()).unwrap();
        fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        let payload = serde_json::json!({"name": "Cursor auto title"});
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            rusqlite::params![format!("composerData:{PARENT}"), payload.to_string()],
        )
        .unwrap();
        drop(conn);

        let entries = cursor_history_entries(tmp.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].custom_title, None);
        assert_eq!(
            entries[0].cursor_title.as_deref(),
            Some("Cursor auto title")
        );
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
        assert_eq!(
            source.parse_count, parsed,
            "unchanged file must be a cache hit"
        );

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
    fn deleted_and_renamed_transcripts_are_removed_from_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        let path = session_dir.join(format!("{PARENT}.jsonl"));
        write_jsonl(&path, &[&user_line("will disappear")]);

        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (sessions, _) = source.detect().unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(source.cache.contains_key(&path));

        fs::remove_file(&path).unwrap();
        let (sessions, _) = source.detect().unwrap();
        assert!(sessions.is_empty());
        assert!(!source.cache.contains_key(&path));

        let renamed_dir = tmp
            .path()
            .join("demo-project")
            .join("agent-transcripts")
            .join(CHILD_DONE);
        fs::rename(&session_dir, &renamed_dir).unwrap();
        let renamed_path = renamed_dir.join(format!("{CHILD_DONE}.jsonl"));
        write_jsonl(&renamed_path, &[&user_line("renamed session")]);

        let (sessions, _) = source.detect().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id.as_deref(), Some(CHILD_DONE));
        assert!(!source.cache.contains_key(&path));
        assert!(source.cache.contains_key(&renamed_path));
    }

    #[test]
    fn large_transcript_incremental_parse_matches_full_parse() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        let path = session_dir.join(format!("{PARENT}.jsonl"));
        let mut lines = vec![user_line("large transcript")];
        for index in 0..8_192 {
            lines.push(assistant_line(&format!("synthetic message {index}")));
        }
        let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_jsonl(&path, &line_refs);

        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (first, _) = source.detect().unwrap();
        let initial = first[0].cursor_summary.as_ref().unwrap().messages.len();
        assert_eq!(initial, 8_193);

        let appended = assistant_line("synthetic appended message");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{appended}").unwrap();
        drop(file);

        let (incremental, _) = source.detect().unwrap();
        let mut fresh = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (full, _) = fresh.detect().unwrap();
        let incremental_messages = &incremental[0].cursor_summary.as_ref().unwrap().messages;
        let full_messages = &full[0].cursor_summary.as_ref().unwrap().messages;
        assert_eq!(incremental_messages.len(), full_messages.len());
        for (incremental, full) in incremental_messages.iter().zip(full_messages) {
            assert_eq!(incremental.role, full.role);
            assert_eq!(incremental.message_type, full.message_type);
            assert_eq!(incremental.content, full.content);
        }
        assert_eq!(
            incremental[0]
                .cursor_summary
                .as_ref()
                .unwrap()
                .messages
                .len(),
            initial + 1
        );
    }

    #[test]
    fn large_append_prefix_validation_has_a_bounded_read_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        let path = session_dir.join(format!("{PARENT}.jsonl"));
        let filler = "x".repeat(1024);
        let line = format!(r#"{{"type":"synthetic_padding","padding":"{filler}"}}"#);
        let line_count = (EXACT_PREFIX_VERIFY_LIMIT as usize / line.len()) + 128;
        let lines = vec![line; line_count];
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_jsonl(&path, &refs);

        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        source.detect().unwrap();
        let cached = source.cache.get(&path).unwrap();
        assert!(cached.offset > EXACT_PREFIX_VERIFY_LIMIT);

        let before = source.prefix_verification_bytes_for_test();
        for index in 0..16 {
            let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(file, r#"{{"type":"synthetic_append","index":{index}}}"#).unwrap();
            drop(file);
            source.detect().unwrap();
        }
        let verified = source.prefix_verification_bytes_for_test() - before;
        let max_expected = 16 * PREFIX_GUARD_BYTES * 2;
        assert!(
            verified <= max_expected,
            "large append validation read {verified} bytes, expected at most {max_expected}"
        );
    }

    #[test]
    fn large_append_reaches_checkpoint_and_detects_middle_rewrite() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        let path = session_dir.join(format!("{PARENT}.jsonl"));
        let padding = "x".repeat(16 * 1024);
        let padding_line = format!(r#"{{"type":"synthetic_padding","padding":"{padding}"}}"#);
        let middle_old = user_line("middle old");
        let middle_new = user_line("middle new");
        assert_eq!(middle_old.len(), middle_new.len());

        let mut file = File::create(&path).unwrap();
        writeln!(file, "{}", user_line("head")).unwrap();
        for _ in 0..160 {
            writeln!(file, "{padding_line}").unwrap();
        }
        let middle_offset = file.stream_position().unwrap();
        writeln!(file, "{middle_old}").unwrap();
        for _ in 0..160 {
            writeln!(file, "{padding_line}").unwrap();
        }
        drop(file);

        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (initial, _) = source.detect().unwrap();
        let initial_summary = initial[0].cursor_summary.as_ref().unwrap();
        assert!(initial_summary
            .messages
            .iter()
            .any(|message| message.content == "middle old"));
        let initial_checkpoint = source.cache.get(&path).unwrap().next_full_verify_offset;
        assert!(source.cache.get(&path).unwrap().offset > EXACT_PREFIX_VERIFY_LIMIT);

        for index in 0..8 {
            let mut append = fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(append, r#"{{"type":"synthetic_append","index":{index}}}"#).unwrap();
            drop(append);
            source.detect().unwrap();
        }
        assert_eq!(
            source.cache.get(&path).unwrap().next_full_verify_offset,
            initial_checkpoint,
            "guard-only appends must not move the scheduled full checkpoint"
        );

        let mut rewrite = fs::OpenOptions::new().write(true).open(&path).unwrap();
        rewrite.seek(SeekFrom::Start(middle_offset)).unwrap();
        rewrite.write_all(middle_new.as_bytes()).unwrap();
        drop(rewrite);

        let mut detected_rewrite = false;
        for _ in 8..96 {
            let mut append = fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(append, "{padding_line}").unwrap();
            drop(append);
            let (sessions, _) = source.detect().unwrap();
            let summary = sessions[0].cursor_summary.as_ref().unwrap();
            if summary
                .messages
                .iter()
                .any(|message| message.content == "middle new")
            {
                detected_rewrite = true;
                break;
            }
        }
        assert!(
            detected_rewrite,
            "a middle rewrite must be detected when the fixed checkpoint is reached"
        );
    }

    #[test]
    fn concurrent_torn_write_is_ignored_then_committed_once() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        let path = session_dir.join(format!("{PARENT}.jsonl"));
        write_jsonl(&path, &[&user_line("before concurrent write")]);

        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        source.detect().unwrap();

        let assistant = assistant_line("concurrent assistant");
        let split = assistant.len() / 2;
        let (partial_ready_tx, partial_ready_rx) = std::sync::mpsc::channel();
        let (continue_tx, continue_rx) = std::sync::mpsc::channel();
        let (complete_tx, complete_rx) = std::sync::mpsc::channel();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open(writer_path)
                .unwrap();
            file.write_all(&assistant.as_bytes()[..split]).unwrap();
            file.flush().unwrap();
            partial_ready_tx.send(()).unwrap();
            continue_rx.recv().unwrap();
            file.write_all(&assistant.as_bytes()[split..]).unwrap();
            writeln!(file).unwrap();
            file.flush().unwrap();
            complete_tx.send(()).unwrap();
        });

        partial_ready_rx.recv().unwrap();
        let (during_write, _) = source.detect().unwrap();
        assert_eq!(
            during_write[0]
                .cursor_summary
                .as_ref()
                .unwrap()
                .messages
                .len(),
            1
        );
        continue_tx.send(()).unwrap();
        complete_rx.recv().unwrap();
        writer.join().unwrap();

        let (after_write, _) = source.detect().unwrap();
        let messages = &after_write[0].cursor_summary.as_ref().unwrap().messages;
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.content == "concurrent assistant")
                .count(),
            1
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
        assert_eq!(
            enriched[0].status,
            crate::session::SessionStatus::WaitingForInput
        );
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
    fn same_length_rewrite_with_preserved_mtime_is_not_a_cache_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        let path = session_dir.join(format!("{PARENT}.jsonl"));
        let old = user_line("old prompt");
        let fresh = user_line("new prompt");
        assert_eq!(old.len(), fresh.len());
        write_jsonl(&path, &[&old]);
        let original_modified = fs::metadata(&path).unwrap().modified().unwrap();

        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (first, _) = source.detect().unwrap();
        assert_eq!(
            first[0].cursor_summary.as_ref().unwrap().first_prompt(),
            Some("old prompt")
        );

        write_jsonl(&path, &[&fresh]);
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(original_modified).unwrap();
        drop(file);

        let (second, _) = source.detect().unwrap();
        assert_eq!(
            second[0].cursor_summary.as_ref().unwrap().first_prompt(),
            Some("new prompt")
        );
    }

    #[test]
    fn suffix_hash_failure_is_reported_instead_of_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("missing-transcript.jsonl");
        let mut hash = FNV_OFFSET_BASIS;

        assert!(feed_suffix(&path, 0, 1, &mut hash).is_none());
    }

    #[test]
    fn truncated_then_rewritten_longer_matches_full_parse() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        let path = session_dir.join(format!("{PARENT}.jsonl"));
        write_jsonl(
            &path,
            &[&user_line("old prompt"), &assistant_line("old answer")],
        );

        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (first, _) = source.detect().unwrap();
        assert_eq!(
            first[0].cursor_summary.as_ref().unwrap().first_prompt(),
            Some("old prompt")
        );
        let cached_offset = source.cache.get(&path).unwrap().offset;

        // Truncate and rewrite with content that is LONGER than the cached offset.
        write_jsonl(
            &path,
            &[
                &user_line("fresh prompt"),
                &assistant_line("fresh answer"),
                r#"{"type":"turn_ended","status":"success"}"#,
            ],
        );
        let new_len = fs::metadata(&path).unwrap().len();
        assert!(new_len > cached_offset, "fixture must exceed cached offset");

        let (second, _) = source.detect().unwrap();
        let summary = second[0].cursor_summary.as_ref().unwrap();

        // Ground truth: a fresh source doing a full parse of the same file.
        let mut fresh = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (expected_sessions, _) = fresh.detect().unwrap();
        let expected = expected_sessions[0].cursor_summary.as_ref().unwrap();

        assert_eq!(
            summary.messages, expected.messages,
            "no stale messages may survive a truncate+rewrite"
        );
        assert_eq!(summary.first_prompt(), Some("fresh prompt"));
        assert_eq!(summary.lifecycle, expected.lifecycle);
        assert_eq!(summary.lifecycle, CursorLifecycle::Idle);
    }

    #[test]
    fn partial_line_is_ignored_until_fully_written() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = layout(tmp.path(), "demo-project");
        let path = session_dir.join(format!("{PARENT}.jsonl"));
        write_jsonl(&path, &[&user_line("hello")]);

        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (first, _) = source.detect().unwrap();
        assert_eq!(first[0].cursor_summary.as_ref().unwrap().messages.len(), 1);

        // A torn write: bytes beyond the last complete line must not advance
        // the cached offset nor produce a message.
        let partial = assistant_line("half written");
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(partial[..partial.len() / 2].as_bytes())
            .unwrap();

        let (torn, _) = source.detect().unwrap();
        assert_eq!(torn[0].cursor_summary.as_ref().unwrap().messages.len(), 1);

        let (second, _) = source.detect().unwrap();
        assert_eq!(second[0].cursor_summary.as_ref().unwrap().messages.len(), 1);

        // Complete the line; it must appear exactly once.
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(partial[partial.len() / 2..].as_bytes())
            .unwrap();
        writeln!(file).unwrap();
        drop(file);

        let (third, _) = source.detect().unwrap();
        let messages = &third[0].cursor_summary.as_ref().unwrap().messages;
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.content == "half written")
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
                &tool_line(
                    "Task",
                    r#"{"subagent_type":"explore","description":"scan"}"#,
                ),
                r#"{"type":"turn_ended","status":"success"}"#,
            ],
        );
        let mut source = CursorSessionSource::at_root(tmp.path().to_path_buf());
        let (sessions, _) = source.detect().unwrap();
        assert!(sessions[0].agent_role.is_none());
    }
}
