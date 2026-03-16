// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Check if CLI subcommands were provided (e.g., `c9watch list`, `c9watch view ...`)
    // If so, run the CLI handler instead of launching the GUI.
    let args: Vec<String> = std::env::args().collect();

    // CLI mode: if there are arguments beyond the binary name and the first arg
    // looks like a known subcommand (not a Tauri internal flag starting with --)
    if args.len() > 1 && !args[1].starts_with('-') {
        let known_commands = ["list", "view", "history", "search", "stop", "watch", "tasks", "help"];
        if known_commands.contains(&args[1].as_str()) {
            use clap::Parser;
            let cli = c9watch_lib::cli::Cli::parse();
            c9watch_lib::cli::run(cli);
            return;
        }
    }

    c9watch_lib::run()
}
