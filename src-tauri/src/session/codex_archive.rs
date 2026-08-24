use super::cache::FileVersion;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const CACHE_VERSION: u32 = 5;
const MAX_DISPLAY_CHARS: usize = 400;
const MAX_INDEXED_MESSAGES: usize = 20_000;
const MAX_INDEXED_MESSAGE_CHARS: usize = 16_384;
const MAX_INDEXED_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
const FILE_ANCHOR_BYTES: usize = 256;
const MAX_REFRESH_ATTEMPTS: usize = 3;
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

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
    #[serde(default)]
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
    pub changed_ns: u64,
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
    #[serde(default)]
    pending: Vec<u8>,
    #[serde(default)]
    current_model: String,
    #[serde(default)]
    indexed_message_bytes: usize,
    #[serde(default)]
    anchor: Vec<u8>,
    /// Stable hash of every byte in `[0, offset)`. The anchor is only a cheap
    /// early rejection; this hash is the correctness check for growing files.
    #[serde(default)]
    prefix_hash: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveCache {
    version: u32,
    files: HashMap<String, CachedRollout>,
}

static ARCHIVE_CACHE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn fingerprint(path: &Path) -> Option<FileFingerprint> {
    let version = FileVersion::read(path).ok()?;
    Some(FileFingerprint {
        size: version.len,
        modified_ns: version.modified_nanos.min(u64::MAX as u128) as u64,
        changed_ns: version.changed_nanos.min(u64::MAX as u128) as u64,
        identity: version.identity,
    })
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
        prefix_hash: FNV_OFFSET_BASIS,
    }
}

fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn hash_file_prefix(path: &Path, length: u64) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let mut remaining = length;
    let mut hash = FNV_OFFSET_BASIS;
    let mut buffer = [0; 8192];
    while remaining > 0 {
        let limit = remaining.min(buffer.len() as u64) as usize;
        let read = file.read(&mut buffer[..limit]).ok()?;
        if read == 0 {
            return None;
        }
        hash = hash_bytes(hash, &buffer[..read]);
        remaining -= read as u64;
    }
    Some(hash)
}

fn has_strong_fingerprint(fingerprint: FileFingerprint) -> bool {
    super::cache::has_strong_file_stamp(u128::from(fingerprint.changed_ns), fingerprint.identity)
}

fn refresh_rollout(
    path: &Path,
    initial_fingerprint: FileFingerprint,
    previous: Option<CachedRollout>,
) -> Option<CachedRollout> {
    let mut expected = initial_fingerprint;
    for _ in 0..MAX_REFRESH_ATTEMPTS {
        let cached = refresh_rollout_once(path, expected, previous.clone())?;
        let observed = fingerprint(path)?;
        if observed == expected {
            return Some(cached);
        }
        expected = observed;
    }
    None
}

