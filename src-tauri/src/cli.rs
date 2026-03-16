use crate::session;
use crate::session::sanitize::strip_system_tags;
use clap::{Parser, Subcommand};
use std::process;

#[derive(Parser)]
#[command(
    name = "c9watch",
    about = "Monitor and manage Claude Code sessions",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Pretty-print JSON output
    #[arg(long, global = true)]
    pub pretty: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    /// List all active Claude Code sessions
    List {
        /// Filter by project path (substring match)
        #[arg(long)]
        project: Option<String>,
    },

    /// View conversation for a session
    View {
        /// Session ID (UUID or unique prefix)
        session_id: String,

        /// Show only the last N messages
        #[arg(short = 'n', long)]
        last: Option<usize>,
    },

    /// List past sessions from history
    History {
        /// Maximum number of entries to show
        #[arg(short = 'n', long)]
        limit: Option<usize>,
    },

    /// Search past sessions by query string
    Search {
        /// Search query
        query: String,
    },

    /// Stop a running Claude session
    Stop {
        /// PID of the session to stop
        pid: u32,
    },

    /// Watch sessions for status changes (streams NDJSON)
    Watch {
        /// Poll interval in seconds
        #[arg(short, long, default_value = "2")]
        interval: u64,

        /// Filter by project path (substring match)
        #[arg(long)]
        project: Option<String>,
    },

    /// Show tasks/todos for a session
    Tasks {
        /// Session ID (UUID or unique prefix)
        session_id: String,
    },
}

/// Run the CLI and return the exit code.
/// Returns Ok(()) on success, Err on failure.
pub fn run(cli: Cli) {
    // Suppress debug log stderr noise — agents shouldn't see warnings
    crate::debug_log::set_quiet(true);

    let result = match cli.command {
        Commands::List { project } => cmd_list(project.as_deref(), cli.pretty),
        Commands::View { session_id, last } => cmd_view(&session_id, last, cli.pretty),
        Commands::History { limit } => cmd_history(limit, cli.pretty),
        Commands::Search { query } => cmd_search(&query, cli.pretty),
        Commands::Stop { pid } => cmd_stop(pid, cli.pretty),
        Commands::Watch { interval, project } => cmd_watch(interval, project.as_deref()),
        Commands::Tasks { session_id } => cmd_tasks(&session_id, cli.pretty),
    };

    if let Err(e) = result {
        let err = serde_json::json!({ "error": e });
        eprintln!("{}", serde_json::to_string(&err).unwrap_or(e));
        process::exit(1);
    }
}

fn print_json(value: &impl serde::Serialize, pretty: bool) -> Result<(), String> {
    let output = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .map_err(|e| format!("Failed to serialize output: {}", e))?;
    println!("{}", output);
    Ok(())
}

fn cmd_list(project_filter: Option<&str>, pretty: bool) -> Result<(), String> {
    let (sessions, diagnostics) = session::enrichment::detect_and_enrich_sessions()?;

    // Sanitize text fields for agent consumption
    let sessions: Vec<serde_json::Value> = sessions
        .into_iter()
        .filter(|s| {
            if let Some(filter) = project_filter {
                s.project_path.contains(filter)
            } else {
                true
            }
        })
        .map(|s| sanitize_session(s))
        .collect();

    let output = serde_json::json!({
        "sessions": sessions,
        "diagnostics": diagnostics,
    });
    print_json(&output, pretty)
}

fn cmd_view(session_id: &str, last: Option<usize>, pretty: bool) -> Result<(), String> {
    // Resolve prefix by scanning JSONL filenames directly (no process detection)
    let resolved_id = resolve_session_id_lightweight(session_id)?;
    let mut conversation = session::conversation::get_conversation_data(&resolved_id)?;

    if let Some(n) = last {
        let len = conversation.messages.len();
        if n < len {
            conversation.messages = conversation.messages.split_off(len - n);
        }
    }

    // Sanitize message content
    for msg in &mut conversation.messages {
        msg.content = strip_system_tags(&msg.content);
    }

    print_json(&conversation, pretty)
}

