# OpenCode monitoring preview

This preview connects to one explicitly configured OpenCode HTTP server. It
adds session cards, the OpenCode filter/badge, parent/child grouping, and
on-demand text/tool-result conversation reads. It does not discover running
servers or inspect OpenCode's local databases/transcripts.

## Try it

1. Start a terminal session with a known port: `opencode --port 4096`.
2. In c9watch Settings → Integration → OpenCode, enter `http://127.0.0.1:4096` and click
   **CONNECT**. If the server requires Basic authentication, enter its username
   (normally `opencode`) and password.
3. Select the **OpenCode** provider filter and open a session card to read its
   conversation.

Connection settings and credentials stay in process memory until c9watch quits.
The desktop's connection status shows authentication, HTTP, malformed-response,
and connection errors. Disconnect clears this provider's cached sessions.

Full `opencode:ses_…` CLI references are read directly, including sessions no
longer present in the monitor's recent-session window.

For startup configuration or CLI use, set `C9WATCH_OPENCODE_URL`, optionally
`C9WATCH_OPENCODE_USERNAME` and `C9WATCH_OPENCODE_PASSWORD`, in the environment
that launches c9watch. CLI session references use `opencode:<session-id>`.

The endpoint must be the server used by the intended OpenCode client. Starting
`opencode serve` separately starts another server; it does not attach to an
existing TUI. Desktop/IDE endpoint discovery is not implemented.

## Behavior and limits

- Background HTTP snapshots refresh every two seconds after the previous
  request finishes. Each request has a three-second timeout and an 8 MiB
  response limit. Redirects are rejected. GUI detection reads the cache without
  waiting on network I/O; one-shot CLI detection waits for a bounded snapshot.
- Unarchived busy/retrying sessions and idle sessions updated within 30 minutes
  appear in the monitor. Deleted/archived/expired sessions disappear on the next
  successful snapshot. These are recent server sessions, not proof that a TUI
  process is still open.
- Status tables are fetched separately for each session directory, matching
  OpenCode's directory-scoped status state.
- `busy` maps to Working; `idle` (including absent entries in the sparse status
  map) maps to WaitingForInput. `retry` and unknown statuses map to Connecting.
- Offline sessions retain their last metadata but become Connecting with the
  actual connection error. Recovery replaces the snapshot and clears the error.
- This preview polls HTTP; SSE, permission/question lifecycle mapping,
  persisted connection settings, history search, usage/cost totals, images,
  prompt sending, opening terminals, stop, and rename are not implemented.
  Permission waits may still show the server's busy status.
- OpenCode sessions advertise no open/stop/rename capabilities and use PID 0.
  Provider-scoped identity keeps them separate from Claude/Codex/Cursor/Pi.

Conversation reads use `limit` / `before` pagination and `X-Next-Cursor`.
Pages are reassembled in chronological order and tool filtering occurs per
page. Oversized pages are retried with smaller page sizes. The 8 MiB limit
applies to each page; a single message larger than that returns an explicit
error. Repeated cursors and loads exceeding one minute are rejected. Initial
conversation errors appear in the panel with a Retry action. Only OpenCode
previews refresh on a two-second timer; local providers load on selection or
manual retry without periodically reparsing unchanged transcripts. Legacy
ID-only conversation lookup includes OpenCode and still rejects cross-provider
ID collisions.

## Native rendering

Native Tauri fade/fly/scale/slide transitions complete immediately. WebKit can
suspend its animation timeline when a window becomes hidden, even after an
intro starts, leaving content at opacity zero while accessibility updates
continue. The message bubble's duplicate CSS fade is removed. Browser
transitions retain motion when visible and respect reduced motion.

OpenCode uses the existing pink accent (`#FF69B4`) in badges and filter markers.

## Verification

Development testing used the official OpenCode 1.18.29 npm binary with isolated
XDG directories and a loopback server. Session/status reads, a real model reply,
and a 23-message conversation across multiple pages were checked. Synthetic
HTTP tests cover directory-scoped status, stale/busy retention, authentication,
HTTP errors/recovery, pagination above 8 MiB total, cursor loops, and filtering.
Full provider-qualified CLI IDs can be read even when absent from discovery.

Native QA of the development bundle exercised Working/Ready across directories,
HTTP 503 with Retry, recovery, live conversation updates, and visible English /
Chinese conversation content after the animation repair. The PR branch is based
on current main and places provider integrations in Settings → Integration:
Claude Code, Codex, Cursor, and Pi use local automatic detection, while OpenCode
keeps its explicit HTTP connection form. Computer Use verified the renamed
navigation item and the rendered Integration page in the independent PR bundle.
It contains no temporary render diagnostics or messaging bridge.

Automated checks include provider isolation, initial-error retry and stale
responses, bounded live conversation windows, and native/browser transition
policy. This is a development preview, not a signed release. Permission/question
mapping and other unsupported capabilities listed above remain out of scope.

On the isolated PR branch, default Rust tests passed (434 passed, 6 ignored),
including doc-test completion; CLI-only tests also passed. Frontend check/build
and provider, conversation, transition, WebSocket, and existing polling checks passed.
