// src-tauri/src/session/cost.rs

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Per-model pricing in USD per million tokens.
struct ModelPricing {
    input: f64,
    cache_write: f64,
    cache_read: f64,
    output: f64,
}

/// OpenAI Standard short-context API pricing in USD per million tokens.
/// Source: https://developers.openai.com/api/docs/pricing (checked 2026-07-13).
struct CodexModelPricing {
    input: f64,
    cached_input: f64,
    output: f64,
}

fn get_codex_pricing(model: &str) -> Option<CodexModelPricing> {
    Some(match model {
        "gpt-5.6-sol" => CodexModelPricing {
            input: 5.0,
            cached_input: 0.50,
            output: 30.0,
        },
        "gpt-5.6-terra" => CodexModelPricing {
            input: 2.50,
            cached_input: 0.25,
            output: 15.0,
        },
        "gpt-5.6-luna" => CodexModelPricing {
            input: 1.0,
            cached_input: 0.10,
            output: 6.0,
        },
        "gpt-5.5" => CodexModelPricing {
            input: 5.0,
            cached_input: 0.50,
            output: 30.0,
        },
        "gpt-5.4" => CodexModelPricing {
            input: 2.50,
            cached_input: 0.25,
            output: 15.0,
        },
        "gpt-5.4-mini" => CodexModelPricing {
            input: 0.75,
            cached_input: 0.075,
            output: 4.50,
        },
        "gpt-5.4-nano" => CodexModelPricing {
            input: 0.20,
            cached_input: 0.02,
            output: 1.25,
        },
        "gpt-5.3-codex" => CodexModelPricing {
            input: 1.75,
            cached_input: 0.175,
            output: 14.0,
        },
        _ => return None,
    })
}

/// Estimate a Codex session's API-equivalent cost. `input_tokens` includes
/// cached input, while `output_tokens` already includes reasoning output.
fn estimate_codex_cost(
    model: &str,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
) -> Option<f64> {
    let pricing = get_codex_pricing(model)?;
    let uncached_input_tokens = input_tokens.saturating_sub(cached_input_tokens);
    Some(
        (uncached_input_tokens as f64 * pricing.input
            + cached_input_tokens as f64 * pricing.cached_input
            + output_tokens as f64 * pricing.output)
            / 1_000_000.0,
    )
}

/// Map model ID strings to their pricing. Handles both dated and undated variants.
/// Pricing sourced from Claude Code v2.1.76 binary (2026-03-15).
fn get_pricing(model: &str, speed: &str) -> Option<ModelPricing> {
    // Normalize: strip date suffixes like "-20250929"
    let base = if model.starts_with("claude-sonnet") {
        "sonnet"
    } else if model.starts_with("claude-opus-4-5") || model.starts_with("claude-opus-4-6") {
        "opus-new"
    } else if model.starts_with("claude-opus") {
        "opus-legacy"
    } else if model.starts_with("claude-haiku-4-5") {
        "haiku-new"
    } else if model.starts_with("claude-haiku") {
        "haiku-legacy"
    } else {
        return None;
    };

    Some(match base {
        "sonnet" => ModelPricing {
            input: 3.0,
            cache_write: 3.75,
            cache_read: 0.30,
            output: 15.0,
        },
        "opus-new" if speed == "fast" => ModelPricing {
            input: 30.0,
            cache_write: 37.50,
            cache_read: 3.00,
            output: 150.0,
        },
        "opus-new" => ModelPricing {
            input: 5.0,
            cache_write: 6.25,
            cache_read: 0.50,
            output: 25.0,
        },
        "opus-legacy" => ModelPricing {
            input: 15.0,
            cache_write: 18.75,
            cache_read: 1.50,
            output: 75.0,
        },
        "haiku-new" => ModelPricing {
            input: 1.0,
            cache_write: 1.25,
            cache_read: 0.10,
            output: 5.0,
        },
        "haiku-legacy" => ModelPricing {
            input: 0.80,
            cache_write: 1.0,
            cache_read: 0.08,
            output: 4.0,
        },
        _ => return None,
    })
}

/// Calculate USD cost from token counts, model ID, and speed mode.
fn calculate_cost(
    model: &str,
    speed: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation: u64,
    cache_read: u64,
) -> f64 {
    let Some(pricing) = get_pricing(model, speed) else {
        return 0.0;
    };
    (input_tokens as f64 * pricing.input
        + output_tokens as f64 * pricing.output
        + cache_creation as f64 * pricing.cache_write
        + cache_read as f64 * pricing.cache_read)
        / 1_000_000.0
}

/// Token usage extracted from a single assistant message line.
struct UsageEntry {
    model: String,
    speed: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
    timestamp: String,
    session_id: String,
    cwd: String,
}

/// Result of parsing a single JSONL line — dispatched by entry type.
enum ParsedLine {
    Usage(UsageEntry),
    Name(SessionNameInfo),
}

/// Session name extracted from a JSONL line.
enum SessionNameInfo {
    CustomTitle { session_id: String, title: String },
    FirstUserMessage { session_id: String, content: String },
}

