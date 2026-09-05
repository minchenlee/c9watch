use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

const CACHE_VERSION: u32 = 4;
const MAX_DISPLAY_CHARS: usize = 400;
const MAX_INDEXED_MESSAGES: usize = 20_000;
const MAX_INDEXED_MESSAGE_CHARS: usize = 16_384;
const MAX_INDEXED_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
const FILE_ANCHOR_BYTES: usize = 256;
const MAX_PROCESS_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexTokenUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
}

impl CodexTokenUsage {
    fn from_value(value: &Value) -> Self {
        Self {
            input_tokens: value
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cached_input_tokens: value
                .get("cached_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output_tokens: value
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            reasoning_output_tokens: value
                .get("reasoning_output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            total_tokens: value
                .get("total_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        }
    }

    fn is_zero(self) -> bool {
        self.input_tokens == 0
            && self.cached_input_tokens == 0
            && self.output_tokens == 0
            && self.reasoning_output_tokens == 0
            && self.total_tokens == 0
    }

    fn delta_from(self, previous: Self) -> Self {
        if self.total_tokens < previous.total_tokens {
            return self;
        }
        Self {
            input_tokens: self.input_tokens.saturating_sub(previous.input_tokens),
            cached_input_tokens: self
                .cached_input_tokens
                .saturating_sub(previous.cached_input_tokens),
            output_tokens: self.output_tokens.saturating_sub(previous.output_tokens),
            reasoning_output_tokens: self
                .reasoning_output_tokens
                .saturating_sub(previous.reasoning_output_tokens),
            total_tokens: self.total_tokens.saturating_sub(previous.total_tokens),
        }
    }

    fn add_assign(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.cached_input_tokens = self
            .cached_input_tokens
            .saturating_add(other.cached_input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.reasoning_output_tokens = self
            .reasoning_output_tokens
            .saturating_add(other.reasoning_output_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexTokenDay {
    pub date: String,
    pub timestamp: String,
    pub model: String,
    pub usage: CodexTokenUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
struct ArchivedMessage {
    timestamp: String,
    role: String,
    text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
struct ArchivedTokenEvent {
    timestamp: String,
    model: String,
    usage: CodexTokenUsage,
    #[serde(default)]
    cumulative: bool,
    /// Cumulative counters restart independently for each rollout stream.
    /// Omitted from the on-disk cache because it is always the parent file path
    /// and was repeating ~100 bytes on every token event.
    #[serde(default, skip_serializing)]
    stream_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexRolloutSnapshot {
    pub thread_id: String,
    pub cwd: String,
    pub surface: String,
    pub agent_kind: String,
    pub parent_thread_id: Option<String>,
    pub display: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub token_days: Vec<CodexTokenDay>,
    pub path: PathBuf,
    #[serde(default)]
    paths: Vec<PathBuf>,
    #[serde(default)]
    messages_complete: bool,
    #[serde(default)]
    messages: Vec<ArchivedMessage>,
    #[serde(default)]
    token_events: Vec<ArchivedTokenEvent>,
}

impl CodexRolloutSnapshot {
    pub(crate) fn is_internal(&self) -> bool {
        self.agent_kind == "internal"
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileFingerprint {
    pub size: u64,
    pub modified_ns: u64,
    #[serde(default)]
    pub identity: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedRollout {
    fingerprint: FileFingerprint,
    snapshot: CodexRolloutSnapshot,
    #[serde(default)]
    offset: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending: Vec<u8>,
    #[serde(default)]
    current_model: String,
    #[serde(default)]
    indexed_message_bytes: usize,
    #[serde(default)]
    anchor: Vec<u8>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveCache {
    version: u32,
    files: HashMap<String, CachedRollout>,
}

#[derive(Clone, Copy)]
enum SnapshotView {
    Full,
    Listing,
}

struct ProcessArchive {
    cache: ArchiveCache,
    merged: Vec<CodexRolloutSnapshot>,
}

static ARCHIVE_STATE: OnceLock<Mutex<HashMap<(PathBuf, PathBuf), ProcessArchive>>> =
    OnceLock::new();

fn archive_state() -> &'static Mutex<HashMap<(PathBuf, PathBuf), ProcessArchive>> {
    ARCHIVE_STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fingerprint(path: &Path) -> Option<FileFingerprint> {
    let metadata = path.metadata().ok()?;
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0);
    Some(FileFingerprint {
        size: metadata.len(),
        modified_ns,
        identity: file_identity(&metadata),
    })
}

#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn file_identity(_metadata: &std::fs::Metadata) -> u64 {
    0
}

pub(crate) fn default_sessions_root(home: &Path) -> PathBuf {
    home.join(".codex").join("sessions")
}

pub(crate) fn default_cache_path(home: &Path) -> PathBuf {
    home.join(".codex").join("c9watch-archive-cache.json")
}

fn collect_rollouts(root: &Path) -> Vec<PathBuf> {
    if !root.exists() {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = pending.pop() {
        if depth > 5 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push((path, depth + 1));
            } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
                result.push(path);
            }
        }
    }
    result.sort();
    result
}

fn timestamp_ms(value: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|timestamp| u64::try_from(timestamp.timestamp_millis()).ok())
        .unwrap_or(0)
}

fn timestamp_date(value: &str) -> String {
    value.get(..10).unwrap_or("unknown").to_string()
}

fn truncate(value: &str, limit: usize) -> String {
    let mut result: String = value.chars().take(limit).collect();
    if value.chars().count() > limit {
        result.push('…');
    }
    result
}

fn message_text(value: &Value) -> Option<(String, String)> {
    let payload = value.get("payload")?;
    if value.get("type").and_then(Value::as_str) == Some("event_msg") {
        let kind = payload.get("type").and_then(Value::as_str)?;
        let role = match kind {
            "user_message" => "user",
            "agent_message" => "assistant",
            _ => return None,
        };
        let text = payload.get("message").and_then(Value::as_str)?.to_string();
        if text.trim().is_empty() {
            return None;
        }
        return Some((role.to_string(), text));
    }
    None
}

fn classify_session(source: &Value, originator: &str) -> (String, String, Option<String>) {
    if matches!(originator, "Claude Code" | "Claude Cowork") {
        return classify_agent(source, "integration".to_string());
    }
    let originator_lower = originator.to_ascii_lowercase();
    let surface = if originator_lower.contains("desktop") {
        "app"
    } else if originator_lower.contains("tui") {
        "cli"
    } else if originator_lower.contains("exec") {
        "exec"
    } else {
        match source.as_str().unwrap_or_default() {
            "vscode" => "app",
            "cli" => "cli",
            "exec" => "exec",
            "appServer" | "app_server" => "integration",
            _ => "unknown",
        }
    }
    .to_string();

    classify_agent(source, surface)
}

fn classify_agent(source: &Value, surface: String) -> (String, String, Option<String>) {
    let Some(subagent) = source.get("subagent") else {
        return (surface, "root".to_string(), None);
    };
    if let Some(spawn) = subagent.get("thread_spawn") {
        let parent = spawn
            .get("parent_thread_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        return (surface, "subagent".to_string(), parent);
    }
    // User-spawned agents carry thread_spawn metadata. Every other subagent
    // source shape is a Codex-owned helper, including kinds added in the future.
    (surface, "internal".to_string(), None)
}

#[derive(Default)]
struct DayAccum {
    timestamp: String,
    usage: CodexTokenUsage,
}

fn empty_cached(path: &Path, fingerprint: FileFingerprint) -> CachedRollout {
    CachedRollout {
        fingerprint,
        snapshot: CodexRolloutSnapshot {
            thread_id: String::new(),
            cwd: String::new(),
            surface: "unknown".to_string(),
            agent_kind: "root".to_string(),
            parent_thread_id: None,
            display: String::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
            token_days: Vec::new(),
            path: path.to_path_buf(),
            paths: vec![path.to_path_buf()],
            messages_complete: true,
            messages: Vec::new(),
            token_events: Vec::new(),
        },
        offset: 0,
        pending: Vec::new(),
        current_model: "unknown".to_string(),
        indexed_message_bytes: 0,
        anchor: Vec::new(),
    }
}

fn refresh_rollout(
    path: &Path,
    fingerprint: FileFingerprint,
    previous: Option<CachedRollout>,
) -> Option<CachedRollout> {
    let mut file = File::open(path).ok()?;
    let mut reset = previous.as_ref().map_or(true, |cached| {
        fingerprint.identity != cached.fingerprint.identity
            || fingerprint.size < cached.offset
            || (fingerprint.size == cached.fingerprint.size
                && fingerprint.modified_ns != cached.fingerprint.modified_ns)
    });
    if !reset {
        let cached = previous.as_ref()?;
        if !cached.anchor.is_empty() {
            let anchor_start = cached.offset.saturating_sub(cached.anchor.len() as u64);
            file.seek(SeekFrom::Start(anchor_start)).ok()?;
            let mut current = vec![0; cached.anchor.len()];
            if file.read_exact(&mut current).is_err() || current != cached.anchor {
                reset = true;
            }
        }
    }
    let mut cached = if reset {
        empty_cached(path, fingerprint)
    } else {
        previous?
    };
    // Older cache files may hold the unfinished suffix at EOF. Rewind it and
    // release its allocation; the authoritative file remains the source of truth.
    cached.offset = cached.offset.saturating_sub(cached.pending.len() as u64);
    cached.pending = Vec::new();
    file.seek(SeekFrom::Start(cached.offset)).ok()?;
    {
        let remaining = fingerprint.size.saturating_sub(cached.offset);
        let mut reader = BufReader::new((&mut file).take(remaining));
        let mut line = Vec::new();
        loop {
            line.clear();
            let bytes = reader.read_until(b'\n', &mut line).ok()?;
            if bytes == 0 {
                break;
            }
            let complete = line.last() == Some(&b'\n');
            match apply_archive_line(&mut cached, &line) {
                Ok(()) => {}
                Err(_) if !complete => break,
                Err(_) => {}
            }
            cached.offset += bytes as u64;
            // A huge tool result must not keep its scratch allocation for the
            // rest of the file. No transcript buffer is stored in CachedRollout.
            if line.capacity() > 64 * 1024 {
                line = Vec::new();
            }
        }
    }
    let anchor_len = FILE_ANCHOR_BYTES.min(cached.offset as usize);
    cached.anchor.resize(anchor_len, 0);
    file.seek(SeekFrom::Start(cached.offset - anchor_len as u64))
        .ok()?;
    file.read_exact(&mut cached.anchor).ok()?;
    cached.fingerprint = fingerprint;
    rebuild_token_days(&mut cached.snapshot);
    Some(cached)
}

#[derive(Deserialize)]
struct ArchiveLineHeader {
    #[serde(default)]
    timestamp: Value,
    #[serde(default, rename = "type")]
    event_type: Value,
}

fn apply_archive_line(cached: &mut CachedRollout, line: &[u8]) -> Result<(), serde_json::Error> {
    // Validate every record but don't allocate tool outputs that the archive
    // never indexes. serde ignores the payload while reading this small header.
    let header: ArchiveLineHeader = serde_json::from_slice(line)?;
    if matches!(
        header.event_type.as_str(),
        Some("session_meta" | "event_msg" | "turn_context")
    ) {
        let value = serde_json::from_slice::<Value>(line)?;
        apply_archive_event(cached, &value);
    } else {
        update_archive_timestamp(cached, header.timestamp.as_str().unwrap_or_default());
    }
    Ok(())
}

fn update_archive_timestamp(cached: &mut CachedRollout, timestamp: &str) {
    let millis = timestamp_ms(timestamp);
    if millis > 0 {
        if cached.snapshot.created_at_ms == 0 {
            cached.snapshot.created_at_ms = millis;
        }
        cached.snapshot.updated_at_ms = cached.snapshot.updated_at_ms.max(millis);
    }
}

fn apply_archive_event(cached: &mut CachedRollout, value: &Value) {
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or_default();
    update_archive_timestamp(cached, timestamp);
    match value.get("type").and_then(Value::as_str) {
        Some("session_meta") => {
            let payload = value.get("payload").unwrap_or(&Value::Null);
            cached.snapshot.thread_id = payload
                .get("id")
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            cached.snapshot.cwd = payload
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let source = payload.get("source").unwrap_or(&Value::Null);
            let originator = payload
                .get("originator")
                .and_then(Value::as_str)
                .unwrap_or_default();
            (
                cached.snapshot.surface,
                cached.snapshot.agent_kind,
                cached.snapshot.parent_thread_id,
            ) = classify_session(source, originator);
        }
        Some("turn_context") => {
            let payload = value.get("payload").unwrap_or(&Value::Null);
            if let Some(model) = payload.get("model").and_then(Value::as_str) {
                cached.current_model = model.to_string();
            }
            if cached.snapshot.cwd.is_empty() {
                cached.snapshot.cwd = payload
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
            }
        }
        Some("event_msg")
            if value.pointer("/payload/type").and_then(Value::as_str) == Some("token_count") =>
        {
            let cumulative = value
                .pointer("/payload/info/total_token_usage")
                .or_else(|| value.pointer("/payload/total_token_usage"));
            let (usage, is_cumulative) = if let Some(usage_value) = cumulative {
                (CodexTokenUsage::from_value(usage_value), true)
            } else {
                (
                    value
                        .pointer("/payload/info/last_token_usage")
                        .map(CodexTokenUsage::from_value)
                        .unwrap_or_default(),
                    false,
                )
            };
            if !usage.is_zero() {
                let event = ArchivedTokenEvent {
                    timestamp: timestamp.to_string(),
                    model: cached.current_model.clone(),
                    usage,
                    cumulative: is_cumulative,
                    stream_id: cached.snapshot.path.to_string_lossy().into_owned(),
                };
                if !cached.snapshot.token_events.contains(&event) {
                    cached.snapshot.token_events.push(event);
                }
            }
        }
        _ => {}
    }
    if let Some((role, text)) = message_text(value) {
        let trimmed = text.trim();
        let message = ArchivedMessage {
            timestamp: timestamp.to_string(),
            role: role.clone(),
            text: truncate(trimmed, MAX_INDEXED_MESSAGE_CHARS),
        };
        if !cached.snapshot.messages.contains(&message) {
            let text_is_complete = trimmed.chars().count() <= MAX_INDEXED_MESSAGE_CHARS;
            let fits = cached.snapshot.messages.len() < MAX_INDEXED_MESSAGES
                && cached
                    .indexed_message_bytes
                    .saturating_add(message.text.len())
                    <= MAX_INDEXED_MESSAGE_BYTES;
            if text_is_complete && fits {
                cached.indexed_message_bytes += message.text.len();
                cached.snapshot.messages.push(message);
            } else {
                cached.snapshot.messages_complete = false;
                if fits {
                    cached.indexed_message_bytes += message.text.len();
                    cached.snapshot.messages.push(message);
                }
            }
        }
        if role == "user" {
            cached.snapshot.display = truncate(text.trim(), MAX_DISPLAY_CHARS);
        }
    }
}

fn rebuild_token_days(snapshot: &mut CodexRolloutSnapshot) {
    let mut days: HashMap<(String, String), DayAccum> = HashMap::new();
    let mut previous_totals: HashMap<&str, CodexTokenUsage> = HashMap::new();
    let mut seen_events: HashSet<(String, String, CodexTokenUsage, bool)> = HashSet::new();
    for event in &snapshot.token_events {
        let usage = if event.cumulative {
            let previous_total = previous_totals
                .get(event.stream_id.as_str())
                .copied()
                .unwrap_or_default();
            let delta = event.usage.delta_from(previous_total);
            previous_totals.insert(event.stream_id.as_str(), event.usage);
            delta
        } else {
            event.usage
        };
        // Resumed rollout files can replay an authoritative prefix. Preserve the
        // per-stream cumulative baseline above, but count an identical event only once.
        if !seen_events.insert((
            event.timestamp.clone(),
            event.model.clone(),
            event.usage,
            event.cumulative,
        )) {
            continue;
        }
        let day = days
            .entry((timestamp_date(&event.timestamp), event.model.clone()))
            .or_default();
        if day.timestamp.is_empty() || event.timestamp < day.timestamp {
            day.timestamp = event.timestamp.clone();
        }
        day.usage.add_assign(usage);
    }
    snapshot.token_days = days
        .into_iter()
        .map(|((date, model), day)| CodexTokenDay {
            date,
            timestamp: day.timestamp,
            model,
            usage: day.usage,
        })
        .collect();
    snapshot.token_days.sort_by(|left, right| {
        left.date
            .cmp(&right.date)
            .then_with(|| left.model.cmp(&right.model))
    });
}

pub(crate) fn scan_rollout(path: &Path) -> Option<CodexRolloutSnapshot> {
    let fingerprint = fingerprint(path)?;
    let cached = refresh_rollout(path, fingerprint, None)?;
    (!cached.snapshot.thread_id.is_empty()).then_some(cached.snapshot)
}

fn write_cache(path: &Path, cache: &ArchiveCache) {
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let temporary = path.with_extension("json.tmp");
    let Ok(file) = File::create(&temporary) else {
        return;
    };
    let mut writer = BufWriter::new(file);
    let ok = serde_json::to_writer(&mut writer, cache).is_ok() && writer.flush().is_ok();
    drop(writer);
    if ok {
        let _ = std::fs::rename(temporary, path);
    } else {
        let _ = std::fs::remove_file(&temporary);
    }
}

fn read_cache_file(path: &Path) -> ArchiveCache {
    let mut cache: ArchiveCache = File::open(path)
        .ok()
        .and_then(|file| serde_json::from_reader(BufReader::new(file)).ok())
        .filter(|cache: &ArchiveCache| cache.version == CACHE_VERSION)
        .unwrap_or_else(|| ArchiveCache {
            version: CACHE_VERSION,
            files: HashMap::new(),
        });
    for entry in cache.files.values_mut() {
        hydrate_stream_ids(entry);
        if !entry.pending.is_empty() {
            entry.offset = entry.offset.saturating_sub(entry.pending.len() as u64);
            entry.anchor.clear();
        }
        entry.pending = Vec::new();
    }
    trim_message_cache(&mut cache, MAX_PROCESS_MESSAGE_BYTES);
    cache
}

/// Bound retained search excerpts across the whole archive, not only per file.
/// Evicted excerpts remain searchable through the existing exact disk fallback.
fn trim_message_cache(cache: &mut ArchiveCache, mut budget: usize) -> bool {
    let mut paths: Vec<_> = cache
        .files
        .iter()
        .map(|(path, entry)| (path.clone(), entry.snapshot.updated_at_ms))
        .collect();
    paths.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut trimmed = false;
    for (path, _) in paths {
        let entry = cache.files.get_mut(&path).expect("known archive file");
        let bytes: usize = entry
            .snapshot
            .messages
            .iter()
            .map(|m| m.text.capacity())
            .sum();
        if bytes > budget {
            entry.snapshot.messages = Vec::new();
            entry.snapshot.messages_complete = false;
            entry.indexed_message_bytes = 0;
            trimmed = true;
        } else {
            budget -= bytes;
        }
    }
    trimmed
}

fn hydrate_stream_ids(cached: &mut CachedRollout) {
    if cached.snapshot.path.as_os_str().is_empty() {
        if let Some(path) = cached.snapshot.paths.first() {
            cached.snapshot.path = path.clone();
        }
    }
    let path = cached.snapshot.path.to_string_lossy().into_owned();
    if path.is_empty() {
        return;
    }
    for event in &mut cached.snapshot.token_events {
        if event.stream_id.is_empty() {
            event.stream_id = path.clone();
        }
    }
}

fn listing_snapshot(snapshot: &CodexRolloutSnapshot) -> CodexRolloutSnapshot {
    CodexRolloutSnapshot {
        thread_id: snapshot.thread_id.clone(),
        cwd: snapshot.cwd.clone(),
        surface: snapshot.surface.clone(),
        agent_kind: snapshot.agent_kind.clone(),
        parent_thread_id: snapshot.parent_thread_id.clone(),
        display: snapshot.display.clone(),
        created_at_ms: snapshot.created_at_ms,
        updated_at_ms: snapshot.updated_at_ms,
        token_days: snapshot.token_days.clone(),
        path: snapshot.path.clone(),
        paths: snapshot.paths.clone(),
        messages_complete: snapshot.messages_complete,
        messages: Vec::new(),
        token_events: Vec::new(),
    }
}

fn merge_all(cache: &ArchiveCache) -> Vec<CodexRolloutSnapshot> {
    let mut by_thread: HashMap<String, CodexRolloutSnapshot> = HashMap::new();
    for entry in cache.files.values() {
        let snapshot = entry.snapshot.clone();
        if let Some(existing) = by_thread.get_mut(&snapshot.thread_id) {
            merge_snapshot(existing, snapshot);
        } else {
            by_thread.insert(snapshot.thread_id.clone(), snapshot);
        }
    }
    by_thread.into_values().collect()
}

fn refresh_cache(cache: &mut ArchiveCache, root: &Path) -> bool {
    let candidates = collect_rollouts(root);
    let active_paths: HashSet<String> = candidates
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect();
    let before = cache.files.len();
    cache.files.retain(|path, _| active_paths.contains(path));
    let mut dirty = cache.files.len() != before;

    for path in candidates {
        let Some(fingerprint) = fingerprint(&path) else {
            continue;
        };
        let key = path.to_string_lossy().to_string();
        if cache
            .files
            .get(&key)
            .is_some_and(|entry| entry.fingerprint == fingerprint)
        {
            continue;
        }
        dirty = true;
        let previous = cache.files.remove(&key);
        if let Some(mut cached) = refresh_rollout(&path, fingerprint, previous) {
            if !cached.snapshot.thread_id.is_empty() {
                hydrate_stream_ids(&mut cached);
                cache.files.insert(key, cached);
            }
        }
    }
    dirty
}

fn load_snapshots_with(
    root: &Path,
    cache_path: &Path,
    view: SnapshotView,
) -> Vec<CodexRolloutSnapshot> {
    let key = (root.to_path_buf(), cache_path.to_path_buf());
    let mut state = archive_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let archive = state.entry(key).or_insert_with(|| ProcessArchive {
        cache: read_cache_file(cache_path),
        merged: Vec::new(),
    });
    let refreshed = refresh_cache(&mut archive.cache, root);
    let trimmed = trim_message_cache(&mut archive.cache, MAX_PROCESS_MESSAGE_BYTES);
    let dirty = refreshed || trimmed;
    if dirty || archive.merged.is_empty() {
        // Keep only listing metadata in the second index. Full text and token
        // events already live in cache.files; cloning them here doubles retention.
        archive.merged = merge_all(&archive.cache)
            .iter()
            .map(listing_snapshot)
            .collect();
        if dirty {
            write_cache(cache_path, &archive.cache);
        }
    }
    match view {
        SnapshotView::Full => merge_all(&archive.cache),
        SnapshotView::Listing => archive.merged.clone(),
    }
}

pub(crate) fn load_snapshots(root: &Path, cache_path: &Path) -> Vec<CodexRolloutSnapshot> {
    load_snapshots_with(root, cache_path, SnapshotView::Full)
}

pub(crate) fn load_listing_snapshots(root: &Path, cache_path: &Path) -> Vec<CodexRolloutSnapshot> {
    load_snapshots_with(root, cache_path, SnapshotView::Listing)
}

/// Fast path for conversation loading when History/Cost already warmed the archive.
pub(crate) fn cached_thread_paths(sessions_root: &Path, thread_id: &str) -> Option<Vec<PathBuf>> {
    let cache_path = sessions_root.parent()?.join("c9watch-archive-cache.json");
    let key = (sessions_root.to_path_buf(), cache_path);
    let state = archive_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let snapshot = state
        .get(&key)?
        .merged
        .iter()
        .find(|item| item.thread_id == thread_id)?;
    let mut paths = snapshot.paths.clone();
    if !paths.contains(&snapshot.path) {
        paths.push(snapshot.path.clone());
    }
    Some(paths)
}

fn merge_snapshot(existing: &mut CodexRolloutSnapshot, incoming: CodexRolloutSnapshot) {
    let incoming_is_newer = (incoming.updated_at_ms, incoming.path.as_os_str())
        > (existing.updated_at_ms, existing.path.as_os_str());
    let fallback_display = if incoming_is_newer && !incoming.display.is_empty() {
        incoming.display.clone()
    } else {
        existing.display.clone()
    };
    existing.created_at_ms = match (existing.created_at_ms, incoming.created_at_ms) {
        (0, created) | (created, 0) => created,
        (left, right) => left.min(right),
    };
    existing.updated_at_ms = existing.updated_at_ms.max(incoming.updated_at_ms);
    existing.messages_complete &= incoming.messages_complete;
    if existing.paths.is_empty() {
        existing.paths.push(existing.path.clone());
    }
    for path in incoming.paths.iter().chain(std::iter::once(&incoming.path)) {
        if !existing.paths.contains(path) {
            existing.paths.push(path.clone());
        }
    }
    let mut indexed_bytes: usize = existing
        .messages
        .iter()
        .map(|message| message.text.len())
        .sum();
    for message in incoming.messages {
        if !existing.messages.contains(&message)
            && existing.messages.len() < MAX_INDEXED_MESSAGES
            && indexed_bytes.saturating_add(message.text.len()) <= MAX_INDEXED_MESSAGE_BYTES
        {
            indexed_bytes += message.text.len();
            existing.messages.push(message);
        }
    }
    existing
        .messages
        .sort_by(|left, right| left.timestamp.cmp(&right.timestamp));
    for event in incoming.token_events {
        if !existing.token_events.contains(&event) {
            existing.token_events.push(event);
        }
    }
    existing
        .token_events
        .sort_by(|left, right| left.timestamp.cmp(&right.timestamp));
    if incoming_is_newer {
        existing.cwd = incoming.cwd;
        existing.surface = incoming.surface;
        existing.agent_kind = incoming.agent_kind;
        existing.parent_thread_id = incoming.parent_thread_id;
        existing.path = incoming.path;
    }
    existing.display = if existing.messages_complete {
        existing
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .map(|message| truncate(message.text.trim(), MAX_DISPLAY_CHARS))
            .unwrap_or(fallback_display)
    } else {
        fallback_display
    };
    rebuild_token_days(existing);
}

pub(crate) fn load_default_snapshots(home: &Path) -> Vec<CodexRolloutSnapshot> {
    load_snapshots(&default_sessions_root(home), &default_cache_path(home))
}

pub(crate) fn load_default_listing(home: &Path) -> Vec<CodexRolloutSnapshot> {
    load_listing_snapshots(&default_sessions_root(home), &default_cache_path(home))
}

pub(crate) fn search_rollout<F>(
    snapshot: &CodexRolloutSnapshot,
    query: &str,
    case_sensitive: bool,
    whole_word: bool,
    phrase_match: F,
) -> Option<String>
where
    F: Fn(&str, &str, bool) -> bool,
{
    if snapshot.messages_complete {
        for message in &snapshot.messages {
            let text = &message.text;
            let normalized = if case_sensitive {
                text.clone()
            } else {
                text.to_lowercase()
            };
            if phrase_match(&normalized, query, whole_word) {
                return Some(text.clone());
            }
        }
        return None;
    }

    // The persisted message cache is intentionally bounded. For an incomplete
    // snapshot, stream the authoritative event_msg records from every merged
    // rollout so deep search remains exact without retaining unbounded text.
    let mut searched = HashSet::new();
    let mut all_paths_read = true;
    for path in snapshot.paths.iter().chain(std::iter::once(&snapshot.path)) {
        if !searched.insert(path) {
            continue;
        }
        let Ok(file) = File::open(path) else {
            all_paths_read = false;
            continue;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some((_, text)) = message_text(&value) else {
                continue;
            };
            let normalized = if case_sensitive {
                text.clone()
            } else {
                text.to_lowercase()
            };
            if phrase_match(&normalized, query, whole_word) {
                return Some(text);
            }
        }
    }
    if all_paths_read {
        return None;
    }

    // If a rollout disappeared after caching, retain the previous best-effort
    // behavior for its cached excerpts rather than dropping all searchability.
    for message in &snapshot.messages {
        let text = &message.text;
        let normalized = if case_sensitive {
            text.clone()
        } else {
            text.to_lowercase()
        };
        if phrase_match(&normalized, query, whole_word) {
            return Some(text.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_rollout(path: &Path, lines: &[&str]) {
        std::fs::write(path, lines.join("\n")).unwrap();
    }

    #[test]
    fn parses_app_cli_subagent_and_internal_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("app.jsonl");
        write_rollout(
            &app,
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"app-1","cwd":"/tmp/app","source":"vscode","originator":"Codex Desktop"}}"#,
            ],
        );
        let cli = directory.path().join("cli.jsonl");
        write_rollout(
            &cli,
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"cli-1","cwd":"/tmp/cli","source":"cli","originator":"codex-tui"}}"#,
            ],
        );
        let child = directory.path().join("child.jsonl");
        write_rollout(
            &child,
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"child-1","cwd":"/tmp/app","originator":"Codex Desktop","source":{"subagent":{"thread_spawn":{"parent_thread_id":"app-1","depth":1}}}}}"#,
            ],
        );
        let guardian = directory.path().join("guardian.jsonl");
        write_rollout(
            &guardian,
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"guardian-1","cwd":"/tmp/app","originator":"Codex Desktop","source":{"subagent":{"other":"guardian"}}}}"#,
            ],
        );
        let future_internal = directory.path().join("future-internal.jsonl");
        write_rollout(
            &future_internal,
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"future-internal-1","cwd":"/tmp/app","originator":"Codex Desktop","source":{"subagent":{"other":"new_helper_kind"}}}}"#,
            ],
        );
        let integration = directory.path().join("integration.jsonl");
        write_rollout(
            &integration,
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"integration-1","cwd":"/tmp/app","source":"vscode","originator":"Claude Code"}}"#,
            ],
        );

        assert_eq!(scan_rollout(&app).unwrap().surface, "app");
        assert_eq!(scan_rollout(&cli).unwrap().surface, "cli");
        let child = scan_rollout(&child).unwrap();
        assert_eq!(child.agent_kind, "subagent");
        assert_eq!(child.parent_thread_id.as_deref(), Some("app-1"));
        assert!(scan_rollout(&guardian).unwrap().is_internal());
        assert!(scan_rollout(&future_internal).unwrap().is_internal());
        assert_eq!(scan_rollout(&integration).unwrap().surface, "integration");
    }

    #[test]
    fn token_totals_use_cumulative_deltas_and_include_reasoning() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tokens.jsonl");
        write_rollout(
            &path,
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"token-1","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                r#"{"timestamp":"2026-07-13T01:02:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":30,"reasoning_output_tokens":10,"total_tokens":130}}}}"#,
                r#"{"timestamp":"2026-07-13T01:03:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":250,"cached_input_tokens":100,"output_tokens":70,"reasoning_output_tokens":25,"total_tokens":320}}}}"#,
            ],
        );
        let snapshot = scan_rollout(&path).unwrap();
        assert_eq!(snapshot.token_days.len(), 1);
        let usage = snapshot.token_days[0].usage;
        assert_eq!(usage.input_tokens, 250);
        assert_eq!(usage.cached_input_tokens, 100);
        assert_eq!(usage.output_tokens, 70);
        assert_eq!(usage.reasoning_output_tokens, 25);
        assert_eq!(usage.total_tokens, 320);
    }

    #[test]
    fn partial_line_is_ignored_and_size_change_invalidates_cache() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("partial.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"partial-1","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#,
                "\n{\"timestamp\":\"incomplete"
            ),
        )
        .unwrap();
        let cache = directory.path().join("cache.json");
        let first = load_snapshots(&root, &cache);
        assert_eq!(first.len(), 1);
        assert!(first[0].display.is_empty());

        std::fs::write(
            &path,
            concat!(
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"partial-1","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#,
                "\n",
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"event_msg","payload":{"type":"user_message","message":"completed prompt"}}"#
            ),
        )
        .unwrap();
        let second = load_snapshots(&root, &cache);
        assert_eq!(second[0].display, "completed prompt");
    }

    #[test]
    fn response_items_are_not_used_as_display() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("messages.jsonl");
        write_rollout(
            &path,
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"message-1","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"private duplicate"}]}}"#,
                r#"{"timestamp":"2026-07-13T01:02:00Z","type":"event_msg","payload":{"type":"user_message","message":"real request"}}"#,
            ],
        );
        assert_eq!(scan_rollout(&path).unwrap().display, "real request");
    }

    #[test]
    fn deep_search_uses_cached_authoritative_messages_without_the_rollout_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("search.jsonl");
        write_rollout(
            &path,
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"search-1","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"private needle"}]}}"#,
                r#"{"timestamp":"2026-07-13T01:03:00Z","type":"event_msg","payload":{"type":"agent_message","message":"visible needle"}}"#,
            ],
        );
        let snapshot = scan_rollout(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let hit = search_rollout(&snapshot, "needle", false, false, |text, query, _| {
            text.contains(query)
        });
        assert_eq!(hit.as_deref(), Some("visible needle"));
    }

    #[test]
    fn deep_search_falls_back_to_full_authoritative_message_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("long-search.jsonl");
        let message = format!(
            "{} needle-beyond-index",
            "x".repeat(MAX_INDEXED_MESSAGE_CHARS + 32)
        );
        let lines = [
            serde_json::json!({
                "timestamp":"2026-07-13T01:00:00Z", "type":"session_meta",
                "payload":{"id":"long-search-1","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}
            })
            .to_string(),
            serde_json::json!({
                "timestamp":"2026-07-13T01:01:00Z", "type":"event_msg",
                "payload":{"type":"agent_message","message":message}
            })
            .to_string(),
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let snapshot = scan_rollout(&path).unwrap();
        assert!(!snapshot.messages[0].text.contains("needle-beyond-index"));
        let hit = search_rollout(
            &snapshot,
            "needle-beyond-index",
            false,
            false,
            |text, query, _| text.contains(query),
        );
        assert!(hit.is_some_and(|text| text.ends_with("needle-beyond-index")));
    }

    #[test]
    fn deep_search_does_not_use_truncation_as_a_word_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("whole-word-search.jsonl");
        let message = format!(
            "{} needleSuffix",
            "x".repeat(MAX_INDEXED_MESSAGE_CHARS - " needle".chars().count())
        );
        let lines = [
            serde_json::json!({
                "timestamp":"2026-07-13T01:00:00Z", "type":"session_meta",
                "payload":{"id":"whole-word-1","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}
            })
            .to_string(),
            serde_json::json!({
                "timestamp":"2026-07-13T01:01:00Z", "type":"event_msg",
                "payload":{"type":"agent_message","message":message}
            })
            .to_string(),
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let snapshot = scan_rollout(&path).unwrap();
        assert!(snapshot.messages[0].text.ends_with("needle…"));
        let hit = search_rollout(
            &snapshot,
            "needle",
            false,
            true,
            |text, query, whole_word| {
                let Some(index) = text.find(query) else {
                    return false;
                };
                !whole_word
                    || text[index + query.len()..]
                        .chars()
                        .next()
                        .map_or(true, |character| !character.is_alphanumeric())
            },
        );
        assert!(hit.is_none());
    }

    #[test]
    fn duplicate_thread_ids_merge_unique_messages_and_token_usage() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        write_rollout(
            &root.join("old.jsonl"),
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"duplicate","cwd":"/tmp/old","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"event_msg","payload":{"type":"user_message","message":"older prompt"}}"#,
                r#"{"timestamp":"2026-07-13T01:02:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":20,"total_tokens":120}}}}"#,
            ],
        );
        write_rollout(
            &root.join("new.jsonl"),
            &[
                r#"{"timestamp":"2026-07-13T02:00:00Z","type":"session_meta","payload":{"id":"duplicate","cwd":"/tmp/new","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T01:02:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":20,"total_tokens":120}}}}"#,
                r#"{"timestamp":"2026-07-13T02:01:00Z","type":"event_msg","payload":{"type":"user_message","message":"newer prompt"}}"#,
                r#"{"timestamp":"2026-07-13T02:02:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":150,"output_tokens":30,"total_tokens":180}}}}"#,
            ],
        );
        let snapshots = load_snapshots(&root, &directory.path().join("cache.json"));
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].cwd, "/tmp/new");
        assert_eq!(snapshots[0].display, "newer prompt");
        assert_eq!(snapshots[0].messages.len(), 2);
        assert_eq!(snapshots[0].token_days[0].usage.total_tokens, 180);
    }

    #[test]
    fn duplicate_thread_rollouts_keep_independent_cumulative_baselines() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        write_rollout(
            &root.join("old.jsonl"),
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"resumed","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T01:00:30Z","type":"turn_context","payload":{"model":"gpt-5.6-sol"}}"#,
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":80,"output_tokens":20,"total_tokens":100}}}}"#,
            ],
        );
        write_rollout(
            &root.join("resumed.jsonl"),
            &[
                r#"{"timestamp":"2026-07-13T02:00:00Z","type":"session_meta","payload":{"id":"resumed","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T02:00:30Z","type":"turn_context","payload":{"model":"gpt-5.6-luna"}}"#,
                r#"{"timestamp":"2026-07-13T02:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":120,"output_tokens":30,"total_tokens":150}}}}"#,
            ],
        );

        let snapshots = load_snapshots(&root, &directory.path().join("cache.json"));
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].token_days.len(), 2);
        let sol = snapshots[0]
            .token_days
            .iter()
            .find(|day| day.model == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(sol.usage.input_tokens, 80);
        assert_eq!(sol.usage.output_tokens, 20);
        let luna = snapshots[0]
            .token_days
            .iter()
            .find(|day| day.model == "gpt-5.6-luna")
            .unwrap();
        assert_eq!(luna.usage.input_tokens, 120);
        assert_eq!(luna.usage.output_tokens, 30);
        assert_eq!(
            snapshots[0]
                .token_days
                .iter()
                .map(|day| day.usage.total_tokens)
                .sum::<u64>(),
            250
        );
    }

    #[test]
    fn incremental_cache_retains_prior_events_and_resets_after_truncation() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("incremental.jsonl");
        write_rollout(
            &path,
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"incremental","cwd":"/tmp/old","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"event_msg","payload":{"type":"user_message","message":"first"}}"#,
            ],
        );
        let cache_path = directory.path().join("cache.json");
        load_snapshots(&root, &cache_path);
        let first_size = std::fs::metadata(&path).unwrap().len();
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            "\n{}",
            r#"{"timestamp":"2026-07-13T01:02:00Z","type":"event_msg","payload":{"type":"user_message","message":"second"}}"#
        )
        .unwrap();
        let appended = load_snapshots(&root, &cache_path);
        assert_eq!(appended[0].messages.len(), 2);
        let cache: ArchiveCache =
            serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
        assert!(cache.files.values().next().unwrap().offset > first_size);

        write_rollout(
            &path,
            &[
                r#"{"timestamp":"2026-07-13T03:00:00Z","type":"session_meta","payload":{"id":"incremental","cwd":"/tmp/new","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T03:01:00Z","type":"event_msg","payload":{"type":"user_message","message":"replacement"}}"#,
            ],
        );
        let truncated = load_snapshots(&root, &cache_path);
        assert_eq!(truncated[0].display, "replacement");
        assert_eq!(truncated[0].messages.len(), 1);
    }

    #[test]
    fn process_listing_does_not_duplicate_full_archive_and_eviction_keeps_search_exact() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sessions");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.jsonl");
        write_rollout(
            &path,
            &[
                r#"{"type":"session_meta","payload":{"id":"bounded","cwd":"/tmp","source":"cli"}}"#,
                r#"{"type":"event_msg","payload":{"type":"user_message","message":"needle"}}"#,
            ],
        );
        let cache_path = directory.path().join("cache.json");
        load_listing_snapshots(&root, &cache_path);
        let mut state = archive_state().lock().unwrap();
        let archive = state.get_mut(&(root.clone(), cache_path.clone())).unwrap();
        assert!(archive
            .merged
            .iter()
            .all(|s| s.messages.is_empty() && s.token_events.is_empty()));
        assert!(trim_message_cache(&mut archive.cache, 0));
        let snapshot = &archive.cache.files.values().next().unwrap().snapshot;
        assert!(!snapshot.messages_complete);
        assert!(snapshot.messages.is_empty());
        assert_eq!(
            search_rollout(snapshot, "needle", true, false, |text, query, _| text
                .contains(query))
            .as_deref(),
            Some("needle")
        );
    }

    #[test]
    fn merged_evicted_fragments_preserve_the_listing_prompt() {
        let directory = tempfile::tempdir().unwrap();
        let mut old = empty_cached(
            &directory.path().join("old"),
            FileFingerprint {
                size: 0,
                modified_ns: 0,
                identity: 0,
            },
        )
        .snapshot;
        old.thread_id = "same".into();
        old.display = "old prompt".into();
        old.updated_at_ms = 1;
        old.messages_complete = false;
        let mut new = old.clone();
        new.updated_at_ms = 2;
        new.display = "new prompt".into();
        merge_snapshot(&mut old, new);
        assert_eq!(old.display, "new prompt");
    }

    #[test]
    fn incomplete_record_is_reread_without_retaining_a_large_pending_buffer() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pending.jsonl");
        let header = "{\"type\":\"session_meta\",\"payload\":{\"id\":\"partial\"}}\n";
        std::fs::write(&path, format!("{header}{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"{}", "x".repeat(1024*1024))).unwrap();
        let cached = refresh_rollout(&path, fingerprint(&path).unwrap(), None).unwrap();
        assert_eq!(cached.offset, header.len() as u64);
        assert_eq!(cached.pending.capacity(), 0);
        let mut file = File::options().append(true).open(&path).unwrap();
        file.write_all(b"\"}}\n").unwrap();
        drop(file);
        let complete = refresh_rollout(&path, fingerprint(&path).unwrap(), Some(cached)).unwrap();
        assert!(!complete.snapshot.display.is_empty());
        assert_eq!(complete.offset, std::fs::metadata(&path).unwrap().len());
        assert_eq!(complete.pending.capacity(), 0);
    }

    #[test]
    fn archive_does_not_retain_consumed_transcript_buffers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            "{}",
            r#"{"type":"session_meta","payload":{"id":"large","cwd":"/tmp","source":"cli"}}"#
        )
        .unwrap();
        let line = format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"output\":\"{}\"}}}}\n",
            "x".repeat(2048)
        );
        for _ in 0..4096 {
            file.write_all(line.as_bytes()).unwrap();
        }
        drop(file);
        let cached = refresh_rollout(&path, fingerprint(&path).unwrap(), None).unwrap();
        assert!(cached.pending.is_empty());
        assert!(
            cached.pending.capacity() <= 64 * 1024,
            "consumed transcript retained {} bytes",
            cached.pending.capacity()
        );
    }

    #[test]
    fn unchanged_reload_does_not_rewrite_cache() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        write_rollout(
            &root.join("stable.jsonl"),
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"stable","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#,
            ],
        );
        let cache_path = directory.path().join("cache.json");
        load_snapshots(&root, &cache_path);
        let first = std::fs::read(&cache_path).unwrap();
        let sentinel = UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        File::options()
            .write(true)
            .open(&cache_path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(sentinel))
            .unwrap();
        let second = load_snapshots(&root, &cache_path);
        assert_eq!(
            std::fs::metadata(&cache_path).unwrap().modified().unwrap(),
            sentinel
        );
        assert_eq!(second[0].display, "hello");
        let after = std::fs::read(&cache_path).unwrap();
        assert_eq!(
            first, after,
            "unchanged rollouts must not rewrite the archive cache"
        );
    }

    #[test]
    fn process_cache_survives_deleted_cache_file() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        write_rollout(
            &root.join("cached.jsonl"),
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"cached","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"event_msg","payload":{"type":"user_message","message":"keep me"}}"#,
            ],
        );
        let cache_path = directory.path().join("cache.json");
        load_snapshots(&root, &cache_path);
        std::fs::remove_file(&cache_path).unwrap();
        let second = load_snapshots(&root, &cache_path);
        assert_eq!(second[0].display, "keep me");
        assert!(
            !cache_path.exists(),
            "memory hit must not rewrite an unchanged cache"
        );
    }

    #[test]
    fn listing_snapshots_omit_search_index_but_keep_cost_fields() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        write_rollout(
            &root.join("listed.jsonl"),
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"listed","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                r#"{"timestamp":"2026-07-13T01:02:00Z","type":"event_msg","payload":{"type":"user_message","message":"prompt text"}}"#,
                r#"{"timestamp":"2026-07-13T01:03:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"output_tokens":4,"total_tokens":14}}}}"#,
            ],
        );
        let cache_path = directory.path().join("cache.json");
        let listing = load_listing_snapshots(&root, &cache_path);
        assert_eq!(listing[0].display, "prompt text");
        assert!(listing[0].messages.is_empty());
        assert!(listing[0].token_events.is_empty());
        assert_eq!(listing[0].token_days[0].usage.total_tokens, 14);

        let full = load_snapshots(&root, &cache_path);
        assert_eq!(full[0].messages.len(), 1);
        assert_eq!(full[0].token_events.len(), 1);
    }

    #[test]
    #[ignore]
    fn real_home_archive_load_timing() {
        let home = dirs::home_dir().expect("home directory");
        let sessions = default_sessions_root(&home);
        if !sessions.exists() {
            return;
        }
        let started = std::time::Instant::now();
        let first = load_default_listing(&home);
        let first_elapsed = started.elapsed();
        let started = std::time::Instant::now();
        let second = load_default_listing(&home);
        let second_elapsed = started.elapsed();
        let started = std::time::Instant::now();
        let full = load_default_snapshots(&home);
        let full_elapsed = started.elapsed();
        let messages: usize = full.iter().map(|snapshot| snapshot.messages.len()).sum();
        eprintln!(
            "codex archive listing {} sessions: first {first_elapsed:?}, second {second_elapsed:?}; full {} messages in {full_elapsed:?}",
            first.len(),
            messages
        );
        assert_eq!(first.len(), second.len());
        assert_eq!(first.len(), full.len());
        assert!(
            second_elapsed * 4 < first_elapsed || second_elapsed.as_millis() < 200,
            "warm listing should reuse the process cache (first {first_elapsed:?}, second {second_elapsed:?})"
        );
    }

    #[test]
    fn token_stream_id_is_not_persisted_and_still_merges_independent_streams() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        write_rollout(
            &root.join("old.jsonl"),
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"resumed","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T01:00:30Z","type":"turn_context","payload":{"model":"gpt-5.6-sol"}}"#,
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":80,"output_tokens":20,"total_tokens":100}}}}"#,
            ],
        );
        write_rollout(
            &root.join("new.jsonl"),
            &[
                r#"{"timestamp":"2026-07-13T02:00:00Z","type":"session_meta","payload":{"id":"resumed","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T02:00:30Z","type":"turn_context","payload":{"model":"gpt-5.6-luna"}}"#,
                r#"{"timestamp":"2026-07-13T02:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":120,"output_tokens":30,"total_tokens":150}}}}"#,
            ],
        );
        let cache_path = directory.path().join("cache.json");
        load_snapshots(&root, &cache_path);
        // Simulate a fresh process so this exercises stream-id hydration from disk.
        archive_state()
            .lock()
            .unwrap()
            .remove(&(root.clone(), cache_path.clone()));
        let snapshots = load_snapshots(&root, &cache_path);
        let json = String::from_utf8(std::fs::read(&cache_path).unwrap()).unwrap();
        assert!(
            !json.contains("streamId"),
            "per-event stream ids must not bloat the on-disk cache"
        );
        assert_eq!(
            snapshots[0]
                .token_days
                .iter()
                .map(|day| day.usage.total_tokens)
                .sum::<u64>(),
            250
        );
    }
}
