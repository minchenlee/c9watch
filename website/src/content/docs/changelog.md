---
title: Changelog
description: All notable changes to c9watch.
---

All notable changes to c9watch are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
