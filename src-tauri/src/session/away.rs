//! "While you were away" summary for a session card: what the session is
//! waiting for, its last assistant line, and what it did since a given time.
//!
//! Activity is counted from the Claude Code transcript only — no AI summary,
//! no network. Each transcript keeps a bounded, content-free event index
//! (timestamp + kind, and a hash for edited file paths) that is extended
//! incrementally on every poll, so a query never re-parses the conversation.

use crate::session::cache::{
    can_incrementally_read, extend_prefix_snapshot, hash_file_prefix, next_full_verify_offset,
    read_lines_from_offset, validate_cached_prefix, FileVersion, PrefixSnapshot,
    PrefixValidationKind, PREFIX_FULL_REVALIDATION_INTERVAL,
};
use crate::session::status::SessionStatus;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

/// One line, short enough for a card row.
pub const LAST_ASSISTANT_MESSAGE_MAX_CHARS: usize = 140;

/// What a session needs from the user. Tool arguments are never included:
/// only the tool *name* leaves the backend (see PR #141).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WaitingFor {
    Approval { tool: String },
    Question,
    Attention,
    Input,
}

pub fn waiting_for(status: &SessionStatus, pending_tool_name: Option<&str>) -> Option<WaitingFor> {
    match status {
        SessionStatus::NeedsAttention => Some(match pending_tool_name {
            Some("Question") | Some("AskUserQuestion") => WaitingFor::Question,
            Some(tool) if !tool.trim().is_empty() => WaitingFor::Approval {
                tool: tool.to_string(),
            },
            _ => WaitingFor::Attention,
        }),
        SessionStatus::WaitingForInput => Some(WaitingFor::Input),
        _ => None,
    }
}

/// One-line, tag-free, bounded preview of the last assistant text.
pub fn last_assistant_line(text: Option<&str>) -> Option<String> {
    let clean = crate::session::strip_system_tags(text?);
    let clean = clean.trim();
    if clean.is_empty() {
        return None;
    }
    Some(crate::session::enrichment::truncate_string(
        clean,
        LAST_ASSISTANT_MESSAGE_MAX_CHARS,
    ))
}

/// Counts of what happened after `since_ms`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AwayActivity {
    pub since_ms: i64,
    pub tool_calls: u32,
    pub files_edited: u32,
    pub commands_run: u32,
    pub subagents_completed: u32,
    /// The index dropped events newer than `since_ms`, so the counts are a
    /// lower bound.
    pub partial: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwayRequest {
    pub session_key: String,
    pub since_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivityKind {
    Edit(u64),
    Command,
    OtherTool,
    SubagentDone,
}

#[derive(Debug, Clone, Copy)]
struct ActivityEvent {
    at_ms: i64,
    kind: ActivityKind,
}

/// Bound per transcript. Older events are dropped first; a query that
/// reaches back past the drop point is flagged `partial`.
const MAX_EVENTS_PER_TRANSCRIPT: usize = 20_000;
const MAX_TRACKED_IDS: usize = 20_000;
const MAX_TRANSCRIPTS: usize = 256;

const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];
const COMMAND_TOOLS: &[&str] = &["Bash"];
const SUBAGENT_TOOLS: &[&str] = &["Agent", "Task"];

#[derive(Debug, Default)]
struct ActivityIndex {
    events: Vec<ActivityEvent>,
    /// Newest timestamp of any event dropped by the bound.
    dropped_through_ms: Option<i64>,
    seen_tool_ids: HashSet<String>,
    /// Agent/Task tool_use id → still waiting for its completion.
    open_subagents: HashSet<String>,
    /// Async agent id (from the launch stub) → Agent/Task tool_use id.
    async_agent_ids: HashMap<String, String>,
}