fn cmd_history(limit: Option<usize>, pretty: bool) -> Result<(), String> {
    let entries = session::get_history()?;

    // Enrich history entries with firstPrompt from JSONL and readable date
    let enriched: Vec<serde_json::Value> = entries
        .into_iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(|entry| enrich_history_entry(entry))
        .collect();

    print_json(&enriched, pretty)
}

fn cmd_search(query: &str, pretty: bool) -> Result<(), String> {
    if query.trim().is_empty() {
        return Err("Search query cannot be empty".to_string());
    }
    let hits = session::deep_search(query)?;

    // Enrich search hits with project path and timestamp
    let enriched: Vec<serde_json::Value> = hits
        .into_iter()
        .map(|hit| enrich_search_hit(hit))
        .collect();

    let output = serde_json::json!({
        "query": query,
        "hits": enriched,
    });
    print_json(&output, pretty)
}

fn cmd_stop(pid: u32, pretty: bool) -> Result<(), String> {
    crate::actions::stop_session(pid)?;
    let output = serde_json::json!({
        "stopped": true,
        "pid": pid,
    });
    print_json(&output, pretty)
}

/// Watch sessions and emit NDJSON events on status changes.
/// Each line is a JSON object: {"event": "...", "session": {...}, "timestamp": "..."}
/// Events: "started", "status_changed", "stopped"
fn cmd_watch(interval_secs: u64, project_filter: Option<&str>) -> Result<(), String> {
    use std::collections::HashMap;
    use std::io::Write;

    let interval = std::time::Duration::from_secs(interval_secs);

    // Track previous state: session_id -> (status, pending_tool_name)
    let mut prev_state: HashMap<String, (String, Option<String>)> = HashMap::new();

    loop {
        let (sessions, _) = match session::enrichment::detect_and_enrich_sessions() {
            Ok(result) => result,
            Err(_) => {
                std::thread::sleep(interval);
                continue;
            }
        };

        let mut current_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for s in &sessions {
            // Apply project filter
            if let Some(filter) = project_filter {
                if !s.project_path.contains(filter) {
                    continue;
                }
            }

            current_ids.insert(s.id.clone());

            let status_str = serde_json::to_string(&s.status).unwrap_or_default();
            let prev = prev_state.get(&s.id);

            let event = match prev {
                None => Some("started"),
                Some((old_status, old_tool)) => {
                    if *old_status != status_str
                        || *old_tool != s.pending_tool_name
                    {
                        Some("status_changed")
                    } else {
                        None
                    }
                }
            };

            if let Some(event_name) = event {
                let sanitized = sanitize_session(s.clone());
                let line = serde_json::json!({
                    "event": event_name,
                    "session": sanitized,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                println!("{}", serde_json::to_string(&line).unwrap_or_default());
                // Flush immediately so piping agents see events in real-time
                let _ = std::io::stdout().flush();
            }

            prev_state.insert(
                s.id.clone(),
                (status_str, s.pending_tool_name.clone()),
            );
        }

        // Detect stopped sessions
        let stopped: Vec<String> = prev_state
            .keys()
            .filter(|id| !current_ids.contains(*id))
            .cloned()
            .collect();

        for id in &stopped {
            let line = serde_json::json!({
                "event": "stopped",
                "sessionId": id,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            });
            println!("{}", serde_json::to_string(&line).unwrap_or_default());
            let _ = std::io::stdout().flush();
            prev_state.remove(id);
        }

        std::thread::sleep(interval);
    }
}

/// Show tasks/todos for a session from ~/.claude/tasks/<session-id>/
fn cmd_tasks(session_id: &str, pretty: bool) -> Result<(), String> {
    let resolved_id = resolve_session_id_lightweight(session_id)?;
    let tasks = read_session_tasks(&resolved_id)?;

    let output = serde_json::json!({
        "sessionId": resolved_id,
        "tasks": tasks,
        "total": tasks.len(),
        "completed": tasks.iter().filter(|t| t.get("status").and_then(|s| s.as_str()) == Some("completed")).count(),
        "inProgress": tasks.iter().filter(|t| t.get("status").and_then(|s| s.as_str()) == Some("in_progress")).count(),
        "pending": tasks.iter().filter(|t| {
            let status = t.get("status").and_then(|s| s.as_str()).unwrap_or("");
            status != "completed" && status != "in_progress"
        }).count(),
    });
    print_json(&output, pretty)
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Sanitize a Session's text fields for agent output.
/// Re-reads full first prompt from JSONL to avoid sanitizing truncated tags.
fn sanitize_session(s: session::enrichment::Session) -> serde_json::Value {
    // Re-read full first prompt so we can sanitize BEFORE truncation.
    // Without this, truncation may cut mid-tag leaving unsanitizable fragments.
    let first_prompt = find_first_prompt_raw_for_session(&s.id)
        .map(|full| {
            let clean = strip_system_tags(&full);
            session::enrichment::truncate_string(&clean, 100)
        })
        .unwrap_or_else(|| strip_system_tags(&s.first_prompt));

    // Include task summary if available
    let task_summary = get_task_summary(&s.id);

    let mut json = serde_json::json!({
        "id": s.id,
        "pid": s.pid,
        "sessionName": s.session_name,
        "customTitle": s.custom_title,
        "projectPath": s.project_path,
        "gitBranch": s.git_branch,
        "firstPrompt": first_prompt,
        "summary": s.summary,
        "messageCount": s.message_count,
        "modified": s.modified,
        "status": s.status,
        "latestMessage": strip_system_tags(&s.latest_message),
        "pendingToolName": s.pending_tool_name,
    });

    // Add pendingToolInput only when there's a pending tool
    if let Some(input) = s.pending_tool_input {
        json.as_object_mut()
            .unwrap()
            .insert("pendingToolInput".to_string(), input);
    }

    // Add task summary if there are any tasks
    if let Some(summary) = task_summary {
        json.as_object_mut()
            .unwrap()
            .insert("taskProgress".to_string(), summary);
    }

    json
}

/// Enrich a history entry with firstPrompt from the JSONL file and ISO date.
fn enrich_history_entry(entry: session::HistoryEntry) -> serde_json::Value {
    // Try to get the real first prompt from the JSONL file (full text, sanitize before truncation)
    let first_prompt = find_first_prompt_raw_for_session(&entry.session_id)
        .map(|full| {
            let clean = strip_system_tags(&full);
            session::enrichment::truncate_string(&clean, 100)
        })
        .unwrap_or_else(|| strip_system_tags(&entry.display));

    // Convert timestamp to ISO 8601
    let date = chrono::DateTime::from_timestamp_millis(entry.timestamp as i64)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();

    serde_json::json!({
        "sessionId": entry.session_id,
        "firstPrompt": first_prompt,
        "display": strip_system_tags(&entry.display),
        "date": date,
        "timestamp": entry.timestamp,
        "project": entry.project,
        "projectName": entry.project_name,
        "customTitle": entry.custom_title,
    })
}

/// Enrich a search hit with project path and timestamp from the JSONL file.
fn enrich_search_hit(hit: session::DeepSearchHit) -> serde_json::Value {
    let (project_path, modified) = find_session_metadata(&hit.session_id);

    serde_json::json!({
        "sessionId": hit.session_id,
        "snippet": strip_system_tags(&hit.snippet),
        "projectPath": project_path,
        "modified": modified,
    })
}

/// Find the first user prompt (full, unsanitized) from a session's JSONL file
/// by searching all project directories. Avoids running process detection.
fn find_first_prompt_raw_for_session(session_id: &str) -> Option<String> {
    let home_dir = dirs::home_dir()?;
    let projects_dir = home_dir.join(".claude").join("projects");
    let session_filename = format!("{}.jsonl", session_id);

    for entry in std::fs::read_dir(&projects_dir).ok()?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let session_file = path.join(&session_filename);
        if session_file.exists() {
            return session::enrichment::get_first_prompt_from_jsonl_raw(&session_file);
        }
    }
    None
}

/// Find project path and modification time for a session JSONL file.
fn find_session_metadata(session_id: &str) -> (Option<String>, Option<String>) {
    let home_dir = match dirs::home_dir() {
        Some(h) => h,
        None => return (None, None),
    };
    let projects_dir = home_dir.join(".claude").join("projects");
    let session_filename = format!("{}.jsonl", session_id);

    for entry in std::fs::read_dir(&projects_dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
    {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        let session_file = project_dir.join(&session_filename);
        if session_file.exists() {
            // Derive project path from the encoded directory name
            let project_path = project_dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string());

            let modified = std::fs::metadata(&session_file)
                .and_then(|m| m.modified())
                .ok()
                .map(|t| {
                    let datetime: chrono::DateTime<chrono::Utc> = t.into();
                    datetime.to_rfc3339()
                });

            return (project_path, modified);
        }
    }
    (None, None)
}

/// Read tasks from ~/.claude/tasks/<session-id>/*.json
fn read_session_tasks(session_id: &str) -> Result<Vec<serde_json::Value>, String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    let tasks_dir = home_dir.join(".claude").join("tasks").join(session_id);

    if !tasks_dir.exists() {
        return Ok(vec![]);
    }

    let mut tasks: Vec<serde_json::Value> = Vec::new();

    for entry in std::fs::read_dir(&tasks_dir)
        .map_err(|e| format!("Failed to read tasks dir: {}", e))?
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(task) = serde_json::from_str::<serde_json::Value>(&content) {
                tasks.push(task);
            }
        }
    }

    // Sort by task ID numerically
    tasks.sort_by(|a, b| {
        let id_a = a
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let id_b = b
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        id_a.cmp(&id_b)
    });

    Ok(tasks)
}

