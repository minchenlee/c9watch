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

        /// Filter by session status (e.g. Working, WaitingForInput, NeedsPermission)
        #[arg(long)]
        status: Option<String>,

        /// Compact output: only id, status, projectPath, pid, pendingToolName
        #[arg(long)]
        compact: bool,
    },

    /// Aggregate status summary of all active sessions
    Status {
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

        /// Filter by project path (substring match)
        #[arg(long)]
        project: Option<String>,

        /// Maximum number of results to return
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
    },

    /// Identify this session (find the calling agent's own session by PID ancestry)
    #[command(name = "self")]
    SelfId,

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

        /// Compact output: only id, status, pendingToolName per event
        #[arg(long)]
        compact: bool,

        /// Skip initial "started" burst, only emit changes and stops
        #[arg(long)]
        changes_only: bool,
    },

    /// Show tasks/todos for a session
    Tasks {
        /// Session ID (UUID or unique prefix)
        session_id: String,
    },
}

/// Run the CLI and return the exit code.
pub fn run(cli: Cli) {
    // Suppress debug log stderr noise — agents shouldn't see warnings
    crate::debug_log::set_quiet(true);

    let result = match cli.command {
        Commands::List { project, status, compact } => {
            cmd_list(project.as_deref(), status.as_deref(), compact, cli.pretty)
        }
        Commands::Status { project } => cmd_status(project.as_deref(), cli.pretty),
        Commands::View { session_id, last } => cmd_view(&session_id, last, cli.pretty),
        Commands::History { limit } => cmd_history(limit, cli.pretty),
        Commands::Search {
            query,
            project,
            limit,
        } => cmd_search(&query, project.as_deref(), limit, cli.pretty),
        Commands::SelfId => cmd_self(cli.pretty),
        Commands::Stop { pid } => cmd_stop(pid, cli.pretty),
        Commands::Watch {
            interval,
            project,
            compact,
            changes_only,
        } => cmd_watch(interval, project.as_deref(), compact, changes_only),
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

// ── Commands ────────────────────────────────────────────────────────

fn cmd_list(
    project_filter: Option<&str>,
    status_filter: Option<&str>,
    compact: bool,
    pretty: bool,
) -> Result<(), String> {
    let (sessions, diagnostics) = session::enrichment::detect_and_enrich_sessions()?;

    let sessions: Vec<serde_json::Value> = sessions
        .into_iter()
        .filter(|s| match project_filter {
            Some(filter) => s.project_path.contains(filter),
            None => true,
        })
        .filter(|s| match status_filter {
            Some(filter) => {
                let status_str = serde_json::to_string(&s.status).unwrap_or_default();
                let status_str = status_str.trim_matches('"');
                status_str.eq_ignore_ascii_case(filter)
            }
            None => true,
        })
        .map(|s| {
            if compact {
                compact_session(&s)
            } else {
                full_session(s)
            }
        })
        .collect();

    let output = serde_json::json!({
        "sessions": sessions,
        "diagnostics": diagnostics,
    });
    print_json(&output, pretty)
}

fn cmd_status(project_filter: Option<&str>, pretty: bool) -> Result<(), String> {
    let (sessions, _) = session::enrichment::detect_and_enrich_sessions()?;

    let filtered: Vec<_> = sessions
        .iter()
        .filter(|s| match project_filter {
            Some(filter) => s.project_path.contains(filter),
            None => true,
        })
        .collect();

    let total = filtered.len();
    let mut by_status = std::collections::HashMap::new();
    let mut by_project = std::collections::HashMap::new();

    for s in &filtered {
        let status_str = serde_json::to_string(&s.status)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();
        *by_status.entry(status_str).or_insert(0u32) += 1;
        *by_project
            .entry(s.session_name.clone())
            .or_insert(0u32) += 1;
    }

    // Find sessions needing attention
    let needs_permission: Vec<serde_json::Value> = filtered
        .iter()
        .filter(|s| s.status == session::SessionStatus::NeedsPermission)
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "sessionName": s.session_name,
                "pendingToolName": s.pending_tool_name,
            })
        })
        .collect();

    let output = serde_json::json!({
        "total": total,
        "byStatus": by_status,
        "byProject": by_project,
        "needsPermission": needs_permission,
    });
    print_json(&output, pretty)
}

