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
c9watch list                                    # All sessions
c9watch list --project c9watch                  # Filter by project path
c9watch list --status Working                   # Filter by status
c9watch list --status NeedsPermission --compact # Combine filters
c9watch list --compact --pretty                 # Readable compact output
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
| `sourceHealth` | string | Source observation health: `fresh`, `stale`, `partial`, `unavailable`, `unknown` (omitted for older backends) |
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
c9watch view codex:<session-id>         # Provider-scoped key disambiguates IDs
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
      "modified": "2026-03-14T17:59:33+00:00",
      "provider": "codex",
      "surface": "cli",
      "agentKind": "root"
    }
  ]
}
```

Search covers Claude Code, Codex, and Cursor history. Each hit includes `provider`, `surface`, and `agentKind` so consumers can distinguish the source and session type.

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

**Event types:** `started`, `status_changed`, `stopped`. Every line has `"schema": 1`, `event`, `sessionId`, `sessionKey`, and `timestamp`; see [cli-events.md](cli-events.md) for all fields. `--json` is accepted and changes nothing.

```json
{"schema": 1, "event": "status_changed", "sessionId": "46fe2af3-...", "sessionKey": "claudeCode:46fe2af3-...", "status": "NeedsAttention", "previousStatus": "Working", "session": {...}, "timestamp": "2026-03-16T12:00:00+00:00"}
```

**Tip:** Use `--changes-only` when you already have state from `list`. Use `--compact` to minimize context window usage.

### `wait` — Block Until a Session Changes

Waits until one live session meets a condition, prints that event, and exits.

```bash
c9watch wait abc123 --until needs-input            # NeedsAttention or WaitingForInput
c9watch wait codex:01a1 --until done --timeout 600 # Working -> anything else
c9watch wait abc123 --until ended                  # the session stops
c9watch wait abc123 --until any-change -i 1
```

Exit codes: `0` met, `2` no single live session matches, `3` stopped before `needs-input`, `124` timeout. `done` is met at once when the session is not Working. Details: [cli-events.md](cli-events.md).

An engine build also has `wait --event turn_ended --agent <id|name> [--since <cursor>]` for an engine agent's next turn end. `--since` takes a cursor you hold, such as the `cursor` that `send` prints; see the note under `send` on why `send --wait` is the exact wait for an answer.

### `tasks` — Session Tasks/Todos

Read tasks from `~/.claude/tasks/<session-id>/`.

```bash
c9watch tasks <session-id>
c9watch tasks cursor:<session-id>       # Provider-scoped key
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

Accepts either a PID (for regular Claude sessions) or a session ID / prefix (for PM workers spawned via `c9watch spawn`). Numeric args are treated as PIDs and dispatched to the session-kill path; non-numeric args are sent to the PM daemon as a worker stop.

```bash
c9watch stop <pid>                   # regular Claude session (by PID)
c9watch stop <session-id-or-prefix>  # PM worker (via daemon RPC)
```

### `spawn` — Start a Worker Session

Spawn a new Claude Code session managed by c9watch. The worker is a real session that appears in `c9watch list`.

```bash
c9watch spawn --name researcher --cwd /path/to/project
c9watch spawn --name writer --append-system-prompt "You are a technical writer."
c9watch spawn --cwd . --permission-mode plan --model sonnet
```

```json
{"ok":true,"sessionId":"abc-1234-...","pid":54321,"name":"researcher"}
```

