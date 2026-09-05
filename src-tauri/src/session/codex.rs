use chrono::{DateTime, Local};
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::parser::MessageType;
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
// Discovery only needs to cover sessions that could survive the active-session
// freshness filter. Older linked parents are resolved on demand through the
// filename index below, so parsing every rollout from the last 24 hours only
// increases memory without making another session visible.
const DISCOVERY_RECENT_SECS: u64 = WORKING_FRESHNESS_SECS;
const FULL_DISCOVERY_INTERVAL_SECS: u64 = 60;
// The monitor only renders 100 chars of the first prompt and 200 chars of the
// latest message. Keep enough content for those exact views while full
// conversations continue to be parsed on demand.
const MONITOR_MESSAGE_CHARS: usize = 200;
const MONITOR_HEADER_CAPTURE_BYTES: usize = 4096;
const FILE_SIGNATURE_SAMPLE_BYTES: u64 = 4096;
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
// The 64-bit prefix digest keeps replacement detection bounded; a theoretical
// digest collision is preferable to retaining the previous transcript bytes.

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
    pub message_type: MessageType,
    pub content: String,
    #[serde(skip)]
    content_identity: MessageIdentity,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
struct MessageIdentity {
    content_len: usize,
    content_hash: u64,
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
    identity: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FileSignature {
    prefix_hash: u64,
    checkpoint_hash: u64,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    stamp: FileStamp,
    offset: u64,
    signature: FileSignature,
    logical_prefix_hash: u64,
    summary: CodexRolloutSummary,
}

#[derive(Default)]
pub struct CodexSessionSource {
    sessions_root: PathBuf,
    cache: HashMap<PathBuf, CacheEntry>,
    archive_index: HashMap<String, Vec<PathBuf>>,
    last_full_discovery: Option<SystemTime>,
    /// When true, parse complete JSONL lines instead of the compact monitor path.
    capture_tool_messages: bool,
    /// When true (and using the full-parse path), keep Codex tool use/result records.
    include_tools: bool,
    #[cfg(test)]
    parse_count: usize,
    #[cfg(test)]
    reset_count: usize,
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
            capture_tool_messages: false,
            include_tools: false,
            #[cfg(test)]
            parse_count: 0,
            #[cfg(test)]
            reset_count: 0,
        }
    }

    fn conversation_at_root(sessions_root: PathBuf, include_tools: bool) -> Self {
        Self {
            capture_tool_messages: true,
            include_tools,
            ..Self::at_root(sessions_root)
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
                if path.is_file()
                    && is_rollout
                    && rollout_age_secs(&path, wall_now) <= DISCOVERY_RECENT_SECS
                {
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
                if rollout_age_secs(&path, wall_now) <= LINKED_PARENT_CEILING_SECS {
                    if let Some(thread_id) = thread_id_from_rollout_filename(&path) {
                        index_rollout_path(&mut next_index, thread_id, path.clone());
                    }
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
        self.summary_for_with_progress(path, None)
    }

    fn summary_for_with_progress(
        &mut self,
        path: &Path,
        mut on_progress: Option<&mut dyn FnMut(u64, u64)>,
    ) -> Result<CodexRolloutSummary, std::io::Error> {
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
                if let Some(cb) = on_progress.as_mut() {
                    cb(stamp.len, stamp.len);
                }
                return Ok(entry.summary.clone());
            }
        }

        let previous = self.cache.get(path).cloned();
        let must_reset = match previous.as_ref() {
            None => true,
            Some(entry) => {
                let structurally_replaced = stamp.identity != entry.stamp.identity
                    || stamp.len < entry.offset
                    || stamp.len < entry.stamp.len;
                if structurally_replaced {
                    true
                } else {
                    match file_signature(path, entry.offset) {
                        Ok(signature) if signature != entry.signature => true,
                        Ok(_) => {
                            if entry.offset <= FILE_SIGNATURE_SAMPLE_BYTES {
                                false
                            } else {
                                // Matching bounded samples is only the cheap
                                // append fast path. Recheck the complete
                                // logical prefix before trusting the cached
                                // summary, so a same-inode rewrite in the
                                // middle cannot leave stale messages behind.
                                File::open(path)
                                    .and_then(|mut file| {
                                        hash_file_region(&mut file, 0, entry.offset)
                                    })
                                    .map(|hash| hash != entry.logical_prefix_hash)
                                    .unwrap_or(true)
                            }
                        }
                        Err(_) => true,
                    }
                }
            }
        };

        #[cfg(test)]
        if must_reset {
            self.reset_count += 1;
        }

        let mut entry = if must_reset {
            CacheEntry {
                stamp,
                offset: 0,
                signature: FileSignature::default(),
                logical_prefix_hash: FNV_OFFSET_BASIS,
                summary: CodexRolloutSummary::default(),
            }
        } else {
            previous.expect("cache entry exists")
        };

        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(entry.offset))?;
        let mut reader = BufReader::new(file);
        if self.capture_tool_messages {
            let mut line = Vec::new();
            loop {
                let line_start = entry.offset;
                line.clear();
                let bytes_read = reader.read_until(b'\n', &mut line)?;
                if bytes_read == 0 {
                    break;
                }
                if line.last() != Some(&b'\n') {
                    // Keep the offset at the start of an incomplete record.
                    // The next poll rereads only this final line after it is
                    // resumed; no partial payload remains in the cache.
                    entry.offset = line_start;
                    break;
                }

                entry.offset = line_start + bytes_read as u64;
                entry.logical_prefix_hash = hash_bytes(entry.logical_prefix_hash, &line);
                apply_rollout_line(&mut entry.summary, &line, self.include_tools);
                if let Some(cb) = on_progress.as_mut() {
                    cb(entry.offset, stamp.len);
                }
            }
        } else {
            loop {
                let line_start = entry.offset;
                let record = read_monitor_line(&mut reader, entry.logical_prefix_hash)?;
                if record.bytes_read == 0 {
                    break;
                }
                if !record.complete {
                    entry.offset = line_start;
                    break;
                }

                entry.offset = line_start + record.bytes_read;
                entry.logical_prefix_hash = record.logical_prefix_hash;
                let Some(header) = record.header else {
                    continue;
                };
                let relevant = matches!(
                    header.event_type.as_deref(),
                    Some("session_meta" | "event_msg")
                );
                if relevant {
                    let line = match record.line {
                        Some(line) => line,
                        None => read_line_at(path, line_start, record.bytes_read)?,
                    };
                    apply_monitor_line(&mut entry.summary, &line);
                } else if let Some(timestamp) = header.timestamp.as_deref() {
                    entry.summary.last_timestamp = timestamp.to_string();
                }
            }
        }
        entry.stamp = stamp;
        entry.signature = file_signature(path, entry.offset).unwrap_or_default();
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
                index_rollout_path(
                    &mut self.archive_index,
                    summary.thread_id.clone(),
                    path.clone(),
                );
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
        let mut existing_paths: HashSet<PathBuf> =
            summaries.iter().map(|(path, _, _)| path.clone()).collect();
        for thread_id in &linked_parent_ids {
            let paths = self
                .archive_index
                .get(thread_id)
                .cloned()
                .unwrap_or_default();
            for path in paths {
                if existing_paths.contains(&path) {
                    continue;
                }
                let age = rollout_age_secs(&path, wall_now);
                if age > LINKED_PARENT_CEILING_SECS {
                    continue;
                }
                if let Ok(summary) = self.summary_for(&path) {
                    existing_paths.insert(path.clone());
                    summaries.push((path, summary, age));
                }
            }
        }
        summaries = merge_live_rollout_summaries(summaries);

        self.cache.retain(|path, _| existing_paths.contains(path));

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

#[derive(Clone, Deserialize)]
struct RolloutEventHeader {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    event_type: Option<String>,
}

struct RolloutHeaderVisitor<'a> {
    header: &'a mut Option<RolloutEventHeader>,
}

