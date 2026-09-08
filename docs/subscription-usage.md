# Subscription usage

The toolbar and tray show separate square indicators for Claude Code, Codex and
Cursor. Each represents the highest used quota window. Hover, focus or tap opens
provider details with segmented meters, reset times and freshness. Unknown values
are unavailable, never zero. Over-limit numbers remain visible while meters cap
at 100%. Escape dismisses details and short windows allow tooltip scrolling.

Settings → Usage controls percentage visibility (auto/fullscreen and tray, always,
or hidden), provider or monochrome colors, center icons and visible subscriptions.
Changes persist locally and synchronize through storage events and window focus.
All provider marks are white; provider color remains on the square perimeter.

## Data sources

- Codex: read-only `account/rateLimits/read` through the installed `codex app-server`,
  using the existing ChatGPT login. No model request or conversation is created.
  The subprocess has a 15-second timeout and is killed/reaped after the snapshot.
- Cursor: the installed client's `DashboardService/GetCurrentPeriodUsage` RPC at
  the fixed HTTPS origin `api2.cursor.sh`. Only access-token and membership entries
  are read from the local database in read-only mode. Credentials stay in the
  backend and are piped to curl over stdin, never logged, written to disk, passed
  as process arguments or returned to the frontend. Redirects and user curl config
  are disabled; requests have time and size limits. Provider quota percentages
  take precedence over legacy included-spend/limit values; bonus spend is excluded.
  This is an implementation interface and may change without notice.
- Claude Code: an opt-in local status-line bridge, described below. No OAuth token
  or account endpoint is used.

Concurrent reads share a 55-second cache. Providers fail independently. Transient
failures retain matching last-known values with their original timestamp and an
explicit stale message. Empty Claude reports clear previous quota windows, and
expired windows are removed. Desktop windows refresh in the background; hidden
browser tabs pause until visible. Slow subagent scans run in a bounded blocking
worker so they do not starve quota reads on the async executor.

Quota data is available over Tauri IPC and the existing authenticated WebSocket.
Demo values are fixtures. Session costs are never presented as subscription usage.
OpenRouter/other gateway monetary displays are not part of this change.

## Claude Code bridge

From a stable installed executable:

```sh
c9watch usage-bridge --install
```

Installation backs up `settings.json`, preserves other settings/status-line
options and pipes the original JSON to an existing status-line command. Repeating
installation with the same executable is a no-op. Symlinked or unsupported settings
are left for manual configuration. Automatic installation supports macOS/Linux;
Windows requires manual shell configuration.

To uninstall, restore only the previous `statusLine` field from the generated
`settings.c9watch-backup-<id>.json`, or remove it if none existed. Before moving the
executable, restore that field and install again from the new stable path. Project
status-line overrides must include the bridge separately.

Only quota percentages, reset times, schema version and observation time are
atomically written to `~/.claude/c9watch/subscription-usage.json`. The full session
payload is not stored. Existing status-line input is preserved even on a cache
write failure. `CLAUDE_CONFIG_DIR` is honored; GUI launches and the bridge must use
the same directory. The bridge accepts at most 1 MiB of input.

An eligible plan and a response in Claude Code are normally required before limits
appear. The observation time reflects a local report, not a new server query.
Reports do not identify accounts; only the latest local report is represented.

## Validation and local rebuild

```sh
npm run check
npm run build
node scripts/test-subagent-polling.mjs
node scripts/test-subscription-polling.mjs
node scripts/test-usage-preferences.mjs
cargo test --manifest-path src-tauri/Cargo.toml --lib usage
node scripts/test-claude-usage-bridge.mjs /path/to/c9watch
```

Live Codex and Cursor data were observed in the native QA app. Real Claude
subscription acceptance remains unverified because no subscribed account was
available; bridge behavior is covered by isolated fixtures. Native tray hover
and the latest visual design are not full release acceptance.

For a disposable local QA app, quit Subscription QA then run
`python3 scripts/rebuild-subscription-qa.py`. It replaces
`~/Applications/c9watch Subscription QA.app`, uses one managed `.qa-build/target`,
and removes package binaries/tests after installation while reusing dependencies.
A single source snapshot, log and receipt are retained. Other apps and worktrees
are not touched. This is not a signed/notarized release.

Provider mark attribution is in `static/providers/README.md`.
