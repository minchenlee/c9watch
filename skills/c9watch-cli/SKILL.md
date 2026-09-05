---
name: c9watch-cli
description: >
  Use c9watch CLI to monitor, query, and coordinate Claude Code sessions.
  Trigger when the user asks about other running Claude Code sessions,
  wants to check what agents are doing, needs to find past session work,
  asks about Claude Code costs or spending, wants to stop or coordinate
  sibling sessions, or mentions "c9watch". Also trigger when the user
  says things like "check on my other sessions", "what's running",
  "any sessions stuck", "find that conversation where I...",
  "how much have I spent", or "what did I work on yesterday".
---

# c9watch CLI

c9watch is a CLI tool that lets you monitor and manage all Claude Code sessions running on the machine. Every command outputs JSON. Use it to see what other sessions are doing, search past work, track costs, and coordinate with sibling agents.

c9watch also tracks **pi agent** (`pi`), **Codex**, and **Cursor** sessions. Every session and history entry carries a `provider` (`claudeCode`, `codex`, `cursor`, `pi`) and a provider-scoped `sessionKey` (`provider:id`, e.g. `pi:01a06659-...`). Use the `sessionKey` with `view`/`tasks` whenever an ID could collide across providers.

## Before you start

Verify `c9watch` is available:

```bash
which c9watch && c9watch --version
```

If not installed, tell the user:
```
c9watch CLI is not installed. Install it with:
curl -fsSL https://raw.githubusercontent.com/minchenlee/c9watch/main/install-cli.sh | bash
```

## Commands reference

### List active sessions

```bash
c9watch list                                    # all sessions
c9watch list --compact                          # minimal fields (saves tokens)
c9watch list --project myapp                    # filter by project path
c9watch list --status NeedsAttention            # filter by status
```

Status values: `Working`, `WaitingForInput`, `NeedsAttention`, `Idle`

Compact output fields: `id`, `pid`, `status`, `projectPath`, `sessionName`, `pendingToolName`

Full output adds: `firstPrompt`, `messageCount`, `modified`, `customTitle`, `gitBranch`, `summary`, `latestMessage`, `pendingToolInput`, `taskProgress`

### Status overview

```bash
c9watch status                                  # counts by status and project
c9watch status --project myapp                  # filtered
```

Returns `total`, `byStatus`, `byProject`, and `needsPermission` (sessions waiting for approval). This is the fastest way to answer "is anything stuck?" — check `needsPermission` first.

### Identify yourself

```bash
c9watch self
```

Walks the PID tree to find the calling Claude Code process and returns your own session object. Use this to discover your own session ID, project path, git branch, or task progress without the user telling you.

### View a conversation

```bash
c9watch view <session-id>                       # full conversation
c9watch view <session-id> --last 5              # last 5 messages (saves tokens)
```

Session IDs support prefix matching — `c9watch view abc` resolves to the full UUID if unambiguous. Prefix with the provider when ambiguous: `c9watch view pi:abc`. System XML tags are automatically stripped from output.

When viewing conversations, prefer `--last N` to avoid loading thousands of tokens from long sessions. Start with `--last 5` and go larger only if needed.

### Browse history

```bash
c9watch history                                 # all past sessions
c9watch history -n 20                           # most recent 20
```

Returns: `sessionId`, `firstPrompt`, `display`, `date` (ISO 8601), `project`, `projectName`, `customTitle`.

### Search past sessions

```bash
c9watch search "implement auth middleware"
c9watch search "fix bug" --project myapp -n 10
```

Multi-word queries use AND logic — all words must appear somewhere in the session. Returns `hits[]` with `sessionId`, `snippet`, `projectPath`, `modified`.

### View tasks

```bash
c9watch tasks <session-id>
```

Returns `total`, `completed`, `inProgress`, `pending`, and the full `tasks[]` array. Tasks are only available for Claude Code sessions.

### Stop a session

```bash
c9watch stop <pid>
```

Takes the PID (not session ID). Get the PID from `c9watch list`. Always confirm with the user before stopping a session — this is destructive and irreversible.

### Watch for changes

```bash
c9watch watch                                   # stream all events
c9watch watch --changes-only --compact           # only changes, minimal fields
c9watch watch --project myapp --interval 5       # filtered, slower poll
```

Streams NDJSON (one JSON object per line). Events: `started`, `status_changed`, `stopped`. This is a long-running command — run it in the background if you need to monitor while doing other work.

## Choosing the right command

| User wants to know... | Command |
|----------------------|---------|
| "What's running right now?" | `c9watch list --compact` |
| "Is anything stuck/waiting?" | `c9watch status` then check `needsPermission` |
| "What's session X doing?" | `c9watch view <id> --last 3` |
| "What did I work on recently?" | `c9watch history -n 20` |
| "Find that conversation where..." | `c9watch search "<keywords>"` |
| "How far along is that task?" | `c9watch tasks <id>` |
| "Who am I?" | `c9watch self` |
| "Stop that other session" | `c9watch list` to find PID, confirm, then `c9watch stop <pid>` |

## Tips for effective use

- Always use `--compact` on `list` when you only need status info. Full output includes conversation content which wastes tokens.
- Use `--last N` on `view` to avoid loading entire long conversations.
- Pipe through `jq` for filtering: `c9watch list | jq '.sessions[] | select(.status == "NeedsAttention")'`
- The `self` command only works when called from within a Claude Code session (it needs a claude parent process in the PID tree).
- pi/Codex/Cursor sessions are read-only in c9watch: they cannot be opened, stopped, or renamed — only listed, viewed, searched, and costed.
- `--pretty` on any command gives indented JSON for human-readable output, but costs more tokens. Skip it when parsing programmatically.
