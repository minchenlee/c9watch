use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::source::{
    AgentKind, DetectedSession, DetectionDiagnostics, SessionDetectorError, SessionKind,
    SessionProvider, SessionSource, SessionSurface,
};

/// Rollouts do not expose a supported process-liveness API. Keep recently idle
/// roots long enough to remain useful, but apply finite ceilings so crashed or
/// closed sessions eventually age out.
const IDLE_FRESHNESS_SECS: u64 = 30 * 60;
const WORKING_FRESHNESS_SECS: u64 = 4 * 60 * 60;
const LINKED_PARENT_CEILING_SECS: u64 = 24 * 60 * 60;
const DISCOVERY_RECENT_SECS: u64 = LINKED_PARENT_CEILING_SECS;
const FULL_DISCOVERY_INTERVAL_SECS: u64 = 60;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CodexLifecycle {
    Working,
    #[default]
    Idle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexMessage {
    pub timestamp: String,
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Default)]
pub struct CodexRolloutSummary {
    pub thread_id: String,
    pub cwd: PathBuf,
    pub surface: SessionSurface,
    pub agent_kind: AgentKind,
    pub parent_thread_id: Option<String>,
    pub root_session_id: Option<String>,
    pub agent_path: Option<String>,
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
    pub internal_kind: Option<String>,
    pub lifecycle: CodexLifecycle,
    pub messages: Vec<CodexMessage>,
    pub started_at_ms: Option<i64>,
    pub last_timestamp: String,
}

impl CodexRolloutSummary {
    pub fn first_prompt(&self) -> Option<&str> {
        self.messages
            .iter()
            .find(|message| message.role == "user")
            .map(|message| message.content.as_str())
    }

