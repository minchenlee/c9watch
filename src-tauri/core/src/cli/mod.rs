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

mod api_schema;
mod cost;
mod session_json;
mod session_ref;
// Engine commands talk to the engine over a unix socket. `api schema`
// also works offline without the engine feature. `task`
// holds the calls of the separate `c9watch-agent` binary (C1a-21).

use cost::cmd_cost;
use session_json::{
    compact_session, enrich_history_entry, enrich_search_hit, full_session,
    search_hit_matches_project,
};
use session_ref::resolve_session_reference_lightweight;

pub mod watch;

/// First arguments that make the app binary run the CLI instead of the GUI.
///
/// Every subcommand must be listed, or the desktop binary launches the GUI
/// for it (v0.8.1 shipped that bug for `cost`). Engine command
/// names are listed only in engine builds, except the offline `api schema`.
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
    "api",
    "spawn",
    "send",
    "mail",
    "cost",
    "usage-bridge",
    "hooks",
    "help",
];

/// Engine command names in builds without the engine, `task`, which moved
/// to `c9watch-agent` (C1a-21), and the removed PM orchestration commands.
/// They stay out of `CLI_COMMANDS`, but `is_cli_invocation` still routes
/// them to Clap, so the user gets an unknown-command error instead of the
/// GUI.
pub const RESERVED_ENGINE_COMMANDS: &[&str] = &[
    "task",
    "engine",
    "export",
    "integration",
    "auth",
    "annotate",
    "workers",
    "inbox",
    "adopt",
    "daemon",
];

