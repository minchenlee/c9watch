use crate::session;
use crate::session::sanitize::strip_system_tags;
use clap::{Parser, Subcommand, ValueEnum};
use std::process;

/// Sort field for the cost subcommand.
#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
pub enum CostSortField {
    Date,
    Cost,
}

/// Sort direction for the cost subcommand.
#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
pub enum CostSortDir {
    Asc,
    Desc,
}

// PM/worker orchestration modules — opt-in only. Compiled out of default builds.
#[cfg(feature = "pm-orchestration")]
pub mod adoption;
#[cfg(feature = "pm-orchestration")]
pub mod bg_backend;
#[cfg(feature = "pm-orchestration")]
pub mod bg_protocol;
#[cfg(feature = "pm-orchestration")]
pub mod pm;
#[cfg(feature = "pm-orchestration")]
pub mod pm_caller;
#[cfg(feature = "pm-orchestration")]
pub mod pm_daemon;
#[cfg(feature = "pm-orchestration")]
pub mod pm_fs;
#[cfg(feature = "pm-orchestration")]
pub mod pm_inbox;
#[cfg(feature = "pm-orchestration")]
pub mod pm_rpc;
#[cfg(feature = "pm-orchestration")]
pub mod pm_worker;
#[cfg(feature = "pm-orchestration")]
pub mod worker_backend;

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

        /// Match case exactly (default: case-insensitive)
        #[arg(long)]
        case_sensitive: bool,

        /// Require the query to match at word boundaries
        #[arg(long)]
        whole_word: bool,
    },

    /// Identify this session (find the calling agent's own session by PID ancestry)
    #[command(name = "self")]
    SelfId,

    /// Stop a session. Worker lookup wins over PID parsing: if the target
    /// matches a live PM worker's session id (exact or unique prefix), the
    /// worker is stopped. Otherwise, a numeric target is treated as a PID
    /// and the corresponding Claude process is killed.
    #[cfg(feature = "pm-orchestration")]
    Stop {
        /// Worker session id/prefix (preferred) or numeric PID for regular sessions
        target: String,
    },

    /// Stop a session by PID (kills the corresponding Claude process).
    #[cfg(not(feature = "pm-orchestration"))]
    Stop {
        /// Numeric PID of the session's Claude process
        target: String,
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

    /// Spawn a new worker session managed by c9watch
    #[cfg(feature = "pm-orchestration")]
    Spawn {
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        append_system_prompt: Option<String>,
        #[arg(long)]
        append_system_prompt_file: Option<String>,
        #[arg(long, default_value = "bypassPermissions")]
        permission_mode: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        add_dir: Vec<String>,
        /// Initial prompt sent at spawn time. REQUIRED for bg backend (CC >= 2.1.150).
        /// PrintBackend ignores this — prompts come via `c9watch send` after spawn.
        #[arg(long)]
        prompt: Option<String>,
    },

    /// Send a message to a spawned worker session
    #[cfg(feature = "pm-orchestration")]
    Send {
        session_id: String,
        #[arg(long)]
        message: Option<String>,
        #[arg(long)]
        stdin: bool,
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        wait: bool,
        #[arg(long, default_value = "300")]
        timeout: u64,
    },

    /// List workers spawned by c9watch
    #[cfg(feature = "pm-orchestration")]
    Workers {
        /// Include workers owned by other PMs (adds a status column per entry)
        #[arg(long)]
        all: bool,
    },

    /// List callback events from workers owned by this PM session
    #[cfg(feature = "pm-orchestration")]
    Inbox {
        /// Remove events after listing
        #[arg(long)]
        consume: bool,
        /// Remove all events without printing them
        #[arg(long)]
        clear: bool,
        /// Only touch this worker's inbox (must be owned by caller)
        #[arg(long)]
        worker: Option<String>,
    },

    /// Adopt an existing worker as owned by this PM session
    #[cfg(feature = "pm-orchestration")]
    Adopt {
        /// Worker session id or unique prefix
        session_id: String,
        /// Force adoption even if worker is OWNED_BY_OTHER_PM
        #[arg(long)]
        force: bool,
    },

    /// Query session cost data
    #[command(name = "cost", about = "Query session cost data")]
    Cost {
        /// Aggregate by day
        #[arg(long)]
        daily: bool,
        /// Aggregate by ISO week (Monday start)
        #[arg(long)]
        weekly: bool,
        /// Filter to a single project (matches the `project` field)
        #[arg(long)]
        project: Option<String>,
        /// Lower bound on date (YYYY-MM-DD, inclusive)
        #[arg(long)]
        since: Option<String>,
        /// Sort field: date or cost
        #[arg(long, value_enum, default_value_t = CostSortField::Date, help = "Sort rows by this field (date or cost)")]
        sort: CostSortField,
        /// Sort direction: asc or desc
        #[arg(long, value_enum, default_value_t = CostSortDir::Desc, help = "Sort direction (asc = oldest/cheapest first, desc = newest/most expensive first)")]
        sort_dir: CostSortDir,
        /// Show cost breakdown for a single session by full id or an 8+ char prefix. Use --session-prefix for shorter prefixes. Incompatible with --daily/--weekly/--project.
        #[arg(long, conflicts_with_all = ["daily", "weekly", "project", "session_prefix"])]
        session: Option<String>,
        /// Show cost breakdown for a single session by id prefix (any length, must be unique). Incompatible with --daily/--weekly/--project.
        #[arg(long, conflicts_with_all = ["daily", "weekly", "project", "session"])]
        session_prefix: Option<String>,
    },

    /// [hidden] Run the PM daemon
    #[cfg(feature = "pm-orchestration")]
    #[command(hide = true)]
    Daemon,
}