    pub fn latest_message(&self) -> Option<&str> {
        self.messages.last().map(|message| message.content.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified_nanos: u128,
    identity: u64,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    stamp: FileStamp,
    offset: u64,
    pending: Vec<u8>,
    summary: CodexRolloutSummary,
}

#[derive(Default)]
pub struct CodexSessionSource {
    sessions_root: PathBuf,
    cache: HashMap<PathBuf, CacheEntry>,
    archive_index: HashMap<String, PathBuf>,
    last_full_discovery: Option<SystemTime>,
    #[cfg(test)]
    parse_count: usize,
}

impl CodexSessionSource {
    pub fn new() -> Result<Self, SessionDetectorError> {
        let home = dirs::home_dir().ok_or(SessionDetectorError::HomeDirectoryNotFound)?;
        Ok(Self::at_root(home.join(".codex").join("sessions")))
    }

    fn at_root(sessions_root: PathBuf) -> Self {
        Self {
            sessions_root,
            cache: HashMap::new(),
            archive_index: HashMap::new(),
            last_full_discovery: None,
            #[cfg(test)]
            parse_count: 0,
        }
    }

    fn candidate_day_dirs_at(&self, now: DateTime<Local>) -> Vec<PathBuf> {
        [now, now - chrono::Duration::days(1)]
            .into_iter()
            .map(|day| {
                self.sessions_root
                    .join(day.format("%Y").to_string())
                    .join(day.format("%m").to_string())
                    .join(day.format("%d").to_string())
            })
            .collect()
    }

    fn rollout_paths_at(&mut self, now: DateTime<Local>, wall_now: SystemTime) -> Vec<PathBuf> {
        let mut paths: HashSet<PathBuf> = self
            .cache
            .keys()
            .filter(|path| rollout_age_secs(path, wall_now) <= DISCOVERY_RECENT_SECS)
            .cloned()
            .collect();
        for day_dir in self.candidate_day_dirs_at(now) {
            let Ok(entries) = fs::read_dir(day_dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let is_rollout = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"));
                if path.is_file() && is_rollout {
                    paths.insert(path);
                }
            }
        }

        let discovery_due = self.last_full_discovery.map_or(true, |last| {
            wall_now.duration_since(last).unwrap_or_default().as_secs()
                >= FULL_DISCOVERY_INTERVAL_SECS
        });
        if discovery_due {
            let mut next_index = HashMap::new();
            for path in collect_rollout_paths(&self.sessions_root) {
                if let Some(thread_id) = thread_id_from_rollout_filename(&path) {
                    next_index.insert(thread_id, path.clone());
                }
                if rollout_age_secs(&path, wall_now) <= DISCOVERY_RECENT_SECS {
                    paths.insert(path);
                }
            }
            self.archive_index = next_index;
            self.last_full_discovery = Some(wall_now);
        }

        let mut paths: Vec<_> = paths.into_iter().collect();
        paths.sort();
        paths
    }

    fn summary_for(&mut self, path: &Path) -> Result<CodexRolloutSummary, std::io::Error> {
        let metadata = fs::metadata(path)?;
        let modified_nanos = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let stamp = FileStamp {
            len: metadata.len(),
            modified_nanos,
            identity: file_identity(&metadata),
        };

        if let Some(entry) = self.cache.get(path) {
            if entry.stamp == stamp {
                return Ok(entry.summary.clone());
            }
        }

        let must_reset = self.cache.get(path).is_none_or(|entry| {
            stamp.identity != entry.stamp.identity
                || stamp.len < entry.offset
                || (stamp.len == entry.stamp.len
                    && stamp.modified_nanos != entry.stamp.modified_nanos)
        });

        let mut entry = if must_reset {
            CacheEntry {
                stamp,
                offset: 0,
                pending: Vec::new(),
                summary: CodexRolloutSummary::default(),
            }
        } else {
            self.cache.get(path).cloned().expect("cache entry exists")
        };

        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(entry.offset))?;
        let mut appended = Vec::new();
        file.read_to_end(&mut appended)?;
        entry.offset += appended.len() as u64;
        entry.pending.extend_from_slice(&appended);

        let complete_len = entry
            .pending
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |position| position + 1);
        let complete = entry.pending[..complete_len].to_vec();
        entry.pending.drain(..complete_len);
        for line in complete.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_slice::<Value>(line) {
                apply_rollout_event(&mut entry.summary, &value);
            }
        }
        entry.stamp = stamp;
        #[cfg(test)]
        {
            self.parse_count += 1;
        }
        let summary = entry.summary.clone();
        self.cache.insert(path.to_path_buf(), entry);
        Ok(summary)
    }

    fn detect_at(
        &mut self,
        now: DateTime<Local>,
    ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
        self.detect_at_with_clock(now, SystemTime::now())
    }

    fn detect_at_with_clock(
        &mut self,
        now: DateTime<Local>,
        wall_now: SystemTime,
    ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
        let paths = self.rollout_paths_at(now, wall_now);
        let mut summaries = Vec::new();
        for path in &paths {
            let Ok(summary) = self.summary_for(path) else {
                continue;
            };
            if !summary.thread_id.is_empty() {
                self.archive_index
                    .insert(summary.thread_id.clone(), path.clone());
            }
            summaries.push((path.clone(), summary, rollout_age_secs(path, wall_now)));
        }
        let mut summaries = merge_live_rollout_summaries(summaries);

        // A root rollout can be quiet while a spawned child is actively writing.
        // Pull its immediate/root ancestors from the cached filename index and pin
        // them only up to a finite ceiling.
        let linked_parent_ids: HashSet<String> = summaries
            .iter()
            .filter(|(_, summary, age)| {
                summary.agent_kind == AgentKind::Subagent
                    && summary.lifecycle == CodexLifecycle::Working
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
        let present_ids: HashSet<String> = summaries
            .iter()
            .map(|(_, summary, _)| summary.thread_id.clone())
            .collect();
        for thread_id in linked_parent_ids
            .iter()
            .filter(|id| !present_ids.contains(*id))
        {
            let Some(path) = self.archive_index.get(thread_id).cloned() else {
                continue;
            };
            let age = rollout_age_secs(&path, wall_now);
            if age > LINKED_PARENT_CEILING_SECS {
                continue;
            }
            if let Ok(summary) = self.summary_for(&path) {
                summaries.push((path, summary, age));
            }
        }

        let existing: HashSet<PathBuf> =
            summaries.iter().map(|(path, _, _)| path.clone()).collect();
        self.cache.retain(|path, _| existing.contains(path));

        let mut sessions = Vec::new();
        for (_path, summary, age_secs) in summaries {
            if summary.thread_id.is_empty() || summary.agent_kind == AgentKind::Internal {
                continue;
            }
            let freshness = if summary.lifecycle == CodexLifecycle::Working {
                WORKING_FRESHNESS_SECS
            } else {
                IDLE_FRESHNESS_SECS
            };
            let linked_parent = linked_parent_ids.contains(&summary.thread_id)
                && age_secs <= LINKED_PARENT_CEILING_SECS;
            if age_secs > freshness && !linked_parent {
                continue;
            }

            let cwd = summary.cwd.clone();
            let project_name = cwd
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Codex")
                .to_string();
            sessions.push(DetectedSession {
                pid: 0,
                cwd: cwd.clone(),
                project_path: cwd,
                session_id: Some(summary.thread_id.clone()),
                project_name,
                kind: SessionKind::Interactive,
                started_at_ms: summary.started_at_ms,
                official_name: summary.agent_nickname.clone(),
                cli_activity: None,
                provider: SessionProvider::Codex,
                surface: summary.surface,
                agent_kind: summary.agent_kind,
                parent_thread_id: summary.parent_thread_id.clone(),
                root_session_id: summary.root_session_id.clone(),
                agent_path: summary.agent_path.clone(),
                agent_nickname: summary.agent_nickname.clone(),
                agent_role: summary.agent_role.clone(),
                internal_kind: summary.internal_kind.clone(),
                can_open: false,
                can_stop: false,
                can_rename: false,
                codex_summary: Some(summary),
            });
        }
        Ok((sessions, DetectionDiagnostics::default()))
    }
}

type LiveRolloutSummary = (PathBuf, CodexRolloutSummary, u64);

fn merge_live_rollout_summaries(summaries: Vec<LiveRolloutSummary>) -> Vec<LiveRolloutSummary> {
    let mut merged: HashMap<String, LiveRolloutSummary> = HashMap::new();
    let mut thread_order = Vec::new();
    let mut unidentified = Vec::new();
    for incoming in summaries {
        if incoming.1.thread_id.is_empty() {
            unidentified.push(incoming);
            continue;
        }
        match merged.entry(incoming.1.thread_id.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                thread_order.push(entry.key().clone());
                entry.insert(incoming);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                merge_live_rollout_summary(entry.get_mut(), incoming);
            }
        }
    }
    unidentified.extend(
        thread_order
            .into_iter()
            .filter_map(|thread_id| merged.remove(&thread_id)),
    );
    unidentified
}

