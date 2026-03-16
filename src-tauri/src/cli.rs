use crate::session;
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
    List,

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
}

/// Run the CLI and return the exit code.
/// Returns Ok(()) on success, Err on failure.
pub fn run(cli: Cli) {
    let result = match cli.command {
        Commands::List => cmd_list(cli.pretty),
        Commands::View { session_id, last } => cmd_view(&session_id, last, cli.pretty),
        Commands::History { limit } => cmd_history(limit, cli.pretty),
        Commands::Search { query } => cmd_search(&query, cli.pretty),
        Commands::Stop { pid } => cmd_stop(pid, cli.pretty),
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

fn cmd_list(pretty: bool) -> Result<(), String> {
    let (sessions, diagnostics) = session::enrichment::detect_and_enrich_sessions()?;
    let output = serde_json::json!({
        "sessions": sessions,
        "diagnostics": diagnostics,
    });
    print_json(&output, pretty)
}

fn cmd_view(session_id: &str, last: Option<usize>, pretty: bool) -> Result<(), String> {
    // Support session ID prefix matching
    let resolved_id = resolve_session_id(session_id)?;
    let mut conversation = session::conversation::get_conversation_data(&resolved_id)?;

    if let Some(n) = last {
        let len = conversation.messages.len();
        if n < len {
            conversation.messages = conversation.messages.split_off(len - n);
        }
    }

    print_json(&conversation, pretty)
}

fn cmd_history(limit: Option<usize>, pretty: bool) -> Result<(), String> {
    let mut entries = session::get_history()?;
    if let Some(n) = limit {
        entries.truncate(n);
    }
    print_json(&entries, pretty)
}

fn cmd_search(query: &str, pretty: bool) -> Result<(), String> {
    if query.trim().is_empty() {
        return Err("Search query cannot be empty".to_string());
    }
    let hits = session::deep_search(query)?;
    let output = serde_json::json!({
        "query": query,
        "hits": hits,
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

/// Resolve a session ID prefix to a full session ID.
/// If the input is already a full UUID, returns it as-is.
/// If it's a prefix, finds the matching active session or searches history.
fn resolve_session_id(prefix: &str) -> Result<String, String> {
    // If it looks like a full UUID, use it directly
    if prefix.len() >= 36 {
        return Ok(prefix.to_string());
    }

    // Try to find among active sessions first
    if let Ok((sessions, _)) = session::enrichment::detect_and_enrich_sessions() {
        let matches: Vec<_> = sessions
            .iter()
            .filter(|s| s.id.starts_with(prefix))
            .collect();
        match matches.len() {
            1 => return Ok(matches[0].id.clone()),
            n if n > 1 => {
                return Err(format!(
                    "Ambiguous session ID prefix '{}' matches {} sessions",
                    prefix, n
                ))
            }
            _ => {}
        }
    }

    // Try history
    if let Ok(entries) = session::get_history() {
        let matches: Vec<_> = entries
            .iter()
            .filter(|e| e.session_id.starts_with(prefix))
            .collect();
        match matches.len() {
            1 => return Ok(matches[0].session_id.clone()),
            n if n > 1 => {
                return Err(format!(
                    "Ambiguous session ID prefix '{}' matches {} history entries",
                    prefix, n
                ))
            }
            _ => {}
        }
    }

    // Fall through: use as-is (might be a full ID for a session not in history)
    Ok(prefix.to_string())
}