/// Run the CLI and return the exit code.
pub fn run(cli: Cli) {
    // Suppress debug log stderr noise — agents shouldn't see warnings
    crate::debug_log::set_quiet(true);

    let result = match cli.command {
        Commands::List {
            project,
            status,
            compact,
        } => cmd_list(project.as_deref(), status.as_deref(), compact, cli.pretty),
        Commands::Status { project } => cmd_status(project.as_deref(), cli.pretty),
        Commands::View { session_id, last } => cmd_view(&session_id, last, cli.pretty),
        Commands::History { limit } => cmd_history(limit, cli.pretty),
        Commands::Search {
            query,
            project,
            limit,
            case_sensitive,
            whole_word,
        } => cmd_search(
            &query,
            project.as_deref(),
            limit,
            case_sensitive,
            whole_word,
            cli.pretty,
        ),
        Commands::SelfId => cmd_self(cli.pretty),
        Commands::Stop { target } => cmd_stop(&target, cli.pretty),
        Commands::Watch {
            interval,
            project,
            compact,
            changes_only,
        } => cmd_watch(interval, project.as_deref(), compact, changes_only),
        Commands::Tasks { session_id } => cmd_tasks(&session_id, cli.pretty),
        #[cfg(feature = "pm-orchestration")]
        Commands::Spawn {
            cwd,
            name,
            append_system_prompt,
            append_system_prompt_file,
            permission_mode,
            model,
            add_dir,
            prompt,
        } => (|| -> Result<(), String> {
            let resolved_cwd = match cwd {
                Some(c) => c,
                None => std::env::current_dir()
                    .map_err(|e| format!("Failed to get current directory: {}", e))?
                    .to_string_lossy()
                    .to_string(),
            };
            let args = pm_worker::SpawnArgs {
                cwd: resolved_cwd,
                name,
                append_system_prompt,
                permission_mode,
                model,
                add_dirs: add_dir,
            };
            pm::cmd_spawn(args, append_system_prompt_file, prompt, cli.pretty)
        })(),
        #[cfg(feature = "pm-orchestration")]
        Commands::Send {
            session_id,
            message,
            stdin,
            file,
            wait,
            timeout,
        } => pm::cmd_send(session_id, message, stdin, file, wait, timeout, cli.pretty),
        #[cfg(feature = "pm-orchestration")]
        Commands::Workers { all } => pm::cmd_workers(all, cli.pretty),
        #[cfg(feature = "pm-orchestration")]
        Commands::Inbox {
            consume,
            clear,
            worker,
        } => pm::cmd_inbox(consume, clear, worker, cli.pretty),
        #[cfg(feature = "pm-orchestration")]
        Commands::Adopt { session_id, force } => pm::cmd_adopt(session_id, force, cli.pretty),
        Commands::Cost {
            daily,
            weekly,
            project,
            since,
            sort,
            sort_dir,
            session,
            session_prefix,
        } => cmd_cost(
            daily,
            weekly,
            project.as_deref(),
            since.as_deref(),
            sort,
            sort_dir,
            session.as_deref(),
            session_prefix.as_deref(),
            cli.pretty,
        ),
        #[cfg(feature = "pm-orchestration")]
        Commands::Daemon => match tokio::runtime::Runtime::new() {
            Ok(rt) => rt.block_on(pm_daemon::run_daemon()),
            Err(e) => Err(format!("Failed to create tokio runtime: {}", e)),
        },
    };

    if let Err(e) = result {
        let err = serde_json::json!({ "error": e });
        eprintln!("{}", serde_json::to_string(&err).unwrap_or(e));
        process::exit(1);
    }
}

pub fn print_json(value: &impl serde::Serialize, pretty: bool) -> Result<(), String> {
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
        *by_project.entry(s.session_name.clone()).or_insert(0u32) += 1;
    }

    // Find sessions needing attention
    let needs_permission: Vec<serde_json::Value> = filtered
        .iter()
        .filter(|s| s.status == session::SessionStatus::NeedsAttention)
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
    let mut conversation = session::conversation::get_conversation_data(&resolved_id, true)?;

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
    case_sensitive: bool,
    whole_word: bool,
    pretty: bool,
) -> Result<(), String> {
    if query.trim().is_empty() {
        return Err("Search query cannot be empty".to_string());
    }
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    let output = cmd_search_output(
        &home_dir,
        query,
        project_filter,
        limit,
        case_sensitive,
        whole_word,
    )?;
    print_json(&output, pretty)
}