impl<'de, 'a> Visitor<'de> for RolloutHeaderVisitor<'a> {
    type Value = RolloutEventHeader;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a rollout JSON object")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut timestamp = None;
        let mut event_type = None;
        let mut timestamp_seen = false;
        let mut event_type_seen = false;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "timestamp" => {
                    timestamp_seen = true;
                    timestamp = map.next_value()?;
                }
                "type" => {
                    event_type_seen = true;
                    event_type = map.next_value()?;
                }
                _ => {
                    map.next_value::<de::IgnoredAny>()?;
                }
            }

            if timestamp_seen && event_type_seen {
                let header = RolloutEventHeader {
                    timestamp,
                    event_type,
                };
                *self.header = Some(header.clone());
                return Ok(header);
            }
        }

        Ok(RolloutEventHeader {
            timestamp,
            event_type,
        })
    }
}

fn apply_rollout_line(summary: &mut CodexRolloutSummary, line: &[u8], capture_tool_messages: bool) {
    let Ok(header) = serde_json::from_slice::<RolloutEventHeader>(line) else {
        return;
    };
    if let Some(timestamp) = header.timestamp.as_deref() {
        summary.last_timestamp = timestamp.to_string();
    }
    let event_type = header.event_type.as_deref();
    let relevant = match event_type {
        Some("session_meta" | "event_msg") => true,
        Some("response_item") => capture_tool_messages,
        _ => false,
    };
    if !relevant {
        return;
    }
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    apply_rollout_event_with_tools(summary, &value, capture_tool_messages, false);
}

fn apply_monitor_line(summary: &mut CodexRolloutSummary, line: &[u8]) {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    apply_rollout_event_with_tools(summary, &value, false, true);
}

fn truncate_monitor_message(message: &str) -> String {
    let mut chars = message.chars();
    let truncated: String = chars.by_ref().take(MONITOR_MESSAGE_CHARS).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn message_identity(content: &str) -> MessageIdentity {
    let content_hash = hash_bytes(FNV_OFFSET_BASIS, content.as_bytes());
    MessageIdentity {
        content_len: content.len(),
        content_hash,
    }
}

fn push_codex_message(
    summary: &mut CodexRolloutSummary,
    timestamp: &str,
    role: &str,
    message_type: MessageType,
    content: String,
    compact_for_monitor: bool,
) {
    let content_identity = message_identity(&content);
    let content = if compact_for_monitor {
        truncate_monitor_message(&content)
    } else {
        content
    };
    summary.messages.push(CodexMessage {
        timestamp: timestamp.to_string(),
        role: role.to_string(),
        message_type,
        content,
        content_identity,
    });
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
            .then_with(|| left.content_identity.cmp(&right.content_identity))
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

fn index_rollout_path(index: &mut HashMap<String, Vec<PathBuf>>, thread_id: String, path: PathBuf) {
    let paths = index.entry(thread_id).or_default();
    if !paths.contains(&path) {
        paths.push(path);
        paths.sort();
    }
}

struct MonitorLine {
    bytes_read: u64,
    complete: bool,
    logical_prefix_hash: u64,
    header: Option<RolloutEventHeader>,
    line: Option<Vec<u8>>,
}

/// A JSONL reader that stops at one newline. It retains only a small prefix
/// until the header identifies a relevant record; ignored records drain with a
/// fixed buffer, while relevant records are captured for one semantic parse.
struct JsonlLineReader<'a> {
    reader: &'a mut BufReader<File>,
    bytes_read: u64,
    logical_prefix_hash: u64,
    complete: bool,
    eof: bool,
    captured: Vec<u8>,
    capture_all: bool,
    capture_overflowed: bool,
}

impl<'a> JsonlLineReader<'a> {
    fn new(reader: &'a mut BufReader<File>, logical_prefix_hash: u64) -> Self {
        Self {
            reader,
            bytes_read: 0,
            logical_prefix_hash,
            complete: false,
            eof: false,
            captured: Vec::new(),
            capture_all: false,
            capture_overflowed: false,
        }
    }

    fn drain_to_newline(&mut self, capture_remainder: bool) -> io::Result<()> {
        if capture_remainder {
            self.capture_all = true;
        } else {
            self.captured.clear();
            self.capture_overflowed = true;
        }
        let mut scratch = [0u8; 4096];
        while !self.complete && !self.eof {
            if self.read(&mut scratch)? == 0 {
                break;
            }
        }
        Ok(())
    }
}

impl Read for JsonlLineReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.complete || self.eof {
            return Ok(0);
        }

        let buffer = self.reader.fill_buf()?;
        if buffer.is_empty() {
            self.eof = true;
            return Ok(0);
        }

        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let available = newline.map_or(buffer.len(), |position| position + 1);
        let bytes_to_copy = available.min(output.len());
        output[..bytes_to_copy].copy_from_slice(&buffer[..bytes_to_copy]);
        self.reader.consume(bytes_to_copy);
        if !self.capture_overflowed {
            if self.capture_all {
                self.captured.extend_from_slice(&output[..bytes_to_copy]);
            } else {
                let remaining_capacity =
                    MONITOR_HEADER_CAPTURE_BYTES.saturating_sub(self.captured.len());
                let capture_len = bytes_to_copy.min(remaining_capacity);
                self.captured.extend_from_slice(&output[..capture_len]);
                if capture_len < bytes_to_copy {
                    self.capture_overflowed = true;
                }
            }
        }
        self.bytes_read += bytes_to_copy as u64;
        self.logical_prefix_hash = hash_bytes(self.logical_prefix_hash, &output[..bytes_to_copy]);
        if newline.is_some_and(|position| bytes_to_copy == position + 1) {
            self.complete = true;
        }
        Ok(bytes_to_copy)
    }
}

