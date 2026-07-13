use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// A raw line from ~/.claude/history.jsonl
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHistoryLine {
    #[serde(default)]
    display: String,
    #[serde(default)]
    timestamp: u64,
    #[serde(default)]
    project: String,
    #[serde(default)]
    session_id: String,
}

/// A deduplicated, enriched history entry returned to the frontend
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub session_id: String,
    pub display: String,
    pub timestamp: u64,
    pub project: String,
    pub project_name: String,
    pub custom_title: Option<String>,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<String>,
}

fn default_provider() -> String {
    "claudeCode".to_string()
}

/// Accumulated state per session during dedup: earliest and latest prompt entries.
struct SessionAccum {
    first: RawHistoryLine,
    last: RawHistoryLine,
}

/// Parse JSONL text into deduplicated HistoryEntry vec, sorted by last activity (newest first).
/// Shows the latest prompt's display text and uses the latest timestamp for sorting.
pub fn parse_history_jsonl(content: &str) -> Vec<HistoryEntry> {
    let mut by_session: HashMap<String, SessionAccum> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(raw) = serde_json::from_str::<RawHistoryLine>(line) {
            if raw.session_id.is_empty() {
                continue;
            }
            let ts = raw.timestamp;
            match by_session.get_mut(&raw.session_id) {
                Some(accum) => {
                    if ts < accum.first.timestamp {
                        accum.first = raw;
                    } else if ts >= accum.last.timestamp {
                        accum.last = raw;
                    }
                }
                None => {
                    let sid = raw.session_id.clone();
                    by_session.insert(
                        sid,
                        SessionAccum {
                            last: raw.clone(),
                            first: raw,
                        },
                    );
                }
            }
        }
    }

    let mut entries: Vec<HistoryEntry> = by_session
        .into_values()
        .map(|accum| {
            let project_name = PathBuf::from(&accum.first.project)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            HistoryEntry {
                session_id: accum.first.session_id,
                display: accum.last.display,
                timestamp: accum.last.timestamp,
                project: accum.first.project,
                project_name,
                custom_title: None,
                provider: default_provider(),
                surface: None,
                agent_kind: None,
            }
        })
        .collect();

    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    entries
}

/// Read ~/.claude/history.jsonl and return deduplicated entries sorted newest first.
/// Custom titles from session-monitor-titles.json are applied when available.
pub fn get_history() -> Result<Vec<HistoryEntry>, String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    let path = home_dir.join(".claude").join("history.jsonl");
    let projects_dir = home_dir.join(".claude").join("projects");
    let mut entries = if path.exists() {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read history.jsonl: {e}"))?;
        filter_and_enrich(parse_history_jsonl(&content), &projects_dir, true)
    } else {
        Vec::new()
    };

    entries.extend(codex_history_entries(&home_dir));
    entries.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
    Ok(entries)
}

fn codex_history_entries(home_dir: &std::path::Path) -> Vec<HistoryEntry> {
    super::codex_archive::load_default_snapshots(home_dir)
        .into_iter()
        .filter(|snapshot| !snapshot.is_internal())
        .map(|snapshot| {
            let project_name = PathBuf::from(&snapshot.cwd)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown")
                .to_string();
            HistoryEntry {
                session_id: snapshot.thread_id,
                display: snapshot.display,
                timestamp: snapshot.updated_at_ms.max(snapshot.created_at_ms),
                project: snapshot.cwd,
                project_name,
                custom_title: None,
                provider: "codex".to_string(),
                surface: Some(snapshot.surface),
                agent_kind: Some(snapshot.agent_kind),
            }
        })
        .collect()
}

