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
longer present in the monitor's recent-session window. Prefer the complete
directory-qualified identity from discovery, for example
`opencode:ses_…?directory=%2Fmy%2Fproject` (quote this argument in a shell).
The local identity includes the directory; the HTTP path uses the original ID
and sends `directory` as a query parameter. Known ambiguous bare IDs are rejected.

For startup configuration or CLI use, set `C9WATCH_OPENCODE_URL`, optionally
`C9WATCH_OPENCODE_USERNAME` and `C9WATCH_OPENCODE_PASSWORD`, in the environment
that launches c9watch. CLI session references use `opencode:<session-id>`.

The endpoint must be the server used by the intended OpenCode client. Starting
`opencode serve` separately starts another server; it does not attach to an
existing TUI. Desktop/IDE endpoint discovery is not implemented.

## Behavior and limits

- Background HTTP snapshots refresh every two seconds after the previous
  operation finishes on a dedicated thread. Each request has a three-second
  timeout and an 8 MiB response limit. The complete snapshot has a ten-second /
  16 MiB budget, at most 512 sessions and 32 directories. Overflow is an error,
  not an apparently complete partial list. Redirects are rejected. GUI detection
  reads the cache without waiting on network I/O; one-shot CLI detection waits
  for a bounded snapshot. Health is checked before discovery/status reads.
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
  Provider- and directory-qualified identity keeps them separate from each
  other and from Claude/Codex/Cursor/Pi. Detail reads validate the returned ID,
  directory and archive state. On-demand read-only children requests validate
  parent/directory and share the conversation admission gate; ordinary discovery
  groups the list's parent IDs without an extra request for each parent.

Conversation reads use `limit` / `before` pagination and `X-Next-Cursor`.
Pages are reassembled in chronological order and tool filtering occurs per
page. Oversized pages are retried with smaller page sizes. The 8 MiB limit
applies to each page; a single message larger than that returns an explicit
error. Each load additionally has a 32 MiB total wire budget (including detail
and oversized retries), 128-page bound, 10,000 rendered-part bound and a
one-minute deadline. Ignored page limits, malformed messages, repeated/empty
cursors and cursors longer than 4096 bytes are rejected. Initial
conversation errors appear in the panel with a Retry action. Only OpenCode
previews refresh on a two-second timer; local providers load on selection or
manual retry without periodically reparsing unchanged transcripts. Legacy
ID-only conversation lookup includes OpenCode and still rejects cross-provider
ID collisions. The DOM window is capped at 400 messages (200-message batches).

One conversation/children HTTP operation is admitted at a time; overlapping
loads fail immediately with a retryable error instead of waiting in a queue.
The last successful conversation alone is cached for one second, scoped by
connection, session and tool visibility. Errors do not enter the cache.
Disconnect/reconnect invalidates cache generations and cancels old operations;
an already-running HTTP request can take up to its remaining three-second
timeout to close, but cannot publish stale results or start another page.
Responses are dropped on success/error/cancellation; each client retains at
most one idle socket per host for five seconds. No SSE/socket subscription is
opened. Settings status polling and preview polling each await completion
before scheduling another poll.

Synchronous discovery/enrichment, subagent scans and subagent transcript reads
run in the blocking pool behind separate one-job, fail-fast admission gates.
Permits stay with the job even if its async caller is cancelled. This preserves
the existing scanner ownership and keeps disk/JSON work off Tokio async workers.

## Native rendering

Native Tauri fade/fly/scale/slide transitions complete immediately. WebKit can
suspend its animation timeline when a window becomes hidden, even after an
intro starts, leaving content at opacity zero while accessibility updates
continue. The message bubble's duplicate CSS fade is removed. Browser
transitions retain motion when visible and respect reduced motion.

OpenCode uses the existing pink accent (`#FF69B4`) in badges and filter markers.

## Verification

See [the dated candidate acceptance record](opencode-preview-acceptance-2026-09-12.md)
for exact revisions, commands, measured performance, observed native rendering
and remaining gates. Historical PR text is not current acceptance evidence.
Current compatibility checks used an isolated OpenCode **1.18.20** server for
health, empty discovery, status and `/doc` schema reads. No real model request
was made during this audit; nonempty conversations and failure modes use the
committed loopback fixture. This is a development preview, not a distribution-
signed, notarized or deployed release.