fn cmd_search_output(
    home_dir: &std::path::Path,
    query: &str,
    project_filter: Option<&str>,
    limit: usize,
    case_sensitive: bool,
    whole_word: bool,
) -> Result<serde_json::Value, String> {
    if query.trim().is_empty() {
        return Err("Search query cannot be empty".to_string());
    }
    let hits = session::history::deep_search_under(home_dir, query, case_sensitive, whole_word)?;

    let enriched: Vec<serde_json::Value> = hits
        .into_iter()
        .map(|hit| enrich_search_hit(hit))
        .filter(|hit| search_hit_matches_project(hit, project_filter))
        .take(limit)
        .collect();

    let output = serde_json::json!({
        "query": query,
        "hits": enriched,
        "truncated": enriched.len() == limit,
    });
    Ok(output)
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

fn cmd_stop(target: &str, pretty: bool) -> Result<(), String> {
    if target.is_empty() {
        return Err("stop target required (PID or worker session id/prefix)".to_string());
    }

    // Worker-first dispatch: worker session UUIDs routinely begin with
    // digits (e.g. `12345678-abcd-...`), so a prefix like `12345678` would
    // previously parse as PID and SIGKILL an arbitrary OS process. Ask the
    // daemon for a worker match first; only fall back to PID if no worker
    // matches AND the target parses as u32.
    #[cfg(feature = "pm-orchestration")]
    if let Some(worker_id) = try_resolve_worker(target) {
        return pm::cmd_stop(worker_id, pretty);
    }

    // If target parses as u32, treat as PID (regular Claude session stop).
    if let Ok(pid) = target.parse::<u32>() {
        crate::actions::stop_session(pid)?;
        let output = serde_json::json!({
            "stopped": true,
            "pid": pid,
        });
        return print_json(&output, pretty);
    }

    // Not numeric, no worker match — fall through to PM so it can surface
    // WORKER_NOT_FOUND (after starting the daemon if needed).
    #[cfg(feature = "pm-orchestration")]
    return pm::cmd_stop(target.to_string(), pretty);

    // Without PM orchestration a non-numeric target can't refer to anything.
    #[cfg(not(feature = "pm-orchestration"))]
    Err(format!(
        "invalid stop target '{}': expected a numeric PID",
        target
    ))
}

/// If the PM daemon is already running and has exactly one worker whose
/// session id matches `target` (exact or unique prefix), return that full
/// session id. Returns `None` if the daemon is unreachable, if there's no
/// match, or if the match is ambiguous — leaving the caller free to fall
/// back to PID-based stop.
///
/// This function never starts the daemon: if the daemon isn't running there
/// can't be any workers to match against, so PID-based stop is safe.
#[cfg(feature = "pm-orchestration")]
fn try_resolve_worker(target: &str) -> Option<String> {
    use std::time::Duration;
    let sock_path = pm_fs::daemon_sock_path().ok()?;
    if !sock_path.exists() {
        return None;
    }
    let request = pm_rpc::RpcRequest::List;
    let response = pm_rpc::rpc_call(&sock_path, &request, Duration::from_secs(2)).ok()?;
    let workers = response.get("workers").and_then(|w| w.as_array())?;
    let ids: Vec<String> = workers
        .iter()
        .filter_map(|w| {
            w.get("sessionId")
                .and_then(|s| s.as_str())
                .map(String::from)
        })
        .collect();

    // Delegate matching to the shared helper in pm_daemon so exact/prefix/
    // ambiguous rules stay in one place. An ambiguous prefix silently falls
    // through to None: the caller will next try PID parsing, which fails for
    // a uuid-shaped prefix, landing in the WORKER_NOT_FOUND path from PM.
    pm_daemon::resolve_worker_id_from_keys(ids.iter().map(|s| s.as_str()), target).ok()
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

        let mut current_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

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

/// Minimum length for `--session` values when treated as a prefix. Use
/// `--session-prefix` for anything shorter.
const SESSION_MIN_LEN: usize = 8;

/// How a session lookup value was provided — controls min-length enforcement.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SessionLookupKind {
    /// `--session`: full id or ≥8-char prefix. Exact matches win outright.
    Session,
    /// `--session-prefix`: any length; must match exactly one id.
    Prefix,
}

/// Resolve a session identifier (full id or prefix) against all known sessions.
///
/// Returns the full `session_id`, or an error if the input is empty, too short
/// for the chosen kind, or matches zero/multiple sessions.
fn resolve_cost_session_id(
    sessions: &[session::cost::SessionCostRecord],
    needle: &str,
    kind: SessionLookupKind,
) -> Result<String, String> {
    if needle.is_empty() {
        return Err("Session id must not be empty".to_string());
    }
    if kind == SessionLookupKind::Session && needle.len() < SESSION_MIN_LEN {
        return Err(format!(
            "--session value '{}' is too short — must be at least {} characters, or use --session-prefix for shorter prefixes",
            needle, SESSION_MIN_LEN
        ));
    }

    let mut matches = std::collections::BTreeSet::new();
    for s in sessions {
        if s.session_id.starts_with(needle) {
            matches.insert(s.session_id.clone());
        }
    }

    // For `--session`, a full-id exact match wins even if other sessions share
    // the same prefix (vanishingly rare, but deterministic).
    if kind == SessionLookupKind::Session && matches.contains(needle) {
        return Ok(needle.to_string());
    }

    match matches.len() {
        0 => Err(format!("No session matching '{}'", needle)),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => {
            let ids: Vec<String> = matches.into_iter().collect();
            let shown = ids.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
            let suffix = if n > 5 {
                format!(" (and {} more)", n - 5)
            } else {
                String::new()
            };
            Err(format!(
                "Ambiguous '{}' matches {} sessions: {}{}",
                needle, n, shown, suffix
            ))
        }
    }
}

/// Rows returned by filtering cost data down to a single session.
#[derive(Debug)]
struct SessionCostBreakdown {
    session_id: String,
    session_name: String,
    project: String,
    project_name: String,
    total_cost: f64,
    total_tokens: u64,
    models: Vec<String>,
    earliest: String,
    latest: String,
    daily: Vec<DailyRow>,
}

#[derive(Debug)]
struct DailyRow {
    date: String,
    cost: f64,
    total_tokens: u64,
    model: String,
}

/// Build a per-session breakdown from raw session records. Factored out so
/// tests can exercise filtering + aggregation without touching disk.
fn build_session_breakdown(
    all_sessions: &[session::cost::SessionCostRecord],
    session_id: &str,
    since_date: Option<chrono::NaiveDate>,
) -> Result<SessionCostBreakdown, String> {
    let mut records: Vec<&session::cost::SessionCostRecord> = all_sessions
        .iter()
        .filter(|s| s.session_id == session_id)
        .filter(|s| match since_date {
            Some(since_d) => chrono::NaiveDate::parse_from_str(&s.date, "%Y-%m-%d")
                .map(|d| d >= since_d)
                .unwrap_or(false),
            None => true,
        })
        .collect();

    if records.is_empty() {
        return Err(format!(
            "No cost data for session '{}' in the given date range",
            session_id
        ));
    }

    records.sort_by(|a, b| a.date.cmp(&b.date).then(a.timestamp.cmp(&b.timestamp)));

    let total_cost: f64 = records.iter().map(|s| s.cost).sum();
    let total_tokens: u64 = records.iter().map(|s| s.total_tokens).sum();

    let mut seen_models = std::collections::HashSet::new();
    let mut models: Vec<String> = Vec::new();
    for s in &records {
        if !s.model.is_empty() && seen_models.insert(s.model.clone()) {
            models.push(s.model.clone());
        }
    }

    let earliest = records
        .iter()
        .map(|s| &s.timestamp)
        .min()
        .cloned()
        .unwrap_or_default();
    let latest = records
        .iter()
        .map(|s| &s.timestamp)
        .max()
        .cloned()
        .unwrap_or_default();

    let daily = records
        .iter()
        .map(|s| DailyRow {
            date: s.date.clone(),
            cost: s.cost,
            total_tokens: s.total_tokens,
            model: s.model.clone(),
        })
        .collect();

    let first = records.first().unwrap();
    Ok(SessionCostBreakdown {
        session_id: session_id.to_string(),
        session_name: first.session_name.clone(),
        project: first.project.clone(),
        project_name: first.project_name.clone(),
        total_cost,
        total_tokens,
        models,
        earliest,
        latest,
        daily,
    })
}

fn breakdown_to_json(b: &SessionCostBreakdown) -> serde_json::Value {
    let by_day: Vec<serde_json::Value> = b
        .daily
        .iter()
        .map(|d| {
            serde_json::json!({
                "date": d.date,
                "cost": d.cost,
                "totalTokens": d.total_tokens,
                "model": d.model,
            })
        })
        .collect();
    serde_json::json!({
        "sessionId": b.session_id,
        "sessionName": b.session_name,
        "project": b.project,
        "projectName": b.project_name,
        "totalCost": b.total_cost,
        "totalTokens": b.total_tokens,
        "turns": b.daily.len(),
        "models": b.models,
        "earliest": b.earliest,
        "latest": b.latest,
        "byDay": by_day,
    })
}

/// Per-session cost breakdown for `c9watch cost --session <id>` / `--session-prefix <prefix>`.
fn cmd_cost_session(
    needle: &str,
    kind: SessionLookupKind,
    since: Option<&str>,
    pretty: bool,
) -> Result<(), String> {
    let since_date = match since {
        Some(s) => Some(
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|e| format!("Invalid --since date '{}': {}", s, e))?,
        ),
        None => None,
    };

    let data = session::get_cost_data()?;
    let all_sessions: Vec<session::cost::SessionCostRecord> = data
        .project_costs
        .iter()
        .flat_map(|p| p.sessions.iter().cloned())
        .collect();

    let session_id = resolve_cost_session_id(&all_sessions, needle, kind)?;
    let breakdown = build_session_breakdown(&all_sessions, &session_id, since_date)?;

    print_json(&breakdown_to_json(&breakdown), pretty)
}

