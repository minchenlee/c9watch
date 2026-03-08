use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// A raw line from ~/.claude/history.jsonl
#[derive(Debug, Deserialize)]
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
}

/// Parse JSONL text into deduplicated HistoryEntry vec, sorted newest first.
/// Keeps the entry with the highest timestamp for each sessionId.
pub fn parse_history_jsonl(content: &str) -> Vec<HistoryEntry> {
    let mut by_session: HashMap<String, RawHistoryLine> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(raw) = serde_json::from_str::<RawHistoryLine>(line) {
            if raw.session_id.is_empty() {
                continue;
            }
            let existing = by_session.get(&raw.session_id);
            if existing.is_none_or(|e| raw.timestamp < e.timestamp) {
                by_session.insert(raw.session_id.clone(), raw);
            }
        }
    }

    let mut entries: Vec<HistoryEntry> = by_session
        .into_values()
        .map(|raw| {
            let project_name = PathBuf::from(&raw.project)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            HistoryEntry {
                session_id: raw.session_id,
                display: raw.display,
                timestamp: raw.timestamp,
                project: raw.project,
                project_name,
            }
        })
        .collect();

    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    entries
}

/// Read ~/.claude/history.jsonl and return deduplicated entries sorted newest first.
pub fn get_history() -> Result<Vec<HistoryEntry>, String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    let path = home_dir.join(".claude").join("history.jsonl");

    if !path.exists() {
        return Ok(vec![]);
    }

    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read history.jsonl: {e}"))?;

    Ok(parse_history_jsonl(&content))
}

/// A single deep-search hit: session ID + first matching snippet (truncated).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepSearchHit {
    pub session_id: String,
    /// Up to 200 chars of context around the first keyword match, from the matching line.
    pub snippet: String,
}

/// Extract a short snippet from `text` centred around the first occurrence of any query word.
/// For multi-word queries, centres on the first word found.
/// Returns at most 200 characters with the match roughly in the middle.
fn extract_snippet(text: &str, query_lower: &str) -> String {
    let lower = text.to_lowercase();
    let words: Vec<&str> = query_lower.split_whitespace().filter(|w| !w.is_empty()).collect();
    // Find the earliest occurrence of any word
    let pos = words.iter()
        .filter_map(|w| lower.find(w).map(|p| (p, w.len())))
        .min_by_key(|(p, _)| *p);
    let Some((pos, word_len)) = pos else {
        return text.chars().take(200).collect();
    };
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
                        b.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
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

/// Search all session JSONL files under ~/.claude/projects/ for a query string.
/// Returns hits with session ID and a short matching snippet, case-insensitive.
/// Runs file reads concurrently using threads.
pub fn deep_search(query: &str) -> Result<Vec<DeepSearchHit>, String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    let projects_dir = home_dir.join(".claude").join("projects");

    if !projects_dir.exists() {
        return Ok(vec![]);
    }

    let query_lower = query.to_lowercase();
    let query_words: Vec<String> = query_lower
        .split_whitespace()
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect();

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
    let query_lower = Arc::new(query_lower);
    let query_words = Arc::new(query_words);

    let handles: Vec<_> = candidates
        .into_iter()
        .map(|(session_id, path)| {
            let matched = Arc::clone(&matched);
            let query_lower = Arc::clone(&query_lower);
            let query_words = Arc::clone(&query_words);
            std::thread::spawn(move || {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    // Only search user/assistant message text — skip metadata entries
                    // (summary, file-history-snapshot, etc.) which contain fields like
                    // sessionId, cwd, gitBranch that would trivially match most queries.
                    // For multi-word queries, ALL words must appear somewhere in the
                    // session's messages (not necessarily in the same line).
                    // Collect (original, lowercased) pairs so we lowercase once.
                    let messages: Vec<(String, String)> = content
                        .lines()
                        .filter_map(|line| extract_message_text(line))
                        .map(|text| {
                            let lower = text.to_lowercase();
                            (text, lower)
                        })
                        .collect();
                    // Check each word against all messages joined.
                    // W is typically ≤5 and Rust's contains() is SIMD-optimized.
                    let combined_lower: String = messages.iter()
                        .map(|(_, lower)| lower.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let all_match = query_words.iter().all(|w| combined_lower.contains(w.as_str()));
                    if all_match {
                        // Find the first message line containing any query word for snippet
                        let snippet = messages.iter()
                            .find(|(_, lower)| {
                                query_words.iter().any(|w| lower.contains(w.as_str()))
                            })
                            .map(|(text, _)| extract_snippet(text, &query_lower))
                            .unwrap_or_default();
                        if !snippet.is_empty() {
                            let mut guard = matched.lock().unwrap();
                            guard.push(DeepSearchHit { session_id, snippet });
                        }
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        let _ = handle.join();
    }

    let result = Arc::try_unwrap(matched)
        .map_err(|_| "Arc unwrap failed")?
        .into_inner()
        .map_err(|e| format!("Mutex poisoned: {e}"))?;

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
        assert_eq!(result[0].display, "First prompt");
        assert_eq!(result[0].timestamp, 1000);
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
}
