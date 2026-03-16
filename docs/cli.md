# c9watch CLI

A JSON-based CLI for monitoring and querying Claude Code sessions. Designed for agent-to-agent workflows — all output is structured JSON on stdout, with no stderr noise.

## Quick Start

```bash
# Build from source
cd src-tauri && cargo build --release
# Binary at: target/release/c9watch

# Or use the debug build
cd src-tauri && cargo build
# Binary at: target/debug/c9watch
```

## Commands

### `status` — Quick Overview

One-line aggregate of all active sessions. The fastest way to check what's happening.

```bash
c9watch status
c9watch status --project c9watch
```

```json
{
  "total": 9,
  "byStatus": { "Working": 2, "WaitingForInput": 7 },
  "byProject": { "c9watch": 5, "agentcheck": 1, "liminchen": 2 },
  "needsPermission": []
}
```

When a session needs permission, `needsPermission` includes the session ID and pending tool:
```json
{
  "needsPermission": [
    { "id": "46fe2af3-...", "sessionName": "c9watch", "pendingToolName": "Bash" }
  ]
}
```

### `list` — Active Sessions

Full details on every running Claude Code session.

```bash
c9watch list                        # All sessions
c9watch list --project c9watch      # Filter by project path
c9watch list --compact              # Minimal fields only
c9watch list --compact --pretty     # Readable compact output
```

**Full output fields:**

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Session UUID |
| `pid` | number | Process ID |
| `status` | string | `Working`, `WaitingForInput`, `NeedsPermission`, `Connecting` |
| `sessionName` | string | Project directory name |
| `projectPath` | string | Full filesystem path |
| `firstPrompt` | string | First user message (truncated to 100 chars) |
| `messageCount` | number | Total messages in session |
| `modified` | string | ISO 8601 last modification time |
| `latestMessage` | string | Most recent message content (omitted if empty) |
| `pendingToolName` | string | Tool waiting for approval (omitted if none) |
| `pendingToolInput` | object | Full input of pending tool (omitted if none) |
| `customTitle` | string | User-set session name (omitted if none) |
| `gitBranch` | string | Git branch (omitted if unknown) |
| `summary` | string | Session summary (omitted if none) |
| `taskProgress` | object | `{total, completed, inProgress, currentTask}` (omitted if no tasks) |

Null/empty optional fields are omitted to save tokens.

**Compact output** (`--compact`) includes only: `id`, `pid`, `status`, `projectPath`, `sessionName`, `pendingToolName`.

### `view` — Read a Conversation

```bash
c9watch view <session-id>               # Full conversation
c9watch view <session-id> --last 5      # Last 5 messages
c9watch view 46fe --last 3 --pretty     # UUID prefix works
```

Each message includes:
- `timestamp` — ISO 8601
- `messageType` — `User`, `Assistant`, `Thinking`, `ToolUse`, `ToolResult`, `System`
- `content` — Sanitized text (system XML tags stripped)
- `images` — Base64 image blocks (if any, omitted if empty)

### `search` — Find Past Sessions

Full-text search across all session JSONL files. Multi-word queries use AND matching.

```bash
c9watch search "backlog"
c9watch search "refactor parser" --project c9watch
```

```json
{
  "query": "backlog",
  "hits": [
    {
      "sessionId": "d8036d8f-...",
      "snippet": "we should add this to our backlog",
      "projectPath": "/Users/you/project",
      "modified": "2026-03-14T17:59:33+00:00"
    }
  ]
}
```

### `history` — Past Sessions

Lists sessions from `~/.claude/history.jsonl`, sorted newest first.

```bash
c9watch history              # All history
c9watch history -n 10        # Last 10
```

Each entry includes `firstPrompt` (read from JSONL, not the raw `/resume` command) and `date` (ISO 8601).

### `watch` — Stream Status Changes

Streams newline-delimited JSON events. Each line is independently parseable.

```bash
c9watch watch                                    # Default: 2s interval, all sessions
c9watch watch -i 1 --project c9watch             # 1s interval, filtered
c9watch watch --compact                          # Minimal fields per event
c9watch watch --changes-only                     # Skip initial "started" burst
c9watch watch --compact --changes-only -i 1      # Optimal for agent polling
```

**Event types:**

| Event | When | Payload |
|-------|------|---------|
| `started` | New session detected | Full or compact session data |
| `status_changed` | Status or pendingToolName changed | Full or compact session data |
| `stopped` | Session disappeared | `{sessionId}` only |

Each event line:
```json
{"event": "status_changed", "session": {...}, "timestamp": "2026-03-16T12:00:00+00:00"}
```

**Tip:** Use `--changes-only` when you already have state from `list`. Use `--compact` to minimize context window usage.

### `tasks` — Session Tasks/Todos

Read tasks from `~/.claude/tasks/<session-id>/`.

```bash
c9watch tasks <session-id>
```

```json
{
  "sessionId": "...",
  "tasks": [
    { "id": "1", "subject": "Research API design", "status": "completed", "blocks": ["3"], "blockedBy": [] },
    { "id": "2", "subject": "Implement endpoints", "status": "in_progress", "blocks": [], "blockedBy": ["1"] }
  ],
  "total": 2,
  "completed": 1,
  "inProgress": 1,
  "pending": 0
}
```

### `stop` — Stop a Session

```bash
c9watch stop <pid>
```

## Global Flags

| Flag | Description |
|------|-------------|
| `--pretty` | Pretty-print JSON output (useful for debugging, not for agent consumption) |
| `--help` / `-h` | Print help |
| `--version` / `-V` | Print version |

## Session ID Prefix Matching

All commands that accept a session ID support UUID prefix matching:

```bash
c9watch view 46fe          # Matches 46fe2af3-fd8e-43e3-bd6d-6c7800722cbf
c9watch tasks 5c54          # Matches 5c5426de-08b0-484a-b1ae-1df59ac84a39
```

If the prefix is ambiguous (matches multiple sessions), an error is returned. Prefix resolution scans JSONL filenames directly — no process detection needed.

## Agent Workflow Examples

### Orchestrator polling loop
```bash
# Initial state
c9watch status
c9watch list --compact

# Watch for changes (minimal tokens)
c9watch watch --compact --changes-only -i 2 --project myproject
```

### Check if any session needs attention
```bash
c9watch status | jq '.needsPermission'
```

### Find what a session is working on
```bash
c9watch view 46fe --last 3
```

### Search across all projects
```bash
c9watch search "database migration" --project myapp
```

## Design Notes

- **All output is JSON** on stdout. Errors go to stderr as `{"error": "..."}` with exit code 1.
- **No stderr noise** — debug warnings are suppressed in CLI mode.
- **System XML tags** (e.g., `<local-command-caveat>`, `<system-reminder>`) are stripped from all text fields.
- **Read-only** — the CLI cannot send input to running sessions or start new ones. Claude Code's stdin is owned by the terminal process that launched it.