#[allow(clippy::too_many_arguments)]
fn cmd_cost(
    daily: bool,
    weekly: bool,
    project_filter: Option<&str>,
    since: Option<&str>,
    sort: CostSortField,
    sort_dir: CostSortDir,
    session: Option<&str>,
    session_prefix: Option<&str>,
    pretty: bool,
) -> Result<(), String> {
    // clap `conflicts_with_all` already blocks mixing these flags, but keep a
    // belt-and-braces check in case the function is called programmatically.
    if let Some(needle) = session {
        if daily || weekly || project_filter.is_some() || session_prefix.is_some() {
            return Err(
                "--session cannot be combined with --daily, --weekly, --project, or --session-prefix"
                    .to_string(),
            );
        }
        return cmd_cost_session(needle, SessionLookupKind::Session, since, pretty);
    }
    if let Some(prefix) = session_prefix {
        if daily || weekly || project_filter.is_some() {
            return Err(
                "--session-prefix cannot be combined with --daily, --weekly, or --project"
                    .to_string(),
            );
        }
        return cmd_cost_session(prefix, SessionLookupKind::Prefix, since, pretty);
    }

    if daily && weekly {
        return Err("--daily and --weekly are mutually exclusive".to_string());
    }

    let since_date = match since {
        Some(s) => Some(
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|e| format!("Invalid --since date '{}': {}", s, e))?,
        ),
        None => None,
    };

    let data = session::get_cost_data()?;

    // Flatten to sessions, then apply filters. project_costs covers all sessions.
    let mut sessions: Vec<session::cost::SessionCostRecord> = data
        .project_costs
        .iter()
        .flat_map(|p| p.sessions.iter().cloned())
        .collect();

    if let Some(proj) = project_filter {
        sessions.retain(|s| s.project == proj);
    }
    if let Some(since_d) = since_date {
        sessions.retain(|s| {
            chrono::NaiveDate::parse_from_str(&s.date, "%Y-%m-%d")
                .map(|d| d >= since_d)
                .unwrap_or(false)
        });
    }

    // Helper: apply sort to a rows vec that has a date key and a cost key.
    // `date_key` is the JSON field name for the date string (ISO format, so
    // lexicographic order equals chronological order).
    let sort_rows = |rows: &mut Vec<serde_json::Value>, date_key: &str| {
        rows.sort_by(|a, b| {
            let ord = match sort {
                CostSortField::Date => a
                    .get(date_key)
                    .and_then(|v| v.as_str())
                    .cmp(&b.get(date_key).and_then(|v| v.as_str())),
                CostSortField::Cost => {
                    let ca = a.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let cb = b.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
                }
            };
            // Flip for descending
            if sort_dir == CostSortDir::Desc {
                ord.reverse()
            } else {
                ord
            }
        });
    };

    if daily {
        let mut by_date: std::collections::HashMap<String, (f64, u64, u32)> =
            std::collections::HashMap::new();
        for s in &sessions {
            let e = by_date.entry(s.date.clone()).or_insert((0.0, 0, 0));
            e.0 += s.cost;
            e.1 += s.total_tokens;
            e.2 += 1;
        }
        let mut rows: Vec<_> = by_date
            .into_iter()
            .map(|(date, (cost, tokens, count))| {
                serde_json::json!({
                    "date": date,
                    "cost": cost,
                    "totalTokens": tokens,
                    "sessionCount": count,
                })
            })
            .collect();
        sort_rows(&mut rows, "date");
        return print_json(&rows, pretty);
    }

    if weekly {
        let mut by_week: std::collections::HashMap<String, (f64, u64, u32)> =
            std::collections::HashMap::new();
        for s in &sessions {
            let Ok(d) = chrono::NaiveDate::parse_from_str(&s.date, "%Y-%m-%d") else {
                continue;
            };
            use chrono::Datelike;
            let iso = d.iso_week();
            let week_start =
                chrono::NaiveDate::from_isoywd_opt(iso.year(), iso.week(), chrono::Weekday::Mon)
                    .unwrap_or(d);
            let key = week_start.format("%Y-%m-%d").to_string();
            let e = by_week.entry(key).or_insert((0.0, 0, 0));
            e.0 += s.cost;
            e.1 += s.total_tokens;
            e.2 += 1;
        }
        let mut rows: Vec<_> = by_week
            .into_iter()
            .map(|(week_start, (cost, tokens, count))| {
                serde_json::json!({
                    "weekStart": week_start,
                    "cost": cost,
                    "totalTokens": tokens,
                    "sessionCount": count,
                })
            })
            .collect();
        sort_rows(&mut rows, "weekStart");
        return print_json(&rows, pretty);
    }

    // Default: emit filtered CostData-shaped output if filters were applied,
    // otherwise emit the full CostData from the GUI.
    if project_filter.is_none() && since_date.is_none() {
        // --sort date --project is nonsensical (no single date per project);
        // silently fall back to cost sort in that case. Here there's no project
        // aggregation at all, so just return raw CostData unchanged.
        return print_json(&data, pretty);
    }

    // Project mode: --sort date is not meaningful (projects have no single date);
    // silently fall back to cost sort and note it in a comment.
    let effective_sort = if project_filter.is_some() && sort == CostSortField::Date {
        // date sort doesn't apply to project aggregations — use cost instead
        CostSortField::Cost
    } else {
        sort
    };

    // Rebuild CostData-ish aggregation from filtered sessions, sorting by session date or cost.
    let total_cost: f64 = sessions.iter().map(|s| s.cost).sum();
    let total_tokens: u64 = sessions.iter().map(|s| s.total_tokens).sum();

    // Sort individual sessions in the filtered output
    sessions.sort_by(|a, b| {
        let ord = match effective_sort {
            CostSortField::Date => a.date.cmp(&b.date),
            CostSortField::Cost => a
                .cost
                .partial_cmp(&b.cost)
                .unwrap_or(std::cmp::Ordering::Equal),
        };
        if sort_dir == CostSortDir::Desc {
            ord.reverse()
        } else {
            ord
        }
    });

    let output = serde_json::json!({
        "totalCost": total_cost,
        "totalTokens": total_tokens,
        "sessions": sessions,
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
    insert_session_contract(&mut json, s);

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
    insert_session_contract(&mut json, &s);

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

fn insert_session_contract(json: &mut serde_json::Value, session: &session::enrichment::Session) {
    let Some(object) = json.as_object_mut() else {
        return;
    };
    object.insert("provider".to_string(), serde_json::json!(session.provider));
    object.insert("surface".to_string(), serde_json::json!(session.surface));
    object.insert(
        "agentKind".to_string(),
        serde_json::json!(session.agent_kind),
    );
    object.insert("canOpen".to_string(), serde_json::json!(session.can_open));
    object.insert("canStop".to_string(), serde_json::json!(session.can_stop));
    object.insert(
        "canRename".to_string(),
        serde_json::json!(session.can_rename),
    );
    for (key, value) in [
        ("parentThreadId", session.parent_thread_id.as_ref()),
        ("rootSessionId", session.root_session_id.as_ref()),
        ("agentPath", session.agent_path.as_ref()),
        ("agentNickname", session.agent_nickname.as_ref()),
        ("agentRole", session.agent_role.as_ref()),
        ("internalKind", session.internal_kind.as_ref()),
    ] {
        if let Some(value) = value {
            object.insert(key.to_string(), serde_json::json!(value));
        }
    }
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
        "provider": entry.provider,
        "surface": entry.surface,
        "agentKind": entry.agent_kind,
    });

    if let Some(title) = entry.custom_title {
        json.as_object_mut()
            .unwrap()
            .insert("customTitle".to_string(), serde_json::json!(title));
    }

    json
}