fn cmd_view(session_id: &str, last: Option<usize>, pretty: bool) -> Result<(), String> {
    let resolved_id = resolve_session_id_lightweight(session_id)?;
    let mut conversation = session::conversation::get_conversation_data(&resolved_id)?;

    if let Some(n) = last {
        let len = conversation.messages.len();
        if n < len {
            conversation.messages = conversation.messages.split_off(len - n);
        }
    }

    for msg in &mut conversation.messages {
        msg.content = strip_system_tags(&msg.content);
    }

    print_json(&conversation, pretty)
}

fn cmd_history(limit: Option<usize>, pretty: bool) -> Result<(), String> {
    let entries = session::get_history()?;

    let enriched: Vec<serde_json::Value> = entries
        .into_iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(|entry| enrich_history_entry(entry))
        .collect();

    print_json(&enriched, pretty)
}

fn cmd_search(
    query: &str,
    project_filter: Option<&str>,
    limit: usize,
    pretty: bool,
) -> Result<(), String> {
    if query.trim().is_empty() {
        return Err("Search query cannot be empty".to_string());
    }
    let hits = session::deep_search(query)?;

    let enriched: Vec<serde_json::Value> = hits
        .into_iter()
        .map(|hit| enrich_search_hit(hit))
        .filter(|hit| match project_filter {
            Some(filter) => hit
                .get("projectPath")
                .and_then(|p| p.as_str())
                .map(|p| p.contains(filter))
                .unwrap_or(false),
            None => true,
        })
        .take(limit)
        .collect();

    let output = serde_json::json!({
        "query": query,
        "hits": enriched,
        "truncated": enriched.len() == limit,
    });
    print_json(&output, pretty)
}

/// Identify the calling agent's own session by walking up the PID tree
/// to find a parent claude process, then matching it to an active session.
fn cmd_self(pretty: bool) -> Result<(), String> {
    let my_pid = std::process::id();

    // Walk up the process tree to find a claude ancestor
    let claude_pid = find_claude_ancestor(my_pid)
        .ok_or("No claude parent process found — this command must be called from within a Claude Code session")?;

    // Find the matching session
    let (sessions, _) = session::enrichment::detect_and_enrich_sessions()?;
    let session = sessions
        .into_iter()
        .find(|s| s.pid == claude_pid)
        .ok_or(format!(
            "Found claude process (PID {}) but no matching session",
            claude_pid
        ))?;

    let output = full_session(session);
    print_json(&output, pretty)
}

/// Walk up the process tree from `pid` to find the nearest ancestor that is a claude process.
fn find_claude_ancestor(start_pid: u32) -> Option<u32> {
    use sysinfo::System;

    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let mut current_pid = sysinfo::Pid::from_u32(start_pid);

    // Walk up at most 20 levels to avoid infinite loops
    for _ in 0..20 {
        let process = sys.process(current_pid)?;
        let parent_pid = process.parent()?;
        let parent = sys.process(parent_pid)?;

        let exe_name = parent
            .exe()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");

        // Check if parent is a claude process
        if exe_name == "claude" || exe_name.starts_with("claude-") {
            return Some(parent_pid.as_u32());
        }

        // Also check command line for "claude" (handles node-based claude)
        let cmd = parent.cmd();
        if cmd.iter().any(|arg| {
            let s = arg.to_string_lossy();
            s.ends_with("/claude") || s.ends_with("\\claude") || s == "claude"
        }) {
            return Some(parent_pid.as_u32());
        }

        current_pid = parent_pid;
    }

    None
}

fn cmd_stop(pid: u32, pretty: bool) -> Result<(), String> {
    crate::actions::stop_session(pid)?;
    let output = serde_json::json!({
        "stopped": true,
        "pid": pid,
    });
    print_json(&output, pretty)
}