/// Drop entries whose session JSONL has been removed (via `/clear`, Claude Code
/// cleanup, or manual deletion) so the HISTORY tab never surfaces rows that
/// would open an empty overlay. Optionally resolves native custom titles via
/// the mtime-cached title reader.
///
/// Exposed so tests can exercise the filter without touching `~/.claude`.
pub(crate) fn filter_and_enrich(
    entries: Vec<HistoryEntry>,
    projects_dir: &std::path::Path,
    enrich_titles: bool,
) -> Vec<HistoryEntry> {
    let mut encoded_cache: HashMap<String, PathBuf> = HashMap::new();
    entries
        .into_iter()
        .filter_map(|mut entry| {
            let project_dir = encoded_cache
                .entry(entry.project.clone())
                .or_insert_with(|| {
                    let encoded = super::detector::encode_path_for_matching(&entry.project);
                    projects_dir.join(encoded)
                });
            let session_file = project_dir.join(format!("{}.jsonl", entry.session_id));
            if !session_file.exists() {
                return None;
            }
            if enrich_titles {
                if let Some(title) = super::enrichment::get_cached_native_title(&session_file) {
                    entry.custom_title = Some(title);
                }
            }
            Some(entry)
        })
        .collect()
}

/// A single deep-search hit: session ID + first matching snippet (truncated).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepSearchHit {
    pub session_id: String,
    /// Up to 200 chars of context around the first keyword match, from the matching line.
    pub snippet: String,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
}