fn merge_live_rollout_summary(existing: &mut LiveRolloutSummary, mut incoming: LiveRolloutSummary) {
    let existing_timestamp = parse_timestamp_millis(&existing.1.last_timestamp).unwrap_or(i64::MIN);
    let incoming_timestamp = parse_timestamp_millis(&incoming.1.last_timestamp).unwrap_or(i64::MIN);
    let incoming_is_newer = incoming_timestamp > existing_timestamp
        || (incoming_timestamp == existing_timestamp
            && (incoming.2 < existing.2 || (incoming.2 == existing.2 && incoming.0 > existing.0)));

    let started_at_ms = match (existing.1.started_at_ms, incoming.1.started_at_ms) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left @ Some(_), None) => left,
        (None, right) => right,
    };
    let mut messages = std::mem::take(&mut existing.1.messages);
    messages.append(&mut incoming.1.messages);
    messages.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.role.cmp(&right.role))
            .then_with(|| left.content.cmp(&right.content))
    });
    messages.dedup();

    if incoming_is_newer {
        *existing = incoming;
    }
    existing.1.started_at_ms = started_at_ms;
    existing.1.messages = messages;
}

fn rollout_age_secs(path: &Path, wall_now: SystemTime) -> u64 {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(|modified| {
            wall_now
                .duration_since(modified)
                .unwrap_or_default()
                .as_secs()
        })
        .unwrap_or(u64::MAX)
}

/// A full discovery reads directory entries and metadata only. It is throttled
/// by `FULL_DISCOVERY_INTERVAL_SECS`; JSONL content is parsed only for recently
/// modified paths and linked ancestors.
fn collect_rollout_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = pending.pop() {
        if depth > 4 {
            continue;
        }
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push((path, depth + 1));
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
            {
                paths.push(path);
            }
        }
    }
    paths
}

fn thread_id_from_rollout_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if stem.len() < 36 {
        return None;
    }
    let candidate = &stem[stem.len() - 36..];
    uuid::Uuid::parse_str(candidate)
        .ok()
        .map(|id| id.to_string())
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata) -> u64 {
    0
}

impl SessionSource for CodexSessionSource {
    fn detect(
        &mut self,
    ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
        self.detect_at(Local::now())
    }

    fn backend_name(&self) -> &'static str {
        "codex-rollout"
    }
}

fn parse_timestamp_millis(timestamp: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
}

fn apply_rollout_event(summary: &mut CodexRolloutSummary, value: &Value) {
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !timestamp.is_empty() {
        summary.last_timestamp = timestamp.to_string();
    }
    let event_type = value.get("type").and_then(Value::as_str);
    let Some(payload) = value.get("payload") else {
        return;
    };
    match event_type {
        Some("session_meta") => apply_session_meta(summary, timestamp, payload),
        Some("event_msg") => apply_event_message(summary, timestamp, payload),
        _ => {}
    }
}