fn read_monitor_line(
    reader: &mut BufReader<File>,
    logical_prefix_hash: u64,
) -> io::Result<MonitorLine> {
    let mut line_reader = JsonlLineReader::new(reader, logical_prefix_hash);
    // serde_json calls end_map after the visitor returns. Keep the early
    // header separately so that end_map cannot force a payload traversal.
    let mut peeked_header = None;
    let parsed_header = {
        let mut deserializer = serde_json::Deserializer::from_reader(&mut line_reader);
        (&mut deserializer).deserialize_map(RolloutHeaderVisitor {
            header: &mut peeked_header,
        })
    };
    let header = parsed_header.ok().or(peeked_header);
    let relevant = header.as_ref().is_some_and(|header| {
        matches!(
            header.event_type.as_deref(),
            Some("session_meta" | "event_msg")
        )
    });
    line_reader.drain_to_newline(relevant)?;
    let line = if relevant && line_reader.complete && !line_reader.capture_overflowed {
        Some(std::mem::take(&mut line_reader.captured))
    } else {
        None
    };
    Ok(MonitorLine {
        bytes_read: line_reader.bytes_read,
        complete: line_reader.complete,
        logical_prefix_hash: line_reader.logical_prefix_hash,
        header,
        line,
    })
}

fn read_line_at(path: &Path, offset: u64, bytes: u64) -> io::Result<Vec<u8>> {
    // Rare fallback for a relevant record whose header appears beyond the
    // bounded capture prefix. Ordinary records are captured and parsed once.
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut line = Vec::new();
    file.take(bytes).read_to_end(&mut line)?;
    Ok(line)
}

/// Read only bounded samples from the already parsed logical prefix. The
/// sample end must not move when a file grows, otherwise an ordinary append
/// would look like a replacement for small rollouts.
fn file_signature(path: &Path, logical_len: u64) -> io::Result<FileSignature> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let logical_end = logical_len.min(file_len);
    let prefix_len = logical_end.min(FILE_SIGNATURE_SAMPLE_BYTES);
    let prefix_hash = hash_file_region(&mut file, 0, prefix_len)?;
    let checkpoint_end = logical_end;
    let checkpoint_start = checkpoint_end.saturating_sub(FILE_SIGNATURE_SAMPLE_BYTES);
    let checkpoint_hash = hash_file_region(
        &mut file,
        checkpoint_start,
        checkpoint_end.saturating_sub(checkpoint_start),
    )?;
    Ok(FileSignature {
        prefix_hash,
        checkpoint_hash,
    })
}

fn hash_file_region(file: &mut File, start: u64, len: u64) -> io::Result<u64> {
    file.seek(SeekFrom::Start(start))?;
    let mut remaining = len;
    let mut buffer = [0u8; 4096];
    let mut hash = FNV_OFFSET_BASIS;
    while remaining > 0 {
        let read_len = remaining.min(buffer.len() as u64) as usize;
        let bytes_read = file.read(&mut buffer[..read_len])?;
        if bytes_read == 0 {
            break;
        }
        hash = hash_bytes(hash, &buffer[..bytes_read]);
        remaining -= bytes_read as u64;
    }
    Ok(hash)
}

fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
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

fn thread_rollout_suffix(thread_id: &str) -> String {
    format!("-{thread_id}.jsonl")
}

fn path_matches_thread(path: &Path, suffix: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(suffix))
}

fn collect_thread_rollouts(sessions_root: &Path, suffix: &str) -> Vec<PathBuf> {
    collect_rollout_paths(sessions_root)
        .into_iter()
        .filter(|path| path_matches_thread(path, suffix))
        .collect()
}

/// Cache is only a seed. Always walk so resume/compaction fragments outside
/// today/yesterday are not dropped just because an older cached path still exists.
fn resolve_thread_rollout_paths(sessions_root: &Path, thread_id: &str) -> Vec<PathBuf> {
    resolve_thread_rollout_paths_with_cache(
        sessions_root,
        thread_id,
        super::codex_archive::cached_thread_paths(sessions_root, thread_id),
    )
}

