// Prevents additional console window on Windows in release — GUI builds only.
// CLI-only builds need the console for stdout/stderr output.
#![cfg_attr(all(not(debug_assertions), feature = "gui"), windows_subsystem = "windows")]

fn main() {
    // CLI mode: if the first arg is a known subcommand or --help/--version,
    // route to the CLI handler instead of launching the GUI.
    #[cfg(feature = "cli")]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.len() > 1 {
            let first = args[1].as_str();
            #[allow(unused_mut)]
            let mut known_commands: Vec<&str> = vec![
                "list", "status", "self", "view", "history", "search", "stop", "watch", "tasks",
                "cost", "help",
            ];
            // PM/worker orchestration subcommands are opt-in only.
            #[cfg(feature = "pm-orchestration")]
            known_commands.extend_from_slice(&[
                "spawn", "send", "workers", "inbox", "adopt", "daemon",
            ]);
            let is_cli = known_commands.contains(&first)
                || first == "--help"
                || first == "-h"
                || first == "--version"
                || first == "-V";
            if is_cli {
                use clap::Parser;
                let cli = c9watch_lib::cli::Cli::parse();
                c9watch_lib::cli::run(cli);
                return;
            }
        }

        // CLI-only build: no GUI available, show help if no args
        #[cfg(not(feature = "gui"))]
        {
            use clap::Parser;
            let cli = c9watch_lib::cli::Cli::parse();
            c9watch_lib::cli::run(cli);
            return;
        }
    }

    // Launch the GUI app
    #[cfg(feature = "gui")]
    c9watch_lib::run();
}
