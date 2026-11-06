//! `c9watch-agent`: what an engine agent runs to talk to the c9 engine
//! (C1a-21, D28 Q6=B). These commands, and no `api`:
//!
//!   c9watch-agent hook <event> [--kind claude]   a Claude Code hook
//!   c9watch-agent task done --report <text|->    the task is finished
//!   c9watch-agent handoff --note <text|->        notes for the next generation
//!   c9watch-agent child create --params <json|-> a child agent (E5)
//!   c9watch-agent child retire <id>              retire a descendant (E5)
//!
//! The engine copies this binary to `<home>/bin/c9watch-agent` and accepts
//! hook-class calls only from that copy (`engine::agent_bin`). The logic
//! lives in `c9watch_core::cli::task`.

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
    /// Send one hook event to the engine. Exits 0, except pre-tool-use,
    /// which exits 2 (blocks the call) unless the engine allows it.
    Hook {
        /// session-start, stop, permission-request, permission-denied,
        /// notification, prompt-submit, post-tool-use or pre-tool-use
        event: String,
        /// The adapter kind of the hook
        #[arg(long, default_value = c9watch_core::cli::task::DEFAULT_HOOK_KIND)]
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
    /// Create or retire this agent's own children
    Child {
        #[command(subcommand)]
        action: ChildAction,
    },
}

#[cfg(unix)]
#[derive(Subcommand)]
enum ChildAction {
    /// Create a child: its guard must be at least as strict as this
    /// agent's, and its cwd inside this agent's write area
    Create {
        /// The agent.create params as JSON (mode, cwd, guard, name,
        /// require), or `-` to read them from stdin
        #[arg(long)]
        params: String,
        /// Run the create once per key; a retry replays its result
        #[arg(long)]
        idem: Option<String>,
    },
    /// Retire a child, or a child of one
    Retire {
        /// The agent id (`agt_<ulid>`)
        id: String,
        /// Run the retire once per key
        #[arg(long)]
        idem: Option<String>,
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
    use c9watch_core::cli::task;
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // A hook never fails the vendor's hook, not even on bad args;
            // pre-tool-use fails closed.
            let mut args = std::env::args().skip(1);
            let hook = args.next().as_deref() == Some("hook");
            if hook && args.next().as_deref() == Some(task::PRE_TOOL_USE) {
                eprintln!("c9watch guard: bad hook args, so the write is blocked");
                std::process::exit(2);
            }
            if hook
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
        Command::Hook { event, kind } if event == task::PRE_TOOL_USE => {
            let code = match task::run_pre_tool_use(
                &kind,
                std::io::stdin(),
                |k| std::env::var(k).ok(),
                task::PRE_TOOL_USE_TIMEOUT,
            ) {
                Ok(()) => 0,
                Err(reason) => {
                    eprintln!("{}", reason);
                    2
                }
            };
            // Leaves a call stuck past the deadline behind.
            std::process::exit(code);
        }
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
        Command::Child {
            action: ChildAction::Create { params, idem },
        } => task::cmd_child_create(&params, idem),
        Command::Child {
            action: ChildAction::Retire { id, idem },
        } => task::cmd_child_retire(&id, idem),
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