/// Get a compact task progress summary for inclusion in session listing.
/// Returns something like "3/7 completed, 1 in_progress" or None if no tasks.
fn get_task_summary(session_id: &str) -> Option<serde_json::Value> {
    let tasks = read_session_tasks(session_id).ok()?;
    if tasks.is_empty() {
        return None;
    }

    let total = tasks.len();
    let completed = tasks
        .iter()
        .filter(|t| t.get("status").and_then(|s| s.as_str()) == Some("completed"))
        .count();
    let in_progress = tasks
        .iter()
        .filter(|t| t.get("status").and_then(|s| s.as_str()) == Some("in_progress"))
        .count();

    // Find the current task (first in_progress)
    let current_task = tasks.iter().find_map(|t| {
        if t.get("status").and_then(|s| s.as_str()) == Some("in_progress") {
            t.get("subject").and_then(|s| s.as_str()).map(String::from)
        } else {
            None
        }
    });

    Some(serde_json::json!({
        "total": total,
        "completed": completed,
        "inProgress": in_progress,
        "currentTask": current_task,
    }))
}

/// Resolve a session ID prefix by scanning JSONL filenames directly.
/// Much lighter than detect_and_enrich_sessions() — no process detection needed.
fn resolve_session_id_lightweight(prefix: &str) -> Result<String, String> {
    // Full UUID — use directly
    if prefix.len() >= 36 {
        return Ok(prefix.to_string());
    }

    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    let projects_dir = home_dir.join(".claude").join("projects");

    let mut matches: Vec<String> = Vec::new();

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
                            if stem.starts_with(prefix) && !stem.starts_with("agent-") {
                                let id = stem.to_string();
                                if !matches.contains(&id) {
                                    matches.push(id);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    match matches.len() {
        0 => Ok(prefix.to_string()), // Fall through — might be a full ID
        1 => Ok(matches.into_iter().next().unwrap()),
        n => Err(format!(
            "Ambiguous session ID prefix '{}' matches {} sessions",
            prefix, n
        )),
    }
}