Options: `--cwd`, `--name`, `--prompt` (the worker's only prompt; there is no later `send` to a worker), `--append-system-prompt`, `--append-system-prompt-file`, `--permission-mode` (default: bypassPermissions), `--model`, `--add-dir`

**Engine builds only (`--features engine`, not released):** `--mode managed` (the default) is the worker above. `--mode free` creates an engine agent instead:

```bash
c9watch spawn --mode free --cwd /path/to/project --name researcher
```

- It auto-starts the engine and calls `agent.create`. It takes only `--cwd` (default: the current directory) and `--name`; any other option fails with `bad_args`.
- It prints `{"agent": <row>}` (c9p/1 `AgentRow`) once the agent is `ready`. This can take up to 30 s, plus a margin.
- A caller with `C9_AGENT_ID` set (inside an engine agent) gets `not_allowed` (the engine's code for the same refusal) and never reaches the engine.
- Errors exit `1` with `{"error":"<code>: <msg>; <hint>"}`. Retire the agent with `c9watch api agent.retire '{"id":"agt_..."}'`.

### `send` — Mail an Engine Agent

**Engine builds only (`--features engine`, not released).** Sends one message to an engine agent (an agent the engine runs in a PTY, such as one from `spawn --mode free`) through the engine's `msg.send`. The engine queues the message. When the agent is ready and its input box is empty, the engine pastes the message into the box and submits it, so the agent reads it as its next prompt. The legacy worker `send` is removed: a `--mode managed` worker takes its only prompt from `spawn --prompt`.

```bash
c9watch send researcher "Summarize findings in notes.md"     # by name
c9watch send agt_01J... --file instructions.md --kind request
c9watch send researcher --stdin --idem ci-build-42 < brief.txt
c9watch send researcher "Review PR 12" --wait --timeout 600
```

- `TO` is an agent id (`agt_...`), `<machine>/<id>`, or a name. A name that two live agents share fails with `unknown_address` and lists the `candidates`; an unknown name gets the `nearest` one.
- Give the text exactly once: `MESSAGE`, `--message`, `--stdin` or `--file`. CRLF line ends become LF.
- `--kind request|info` (default `info`), `--reply-to <msg id>` (the reply joins that message's thread), `--idem <key>`.
- It prints `{"msg": {"msgId", "state", "to", "cursor", "replayed"}}`. `state` is `queued`.
- An engine error prints `{"error": {"code", "msg", "hint", "data"}}` on stderr and exits `1`. `data` is there only for some codes, for example `nearest`, `candidates`, or the target's `state` for `not_controlled`. Mail goes only to an agent the engine controls (`control='pty'`) that has not ended.
- `--idem` makes a resend safe: the same sender, key and request get the first message back with `replayed: true`; the same key with a different request is `duplicate`. The key is unique per sender principal. Every terminal script is the same principal, `local-unverified`, so give each script its own prefix (for example `ci-build-42`, not `42`).

**Waiting for the answer.** `--wait` follows the message from its `cursor`. It waits until the message is `delivered`, then for the agent's next turn end: the turn that answered the message. It prints a second line, `{"msg": {..., "state": "delivered", "detail": ...}, "turnEnd": "<cursor>"}`, and exits `0`. It exits `1` when the message ends `failed` or `uncertain` (the engine is not sure the agent got it, and does not send it again), and `2` when `--timeout` (default 300 s) runs out. After an `engine restart` it reconnects and goes on.

`cursor` can also go to `c9watch wait --event turn_ended --agent <id> --since <cursor>`. That wait is not exact: the cursor is where the message was queued, so if the agent was mid-turn then, the wait ends with that turn, which did not read the message. Use `send --wait` to wait for the answer. A cursor older than the 24 h event retention (an idem replay can return one) gets `cursor_expired`.

A message sent while the engine drains (`engine restart`) is still taken and stays `queued`; the resumed agent gets it.

**From inside an agent** (`C9_AGENT_ID` set), `send` connects only to `C9_ENGINE_SOCK` and never starts the engine. `--wait` is refused there: end the turn instead, and the answer arrives as a new message. Every agent's SessionStart context carries this rule.

### `workers` — List Managed Workers

```bash
c9watch workers
c9watch workers --all --pretty
```

```json
{"ok":true,"workers":[{"sessionId":"...","pid":54321,"name":"researcher","cwd":"/path","spawnedAt":"...","alive":true}]}
```

### `hooks` — Report Permission Prompts

```bash
c9watch hooks --install     # register c9watch hooks in ~/.claude/settings.json
c9watch hooks --uninstall   # remove them
```

With no flags, reads one Claude Code hook event from stdin; Claude Code runs it.
See [Claude Code hooks](claude-hooks.md).

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

`view`, `tasks`, and `cost --session` also accept the provider-scoped keys
emitted by `list` and `watch`: `claudeCode:<id>`, `codex:<id>`, or
`cursor:<id>`. This is the explicit form to use when the same raw ID exists
under more than one provider.

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
- **Write path** — `spawn` starts new sessions; in an engine build, `send` mails engine agents. All other commands are read-only. Claude Code's stdin for sessions not managed by c9watch is owned by the terminal process that launched them.

### Public preview data

An engine-enabled build also provides `c9watch export <sessions|cost|history|events|annotations>`.
It reads through c9p and starts core when needed. Use `--format csv`, `--since`,
`--until`, and `--out` for reports. History and events omit conversation fields
unless `--include-conversation` is set. Annotation export needs a core with L1.
See [the export guide](api/extending.md) for exact row and time semantics.

`c9watch api schema [--public]` prints the embedded c9p schema offline. This works
in CLI-only builds too. Raw `api <method> '<json>'` calls still need the engine
feature. See [the preview API guide](api/README.md).

### Engine mail and mailbox controls

In engine builds:

```sh
c9watch mail list [agent] --thread THREAD --before MSG_ID --limit 50 --pretty
c9watch mail read MSG_ID --pretty
c9watch mail cancel MSG_ID
c9watch mail pause AGENT
c9watch mail resume AGENT
c9watch send AGENT --reply-to MSG_ID "reply"
```

Each filter is optional. List/read print JSON by default. `--pretty`
groups messages by thread and shows peer/paste, state, reply links and
history. Reading does not mark a message read by the model. Only a person
may cancel queued mail or pause/resume incoming agent/system mail; human
mail continues. Errors include `not_cancellable` (with current state) and
`hop_limit` (with hop). `send --wait` exits 1 on rejected, expired or
cancelled mail, and keeps waiting on a held detail.

`C9_MAIL_SOCKET` defaults off. Set it to `on` for the experimental CC peer
path; human mail always uses paste. Temporary socket failures never fall
back to paste. A successful write is not delivery; `uncertain` mail is
never resent automatically. Reply through the CLI, not SendMessage. Do
not reply to system mail. See [mail semantics](engine-mail.md).