fn enrich_search_hit(hit: session::DeepSearchHit) -> serde_json::Value {
    let (project_path, modified) = if hit.provider.eq_ignore_ascii_case("codex") {
        (hit.project_path.clone(), hit.modified.clone())
    } else {
        let (project_path_encoded, modified) = find_session_metadata(&hit.session_id);

        // Decode the encoded project path back to a real filesystem path
        // e.g. "-Users-liminchen-Documents-GitHub-c9watch" -> "/Users/liminchen/Documents/GitHub/c9watch"
        (
            project_path_encoded.map(|encoded| decode_project_path(&encoded)),
            modified,
        )
    };

    format_search_hit(hit, project_path, modified)
}

fn search_hit_matches_project(hit: &serde_json::Value, project_filter: Option<&str>) -> bool {
    match project_filter {
        Some(filter) => hit
            .get("projectPath")
            .and_then(|path| path.as_str())
            .map(|path| path.contains(filter))
            .unwrap_or(false),
        None => true,
    }
}

fn format_search_hit(
    hit: session::DeepSearchHit,
    project_path: Option<String>,
    modified: Option<String>,
) -> serde_json::Value {
    serde_json::json!({
        "sessionId": hit.session_id,
        "snippet": strip_system_tags(&hit.snippet),
        "projectPath": project_path,
        "modified": modified,
        "provider": hit.provider,
        "surface": hit.surface,
        "agentKind": hit.agent_kind,
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

pub(crate) fn read_session_tasks(session_id: &str) -> Result<Vec<serde_json::Value>, String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    read_session_tasks_in(&home_dir, session_id)
}

fn read_session_tasks_in(
    home_dir: &std::path::Path,
    session_id: &str,
) -> Result<Vec<serde_json::Value>, String> {
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
    resolve_session_id_lightweight_under(prefix, &home_dir)
}

fn resolve_session_id_lightweight_under(
    prefix: &str,
    home_dir: &std::path::Path,
) -> Result<String, String> {
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

    matches.extend(session::codex_session_ids(home_dir, prefix));

    resolve_session_id_matches(prefix, matches)
}

fn resolve_session_id_matches(prefix: &str, matches: Vec<String>) -> Result<String, String> {
    let mut matches = matches;
    matches.sort();
    matches.dedup();

    match matches.len() {
        0 => {
            // If it looks like a full UUID, let it through (might be a valid ID
            // in a project dir we couldn't enumerate). Otherwise, fail clearly.
            if prefix.len() >= 32 {
                Ok(prefix.to_string())
            } else {
                Err(format!(
                    "No session found matching prefix '{}' across Claude Code and Codex",
                    prefix
                ))
            }
        }
        1 => Ok(matches.into_iter().next().unwrap()),
        n => Err(format!(
            "Ambiguous session ID prefix '{}' matches {} sessions across Claude Code and Codex",
            prefix, n
        )),
    }
}

#[cfg(test)]
mod session_formatter_tests {
    use super::*;
    use crate::session::source::{AgentKind, SessionProvider, SessionSurface};
    use crate::session::SessionStatus;

    fn codex_subagent() -> session::enrichment::Session {
        session::enrichment::Session {
            id: "child-thread".to_string(),
            pid: 0,
            session_name: "child".to_string(),
            custom_title: None,
            project_path: "/tmp/project".to_string(),
            git_branch: None,
            first_prompt: "investigate".to_string(),
            summary: None,
            message_count: 3,
            modified: "2026-07-13T00:00:00Z".to_string(),
            status: SessionStatus::Working,
            latest_message: String::new(),
            pending_tool_name: None,
            pending_tool_input: None,
            worker_of: None,
            official_name: None,
            started_at_ms: Some(1_752_364_800_000),
            provider: SessionProvider::Codex,
            surface: SessionSurface::App,
            agent_kind: AgentKind::Subagent,
            parent_thread_id: Some("parent-thread".to_string()),
            root_session_id: Some("root-thread".to_string()),
            agent_path: Some("/root/investigator".to_string()),
            agent_nickname: Some("Scout".to_string()),
            agent_role: Some("investigator".to_string()),
            internal_kind: Some("spawned".to_string()),
            can_open: false,
            can_stop: false,
            can_rename: false,
        }
    }

    fn assert_codex_contract(value: &serde_json::Value) {
        assert_eq!(value["provider"], "codex");
        assert_eq!(value["surface"], "app");
        assert_eq!(value["agentKind"], "subagent");
        assert_eq!(value["parentThreadId"], "parent-thread");
        assert_eq!(value["rootSessionId"], "root-thread");
        assert_eq!(value["agentPath"], "/root/investigator");
        assert_eq!(value["agentNickname"], "Scout");
        assert_eq!(value["agentRole"], "investigator");
        assert_eq!(value["internalKind"], "spawned");
        assert_eq!(value["canOpen"], false);
        assert_eq!(value["canStop"], false);
        assert_eq!(value["canRename"], false);
    }

    #[test]
    fn search_hit_includes_provider_surface_and_agent_kind() {
        let value = enrich_search_hit(session::DeepSearchHit {
            session_id: "search-session".to_string(),
            snippet: "matching text".to_string(),
            project_path: Some("/tmp/codex-project".to_string()),
            modified: Some("2026-07-13T02:03:04+00:00".to_string()),
            provider: "codex".to_string(),
            surface: Some("cli".to_string()),
            agent_kind: "subagent".to_string(),
        });

        assert_eq!(value["provider"], "codex");
        assert_eq!(value["surface"], "cli");
        assert_eq!(value["agentKind"], "subagent");
    }

    #[test]
    fn codex_rollout_search_wiring_attaches_metadata_and_filters_project() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join(".codex/sessions/2026/07/13");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("rollout-codex-search.jsonl"),
            concat!(
                r#"{"timestamp":"2026-07-13T02:03:04Z","type":"session_meta","payload":{"id":"codex-search","cwd":"/tmp/codex-project","source":"cli","originator":"codex-tui"}}"#,
                "\n",
                r#"{"timestamp":"2026-07-13T02:03:04Z","type":"event_msg","payload":{"type":"user_message","message":"search wiring marker"}}"#,
            ),
        )
        .unwrap();

        let matching = cmd_search_output(
            home.path(),
            "search wiring marker",
            Some("codex-project"),
            20,
            false,
            false,
        )
        .unwrap();
        let hits = matching["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["sessionId"], "codex-search");
        assert_eq!(hits[0]["projectPath"], "/tmp/codex-project");
        assert_eq!(hits[0]["modified"], "2026-07-13T02:03:04+00:00");

        let nonmatching = cmd_search_output(
            home.path(),
            "search wiring marker",
            Some("other-project"),
            20,
            false,
            false,
        )
        .unwrap();
        assert!(nonmatching["hits"].as_array().unwrap().is_empty());
    }

    #[test]
    fn session_prefix_resolves_unique_codex_id() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join(".codex/sessions/2026/07/13");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("rollout-codex-unique.jsonl"),
            r#"{"timestamp":"2026-07-13T02:03:04Z","type":"session_meta","payload":{"id":"codex-unique-thread","cwd":"/tmp/codex","source":"cli","originator":"codex-tui"}}"#,
        )
        .unwrap();

        assert_eq!(
            resolve_session_id_lightweight_under("codex-unique", home.path()).unwrap(),
            "codex-unique-thread"
        );
    }

    #[test]
    fn session_prefix_reports_ambiguity_across_providers() {
        let home = tempfile::tempdir().unwrap();
        let claude_project = home.path().join(".claude/projects/project");
        std::fs::create_dir_all(&claude_project).unwrap();
        std::fs::write(claude_project.join("shared-claude-session.jsonl"), "").unwrap();
        let codex_sessions = home.path().join(".codex/sessions/2026/07/13");
        std::fs::create_dir_all(&codex_sessions).unwrap();
        std::fs::write(
            codex_sessions.join("rollout-shared-codex.jsonl"),
            r#"{"timestamp":"2026-07-13T02:03:04Z","type":"session_meta","payload":{"id":"shared-codex-thread","cwd":"/tmp/codex","source":"cli","originator":"codex-tui"}}"#,
        )
        .unwrap();

        let error = resolve_session_id_lightweight_under("shared", home.path()).unwrap_err();

        assert!(error.contains("Ambiguous session ID prefix 'shared'"));
        assert!(error.contains("matches 2 sessions"));
        assert!(error.contains("Claude Code and Codex"));
    }

    #[test]
    fn session_prefix_reports_no_match_across_providers() {
        let home = tempfile::tempdir().unwrap();
        let error = resolve_session_id_lightweight_under("missing", home.path()).unwrap_err();

        assert_eq!(
            error,
            "No session found matching prefix 'missing' across Claude Code and Codex"
        );
    }

    #[test]
    fn compact_session_includes_provider_hierarchy_and_capabilities() {
        assert_codex_contract(&compact_session(&codex_subagent()));
    }

    #[test]
    fn full_session_includes_provider_hierarchy_and_capabilities() {
        assert_codex_contract(&full_session(codex_subagent()));
    }
}

