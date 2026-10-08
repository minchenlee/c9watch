# Changelog

All notable changes to c9watch are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.10.0] - 2026-10-08

### Added
- **Cursor Agent monitoring (monitor-only).** Detect Cursor Agent root and
  subagent sessions, show their lifecycle and provider metadata, and include
  them in Monitor, History, conversation search, and the provider filter
  ([#120](https://github.com/minchenlee/c9watch/pull/120)).
- **Provider-scoped session identity.** Claude Code, Codex, and Cursor session
  IDs no longer collide across detection, enrichment, CLI output,
  notifications, and frontend selection
  ([#120](https://github.com/minchenlee/c9watch/pull/120)).
- **Pi Agent monitoring (read-only).** Monitor, History, conversation, search,
  Cost, and CLI support for Pi, with provider-reported cost data. Pi sessions
  cannot be opened, stopped, or renamed from c9watch
  ([#121](https://github.com/minchenlee/c9watch/pull/121)).
- **OpenCode monitoring (preview).** Connect c9watch to one OpenCode HTTP server
  you start yourself (`opencode --port 4096`) in Settings → Integration. Adds
  session cards, an OpenCode filter and badge, parent/child grouping, and
  on-demand conversation reads. It does not discover servers on its own. See
  [docs/opencode-preview.md](docs/opencode-preview.md)
  ([#127](https://github.com/minchenlee/c9watch/pull/127)).
- **Attention inbox and source health.** A deduplicated inbox on the Monitor
  page for sessions that wait for you, a per-session health badge (`fresh`,
  `stale`, `partial`, `unavailable`, `unknown`) so an old observation no longer
  looks live, and a declared return target for attention notifications
  ([#141](https://github.com/minchenlee/c9watch/pull/141)).
- **Subscription usage indicators.** Per-provider quota meters in the toolbar and
  tray for Claude Code, Codex, and Cursor, with reset times and a Settings →
  Usage page. Claude Code needs an optional status-line bridge
  ([#126](https://github.com/minchenlee/c9watch/pull/126)).
- **Elapsed-window marker under usage meters.** A small triangle shows how far
  through each quota window you are, and the reset line shows "N% elapsed"
  ([#140](https://github.com/minchenlee/c9watch/pull/140)).
- **Native notification settings.** Choose which events notify you, brief or
  detailed content, sound, and a per-session cooldown. Reply notifications show
  a short excerpt of the assistant's reply
  ([#125](https://github.com/minchenlee/c9watch/pull/125)).
- **Full-screen chat layout** in Monitor and History, and a resizable Monitor
  sidebar ([#126](https://github.com/minchenlee/c9watch/pull/126)).
- **Session kind and entrypoint badges.** Background-pinned Claude Code sessions
  show a `BG` badge. Sessions not launched from the plain `cli` entrypoint show
  that entrypoint ([#134](https://github.com/minchenlee/c9watch/pull/134)).
- **Claude Code hook bridge (opt-in).** `c9watch hooks --install` registers async
  hooks so Claude Code reports permission prompts directly. Hooks never delay
  Claude Code or affect permission decisions. `--uninstall` removes only
  c9watch's entries. Not available on Windows. See
  [docs/claude-hooks.md](docs/claude-hooks.md)
  ([#136](https://github.com/minchenlee/c9watch/pull/136)).

### Fixed
- **Needs Attention accuracy.** Running sub-agents no longer flag their parent
  session. c9watch now uses the `waiting` status that `claude agents --json`
  reports, and merges allow rules from user and project `settings.json`
  ([#136](https://github.com/minchenlee/c9watch/pull/136)).
- **Claude Code not found when launched from Finder, the Dock, or a login
  item.** c9watch adds the usual install directories to `PATH` at startup. The
  fallback scanner no longer matches Claude Desktop helper processes, so STOP can
  no longer kill one ([#139](https://github.com/minchenlee/c9watch/pull/139)).
- **Dead sessions in `claude agents --json`.** Entries whose process is gone are
  dropped before status inference. Rows with a missing PID no longer cause a
  parse error ([#132](https://github.com/minchenlee/c9watch/pull/132)).
- **Pi sessions end when the process ends.** A killed Pi session no longer shows
  LIVE for up to 4 hours. When the process evidence is unclear, the old
  freshness rule stays ([#141](https://github.com/minchenlee/c9watch/pull/141)).
- **Cursor detection** no longer blocks on the composer database. WAL changes
  invalidate the cache, failed composer loads retry, and stale overlays clear
  ([#141](https://github.com/minchenlee/c9watch/pull/141)).
- **Cursor transcript cache.** Appends verify the cached prefix, so a rewrite
  falls back to a full parse. An incomplete final line stays pending until it is
  complete ([#120](https://github.com/minchenlee/c9watch/pull/120)).
- **Codex archive memory and cache.** Bounded archive memory, released consumed
  buffers, and a persistent cache that detects same-length and middle rewrites
  ([#122](https://github.com/minchenlee/c9watch/pull/122),
  [#123](https://github.com/minchenlee/c9watch/pull/123)).
- **Codex conversation content** is kept across filtering and polling
  ([#124](https://github.com/minchenlee/c9watch/pull/124)).
- **Rename safety across providers.** Explicit Codex and Cursor rename targets
  are rejected. A request without a provider is rejected before writing when
  the raw ID exists under another provider
  ([#120](https://github.com/minchenlee/c9watch/pull/120)).
- **OpenCode conversation loading** is single-flight and bounded, and shows an
  explicit Retry error. Window transitions complete at once in the native app,
  so a hidden window no longer stalls animations
  ([#127](https://github.com/minchenlee/c9watch/pull/127)).

### Improved
- **Lower energy use.** c9watch no longer runs `claude agents --json` on every
  poll, holds updates while a window is hidden, and animates the status bar
  with `transform` and `opacity`. The contributor measured with `powermetrics`,
  against an earlier commit in the same PR (not against v0.9.0): energy use
  fell 97% with the dashboard open and 93% with it closed. The status bar no
  longer has a per-block glow or a left-to-right ripple
  ([#138](https://github.com/minchenlee/c9watch/pull/138)).
- **Less work per poll.** Subagent detection and message counts are cached per
  file and read incrementally, so old sessions are not re-parsed
  ([#133](https://github.com/minchenlee/c9watch/pull/133)).
- **Codex archive reads** reuse a cache, and conversation tools load on demand
  ([#122](https://github.com/minchenlee/c9watch/pull/122)).
- **History** reads the first prompt with a bound and scans history off the main
  thread ([#141](https://github.com/minchenlee/c9watch/pull/141)).

### Known limits
- OpenCode is a preview. It needs a manual server connection.
- Pi is read-only. It cannot be opened, stopped, or renamed from c9watch.
- Cursor is monitor-only.
- Codex shows working and idle only. It has no approval or question state.
- Pi: one process keeps at most one transcript. If pi writes in its own project
  and then `/resume`s into another project, the resumed card shows ended until
  that pi exits.
- Pi: an idle pi in another working directory, or a process whose start time
  cannot be read, can keep a killed card alive.
- Pi: a killed card can reappear for one poll if a node or bun process cannot be
  read.

## [0.9.0] - 2026-08-16

### Added
- **Codex App and CLI monitoring.** c9watch now detects both Codex surfaces,
  displays provider-aware `CODEX` badges, and groups Codex subagents under
  their parent sessions.
- **Shared provider filtering.** The `All | Claude Code | Codex` filter now
  applies to Monitor, History, Cost, and Memory, including provider-specific
  memory files and Codex model cost estimates.
- **Claude agent metadata backend.** Use `claude agents --json` when
  available, with a legacy fallback for older Claude Code installations.
- **User-controlled updates.** The new SETTINGS tab and update banner let
  users decide when an available app update is installed.
- **TODO side panel.** Conversation previews can show the session's TODO list
  without leaving the preview overlay.
- **PM `bg` backend.** PM workers can use `claude --bg` instead of
  `claude --print`, with automatic detection for newer Claude Code versions
  and `C9WATCH_WORKER_BACKEND=bg|print|auto` override support.
- `c9watch spawn --prompt <text>` is available when the `bg` backend is active.
- Added `EventStatus::Awaiting` and `InboxEvent::awaiting()` for workers that
  are waiting for user input.

### Changed
- **PM orchestration is now disabled by default.** Normal builds omit the PM
  CLI subcommands and hide the HUMANS/WORKERS toggle, worker badges, worker
  title prefix, and Workers panel. The implementation remains opt-in through
  the `pm-orchestration` Cargo feature and `PM_ORCHESTRATION_ENABLED` UI flag.
- **History search now defaults to phrase matching** and exposes
  case-sensitive and whole-word toggles.
- **The MEMORY tab uses an accordion layout** with improved reading size and
  animation.
- Added `Cmd+1` through `Cmd+4` shortcuts for tab navigation.

### Fixed
- Preserved Codex CLI search metadata and prefixes across search results.
- Dropped history entries whose JSONL files were cleared instead of showing
  unviewable stale cards.

### Improved
- **Lower memory usage for Codex session monitoring.** Incremental rollout
  parsing and bounded monitor representations reduce peak RSS by 65.8% on a
  controlled workload while preserving full conversation and tool details.
  ([#116](https://github.com/minchenlee/c9watch/pull/116))
- Opening a session now focuses the matching macOS Terminal/iTerm2 target by
  tty, and can focus the exact Supacode tab and surface when its coordinates
  are available.
- Added a dry-run-by-default utility for cleaning stale Cargo targets from
  other registered worktrees.

## [0.8.1] - 2026-04-20

### Fixed
- **`c9watch inbox`, `cost`, `adopt` launched the GUI instead of the CLI** — `src-tauri/src/main.rs` kept a hardcoded allowlist of subcommand names for dispatching to the CLI handler. The three subcommands added in v0.8.0 (`inbox`, `adopt`) and v0.7.0 extension (`cost`) weren't added to the list, so running them on a GUI-inclusive build fell through and launched the Tauri desktop app. Added all three to the allowlist.

## [0.8.0] - 2026-04-20

### Added
- **PM orchestration CLI** — spawn, send messages to, and manage child Claude Code worker sessions from a parent "PM" session. New commands: `c9watch spawn`, `send`, `workers`, `stop`, `adopt`, `inbox`, `tasks`. Workers show a "WORKER" badge on session cards; PMs show a "PM" badge with a Workers panel in the expanded overlay. ([#84](https://github.com/minchenlee/c9watch/pull/84))
- **Subagent visibility** — detect and display Task-tool subagents spawned inside any session. 2-row card layout with click-to-preview transcript and async completion resolution. ([#92](https://github.com/minchenlee/c9watch/pull/92))
- **Entry animations** — three-tier cascade pacing across MONITOR, HISTORY, MEMORY, and COST tabs plus overlay panel fly-ins. History preloaded at startup to avoid tab-switch lag. ([#93](https://github.com/minchenlee/c9watch/pull/93))
- **Cost per session** — new `c9watch cost --session <id>` and `--session-prefix <any>` CLI flags for per-session breakdown in JSON. ([#94](https://github.com/minchenlee/c9watch/pull/94))
- **USD/TOKENS toggle** in cost tab plus per-card cost pill on SessionCard and overlay charts. ([#85](https://github.com/minchenlee/c9watch/pull/85))
- **CLI auto-symlink** — installing the desktop app now symlinks the `c9watch` CLI into `~/.local/bin/` automatically. ([#83](https://github.com/minchenlee/c9watch/pull/83))

### Fixed
- **App icon padding** — added ~100px inset to `icon.png` master so macOS Sequoia no longer oversizes it in the Dock. Master restored to 1024×1024 after Tauri CLI downscale. ([#97](https://github.com/minchenlee/c9watch/pull/97))
- **debug_log test races** — serialized ring-buffer tests to prevent flaky CI. ([#82](https://github.com/minchenlee/c9watch/pull/82))

### Improved
- **Pink agent accent + de-duped nav header** — agent sessions now use pink for visual distinction; overlay navigation header consolidated. ([#90](https://github.com/minchenlee/c9watch/pull/90))
- **Shared UI utilities** — extracted `time-utils.ts`, `status-utils.ts`, unified preview state, and usage-stats helper from PR #92 for reuse across cards and overlay. ([#96](https://github.com/minchenlee/c9watch/pull/96))
- **Removed leaked internal planning docs** — repo now ignores `docs/superpowers/` by default. ([#89](https://github.com/minchenlee/c9watch/pull/89))

## [0.7.0] - 2026-04-06

### Added
- CLI for scriptable session management — `c9watch list`, `view`, `history`, `search`, `stop`, `watch` commands for agent-to-agent monitoring ([#75](https://github.com/minchenlee/c9watch/pull/75))
- Cost records split by date for accurate daily totals — sessions spanning midnight now attribute costs to the correct day ([#78](https://github.com/minchenlee/c9watch/pull/78))
- Session names in cost tab — display custom title or first user message alongside session ID ([#79](https://github.com/minchenlee/c9watch/pull/79))
- Conversation preview in cost tab — click any session row to open the conversation overlay ([#79](https://github.com/minchenlee/c9watch/pull/79))
- DATE/COST sort toggles in cost tab with ascending/descending order ([#79](https://github.com/minchenlee/c9watch/pull/79))
- History tab shows latest prompt text and native custom titles from JSONL files ([#80](https://github.com/minchenlee/c9watch/pull/80))

### Fixed
- Session detection on macOS now uses cmd args instead of binary path for more reliable process matching ([#77](https://github.com/minchenlee/c9watch/pull/77))
- PID-to-session mapping after `/clear` now uses session metadata for accuracy ([#73](https://github.com/minchenlee/c9watch/pull/73))
- Notification title now uses renamed session title instead of generic text ([#74](https://github.com/minchenlee/c9watch/pull/74))

### Improved
- History tab layout redesigned with CSS grid for better alignment ([#80](https://github.com/minchenlee/c9watch/pull/80))
- Session count shown per project in cost tab ([#79](https://github.com/minchenlee/c9watch/pull/79))

## [0.6.0] - 2026-03-23

### Added
- Session metadata improvements — richer session info display ([#65](https://github.com/minchenlee/c9watch/pull/65))
- NeedsPermission renamed to NeedsAttention with user question detection — sessions now surface when Claude asks the user a question, not just on permission requests ([#66](https://github.com/minchenlee/c9watch/pull/66))
- Draggable title bar and mobile responsive styling improvements ([#58](https://github.com/minchenlee/c9watch/pull/58))
- 5 new token distance milestones: Angel Falls, Mt. Vesuvius, Krubera Cave, Mt. Olympus, Mt. Etna ([#63](https://github.com/minchenlee/c9watch/pull/63))

### Fixed
- Cost pricing updated — Opus 4.5/4.6 corrected to $5/$25 (standard) and $30/$150 (fast), Haiku 4.5 to $1/$5; added cache versioning for automatic invalidation ([#64](https://github.com/minchenlee/c9watch/pull/64))
- Session titles no longer forced to uppercase with pixel font — improves readability of custom titles and prompts ([#67](https://github.com/minchenlee/c9watch/pull/67))
- History "newest" sort now uses last activity time instead of creation time ([#68](https://github.com/minchenlee/c9watch/pull/68))
- JetBrains IDE "Open" action now focuses existing window instead of opening a new one ([#69](https://github.com/minchenlee/c9watch/pull/69))

### Improved
- Website SEO & AEO optimization ([#59](https://github.com/minchenlee/c9watch/pull/59))

## [0.5.0] - 2026-03-14

### Added
- Memory tab with two-panel viewer for browsing Claude Code memory files and Claude command integration ([#41](https://github.com/minchenlee/c9watch/pull/41))
- FDA permission banner — heuristic detection when Full Disk Access is missing, with deep-link to System Settings ([#48](https://github.com/minchenlee/c9watch/pull/48))
- Debug console (`Cmd+Shift+D`) — hidden panel showing real-time diagnostic logs for troubleshooting session detection ([#48](https://github.com/minchenlee/c9watch/pull/48))
- Custom title and ACTIVE badge display in history tab ([#52](https://github.com/minchenlee/c9watch/pull/52))
- Multi-word AND search in history — search terms are combined with AND logic for more precise results ([#51](https://github.com/minchenlee/c9watch/pull/51))
- List item numbers in history session rows ([#50](https://github.com/minchenlee/c9watch/pull/50))
- Restore minimized terminal windows when clicking Open on a session ([#49](https://github.com/minchenlee/c9watch/pull/49))
- Thinking toggle restored in conversation preview ([#45](https://github.com/minchenlee/c9watch/pull/45))
- Product website at c9watch.mclee.dev ([#42](https://github.com/minchenlee/c9watch/pull/42))
- Website migrated to Starlight documentation framework ([#54](https://github.com/minchenlee/c9watch/pull/54))
- Token distance visualizer — full-screen animated overlay that converts token usage into a rice stack height with 17 real-world landmark milestones, native share sheet, and Instagram-ready PNG export ([#62](https://github.com/minchenlee/c9watch/pull/62))

### Fixed
- Path encoding mismatch — dots in directory names now correctly encoded as dashes for session matching ([#57](https://github.com/minchenlee/c9watch/pull/57))
- Path encoding aligned with Claude Code's algorithm — all non-alphanumeric characters replaced with dashes ([#48](https://github.com/minchenlee/c9watch/pull/48))
- Sliding window rendering for large conversations — prevents DOM overload ([#53](https://github.com/minchenlee/c9watch/pull/53))
- Cloudflare Workers deploy configuration for website ([#43](https://github.com/minchenlee/c9watch/pull/43))

## [0.4.0] - 2026-02-28

### Added
- Session history search tab — browse and search all past Claude Code sessions with instant metadata filter + debounced deep content search ([#33](https://github.com/minchenlee/c9watch/pull/33))
- Full conversation viewer overlay for history sessions with message rendering, tool toggle, message nav sidebar, and copyable RESUME command chip ([#33](https://github.com/minchenlee/c9watch/pull/33))
- Collapsible project groups in history BY PROJECT view with collapse/expand all ([#33](https://github.com/minchenlee/c9watch/pull/33))
- Search result snippets with keyword highlighting ([#33](https://github.com/minchenlee/c9watch/pull/33))
- Click a deep search result to scroll to and highlight the matching message in the conversation viewer ([#36](https://github.com/minchenlee/c9watch/pull/36))
- Inline image rendering for screenshots pasted in user messages ([#38](https://github.com/minchenlee/c9watch/pull/38))
- Cost tracker dashboard tab with daily, by-project, and by-model spending views ([#34](https://github.com/minchenlee/c9watch/pull/34))
- Rust cost backend with per-model pricing tables (Sonnet, Opus, Haiku) and mtime-based caching ([#34](https://github.com/minchenlee/c9watch/pull/34))
- Tab bar in native macOS title bar area with drag region and grip dots ([#33](https://github.com/minchenlee/c9watch/pull/33))

### Improved
- Drag dots handle shows hover brightness effect for better UX feedback ([#33](https://github.com/minchenlee/c9watch/pull/33))
- Removed non-functional thinking toggle — JSONL files never contain thinking blocks ([#38](https://github.com/minchenlee/c9watch/pull/38))

### Fixed
- Search highlight blink after animation fade, wrong message highlighted on deep search, and NavMap scroll targeting wrong element ([#37](https://github.com/minchenlee/c9watch/pull/37))

## [0.3.0] - 2026-02-27

### Added
- Native tray popover with session overview — click the menu bar icon to see all sessions at a glance ([#25](https://github.com/minchenlee/c9watch/pull/25))
- JetBrains IDE support: 15 IDEs (PhpStorm, IntelliJ IDEA, WebStorm, PyCharm, GoLand, CLion, Rider, RubyMine, DataGrip, Android Studio, Aqua, Fleet, RustRover) with 3-tier path resolution via Toolbox scripts dir, user Applications, and system Applications ([#26](https://github.com/minchenlee/c9watch/pull/26))

### Improved
- Test coverage increased from 53% to 65%
- Clippy warnings resolved and rustfmt applied throughout Rust codebase

## [0.2.1] - 2026-02-18

See [releases](https://github.com/minchenlee/c9watch/releases) for earlier changelogs.
