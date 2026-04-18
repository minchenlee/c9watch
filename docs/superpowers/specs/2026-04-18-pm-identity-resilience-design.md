# PM identity resilience — design

**Date:** 2026-04-18
**Status:** Design, ready for plan
**Depends on:** PR #84 (PM orchestration Phase 1)

## Problem

A PM Claude Code session's identity (its session UUID) is not stable. Two common events change it:

1. **`/clear` or same-process new session.** Same CC process PID, new UUID.
2. **CC process restart.** New PID, new UUID.

PR #84 freezes `spawnedBy: <pm-uuid>` on `meta.json` at spawn time and keys the inbox at `~/.claude/c9watch/inbox/<pm-uuid>/`. When the PM's UUID changes:

- `c9watch workers` (filtered by caller's current PM UUID via parent-PID walk) shows nothing.
- `c9watch inbox` reads from the new UUID's dir (empty), while turn-end events keep landing in the old UUID's dir.
- The PM has no documented way to recover the link to its still-alive workers.

## Goals

1. `/clear` must work with zero user action — same-process new session still sees its workers.
2. CC restart has an explicit, auditable recovery path (`adopt`).
3. Old inbox events are never lost — they remain readable after identity change.
4. Minimal write amplification. `meta.json` stays immutable after spawn.
5. No shared-machine footguns: visibility is free, but taking ownership of another PM's worker requires intent.

## Non-goals

- Automatic cross-machine adoption.
- Recovery when the worker process itself died (out of scope — that's a different feature).
- Attribution across multiple overlapping PM processes owning the same worker.

## Design

### 1. Worker-keyed inbox

Change the inbox layout from PM-keyed to worker-keyed:

```
before:  ~/.claude/c9watch/inbox/<pm-uuid>/<event>.json
after:   ~/.claude/c9watch/inbox/<worker-session-id>/<event>.json
```

Rationale: the PM UUID is unstable; the worker session ID is not. Keying by worker means old events survive identity change for free.

**Writer change.** `stdout_tee_task` in `pm_worker.rs` writes to `inbox_worker_dir(worker_session_id)` instead of `inbox_pm_dir(pm_session_id)`. The worker knows its own session ID (already passed in construction); it no longer needs `spawnedBy` for the write path.

**Reader change.** `cmd_inbox`:
1. Compute the list of workers the current PM owns (§2 + §3).
2. Read events from each owned worker's `inbox/<worker-id>/` dir.
3. Merge, sort by `finished_at` descending, return.

`--consume` deletes only the event files that were just returned (already the post-C1-fix behavior). `--clear` without a worker filter clears all owned workers' inboxes; with a worker filter, only that worker.

### 2. PID-based default ownership

`meta.json` gains one field:

```jsonc
{
  "sessionId": "...",
  "pid": 61407,
  "spawnedBy": "<pm-uuid-at-spawn>",    // unchanged, audit trail
  "pmPid": 55123,                       // NEW: PM's CC process PID at spawn
  "spawnedAt": "...",
  // ...
}
```

`pm_pid` is the caller's Claude CC process PID, detected by `pm_caller::get_parent_claude_pid()` (walks parent PID chain, matches against `~/.claude/sessions/<pid>.json`). The daemon captures this during `handle_spawn`.

`meta.json` remains immutable after spawn. `pm_pid` never updates.

**Ownership rule (default case).**

A PM owns a worker if either:
- `meta.pm_pid == current_pm_pid`, or
- worker is listed in the PM's adoption sidecar (§3).

Case A (`/clear`, same process): `pm_pid` matches → worker visible, zero writes.
Case B (CC restart): `pm_pid` doesn't match → user runs `adopt`, writes sidecar entry.

### 3. Adoption sidecar

```
~/.claude/c9watch/adoptions/<current-pm-uuid>.json
```

Shape:

```json
{
  "pmSessionId": "<uuid>",
  "adoptedAt": "2026-04-18T10:00:00Z",
  "workerIds": ["<worker-session-id-1>", "<worker-session-id-2>"]
}
```

Created on first `c9watch adopt <worker-id>`. Additional adopts append to `workerIds`.

**Lazy GC on read.** When a command reads `adoptions/<pm-uuid>.json`, it filters out worker IDs whose `workers/<id>/meta.json` no longer exists (worker was stopped and cleaned up). If the resulting list is empty, the file is deleted. Otherwise the file is rewritten with the filtered list. Read-time cost is one stat per listed worker.

### 4. `workers --all` with status column

New flag on `c9watch workers`:

```
c9watch workers            # default: only workers owned by current PM
c9watch workers --all      # all live workers, with status column
```

Output with `--all`:

```jsonc
{
  "ok": true,
  "workers": [
    {
      "sessionId": "...",
      "pid": 61407,
      "spawnedBy": "<pm-uuid-at-spawn>",
      "pmPid": 55123,
      "status": "OWNED_BY_YOU" | "OWNED_BY_OTHER_PM" | "ORPHANED",
      // ... rest of fields as today
    }
  ]
}
```

Status rules:
- `OWNED_BY_YOU`: worker is in current PM's ownership set (per §2).
- `ORPHANED`: `meta.pm_pid` does not point to any live PM process on this machine AND worker is not in any adoption sidecar.
- `OWNED_BY_OTHER_PM`: otherwise (some other live PM owns it via PID or adoption).

### 5. `c9watch adopt <worker-id>`

```
c9watch adopt <worker-id-or-prefix>         # refuses non-ORPHANED
c9watch adopt <worker-id-or-prefix> --force # overrides refusal
```

Behavior:
1. Resolve worker-id-or-prefix against `workers/` dir (same resolution rules as `send`/`stop`).
2. Compute current status (OWNED_BY_YOU / OWNED_BY_OTHER_PM / ORPHANED).
3. If OWNED_BY_YOU: no-op, exit 0 with a message.
4. If ORPHANED: append to `adoptions/<current-pm-uuid>.json`.
5. If OWNED_BY_OTHER_PM without `--force`: refuse with error `WORKER_OWNED_BY_OTHER_PM` and a hint about `--force`.
6. If OWNED_BY_OTHER_PM with `--force`: append to `adoptions/<current-pm-uuid>.json`. The previous PM's sidecar is **not** modified — it still lists this worker, but its ownership rule will now match neither PID nor the same adoption record, depending on whether that PM is still alive. (Accept the ambiguity; document that `--force` is adversarial.)

Emits a success response:

```json
{
  "ok": true,
  "adopted": "<worker-id>",
  "pmSessionId": "<current-pm-uuid>"
}
```

### 6. Response schema additions

`spawn`, `send`, `workers` responses keep their current shape. Two additions:

- `workers` (non-`--all`) gains `pmPid` passthrough for debug visibility.
- `workers --all` adds the `status` field per-entry.
- New `adopt` response above.

The `callbackInbox` hint in spawn/send responses stays pointing at `~/.claude/c9watch/inbox/` (generic), but the docs clarify that events are under `<worker-id>/` subdirs, not `<pm-uuid>/`.

## Data flow

### Spawn (unchanged externally)

```
PM calls `c9watch spawn`
  → daemon detects PM UUID + PM PID via pm_caller
  → writes meta.json with pmPid field
  → worker's stdout_tee writes inbox events to inbox/<worker-id>/  [changed]
```

### Inbox read after /clear (Case A)

```
PM runs `/clear` — PM UUID changes, PM PID unchanged
  → PM calls `c9watch inbox`
  → daemon resolves ownership: meta.pm_pid == current_pm_pid → match
  → reads inbox/<worker-id>/*.json for each owned worker
  → returns merged events
```

Zero writes. Old events still visible because they're keyed by worker, not PM.

### Inbox read after CC restart (Case B)

```
User restarts CC — PM UUID + PM PID both change
  → PM calls `c9watch workers --all`
    → daemon lists all workers; current worker status = ORPHANED (old pmPid dead)
  → PM calls `c9watch adopt <worker-id>`
    → daemon writes adoptions/<new-pm-uuid>.json with this worker
  → PM calls `c9watch inbox`
    → ownership resolved via adoption sidecar → match
    → reads inbox/<worker-id>/*.json
    → returns merged events
```

One write (adoption sidecar). Old events still visible.

## Error handling

- `adopt` with unknown worker ID: `WORKER_NOT_FOUND` (reuse existing code).
- `adopt` on OWNED_BY_OTHER_PM without `--force`: new code `WORKER_OWNED_BY_OTHER_PM`.
- `adopt` on already-owned worker: exit 0 with info message, not error.
- Adoption sidecar corrupted (bad JSON): log warning, treat as empty, overwrite on next adopt.
- `workers --all` when daemon not running: same as `workers` — RPC unreachable error.

## Testing

**Unit tests.**
- `pm_fs::validate_session_id` coverage remains.
- New `pm_fs::inbox_worker_dir` path helper + path-traversal test.
- New `adoption::read_filter`: lazy GC drops workers whose meta.json is gone.
- Ownership resolver: PID match, adoption match, neither (ORPHANED).

**Smoke tests.**
- Existing 7 tests unaffected (inbox path change is internal; worker-id keying doesn't touch test surface).
- New test 8: spawn worker, simulate PM UUID change (mock pm_caller to return new UUID same PID), assert `c9watch inbox` still returns the pre-change event.
- New test 9: spawn worker, kill PM PID (mock sessions file cleanup), assert `c9watch workers --all` reports ORPHANED status, `adopt` succeeds without `--force`, subsequent `inbox` returns the event.
- New test 10: spawn worker under PM1, start fake PM2, assert PM2 sees OWNED_BY_OTHER_PM and `adopt` refuses without `--force`.

## Migration

PR #84 hasn't merged. This design rewrites the inbox layout before any real users encounter the old layout. No runtime migration needed — we ship the new layout as the initial layout. The PR #84 branch picks up the new layout before merge.

## Out of scope (follow-ups)

- Automatic GUI surfacing of adoption (e.g., notification: "3 orphaned workers — adopt?"). The CLI lands first; GUI follows.
- Cross-machine adoption (workers running on a remote machine).
- Worker re-attach after the worker's own CC process dies (different problem).

## Files changed (preview)

- `src-tauri/src/cli/pm_fs.rs` — add `inbox_worker_dir`, deprecate `inbox_pm_dir`; add `adoption_file` path helper
- `src-tauri/src/cli/pm_inbox.rs` — read/write by worker ID; merge-across-workers logic moves to daemon
- `src-tauri/src/cli/pm_worker.rs` — `stdout_tee_task` writes to worker-keyed path
- `src-tauri/src/cli/pm_daemon.rs` — new `handle_adopt`, ownership resolver, `workers --all` status enrichment; `handle_inbox` reads across owned workers
- `src-tauri/src/cli/pm.rs` — new `cmd_adopt`; `cmd_workers` accepts `--all`; `cmd_inbox` unchanged from CLI perspective
- `src-tauri/src/cli/mod.rs` — new `Adopt` subcommand; `Workers` gains `--all`
- `src-tauri/src/cli/adoption.rs` — **new file** — read/write adoption sidecar with lazy GC
- `src-tauri/tests/pm_smoke.sh` — tests 8/9/10 above
- Docs: update PM orchestration docs to describe adopt + inbox-by-worker keying
