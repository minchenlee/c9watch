# CLI events: `watch` and `wait`

`c9watch watch` streams one JSON event per line while sessions change.
`c9watch wait` blocks until one session meets a condition, prints that one
event, and exits. Both use the same event rules (`src-tauri/src/cli/watch.rs`),
so a script can switch between them without new parsing.

Neither command needs Herdr, tmux, or a terminal. Use them in place of
reading a terminal pane in a loop.

## Event line

Every line is a complete JSON object.

| Field | Type | In | Meaning |
|---|---|---|---|
| `schema` | number | all | Format version. Now `1`. It changes only for a breaking change; new fields can appear without a change. |
| `event` | string | all | `started`, `status_changed`, or `stopped`. |
| `sessionId` | string | all | The agent's own session id. Two agents can use the same id. |
| `sessionKey` | string | all | `provider:sessionId`. Unique. Use this to tell sessions apart. |
| `timestamp` | string | all | RFC 3339 time of the poll. |
| `status` | string | `started`, `status_changed` | `Working`, `NeedsAttention`, `WaitingForInput`, or `Connecting`. |
| `previousStatus` | string | `status_changed`, `stopped` | Status in the previous poll. |
| `provider` | string | `started`, `status_changed` | `claudeCode`, `codex`, `cursor`, `pi`, or `opencode`. |
| `session` | object | `started`, `status_changed` | The session as `c9watch list` shows it (`--compact` gives the short form; `wait` always uses the short form). |

### Events

| Event | When |
|---|---|
| `started` | The session is seen for the first time. `watch --changes-only` skips these. |
| `status_changed` | `status` or the pending tool (`session.pendingToolName`) changed since the previous poll. The status can stay the same when only the tool changed. |
| `stopped` | The session is no longer in the live list (the process ended, or the session went stale). Sent once. |

Statuses:

| Status | Meaning |
|---|---|
| `Working` | The agent runs tools or thinks. |
| `NeedsAttention` | The agent waits for you: a permission prompt or a question. |
| `WaitingForInput` | The agent finished its turn and waits for your next prompt. |
| `Connecting` | The session is starting, or has had no activity yet. |

### Example

The `session` object is shortened here.

```json
{"schema":1,"event":"status_changed","sessionId":"2503881a-33cc-4544-b71d-4f5f4ba6fe5e","sessionKey":"claudeCode:2503881a-33cc-4544-b71d-4f5f4ba6fe5e","status":"NeedsAttention","previousStatus":"Working","provider":"claudeCode","session":{"id":"2503881a-…","status":"NeedsAttention","pendingToolName":"Bash","projectPath":"/Users/me/spark","sessionName":"spark"},"timestamp":"2026-10-14T14:50:02+00:00"}
{"schema":1,"event":"stopped","sessionId":"2503881a-33cc-4544-b71d-4f5f4ba6fe5e","sessionKey":"claudeCode:2503881a-33cc-4544-b71d-4f5f4ba6fe5e","previousStatus":"WaitingForInput","timestamp":"2026-10-14T15:02:40+00:00"}
```

## `watch`

```bash
c9watch watch                          # started burst, then changes, every 2 s
c9watch watch --json                   # same output; --json is accepted for scripts
c9watch watch --compact --changes-only # only changes, short session objects
c9watch watch --project myapp -i 5     # one project, every 5 s
```

Output is always NDJSON. `--json` changes nothing.

## `wait`

```bash
c9watch wait <session> --until <condition> [--timeout <seconds>] [--interval <seconds>]
```

`<session>` is a session key (`codex:…`), a session id, or a unique prefix of
either, like the other commands. It must match exactly one **live** session
(one that `c9watch list` shows).

| Condition | Met when |
|---|---|
| `needs-input` | The status is `NeedsAttention` or `WaitingForInput`. Met at once if it already is. |
| `done` | The status changes from `Working` to anything else, or the session stops. **Met at once if the session is not `Working` at the start** (including `Connecting`). |
| `ended` (alias `stopped`) | A `stopped` event. |
| `any-change` | Any `status_changed` or `stopped` event after the first poll. |

When the condition is met, `wait` prints the event that met it (one line,
short `session` object). When it is met at once, that event is `started`.

| Exit code | Meaning |
|---|---|
| `0` | The condition is met. The event is on stdout. |
| `2` | No live session matches, or more than one does. The error is JSON on stderr. |
| `3` | The session stopped while waiting for `needs-input`. The `stopped` event is on stdout. |
| `124` | `--timeout` ran out (same code as `timeout(1)`). Nothing on stdout. |
| `1` | Other error, such as the session list cannot be read. JSON on stderr. |

Timing: one poll reads every session, so it takes about as long as
`c9watch list` (about 2–4 s with 30+ sessions in a release build; a debug
build is about ten times slower). `--timeout` is exact even while a poll is
running. With a timeout shorter than one poll, `wait` exits `124` before it
can check the session.

## Shell example: notify me when a session needs me

```bash
#!/bin/sh
# Usage: notify-when-needed.sh <session>
session="$1"
event=$(c9watch wait "$session" --until needs-input)
case $? in
  0)
    name=$(printf '%s' "$event" | jq -r '.session.sessionName')
    status=$(printf '%s' "$event" | jq -r '.status')
    osascript -e "display notification \"$name: $status\" with title \"c9watch\""
    ;;
  3) echo "session ended before it needed you" ;;
  2) echo "no single live session matches $session" >&2; exit 2 ;;
esac
```

Wait for a worker to finish its turn, for up to 10 minutes:

```bash
c9watch wait codex:01a13add --until done --timeout 600 || echo "still working"
```
