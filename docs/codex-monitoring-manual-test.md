# Codex Monitoring Manual Test Guide

This guide validates the Codex monitoring functionality in PR [#112](https://github.com/minchenlee/c9watch/pull/112).

## Available Debug App and Binary

After building the feature branch, the macOS arm64 debug app is available at:

```text
src-tauri/target/debug/bundle/macos/c9watch.app
```

Quit any other running c9watch instance first. Running two apps with the same bundle identifier can make it unclear which build is active. Launch the debug app from the repository root:

```bash
open src-tauri/target/debug/bundle/macos/c9watch.app
```

To open the output folder in Finder and launch the app manually:

```bash
open src-tauri/target/debug/bundle/macos
```

The current debug app has these properties:

- Version: `c9watch 0.8.1`
- Architecture: macOS arm64
- Signing: local ad-hoc signature
- Not notarized and not intended for release distribution

The combined GUI and CLI executable is available at:

```text
src-tauri/target/debug/c9watch
```

Running it without arguments should also start the GUI:

```bash
./src-tauri/target/debug/c9watch
```

If it only prints `Monitor and manage Claude Code sessions` followed by CLI usage, a CLI-only test build has overwritten the executable. Rebuild the debug app using the command at the end of this guide.

Confirm the binary version:

```bash
./src-tauri/target/debug/c9watch --version
```

Calculate the current app executable checksum:

```bash
shasum -a 256 src-tauri/target/debug/bundle/macos/c9watch.app/Contents/MacOS/c9watch
```

Inspect detected sessions:

```bash
./src-tauri/target/debug/c9watch list --pretty
```

## Test Preparation

1. Confirm that the Codex CLI is available:

   ```bash
   codex --version
   ```

2. Confirm that local Codex session data exists:

   ```bash
   ls ~/.codex/sessions
   ```

3. Use a unique, searchable marker for each test, for example:

   ```text
   C9WATCH-MANUAL-20260713-01
   ```

4. To test provider filtering, keep at least one Claude Code session and one Codex session available.

## Minimum Acceptance Flow

If time is limited, complete at least these six checks:

- [ ] Codex sessions use a blue `CODEX` badge and Claude Code sessions use an orange `CLAUDE CODE` badge in the main window and menu bar popover.
- [ ] A Codex CLI session appears in Monitor and its CLI JSON reports `surface: cli`.
- [ ] The `All | Claude Code | Codex` filter changes the actual data in Monitor, History, Cost, and Memory.
- [ ] Resuming the same Codex session produces one card and retains the full conversation.
- [ ] The Cost tab estimates known Codex models, gives each model a consistent distinct color, and shows one row per session under `BY PROJECT`.
- [ ] The Memory tab's `Codex` filter displays `MEMORY.md` and `memory_summary.md` with a blue `CODEX` badge when they exist.

## Full Test Procedure

### 1. Startup and Basic Checks

1. Launch the debug app.
2. Open the main window and menu bar popover.
3. Confirm that the app stays open and does not display a blank window.
4. Run:

   ```bash
   ./src-tauri/target/debug/c9watch status
   ./src-tauri/target/debug/c9watch list --pretty
   ```

Expected results:

- Both commands return valid JSON.
- Existing Claude Code sessions still appear normally.
- Adding Codex monitoring does not break existing session detection.

### 2. Codex App Session

1. Create a session in the Codex App.
2. Enter a prompt containing a unique marker, for example:

   ```text
   Reply only with C9WATCH-APP-20260713-01
   ```

3. Return to c9watch Monitor.

Expected results:

- The session appears in Monitor.
- The card displays a blue `CODEX` badge.
- The project path and prompt content are correct.
- Unsupported Stop, Rename, and Open Session actions are not shown.
- CLI JSON reports `provider: codex` and `surface: app`.

### 3. Codex CLI Session

1. Start Codex from another terminal in a test project directory:

   ```bash
   codex
   ```

2. Enter a unique marker:

   ```text
   Reply only with C9WATCH-CLI-20260713-01
   ```

3. Run:

   ```bash
   ./src-tauri/target/debug/c9watch list --pretty
   ```

Expected results:

- Monitor displays one card for the Codex session.
- The card displays a blue `CODEX` badge.
- CLI JSON reports:
  - `provider: codex`
  - `surface: cli`
  - `agentKind: root`
  - `canOpen: false`
  - `canStop: false`
  - `canRename: false`

### 4. Provider Filter

1. Keep at least one Claude Code session and one Codex session available.
2. Select each provider option in the main window:
   - `All`
   - `Claude Code`
   - `Codex`
3. Open the menu bar popover and confirm that the filter is synchronized.
4. In the popover header, use the compact `SHOW` dropdown to select each provider.
5. Check Monitor, History, Cost, and Memory under each filter.
6. Quit and restart c9watch.

Expected results:

- `All` displays both providers.
- `Claude Code` displays only Claude Code data.
- `Codex` displays only Codex data.
- The filter is synchronized with the popover.
- The popover header keeps the session count, compact provider dropdown, and icon-only dashboard action on one line without overlap or wrapping.
- The main window keeps the segmented provider control, while the popover uses one compact native dropdown.
- Claude Code badges are orange and Codex badges are blue wherever provider badges appear.
- Changing tabs does not reset the filter or make it cosmetic only.
- Memory project counts, selection, content, and empty states follow the filter.
- The selected filter persists after restarting c9watch.

### 5. Codex Subagent Grouping

1. Ask a Codex session to create a subagent, for example:

   ```text
   Start a subagent to list the Markdown files in the current directory and report the total count.
   ```

2. Check Monitor and the popover while the subagent is running.

Expected results:

- A normal subagent appears under its parent session's Subagents section.
- A nested subagent remains visible under an appropriate root session.
- A running orphan subagent remains visible when its parent is temporarily unavailable.
- Internal guardian, review, and other helper agents do not appear as independent session cards.

### 6. Resume the Same Codex Session

This is an important regression test for the PR review fixes.

1. Start a Codex CLI session and enter:

   ```text
   Remember C9WATCH-RESUME-FIRST-20260713-01
   ```

2. Exit the Codex CLI.
3. Resume the latest session immediately:

   ```bash
   codex resume --last "Reply with C9WATCH-RESUME-SECOND-20260713-01"
   ```

   If `--last` may select a different session, find the session ID with the c9watch CLI and run:

   ```bash
   codex resume <SESSION_ID> "Reply with C9WATCH-RESUME-SECOND-20260713-01"
   ```

4. Check Monitor.
5. Open the session conversation.

Expected results:

- The session ID appears on only one card.
- The card uses the latest rollout state instead of being overwritten by an older idle state.
- The conversation contains both the `FIRST` and `SECOND` markers.
- Assistant messages use the provider-neutral `AGENT` role label.
- The Tools toggle hides and restores Codex tool calls and tool results.
- Duplicate rollout messages are not displayed twice.

### 7. History and Search

1. Complete a Codex App or CLI session.
2. Open History.
3. Select the `Codex` filter.
4. Search for the unique marker used earlier.
5. Open the complete conversation from the result.

Expected results:

- Codex sessions appear in History.
- Metadata search and deep search both find Codex content.
- Claude Code data does not appear under the `Codex` filter.
- All conversation fragments from a resumed session are available.
- Internal helper sessions do not appear in History.

### 8. Cost and Token Usage

1. Open Cost.
2. Select the `Codex` filter.
3. Switch between the USD and TOKENS charts.
4. Switch back to `All` and inspect mixed-provider data.

Expected results:

- Codex tokens are included in totals and charts.
- Dates containing only Codex usage remain visible in token charts.
- Known models such as `gpt-5.6-sol`, `gpt-5.6-terra`, and `gpt-5.6-luna` display USD estimates.
- The UI explains that estimates use OpenAI Standard short-context API rates, are a lower bound, may understate long-context calls, and are not the subscription bill.
- Unknown Codex models display `UNPRICED`, not `$0`.
- Codex usage is blue, Claude Code usage is amber, and a visible provider legend is present.
- Each model in `BY MODEL` has a consistent distinct color in both the summary bar and its legend marker.
- `BY PROJECT` displays one row per provider/session ID, even when accounting spans multiple days or models.
- A merged multi-model session displays `N MODELS`; hovering it lists the contributing models.
- Merging visible rows does not change per-model pricing, token totals, or priced/unpriced totals.
- Each session's cost or token value stays on the same row as its badge, prompt, date, and model.
- Usage is priced separately when a session changes models on the same day.
- Unknown-model tokens do not affect estimates for known models.
- Provider filtering recalculates the displayed data rather than hiding labels only.

### 9. Memory and Provider Filtering

1. Confirm that at least one durable Codex memory file exists:

   ```bash
   ls ~/.codex/memories/MEMORY.md ~/.codex/memories/memory_summary.md
   ```

2. Open Memory.
3. Select `All`, `Claude Code`, and `Codex` in turn.
4. Under the `Codex` filter, open the Codex memory group and a Markdown file.

Expected results:

- The `Codex` filter displays only the Codex memory group with a blue `CODEX` badge.
- Existing top-level `MEMORY.md` and `memory_summary.md` files are available.
- The tab does not recursively load `rollout_summaries/`, `raw_memories.md`, or other internal data.
- The `Claude Code` filter still displays memory from `~/.claude/projects/*/memory/`.
- Counts, selection, content, and empty states update correctly when the filter changes.
- Codex memory does not display the Claude-only `claude "Review my memory files"` command.
- Reveal in Finder opens `~/.codex/memories/`.

## Test Result Template

Copy this template when recording results:

```text
Date:
macOS version:
Codex version:
c9watch binary SHA-256:

[ ] 1. Startup and basic checks
[ ] 2. Codex App session
[ ] 3. Codex CLI session
[ ] 4. Provider filter
[ ] 5. Codex subagent grouping
[ ] 6. Resume the same session
[ ] 7. History and search
[ ] 8. Cost and token usage
[ ] 9. Memory and provider filtering

Issues and notes:
```

## Information to Capture for a Bug Report

Keep the following information:

- A screenshot of the issue.
- The unique test marker.
- The Codex session ID.
- Output from `codex --version` and `c9watch --version`.
- Relevant c9watch logs.
- Output from:

  ```bash
  ./src-tauri/target/debug/c9watch list --pretty
  find ~/.codex/sessions -name "*<SESSION_ID>.jsonl" -print
  ```

Rollout files may contain private prompts, paths, or tool output. Do not post a complete rollout file publicly. Remove sensitive content first.

## Rebuild the Debug App

From the feature branch, run:

```bash
git switch feature/codex-monitoring
npm install
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_DEV_STRIP=none CARGO_INCREMENTAL=0 \
  npm run tauri build -- --debug --bundles app \
  --config '{"bundle":{"createUpdaterArtifacts":false}}'
```

The debug app is written to:

```text
src-tauri/target/debug/bundle/macos/c9watch.app
```

The combined GUI and CLI executable is written to:

```text
src-tauri/target/debug/c9watch
```

To build release `.app` and `.dmg` artifacts, run:

```bash
npm run tauri build
```

Release outputs are written to:

```text
src-tauri/target/release/bundle/macos/c9watch.app
src-tauri/target/release/bundle/dmg/
```

Use the debug app for local manual testing. Release artifacts require the project's normal signing and notarization process before distribution.