fn timestamp_ms(value: &Value) -> Option<i64> {
    let ts = value.get("timestamp")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn path_hash(path: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

fn message_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

impl ActivityIndex {
    fn push(&mut self, at_ms: i64, kind: ActivityKind) {
        self.events.push(ActivityEvent { at_ms, kind });
    }

    fn complete_subagent(&mut self, tool_use_id: &str, at_ms: i64) {
        if self.open_subagents.remove(tool_use_id) {
            self.push(at_ms, ActivityKind::SubagentDone);
        }
    }

    fn complete_from_notification(&mut self, text: &str, at_ms: i64) {
        if !text.contains("<task-notification>") {
            return;
        }
        use crate::session::subagents::extract_tag;
        let tool_use_id = extract_tag(text, "tool-use-id")
            .filter(|id| self.open_subagents.contains(id))
            .or_else(|| {
                extract_tag(text, "task-id").and_then(|id| self.async_agent_ids.get(&id).cloned())
            });
        if let Some(id) = tool_use_id {
            self.complete_subagent(&id, at_ms);
        }
    }

    fn ingest(&mut self, lines: &[String]) {
        for line in lines {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if value.get("isSidechain").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let Some(at_ms) = timestamp_ms(&value) else {
                continue;
            };
            let content = value.get("message").and_then(|m| m.get("content"));
            match value.get("type").and_then(Value::as_str) {
                Some("assistant") => {
                    for block in content.and_then(Value::as_array).into_iter().flatten() {
                        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                            continue;
                        }
                        let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                        if id.is_empty() || !self.seen_tool_ids.insert(id.to_string()) {
                            continue;
                        }
                        let input = block.get("input");
                        let kind = if EDIT_TOOLS.contains(&name) {
                            let path = input
                                .and_then(|i| i.get("file_path").or_else(|| i.get("notebook_path")))
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            ActivityKind::Edit(path_hash(path))
                        } else if COMMAND_TOOLS.contains(&name) {
                            ActivityKind::Command
                        } else {
                            if SUBAGENT_TOOLS.contains(&name) {
                                self.open_subagents.insert(id.to_string());
                            }
                            ActivityKind::OtherTool
                        };
                        self.push(at_ms, kind);
                    }
                }
                Some("user") => {
                    let launch = value.get("toolUseResult");
                    let is_async = launch.is_some_and(|r| {
                        r.get("isAsync").and_then(Value::as_bool) == Some(true)
                            || r.get("status").and_then(Value::as_str) == Some("async_launched")
                    });
                    for block in content.and_then(Value::as_array).into_iter().flatten() {
                        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                            continue;
                        }
                        let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
                            continue;
                        };
                        if !self.open_subagents.contains(id) {
                            continue;
                        }
                        if is_async {
                            if let Some(agent_id) = launch
                                .and_then(|r| r.get("agentId"))
                                .and_then(Value::as_str)
                            {
                                self.async_agent_ids
                                    .insert(agent_id.to_string(), id.to_string());
                            }
                        } else {
                            self.complete_subagent(id, at_ms);
                        }
                    }
                    self.complete_from_notification(&message_text(content), at_ms);
                }
                Some("queue-operation") => {
                    if let Some(text) = value.get("content").and_then(Value::as_str) {
                        self.complete_from_notification(text, at_ms);
                    }
                }
                _ => {}
            }
        }
        self.enforce_bounds();
    }

    fn enforce_bounds(&mut self) {
        if self.events.len() > MAX_EVENTS_PER_TRANSCRIPT {
            let excess = self.events.len() - MAX_EVENTS_PER_TRANSCRIPT;
            let newest_dropped = self.events[..excess].iter().map(|e| e.at_ms).max();
            self.dropped_through_ms = self.dropped_through_ms.max(newest_dropped);
            self.events.drain(..excess);
        }
        // Ids only guard against duplicates and late completions; losing old
        // ones can at worst miss a completion for a very old subagent.
        if self.seen_tool_ids.len() > MAX_TRACKED_IDS {
            self.seen_tool_ids.clear();
        }
        if self.async_agent_ids.len() > MAX_TRACKED_IDS {
            self.async_agent_ids.clear();
        }
        if self.open_subagents.len() > MAX_TRACKED_IDS {
            self.open_subagents.clear();
        }
    }

    fn summarize(&self, since_ms: i64) -> AwayActivity {
        let mut out = AwayActivity {
            since_ms,
            partial: self.dropped_through_ms.is_some_and(|ms| ms > since_ms),
            ..AwayActivity::default()
        };
        let mut files = HashSet::new();
        for event in self.events.iter().filter(|e| e.at_ms > since_ms) {
            match event.kind {
                ActivityKind::Edit(file) => {
                    out.tool_calls += 1;
                    files.insert(file);
                }
                ActivityKind::Command => {
                    out.tool_calls += 1;
                    out.commands_run += 1;
                }
                ActivityKind::OtherTool => out.tool_calls += 1,
                ActivityKind::SubagentDone => out.subagents_completed += 1,
            }
        }
        out.files_edited = files.len() as u32;
        out
    }
}