/// Returns true if `haystack` contains `needle` according to the given mode flags.
/// - `case_sensitive = false`: both sides are lowercased (haystack is assumed pre-lowered when passed in).
/// - `whole_word = true`: the match must be flanked by non-alphanumeric chars (or the string ends).
fn phrase_match(haystack: &str, needle: &str, whole_word: bool) -> bool {
    if needle.is_empty() {
        return false;
    }
    if !whole_word {
        return haystack.contains(needle);
    }
    let nlen = needle.len();
    let mut start = 0usize;
    while let Some(rel) = haystack[start..].find(needle) {
        let pos = start + rel;
        // `pos` from str::find is always on a char boundary.
        let before_ok = haystack[..pos]
            .chars()
            .next_back()
            .map_or(true, |c| !c.is_alphanumeric());
        let after_ok = haystack[pos + nlen..]
            .chars()
            .next()
            .map_or(true, |c| !c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        start = pos + 1;
    }
    false
}

/// Extract a short snippet from `text` centred around the first occurrence of the query.
/// `query_norm` is the case-normalised query (lowercased if case-insensitive, original if
/// case-sensitive). `text_norm` is the matching-form of `text`.
/// Returns at most 200 characters with the match roughly in the middle.
fn extract_snippet(text: &str, text_norm: &str, query_norm: &str) -> String {
    if query_norm.is_empty() {
        return text.chars().take(200).collect();
    }
    let Some(pos) = text_norm.find(query_norm) else {
        return text.chars().take(200).collect();
    };
    let word_len = query_norm.len();
    let half = 80usize;
    let start = pos.saturating_sub(half);
    let end = (pos + word_len + half).min(text.len());
    // Align to char boundaries
    let start = text
        .char_indices()
        .map(|(i, _)| i)
        .filter(|&i| i <= start)
        .last()
        .unwrap_or(0);
    let end = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .filter(|&i| i >= end)
        .next()
        .unwrap_or(text.len());

    let mut snippet = String::new();
    if start > 0 {
        snippet.push('…');
    }
    snippet.push_str(&text[start..end]);
    if end < text.len() {
        snippet.push('…');
    }
    snippet
}

/// Extract plain text content from a user or assistant JSONL line.
/// Returns `None` for metadata lines (summary, file-history-snapshot, etc.).
/// For user entries: returns the prompt string (skips tool-result arrays).
/// For assistant entries: concatenates all `text` content blocks.
fn extract_message_text(line: &str) -> Option<String> {
    use serde_json::Value;
    let obj: Value = serde_json::from_str(line).ok()?;
    let msg_type = obj.get("type").and_then(|v| v.as_str())?;

    match msg_type {
        "user" => {
            let content = obj.get("message")?.get("content")?;
            match content {
                Value::String(s) => Some(s.clone()),
                // Array content = tool results — not user-typed text, skip
                _ => None,
            }
        }
        "assistant" => {
            let blocks = obj.get("message")?.get("content")?.as_array()?;
            let text: String = blocks
                .iter()
                .filter_map(|b| {
                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                        b.get("text")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        _ => None,
    }
}

/// Search all session JSONL files under ~/.claude/projects/ for a query.
///
/// Matches the query as a literal substring (phrase match). When
/// `case_sensitive` is false the comparison is lowercased; when `whole_word`
/// is true the match must be flanked by non-alphanumeric chars.
/// Runs file reads concurrently using threads.
pub fn deep_search(
    query: &str,
    case_sensitive: bool,
    whole_word: bool,
) -> Result<Vec<DeepSearchHit>, String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    let projects_dir = home_dir.join(".claude").join("projects");

    let query_norm = if case_sensitive {
        query.to_string()
    } else {
        query.to_lowercase()
    };
    if query_norm.is_empty() {
        return Ok(vec![]);
    }

    // Collect all candidate JSONL file paths
    let mut candidates: Vec<(String, std::path::PathBuf)> = Vec::new();
    if let Ok(project_entries) = std::fs::read_dir(&projects_dir) {
        for project_entry in project_entries.flatten() {
            let project_path = project_entry.path();
            if !project_path.is_dir() {
                continue;
            }
            if let Ok(files) = std::fs::read_dir(&project_path) {
                for file_entry in files.flatten() {
                    let file_path = file_entry.path();
                    if file_path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        if let Some(stem) = file_path.file_stem().and_then(|s| s.to_str()) {
                            // Skip agent-* sidechains and non-UUID files
                            if !stem.starts_with("agent-") && stem.contains('-') {
                                candidates.push((stem.to_string(), file_path));
                            }
                        }
                    }
                }
            }
        }
    }

    // Search files concurrently using threads
    use std::sync::{Arc, Mutex};
    let matched: Arc<Mutex<Vec<DeepSearchHit>>> = Arc::new(Mutex::new(Vec::new()));
    let query_norm = Arc::new(query_norm);

    let handles: Vec<_> = candidates
        .into_iter()
        .map(|(session_id, path)| {
            let matched = Arc::clone(&matched);
            let query_norm = Arc::clone(&query_norm);
            std::thread::spawn(move || {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    // Only search user/assistant message text — skip metadata entries
                    // (summary, file-history-snapshot, etc.) which contain fields like
                    // sessionId, cwd, gitBranch that would trivially match most queries.
                    // Collect (original, matching-form) pairs; matching form is the
                    // original when case-sensitive, or lowercased otherwise.
                    let messages: Vec<(String, String)> = content
                        .lines()
                        .filter_map(|line| extract_message_text(line))
                        .map(|text| {
                            let norm = if case_sensitive {
                                text.clone()
                            } else {
                                text.to_lowercase()
                            };
                            (text, norm)
                        })
                        .collect();
                    // Find first message containing the query (as phrase).
                    let hit = messages
                        .iter()
                        .find(|(_, norm)| phrase_match(norm, query_norm.as_str(), whole_word));
                    if let Some((text, norm)) = hit {
                        let snippet = extract_snippet(text, norm, query_norm.as_str());
                        if !snippet.is_empty() {
                            let mut guard = matched.lock().unwrap();
                            guard.push(DeepSearchHit {
                                session_id,
                                snippet,
                                provider: default_provider(),
                                surface: None,
                            });
                        }
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        let _ = handle.join();
    }

    let mut result = Arc::try_unwrap(matched)
        .map_err(|_| "Arc unwrap failed")?
        .into_inner()
        .map_err(|e| format!("Mutex poisoned: {e}"))?;

    let codex_snapshots: Vec<_> = super::codex_archive::load_default_snapshots(&home_dir)
        .into_iter()
        .filter(|snapshot| !snapshot.is_internal())
        .collect();
    let codex_matched: Arc<Mutex<Vec<DeepSearchHit>>> = Arc::new(Mutex::new(Vec::new()));
    let candidates = Arc::new(codex_snapshots);
    let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let worker_count = candidates.len().min(4);
    let handles: Vec<_> = (0..worker_count)
        .map(|_| {
            let candidates = Arc::clone(&candidates);
            let next = Arc::clone(&next);
            let matched = Arc::clone(&codex_matched);
            let query_norm = Arc::clone(&query_norm);
            std::thread::spawn(move || loop {
                let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(snapshot) = candidates.get(index) else {
                    break;
                };
                if let Some(text) = super::codex_archive::search_rollout(
                    snapshot,
                    query_norm.as_str(),
                    case_sensitive,
                    whole_word,
                    phrase_match,
                ) {
                    let normalized = if case_sensitive {
                        text.clone()
                    } else {
                        text.to_lowercase()
                    };
                    let snippet = extract_snippet(&text, &normalized, query_norm.as_str());
                    if !snippet.is_empty() {
                        matched.lock().unwrap().push(DeepSearchHit {
                            session_id: snapshot.thread_id.clone(),
                            snippet,
                            provider: "codex".to_string(),
                            surface: Some(snapshot.surface.clone()),
                        });
                    }
                }
            })
        })
        .collect();
    for handle in handles {
        let _ = handle.join();
    }
    result.extend(
        Arc::try_unwrap(codex_matched)
            .map_err(|_| "Codex search Arc unwrap failed")?
            .into_inner()
            .map_err(|error| format!("Codex search mutex poisoned: {error}"))?,
    );
    result.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_history_jsonl_empty() {
        let result = parse_history_jsonl("");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_history_jsonl_single_entry() {
        let jsonl = r#"{"display":"Hello world","timestamp":1000,"project":"/Users/you/myproject","sessionId":"abc-123"}"#;
        let result = parse_history_jsonl(jsonl);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].session_id, "abc-123");
        assert_eq!(result[0].display, "Hello world");
        assert_eq!(result[0].project_name, "myproject");
        assert_eq!(result[0].timestamp, 1000);
    }

    #[test]
    fn test_parse_history_jsonl_deduplicates_by_session_id() {
        let jsonl = concat!(
            r#"{"display":"First prompt","timestamp":1000,"project":"/p/proj","sessionId":"abc"}"#,
            "\n",
            r#"{"display":"Second prompt","timestamp":2000,"project":"/p/proj","sessionId":"abc"}"#,
        );
        let result = parse_history_jsonl(jsonl);
        assert_eq!(result.len(), 1);
        // Display shows latest prompt, timestamp reflects last activity
        assert_eq!(result[0].display, "Second prompt");
        assert_eq!(result[0].timestamp, 2000);
    }

    #[test]
    fn test_parse_history_jsonl_sorted_newest_first() {
        let jsonl = concat!(
            r#"{"display":"Old","timestamp":1000,"project":"/p/a","sessionId":"aaa"}"#,
            "\n",
            r#"{"display":"New","timestamp":3000,"project":"/p/b","sessionId":"bbb"}"#,
            "\n",
            r#"{"display":"Mid","timestamp":2000,"project":"/p/c","sessionId":"ccc"}"#,
        );
        let result = parse_history_jsonl(jsonl);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].timestamp, 3000);
        assert_eq!(result[1].timestamp, 2000);
        assert_eq!(result[2].timestamp, 1000);
    }

    #[test]
    fn test_parse_history_jsonl_skips_empty_session_id() {
        let jsonl = concat!(
            r#"{"display":"No session","timestamp":1000,"project":"/p/a","sessionId":""}"#,
            "\n",
            r#"{"display":"Has session","timestamp":2000,"project":"/p/b","sessionId":"valid-id"}"#,
        );
        let result = parse_history_jsonl(jsonl);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].session_id, "valid-id");
    }

    #[test]
    fn test_parse_history_jsonl_skips_malformed_lines() {
        let jsonl = "not json at all\n{\"display\":\"ok\",\"timestamp\":1,\"project\":\"/p\",\"sessionId\":\"s1\"}";
        let result = parse_history_jsonl(jsonl);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_extract_message_text_user_prompt() {
        let line = r#"{"type":"user","uuid":"u1","timestamp":"2025-01-01T00:00:00Z","message":{"role":"user","content":"Hello world"}}"#;
        assert_eq!(extract_message_text(line), Some("Hello world".to_string()));
    }

    #[test]
    fn test_extract_message_text_user_tool_result_skipped() {
        // Tool-result arrays should be skipped (not user-typed text)
        let line = r#"{"type":"user","uuid":"u1","timestamp":"2025-01-01T00:00:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"result"}]}}"#;
        assert_eq!(extract_message_text(line), None);
    }

    #[test]
    fn test_extract_message_text_assistant() {
        let line = r#"{"type":"assistant","uuid":"a1","timestamp":"2025-01-01T00:00:00Z","message":{"role":"assistant","model":"claude","id":"m1","content":[{"type":"text","text":"Sure, I can help."}]}}"#;
        assert_eq!(
            extract_message_text(line),
            Some("Sure, I can help.".to_string())
        );
    }

    #[test]
    fn test_extract_message_text_metadata_skipped() {
        // Summary/metadata lines should return None
        let line = r#"{"type":"summary","summary":"A session","leafUuid":"x","sessionId":"abc","cwd":"/home"}"#;
        assert_eq!(extract_message_text(line), None);
    }

    #[test]
    fn test_project_name_derived_from_path() {
        let jsonl = r#"{"display":"x","timestamp":1,"project":"/Users/you/Documents/GitHub/c9watch","sessionId":"s1"}"#;
        let result = parse_history_jsonl(jsonl);
        assert_eq!(result[0].project_name, "c9watch");
    }

    #[test]
    fn test_filter_and_enrich_drops_missing_jsonl() {
        use std::fs;
        let tmp = tempfile::tempdir().expect("tempdir");
        let projects_dir = tmp.path();

        // Two entries share the same project. Create JSONL for the first only.
        let project = "/p/myproj";
        let encoded = super::super::detector::encode_path_for_matching(project);
        let project_dir = projects_dir.join(&encoded);
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(project_dir.join("keep-sid.jsonl"), "").unwrap();
        // Deliberately do not create drop-sid.jsonl.

        let entries = vec![
            HistoryEntry {
                session_id: "keep-sid".into(),
                display: "kept".into(),
                timestamp: 2,
                project: project.into(),
                project_name: "myproj".into(),
                custom_title: None,
                provider: default_provider(),
                surface: None,
                agent_kind: None,
            },
            HistoryEntry {
                session_id: "drop-sid".into(),
                display: "cleared".into(),
                timestamp: 1,
                project: project.into(),
                project_name: "myproj".into(),
                custom_title: None,
                provider: default_provider(),
                surface: None,
                agent_kind: None,
            },
        ];

        let out = filter_and_enrich(entries, projects_dir, false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].session_id, "keep-sid");
    }

    #[test]
    fn test_filter_and_enrich_drops_all_when_projects_dir_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let entries = vec![HistoryEntry {
            session_id: "orphan".into(),
            display: "x".into(),
            timestamp: 1,
            project: "/p/gone".into(),
            project_name: "gone".into(),
            custom_title: None,
            provider: default_provider(),
            surface: None,
            agent_kind: None,
        }];
        let out = filter_and_enrich(entries, tmp.path(), false);
        assert!(out.is_empty());
    }

    #[test]
    fn codex_history_includes_app_cli_and_normal_subagents_but_not_guardians() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join(".codex/sessions/2026/07/13");
        std::fs::create_dir_all(&sessions).unwrap();
        let fixtures = [
            (
                "app.jsonl",
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"app-history","cwd":"/tmp/app","source":"vscode","originator":"Codex Desktop"}}
{"timestamp":"2026-07-13T01:01:00Z","type":"event_msg","payload":{"type":"user_message","message":"app request"}}"#,
            ),
            (
                "cli.jsonl",
                r#"{"timestamp":"2026-07-13T02:00:00Z","type":"session_meta","payload":{"id":"cli-history","cwd":"/tmp/cli","source":"cli","originator":"codex-tui"}}
{"timestamp":"2026-07-13T02:01:00Z","type":"event_msg","payload":{"type":"user_message","message":"cli request"}}"#,
            ),
            (
                "child.jsonl",
                r#"{"timestamp":"2026-07-13T03:00:00Z","type":"session_meta","payload":{"id":"child-history","cwd":"/tmp/app","originator":"Codex Desktop","source":{"subagent":{"thread_spawn":{"parent_thread_id":"app-history","depth":1}}}}}"#,
            ),
            (
                "guardian.jsonl",
                r#"{"timestamp":"2026-07-13T04:00:00Z","type":"session_meta","payload":{"id":"guardian-history","cwd":"/tmp/app","originator":"Codex Desktop","source":{"subagent":{"other":"guardian"}}}}"#,
            ),
        ];
        for (name, content) in fixtures {
            std::fs::write(sessions.join(name), content).unwrap();
        }

        let entries = codex_history_entries(home.path());
        assert_eq!(entries.len(), 3);
        let app = entries
            .iter()
            .find(|entry| entry.session_id == "app-history")
            .unwrap();
        assert_eq!(app.provider, "codex");
        assert_eq!(app.surface.as_deref(), Some("app"));
        assert_eq!(app.display, "app request");
        let cli = entries
            .iter()
            .find(|entry| entry.session_id == "cli-history")
            .unwrap();
        assert_eq!(cli.surface.as_deref(), Some("cli"));
        assert_eq!(cli.display, "cli request");
        assert!(entries
            .iter()
            .any(|entry| entry.agent_kind.as_deref() == Some("subagent")));
        assert!(!entries
            .iter()
            .any(|entry| entry.session_id == "guardian-history"));
    }

    // ── phrase_match ────────────────────────────────────────────────

    #[test]
    fn test_phrase_match_substring_default() {
        // Plain substring — case-insensitive caller lowercases both sides.
        assert!(phrase_match(
            "hello claude code world",
            "claude code",
            false
        ));
        // Two words present but not as a phrase → no match
        assert!(!phrase_match(
            "claude is here and code is there",
            "claude code",
            false
        ));
    }

    #[test]
    fn test_phrase_match_case_sensitive_caller_controls_case() {
        // Helper itself is byte-level; the case_sensitive flag at the caller
        // decides whether to lowercase inputs. Verify that the raw compare
        // distinguishes case when caller does NOT lowercase.
        assert!(phrase_match("HELLO CLAUDE CODE", "CLAUDE CODE", false));
        assert!(!phrase_match("HELLO CLAUDE CODE", "claude code", false));
    }

    #[test]
    fn test_phrase_match_whole_word() {
        // whole-word ON: "cat" must not match "concatenate"
        assert!(!phrase_match("the concatenate function", "cat", true));
        assert!(phrase_match("the cat sat on the mat", "cat", true));
        // Edge of string
        assert!(phrase_match("cat", "cat", true));
        assert!(phrase_match("cat.", "cat", true));
        assert!(phrase_match(".cat", "cat", true));
        // Numeric boundary also counts as alphanumeric
        assert!(!phrase_match("cats123", "cats", true));
    }

    #[test]
    fn test_phrase_match_empty_needle_false() {
        assert!(!phrase_match("anything", "", false));
        assert!(!phrase_match("anything", "", true));
    }

    #[test]
    fn test_phrase_match_multiple_occurrences_with_whole_word() {
        // Earlier occurrence is a non-boundary hit; later one is a match.
        assert!(phrase_match("catalog, then cat", "cat", true));
    }
}
