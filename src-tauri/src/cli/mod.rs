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

mod cost;
mod session_json;
mod session_ref;

use cost::cmd_cost;
use session_json::{
    compact_session, enrich_history_entry, enrich_search_hit, full_session,
    search_hit_matches_project,
};
use session_ref::resolve_session_reference_lightweight;

// PM/worker orchestration modules — opt-in only. Compiled out of default builds.
pub mod watch;

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

/// First arguments that make the app binary run the CLI instead of the GUI.
///
/// Every subcommand must be listed, or the desktop binary launches the GUI
/// for it (v0.8.1 shipped that bug for `inbox`/`cost`/`adopt`). PM command
/// names are listed even when the `pm-orchestration` feature is off, so Clap
/// reports an unknown command instead of launching the GUI.
pub const CLI_COMMANDS: &[&str] = &[
    "list",
    "status",
    "self",
    "view",
    "history",
    "search",
    "stop",
    "watch",
    "wait",
    "tasks",
    "spawn",
    "send",
    "workers",
    "inbox",
    "adopt",
    "daemon",
    "cost",
    "usage-bridge",
    "hooks",
    "help",
];

/// Whether `first` (the first argument) selects the CLI.
pub fn is_cli_invocation(first: &str) -> bool {
    CLI_COMMANDS.contains(&first) || matches!(first, "--help" | "-h" | "--version" | "-V")
}

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
    /// Capture Claude Code status-line quotas locally (no API requests)
    UsageBridge {
        /// Back up settings and connect the bridge, preserving the existing status line
        #[arg(long, conflicts_with = "passthrough")]
        install: bool,
        /// Forward the original JSON to an existing status line command
        #[arg(long)]
        passthrough: bool,
    },
    /// Report Claude Code permission prompts to c9watch through hooks
    ///
    /// With no flags, reads one hook event from stdin (Claude Code runs this).
    Hooks {
        /// Back up settings and register the c9watch hooks
        #[arg(long, conflicts_with = "uninstall")]
        install: bool,
        /// Back up settings and remove the c9watch hooks
        #[arg(long)]
        uninstall: bool,
    },
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
        /// Session key (`provider:id`), session ID, or unique prefix
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

        /// Accepted for scripts; output is always NDJSON (see docs/cli-events.md)
        #[arg(long)]
        json: bool,
    },

    /// Wait until a live session meets a condition, then print that event
    ///
    /// Exit codes: 0 condition met, 2 no (or more than one) live session
    /// matches, 3 the session stopped before `needs-input`, 124 timeout.
    /// `done` is met at once when the session is not Working at the start.
    Wait {
        /// Session key (`provider:id`), session ID, or unique prefix of a live session
        session_id: String,

        /// Condition to wait for
        #[arg(long, value_enum)]
        until: watch::Until,

        /// Give up after this many seconds (exit 124). Default: wait forever
        #[arg(long)]
        timeout: Option<u64>,

        /// Poll interval in seconds
        #[arg(short, long, default_value = "2")]
        interval: u64,
    },

    /// Show tasks/todos for a session
    Tasks {
        /// Session key (`provider:id`), session ID, or unique prefix
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
        /// Show cost breakdown for a single session by provider-scoped key, full id, or an 8+ char prefix. Use --session-prefix for shorter prefixes. Incompatible with --daily/--weekly/--project.
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
        Commands::UsageBridge {
            install,
            passthrough,
        } => crate::claude_usage::run(install, passthrough),
        Commands::Hooks { install, uninstall } => crate::claude_hooks::run(install, uninstall),
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
            json: _,
        } => cmd_watch(interval, project.as_deref(), compact, changes_only),
        Commands::Wait {
            session_id,
            until,
            timeout,
            interval,
        } => match cmd_wait(&session_id, until, timeout, interval) {
            Ok(0) => Ok(()),
            Ok(code) => process::exit(code),
            Err(e) => Err(e),
        },
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
                "sessionKey": s.session_key,
                "provider": s.provider,
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
    let (resolved_id, provider) = resolve_session_reference_lightweight(session_id)?;
    let mut conversation =
        session::conversation::get_conversation_data_for_provider(&resolved_id, provider)?;

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
    // Only a pid detection reports for a stoppable Claude Code session is
    // signalled, and only while it is still the process detection saw.
    if let Ok(pid) = target.parse::<u32>() {
        crate::actions::stop_session_by_pid(
            pid,
            || {
                session::create_session_source()
                    .detect()
                    .map(|(detected, _)| detected)
                    .map_err(|e| format!("Failed to detect sessions: {e}"))
            },
            crate::actions::process_start_secs,
        )?;
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

/// One poll of live sessions as `watch`/`wait` snapshots, keyed by the
/// provider-scoped key so a Codex/Cursor session never overwrites a Claude
/// session with the same opaque id.
fn poll_sessions(
    project_filter: Option<&str>,
) -> Option<(Vec<watch::Snapshot>, Vec<session::enrichment::Session>)> {
    let (sessions, _) = session::enrichment::detect_and_enrich_sessions().ok()?;
    let sessions: Vec<_> = sessions
        .into_iter()
        .filter(|s| project_filter.is_none_or(|f| s.project_path.contains(f)))
        .collect();
    let snapshots = sessions
        .iter()
        .map(|s| watch::Snapshot {
            key: s.session_key.clone(),
            id: s.id.clone(),
            status: s.status.clone(),
            pending_tool: s.pending_tool_name.clone(),
        })
        .collect();
    Some((snapshots, sessions))
}

/// The event line for `change`, with the live session payload when it exists.
fn change_line(
    change: &watch::Change,
    sessions: &[session::enrichment::Session],
    compact: bool,
) -> serde_json::Value {
    let live = sessions.iter().find(|s| s.session_key == change.key);
    watch::event_json(
        change,
        live.map(|s| serde_json::json!(s.provider)),
        live.map(|s| {
            if compact {
                compact_session(s)
            } else {
                full_session(s.clone())
            }
        }),
        &chrono::Utc::now().to_rfc3339(),
    )
}

fn print_line(line: &serde_json::Value) {
    use std::io::Write;
    println!("{}", serde_json::to_string(line).unwrap_or_default());
    let _ = std::io::stdout().flush();
}

fn cmd_watch(
    interval_secs: u64,
    project_filter: Option<&str>,
    compact: bool,
    changes_only: bool,
) -> Result<(), String> {
    let interval = std::time::Duration::from_secs(interval_secs);
    let mut prev = std::collections::HashMap::new();

    loop {
        if let Some((snapshots, sessions)) = poll_sessions(project_filter) {
            for change in watch::diff_sessions(&mut prev, snapshots, !changes_only) {
                print_line(&change_line(&change, &sessions, compact));
            }
        }
        std::thread::sleep(interval);
    }
}

/// `c9watch wait`: returns the process exit code (see `watch::EXIT_*`).
///
/// Polls run on a background thread: one poll can take many seconds with
/// many sessions, and `--timeout` must still end the wait on time.
fn cmd_wait(
    reference: &str,
    until: watch::Until,
    timeout_secs: Option<u64>,
    interval_secs: u64,
) -> Result<i32, String> {
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::{Duration, Instant};

    let deadline = timeout_secs.map(|t| Instant::now() + Duration::from_secs(t));
    let interval = Duration::from_secs(interval_secs.max(1));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || loop {
        if tx.send(poll_sessions(None)).is_err() {
            return;
        }
        std::thread::sleep(interval);
    });

    let mut key: Option<String> = None;
    let mut prev = std::collections::HashMap::new();
    loop {
        let poll = match deadline {
            Some(deadline) => {
                match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(poll) => poll,
                    Err(RecvTimeoutError::Timeout) => return Ok(watch::EXIT_TIMEOUT),
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err("The session poller stopped".to_string())
                    }
                }
            }
            None => rx
                .recv()
                .map_err(|_| "The session poller stopped".to_string())?,
        };
        let Some((snapshots, sessions)) = poll else {
            if key.is_none() {
                return Err("Failed to read the live sessions".to_string());
            }
            continue;
        };
        let target = match &key {
            Some(target) => target.clone(),
            None => match watch::resolve_live_session(reference, &snapshots) {
                Ok(target) => key.insert(target).clone(),
                Err(e) => {
                    eprintln!("{}", serde_json::json!({ "error": e }));
                    return Ok(watch::EXIT_NOT_FOUND);
                }
            },
        };
        let mine = snapshots.into_iter().filter(|s| s.key == target).collect();
        for change in watch::diff_sessions(&mut prev, mine, true) {
            if let Some(outcome) = watch::wait_outcome(until, &change) {
                print_line(&change_line(&change, &sessions, true));
                return Ok(match outcome {
                    watch::WaitOutcome::Met => 0,
                    watch::WaitOutcome::EndedFirst => watch::EXIT_ENDED_FIRST,
                });
            }
        }
    }
}