/// Parse a JSONL line once and extract either usage data or session name info.
fn parse_line(line: &str) -> Option<ParsedLine> {
    use serde_json::Value;
    let obj: Value = serde_json::from_str(line).ok()?;
    let entry_type = obj.get("type").and_then(|v| v.as_str())?;

    match entry_type {
        "assistant" => {
            let msg = obj.get("message")?;
            let usage = msg.get("usage")?;
            let model = msg
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            // Skip synthetic messages — they carry zero tokens and produce empty cost records
            if model == "<synthetic>" {
                return None;
            }

            let speed = usage
                .get("speed")
                .and_then(|v| v.as_str())
                .unwrap_or("standard");

            Some(ParsedLine::Usage(UsageEntry {
                model: model.to_string(),
                speed: speed.to_string(),
                input_tokens: usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                output_tokens: usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                cache_creation_input_tokens: usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                cache_read_input_tokens: usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                timestamp: obj
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                session_id: obj
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                cwd: obj
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }))
        }
        "custom-title" => {
            let title = obj.get("customTitle").and_then(|v| v.as_str())?.to_string();
            let session_id = obj.get("sessionId").and_then(|v| v.as_str())?.to_string();
            if title.is_empty() {
                return None;
            }
            Some(ParsedLine::Name(SessionNameInfo::CustomTitle {
                session_id,
                title,
            }))
        }
        "user" => {
            let session_id = obj.get("sessionId").and_then(|v| v.as_str())?.to_string();
            let msg = obj.get("message")?;
            if msg.get("role").and_then(|v| v.as_str()) != Some("user") {
                return None;
            }
            let content = match msg.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|block| {
                        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                            block
                                .get("text")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => return None,
            };
            if super::parser::is_system_content(&content) {
                return None;
            }
            let cleaned = super::sanitize::strip_system_tags(&content);
            if cleaned.is_empty() {
                return None;
            }
            Some(ParsedLine::Name(SessionNameInfo::FirstUserMessage {
                session_id,
                content: cleaned,
            }))
        }
        _ => None,
    }
}

/// A single session's cost record (stored in cache).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCostRecord {
    pub session_id: String,
    pub project: String,
    pub project_name: String,
    pub model: String, // primary model (highest cost)
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<String>,
    pub cost: f64,
    #[serde(default)]
    pub cost_available: bool,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub reasoning_output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64, // input_tokens + output_tokens
    pub timestamp: String, // ISO 8601 — earliest assistant message
    pub date: String,      // "2026-02-28" derived from timestamp
    #[serde(default)]
    pub session_name: String, // custom title or first user message
}

fn default_provider() -> String {
    "claudeCode".to_string()
}

