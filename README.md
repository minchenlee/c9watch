<p align="center">
  <img src="src-tauri/icons/icon.png" width="120" alt="c9watch icon" />
</p>

<h1 align="center">c9watch</h1>

<p align="center">Monitor and control all your Claude Code sessions — built for both humans and agents.</p>

**c9watch** (short for **c**laude cod**e** watch, like k8s for Kubernetes) gives you a real-time view of every Claude Code session running on your machine. A **desktop dashboard** for you, and a **JSON CLI** for your agents — both watching the same sessions at the same time.

You see which agent needs permission, which one is working, and which one is idle. Your agents can do the same — querying session status, searching past work, and coordinating with each other — all through the same tool.

## Demo

![Demo](docs/screenshots/demo.gif)

## Works with everything. Tied to nothing.

Unlike other Claude Code management tools that require you to launch sessions from within their app, **c9watch doesn't care where you start your sessions**. It discovers them automatically by scanning running processes at the OS level.

Start Claude Code from any terminal or IDE you already use -- VS Code, Zed, iTerm2, Antigravity, you name it -- and c9watch picks them all up. No plugins to install. No workflows to change. No vendor lock-in.

Just open c9watch and see everything.

## Lightweight and fast.

Built with **Tauri**, **Rust**, and **Svelte** -- not Electron. The app binary is small, memory usage is minimal, and the UI stays snappy. Rust handles process scanning and file parsing at native speed. Svelte compiles away the framework overhead. You're already running multiple Claude Code agents eating up resources -- your monitoring tool shouldn't add to the pile.

## Install

### Desktop app (macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/minchenlee/c9watch/main/install.sh | bash
```

This installs the app to `/Applications` and symlinks the CLI to `~/.local/bin/c9watch`, so both the GUI and `c9watch list`, `c9watch view`, etc. work out of the box.

Or grab the latest `.dmg` from the [Releases](https://github.com/minchenlee/c9watch/releases) page (you'll need to symlink the CLI manually: `ln -s /Applications/c9watch.app/Contents/MacOS/c9watch ~/.local/bin/c9watch`).

### CLI only (macOS & Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/minchenlee/c9watch/main/install-cli.sh | bash
```

Installs to `~/.local/bin` by default. Use `| bash -s -- --global` to install to `/usr/local/bin` instead.

### Build from source