fn resolve_thread_rollout_paths_with_cache(
    sessions_root: &Path,
    thread_id: &str,
    cached: Option<Vec<PathBuf>>,
) -> Vec<PathBuf> {
    let suffix = thread_rollout_suffix(thread_id);
    let mut paths = cached.unwrap_or_default();
    for path in collect_thread_rollouts(sessions_root, &suffix) {
        if !paths.iter().any(|existing| existing == &path) {
            paths.push(path);
        }
    }
    paths.retain(|path| path.is_file());
    paths.sort();
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

#[cfg(test)]
fn apply_rollout_event(summary: &mut CodexRolloutSummary, value: &Value) {
    apply_rollout_event_with_tools(summary, value, false, true);
}

fn apply_rollout_event_with_tools(
    summary: &mut CodexRolloutSummary,
    value: &Value,
    capture_tool_messages: bool,
    compact_for_monitor: bool,
) {
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
        Some("event_msg") => apply_event_message(summary, timestamp, payload, compact_for_monitor),
        Some("response_item") if capture_tool_messages => {
            apply_response_item(summary, timestamp, payload)
        }
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

fn apply_event_message(
    summary: &mut CodexRolloutSummary,
    timestamp: &str,
    payload: &Value,
    compact_for_monitor: bool,
) {
    match payload.get("type").and_then(Value::as_str) {
        Some("user_message") | Some("agent_message") => {
            let role = if payload.get("type").and_then(Value::as_str) == Some("user_message") {
                "user"
            } else {
                "assistant"
            };
            if let Some(content) = payload.get("message").and_then(Value::as_str) {
                push_codex_message(
                    summary,
                    timestamp,
                    role,
                    if role == "user" {
                        MessageType::User
                    } else {
                        MessageType::Assistant
                    },
                    content.to_string(),
                    compact_for_monitor,
                );
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

fn response_item_value(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    if let Some(text) = value.as_str() {
        return serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|parsed| serde_json::to_string_pretty(&parsed).ok())
            .unwrap_or_else(|| text.to_string());
    }
    serde_json::to_string_pretty(value).unwrap_or_default()
}

fn response_item_id(payload: &Value) -> &str {
    payload
        .get("call_id")
        .or_else(|| payload.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

fn apply_response_item(summary: &mut CodexRolloutSummary, timestamp: &str, payload: &Value) {
    let Some(kind) = payload.get("type").and_then(Value::as_str) else {
        return;
    };
    let (message_type, role, content) = match kind {
        "function_call" | "custom_tool_call" => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let details =
                response_item_value(payload.get("arguments").or_else(|| payload.get("input")));
            (
                MessageType::ToolUse,
                "tool_use",
                format!("[{name}] {} - {details}", response_item_id(payload)),
            )
        }
        "function_call_output" | "custom_tool_call_output" => {
            let output = response_item_value(payload.get("output"));
            (
                MessageType::ToolResult,
                "tool_result",
                format!("[Result] {}: {output}", response_item_id(payload)),
            )
        }
        "web_search_call" => {
            let details = response_item_value(payload.get("action"));
            (
                MessageType::ToolUse,
                "tool_use",
                format!("[web_search] {} - {details}", response_item_id(payload)),
            )
        }
        "tool_search_call" => {
            let details = response_item_value(payload.get("arguments"));
            (
                MessageType::ToolUse,
                "tool_use",
                format!("[tool_search] {} - {details}", response_item_id(payload)),
            )
        }
        "tool_search_output" => {
            let output = response_item_value(payload.get("tools"));
            (
                MessageType::ToolResult,
                "tool_result",
                format!("[Result] {}: {output}", response_item_id(payload)),
            )
        }
        "image_generation_call" => {
            let details = response_item_value(payload.get("revised_prompt"));
            (
                MessageType::ToolUse,
                "tool_use",
                format!(
                    "[image_generation] {} - {details}",
                    response_item_id(payload)
                ),
            )
        }
        // Message and reasoning response items have equivalent public event_msg
        // records. Ignoring them avoids duplicate conversation content.
        _ => return,
    };
    push_codex_message(summary, timestamp, role, message_type, content, false);
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

pub fn find_codex_conversation(
    thread_id: &str,
    include_tools: bool,
) -> Result<Vec<CodexMessage>, String> {
    find_codex_conversation_with_progress(thread_id, include_tools, &mut |_, _| {})
}

pub fn find_codex_conversation_with_progress(
    thread_id: &str,
    include_tools: bool,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<CodexMessage>, String> {
    let home = dirs::home_dir().ok_or("Failed to get home directory")?;
    find_codex_conversation_under_with_progress(
        &home.join(".codex").join("sessions"),
        thread_id,
        include_tools,
        on_progress,
    )
}

fn find_codex_conversation_under(
    sessions_root: &Path,
    thread_id: &str,
    include_tools: bool,
) -> Result<Vec<CodexMessage>, String> {
    find_codex_conversation_under_with_progress(sessions_root, thread_id, include_tools, &mut |_, _| {})
}

fn find_codex_conversation_under_with_progress(
    sessions_root: &Path,
    thread_id: &str,
    include_tools: bool,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<CodexMessage>, String> {
    let paths = resolve_thread_rollout_paths(sessions_root, thread_id);
    if paths.is_empty() {
        return Err(format!("Codex session {thread_id} not found"));
    }

    let mut source =
        CodexSessionSource::conversation_at_root(sessions_root.to_path_buf(), include_tools);
    let mut messages = Vec::new();
    let mut parsed_any = false;
    let mut last_error = None;
    let file_lens: Vec<u64> = paths
        .iter()
        .map(|path| fs::metadata(path).map(|meta| meta.len()).unwrap_or(0))
        .collect();
    let total: u64 = file_lens.iter().sum();
    on_progress(0, total);
    let mut read_acc = 0u64;
    for (path, file_len) in paths.into_iter().zip(file_lens) {
        match source.summary_for_with_progress(
            &path,
            Some(&mut |file_read, _file_total| {
                on_progress(read_acc.saturating_add(file_read.min(file_len)), total);
            }),
        ) {
            Ok(mut summary) => {
                parsed_any = true;
                messages.append(&mut summary.messages);
            }
            Err(error) => last_error = Some(error.to_string()),
        }
        read_acc = read_acc.saturating_add(file_len);
        on_progress(read_acc, total);
    }
    if !parsed_any {
        return Err(last_error.unwrap_or_else(|| format!("Codex session {thread_id} not found")));
    }
    messages.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.role.cmp(&right.role))
            .then_with(|| left.content.cmp(&right.content))
            .then_with(|| left.content_identity.cmp(&right.content_identity))
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
        ordered_event(timestamp, event_type, payload)
    }

    fn ordered_event(timestamp: &str, event_type: &str, payload: Value) -> String {
        format!(
            r#"{{"timestamp":{},"type":{},"payload":{}}}"#,
            serde_json::to_string(timestamp).unwrap(),
            serde_json::to_string(event_type).unwrap(),
            serde_json::to_string(&payload).unwrap()
        )
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
            apply_event_message(&mut summary, "", &serde_json::json!({"type": kind}), false);
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
                false,
            );
        }
        apply_event_message(
            &mut summary,
            "",
            &serde_json::json!({"type": "thread_rolled_back", "num_turns": 1}),
            false,
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

        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{{not-json}}").unwrap();
        writeln!(
            file,
            "{}",
            event(
                "2026-07-13T00:00:04Z",
                "event_msg",
                serde_json::json!({"type":"agent_message","message":"recovered"}),
            )
        )
        .unwrap();
        drop(file);
        let summary = source.summary_for(&path).unwrap();
        assert_eq!(summary.messages.len(), 1);
        assert_eq!(summary.latest_message(), Some("recovered"));
        assert_eq!(summary.last_timestamp, "2026-07-13T00:00:04Z");
    }

    #[test]
    fn small_append_reuses_cached_prefix_without_resetting() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("rollout-small.jsonl");
        let lines = root_fixture(Value::String("cli".into()), "codex-tui");
        write_lines(&path, &lines, true);
        assert!(fs::metadata(&path).unwrap().len() < FILE_SIGNATURE_SAMPLE_BYTES);

        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        assert_eq!(source.summary_for(&path).unwrap().messages.len(), 1);
        assert_eq!(source.reset_count, 1);

        let appended = event(
            "2026-07-13T00:00:03Z",
            "event_msg",
            serde_json::json!({"type":"agent_message","message":"appended"}),
        );
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{appended}").unwrap();
        drop(file);

        let summary = source.summary_for(&path).unwrap();
        assert_eq!(summary.messages.len(), 2);
        assert_eq!(summary.latest_message(), Some("appended"));
        assert_eq!(
            source.reset_count, 1,
            "a small append must not reparse the prefix"
        );
    }

    #[test]
    fn middle_same_inode_rewrite_that_grows_discards_stale_summary() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("rollout-middle-rewrite.jsonl");
        let old_middle = "old-middle-".to_string() + &"m".repeat(5000);
        let new_middle = "new-middle-".to_string() + &"n".repeat(5000);
        let lines = vec![
            event(
                "2026-07-13T00:00:00Z",
                "session_meta",
                serde_json::json!({
                    "id":"middle-rewrite", "cwd":"/tmp/project", "source":"cli",
                    "originator":"codex-tui"
                }),
            ),
            event(
                "2026-07-13T00:00:01Z",
                "prefix_padding",
                serde_json::json!({"padding":"p".repeat(6000)}),
            ),
            event(
                "2026-07-13T00:00:02Z",
                "event_msg",
                serde_json::json!({"type":"user_message","message":old_middle.clone()}),
            ),
            event(
                "2026-07-13T00:00:03Z",
                "suffix_padding",
                serde_json::json!({"padding":"s".repeat(6000)}),
            ),
            event(
                "2026-07-13T00:00:04Z",
                "event_msg",
                serde_json::json!({"type":"task_complete"}),
            ),
        ];
        write_lines(&path, &lines, true);

        let old_middle_preview = truncate_monitor_message(&old_middle);
        let new_middle_preview = truncate_monitor_message(&new_middle);
        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        let old_summary = source.summary_for(&path).unwrap();
        assert_eq!(old_summary.first_prompt().unwrap(), old_middle_preview);
        let old_length = fs::metadata(&path).unwrap().len();
        let old_inode = file_identity(&fs::metadata(&path).unwrap());

        let mut rewritten = fs::read(&path).unwrap();
        let old_middle_offset = rewritten
            .windows(old_middle.len())
            .position(|window| window == old_middle.as_bytes())
            .unwrap();
        assert!(old_middle_offset as u64 >= FILE_SIGNATURE_SAMPLE_BYTES);
        assert!(
            old_length - (old_middle_offset + old_middle.len()) as u64
                >= FILE_SIGNATURE_SAMPLE_BYTES
        );
        rewritten[old_middle_offset..old_middle_offset + old_middle.len()]
            .copy_from_slice(new_middle.as_bytes());
        assert_eq!(rewritten.len() as u64, old_length);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(&rewritten).unwrap();
        writeln!(
            file,
            "{}",
            event(
                "2026-07-13T00:00:05Z",
                "event_msg",
                serde_json::json!({"type":"agent_message","message":"growth"}),
            )
        )
        .unwrap();
        drop(file);

        let metadata = fs::metadata(&path).unwrap();
        assert!(metadata.len() > old_length);
        assert_eq!(file_identity(&metadata), old_inode);
        let summary = source.summary_for(&path).unwrap();
        assert_eq!(summary.first_prompt().unwrap(), new_middle_preview);
        assert!(!summary
            .messages
            .iter()
            .any(|message| message.content == old_middle_preview));
        assert_eq!(summary.latest_message(), Some("growth"));
        assert_eq!(source.reset_count, 2);
    }

    #[test]
    fn same_inode_replacement_that_grows_discards_stale_summary() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("rollout-replaced.jsonl");
        let original = root_fixture(Value::String("cli".into()), "codex-tui");
        write_lines(&path, &original, true);
        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        let original_summary = source.summary_for(&path).unwrap();
        let original_metadata = fs::metadata(&path).unwrap();

        let replacement = vec![
            event(
                "2026-07-13T01:00:00Z",
                "session_meta",
                serde_json::json!({
                    "id":"replacement-id", "cwd":"/tmp/project", "source":"cli",
                    "originator":"codex-tui"
                }),
            ),
            event(
                "2026-07-13T01:00:01Z",
                "event_msg",
                serde_json::json!({
                    "type":"user_message",
                    "message":"replacement prompt with a longer body than before"
                }),
            ),
            event(
                "2026-07-13T01:00:02Z",
                "event_msg",
                serde_json::json!({"type":"task_started"}),
            ),
        ];
        write_lines(&path, &replacement, true);
        let replacement_metadata = fs::metadata(&path).unwrap();
        assert!(replacement_metadata.len() > original_metadata.len());
        assert_eq!(
            file_identity(&original_metadata),
            file_identity(&replacement_metadata),
            "the fixture must exercise a same-inode rewrite"
        );

        let summary = source.summary_for(&path).unwrap();
        assert_ne!(summary.thread_id, original_summary.thread_id);
        assert_eq!(summary.thread_id, "replacement-id");
        assert_eq!(
            summary.first_prompt(),
            Some("replacement prompt with a longer body than before")
        );
        assert!(!summary
            .messages
            .iter()
            .any(|message| message.content == "hello"));
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
    fn discovery_does_not_parse_rollouts_older_than_visible_working_ceiling() {
        let temp = TempDir::new().unwrap();
        let now = Local::now();
        let wall_now = SystemTime::now();
        let day = temp.path().join(now.format("%Y/%m/%d").to_string());
        fs::create_dir_all(&day).unwrap();

        let recent = day.join("rollout-recent.jsonl");
        write_lines(
            &recent,
            &[
                event(
                    "2026-07-13T00:00:00Z",
                    "session_meta",
                    serde_json::json!({"id":"recent","cwd":"/tmp/project","source":"cli","originator":"codex-tui"}),
                ),
                event(
                    "2026-07-13T00:00:01Z",
                    "event_msg",
                    serde_json::json!({"type":"task_started"}),
                ),
            ],
            true,
        );
        set_modified_age(&recent, wall_now, Duration::from_secs(60));

        const STALE_ID: &str = "019f58e8-afcb-7681-bf1b-585420b500c3";
        let stale = day.join(format!("rollout-stale-{STALE_ID}.jsonl"));
        write_lines(
            &stale,
            &[
                event(
                    "2026-07-13T00:00:00Z",
                    "session_meta",
                    serde_json::json!({"id":STALE_ID,"cwd":"/tmp/project","source":"cli","originator":"codex-tui"}),
                ),
                event(
                    "2026-07-13T00:00:01Z",
                    "event_msg",
                    serde_json::json!({"type":"task_started"}),
                ),
            ],
            true,
        );
        set_modified_age(
            &stale,
            wall_now,
            Duration::from_secs(WORKING_FRESHNESS_SECS + 1),
        );

        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        let (sessions, _) = source.detect_at_with_clock(now, wall_now).unwrap();

        assert_eq!(session_ids(&sessions), HashSet::from(["recent"]));
        assert_eq!(
            source.parse_count, 1,
            "stale rollout content must not be parsed"
        );
        assert!(source.archive_index.contains_key(STALE_ID));
    }

    #[test]
    fn monitor_compacts_message_content_but_conversation_keeps_full_text() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("rollout-long.jsonl");
        let long_message = "x".repeat(32 * 1024);
        write_lines(
            &path,
            &[
                event(
                    "2026-07-13T00:00:00Z",
                    "session_meta",
                    serde_json::json!({"id":"long","cwd":"/tmp/project","source":"cli","originator":"codex-tui"}),
                ),
                event(
                    "2026-07-13T00:00:01Z",
                    "event_msg",
                    serde_json::json!({"type":"agent_message","message":long_message}),
                ),
            ],
            true,
        );

        let mut monitor = CodexSessionSource::at_root(temp.path().to_path_buf());
        let monitor_summary = monitor.summary_for(&path).unwrap();
        assert_eq!(
            monitor_summary.latest_message().unwrap().chars().count(),
            MONITOR_MESSAGE_CHARS + 3
        );
        assert!(monitor_summary.latest_message().unwrap().ends_with("..."));

        let mut conversation =
            CodexSessionSource::conversation_at_root(temp.path().to_path_buf(), false);
        let conversation_summary = conversation.summary_for(&path).unwrap();
        assert_eq!(
            conversation_summary.latest_message().unwrap(),
            "x".repeat(32 * 1024)
        );

        let unicode = "🦀".repeat(MONITOR_MESSAGE_CHARS + 10);
        let mut unicode_summary = CodexRolloutSummary::default();
        apply_rollout_event(
            &mut unicode_summary,
            &serde_json::json!({
                "timestamp":"2026-07-13T00:00:02Z", "type":"event_msg",
                "payload":{"type":"agent_message","message":unicode}
            }),
        );
        let compacted = unicode_summary.latest_message().unwrap();
        assert_eq!(compacted.chars().count(), MONITOR_MESSAGE_CHARS + 3);
        assert_eq!(
            compacted.strip_suffix("...").unwrap().chars().count(),
            MONITOR_MESSAGE_CHARS
        );
        assert!(compacted
            .strip_suffix("...")
            .unwrap()
            .chars()
            .all(|character| character == '🦀'));
    }

    #[test]
    fn filtered_valid_records_still_update_activity_timestamp() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("rollout-filtered.jsonl");
        write_lines(
            &path,
            &[
                event(
                    "2026-07-13T00:00:00Z",
                    "session_meta",
                    serde_json::json!({"id":"filtered","cwd":"/tmp/project","source":"cli","originator":"codex-tui"}),
                ),
                event(
                    "2026-07-13T00:00:01Z",
                    "response_item",
                    serde_json::json!({
                        "type":"message", "role":"assistant",
                        "content":[{"type":"output_text","text":"large duplicate"}]
                    }),
                ),
                event(
                    "2026-07-13T00:00:02Z",
                    "token_count",
                    serde_json::json!({"info":{"input_tokens":999999}}),
                ),
            ],
            true,
        );

        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        let summary = source.summary_for(&path).unwrap();
        assert_eq!(summary.last_timestamp, "2026-07-13T00:00:02Z");
        assert!(summary.messages.is_empty());
    }

    #[test]
    fn monitor_skips_oversized_ignored_records_without_retaining_the_body() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("rollout-oversized-ignored.jsonl");
        write_lines(
            &path,
            &[
                event(
                    "2026-07-13T00:00:00Z",
                    "session_meta",
                    serde_json::json!({"id":"oversized","cwd":"/tmp/project","source":"cli","originator":"codex-tui"}),
                ),
                event(
                    "2026-07-13T00:00:01Z",
                    "response_item",
                    serde_json::json!({
                        "type":"reasoning",
                        "encrypted_content":"r".repeat(128 * 1024)
                    }),
                ),
                event(
                    "2026-07-13T00:00:02Z",
                    "event_msg",
                    serde_json::json!({"type":"task_started"}),
                ),
            ],
            true,
        );

        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        let summary = source.summary_for(&path).unwrap();
        assert_eq!(summary.last_timestamp, "2026-07-13T00:00:02Z");
        assert!(summary.messages.is_empty());
        assert_eq!(
            source.cache.get(&path).unwrap().offset,
            fs::metadata(&path).unwrap().len()
        );
    }

    #[test]
    fn monitor_header_peek_consumes_one_record_and_preserves_offsets() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("rollout-header-peek.jsonl");
        let ignored = ordered_event(
            "2026-07-13T00:00:00Z",
            "response_item",
            serde_json::json!({
                "type":"reasoning",
                "encrypted_content":"r".repeat(32 * 1024)
            }),
        );
        let relevant = ordered_event(
            "2026-07-13T00:00:01Z",
            "event_msg",
            serde_json::json!({"type":"task_started"}),
        );
        write_lines(&path, &[ignored.clone(), relevant.clone()], true);
        let first_line = format!("{ignored}\n");
        let second_line = format!("{relevant}\n");

        let mut reader = BufReader::new(File::open(&path).unwrap());
        let first = read_monitor_line(&mut reader, FNV_OFFSET_BASIS).unwrap();
        assert!(first.complete);
        assert_eq!(first.bytes_read, first_line.len() as u64);
        assert_eq!(
            first
                .header
                .as_ref()
                .and_then(|header| header.event_type.as_deref()),
            Some("response_item")
        );
        assert!(first.line.is_none());

        let second = read_monitor_line(&mut reader, first.logical_prefix_hash).unwrap();
        assert!(second.complete);
        assert_eq!(second.bytes_read, second_line.len() as u64);
        assert_eq!(
            second
                .header
                .as_ref()
                .and_then(|header| header.event_type.as_deref()),
            Some("event_msg")
        );
        assert_eq!(second.line.as_deref(), Some(second_line.as_bytes()));
        assert_eq!(
            first.bytes_read + second.bytes_read,
            fs::metadata(&path).unwrap().len()
        );
        assert_eq!(
            second.logical_prefix_hash,
            hash_bytes(
                hash_bytes(FNV_OFFSET_BASIS, first_line.as_bytes()),
                second_line.as_bytes()
            )
        );
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
    fn active_child_keeps_all_linked_parent_fragments_within_ceiling() {
        const ROOT: &str = "019f58e8-afcb-7681-bf1b-585420b500c3";
        const CHILD: &str = "019f5986-4549-79e1-a409-0d09dcb7044c";
        let temp = TempDir::new().unwrap();
        let now = Local::now();
        let wall_now = SystemTime::now();
        let day = temp.path().join(now.format("%Y/%m/%d").to_string());
        fs::create_dir_all(&day).unwrap();

        let first_parent = day.join(format!("rollout-2026-07-13T00-00-00-{ROOT}.jsonl"));
        write_lines(
            &first_parent,
            &[
                event(
                    "2026-07-13T00:00:00Z",
                    "session_meta",
                    serde_json::json!({"id":ROOT,"cwd":"/tmp/project","source":"cli","originator":"codex-tui"}),
                ),
                event(
                    "2026-07-13T00:00:01Z",
                    "event_msg",
                    serde_json::json!({"type":"user_message","message":"first fragment"}),
                ),
                event(
                    "2026-07-13T00:00:02Z",
                    "event_msg",
                    serde_json::json!({"type":"task_complete"}),
                ),
            ],
            true,
        );
        set_modified_age(&first_parent, wall_now, Duration::from_secs(6 * 60 * 60));

        let resumed_parent = day.join(format!("rollout-2026-07-13T01-00-00-{ROOT}.jsonl"));
        write_lines(
            &resumed_parent,
            &[
                event(
                    "2026-07-13T01:00:00Z",
                    "session_meta",
                    serde_json::json!({"id":ROOT,"cwd":"/tmp/project","source":"cli","originator":"codex-tui"}),
                ),
                event(
                    "2026-07-13T01:00:01Z",
                    "event_msg",
                    serde_json::json!({"type":"agent_message","message":"resumed fragment"}),
                ),
                event(
                    "2026-07-13T01:00:02Z",
                    "event_msg",
                    serde_json::json!({"type":"task_complete"}),
                ),
            ],
            true,
        );
        set_modified_age(&resumed_parent, wall_now, Duration::from_secs(5 * 60 * 60));

        let child = day.join(format!("rollout-2026-07-13T02-00-00-{CHILD}.jsonl"));
        write_lines(
            &child,
            &[
                event(
                    "2026-07-13T02:00:00Z",
                    "session_meta",
                    serde_json::json!({
                        "id":CHILD,"session_id":ROOT,"cwd":"/tmp/project",
                        "originator":"Codex Desktop","source":{"subagent":{"thread_spawn":{
                            "parent_thread_id":ROOT,"agent_path":"/root/child"
                        }}}
                    }),
                ),
                event(
                    "2026-07-13T02:00:01Z",
                    "event_msg",
                    serde_json::json!({"type":"task_started"}),
                ),
            ],
            true,
        );
        set_modified_age(&child, wall_now, Duration::from_secs(60));

        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        let (sessions, _) = source.detect_at_with_clock(now, wall_now).unwrap();
        let root = sessions
            .iter()
            .find(|session| session.session_id.as_deref() == Some(ROOT))
            .and_then(|session| session.codex_summary.as_ref())
            .unwrap();

        assert_eq!(source.archive_index.get(ROOT).unwrap().len(), 2);
        assert_eq!(root.messages.len(), 2);
        assert_eq!(root.messages[0].content, "first fragment");
        assert_eq!(root.messages[1].content, "resumed fragment");

        // A parent fragment exactly at the linked-parent ceiling must remain
        // discoverable through the existing index/cache on a later poll.
        set_modified_age(
            &first_parent,
            wall_now,
            Duration::from_secs(LINKED_PARENT_CEILING_SECS),
        );
        let (sessions, _) = source.detect_at_with_clock(now, wall_now).unwrap();
        let root = sessions
            .iter()
            .find(|session| session.session_id.as_deref() == Some(ROOT))
            .and_then(|session| session.codex_summary.as_ref())
            .unwrap();
        assert_eq!(
            source.archive_index.get(ROOT).unwrap(),
            &vec![first_parent.clone(), resumed_parent.clone()]
        );
        assert!(source.cache.contains_key(&first_parent));
        assert!(source.cache.contains_key(&resumed_parent));
        assert_eq!(root.messages.len(), 2);
        assert_eq!(root.messages[0].content, "first fragment");
        assert_eq!(root.messages[1].content, "resumed fragment");
    }

    #[test]
    fn monitor_dedup_keeps_long_messages_with_identical_visible_prefixes() {
        let temp = TempDir::new().unwrap();
        let now = Local::now();
        let day = temp.path().join(now.format("%Y/%m/%d").to_string());
        fs::create_dir_all(&day).unwrap();
        let prefix = "p".repeat(MONITOR_MESSAGE_CHARS);

        for suffix in ["A", "B"] {
            let path = day.join(format!(
                "rollout-2026-07-13T00-00-01-{suffix}-duplicate.jsonl"
            ));
            write_lines(
                &path,
                &[
                    event(
                        "2026-07-13T00:00:00Z",
                        "session_meta",
                        serde_json::json!({"id":"same-prefix","cwd":"/tmp/project","source":"cli","originator":"codex-tui"}),
                    ),
                    event(
                        "2026-07-13T00:00:01Z",
                        "event_msg",
                        serde_json::json!({"type":"agent_message","message":format!("{prefix}{suffix}")}),
                    ),
                ],
                true,
            );
        }

        let mut source = CodexSessionSource::at_root(temp.path().to_path_buf());
        let (sessions, _) = source.detect_at(now).unwrap();
        let summary = sessions[0].codex_summary.as_ref().unwrap();
        assert_eq!(summary.messages.len(), 2);
        assert_eq!(summary.messages[0].timestamp, "2026-07-13T00:00:01Z");
        assert_eq!(summary.messages[1].timestamp, "2026-07-13T00:00:01Z");
        assert_eq!(summary.messages[0].role, "assistant");
        assert_eq!(summary.messages[1].role, "assistant");
        assert_eq!(summary.messages[0].message_type, MessageType::Assistant);
        assert_eq!(summary.messages[1].message_type, MessageType::Assistant);
        assert_eq!(summary.messages[0].content, summary.messages[1].content);
        assert_ne!(
            summary.messages[0].content_identity, summary.messages[1].content_identity,
            "full-content identity must keep identical compact previews distinct"
        );
        assert!(summary
            .messages
            .iter()
            .any(|message| message.content.ends_with("p...")));
    }

    #[test]
    fn response_item_messages_are_not_duplicated_into_conversations() {
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
    fn response_item_tool_calls_are_filterable_conversation_messages() {
        let mut summary = CodexRolloutSummary::default();
        apply_rollout_event_with_tools(
            &mut summary,
            &serde_json::json!({
                "timestamp":"2026-07-13T00:00:01Z", "type":"response_item",
                "payload":{"type":"function_call","name":"exec_command","call_id":"call-1","arguments":"{\"cmd\":\"pwd\"}"}
            }),
            true,
            false,
        );
        apply_rollout_event_with_tools(
            &mut summary,
            &serde_json::json!({
                "timestamp":"2026-07-13T00:00:02Z", "type":"response_item",
                "payload":{"type":"function_call_output","call_id":"call-1","output":"/tmp/project"}
            }),
            true,
            false,
        );

        assert_eq!(summary.messages.len(), 2);
        assert_eq!(summary.messages[0].message_type, MessageType::ToolUse);
        assert!(summary.messages[0].content.contains("exec_command"));
        assert!(summary.messages[0].content.contains("pwd"));
        assert_eq!(summary.messages[1].message_type, MessageType::ToolResult);
        assert!(summary.messages[1].content.contains("/tmp/project"));
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
    fn conversation_lookup_ignores_duplicate_response_item_messages() {
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
        let messages = find_codex_conversation_under(temp.path(), "thread-id", true).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "hello");
    }

    #[test]
    fn conversation_lookup_includes_codex_tool_messages() {
        let temp = TempDir::new().unwrap();
        let day = temp.path().join("2026/07/13");
        fs::create_dir_all(&day).unwrap();
        let path = day.join("rollout-2026-07-13T00-00-00-thread-id.jsonl");
        let mut lines = root_fixture(Value::String("cli".into()), "codex-tui");
        lines.push(event(
            "2026-07-13T00:00:04Z",
            "response_item",
            serde_json::json!({"type":"custom_tool_call", "name":"view_image", "call_id":"call-1", "input":"{\"path\":\"/tmp/image.png\"}"}),
        ));
        lines.push(event(
            "2026-07-13T00:00:05Z",
            "response_item",
            serde_json::json!({"type":"custom_tool_call_output", "call_id":"call-1", "output":"ok"}),
        ));
        write_lines(&path, &lines, true);

        let messages = find_codex_conversation_under(temp.path(), "thread-id", true).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].message_type, MessageType::ToolUse);
        assert_eq!(messages[2].message_type, MessageType::ToolResult);

        let without_tools = find_codex_conversation_under(temp.path(), "thread-id", false).unwrap();
        assert_eq!(without_tools.len(), 1);
        assert_eq!(without_tools[0].content, "hello");
    }

    #[test]
    fn conversation_lookup_reports_byte_progress() {
        let temp = TempDir::new().unwrap();
        let day = temp.path().join("2026/07/13");
        fs::create_dir_all(&day).unwrap();
        let path = day.join("rollout-2026-07-13T00-00-00-thread-id.jsonl");
        let lines = root_fixture(Value::String("cli".into()), "codex-tui");
        write_lines(&path, &lines, true);
        let total = fs::metadata(&path).unwrap().len();

        let mut reports = Vec::new();
        let messages = find_codex_conversation_under_with_progress(
            temp.path(),
            "thread-id",
            false,
            &mut |read, reported_total| reports.push((read, reported_total)),
        )
        .unwrap();

        assert_eq!(messages.len(), 1);
        assert!(!reports.is_empty());
        assert_eq!(reports[0], (0, total));
        assert_eq!(*reports.last().unwrap(), (total, total));
        assert!(reports.windows(2).all(|pair| pair[0].0 <= pair[1].0));
        assert!(reports.iter().all(|(_, reported)| *reported == total));
    }

    #[test]
    fn conversation_lookup_unions_cached_paths_with_recent_day_files() {
        let temp = TempDir::new().unwrap();
        let old_day = temp.path().join("2026/07/12");
        fs::create_dir_all(&old_day).unwrap();
        let old_path = old_day.join("rollout-2026-07-12T23-00-00-thread-id.jsonl");
        write_lines(
            &old_path,
            &root_fixture(Value::String("cli".into()), "codex-tui"),
            true,
        );

        let now = Local::now();
        let today = temp
            .path()
            .join(now.format("%Y").to_string())
            .join(now.format("%m").to_string())
            .join(now.format("%d").to_string());
        fs::create_dir_all(&today).unwrap();
        let new_path = today.join("rollout-today-thread-id.jsonl");
        write_lines(
            &new_path,
            &[event(
                "2026-08-19T00:00:04Z",
                "event_msg",
                serde_json::json!({"type":"user_message","message":"resumed"}),
            )],
            true,
        );

        let resolved = resolve_thread_rollout_paths_with_cache(
            temp.path(),
            "thread-id",
            Some(vec![old_path.clone()]),
        );
        assert!(resolved.contains(&old_path));
        assert!(resolved.contains(&new_path));
    }

    #[test]
    fn conversation_lookup_falls_back_to_walk_when_cached_paths_are_gone() {
        let temp = TempDir::new().unwrap();
        let old_day = temp.path().join("2026/07/12");
        fs::create_dir_all(&old_day).unwrap();
        let old_path = old_day.join("rollout-2026-07-12T23-00-00-thread-id.jsonl");
        write_lines(
            &old_path,
            &root_fixture(Value::String("cli".into()), "codex-tui"),
            true,
        );

        let resolved = resolve_thread_rollout_paths_with_cache(
            temp.path(),
            "thread-id",
            Some(vec![old_day.join("deleted.jsonl")]),
        );
        assert_eq!(resolved, vec![old_path]);
    }

    #[test]
    fn conversation_lookup_walks_for_fragments_outside_cached_days() {
        let temp = TempDir::new().unwrap();
        let cached_day = temp.path().join("2026/07/12");
        let later_day = temp.path().join("2026/07/14");
        fs::create_dir_all(&cached_day).unwrap();
        fs::create_dir_all(&later_day).unwrap();
        let cached_path = cached_day.join("rollout-2026-07-12T23-00-00-thread-id.jsonl");
        let later_path = later_day.join("rollout-2026-07-14T01-00-00-thread-id.jsonl");
        write_lines(
            &cached_path,
            &root_fixture(Value::String("cli".into()), "codex-tui"),
            true,
        );
        write_lines(
            &later_path,
            &[event(
                "2026-07-14T01:00:00Z",
                "event_msg",
                serde_json::json!({"type":"user_message","message":"later fragment"}),
            )],
            true,
        );

        let resolved = resolve_thread_rollout_paths_with_cache(
            temp.path(),
            "thread-id",
            Some(vec![cached_path.clone()]),
        );
        assert!(resolved.contains(&cached_path));
        assert!(resolved.contains(&later_path));
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

        let messages = find_codex_conversation_under(temp.path(), "thread-id", true).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "first prompt");
        assert_eq!(messages[1].content, "resumed answer");
    }
}
