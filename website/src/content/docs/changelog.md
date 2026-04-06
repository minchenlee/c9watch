---
title: Changelog
description: All notable changes to c9watch — release notes, new features, and bug fixes.
head:
  - tag: script
    attrs:
      type: application/ld+json
    content: '{"@context":"https://schema.org","@type":"BreadcrumbList","itemListElement":[{"@type":"ListItem","position":1,"name":"Home","item":"https://c9watch.mclee.dev"},{"@type":"ListItem","position":2,"name":"Changelog","item":"https://c9watch.mclee.dev/changelog/"}]}'
---

All notable changes to c9watch are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.7.0 — 2026-04-06

### Added

- CLI for scriptable session management — `c9watch list`, `view`, `history`, `search`, `stop`, `watch`, `self`, `status`, `tasks` commands for agent-to-agent monitoring ([#75](https://github.com/minchenlee/c9watch/pull/75))
- One-line CLI installer for macOS and Linux ([install-cli.sh](https://github.com/minchenlee/c9watch/blob/main/install-cli.sh))
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

## 0.6.0 — 2026-03-23

### Added

- Session metadata improvements — richer session info display ([#65](https://github.com/minchenlee/c9watch/pull/65))
- NeedsPermission renamed to NeedsAttention with user question detection ([#66](https://github.com/minchenlee/c9watch/pull/66))
- Draggable title bar and mobile responsive styling improvements ([#58](https://github.com/minchenlee/c9watch/pull/58))
- 5 new token distance milestones: Angel Falls, Mt. Vesuvius, Krubera Cave, Mt. Olympus, Mt. Etna ([#63](https://github.com/minchenlee/c9watch/pull/63))

### Fixed

- Cost pricing updated — Opus 4.5/4.6 corrected to $5/$25 (standard) and $30/$150 (fast), Haiku 4.5 to $1/$5 ([#64](https://github.com/minchenlee/c9watch/pull/64))
- Session titles no longer forced to uppercase with pixel font ([#67](https://github.com/minchenlee/c9watch/pull/67))
- History "newest" sort now uses last activity time instead of creation time ([#68](https://github.com/minchenlee/c9watch/pull/68))
- JetBrains IDE "Open" action now focuses existing window instead of opening a new one ([#69](https://github.com/minchenlee/c9watch/pull/69))

## 0.5.0 — 2026-03-14

### Added

- Memory tab with two-panel viewer for browsing Claude Code memory files and Claude command integration ([#41](https://github.com/minchenlee/c9watch/pull/41))
- Token distance visualizer — animated rice stack overlay with 17 real-world landmarks, native share sheet, and Instagram-ready PNG export ([#62](https://github.com/minchenlee/c9watch/pull/62))
- FDA permission banner — heuristic detection when Full Disk Access is missing, with deep-link to System Settings ([#48](https://github.com/minchenlee/c9watch/pull/48))
- Debug console (`Cmd+Shift+D`) — hidden panel showing real-time diagnostic logs for troubleshooting session detection ([#48](https://github.com/minchenlee/c9watch/pull/48))
- Custom title and ACTIVE badge display in history tab ([#52](https://github.com/minchenlee/c9watch/pull/52))
- Multi-word AND search in history — search terms are combined with AND logic for more precise results ([#51](https://github.com/minchenlee/c9watch/pull/51))
- List item numbers in history session rows ([#50](https://github.com/minchenlee/c9watch/pull/50))
- Restore minimized terminal windows when clicking Open on a session ([#49](https://github.com/minchenlee/c9watch/pull/49))
- Thinking toggle restored in conversation preview ([#45](https://github.com/minchenlee/c9watch/pull/45))
- Product website at c9watch.mclee.dev ([#42](https://github.com/minchenlee/c9watch/pull/42))
- Website migrated to Starlight documentation framework ([#54](https://github.com/minchenlee/c9watch/pull/54))

### Fixed

- Path encoding mismatch — dots in directory names now correctly encoded as dashes for session matching ([#57](https://github.com/minchenlee/c9watch/pull/57))
- Path encoding aligned with Claude Code's algorithm — all non-alphanumeric characters replaced with dashes ([#48](https://github.com/minchenlee/c9watch/pull/48))
- Sliding window rendering for large conversations — prevents DOM overload ([#53](https://github.com/minchenlee/c9watch/pull/53))
- Cloudflare Workers deploy configuration for website ([#43](https://github.com/minchenlee/c9watch/pull/43))

## 0.4.0 — 2026-03-01

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

## 0.3.0 — 2026-02-27

### Added

- Native tray popover with session overview — click the menu bar icon to see all sessions at a glance ([#25](https://github.com/minchenlee/c9watch/pull/25))
- Pixel grid status bar with sweep animation on state changes ([#25](https://github.com/minchenlee/c9watch/pull/25))
- Fullscreen space support — popover uses NSPanel to appear above fullscreen apps ([#25](https://github.com/minchenlee/c9watch/pull/25))
- JetBrains IDE support: 15 IDEs (PhpStorm, IntelliJ IDEA, WebStorm, PyCharm, GoLand, CLion, Rider, RubyMine, DataGrip, Android Studio, Aqua, Fleet, RustRover) with 3-tier path resolution via Toolbox scripts dir, user Applications, and system Applications ([#26](https://github.com/minchenlee/c9watch/pull/26))

### Improved

- Test coverage increased from 53% to 65% ([#31](https://github.com/minchenlee/c9watch/pull/31))
- Clippy warnings resolved and rustfmt applied throughout Rust codebase ([#31](https://github.com/minchenlee/c9watch/pull/31))

### Fixed

- Popover not appearing above fullscreen app Spaces ([#25](https://github.com/minchenlee/c9watch/pull/25))
- App quitting when main window is closed — tray icon now keeps app alive ([#25](https://github.com/minchenlee/c9watch/pull/25))
- "Open Dashboard" button not working after main window was closed ([#25](https://github.com/minchenlee/c9watch/pull/25))

## 0.2.1 — 2026-02-16

### Fixed

- Strip 'v' prefix from version in latest.json for updater compatibility ([#23](https://github.com/minchenlee/c9watch/pull/23))

## 0.2.0 — 2026-02-16

### Added

- WebSocket server for mobile/remote access — view sessions from any device on the same network ([#6](https://github.com/minchenlee/c9watch/pull/6))
- QR code connection for instant mobile browser pairing
- Custom session titles with inline editing ([#9](https://github.com/minchenlee/c9watch/pull/9))
- Linux support via AppImage ([#2](https://github.com/minchenlee/c9watch/pull/2))

### Improved

- ~60% CPU reduction — optimized polling and status detection, from ~15% to ~5-9% ([#14](https://github.com/minchenlee/c9watch/pull/14), [#19](https://github.com/minchenlee/c9watch/pull/19))
- Simplified notifications — removed custom permission banner, macOS handles prompts natively ([#20](https://github.com/minchenlee/c9watch/pull/20))
- Better iTerm2 click-to-focus using tty matching instead of window title matching ([#5](https://github.com/minchenlee/c9watch/pull/5))

### Fixed

- Status flickering when sessions are actively working ([#19](https://github.com/minchenlee/c9watch/pull/19))
- Duplicate notification firing ([#19](https://github.com/minchenlee/c9watch/pull/19))
- Register missing `get_terminal_title` command ([#21](https://github.com/minchenlee/c9watch/pull/21))

## 0.1.0 — 2026-02-08

### Initial release

- Automatic session discovery — detects Claude Code sessions by scanning running processes at the OS level
- Real-time dashboard with status indicators (Working, Needs Permission, Idle)
- Status view (grouped by state) and project view (grouped by directory)
- Session control — expand to read full message history, approve permissions, manage agents
- Auto-updater for future releases
- Built with Tauri, Rust, and Svelte for minimal resource usage