/// Whether `first` (the first argument) selects the CLI.
pub fn is_cli_invocation(first: &str) -> bool {
    CLI_COMMANDS.contains(&first)
        || RESERVED_ENGINE_COMMANDS.contains(&first)
        || matches!(first, "--help" | "-h" | "--version" | "-V")
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

/// How a detection command (`list`, `status`, `self`, `watch`, `wait`)
/// finds sessions. An engine build adds `--direct`; in other builds it
/// holds nothing, so the release CLI is unchanged.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct DetectArgs {}

impl DetectArgs {}

/// `spawn --mode` (C1a-38). An engine build adds the flag; in other builds
/// this holds nothing.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct SpawnModeArgs {}

/// `wait --event turn_ended --agent X` (C1a-37). An engine build adds the
/// flags; in other builds it holds nothing, so the release CLI is
/// unchanged.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct WaitEventArgs {}

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

        #[command(flatten)]
        detect: DetectArgs,
    },

    /// Aggregate status summary of all active sessions
    Status {
        /// Filter by project path (substring match)
        #[arg(long)]
        project: Option<String>,

        #[command(flatten)]
        detect: DetectArgs,
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
    SelfId {
        #[command(flatten)]
        detect: DetectArgs,
    },

    /// Stop a session by PID (kills the corresponding Claude process).
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

        #[command(flatten)]
        detect: DetectArgs,
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

        #[command(flatten)]
        detect: DetectArgs,

        #[command(flatten)]
        event: WaitEventArgs,
    },

    /// Show tasks/todos for a session
    Tasks {
        /// Session key (`provider:id`), session ID, or unique prefix
        session_id: String,
    },

    /// Export the embedded schema, or send a raw c9p/1 request (engine build)
    Api {
        /// Method name, e.g. `hello`
        method: String,
        /// Params as one JSON value
        #[arg(default_value = "{}")]
        params: String,
        /// Omit private methods, topics, fields and types (schema only)
        #[arg(long = "public")]
        public_only: bool,
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
            detect,
        } => cmd_list(
            project.as_deref(),
            status.as_deref(),
            compact,
            cli.pretty,
            &detect,
        ),
        Commands::Status { project, detect } => cmd_status(project.as_deref(), cli.pretty, &detect),
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
        Commands::SelfId { detect } => cmd_self(cli.pretty, &detect),
        Commands::Stop { target } => cmd_stop(&target, cli.pretty),
        Commands::Watch {
            interval,
            project,
            compact,
            changes_only,
            json: _,
            detect,
        } => cmd_watch(interval, project.as_deref(), compact, changes_only, &detect),
        Commands::Wait {
            session_id,
            until,
            timeout,
            interval,
            detect,
            event,
        } => match wait_command(session_id, until, timeout, interval, &detect, &event) {
            Ok(0) => Ok(()),
            Ok(code) => process::exit(code),
            Err(e) => Err(e),
        },
        Commands::Tasks { session_id } => cmd_tasks(&session_id, cli.pretty),
        Commands::Api {
            method,
            params,
            public_only,
        } => (|| {
            if method == "schema" {
                if params != "{}" {
                    return Err("api schema takes no params".into());
                }
                api_schema::print(public_only)
            } else {
                if public_only {
                    return Err("--public is only valid for api schema".into());
                }
                {
                    Err("Raw API calls need an engine build; api schema works offline".into())
                }
            }
        })(),
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
    detect: &DetectArgs,
) -> Result<(), String> {
    let _ = detect;
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

fn cmd_status(
    project_filter: Option<&str>,
    pretty: bool,
    detect: &DetectArgs,
) -> Result<(), String> {
    let _ = detect;
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
fn cmd_self(pretty: bool, detect: &DetectArgs) -> Result<(), String> {
    let my_pid = std::process::id();

    // Walk up the process tree to find a claude ancestor
    let claude_pid = find_claude_ancestor(my_pid)
        .ok_or("No claude parent process found — this command must be called from within a Claude Code session")?;

    let _ = detect;

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

    Err(format!(
        "invalid stop target '{}': expected a numeric PID",
        target
    ))
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
    detect: &DetectArgs,
) -> Result<(), String> {
    let _ = detect;
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

/// One read of the live sessions for `wait` (and the engine's `watch`):
/// the snapshots and the payloads their event lines take.
pub(crate) enum Poll<L> {
    Live(Vec<watch::Snapshot>, L),
    /// This read failed; a later one may work.
    Missed,
    /// No read will work again.
    #[allow(dead_code)]
    Failed(String),
}

/// `c9watch wait` as parsed: the session form, or in an engine build
/// `--event turn_ended --agent X` (C1a-37).
fn wait_command(
    session_id: String,
    until: watch::Until,
    timeout_secs: Option<u64>,
    interval_secs: u64,
    detect: &DetectArgs,
    _event: &WaitEventArgs,
) -> Result<i32, String> {
    cmd_wait(&session_id, until, timeout_secs, interval_secs, detect)
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
    detect: &DetectArgs,
) -> Result<i32, String> {
    let _ = detect;
    use std::sync::mpsc;
    use std::time::Duration;

    let interval = Duration::from_secs(interval_secs.max(1));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || loop {
        let poll = match poll_sessions(None) {
            Some((snapshots, sessions)) => Poll::Live(snapshots, sessions),
            None => Poll::Missed,
        };
        if tx.send(poll).is_err() {
            return;
        }
        std::thread::sleep(interval);
    });
    wait_loop(reference, until, timeout_secs, rx, |change, sessions| {
        change_line(change, sessions, true)
    })
}

/// The `wait` loop over `polls`, from in-process detection or the engine.
/// `line` makes the event line printed when the condition is met.
fn wait_loop<L>(
    reference: &str,
    until: watch::Until,
    timeout_secs: Option<u64>,
    polls: std::sync::mpsc::Receiver<Poll<L>>,
    line: impl Fn(&watch::Change, &L) -> serde_json::Value,
) -> Result<i32, String> {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};

    let deadline = timeout_secs.map(|t| Instant::now() + Duration::from_secs(t));
    let mut key: Option<String> = None;
    let mut prev = std::collections::HashMap::new();
    loop {
        let poll = match deadline {
            Some(deadline) => {
                match polls.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(poll) => poll,
                    Err(RecvTimeoutError::Timeout) => return Ok(watch::EXIT_TIMEOUT),
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err("The session poller stopped".to_string())
                    }
                }
            }
            None => polls
                .recv()
                .map_err(|_| "The session poller stopped".to_string())?,
        };
        let (snapshots, payload) = match poll {
            Poll::Live(snapshots, payload) => (snapshots, payload),
            Poll::Missed if key.is_none() => {
                return Err("Failed to read the live sessions".to_string())
            }
            Poll::Missed => continue,
            Poll::Failed(e) => return Err(e),
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
                print_line(&line(&change, &payload));
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

pub(crate) use crate::session::tasks::read_session_tasks;

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
    fn offline_schema_routes_to_cli_in_every_build() {
        assert!(is_cli_invocation("api"));
        let cli = Cli::try_parse_from(["c9watch", "api", "schema", "--public"]).unwrap();
        assert!(
            matches!(cli.command, Commands::Api { method, public_only: true, .. } if method == "schema")
        );
    }

    #[test]
    fn wait_routes_to_the_cli_and_the_gui_still_launches_without_args() {
        assert!(is_cli_invocation("wait"));
        assert!(is_cli_invocation("--version"));
        assert!(!is_cli_invocation("--some-tauri-flag"));
        assert!(!is_cli_invocation(""));
    }

    #[test]
    fn removed_pm_commands_get_a_cli_error_not_the_gui() {
        for name in ["workers", "inbox", "adopt", "daemon"] {
            assert!(is_cli_invocation(name), "{}", name);
            assert!(Cli::try_parse_from(["c9watch", name]).is_err(), "{}", name);
        }
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
            ..
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

    /// `wait --event turn_ended --agent X` exists only in engine builds
    /// (C1a-37), and replaces the session form instead of adding to it.
    #[test]
    fn wait_event_is_an_engine_build_form() {
        let event = ["c9watch", "wait", "--event", "turn_ended", "--agent", "w"];
        let parsed = Cli::try_parse_from(event);
        assert!(parsed.is_err(), "wait --event parsed in a release CLI");
    }

    /// `--direct` exists only in engine builds, on the five detection
    /// commands (C1a-36); a release CLI rejects it.
    #[test]
    fn direct_is_an_engine_build_flag() {
        let with_direct: [&[&str]; 5] = [
            &["c9watch", "list", "--direct"],
            &["c9watch", "status", "--direct"],
            &["c9watch", "self", "--direct"],
            &["c9watch", "watch", "--direct"],
            &["c9watch", "wait", "abc", "--until", "done", "--direct"],
        ];
        for args in with_direct {
            let parsed = Cli::try_parse_from(args);
            assert!(parsed.is_err(), "{args:?} parsed in a release CLI");
        }
        assert!(Cli::try_parse_from(["c9watch", "view", "abc", "--direct"]).is_err());
    }

    /// A release CLI (`--features cli`) has no engine commands (v3.1 M-a).
    #[test]
    fn cli_build_has_no_engine_commands() {
        for name in ["engine", "export", "task", "integration"] {
            assert!(
                !CLI_COMMANDS.contains(&name),
                "{name} in a non-engine build"
            );
            assert!(Cli::try_parse_from(["c9watch", name]).is_err());
            // Clap reports the unknown command; the GUI does not launch.
            assert!(is_cli_invocation(name), "{name} would launch the GUI");
        }
    }
}