#[cfg(test)]
mod cost_session_tests {
    use super::*;
    use crate::session::cost::SessionCostRecord;

    fn rec(session_id: &str, date: &str, cost: f64, tokens: u64, model: &str) -> SessionCostRecord {
        SessionCostRecord {
            session_id: session_id.to_string(),
            project: "/tmp/proj".to_string(),
            project_name: "proj".to_string(),
            model: model.to_string(),
            provider: "claudeCode".to_string(),
            surface: None,
            agent_kind: None,
            cost,
            cost_available: true,
            input_tokens: tokens,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: tokens,
            timestamp: format!("{}T00:00:00Z", date),
            date: date.to_string(),
            session_name: "sample session".to_string(),
        }
    }

    fn fixtures() -> Vec<SessionCostRecord> {
        vec![
            rec(
                "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
                "2026-04-10",
                1.25,
                1000,
                "sonnet",
            ),
            rec(
                "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
                "2026-04-11",
                0.75,
                500,
                "sonnet",
            ),
            rec(
                "aaaa2222-bbbb-cccc-dddd-ffffffffffff",
                "2026-04-10",
                0.50,
                200,
                "haiku",
            ),
            rec(
                "bbbb3333-cccc-dddd-eeee-000000000000",
                "2026-04-11",
                2.00,
                5000,
                "opus",
            ),
        ]
    }

