# c9watch CLI — Agent Skills Reference

> Add this file to your project's CLAUDE.md to give Claude Code the ability to monitor sibling sessions, search past work, track costs, and coordinate with other agents.
>
> ```markdown
> # in your CLAUDE.md
> See [c9watch SKILLS.md](path/to/c9watch/SKILLS.md) for CLI commands to monitor other Claude Code sessions.
> ```

## Prerequisites

The `c9watch` binary must be on your `$PATH`. Install with:

```bash
curl -fsSL https://raw.githubusercontent.com/minchenlee/c9watch/main/install-cli.sh | bash
```

All commands output JSON. Use `--pretty` for human-readable output. Errors return `{"error": "message"}` on stderr with exit code 1.

## Commands

### Discover active sessions

```bash
# List all active sessions
c9watch list

# Filter by project or status
c9watch list --project myapp
c9watch list --status Working
c9watch list --status NeedsAttention

# Compact output (fewer fields, less tokens)
c9watch list --compact
```

**Output fields (full):** `id`, `pid`, `sessionName`, `projectPath`, `firstPrompt`, `messageCount`, `modified`, `status`, `customTitle`, `gitBranch`, `summary`, `latestMessage`, `pendingToolName`, `pendingToolInput`, `taskProgress`

**Output fields (compact):** `id`, `pid`, `status`, `projectPath`, `sessionName`, `pendingToolName`

**Status values:** `Working`, `WaitingForInput`, `NeedsAttention`, `Idle`

### Get a status overview

```bash
c9watch status
```

Returns `total`, `byStatus` (count per status), `byProject` (count per project), and `needsPermission` (list of sessions waiting for approval).

### Identify yourself

```bash
c9watch self
```

Walks up the PID tree to find the calling Claude Code process, then returns the full session object. Use this to find your own session ID, project path, or task progress.

### View a conversation

```bash
# Full conversation (session ID supports prefix matching)
c9watch view abc123

# Last 5 messages only (saves tokens)
c9watch view abc123 --last 5
```

Returns `sessionId`, `messages[]` (each with `role`, `content`, `timestamp`). System-injected XML tags are automatically stripped.

### Browse history

```bash
# Recent sessions (default: all)
c9watch history -n 20
```

Returns an array with `sessionId`, `firstPrompt`, `display`, `date`, `project`, `projectName`, `customTitle`.

### Search past sessions

```bash
c9watch search "implement auth middleware"
c9watch search "fix bug" --project myapp -n 10
```

Returns `query`, `hits[]` (each with `sessionId`, `snippet`, `projectPath`, `modified`), `truncated`. Multi-word queries use AND logic — all words must appear in the session.

### View tasks

```bash
c9watch tasks abc123
```

Returns `sessionId`, `tasks[]`, `total`, `completed`, `inProgress`, `pending`.

### Stop a session

```bash
c9watch stop 12345  # pass the PID, not the session ID
```

### Watch for changes

```bash
# Stream NDJSON events
c9watch watch

# Only emit changes (skip initial state)
c9watch watch --changes-only

# Compact + filtered
c9watch watch --compact --project myapp --interval 5
```

Each line is a JSON object with `event` (`started`, `status_changed`, `stopped`), `sessionId`, `session`, `timestamp`.

## Common agent workflows

### Check if any session needs attention

```bash
c9watch status | jq '.needsPermission'
```

### Find what you worked on yesterday

```bash
c9watch history -n 50 | jq '[.[] | select(.date | startswith("2026-04-05"))]'
```

### Monitor a specific project while working

```bash
c9watch watch --compact --project myapp --changes-only
```

### Get your own session's task progress

```bash
c9watch self | jq '.taskProgress'
```
