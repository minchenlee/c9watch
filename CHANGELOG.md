# Changelog

All notable changes to c9watch are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
