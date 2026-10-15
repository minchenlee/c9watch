// Prevents additional console window on Windows in release — GUI builds only.
// CLI-only builds need the console for stdout/stderr output.
#![cfg_attr(
    all(not(debug_assertions), feature = "gui"),
    windows_subsystem = "windows"
)]

fn main() {
    c9watch_lib::claude_path::ensure_claude_on_path();

    // CLI mode: if the first arg is a known subcommand or --help/--version,
    // route to the CLI handler instead of launching the GUI.
    #[cfg(feature = "cli")]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.len() > 1 {
            // `CLI_COMMANDS` lists every subcommand (a test enforces it).
            if c9watch_lib::cli::is_cli_invocation(&args[1]) {
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