/// Aggregated cost data returned to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostData {
    pub total_cost: f64,
    pub total_tokens: u64,
    /// Tokens whose provider/model has no local USD pricing table.
    pub unpriced_tokens: u64,
    pub daily_costs: Vec<DailyCost>,
    pub project_costs: Vec<ProjectCost>,
    pub model_costs: Vec<ModelCost>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyCost {
    pub date: String,
    pub cost: f64,
    pub sessions: Vec<SessionCostRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCost {
    pub project: String,
    pub project_name: String,
    pub total_cost: f64,
    pub sessions: Vec<SessionCostRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    pub model: String,
    pub display_name: String,
    #[serde(default = "default_provider")]
    pub provider: String,
    pub cost: f64,
    pub percentage: f64,
    pub total_tokens: u64,
    pub cost_available: bool,
}

/// Bump this when pricing or token counting logic changes to force cache rebuild.
const CACHE_VERSION: u32 = 6;

/// Cache structure stored at ~/.claude/cost-cache.json
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CostCache {
    /// Cache format version — mismatches trigger full rebuild
    #[serde(default)]
    version: u32,
    /// Per-file mtime (unix seconds) — used to skip unchanged files
    file_mtimes: HashMap<String, u64>,
    /// All session cost records
    sessions: Vec<SessionCostRecord>,
}

/// Map a model ID to a short display name.
fn model_display_name(model: &str) -> String {
    if model.starts_with("claude-sonnet") {
        "Sonnet 4.6".to_string()
    } else if model.starts_with("claude-opus") {
        "Opus 4.6".to_string()
    } else if model.starts_with("claude-haiku") {
        "Haiku 4.5".to_string()
    } else {
        model.to_string()
    }
}

/// Derive project name from a cwd path (last segment).
fn project_name_from_path(path: &str) -> String {
    PathBuf::from(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Extract the date portion "YYYY-MM-DD" from an ISO 8601 timestamp.
fn date_from_timestamp(ts: &str) -> String {
    ts.get(..10).unwrap_or("unknown").to_string()
}

/// Scan a single JSONL file and return per-(session, date) cost records.
fn scan_file(path: &std::path::Path) -> Vec<SessionCostRecord> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    // Group entries by (session_id, date) in a single pass, and collect session name info
    let mut by_key: HashMap<(String, String), Vec<UsageEntry>> = HashMap::new();
    let mut cwd_by_session: HashMap<String, String> = HashMap::new();
    let mut custom_titles: HashMap<String, String> = HashMap::new();
    let mut first_user_msg: HashMap<String, String> = HashMap::new();

    for line in content.lines() {
        match parse_line(line) {
            Some(ParsedLine::Usage(entry)) => {
                if !entry.session_id.is_empty() {
                    let sid = entry.session_id.clone();
                    if !entry.cwd.is_empty() {
                        cwd_by_session
                            .entry(sid.clone())
                            .or_insert_with(|| entry.cwd.clone());
                    }
                    let date = date_from_timestamp(&entry.timestamp);
                    by_key.entry((sid, date)).or_default().push(entry);
                }
            }
            Some(ParsedLine::Name(SessionNameInfo::CustomTitle { session_id, title })) => {
                custom_titles.insert(session_id, title);
            }
            Some(ParsedLine::Name(SessionNameInfo::FirstUserMessage {
                session_id,
                content,
            })) => {
                first_user_msg.entry(session_id).or_insert(content);
            }
            None => {}
        }
    }

    // Pre-compute project_name per session to avoid redundant path parsing
    let name_by_session: HashMap<&str, String> = cwd_by_session
        .iter()
        .map(|(sid, cwd)| (sid.as_str(), project_name_from_path(cwd)))
        .collect();

    let mut records = Vec::new();

    for ((session_id, date), day_entries) in by_key {
        let cwd = cwd_by_session.get(&session_id).cloned().unwrap_or_default();
        let project_name = name_by_session
            .get(session_id.as_str())
            .cloned()
            .unwrap_or_else(|| project_name_from_path(&cwd));

        let mut cost_by_model: HashMap<String, f64> = HashMap::new();
        let mut total_cost = 0.0;
        let mut total_tokens: u64 = 0;
        let mut input_tokens: u64 = 0;
        let mut cached_input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut cost_available = false;
        let mut earliest_ts = &day_entries[0].timestamp;

        for e in &day_entries {
            let c = calculate_cost(
                &e.model,
                &e.speed,
                e.input_tokens,
                e.output_tokens,
                e.cache_creation_input_tokens,
                e.cache_read_input_tokens,
            );
            total_cost += c;
            total_tokens += e.input_tokens + e.output_tokens;
            input_tokens += e.input_tokens;
            cached_input_tokens += e.cache_read_input_tokens;
            output_tokens += e.output_tokens;
            cost_available |= get_pricing(&e.model, &e.speed).is_some();
            *cost_by_model.entry(e.model.clone()).or_default() += c;
            if e.timestamp < *earliest_ts {
                earliest_ts = &e.timestamp;
            }
        }

        let primary_model = cost_by_model
            .iter()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(m, _)| m.clone())
            .unwrap_or_default();

        // Resolve session name: custom title > first user message (truncated) > empty
        let session_name = custom_titles
            .get(&session_id)
            .cloned()
            .or_else(|| {
                first_user_msg.get(&session_id).map(|msg| {
                    let truncated: String = msg.chars().take(40).collect();
                    if truncated.len() < msg.len() {
                        format!("{}…", truncated)
                    } else {
                        truncated
                    }
                })
            })
            .unwrap_or_default();

        records.push(SessionCostRecord {
            session_id,
            project: cwd,
            project_name,
            model: primary_model,
            provider: default_provider(),
            surface: None,
            agent_kind: None,
            cost: total_cost,
            cost_available,
            input_tokens,
            cached_input_tokens,
            output_tokens,
            reasoning_output_tokens: 0,
            total_tokens,
            timestamp: earliest_ts.clone(),
            date,
            session_name,
        });
    }

    records
}

fn codex_cost_records(home_dir: &std::path::Path) -> Vec<SessionCostRecord> {
    super::codex_archive::load_default_snapshots(home_dir)
        .into_iter()
        .filter(|snapshot| !snapshot.is_internal())
        .flat_map(|snapshot| {
            let project_name = project_name_from_path(&snapshot.cwd);
            let session_name = {
                let truncated: String = snapshot.display.chars().take(40).collect();
                if snapshot.display.chars().count() > 40 {
                    format!("{truncated}…")
                } else {
                    truncated
                }
            };
            snapshot.token_days.into_iter().map(move |day| {
                let estimated_cost = estimate_codex_cost(
                    &day.model,
                    day.usage.input_tokens,
                    day.usage.cached_input_tokens,
                    day.usage.output_tokens,
                );
                SessionCostRecord {
                    session_id: snapshot.thread_id.clone(),
                    project: snapshot.cwd.clone(),
                    project_name: project_name.clone(),
                    model: day.model,
                    provider: "codex".to_string(),
                    surface: Some(snapshot.surface.clone()),
                    agent_kind: Some(snapshot.agent_kind.clone()),
                    cost: estimated_cost.unwrap_or_default(),
                    cost_available: estimated_cost.is_some(),
                    input_tokens: day.usage.input_tokens,
                    cached_input_tokens: day.usage.cached_input_tokens,
                    output_tokens: day.usage.output_tokens,
                    reasoning_output_tokens: day.usage.reasoning_output_tokens,
                    total_tokens: day.usage.total_tokens,
                    timestamp: day.timestamp,
                    date: day.date,
                    session_name: session_name.clone(),
                }
            })
        })
        .collect()
}

/// Load cache from disk, scan new/modified files, update cache, return aggregated CostData.
pub fn get_cost_data() -> Result<CostData, String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    let projects_dir = home_dir.join(".claude").join("projects");
    let cache_path = home_dir.join(".claude").join("cost-cache.json");

    // Load existing cache, rebuild if version mismatch (pricing/logic changed)
    let mut cache: CostCache = std::fs::read_to_string(&cache_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .map(|c: CostCache| {
            if c.version != CACHE_VERSION {
                CostCache {
                    version: CACHE_VERSION,
                    file_mtimes: HashMap::new(),
                    sessions: vec![],
                }
            } else {
                c
            }
        })
        .unwrap_or(CostCache {
            version: CACHE_VERSION,
            file_mtimes: HashMap::new(),
            sessions: vec![],
        });

    // Collect all JSONL candidate files with their mtimes
    let mut candidates: Vec<(String, PathBuf, u64)> = Vec::new();
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
                            if !stem.starts_with("agent-") && stem.contains('-') {
                                let mtime = file_entry
                                    .metadata()
                                    .and_then(|m| m.modified())
                                    .ok()
                                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0);
                                let key = file_path.to_string_lossy().to_string();
                                candidates.push((key, file_path, mtime));
                            }
                        }
                    }
                }
            }
        }
    }

    // Find files that are new or modified since last cache
    let files_to_scan: Vec<(String, PathBuf)> = candidates
        .iter()
        .filter(|(key, _, mtime)| {
            cache
                .file_mtimes
                .get(key)
                .map_or(true, |cached_mtime| mtime > cached_mtime)
        })
        .map(|(key, path, _)| (key.clone(), path.clone()))
        .collect();

    if !files_to_scan.is_empty() {
        // Remove stale session records from files we're about to re-scan
        let rescan_paths: std::collections::HashSet<&str> =
            files_to_scan.iter().map(|(k, _)| k.as_str()).collect();

        // We need to identify which sessions came from which file.
        // Since we don't track that, re-scan all changed files and
        // remove sessions whose session_id appears in the new scan.
        let new_records: Vec<SessionCostRecord> = {
            let matched: Arc<Mutex<Vec<SessionCostRecord>>> = Arc::new(Mutex::new(Vec::new()));
            let handles: Vec<_> = files_to_scan
                .iter()
                .map(|(_, path)| {
                    let matched = Arc::clone(&matched);
                    let path = path.clone();
                    std::thread::spawn(move || {
                        let records = scan_file(&path);
                        let mut guard = matched.lock().unwrap();
                        guard.extend(records);
                    })
                })
                .collect();
            for h in handles {
                let _ = h.join();
            }
            Arc::try_unwrap(matched)
                .map_err(|_| "Arc unwrap failed")?
                .into_inner()
                .map_err(|e| format!("Mutex poisoned: {e}"))?
        };

        // Merge: remove old records for sessions that appear in new_records
        let new_session_ids: std::collections::HashSet<&str> =
            new_records.iter().map(|r| r.session_id.as_str()).collect();
        cache
            .sessions
            .retain(|r| !new_session_ids.contains(r.session_id.as_str()));
        cache.sessions.extend(new_records);

        // Update mtimes for scanned files
        for (key, _, mtime) in &candidates {
            if rescan_paths.contains(key.as_str()) {
                cache.file_mtimes.insert(key.clone(), *mtime);
            }
        }

        // Also add mtimes for files we didn't scan (first-time cache build)
        for (key, _, mtime) in &candidates {
            cache.file_mtimes.entry(key.clone()).or_insert(*mtime);
        }

        // Write updated cache
        if let Ok(json) = serde_json::to_string(&cache) {
            let _ = std::fs::write(&cache_path, json);
        }
    }

    let mut sessions = cache.sessions;
    sessions.extend(codex_cost_records(&home_dir));
    Ok(aggregate(&sessions))
}