/// Cached index for one transcript and the file state it is current as of.
struct IndexEntry {
    path: PathBuf,
    stamp: FileVersion,
    offset: u64,
    prefix: PrefixSnapshot,
    next_full_verify_offset: u64,
    last_full_validation_at: Instant,
    index: ActivityIndex,
}

/// Keyed by provider-scoped session key.
static AWAY_INDEX: LazyLock<Mutex<HashMap<String, IndexEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Bring the index for `session_key` up to date with its transcript. Called
/// once per poll per Claude Code session. An unchanged file costs one
/// metadata read; a grown file costs a read of only the appended bytes.
pub fn refresh(session_key: &str, path: &Path) {
    refresh_at(session_key, path, Instant::now());
}

fn refresh_at(session_key: &str, path: &Path, now: Instant) {
    // Take the entry out so file reads happen without holding the lock; a
    // concurrent query just sees no index for this one session meanwhile.
    let previous = match AWAY_INDEX.lock() {
        Ok(mut cache) => cache.remove(session_key),
        Err(_) => return,
    };
    let Some(entry) = updated_entry(previous, path, now) else {
        return;
    };
    if let Ok(mut cache) = AWAY_INDEX.lock() {
        cache.insert(session_key.to_string(), entry);
        if cache.len() > MAX_TRANSCRIPTS {
            // Live sessions are re-added on the next poll; dropping an
            // arbitrary entry only costs one rebuild.
            if let Some(key) = cache.keys().find(|k| k.as_str() != session_key).cloned() {
                cache.remove(&key);
            }
        }
    }
}

/// The entry brought up to date with `path`, or `None` when the file cannot
/// be read.
fn updated_entry(previous: Option<IndexEntry>, path: &Path, now: Instant) -> Option<IndexEntry> {
    let stamp = FileVersion::read(path).ok()?;
    if let Some(mut entry) = previous.filter(|e| e.path == path) {
        if entry.stamp == stamp && stamp.supports_unchanged_fast_path() {
            if now.saturating_duration_since(entry.last_full_validation_at)
                < PREFIX_FULL_REVALIDATION_INTERVAL
            {
                return Some(entry);
            }
            if let Some(validation) = validate_cached_prefix(
                path,
                entry.prefix,
                entry.next_full_verify_offset,
                entry.last_full_validation_at,
                now,
            ) {
                entry.prefix = validation.snapshot;
                if validation.kind == PrefixValidationKind::Full {
                    entry.last_full_validation_at = now;
                    entry.next_full_verify_offset = next_full_verify_offset(entry.offset);
                }
                return Some(entry);
            }
        } else if can_incrementally_read(entry.stamp, stamp) {
            if let Some(validation) = validate_cached_prefix(
                path,
                entry.prefix,
                entry.next_full_verify_offset,
                entry.last_full_validation_at,
                now,
            ) {
                if let Ok((lines, new_offset)) = read_lines_from_offset(path, entry.offset) {
                    if let Ok(prefix) =
                        extend_prefix_snapshot(path, entry.offset, new_offset, validation.snapshot)
                    {
                        entry.index.ingest(&lines);
                        entry.stamp = stamp;
                        entry.offset = new_offset;
                        entry.prefix = prefix;
                        if validation.kind == PrefixValidationKind::Full {
                            entry.last_full_validation_at = now;
                            entry.next_full_verify_offset = next_full_verify_offset(new_offset);
                        }
                        return Some(entry);
                    }
                }
            }
        }
    }

    // First sight, replaced file, or failed validation: rebuild from byte 0.
    let (lines, offset) = read_lines_from_offset(path, 0).ok()?;
    let prefix = hash_file_prefix(path, offset).ok()?;
    let mut index = ActivityIndex::default();
    index.ingest(&lines);
    Some(IndexEntry {
        path: path.to_path_buf(),
        stamp,
        offset,
        prefix,
        next_full_verify_offset: next_full_verify_offset(offset),
        last_full_validation_at: now,
        index,
    })
}

