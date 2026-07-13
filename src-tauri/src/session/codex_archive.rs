use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

const CACHE_VERSION: u32 = 1;
const MAX_DISPLAY_CHARS: usize = 400;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedRollout {
    fingerprint: FileFingerprint,
    snapshot: CodexRolloutSnapshot,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveCache {
    version: u32,
    files: HashMap<String, CachedRollout>,
}

static ARCHIVE_CACHE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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

pub(crate) fn is_injected_codex_text(value: &str) -> bool {
    let value = value.trim_start();
    [
        "<environment_context>",
        "<permissions instructions>",
        "<app-context>",
        "<collaboration_mode>",
        "<skills_instructions>",
        "<recommended_plugins>",
        "<developer>",
        "<system>",
        "<memory>",
        "<user_instructions>",
    ]
    .iter()
    .any(|prefix| value.starts_with(prefix))
}

fn message_text(value: &Value) -> Option<(String, String)> {
    let payload = value.get("payload")?;
    if value.get("type").and_then(Value::as_str) == Some("response_item")
        && payload.get("type").and_then(Value::as_str) == Some("message")
    {
        let role = payload.get("role")?.as_str()?.to_string();
        if role != "user" && role != "assistant" {
            return None;
        }
        let text = payload
            .get("content")?
            .as_array()?
            .iter()
            .filter_map(|block| {
                let kind = block.get("type").and_then(Value::as_str)?;
                if matches!(kind, "input_text" | "output_text" | "text") {
                    block.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        if text.trim().is_empty() || (role == "user" && is_injected_codex_text(&text)) {
            return None;
        }
        return Some((role, text));
    }

    if value.get("type").and_then(Value::as_str) == Some("event_msg") {
        let kind = payload.get("type").and_then(Value::as_str)?;
        let role = match kind {
            "user_message" => "user",
            "agent_message" => "assistant",
            _ => return None,
        };
        let text = payload.get("message").and_then(Value::as_str)?.to_string();
        if text.trim().is_empty() || (role == "user" && is_injected_codex_text(&text)) {
            return None;
        }
        return Some((role.to_string(), text));
    }
    None
}

fn classify_session(source: &Value, originator: &str) -> (String, String, Option<String>) {
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
    let internal_label = subagent
        .as_str()
        .or_else(|| subagent.get("other").and_then(Value::as_str))
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ["review", "guardian", "compact", "memory"]
        .iter()
        .any(|part| internal_label.contains(part))
    {
        (surface, "internal".to_string(), None)
    } else {
        (surface, "subagent".to_string(), None)
    }
}

#[derive(Default)]
struct DayAccum {
    timestamp: String,
    usage: CodexTokenUsage,
    models: HashMap<String, u64>,
}

pub(crate) fn scan_rollout(path: &Path) -> Option<CodexRolloutSnapshot> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::with_capacity(128 * 1024, file);
    let mut line = String::new();
    let mut thread_id = String::new();
    let mut cwd = String::new();
    let mut surface = "unknown".to_string();
    let mut agent_kind = "root".to_string();
    let mut parent_thread_id = None;
    let mut display = String::new();
    let mut created_at_ms = 0;
    let mut updated_at_ms = 0;
    let mut current_model = "unknown".to_string();
    let mut previous_total = CodexTokenUsage::default();
    let mut token_days: HashMap<String, DayAccum> = HashMap::new();

    loop {
        line.clear();
        let read = reader.read_line(&mut line).ok()?;
        if read == 0 {
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(line.trim_end()) else {
            // A live writer may leave a partial final JSONL record. Its size change will
            // invalidate the cache and the completed record will be read next time.
            continue;
        };
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let timestamp_ms = timestamp_ms(timestamp);
        if timestamp_ms > 0 {
            if created_at_ms == 0 {
                created_at_ms = timestamp_ms;
            }
            updated_at_ms = updated_at_ms.max(timestamp_ms);
        }

        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                let payload = value.get("payload").unwrap_or(&Value::Null);
                thread_id = payload
                    .get("id")
                    .or_else(|| payload.get("session_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                cwd = payload
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let source = payload.get("source").unwrap_or(&Value::Null);
                let originator = payload
                    .get("originator")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                (surface, agent_kind, parent_thread_id) = classify_session(source, originator);
            }
            Some("turn_context") => {
                let payload = value.get("payload").unwrap_or(&Value::Null);
                if let Some(model) = payload.get("model").and_then(Value::as_str) {
                    current_model = model.to_string();
                }
                if cwd.is_empty() {
                    cwd = payload
                        .get("cwd")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                }
            }
            Some("event_msg")
                if value.pointer("/payload/type").and_then(Value::as_str)
                    == Some("token_count") =>
            {
                let usage_value = value
                    .pointer("/payload/info/total_token_usage")
                    .or_else(|| value.pointer("/payload/total_token_usage"));
                if let Some(usage_value) = usage_value {
                    let cumulative = CodexTokenUsage::from_value(usage_value);
                    if !cumulative.is_zero() {
                        let delta = cumulative.delta_from(previous_total);
                        previous_total = cumulative;
                        if !delta.is_zero() {
                            let date = timestamp_date(timestamp);
                            let day = token_days.entry(date).or_default();
                            if day.timestamp.is_empty() || timestamp < day.timestamp.as_str() {
                                day.timestamp = timestamp.to_string();
                            }
                            day.usage.add_assign(delta);
                            *day.models.entry(current_model.clone()).or_default() +=
                                delta.total_tokens;
                        }
                    }
                } else if let Some(last) = value.pointer("/payload/info/last_token_usage") {
                    let usage = CodexTokenUsage::from_value(last);
                    if !usage.is_zero() {
                        let date = timestamp_date(timestamp);
                        let day = token_days.entry(date).or_default();
                        if day.timestamp.is_empty() || timestamp < day.timestamp.as_str() {
                            day.timestamp = timestamp.to_string();
                        }
                        day.usage.add_assign(usage);
                        *day.models.entry(current_model.clone()).or_default() += usage.total_tokens;
                    }
                }
            }
            _ => {}
        }

        if let Some((role, text)) = message_text(&value) {
            if role == "user" {
                display = truncate(text.trim(), MAX_DISPLAY_CHARS);
            }
        }
    }

    if thread_id.is_empty() {
        return None;
    }
    let mut token_days: Vec<CodexTokenDay> = token_days
        .into_iter()
        .map(|(date, day)| {
            let model = day
                .models
                .into_iter()
                .max_by_key(|(_, tokens)| *tokens)
                .map(|(model, _)| model)
                .unwrap_or_else(|| "unknown".to_string());
            CodexTokenDay {
                date,
                timestamp: day.timestamp,
                model,
                usage: day.usage,
            }
        })
        .collect();
    token_days.sort_by(|left, right| left.date.cmp(&right.date));
    Some(CodexRolloutSnapshot {
        thread_id,
        cwd,
        surface,
        agent_kind,
        parent_thread_id,
        display,
        created_at_ms,
        updated_at_ms,
        token_days,
        path: path.to_path_buf(),
    })
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
        let unchanged = cache
            .files
            .get(&key)
            .is_some_and(|entry| entry.fingerprint == fingerprint);
        if unchanged {
            continue;
        }
        if let Some(snapshot) = scan_rollout(&path) {
            cache.files.insert(
                key,
                CachedRollout {
                    fingerprint,
                    snapshot,
                },
            );
        } else {
            cache.files.remove(&key);
        }
    }
    write_cache(cache_path, &cache);

    // A thread can have more than one persisted path after migrations/resume. Keep
    // the newest complete snapshot and never double-count its history or tokens.
    let mut by_thread: HashMap<String, CodexRolloutSnapshot> = HashMap::new();
    for entry in cache.files.into_values() {
        let snapshot = entry.snapshot;
        match by_thread.get(&snapshot.thread_id) {
            Some(existing)
                if (existing.updated_at_ms, existing.path.as_os_str())
                    >= (snapshot.updated_at_ms, snapshot.path.as_os_str()) => {}
            _ => {
                by_thread.insert(snapshot.thread_id.clone(), snapshot);
            }
        }
    }
    by_thread.into_values().collect()
}

pub(crate) fn load_default_snapshots(home: &Path) -> Vec<CodexRolloutSnapshot> {
    load_snapshots(&default_sessions_root(home), &default_cache_path(home))
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct SearchCacheKey {
    path: PathBuf,
    fingerprint: FileFingerprint,
    query: String,
    case_sensitive: bool,
    whole_word: bool,
}

static SEARCH_CACHE: OnceLock<Mutex<HashMap<SearchCacheKey, Option<String>>>> = OnceLock::new();
const MAX_SEARCH_CACHE_ENTRIES: usize = 2048;

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
    let fingerprint = fingerprint(&snapshot.path)?;
    let key = SearchCacheKey {
        path: snapshot.path.clone(),
        fingerprint,
        query: query.to_string(),
        case_sensitive,
        whole_word,
    };
    let cache = SEARCH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = cache.lock().ok()?.get(&key).cloned() {
        return cached;
    }

    let file = File::open(&snapshot.path).ok()?;
    let mut reader = BufReader::with_capacity(128 * 1024, file);
    let mut line = String::new();
    let mut result = None;
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(line.trim_end()) else {
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
            result = Some(text);
            break;
        }
    }

    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= MAX_SEARCH_CACHE_ENTRIES {
            cache.clear();
        }
        cache.insert(key, result.clone());
    }
    result
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

        assert_eq!(scan_rollout(&app).unwrap().surface, "app");
        assert_eq!(scan_rollout(&cli).unwrap().surface, "cli");
        let child = scan_rollout(&child).unwrap();
        assert_eq!(child.agent_kind, "subagent");
        assert_eq!(child.parent_thread_id.as_deref(), Some("app-1"));
        assert!(scan_rollout(&guardian).unwrap().is_internal());
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
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"completed prompt"}]}}"#
            ),
        )
        .unwrap();
        let second = load_snapshots(&root, &cache);
        assert_eq!(second[0].display, "completed prompt");
    }

    #[test]
    fn injected_context_is_not_used_as_display() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("messages.jsonl");
        write_rollout(
            &path,
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"message-1","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"real request"}]}}"#,
                r#"{"timestamp":"2026-07-13T01:02:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>secret system context</environment_context>"}]}}"#,
            ],
        );
        assert_eq!(scan_rollout(&path).unwrap().display, "real request");
    }

    #[test]
    fn deep_search_skips_injected_and_developer_messages() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("search.jsonl");
        write_rollout(
            &path,
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"search-1","cwd":"/tmp/p","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"private needle"}]}}"#,
                r#"{"timestamp":"2026-07-13T01:02:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>private needle</environment_context>"}]}}"#,
                r#"{"timestamp":"2026-07-13T01:03:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"visible needle"}]}}"#,
            ],
        );
        let snapshot = scan_rollout(&path).unwrap();
        let hit = search_rollout(&snapshot, "needle", false, false, |text, query, _| {
            text.contains(query)
        });
        assert_eq!(hit.as_deref(), Some("visible needle"));
    }

    #[test]
    fn duplicate_thread_ids_keep_newest_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        write_rollout(
            &root.join("old.jsonl"),
            &[
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"duplicate","cwd":"/tmp/old","source":"cli","originator":"codex-tui"}}"#,
            ],
        );
        write_rollout(
            &root.join("new.jsonl"),
            &[
                r#"{"timestamp":"2026-07-13T02:00:00Z","type":"session_meta","payload":{"id":"duplicate","cwd":"/tmp/new","source":"cli","originator":"codex-tui"}}"#,
            ],
        );
        let snapshots = load_snapshots(&root, &directory.path().join("cache.json"));
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].cwd, "/tmp/new");
    }
}