    #[test]
    fn session_exact_full_id_match() {
        let sessions = fixtures();
        let full = "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee";
        let got = resolve_cost_session_id(&sessions, full, SessionLookupKind::Session).unwrap();
        assert_eq!(got, full);
    }

    #[test]
    fn session_prefix_unambiguous() {
        let sessions = fixtures();
        let got =
            resolve_cost_session_id(&sessions, "bbbb3333", SessionLookupKind::Session).unwrap();
        assert_eq!(got, "bbbb3333-cccc-dddd-eeee-000000000000");
    }

    #[test]
    fn session_prefix_too_short_for_session_flag() {
        let sessions = fixtures();
        let err =
            resolve_cost_session_id(&sessions, "aaaa", SessionLookupKind::Session).unwrap_err();
        assert!(err.contains("too short"), "got: {}", err);
        assert!(err.contains("--session-prefix"), "got: {}", err);
    }

    #[test]
    fn session_prefix_kind_allows_short_unique() {
        let sessions = fixtures();
        let got = resolve_cost_session_id(&sessions, "bbbb", SessionLookupKind::Prefix).unwrap();
        assert_eq!(got, "bbbb3333-cccc-dddd-eeee-000000000000");
    }

    #[test]
    fn session_prefix_ambiguous_lists_candidates() {
        let sessions = fixtures();
        let err =
            resolve_cost_session_id(&sessions, "aaaa", SessionLookupKind::Prefix).unwrap_err();
        assert!(err.contains("Ambiguous"), "got: {}", err);
        assert!(err.contains("aaaa1111-"), "got: {}", err);
        assert!(err.contains("aaaa2222-"), "got: {}", err);
    }

    #[test]
    fn session_prefix_no_match_errors() {
        let sessions = fixtures();
        let err =
            resolve_cost_session_id(&sessions, "zzzzzzzz", SessionLookupKind::Prefix).unwrap_err();
        assert!(err.contains("No session matching"), "got: {}", err);
    }

    #[test]
    fn session_empty_errors() {
        let sessions = fixtures();
        let err = resolve_cost_session_id(&sessions, "", SessionLookupKind::Prefix).unwrap_err();
        assert!(err.contains("must not be empty"), "got: {}", err);
    }

    #[test]
    fn build_breakdown_sums_across_days() {
        let sessions = fixtures();
        let b = build_session_breakdown(&sessions, "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee", None)
            .unwrap();
        assert!((b.total_cost - 2.00).abs() < 1e-9);
        assert_eq!(b.total_tokens, 1500);
        assert_eq!(b.daily.len(), 2);
        assert_eq!(b.daily[0].date, "2026-04-10");
        assert_eq!(b.daily[1].date, "2026-04-11");
        assert_eq!(b.models, vec!["sonnet".to_string()]);
        assert_eq!(b.earliest, "2026-04-10T00:00:00Z");
        assert_eq!(b.latest, "2026-04-11T00:00:00Z");
    }