fn apply_session_meta(summary: &mut CodexRolloutSummary, timestamp: &str, payload: &Value) {
    summary.started_at_ms = parse_timestamp_millis(timestamp).or_else(|| {
        payload
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_timestamp_millis)
    });
    summary.thread_id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    summary.cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_default();
    summary.parent_thread_id = payload
        .get("parent_thread_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    summary.root_session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| Some(summary.thread_id.clone()));
    summary.agent_path = payload
        .get("agent_path")
        .and_then(Value::as_str)
        .map(str::to_string);
    summary.agent_nickname = payload
        .get("agent_nickname")
        .and_then(Value::as_str)
        .map(str::to_string);

    let originator = payload
        .get("originator")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let source = payload.get("source").unwrap_or(&Value::Null);
    summary.surface = classify_surface(source, originator);
    summary.agent_kind = AgentKind::Root;

    if let Some(subagent) = source.get("subagent") {
        if let Some(spawn) = subagent.get("thread_spawn") {
            summary.agent_kind = AgentKind::Subagent;
            summary.parent_thread_id = spawn
                .get("parent_thread_id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or(summary.parent_thread_id.take());
            summary.agent_path = spawn
                .get("agent_path")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or(summary.agent_path.take());
            summary.agent_nickname = spawn
                .get("agent_nickname")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or(summary.agent_nickname.take());
            summary.agent_role = spawn
                .get("agent_role")
                .and_then(Value::as_str)
                .map(str::to_string);
        } else if let Some(kind) = subagent.get("other").and_then(Value::as_str) {
            summary.agent_kind = AgentKind::Internal;
            summary.internal_kind = Some(kind.to_string());
        } else if let Some(kind) = subagent.as_str() {
            summary.agent_kind = AgentKind::Internal;
            summary.internal_kind = Some(kind.to_string());
        }
    }
}

fn classify_surface(source: &Value, originator: &str) -> SessionSurface {
    if matches!(originator, "Claude Code" | "Claude Cowork") {
        return SessionSurface::Integration;
    }
    match source.as_str() {
        Some("vscode") => SessionSurface::App,
        Some("cli") => SessionSurface::Cli,
        Some("exec") => SessionSurface::Exec,
        _ if originator == "Codex Desktop" => SessionSurface::App,
        _ if originator == "codex-tui" => SessionSurface::Cli,
        _ if originator == "codex_exec" => SessionSurface::Exec,
        _ => SessionSurface::Unknown,
    }
}

fn apply_event_message(summary: &mut CodexRolloutSummary, timestamp: &str, payload: &Value) {
    match payload.get("type").and_then(Value::as_str) {
        Some("user_message") | Some("agent_message") => {
            let role = if payload.get("type").and_then(Value::as_str) == Some("user_message") {
                "user"
            } else {
                "assistant"
            };
            if let Some(content) = payload.get("message").and_then(Value::as_str) {
                summary.messages.push(CodexMessage {
                    timestamp: timestamp.to_string(),
                    role: role.to_string(),
                    content: content.to_string(),
                });
            }
        }
        Some("task_started") => summary.lifecycle = CodexLifecycle::Working,
        Some("task_complete") | Some("turn_aborted") => summary.lifecycle = CodexLifecycle::Idle,
        Some("thread_rolled_back") => {
            let turns = payload
                .get("num_turns")
                .and_then(Value::as_u64)
                .unwrap_or(1) as usize;
            rollback_messages(&mut summary.messages, turns);
            summary.lifecycle = CodexLifecycle::Idle;
        }
        _ => {}
    }
}

fn rollback_messages(messages: &mut Vec<CodexMessage>, turns: usize) {
    let mut users_removed = 0;
    while users_removed < turns {
        let Some(message) = messages.pop() else {
            break;
        };
        if message.role == "user" {
            users_removed += 1;
        }
    }
}

pub fn find_codex_conversation(thread_id: &str) -> Result<Vec<CodexMessage>, String> {
    let home = dirs::home_dir().ok_or("Failed to get home directory")?;
    find_codex_conversation_under(&home.join(".codex").join("sessions"), thread_id)
}

fn find_codex_conversation_under(
    sessions_root: &Path,
    thread_id: &str,
) -> Result<Vec<CodexMessage>, String> {
    let suffix = format!("-{thread_id}.jsonl");
    let mut paths: Vec<_> = collect_rollout_paths(sessions_root)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&suffix))
        })
        .collect();
    paths.sort();
    if paths.is_empty() {
        return Err(format!("Codex session {thread_id} not found"));
    }

    let mut source = CodexSessionSource::at_root(sessions_root.to_path_buf());
    let mut messages = Vec::new();
    let mut parsed_any = false;
    let mut last_error = None;
    for path in paths {
        match source.summary_for(&path) {
            Ok(mut summary) => {
                parsed_any = true;
                messages.append(&mut summary.messages);
            }
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    if !parsed_any {
        return Err(last_error.unwrap_or_else(|| format!("Codex session {thread_id} not found")));
    }
    messages.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.role.cmp(&right.role))
            .then_with(|| left.content.cmp(&right.content))
    });
    messages.dedup();
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::io::Write;
    use std::time::Duration;
    use tempfile::TempDir;

    fn event(timestamp: &str, event_type: &str, payload: Value) -> String {
        serde_json::json!({"timestamp": timestamp, "type": event_type, "payload": payload})
            .to_string()
    }

    fn root_fixture(source: Value, originator: &str) -> Vec<String> {
        vec![
            event(
                "2026-07-13T00:00:00Z",
                "session_meta",
                serde_json::json!({
                    "id": "root-id", "cwd": "/tmp/project", "source": source,
                    "originator": originator
                }),
            ),
            event(
                "2026-07-13T00:00:01Z",
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "hello"}),
            ),
            event(
                "2026-07-13T00:00:02Z",
                "event_msg",
                serde_json::json!({"type": "task_started"}),
            ),
        ]
    }

    fn write_lines(path: &Path, lines: &[String], final_newline: bool) {
        let mut file = File::create(path).unwrap();
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                writeln!(file).unwrap();
            }
            write!(file, "{line}").unwrap();
        }
        if final_newline {
            writeln!(file).unwrap();
        }
    }

    fn set_modified_age(path: &Path, wall_now: SystemTime, age: Duration) {
        let modified = wall_now.checked_sub(age).unwrap();
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }

    fn session_ids(sessions: &[DetectedSession]) -> HashSet<&str> {
        sessions
            .iter()
            .filter_map(|session| session.session_id.as_deref())
            .collect()
    }

    #[test]
    fn parses_app_and_cli_roots() {
        let temp = TempDir::new().unwrap();
        let app = temp.path().join("app.jsonl");
        let cli = temp.path().join("cli.jsonl");
        write_lines(
            &app,
            &root_fixture(Value::String("vscode".into()), "Codex Desktop"),
            true,
        );
        write_lines(
            &cli,
            &root_fixture(Value::String("cli".into()), "codex-tui"),
            true,
        );
        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        assert_eq!(
            source.summary_for(&app).unwrap().surface,
            SessionSurface::App
        );
        assert_eq!(
            source.summary_for(&cli).unwrap().surface,
            SessionSurface::Cli
        );
    }

    #[test]
    fn parses_spawned_guardian_review_and_integration_sources() {
        let mut spawned = CodexRolloutSummary::default();
        apply_session_meta(
            &mut spawned,
            "2026-07-13T00:00:00Z",
            &serde_json::json!({
                "id": "child", "cwd": "/tmp", "originator": "Codex Desktop",
                "session_id": "root", "source": {"subagent": {"thread_spawn": {
                    "parent_thread_id": "parent", "agent_path": "/root/a",
                    "agent_nickname": "Ada", "agent_role": "reviewer"
                }}}
            }),
        );
        assert_eq!(spawned.agent_kind, AgentKind::Subagent);
        assert_eq!(spawned.parent_thread_id.as_deref(), Some("parent"));
        assert_eq!(spawned.root_session_id.as_deref(), Some("root"));

        let mut guardian = CodexRolloutSummary::default();
        apply_session_meta(
            &mut guardian,
            "2026-07-13T00:00:00Z",
            &serde_json::json!({
                "id": "g", "cwd": "/tmp", "originator": "Codex Desktop",
                "source": {"subagent": {"other": "guardian"}}
            }),
        );
        assert_eq!(guardian.agent_kind, AgentKind::Internal);
        assert_eq!(guardian.internal_kind.as_deref(), Some("guardian"));

        let mut review = CodexRolloutSummary::default();
        apply_session_meta(
            &mut review,
            "2026-07-13T00:00:00Z",
            &serde_json::json!({
                "id": "r", "cwd": "/tmp", "originator": "codex_exec",
                "source": {"subagent": "review"}
            }),
        );
        assert_eq!(review.agent_kind, AgentKind::Internal);
        assert_eq!(review.internal_kind.as_deref(), Some("review"));

        assert_eq!(
            classify_surface(&Value::String("exec".into()), "Claude Code"),
            SessionSurface::Integration
        );
    }

    #[test]
    fn lifecycle_complete_aborted_and_rollback_are_ordered() {
        let mut summary = CodexRolloutSummary::default();
        for kind in [
            "task_started",
            "task_complete",
            "task_started",
            "turn_aborted",
        ] {
            apply_event_message(&mut summary, "", &serde_json::json!({"type": kind}));
        }
        assert_eq!(summary.lifecycle, CodexLifecycle::Idle);
        for (role, message) in [
            ("user_message", "one"),
            ("agent_message", "answer"),
            ("user_message", "two"),
        ] {
            apply_event_message(
                &mut summary,
                "",
                &serde_json::json!({"type": role, "message": message}),
            );
        }
        apply_event_message(
            &mut summary,
            "",
            &serde_json::json!({"type": "thread_rolled_back", "num_turns": 1}),
        );
        assert_eq!(summary.messages.len(), 2);
        assert_eq!(summary.lifecycle, CodexLifecycle::Idle);
    }

    #[test]
    fn incremental_cache_handles_hits_partial_lines_and_truncation() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("rollout-test.jsonl");
        let lines = root_fixture(Value::String("cli".into()), "codex-tui");
        write_lines(&path, &lines[..2], true);
        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        assert_eq!(source.summary_for(&path).unwrap().messages.len(), 1);
        assert_eq!(source.parse_count, 1);
        assert_eq!(source.summary_for(&path).unwrap().messages.len(), 1);
        assert_eq!(source.parse_count, 1, "unchanged files must be cache hits");

        let partial = event(
            "2026-07-13T00:00:03Z",
            "event_msg",
            serde_json::json!({"type":"agent_message","message":"partial"}),
        );
        let split = partial.len() / 2;
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "{}", &partial[..split]).unwrap();
        drop(file);
        assert_eq!(source.summary_for(&path).unwrap().messages.len(), 1);
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{}", &partial[split..]).unwrap();
        drop(file);
        assert_eq!(source.summary_for(&path).unwrap().messages.len(), 2);

        write_lines(&path, &lines[..1], true);
        assert_eq!(source.summary_for(&path).unwrap().messages.len(), 0);
    }

    #[test]
    fn day_candidates_include_today_and_yesterday_across_midnight() {
        let source = CodexSessionSource::at_root(PathBuf::from("/sessions"));
        let now = Local
            .with_ymd_and_hms(2026, 7, 13, 0, 0, 1)
            .single()
            .unwrap();
        let dirs = source.candidate_day_dirs_at(now);
        assert!(dirs[0].ends_with("2026/07/13"));
        assert!(dirs[1].ends_with("2026/07/12"));
    }

    #[test]
    fn duplicate_live_rollouts_merge_into_the_newest_thread_state() {
        let temp = TempDir::new().unwrap();
        let now = Local::now();
        let day = temp.path().join(now.format("%Y/%m/%d").to_string());
        fs::create_dir_all(&day).unwrap();
        let old_path = day.join("rollout-2026-07-13T00-00-00-duplicate.jsonl");
        write_lines(
            &old_path,
            &[
                event(
                    "2026-07-13T00:00:00Z",
                    "session_meta",
                    serde_json::json!({"id":"duplicate","cwd":"/tmp/project","source":"cli","originator":"codex-tui"}),
                ),
                event(
                    "2026-07-13T00:00:01Z",
                    "event_msg",
                    serde_json::json!({"type":"user_message","message":"older prompt"}),
                ),
                event(
                    "2026-07-13T00:00:02Z",
                    "event_msg",
                    serde_json::json!({"type":"task_complete"}),
                ),
            ],
            true,
        );
        let new_path = day.join("rollout-2026-07-13T01-00-00-duplicate.jsonl");
        write_lines(
            &new_path,
            &[
                event(
                    "2026-07-13T01:00:00Z",
                    "session_meta",
                    serde_json::json!({"id":"duplicate","cwd":"/tmp/project","source":"cli","originator":"codex-tui"}),
                ),
                event(
                    "2026-07-13T01:00:01Z",
                    "event_msg",
                    serde_json::json!({"type":"user_message","message":"newer prompt"}),
                ),
                event(
                    "2026-07-13T01:00:02Z",
                    "event_msg",
                    serde_json::json!({"type":"task_started"}),
                ),
            ],
            true,
        );

        let wall_now = SystemTime::now();
        set_modified_age(&old_path, wall_now, Duration::from_secs(60));
        set_modified_age(&new_path, wall_now, Duration::from_secs(1));
        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        let (sessions, _) = source.detect_at_with_clock(now, wall_now).unwrap();

        assert_eq!(sessions.len(), 1);
        let summary = sessions[0].codex_summary.as_ref().unwrap();
        assert_eq!(summary.lifecycle, CodexLifecycle::Working);
        assert_eq!(summary.messages.len(), 2);
        assert_eq!(summary.latest_message(), Some("newer prompt"));
    }

    #[test]
    fn discovers_recently_modified_rollout_in_old_creation_directory() {
        let temp = TempDir::new().unwrap();
        let old_day = temp.path().join("2025/01/02");
        fs::create_dir_all(&old_day).unwrap();
        let path =
            old_day.join("rollout-2025-01-02T00-00-00-019f58e8-afcb-7681-bf1b-585420b500c3.jsonl");
        let mut lines = root_fixture(Value::String("cli".into()), "codex-tui");
        lines.push(event(
            "2026-07-13T00:00:03Z",
            "event_msg",
            serde_json::json!({"type":"task_complete"}),
        ));
        write_lines(&path, &lines, true);

        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        let (sessions, _) = source
            .detect_at_with_clock(Local::now(), SystemTime::now())
            .unwrap();
        assert!(session_ids(&sessions).contains("root-id"));
    }

    #[test]
    fn retains_idle_root_beyond_ninety_seconds_with_finite_ceiling() {
        let temp = TempDir::new().unwrap();
        let now = Local::now();
        let wall_now = SystemTime::now();
        let day = temp.path().join(now.format("%Y/%m/%d").to_string());
        fs::create_dir_all(&day).unwrap();
        let path = day.join("rollout-idle-root.jsonl");
        let mut lines = root_fixture(Value::String("vscode".into()), "Codex Desktop");
        lines.push(event(
            "2026-07-13T00:00:03Z",
            "event_msg",
            serde_json::json!({"type":"task_complete"}),
        ));
        write_lines(&path, &lines, true);
        set_modified_age(&path, wall_now, Duration::from_secs(5 * 60));

        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        let (sessions, _) = source.detect_at_with_clock(now, wall_now).unwrap();
        assert!(session_ids(&sessions).contains("root-id"));
    }

    #[test]
    fn retains_long_working_turn() {
        let temp = TempDir::new().unwrap();
        let now = Local::now();
        let wall_now = SystemTime::now();
        let day = temp.path().join(now.format("%Y/%m/%d").to_string());
        fs::create_dir_all(&day).unwrap();
        let path = day.join("rollout-working-root.jsonl");
        write_lines(
            &path,
            &root_fixture(Value::String("cli".into()), "codex-tui"),
            true,
        );
        set_modified_age(&path, wall_now, Duration::from_secs(30 * 60));

        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        let (sessions, _) = source.detect_at_with_clock(now, wall_now).unwrap();
        assert!(session_ids(&sessions).contains("root-id"));
    }

    #[test]
    fn active_child_pins_stale_root_but_not_forever() {
        const ROOT: &str = "019f58e8-afcb-7681-bf1b-585420b500c3";
        const CHILD: &str = "019f5986-4549-79e1-a409-0d09dcb7044c";
        let temp = TempDir::new().unwrap();
        let now = Local::now();
        let wall_now = SystemTime::now();
        let day = temp.path().join(now.format("%Y/%m/%d").to_string());
        fs::create_dir_all(&day).unwrap();
        let root_path = day.join(format!("rollout-2026-07-13T00-00-00-{ROOT}.jsonl"));
        write_lines(
            &root_path,
            &[
                event(
                    "2026-07-13T00:00:00Z",
                    "session_meta",
                    serde_json::json!({"id":ROOT,"cwd":"/tmp/project","source":"vscode","originator":"Codex Desktop"}),
                ),
                event(
                    "2026-07-13T00:00:01Z",
                    "event_msg",
                    serde_json::json!({"type":"task_complete"}),
                ),
            ],
            true,
        );
        set_modified_age(&root_path, wall_now, Duration::from_secs(2 * 60 * 60));
        let child_path = day.join(format!("rollout-2026-07-13T01-00-00-{CHILD}.jsonl"));
        write_lines(
            &child_path,
            &[
                event(
                    "2026-07-13T01:00:00Z",
                    "session_meta",
                    serde_json::json!({
                        "id":CHILD,"session_id":ROOT,"cwd":"/tmp/project",
                        "originator":"Codex Desktop","source":{"subagent":{"thread_spawn":{
                            "parent_thread_id":ROOT,"depth":1,"agent_path":"/root/child"
                        }}}
                    }),
                ),
                event(
                    "2026-07-13T01:00:01Z",
                    "event_msg",
                    serde_json::json!({"type":"task_started"}),
                ),
            ],
            true,
        );

        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        let (sessions, _) = source.detect_at_with_clock(now, wall_now).unwrap();
        let ids = session_ids(&sessions);
        assert!(ids.contains(ROOT));
        assert!(ids.contains(CHILD));

        set_modified_age(
            &root_path,
            wall_now,
            Duration::from_secs(LINKED_PARENT_CEILING_SECS + 1),
        );
        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        let (sessions, _) = source.detect_at_with_clock(now, wall_now).unwrap();
        assert!(!session_ids(&sessions).contains(ROOT));
    }

    #[test]
    fn response_items_are_never_conversation_messages() {
        let mut summary = CodexRolloutSummary::default();
        apply_rollout_event(
            &mut summary,
            &serde_json::json!({
                "timestamp":"2026-07-13T00:00:00Z", "type":"response_item",
                "payload":{"type":"message","role":"user","content":"injected"}
            }),
        );
        assert!(summary.messages.is_empty());
    }

    #[test]
    fn detector_hides_internal_agents_and_exposes_read_only_capabilities() {
        let temp = TempDir::new().unwrap();
        let now = Local::now();
        let day = temp
            .path()
            .join(now.format("%Y").to_string())
            .join(now.format("%m").to_string())
            .join(now.format("%d").to_string());
        fs::create_dir_all(&day).unwrap();
        let root_path = day.join("rollout-root-id.jsonl");
        write_lines(
            &root_path,
            &root_fixture(Value::String("vscode".into()), "Codex Desktop"),
            true,
        );
        let guardian_path = day.join("rollout-guardian-id.jsonl");
        write_lines(
            &guardian_path,
            &[event(
                "2026-07-13T00:00:00Z",
                "session_meta",
                serde_json::json!({
                    "id":"guardian-id", "cwd":"/tmp/project", "originator":"Codex Desktop",
                    "source":{"subagent":{"other":"guardian"}}
                }),
            )],
            true,
        );

        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        let (sessions, _) = source.detect_at(now).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].provider, SessionProvider::Codex);
        assert_eq!(sessions[0].agent_kind, AgentKind::Root);
        assert_eq!(
            sessions[0].started_at_ms,
            parse_timestamp_millis("2026-07-13T00:00:00Z")
        );
        assert!(!sessions[0].can_open);
        assert!(!sessions[0].can_stop);
        assert!(!sessions[0].can_rename);
    }

    #[test]
    fn conversation_lookup_uses_only_codex_event_messages() {
        let temp = TempDir::new().unwrap();
        let day = temp.path().join("2026/07/13");
        fs::create_dir_all(&day).unwrap();
        let path = day.join("rollout-2026-07-13T00-00-00-thread-id.jsonl");
        let mut lines = root_fixture(Value::String("cli".into()), "codex-tui");
        lines.push(event(
            "2026-07-13T00:00:04Z",
            "response_item",
            serde_json::json!({"type":"message", "role":"user", "content":"injected"}),
        ));
        write_lines(&path, &lines, true);
        let messages = find_codex_conversation_under(temp.path(), "thread-id").unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "hello");
    }

    #[test]
    fn conversation_lookup_merges_resumed_rollout_fragments() {
        let temp = TempDir::new().unwrap();
        let old_day = temp.path().join("2026/07/12");
        let new_day = temp.path().join("2026/07/13");
        fs::create_dir_all(&old_day).unwrap();
        fs::create_dir_all(&new_day).unwrap();
        let old_path = old_day.join("rollout-2026-07-12T23-00-00-thread-id.jsonl");
        write_lines(
            &old_path,
            &[
                event(
                    "2026-07-12T23:00:00Z",
                    "session_meta",
                    serde_json::json!({"id":"thread-id","cwd":"/tmp/project","source":"cli","originator":"codex-tui"}),
                ),
                event(
                    "2026-07-12T23:00:01Z",
                    "event_msg",
                    serde_json::json!({"type":"user_message","message":"first prompt"}),
                ),
            ],
            true,
        );
        let new_path = new_day.join("rollout-2026-07-13T01-00-00-thread-id.jsonl");
        write_lines(
            &new_path,
            &[
                event(
                    "2026-07-13T01:00:00Z",
                    "session_meta",
                    serde_json::json!({"id":"thread-id","cwd":"/tmp/project","source":"cli","originator":"codex-tui"}),
                ),
                event(
                    "2026-07-12T23:00:01Z",
                    "event_msg",
                    serde_json::json!({"type":"user_message","message":"first prompt"}),
                ),
                event(
                    "2026-07-13T01:00:01Z",
                    "event_msg",
                    serde_json::json!({"type":"agent_message","message":"resumed answer"}),
                ),
            ],
            true,
        );

        let messages = find_codex_conversation_under(temp.path(), "thread-id").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "first prompt");
        assert_eq!(messages[1].content, "resumed answer");
    }
}