Prerequisites: [Rust](https://rustup.rs/), [Node.js](https://nodejs.org/) (v18+), and the [Tauri CLI](https://v2.tauri.app/start/prerequisites/).

```bash
git clone https://github.com/minchenlee/c9watch.git
cd c9watch
npm install
npm run tauri build       # Desktop app → src-tauri/target/release/bundle/macos/
```

To build just the CLI (no Node.js or Tauri CLI needed):

```bash
cd src-tauri
cargo build --release --no-default-features --features cli
# Binary → target/release/c9watch
```

## Screenshots

### Monitor -- see what needs your attention first

Sessions grouped by status. Permission requests surface to the top so you never leave an agent stuck waiting.

![Monitor tab](docs/screenshots/monitor-tab.png)

### History -- browse and search past sessions

Search all past sessions with instant metadata filter and deep content search. Click a result to view the full conversation.

![History tab](docs/screenshots/history-tab.png)

### Cost -- track your spending

Daily, per-project, and per-model spending breakdowns across all your Claude Code sessions.

![Cost tab](docs/screenshots/cost-tab.png)

### Conversation viewer -- inspect any session

Expand any session to see the full conversation with formatted markdown, code blocks, inline images, and a navigation sidebar.

![Conversation viewer](docs/screenshots/conversation-viewer.png)

### Tray popover -- monitor without opening the dashboard

A quick-glance overlay showing all active sessions and their status directly from the menu bar.

![Tray popover](docs/screenshots/tray-popover.jpeg)

### Memory -- browse Claude Code memory files

View and inspect all Claude Code memory files in a two-panel layout with quick access to Claude commands.

![Memory tab](docs/screenshots/memory-tab.png)

### Token distance -- visualize your usage

See your total token usage as a rice stack towering past real-world landmarks. Share the result as an Instagram-ready PNG.

![Token distance visualizer](docs/screenshots/token-distance-visualizer.png)

## CLI

The same `c9watch` binary doubles as a CLI for scriptable session management. All output is JSON, designed for piping into `jq` or consumption by other coding agents.

```bash
# List active sessions
c9watch list
c9watch list --project myapp --status Working --compact

# Aggregate status summary
c9watch status

# View a conversation (prefix matching works)
c9watch view abc123 --last 5 --pretty

# Browse and search history
c9watch history -n 20
c9watch search "fix the auth bug" --project myapp

# Identify the calling agent's own session
c9watch self

# Stop a session
c9watch stop 12345

# Watch for status changes (NDJSON stream)
c9watch watch --interval 2 --compact --changes-only

# View tasks/todos for a session
c9watch tasks abc123

# Cost breakdown, including per-session lookup
c9watch cost --daily
c9watch cost --session abc12345 --pretty
c9watch cost --session-prefix abc

# PM orchestration: spawn and manage child worker sessions
c9watch spawn --name my-worker --cwd ../my-worktree --append-system-prompt "..."
c9watch send <session-id> --message "do this task"
c9watch workers
c9watch inbox --pretty
```

Use `--pretty` on any command for human-readable JSON. The `watch` command streams newline-delimited JSON events (`started`, `status_changed`, `stopped`) for real-time monitoring.

c9watch ships with a [Claude Code skill](skills/c9watch-cli/SKILL.md) that teaches Claude how to use the CLI. Install it to let Claude Code monitor sibling sessions, search past work, and coordinate with other agents:

```bash
ln -s /path/to/c9watch/skills/c9watch-cli ~/.claude/skills/c9watch-cli
```

See [SKILLS.md](SKILLS.md) for more install options.

## Features

- **Zero-integration setup** -- Works with any terminal or IDE, no plugins or extensions required
- **Auto-discovery** -- Detects all running Claude Code sessions by scanning processes at the OS level
- **Real-time status** -- See at a glance which sessions are Working, Need Attention (permission requests or user questions), or Idle
- **Conversation viewer** -- Expand any session to view the full conversation with formatted markdown, code blocks, and inline images
- **Session control** -- Stop sessions, open their parent terminal/IDE, or rename them for easier tracking
- **Multi-project view** -- Sessions grouped by project with git branch info
- **Tray popover** -- Click the menu bar icon for a quick-glance overlay with session status indicators and latest messages
- **Status notifications** -- Get a native macOS notification when a session needs your attention
- **Mobile/Web client** -- Connect from any browser or mobile device via WebSocket; scan the QR code to monitor sessions remotely
- **Session history** -- Browse and search all past sessions with instant metadata filter and deep content search; click a result to scroll to and highlight the matching message
- **Memory viewer** -- Browse and inspect Claude Code memory files with a two-panel layout and quick Claude command access
- **Cost tracker** -- Track Claude Code spending with daily, per-project, and per-model breakdowns; click any session to preview the conversation; sort by date or cost
- **CLI for agents** -- `c9watch list`, `view`, `history`, `search`, `stop`, `watch`, `cost` commands for scriptable session management and agent-to-agent monitoring
- **PM orchestration** -- Spawn, message, and manage child Claude Code "worker" sessions from a parent "PM" session via `c9watch spawn`, `send`, `workers`, `adopt`, `inbox`, `tasks`. WORKER and PM badges visible on session cards; PMs show a Workers panel in the overlay
- **PM-orchestration on subscription quota** — c9watch workers run as
  `claude --bg` background-pinned sessions, billed against your Pro/Max
  chat quota (not the separate Agent SDK credit pool that took effect
  2026-06-15). Falls back to legacy `--print` mode on older CC.
- **Subagent visibility** -- Detect and display Task-tool subagents spawned inside any session, with click-to-preview transcripts
- **Entry animations** -- Staggered cascade transitions across tabs and overlay panels for fluid navigation
- **Token distance visualizer** -- See your token usage as a rice stack towering past 22 real-world landmarks, with animated stacking, native share sheet, and Instagram-ready PNG export
- **Debug console** -- Hidden diagnostic panel (`Cmd+Shift+D`) for troubleshooting session detection issues

## How it works

**Live monitoring** -- A background thread polls every 2 seconds, scanning for running `claude` processes using `sysinfo`. Each process is matched to its session file in `~/.claude/projects/` via path encoding and timestamp correlation. The last N entries of each session's JSONL file are parsed to determine status:
- **Working** -- Claude is generating a response or executing tools
- **Needs Attention** -- A tool requires user approval, or Claude is asking the user a question
- **Idle** -- Session is waiting for your next prompt

Status updates are pushed to the Svelte frontend via Tauri events. The UI reactively updates, sorting sessions by priority (permission requests surface first).

**Session history** -- Reads `~/.claude/history.jsonl` for the session index, then scans individual JSONL files across all project directories for deep content search. Results link back to the full conversation viewer with scroll-to-match highlighting.

**Cost tracking** -- Parses assistant message metadata from JSONL files to extract model usage and token counts. Costs are computed using per-model pricing tables and cached by file mtime to avoid re-scanning unchanged sessions.

## Tech stack

| Layer | Technology |
|-------|-----------|
| Desktop framework | [Tauri 2](https://v2.tauri.app/) |
| Frontend | [SvelteKit](https://svelte.dev/) + [Svelte 5](https://svelte.dev/docs/svelte/overview) |
| Backend | Rust |
| Process discovery | [sysinfo](https://crates.io/crates/sysinfo) |
| Design system | Vercel Noir (true black, [Geist](https://vercel.com/font) fonts) |

## Development

```bash
npm install
npm run tauri dev
```

This starts both the Vite dev server (hot-reload for the frontend) and the Tauri Rust backend.

### Project structure

```
c9watch/
├── src/                        # SvelteKit frontend
│   ├── routes/
│   │   ├── (app)/              # Main dashboard (monitor, history, cost tabs)
│   │   └── popover/            # Tray popover window
│   ├── lib/
│   │   ├── components/         # Svelte components
│   │   │   ├── SessionCard     # Live session cards with status
│   │   │   ├── SessionHistory  # History browser with search
│   │   │   ├── CostTracker     # Spending dashboard
│   │   │   ├── MessageBubble   # Conversation message rendering
│   │   │   └── ...             # Overlays, nav map, status bar, etc.
│   │   ├── stores/             # Reactive state management
│   │   ├── demo/               # Demo mode with mock data
│   │   ├── api.ts              # Tauri command wrappers
│   │   └── types.ts            # TypeScript type definitions
│   └── app.css                 # Global styles (Vercel Noir theme)
├── src-tauri/                  # Rust backend (Tauri)
│   └── src/
│       ├── lib.rs              # App setup, tray icon, NSPanel popover
│       ├── polling.rs          # Background session detection loop
│       ├── actions.rs          # Stop/open session, IDE detection
│       ├── web_server.rs       # WebSocket server for mobile clients
│       ├── auth.rs             # Token generation, local IP discovery
│       └── session/
│           ├── parser.rs       # JSONL file parsing and message extraction
│           ├── detector.rs     # Process-to-session matching
│           ├── status.rs       # Status determination from JSONL entries
│           ├── history.rs      # Session history index and deep search
│           ├── cost.rs         # Cost aggregation with mtime caching
│           ├── enrichment.rs   # Session metadata enrichment (titles, first prompt)
│           ├── conversation.rs # Conversation loading and rendering
│           ├── sanitize.rs     # Strip internal XML tags from messages
│           ├── permissions.rs  # Auto-approval rule checking
│           └── custom_names.rs # User-defined session titles
```

## Demo mode

Press `Cmd+D` to toggle demo mode, which loads simulated sessions with animated status transitions. Useful for testing the UI without running real Claude Code sessions.

## Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines on:
- Setting up the development environment
- Coding standards and commit message format
- Pull request process
- Platform-specific contributions (Windows, Linux)

## Contributors

Thanks to these wonderful people who have contributed to c9watch:

<!-- ALL-CONTRIBUTORS-LIST:START - Do not remove or modify this section -->
<!-- prettier-ignore-start -->
<!-- markdownlint-disable -->
<table>
  <tbody>
    <tr>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/minchenlee"><img src="https://github.com/minchenlee.png?s=100" width="100px;" alt="Min-Chen Lee"/><br /><sub><b>Min-Chen Lee</b></sub></a><br /><a href="#code-minchenlee" title="Code">💻</a> <a href="#doc-minchenlee" title="Documentation">📖</a> <a href="#design-minchenlee" title="Design">🎨</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/cynaptic"><img src="https://github.com/cynaptic.png?s=100" width="100px;" alt="Ray Lee"/><br /><sub><b>Ray Lee</b></sub></a><br /><a href="#code-cynaptic" title="Code">💻</a> <a href="#platform-cynaptic" title="Platform">📦</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/grimmerk"><img src="https://github.com/grimmerk.png?s=100" width="100px;" alt="Grimmer Kang"/><br /><sub><b>Grimmer Kang</b></sub></a><br /><a href="#code-grimmerk" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/stanimir93"><img src="https://github.com/stanimir93.png?s=100" width="100px;" alt="Stanimir"/><br /><sub><b>Stanimir</b></sub></a><br /><a href="#code-stanimir93" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/josh-dev-cho"><img src="https://github.com/josh-dev-cho.png?s=100" width="100px;" alt="josh.dev"/><br /><sub><b>josh.dev</b></sub></a><br /><a href="#code-josh-dev-cho" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/maxyharr"><img src="https://github.com/maxyharr.png?s=100" width="100px;" alt="Max Harris"/><br /><sub><b>Max Harris</b></sub></a><br /><a href="#code-maxyharr" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/vladan-me"><img src="https://github.com/vladan-me.png?s=100" width="100px;" alt="Vladan"/><br /><sub><b>Vladan</b></sub></a><br /><a href="#code-vladan-me" title="Code">💻</a></td>
    </tr>
    <tr>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/ajonesw"><img src="https://github.com/ajonesw.png?s=100" width="100px;" alt="Alex Jones-Wolsey"/><br /><sub><b>Alex Jones-Wolsey</b></sub></a><br /><a href="#code-ajonesw" title="Code">💻</a></td>
    </tr>
  </tbody>
</table>

<!-- markdownlint-restore -->
<!-- prettier-ignore-end -->

<!-- ALL-CONTRIBUTORS-LIST:END -->

See [CONTRIBUTORS.md](CONTRIBUTORS.md) for the full list and contribution details.

## License

MIT