/// Drop indexes for sessions that are no longer live.
pub fn retain_sessions(live_keys: &HashSet<&str>) {
    if let Ok(mut cache) = AWAY_INDEX.lock() {
        cache.retain(|key, _| live_keys.contains(key.as_str()));
    }
}

/// Counts since each request's `since_ms`. Sessions without an index (not
/// Claude Code, or not seen yet) are absent from the result.
pub fn away_activity(requests: &[AwayRequest]) -> HashMap<String, AwayActivity> {
    let Ok(cache) = AWAY_INDEX.lock() else {
        return HashMap::new();
    };
    requests
        .iter()
        .filter_map(|req| {
            cache
                .get(&req.session_key)
                .map(|entry| (req.session_key.clone(), entry.index.summarize(req.since_ms)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(v: Value) -> String {
        v.to_string()
    }

    fn tool_use(ts: &str, id: &str, name: &str, input: Value) -> String {
        line(json!({
            "type": "assistant", "timestamp": ts, "uuid": id,
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": id, "name": name, "input": input}
            ]}
        }))
    }

    fn tool_result(ts: &str, id: &str, extra: Option<Value>) -> String {
        let mut v = json!({
            "type": "user", "timestamp": ts, "uuid": format!("r-{id}"),
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": "ok"}
            ]}
        });
        if let Some(extra) = extra {
            v["toolUseResult"] = extra;
        }
        line(v)
    }

    fn ms(ts: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn counts_tools_files_commands_after_since() {
        let lines = vec![
            tool_use(
                "2026-10-14T10:00:00Z",
                "t0",
                "Bash",
                json!({"command": "ls"}),
            ),
            tool_use(
                "2026-10-14T10:05:00Z",
                "t1",
                "Edit",
                json!({"file_path": "/a.rs"}),
            ),
            tool_use(
                "2026-10-14T10:06:00Z",
                "t2",
                "Edit",
                json!({"file_path": "/a.rs"}),
            ),
            tool_use(
                "2026-10-14T10:07:00Z",
                "t3",
                "Write",
                json!({"file_path": "/b.rs"}),
            ),
            tool_use(
                "2026-10-14T10:08:00Z",
                "t4",
                "Bash",
                json!({"command": "npm test"}),
            ),
            tool_use(
                "2026-10-14T10:09:00Z",
                "t5",
                "Read",
                json!({"file_path": "/c.rs"}),
            ),
        ];
        let mut index = ActivityIndex::default();
        index.ingest(&lines);
        let got = index.summarize(ms("2026-10-14T10:01:00Z"));
        assert_eq!(got.tool_calls, 5);
        assert_eq!(got.files_edited, 2);
        assert_eq!(got.commands_run, 1);
        assert_eq!(got.subagents_completed, 0);
        assert!(!got.partial);
        assert_eq!(index.summarize(0).commands_run, 2);
    }

    #[test]
    fn duplicate_tool_use_lines_count_once_and_sidechains_are_skipped() {
        let dup = tool_use("2026-10-14T10:05:00Z", "t1", "Bash", json!({}));
        let mut side: Value =
            serde_json::from_str(&tool_use("2026-10-14T10:06:00Z", "t2", "Bash", json!({})))
                .unwrap();
        side["isSidechain"] = json!(true);
        let mut index = ActivityIndex::default();
        index.ingest(&[dup.clone(), dup, side.to_string()]);
        assert_eq!(index.summarize(0).commands_run, 1);
    }

    #[test]
    fn sync_subagent_completes_on_its_tool_result() {
        let mut index = ActivityIndex::default();
        index.ingest(&[
            tool_use("2026-10-14T10:00:00Z", "a1", "Agent", json!({})),
            tool_result(
                "2026-10-14T10:10:00Z",
                "a1",
                Some(json!({"status": "completed"})),
            ),
            tool_result("2026-10-14T10:11:00Z", "a1", None),
        ]);
        assert_eq!(
            index
                .summarize(ms("2026-10-14T10:05:00Z"))
                .subagents_completed,
            1
        );
        assert_eq!(
            index
                .summarize(ms("2026-10-14T10:10:00Z"))
                .subagents_completed,
            0
        );
    }

    #[test]
    fn async_subagent_completes_on_task_notification_not_on_launch() {
        let mut index = ActivityIndex::default();
        index.ingest(&[
            tool_use("2026-10-14T10:00:00Z", "a1", "Task", json!({})),
            tool_result(
                "2026-10-14T10:00:01Z",
                "a1",
                Some(json!({"isAsync": true, "status": "async_launched", "agentId": "ag-9"})),
            ),
        ]);
        assert_eq!(index.summarize(0).subagents_completed, 0);
        // A background shell notification is not a subagent.
        index.ingest(&[line(json!({
            "type": "queue-operation", "operation": "enqueue",
            "timestamp": "2026-10-14T10:20:00Z",
            "content": "<task-notification><task-id>shell-1</task-id><result>x</result></task-notification>"
        }))]);
        assert_eq!(index.summarize(0).subagents_completed, 0);
        index.ingest(&[
            line(json!({
                "type": "queue-operation", "operation": "enqueue",
                "timestamp": "2026-10-14T10:30:00Z",
                "content": "<task-notification><task-id>ag-9</task-id><result>done</result></task-notification>"
            })),
            // The same notification delivered again as a user message.
            line(json!({
                "type": "user", "timestamp": "2026-10-14T10:30:05Z", "uuid": "u9",
                "message": {"role": "user", "content": "<task-notification><task-id>ag-9</task-id></task-notification>"}
            })),
        ]);
        let got = index.summarize(ms("2026-10-14T10:25:00Z"));
        assert_eq!(got.subagents_completed, 1);
    }

    #[test]
    fn index_is_bounded_and_reports_partial_counts() {
        let mut index = ActivityIndex::default();
        let lines: Vec<String> = (0..MAX_EVENTS_PER_TRANSCRIPT + 10)
            .map(|i| {
                tool_use(
                    &format!(
                        "2026-10-14T10:{:02}:{:02}.{:03}Z",
                        i / 60_000,
                        (i / 1000) % 60,
                        i % 1000
                    ),
                    &format!("t{i}"),
                    "Bash",
                    json!({}),
                )
            })
            .collect();
        index.ingest(&lines);
        assert_eq!(index.events.len(), MAX_EVENTS_PER_TRANSCRIPT);
        assert!(index.summarize(0).partial);
        let recent = index.summarize(index.events[100].at_ms);
        assert!(!recent.partial);
    }

    fn query(key: &str, since_ms: i64) -> Option<AwayActivity> {
        away_activity(&[AwayRequest {
            session_key: key.to_string(),
            since_ms,
        }])
        .remove(key)
    }

    #[test]
    fn refresh_extends_index_on_append_and_drops_on_delete_or_prune() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let key = "claudeCode:away-refresh-test";
        std::fs::write(
            &path,
            format!(
                "{}\n",
                tool_use("2026-10-14T10:00:00Z", "t1", "Bash", json!({}))
            ),
        )
        .unwrap();
        let t0 = Instant::now();
        refresh_at(key, &path, t0);
        assert_eq!(query(key, 0).unwrap().commands_run, 1);

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            "{}",
            tool_use("2026-10-14T10:01:00Z", "t2", "Bash", json!({}))
        )
        .unwrap();
        // A partial trailing line is not consumed until it is complete.
        write!(file, "{{\"type\":\"assistant\"").unwrap();
        drop(file);
        refresh_at(key, &path, t0);
        assert_eq!(query(key, 0).unwrap().commands_run, 2);

        // An in-place rewrite that keeps the length must not keep stale counts.
        let rewritten = format!(
            "{}\n",
            tool_use("2026-10-14T10:00:00Z", "t9", "Read", json!({}))
        );
        std::fs::write(&path, rewritten).unwrap();
        refresh_at(key, &path, t0);
        let got = query(key, 0).unwrap();
        assert_eq!((got.commands_run, got.tool_calls), (0, 1));

        retain_sessions(&HashSet::new());
        assert_eq!(query(key, 0), None);

        refresh_at(key, &path, t0);
        assert!(query(key, 0).is_some());
        std::fs::remove_file(&path).unwrap();
        refresh_at(key, &path, t0);
        assert_eq!(query(key, 0), None);
    }

    /// Per-poll cost on this machine's real transcripts. Run with
    /// `cargo test --lib away_poll_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn away_poll_cost() {
        let projects = dirs::home_dir().unwrap().join(".claude/projects");
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&projects)
            .unwrap()
            .flatten()
            .filter_map(|d| std::fs::read_dir(d.path()).ok())
            .flatten()
            .flatten()
            .map(|f| f.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
            .collect();
        files.sort_by_key(|f| std::cmp::Reverse(f.0));
        files.truncate(12);
        let bytes: u64 = files
            .iter()
            .map(|(_, p)| std::fs::metadata(p).unwrap().len())
            .sum();
        let keys: Vec<String> = (0..files.len()).map(|i| format!("bench:{i}")).collect();
        let cold = Instant::now();
        for (key, (_, path)) in keys.iter().zip(&files) {
            refresh(key, path);
        }
        let cold = cold.elapsed();
        let rounds = 50;
        let warm = Instant::now();
        for _ in 0..rounds {
            for (key, (_, path)) in keys.iter().zip(&files) {
                refresh(key, path);
            }
        }
        let warm = warm.elapsed() / rounds;
        let requests: Vec<AwayRequest> = keys
            .iter()
            .map(|k| AwayRequest {
                session_key: k.clone(),
                since_ms: 0,
            })
            .collect();
        let query = Instant::now();
        for _ in 0..rounds {
            away_activity(&requests);
        }
        let query = query.elapsed() / rounds;
        println!(
            "transcripts={} bytes={} cold_first_poll={:?} warm_poll={:?} query_all={:?}",
            files.len(),
            bytes,
            cold,
            warm,
            query
        );
    }

    #[test]
    fn waiting_for_names_tool_only() {
        assert_eq!(
            waiting_for(&SessionStatus::NeedsAttention, Some("Bash")),
            Some(WaitingFor::Approval {
                tool: "Bash".into()
            })
        );
        assert_eq!(
            waiting_for(&SessionStatus::NeedsAttention, Some("AskUserQuestion")),
            Some(WaitingFor::Question)
        );
        assert_eq!(
            waiting_for(&SessionStatus::NeedsAttention, None),
            Some(WaitingFor::Attention)
        );
        assert_eq!(
            waiting_for(&SessionStatus::WaitingForInput, None),
            Some(WaitingFor::Input)
        );
        assert_eq!(waiting_for(&SessionStatus::Working, Some("Bash")), None);
        let json = serde_json::to_value(WaitingFor::Approval {
            tool: "Bash".into(),
        })
        .unwrap();
        assert_eq!(json, json!({"kind": "approval", "tool": "Bash"}));
    }

    #[test]
    fn last_assistant_line_strips_tags_and_bounds_length() {
        assert_eq!(last_assistant_line(None), None);
        assert_eq!(
            last_assistant_line(Some("<system-reminder>x</system-reminder>  ")),
            None
        );
        assert_eq!(
            last_assistant_line(Some(
                "Done.\n\nTests <system-reminder>r</system-reminder>pass"
            )),
            Some("Done. Tests pass".to_string())
        );
        let long = "a".repeat(500);
        assert!(
            last_assistant_line(Some(&long)).unwrap().chars().count()
                <= LAST_ASSISTANT_MESSAGE_MAX_CHARS + 3
        );
    }
}
