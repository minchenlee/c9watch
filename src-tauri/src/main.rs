// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // CLI mode: if the first arg is a known subcommand or --help/--version,
    // route to the CLI handler instead of launching the GUI.
    #[cfg(feature = "cli")]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.len() > 1 {
            let first = args[1].as_str();
            let known_commands = [
                "list", "status", "self", "view", "history", "search", "stop", "watch", "tasks",
                "help",
            ];
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