fn cmd_tasks(session_id: &str, pretty: bool) -> Result<(), String> {
    let (resolved_id, provider) = resolve_session_reference_lightweight(session_id)?;
    if let Some(provider) = provider {
        if provider != session::SessionProvider::ClaudeCode {
            return Err(format!(
                "tasks are only available for Claude Code sessions; '{}' belongs to {:?}",
                resolved_id, provider
            ));
        }
    }
    let tasks = read_session_tasks(&resolved_id)?;

    let output = serde_json::json!({
        "sessionId": resolved_id,
        "provider": provider.unwrap_or(session::SessionProvider::ClaudeCode),
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

#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use clap::CommandFactory;

    /// The app binary routes only allowlisted first arguments to the CLI.
    /// A subcommand missing from the list would launch the GUI instead.
    #[test]
    fn every_subcommand_routes_to_the_cli() {
        for sub in Cli::command().get_subcommands() {
            let name = sub.get_name();
            assert!(
                is_cli_invocation(name),
                "`c9watch {name}` would launch the GUI: add it to CLI_COMMANDS"
            );
        }
    }

    #[test]
    fn wait_routes_to_the_cli_and_the_gui_still_launches_without_args() {
        assert!(is_cli_invocation("wait"));
        assert!(is_cli_invocation("--version"));
        assert!(!is_cli_invocation("--some-tauri-flag"));
        assert!(!is_cli_invocation(""));
    }

    #[test]
    fn wait_parses_its_arguments() {
        let cli = Cli::try_parse_from([
            "c9watch",
            "wait",
            "abc",
            "--until",
            "needs-input",
            "--timeout",
            "5",
        ])
        .unwrap();
        let Commands::Wait {
            session_id,
            until,
            timeout,
            interval,
        } = cli.command
        else {
            panic!("not a wait command");
        };
        assert_eq!(session_id, "abc");
        assert_eq!(until, watch::Until::NeedsInput);
        assert_eq!(timeout, Some(5));
        assert_eq!(interval, 2);
        assert!(Cli::try_parse_from(["c9watch", "wait", "abc"]).is_err());
        assert!(Cli::try_parse_from(["c9watch", "watch", "--json"]).is_ok());
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