fn cmd_watch(
    interval_secs: u64,
    project_filter: Option<&str>,
    compact: bool,
    changes_only: bool,
) -> Result<(), String> {
    use std::collections::HashMap;
    use std::io::Write;

    let interval = std::time::Duration::from_secs(interval_secs);
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
            if let Some(filter) = project_filter {
                if !s.project_path.contains(filter) {
                    continue;
                }
            }

            current_ids.insert(s.id.clone());

            let status_str = serde_json::to_string(&s.status).unwrap_or_default();
            let prev = prev_state.get(&s.id);

            let event = match prev {
                None => {
                    if changes_only {
                        None // Skip initial "started" burst
                    } else {
                        Some("started")
                    }
                }
                Some((old_status, old_tool)) => {
                    if *old_status != status_str || *old_tool != s.pending_tool_name {
                        Some("status_changed")
                    } else {
                        None
                    }
                }
            };

            if let Some(event_name) = event {
                let session_data = if compact {
                    compact_session(s)
                } else {
                    full_session(s.clone())
                };
                let line = serde_json::json!({
                    "event": event_name,
                    "sessionId": s.id,
                    "session": session_data,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                println!("{}", serde_json::to_string(&line).unwrap_or_default());
                let _ = std::io::stdout().flush();
            }

            prev_state.insert(s.id.clone(), (status_str, s.pending_tool_name.clone()));
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

// ── Session formatters ──────────────────────────────────────────────

/// Compact session: minimal fields for status polling.
fn compact_session(s: &session::enrichment::Session) -> serde_json::Value {
    let mut json = serde_json::json!({
        "id": s.id,
        "pid": s.pid,
        "status": s.status,
        "projectPath": s.project_path,
        "sessionName": s.session_name,
    });

    // Only include non-null optional fields
    if let Some(ref tool) = s.pending_tool_name {
        json.as_object_mut()
            .unwrap()
            .insert("pendingToolName".to_string(), serde_json::json!(tool));
    }

    json
}

/// Full session: all fields, sanitized, with null fields omitted.
fn full_session(s: session::enrichment::Session) -> serde_json::Value {
    // Re-read full first prompt so we can sanitize BEFORE truncation.
    let first_prompt = find_first_prompt_raw_for_session(&s.id)
        .map(|full| {
            let clean = strip_system_tags(&full);
            let trimmed = clean.trim().to_string();
            if trimmed.is_empty() {
                // First prompt was entirely a system tag — return empty
                String::new()
            } else {
                session::enrichment::truncate_string(&trimmed, 100)
            }
        })
        .unwrap_or_else(|| {
            let cleaned = strip_system_tags(&s.first_prompt);
            cleaned.trim().to_string()
        });

    let latest_message = strip_system_tags(&s.latest_message);

    let mut json = serde_json::json!({
        "id": s.id,
        "pid": s.pid,
        "sessionName": s.session_name,
        "projectPath": s.project_path,
        "firstPrompt": first_prompt,
        "messageCount": s.message_count,
        "modified": s.modified,
        "status": s.status,
    });

    let obj = json.as_object_mut().unwrap();

    // Only include non-null/non-empty optional fields to save tokens
    if let Some(ref title) = s.custom_title {
        obj.insert("customTitle".to_string(), serde_json::json!(title));
    }
    if let Some(ref branch) = s.git_branch {
        obj.insert("gitBranch".to_string(), serde_json::json!(branch));
    }
    if let Some(ref summary) = s.summary {
        obj.insert("summary".to_string(), serde_json::json!(summary));
    }
    if !latest_message.trim().is_empty() {
        obj.insert(
            "latestMessage".to_string(),
            serde_json::json!(latest_message),
        );
    }
    if let Some(ref tool) = s.pending_tool_name {
        obj.insert("pendingToolName".to_string(), serde_json::json!(tool));
    }
    if let Some(input) = s.pending_tool_input {
        obj.insert("pendingToolInput".to_string(), input);
    }
    if let Some(summary) = get_task_summary(&s.id) {
        obj.insert("taskProgress".to_string(), summary);
    }

    json
}

// ── Helpers ─────────────────────────────────────────────────────────

fn enrich_history_entry(entry: session::HistoryEntry) -> serde_json::Value {
    let first_prompt = find_first_prompt_raw_for_session(&entry.session_id)
        .map(|full| {
            let clean = strip_system_tags(&full);
            let trimmed = clean.trim().to_string();
            if trimmed.is_empty() {
                strip_system_tags(&entry.display)
            } else {
                session::enrichment::truncate_string(&trimmed, 100)
            }
        })
        .unwrap_or_else(|| strip_system_tags(&entry.display));

    let date = chrono::DateTime::from_timestamp_millis(entry.timestamp as i64)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();

    let mut json = serde_json::json!({
        "sessionId": entry.session_id,
        "firstPrompt": first_prompt,
        "display": strip_system_tags(&entry.display),
        "date": date,
        "timestamp": entry.timestamp,
        "project": entry.project,
        "projectName": entry.project_name,
    });

    if let Some(title) = entry.custom_title {
        json.as_object_mut()
            .unwrap()
            .insert("customTitle".to_string(), serde_json::json!(title));
    }

    json
}

fn enrich_search_hit(hit: session::DeepSearchHit) -> serde_json::Value {
    let (project_path_encoded, modified) = find_session_metadata(&hit.session_id);

    // Decode the encoded project path back to a real filesystem path
    // e.g. "-Users-liminchen-Documents-GitHub-c9watch" -> "/Users/liminchen/Documents/GitHub/c9watch"
    let project_path = project_path_encoded.map(|encoded| decode_project_path(&encoded));

    serde_json::json!({
        "sessionId": hit.session_id,
        "snippet": strip_system_tags(&hit.snippet),
        "projectPath": project_path,
        "modified": modified,
    })
}

/// Decode Claude Code's encoded project directory name back to a real path.
/// Claude Code encodes paths by replacing every non-alphanumeric char with `-`.
///
/// Strategy: try sessions-index.json first (authoritative), then use the active
/// session's CWD from enrichment data, then fall back to heuristic decoding.
fn decode_project_path(encoded: &str) -> String {
    // 1. Try sessions-index.json
    if let Some(home_dir) = dirs::home_dir() {
        let project_dir = home_dir.join(".claude").join("projects").join(encoded);
        let index_path = project_dir.join("sessions-index.json");
        if let Ok(index) = session::parse_sessions_index(&index_path) {
            if let Some(entry) = index.entries.first() {
                return entry.project_path.to_string_lossy().to_string();
            }
        }
    }

    // 2. Heuristic: the encoded form is a path with non-alphanumeric chars replaced by `-`.
    //    We can't perfectly reverse it, but common patterns work:
    //    "-Users-liminchen-Documents-GitHub-c9watch" -> "/Users/liminchen/Documents/GitHub/c9watch"
    //    This works because most path components are alphanumeric.
    if encoded.starts_with('-') {
        let decoded = encoded.replacen('-', "/", 1); // First `-` -> `/`
        // Replace remaining `-` with `/`, but this is lossy for hyphenated dirs
        return decoded.replace('-', "/");
    }

    encoded.to_string()
}

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

fn resolve_session_id_lightweight(prefix: &str) -> Result<String, String> {
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
        0 => {
            // If it looks like a full UUID, let it through (might be a valid ID
            // in a project dir we couldn't enumerate). Otherwise, fail clearly.
            if prefix.len() >= 32 {
                Ok(prefix.to_string())
            } else {
                Err(format!(
                    "No session found matching prefix '{}'",
                    prefix
                ))
            }
        }
        1 => Ok(matches.into_iter().next().unwrap()),
        n => Err(format!(
            "Ambiguous session ID prefix '{}' matches {} sessions",
            prefix, n
        )),
    }
}