fn refresh_rollout_once(
    path: &Path,
    fingerprint: FileFingerprint,
    previous: Option<CachedRollout>,
) -> Option<CachedRollout> {
    let mut file = File::open(path).ok()?;
    let mut reset = previous.as_ref().map_or(true, |cached| {
        !has_strong_fingerprint(fingerprint)
            || !has_strong_fingerprint(cached.fingerprint)
            || cached.prefix_hash == 0
            || fingerprint.identity != cached.fingerprint.identity
            || fingerprint.size < cached.offset
            || (fingerprint.size == cached.fingerprint.size
                && (fingerprint.modified_ns != cached.fingerprint.modified_ns
                    || fingerprint.changed_ns != cached.fingerprint.changed_ns))
    });
    if !reset {
        let cached = previous.as_ref()?;
        // The fixed-size anchor is useful for a cheap early rejection, but it
        // cannot detect a rewrite before the last 256 bytes. Verify the whole
        // cached prefix before appending so a middle rewrite cannot leave stale
        // messages or token totals in the archive cache.
        if !cached.anchor.is_empty() {
            let anchor_start = cached.offset.saturating_sub(cached.anchor.len() as u64);
            file.seek(SeekFrom::Start(anchor_start)).ok()?;
            let mut current = vec![0; cached.anchor.len()];
            if file.read_exact(&mut current).is_err() || current != cached.anchor {
                reset = true;
            }
        }
        if !reset
            && match hash_file_prefix(path, cached.offset) {
                Some(hash) => hash != cached.prefix_hash,
                None => true,
            }
        {
            reset = true;
        }
    }
    let mut cached = if reset {
        empty_cached(path, fingerprint)
    } else {
        previous?
    };
    file.seek(SeekFrom::Start(cached.offset)).ok()?;
    let mut appended = Vec::new();
    file.read_to_end(&mut appended).ok()?;
    cached.offset += appended.len() as u64;
    cached.prefix_hash = hash_bytes(cached.prefix_hash, &appended);
    cached.pending.extend_from_slice(&appended);

    let complete_len = cached
        .pending
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let complete = cached.pending[..complete_len].to_vec();
    cached.pending.drain(..complete_len);
    for line in complete.split(|byte| *byte == b'\n') {
        if let Ok(value) = serde_json::from_slice::<Value>(line) {
            apply_archive_event(&mut cached, &value);
        }
    }
    // Static rollout files and tests may omit a terminal newline. A syntactically
    // complete record is safe to consume; an incomplete writer record remains buffered.
    if let Ok(value) = serde_json::from_slice::<Value>(&cached.pending) {
        apply_archive_event(&mut cached, &value);
        cached.pending.clear();
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

fn apply_archive_event(cached: &mut CachedRollout, value: &Value) {
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let millis = timestamp_ms(timestamp);
    if millis > 0 {
        if cached.snapshot.created_at_ms == 0 {
            cached.snapshot.created_at_ms = millis;
        }
        cached.snapshot.updated_at_ms = cached.snapshot.updated_at_ms.max(millis);
    }
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
    let Ok(json) = serde_json::to_vec(cache) else {
        return;
    };
    let temporary = path.with_extension("json.tmp");
    if std::fs::write(&temporary, json).is_ok() {
        let _ = std::fs::rename(temporary, path);
    }
}

pub(crate) fn load_snapshots(root: &Path, cache_path: &Path) -> Vec<CodexRolloutSnapshot> {
    let _guard = ARCHIVE_CACHE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut cache: ArchiveCache = std::fs::read(cache_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|cache: &ArchiveCache| cache.version == CACHE_VERSION)
        .unwrap_or_else(|| ArchiveCache {
            version: CACHE_VERSION,
            files: HashMap::new(),
        });

    let candidates = collect_rollouts(root);
    let active_paths: HashSet<String> = candidates
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect();
    cache.files.retain(|path, _| active_paths.contains(path));

    for path in candidates {
        let Some(fingerprint) = fingerprint(&path) else {
            continue;
        };
        let key = path.to_string_lossy().to_string();
        if cache.files.get(&key).is_some_and(|entry| {
            entry.fingerprint == fingerprint
                && entry.prefix_hash != 0
                && has_strong_fingerprint(fingerprint)
        }) {
            continue;
        }
        let previous = cache.files.remove(&key);
        if let Some(cached) = refresh_rollout(&path, fingerprint, previous) {
            if !cached.snapshot.thread_id.is_empty() {
                cache.files.insert(key, cached);
            }
        } else {
            cache.files.remove(&key);
        }
    }
    write_cache(cache_path, &cache);

    // A resumed thread may span several rollout paths. Merge their authoritative
    // events so older prompts and token usage survive without double counting overlap.
    let mut by_thread: HashMap<String, CodexRolloutSnapshot> = HashMap::new();
    for entry in cache.files.into_values() {
        let snapshot = entry.snapshot;
        if let Some(existing) = by_thread.get_mut(&snapshot.thread_id) {
            merge_snapshot(existing, snapshot);
        } else {
            by_thread.insert(snapshot.thread_id.clone(), snapshot);
        }
    }
    by_thread.into_values().collect()
}

fn merge_snapshot(existing: &mut CodexRolloutSnapshot, incoming: CodexRolloutSnapshot) {
    let incoming_is_newer = (incoming.updated_at_ms, incoming.path.as_os_str())
        > (existing.updated_at_ms, existing.path.as_os_str());
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
    existing.display = existing
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .map(|message| truncate(message.text.trim(), MAX_DISPLAY_CHARS))
        .unwrap_or_default();
    rebuild_token_days(existing);
}

pub(crate) fn load_default_snapshots(home: &Path) -> Vec<CodexRolloutSnapshot> {
    load_snapshots(&default_sessions_root(home), &default_cache_path(home))
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
    fn archive_cache_invalidates_same_length_rewrite_with_preserved_mtime() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("same-length.jsonl");
        let old_prompt = "old prompt";
        let new_prompt = "new prompt";
        let session = r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"same-length","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#;
        let old_message = format!(
            r#"{{"timestamp":"2026-07-13T01:01:00Z","type":"event_msg","payload":{{"type":"user_message","message":"{old_prompt}"}}}}"#
        );
        let new_message = format!(
            r#"{{"timestamp":"2026-07-13T01:01:00Z","type":"event_msg","payload":{{"type":"user_message","message":"{new_prompt}"}}}}"#
        );
        assert_eq!(old_message.len(), new_message.len());
        std::fs::write(&path, format!("{session}\n{old_message}")).unwrap();
        let original_modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let cache_path = directory.path().join("cache.json");
        assert_eq!(load_snapshots(&root, &cache_path)[0].display, old_prompt);

        std::fs::write(&path, format!("{session}\n{new_message}")).unwrap();
        std::fs::File::open(&path)
            .unwrap()
            .set_modified(original_modified)
            .unwrap();
        let refreshed = load_snapshots(&root, &cache_path);
        assert_eq!(refreshed[0].display, new_prompt);
    }

    #[test]
    fn archive_cache_detects_middle_rewrite_before_anchor_on_append() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("middle-rewrite.jsonl");
        let old_prompt = format!("old prompt {}", "a".repeat(96));
        let new_prompt = format!("new prompt {}", "b".repeat(96));
        assert_eq!(old_prompt.len(), new_prompt.len());
        let session = r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"middle-rewrite","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#;
        let old_message = format!(
            r#"{{"timestamp":"2026-07-13T01:01:00Z","type":"event_msg","payload":{{"type":"user_message","message":"{old_prompt}"}}}}"#
        );
        let stable_tail = format!(
            r#"{{"timestamp":"2026-07-13T01:02:00Z","type":"event_msg","payload":{{"type":"agent_message","message":"{}"}}}}"#,
            "stable tail ".repeat(48)
        );
        assert!(stable_tail.len() > FILE_ANCHOR_BYTES);
        std::fs::write(&path, format!("{session}\n{old_message}\n{stable_tail}")).unwrap();
        let cache_path = directory.path().join("cache.json");
        assert_eq!(load_snapshots(&root, &cache_path)[0].display, old_prompt);

        let new_message = format!(
            r#"{{"timestamp":"2026-07-13T01:01:00Z","type":"event_msg","payload":{{"type":"user_message","message":"{new_prompt}"}}}}"#
        );
        assert_eq!(old_message.len(), new_message.len());
        std::fs::write(&path, format!("{session}\n{new_message}\n{stable_tail}")).unwrap();
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            r#"{{"timestamp":"2026-07-13T01:03:00Z","type":"event_msg","payload":{{"type":"agent_message","message":"appended"}}}}"#
        )
        .unwrap();

        let refreshed = load_snapshots(&root, &cache_path);
        assert_eq!(refreshed[0].display, new_prompt);
        assert!(refreshed[0]
            .messages
            .iter()
            .any(|message| message.text == new_prompt));
        assert!(!refreshed[0]
            .messages
            .iter()
            .any(|message| message.text == old_prompt));
    }
}
