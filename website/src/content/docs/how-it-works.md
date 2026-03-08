---
title: How It Works
description: Architecture and internals of c9watch — process scanning, JSONL parsing, IDE detection, and more.
---

c9watch is a native macOS desktop app built with Tauri (Rust + Svelte). It monitors Claude Code sessions by reading local files and scanning OS processes — no network calls, no API keys, no telemetry.

## Architecture overview

The app has two layers: a Rust backend that handles process scanning, file parsing, and system integration, and a Svelte frontend that reactively renders the UI. They communicate via Tauri's IPC bridge (commands and events).

### Project structure

```
c9watch/
├── src/                        # SvelteKit frontend
│   ├── routes/
│   │   ├── (app)/              # Main dashboard (monitor, history, cost tabs)
│   │   └── popover/            # Tray popover window
│   ├── lib/
│   │   ├── components/         # Svelte components
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
│           ├── permissions.rs  # Auto-approval rule checking
│           └── custom_names.rs # User-defined session titles
```

## Live monitoring

A background thread runs a polling loop every 2 seconds:

1. **Process scan** — Uses the [sysinfo](https://crates.io/crates/sysinfo) crate to find all running `claude` processes
2. **Path matching** — Each process's working directory is encoded and matched to session files in `~/.claude/projects/`
3. **JSONL parsing** — The last N entries of each session's JSONL file are parsed to extract the current conversation state
4. **Status determination** — Based on the latest messages:
   - **Working** — An assistant message is being streamed, or a tool is executing
   - **Needs Permission** — A tool call is pending with `requires_approval: true`
   - **Idle** — The last message is from the assistant and no tool is pending
5. **Event push** — Status updates are emitted as Tauri events, which the Svelte frontend subscribes to reactively

The UI sorts sessions by priority: permission requests surface first, then working sessions, then idle ones. Each session card shows metadata extracted from the JSONL: model name, project path, git branch, current tool, and elapsed time.

## IDE detection

When you click "Open" on a session, c9watch needs to find the parent terminal or IDE. It does this by:

1. Walking up the process tree from the `claude` process to find the parent application
2. Matching the parent binary name against a known list of terminals and IDEs
3. For JetBrains IDEs, checking three resolution tiers: Toolbox scripts directory, user Applications, and system Applications

Currently supported: VS Code, Zed, iTerm2, Ghostty, tmux, Terminal.app, Antigravity, and 15 JetBrains IDEs (IntelliJ IDEA, PhpStorm, WebStorm, PyCharm, GoLand, CLion, Rider, RubyMine, DataGrip, Android Studio, Aqua, Fleet, RustRover, and their CE variants).

## Session history

The history feature works in two stages:

1. **Index loading** — Reads `~/.claude/history.jsonl` to get the list of all past sessions with their metadata (project, title, timestamp)
2. **Deep search** — When you type a search query, individual session JSONL files are scanned across all project directories for matching content. This is debounced to avoid excessive I/O.

Search results include context snippets with keyword highlighting. Clicking a result opens the full conversation viewer and scrolls to the matching message with a highlight animation.

## Cost tracking

The cost tracker parses assistant message metadata from JSONL files to extract:

- Model name (claude-3.5-sonnet, claude-3-opus, claude-3-haiku, etc.)
- Input and output token counts
- Cache read/write token counts

Costs are computed using a per-model pricing table maintained in `session/cost.rs`. To avoid re-scanning unchanged files on every dashboard open, results are cached by file `mtime` — only modified files are re-parsed.

## Tray popover

The menu bar popover uses a native macOS `NSPanel` (not a web view) configured with:

- `NSWindowStyleMask.nonactivatingPanel` — doesn't steal focus from your terminal
- `NSWindowLevel.popUpMenu` — appears above full-screen apps
- Custom positioning relative to the tray icon

The popover content is a separate Svelte route (`/popover`) loaded in a second Tauri webview, with its own optimized layout for the smaller window size.

## Mobile / Web client

c9watch includes a WebSocket server (powered by `tokio-tungstenite`) that broadcasts session updates to connected clients. When you scan the QR code:

1. The app detects your local IP address
2. Generates a one-time auth token
3. Encodes both into a QR code URL
4. Your phone's browser connects via WebSocket and receives real-time updates

## Tech stack

| Layer | Technology |
|---|---|
| Desktop framework | [Tauri 2](https://v2.tauri.app/) |
| Frontend | [SvelteKit](https://svelte.dev/) + [Svelte 5](https://svelte.dev/docs/svelte/overview) |
| Backend | Rust |
| Process discovery | [sysinfo](https://crates.io/crates/sysinfo) |
| WebSocket server | [tokio-tungstenite](https://crates.io/crates/tokio-tungstenite) |
| Design system | Vercel Noir (true black, [Geist](https://vercel.com/font) fonts) |