    #[test]
    fn build_breakdown_since_filter_drops_earlier_days() {
        let sessions = fixtures();
        let since = chrono::NaiveDate::parse_from_str("2026-04-11", "%Y-%m-%d").unwrap();
        let b = build_session_breakdown(
            &sessions,
            "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
            Some(since),
        )
        .unwrap();
        assert_eq!(b.daily.len(), 1);
        assert_eq!(b.daily[0].date, "2026-04-11");
        assert!((b.total_cost - 0.75).abs() < 1e-9);
    }

    #[test]
    fn build_breakdown_empty_for_missing_session() {
        let sessions = fixtures();
        let err = build_session_breakdown(&sessions, "nonexistent", None).unwrap_err();
        assert!(err.contains("No cost data"), "got: {}", err);
    }

    #[test]
    fn json_output_shape_has_expected_fields() {
        let sessions = fixtures();
        let b = build_session_breakdown(&sessions, "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee", None)
            .unwrap();
        let v = breakdown_to_json(&b);
        for key in [
            "sessionId",
            "sessionName",
            "project",
            "projectName",
            "totalCost",
            "totalTokens",
            "turns",
            "models",
            "earliest",
            "latest",
            "byDay",
        ] {
            assert!(v.get(key).is_some(), "missing key {} in {}", key, v);
        }
        assert_eq!(v["sessionId"], "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee");
        assert_eq!(v["turns"], 2);
        let by_day = v["byDay"].as_array().unwrap();
        assert_eq!(by_day.len(), 2);
        for row in by_day {
            for key in ["date", "cost", "totalTokens", "model"] {
                assert!(row.get(key).is_some(), "missing {} in {}", key, row);
            }
        }
    }

    #[test]
    fn json_compact_is_single_line() {
        let sessions = fixtures();
        let b = build_session_breakdown(&sessions, "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee", None)
            .unwrap();
        let v = breakdown_to_json(&b);
        let compact = serde_json::to_string(&v).unwrap();
        assert!(
            !compact.contains('\n'),
            "compact output must be single-line: {}",
            compact
        );
        let parsed: serde_json::Value = serde_json::from_str(&compact).unwrap();
        assert_eq!(parsed["sessionId"], "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee");
    }

    #[test]
    fn json_pretty_is_indented_and_valid() {
        let sessions = fixtures();
        let b = build_session_breakdown(&sessions, "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee", None)
            .unwrap();
        let v = breakdown_to_json(&b);
        let pretty = serde_json::to_string_pretty(&v).unwrap();
        assert!(pretty.contains('\n'), "pretty output must be multi-line");
        assert!(pretty.contains("  "), "pretty output must be indented");
        let parsed: serde_json::Value = serde_json::from_str(&pretty).unwrap();
        assert_eq!(parsed["sessionId"], "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee");
        assert_eq!(parsed["byDay"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn clap_rejects_session_with_daily() {
        use clap::Parser;
        let res = Cli::try_parse_from(["c9watch", "cost", "--session", "aaaa1111-bbbb", "--daily"]);
        assert!(res.is_err(), "expected clap to reject --session + --daily");
    }

    #[test]
    fn clap_rejects_session_with_session_prefix() {
        use clap::Parser;
        let res = Cli::try_parse_from([
            "c9watch",
            "cost",
            "--session",
            "aaaa1111-bbbb",
            "--session-prefix",
            "aa",
        ]);
        assert!(
            res.is_err(),
            "expected clap to reject --session + --session-prefix"
        );
    }

    #[test]
    fn clap_accepts_session_alone() {
        use clap::Parser;
        let res = Cli::try_parse_from(["c9watch", "cost", "--session", "aaaa1111-bbbb"]);
        assert!(res.is_ok(), "parse failed: {:?}", res.err());
    }
}

#[cfg(test)]
mod read_session_tasks_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_task(home: &std::path::Path, session_id: &str, file: &str, body: &str) {
        let dir = home.join(".claude").join("tasks").join(session_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(file), body).unwrap();
    }

    #[test]
    fn returns_empty_when_dir_missing() {
        let home = tempdir().unwrap();
        let got = read_session_tasks_in(home.path(), "no-such-session").unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn parses_and_sorts_by_numeric_id() {
        let home = tempdir().unwrap();
        let sid = "session-abc";
        write_task(
            home.path(),
            sid,
            "10.json",
            r#"{"id":"10","content":"ten","status":"pending"}"#,
        );
        write_task(
            home.path(),
            sid,
            "2.json",
            r#"{"id":"2","content":"two","status":"in_progress"}"#,
        );
        write_task(
            home.path(),
            sid,
            "1.json",
            r#"{"id":"1","content":"one","status":"completed"}"#,
        );

        let got = read_session_tasks_in(home.path(), sid).unwrap();
        let ids: Vec<&str> = got
            .iter()
            .map(|t| t.get("id").and_then(|v| v.as_str()).unwrap_or(""))
            .collect();
        assert_eq!(ids, vec!["1", "2", "10"]);
    }

    #[test]
    fn skips_malformed_json_and_non_json_files() {
        let home = tempdir().unwrap();
        let sid = "session-xyz";
        write_task(
            home.path(),
            sid,
            "1.json",
            r#"{"id":"1","content":"good","status":"pending"}"#,
        );
        write_task(home.path(), sid, "2.json", "not valid json {{{");
        write_task(home.path(), sid, "ignore.txt", r#"{"id":"99"}"#);

        let got = read_session_tasks_in(home.path(), sid).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].get("id").and_then(|v| v.as_str()), Some("1"));
    }
}
