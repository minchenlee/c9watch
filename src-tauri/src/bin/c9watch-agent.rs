//! `c9watch-agent`: what an engine agent runs to talk to the c9 engine
//! (C1a-21, D28 Q6=B). Three commands, and no `api`:
//!
//!   c9watch-agent hook <event> [--kind claude]   a Claude Code hook
//!   c9watch-agent task done --report <text|->    the task is finished
//!   c9watch-agent handoff --note <text|->        notes for the next generation
//!
//! The engine copies this binary to `<home>/bin/c9watch-agent` and accepts
//! hook-class calls only from that copy (`engine::agent_bin`). The logic
//! lives in `c9watch_lib::cli::task`.

#[cfg(unix)]
use clap::{Parser, Subcommand};

#[cfg(unix)]
#[derive(Parser)]
#[command(name = "c9watch-agent", version, about = "Agent side of the c9 engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[cfg(unix)]
#[derive(Subcommand)]
enum Command {
    /// Send one hook event to the engine. Always exits 0.
    Hook {
        /// session-start, stop, permission-request, permission-denied or
        /// notification
        event: String,
        /// The adapter kind of the hook
        #[arg(long, default_value = c9watch_lib::cli::task::DEFAULT_HOOK_KIND)]
        kind: String,
    },
    /// Report the agent's task to the engine
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },
    /// Leave a note for the agent's next generation
    Handoff {
        /// Note text, or `-` to read it from stdin
        #[arg(long)]
        note: String,
    },
}

#[cfg(unix)]
#[derive(Subcommand)]
enum TaskAction {
    /// Send the final report (needs C9_ENGINE_SOCK and C9_AGENT_ID)
    Done {
        /// Report text, or `-` to read it from stdin
        #[arg(long)]
        report: String,
    },
}

#[cfg(unix)]
fn main() {
    use c9watch_lib::cli::task;
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // A hook never fails the vendor's hook, not even on bad args.
            if std::env::args().nth(1).as_deref() == Some("hook")
                && !matches!(
                    e.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                )
            {
                std::process::exit(0);
            }
            e.exit();
        }
    };
    let result = match cli.command {
        Command::Hook { event, kind } => {
            if let Some(out) = task::run_hook(
                &event,
                &kind,
                std::io::stdin(),
                |k| std::env::var(k).ok(),
                task::hook_timeout(&event),
            ) {
                println!("{}", out);
            }
            // Leaves a call stuck past the deadline behind.
            std::process::exit(0);
        }
        Command::Task {
            action: TaskAction::Done { report },
        } => task::cmd_task_done(&report),
        Command::Handoff { note } => task::cmd_handoff(&note),
    };
    if let Err(e) = result {
        eprintln!("c9watch-agent: {}", e);
        std::process::exit(1);
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("c9watch-agent runs only on unix");
    std::process::exit(2);
}