/// Aggregate flat session records into the CostData structure.
fn aggregate(sessions: &[SessionCostRecord]) -> CostData {
    let total_cost: f64 = sessions.iter().map(|s| s.cost).sum();
    let total_tokens: u64 = sessions.iter().map(|s| s.total_tokens).sum();
    let unpriced_tokens: u64 = sessions
        .iter()
        .filter(|session| !session.cost_available)
        .map(|session| session.total_tokens)
        .sum();

    // --- Daily costs (sorted newest first) ---
    let mut by_date: HashMap<String, Vec<SessionCostRecord>> = HashMap::new();
    for s in sessions {
        by_date.entry(s.date.clone()).or_default().push(s.clone());
    }
    let mut daily_costs: Vec<DailyCost> = by_date
        .into_iter()
        .map(|(date, mut sess)| {
            sess.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
            let cost = sess.iter().map(|s| s.cost).sum();
            DailyCost {
                date,
                cost,
                sessions: sess,
            }
        })
        .collect();
    daily_costs.sort_by(|a, b| b.date.cmp(&a.date));

    // --- Project costs (sorted by total cost desc) ---
    let mut by_project: HashMap<String, Vec<SessionCostRecord>> = HashMap::new();
    for s in sessions {
        by_project
            .entry(s.project.clone())
            .or_default()
            .push(s.clone());
    }
    let mut project_costs: Vec<ProjectCost> = by_project
        .into_iter()
        .map(|(project, mut sess)| {
            sess.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
            let total = sess.iter().map(|s| s.cost).sum();
            let project_name = sess
                .first()
                .map(|s| s.project_name.clone())
                .unwrap_or_default();
            ProjectCost {
                project,
                project_name,
                total_cost: total,
                sessions: sess,
            }
        })
        .collect();
    project_costs.sort_by(|a, b| {
        b.total_cost
            .partial_cmp(&a.total_cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // --- Model costs (sorted by cost desc) ---
    let mut by_model: HashMap<(String, String), (f64, u64, bool)> = HashMap::new();
    for s in sessions {
        // Attribute entire session cost to its primary model for simplicity
        let entry = by_model
            .entry((s.provider.clone(), s.model.clone()))
            .or_insert((0.0, 0, false));
        entry.0 += s.cost;
        entry.1 += s.total_tokens;
        entry.2 |= s.cost_available;
    }
    let mut model_costs: Vec<ModelCost> = by_model
        .into_iter()
        .map(
            |((provider, model), (cost, model_tokens, cost_available))| {
                let pct = if total_cost > 0.0 {
                    cost / total_cost * 100.0
                } else {
                    0.0
                };
                ModelCost {
                    display_name: model_display_name(&model),
                    model,
                    provider,
                    cost,
                    percentage: pct,
                    total_tokens: model_tokens,
                    cost_available,
                }
            },
        )
        .collect();
    model_costs.sort_by(|a, b| {
        b.cost
            .partial_cmp(&a.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    CostData {
        total_cost,
        total_tokens,
        unpriced_tokens,
        daily_costs,
        project_costs,
        model_costs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_pricing_sonnet_variants() {
        assert!(get_pricing("claude-sonnet-4-6", "standard").is_some());
        assert!(get_pricing("claude-sonnet-4-5-20250929", "standard").is_some());
    }

    #[test]
    fn test_get_pricing_opus() {
        assert!(get_pricing("claude-opus-4-6", "standard").is_some());
        assert!(get_pricing("claude-opus-4-1", "standard").is_some());
    }

    #[test]
    fn test_get_pricing_opus_new_vs_legacy() {
        let new = get_pricing("claude-opus-4-6", "standard").unwrap();
        let legacy = get_pricing("claude-opus-4-1", "standard").unwrap();
        assert!(
            (new.input - 5.0).abs() < 1e-10,
            "Opus 4.6 input should be $5/MTok"
        );
        assert!(
            (legacy.input - 15.0).abs() < 1e-10,
            "Opus 4.1 input should be $15/MTok"
        );
    }

    #[test]
    fn test_get_pricing_opus_fast_mode() {
        let fast = get_pricing("claude-opus-4-6", "fast").unwrap();
        let standard = get_pricing("claude-opus-4-6", "standard").unwrap();
        assert!(
            (fast.input - 30.0).abs() < 1e-10,
            "Opus 4.6 fast input should be $30/MTok"
        );
        assert!(
            (standard.input - 5.0).abs() < 1e-10,
            "Opus 4.6 standard input should be $5/MTok"
        );
    }

    #[test]
    fn test_get_pricing_haiku() {
        assert!(get_pricing("claude-haiku-4-5-20251001", "standard").is_some());
    }

    #[test]
    fn test_get_pricing_haiku_new_vs_legacy() {
        let new = get_pricing("claude-haiku-4-5-20251001", "standard").unwrap();
        assert!(
            (new.input - 1.0).abs() < 1e-10,
            "Haiku 4.5 input should be $1/MTok"
        );
        assert!(
            (new.output - 5.0).abs() < 1e-10,
            "Haiku 4.5 output should be $5/MTok"
        );
    }

    #[test]
    fn test_get_pricing_unknown_returns_none() {
        assert!(get_pricing("unknown", "standard").is_none());
        assert!(get_pricing("<synthetic>", "standard").is_none());
        assert!(get_pricing("", "standard").is_none());
    }

    #[test]
    fn test_estimate_codex_cost_sol() {
        let cost = estimate_codex_cost("gpt-5.6-sol", 1_000, 200, 100).unwrap();
        assert!((cost - 0.0071).abs() < 1e-10);
    }

    #[test]
    fn test_estimate_codex_cost_luna() {
        let cost = estimate_codex_cost("gpt-5.6-luna", 1_000, 0, 100).unwrap();
        assert!((cost - 0.0016).abs() < 1e-10);
    }

    #[test]
    fn test_estimate_codex_cost_charges_cached_input_separately() {
        let cost = estimate_codex_cost("gpt-5.4", 1_000, 400, 0).unwrap();
        // 600 uncached at $2.50/MTok + 400 cached at $0.25/MTok.
        assert!((cost - 0.0016).abs() < 1e-10);
    }

    #[test]
    fn test_estimate_codex_cost_unknown_model_is_unpriced() {
        assert!(estimate_codex_cost("gpt-future-codex", 1_000, 400, 100).is_none());
    }

    #[test]
    fn test_codex_standard_pricing_table() {
        let expected = [
            ("gpt-5.6-sol", 5.0, 0.50, 30.0),
            ("gpt-5.6-terra", 2.50, 0.25, 15.0),
            ("gpt-5.6-luna", 1.0, 0.10, 6.0),
            ("gpt-5.5", 5.0, 0.50, 30.0),
            ("gpt-5.4", 2.50, 0.25, 15.0),
            ("gpt-5.4-mini", 0.75, 0.075, 4.50),
            ("gpt-5.4-nano", 0.20, 0.02, 1.25),
            ("gpt-5.3-codex", 1.75, 0.175, 14.0),
        ];

        for (model, input, cached_input, output) in expected {
            let pricing = get_codex_pricing(model).unwrap();
            assert!((pricing.input - input).abs() < 1e-10, "{model} input");
            assert!(
                (pricing.cached_input - cached_input).abs() < 1e-10,
                "{model} cached input"
            );
            assert!((pricing.output - output).abs() < 1e-10, "{model} output");
        }
    }

    #[test]
    fn test_calculate_cost_sonnet() {
        // 1000 input tokens at $3/M = $0.003
        // 500 output tokens at $15/M = $0.0075
        // 2000 cache write at $3.75/M = $0.0075
        // 10000 cache read at $0.30/M = $0.003
        let cost = calculate_cost("claude-sonnet-4-6", "standard", 1000, 500, 2000, 10000);
        let expected = 0.003 + 0.0075 + 0.0075 + 0.003;
        assert!((cost - expected).abs() < 1e-10);
    }

    #[test]
    fn test_calculate_cost_opus_new() {
        // 1000 input at $5/M = $0.005
        // 500 output at $25/M = $0.0125
        let cost = calculate_cost("claude-opus-4-6", "standard", 1000, 500, 0, 0);
        let expected = 0.005 + 0.0125;
        assert!((cost - expected).abs() < 1e-10);
    }

    #[test]
    fn test_calculate_cost_unknown_model_returns_zero() {
        let cost = calculate_cost("unknown", "standard", 1000, 500, 2000, 10000);
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn test_parse_line_assistant() {
        let line = r#"{"type":"assistant","sessionId":"abc-123","timestamp":"2026-02-28T10:00:00Z","cwd":"/Users/you/proj","message":{"model":"claude-sonnet-4-6","role":"assistant","id":"m1","content":[],"usage":{"input_tokens":100,"output_tokens":200,"cache_creation_input_tokens":300,"cache_read_input_tokens":400}}}"#;
        let parsed = parse_line(line).unwrap();
        if let ParsedLine::Usage(entry) = parsed {
            assert_eq!(entry.model, "claude-sonnet-4-6");
            assert_eq!(entry.input_tokens, 100);
            assert_eq!(entry.output_tokens, 200);
            assert_eq!(entry.cache_creation_input_tokens, 300);
            assert_eq!(entry.cache_read_input_tokens, 400);
            assert_eq!(entry.session_id, "abc-123");
        } else {
            panic!("Expected ParsedLine::Usage");
        }
    }

    #[test]
    fn test_parse_line_user_returns_name() {
        let line = r#"{"type":"user","sessionId":"abc-123","timestamp":"2026-02-28T10:00:00Z","message":{"role":"user","content":"hello world"}}"#;
        let parsed = parse_line(line).unwrap();
        assert!(matches!(
            parsed,
            ParsedLine::Name(SessionNameInfo::FirstUserMessage { .. })
        ));
    }

    #[test]
    fn test_parse_line_no_usage_returns_none() {
        let line = r#"{"type":"assistant","message":{"model":"claude-sonnet-4-6","role":"assistant","id":"m1","content":[]}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn test_parse_line_synthetic_returns_none() {
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-02-28T10:00:00Z","message":{"model":"<synthetic>","role":"assistant","content":[],"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn test_parse_line_system_content_skipped() {
        let line = r#"{"type":"user","sessionId":"abc","timestamp":"2026-02-28T10:00:00Z","message":{"role":"user","content":"<local-command-caveat>Caveat: system text</local-command-caveat>"}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn test_aggregate_includes_total_tokens() {
        let sessions = vec![
            SessionCostRecord {
                session_id: "s1".into(),
                project: "/tmp/a".into(),
                project_name: "a".into(),
                model: "claude-sonnet-4-6".into(),
                provider: default_provider(),
                surface: None,
                agent_kind: None,
                cost: 1.0,
                cost_available: true,
                input_tokens: 5000,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_output_tokens: 0,
                total_tokens: 5000,
                timestamp: "2026-03-14T10:00:00Z".into(),
                date: "2026-03-14".into(),
                session_name: String::new(),
            },
            SessionCostRecord {
                session_id: "s2".into(),
                project: "/tmp/a".into(),
                project_name: "a".into(),
                model: "claude-sonnet-4-6".into(),
                provider: default_provider(),
                surface: None,
                agent_kind: None,
                cost: 2.0,
                cost_available: true,
                input_tokens: 8000,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_output_tokens: 0,
                total_tokens: 8000,
                timestamp: "2026-03-14T11:00:00Z".into(),
                date: "2026-03-14".into(),
                session_name: String::new(),
            },
        ];
        let data = aggregate(&sessions);
        assert_eq!(data.total_tokens, 13000);
    }

    #[test]
    fn test_scan_file_splits_by_date() {
        let dir = std::env::temp_dir().join("c9watch_test_split");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("test-split.jsonl");

        let content = [
            r#"{"type":"assistant","sessionId":"sess-1","timestamp":"2026-03-20T10:00:00Z","cwd":"/tmp/proj","message":{"model":"claude-sonnet-4-6","role":"assistant","id":"m1","content":[],"usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            r#"{"type":"assistant","sessionId":"sess-1","timestamp":"2026-03-20T14:00:00Z","cwd":"/tmp/proj","message":{"model":"claude-sonnet-4-6","role":"assistant","id":"m2","content":[],"usage":{"input_tokens":200,"output_tokens":100,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            r#"{"type":"assistant","sessionId":"sess-1","timestamp":"2026-03-21T09:00:00Z","cwd":"/tmp/proj","message":{"model":"claude-opus-4-6","role":"assistant","id":"m3","content":[],"usage":{"input_tokens":400,"output_tokens":200,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
        ].join("\n");
        std::fs::write(&file, &content).unwrap();

        let records = scan_file(&file);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(records.len(), 2, "Should produce 2 records for 2 days");

        let mut records = records;
        records.sort_by(|a, b| a.date.cmp(&b.date));

        // Day 1: Sonnet — 300 input + 150 output = 450 tokens
        // Cost: (300 * 3.0 + 150 * 15.0) / 1_000_000 = 0.003150
        assert_eq!(records[0].date, "2026-03-20");
        assert_eq!(records[0].session_id, "sess-1");
        assert_eq!(records[0].total_tokens, 450);
        assert!((records[0].cost - 0.003150).abs() < 1e-10);
        assert_eq!(records[0].model, "claude-sonnet-4-6");
        assert_eq!(records[0].timestamp, "2026-03-20T10:00:00Z");
        assert_eq!(records[0].project, "/tmp/proj");

        // Day 2: Opus — 400 input + 200 output = 600 tokens
        // Cost: (400 * 5.0 + 200 * 25.0) / 1_000_000 = 0.007000
        assert_eq!(records[1].date, "2026-03-21");
        assert_eq!(records[1].session_id, "sess-1");
        assert_eq!(records[1].total_tokens, 600);
        assert!((records[1].cost - 0.007000).abs() < 1e-10);
        assert_eq!(records[1].model, "claude-opus-4-6");
        assert_eq!(records[1].timestamp, "2026-03-21T09:00:00Z");
        assert_eq!(records[1].project, "/tmp/proj");
    }

    #[test]
    fn test_scan_file_single_day_unchanged() {
        let dir = std::env::temp_dir().join("c9watch_test_single");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("test-single.jsonl");

        let content = [
            r#"{"type":"assistant","sessionId":"sess-2","timestamp":"2026-03-20T10:00:00Z","cwd":"/tmp/proj","message":{"model":"claude-sonnet-4-6","role":"assistant","id":"m1","content":[],"usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            r#"{"type":"assistant","sessionId":"sess-2","timestamp":"2026-03-20T14:00:00Z","cwd":"/tmp/proj","message":{"model":"claude-sonnet-4-6","role":"assistant","id":"m2","content":[],"usage":{"input_tokens":200,"output_tokens":100,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
        ].join("\n");
        std::fs::write(&file, &content).unwrap();

        let records = scan_file(&file);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            records.len(),
            1,
            "Single-day session should produce 1 record"
        );
        assert_eq!(records[0].date, "2026-03-20");
        assert_eq!(records[0].total_tokens, 450);
        assert!((records[0].cost - 0.003150).abs() < 1e-10);
    }

    #[test]
    fn codex_cost_records_include_token_breakdown_and_usd_estimate() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join(".codex/sessions/2026/07/13");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("app.jsonl"),
            [
                r#"{"timestamp":"2026-07-13T01:00:00Z","type":"session_meta","payload":{"id":"codex-app-cost","cwd":"/tmp/app","source":"vscode","originator":"Codex Desktop"}}"#,
                r#"{"timestamp":"2026-07-13T01:01:00Z","type":"event_msg","payload":{"type":"user_message","message":"measure this session"}}"#,
                r#"{"timestamp":"2026-07-13T01:02:00Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                r#"{"timestamp":"2026-07-13T01:03:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":120,"cached_input_tokens":40,"output_tokens":30,"reasoning_output_tokens":12,"total_tokens":150}}}}"#,
                r#"{"timestamp":"2026-07-13T01:04:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"cached_input_tokens":100,"output_tokens":80,"reasoning_output_tokens":32,"total_tokens":380}}}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            sessions.join("cli.jsonl"),
            [
                r#"{"timestamp":"2026-07-13T01:30:00Z","type":"session_meta","payload":{"id":"codex-cli-cost","cwd":"/tmp/cli","source":"cli","originator":"codex-tui"}}"#,
                r#"{"timestamp":"2026-07-13T01:31:00Z","type":"turn_context","payload":{"model":"gpt-5.4-mini"}}"#,
                r#"{"timestamp":"2026-07-13T01:32:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":20,"cached_input_tokens":5,"output_tokens":10,"reasoning_output_tokens":2,"total_tokens":30}}}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            sessions.join("guardian.jsonl"),
            [
                r#"{"timestamp":"2026-07-13T02:00:00Z","type":"session_meta","payload":{"id":"codex-guardian-cost","cwd":"/tmp/app","originator":"Codex Desktop","source":{"subagent":{"other":"guardian"}}}}"#,
                r#"{"timestamp":"2026-07-13T02:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":999,"output_tokens":999,"total_tokens":1998}}}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let records = codex_cost_records(home.path());
        assert_eq!(records.len(), 2);
        let record = records
            .iter()
            .find(|record| record.session_id == "codex-app-cost")
            .unwrap();
        assert_eq!(record.provider, "codex");
        assert_eq!(record.surface.as_deref(), Some("app"));
        assert_eq!(record.model, "gpt-5.4");
        assert_eq!(record.input_tokens, 300);
        assert_eq!(record.cached_input_tokens, 100);
        assert_eq!(record.output_tokens, 80);
        assert_eq!(record.reasoning_output_tokens, 32);
        assert_eq!(record.total_tokens, 380);
        // output_tokens already includes reasoning tokens, so reasoning is not added again.
        assert!((record.cost - 0.001725).abs() < 1e-10);
        assert!(record.cost_available);
        let cli = records
            .iter()
            .find(|record| record.session_id == "codex-cli-cost")
            .unwrap();
        assert_eq!(cli.surface.as_deref(), Some("cli"));
        assert_eq!(cli.total_tokens, 30);
        assert!((cli.cost - 0.000056625).abs() < 1e-10);
        assert!(cli.cost_available);

        let data = aggregate(&records);
        assert_eq!(data.total_tokens, 410);
        assert_eq!(data.unpriced_tokens, 0);
        assert!((data.total_cost - 0.001781625).abs() < 1e-10);
        assert_eq!(data.model_costs.len(), 2);
        assert!(data
            .model_costs
            .iter()
            .all(|model| model.provider == "codex" && model.cost_available));
    }

    #[test]
    fn test_cwd_shared_across_date_splits() {
        let dir = std::env::temp_dir().join("c9watch_test_cwd");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("test-cwd.jsonl");

        let content = [
            r#"{"type":"assistant","sessionId":"sess-3","timestamp":"2026-03-20T10:00:00Z","cwd":"/tmp/myproject","message":{"model":"claude-sonnet-4-6","role":"assistant","id":"m1","content":[],"usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            r#"{"type":"assistant","sessionId":"sess-3","timestamp":"2026-03-21T09:00:00Z","cwd":"","message":{"model":"claude-sonnet-4-6","role":"assistant","id":"m2","content":[],"usage":{"input_tokens":200,"output_tokens":100,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
        ].join("\n");
        std::fs::write(&file, &content).unwrap();

        let records = scan_file(&file);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(records.len(), 2);
        let mut records = records;
        records.sort_by(|a, b| a.date.cmp(&b.date));

        assert_eq!(records[0].project, "/tmp/myproject");
        assert_eq!(records[1].project, "/tmp/myproject");
        assert_eq!(records[0].project_name, "myproject");
        assert_eq!(records[1].project_name, "myproject");
    }

    #[test]
    fn test_aggregate_date_split() {
        let sessions = vec![
            SessionCostRecord {
                session_id: "s1".into(),
                project: "/tmp/a".into(),
                project_name: "a".into(),
                model: "claude-sonnet-4-6".into(),
                provider: default_provider(),
                surface: None,
                agent_kind: None,
                cost: 1.0,
                cost_available: true,
                input_tokens: 5000,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_output_tokens: 0,
                total_tokens: 5000,
                timestamp: "2026-03-20T10:00:00Z".into(),
                date: "2026-03-20".into(),
                session_name: String::new(),
            },
            SessionCostRecord {
                session_id: "s1".into(),
                project: "/tmp/a".into(),
                project_name: "a".into(),
                model: "claude-sonnet-4-6".into(),
                provider: default_provider(),
                surface: None,
                agent_kind: None,
                cost: 2.0,
                cost_available: true,
                input_tokens: 8000,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_output_tokens: 0,
                total_tokens: 8000,
                timestamp: "2026-03-21T09:00:00Z".into(),
                date: "2026-03-21".into(),
                session_name: String::new(),
            },
        ];
        let data = aggregate(&sessions);

        assert_eq!(data.daily_costs.len(), 2, "Should have 2 daily buckets");
        assert_eq!(data.total_cost, 3.0);
        assert_eq!(data.total_tokens, 13000);

        let day20 = data
            .daily_costs
            .iter()
            .find(|d| d.date == "2026-03-20")
            .unwrap();
        let day21 = data
            .daily_costs
            .iter()
            .find(|d| d.date == "2026-03-21")
            .unwrap();
        assert!((day20.cost - 1.0).abs() < 1e-10);
        assert!((day21.cost - 2.0).abs() < 1e-10);
    }
}
