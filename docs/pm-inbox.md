# PM Inbox — async callbacks from workers

When a PM session spawns a worker via `c9watch spawn`, the daemon writes an event
to the worker's inbox every time its turn ends (success, error, or crash).
Inbox events live at `~/.claude/c9watch/inbox/<worker-session-id>/<event-id>.json`.

Keying by worker (stable) instead of PM session UUID (unstable across `/clear`
and CC restarts) means old events survive PM identity change for free. The
daemon resolves which workers the current caller owns and merges across them.

PMs read events on their own schedule:

```bash
c9watch inbox                     # list pending events, newest first (non-blocking)
c9watch inbox --consume           # list + remove
c9watch inbox --clear             # remove all without listing
c9watch inbox --worker <id>       # only this worker's inbox (must be owned)
```

Each event includes: `status` (done | error | crashed), `sessionId`, `finishedAt`,
`durationMs`, `numTurns`, `stopReason`, `totalCostUsd`, and a bounded `resultExcerpt`
(≤500 chars). Full worker transcripts remain at
`~/.claude/c9watch/workers/<session-id>/stdout.log`.

## Ownership

A PM owns a worker if either:

- `meta.pmPid` (captured at spawn) matches the caller's current Claude CC process
  PID — covers the `/clear` case (same process, new UUID), or
- the worker is listed in the PM's adoption sidecar at
  `~/.claude/c9watch/adoptions/<pm-session-id>.json` — covers the CC-restart case.

`meta.json` is immutable after spawn. The adoption sidecar is only written when
the user runs `c9watch adopt`. It's garbage-collected lazily on read: entries
whose `workers/<id>/meta.json` is gone are dropped; empty sidecars are deleted.

## Workers visibility

```bash
c9watch workers        # workers owned by the current PM
c9watch workers --all  # all live workers, each tagged with a status column
```

Status values (only shown with `--all`):

- `OWNED_BY_YOU` — matches on PID or adoption sidecar.
- `OWNED_BY_OTHER_PM` — another live PM owns it via PID or its own sidecar.
- `ORPHANED` — no live PM has a claim.

## Adopting a worker after CC restart

```bash
c9watch adopt <worker-id-or-prefix>          # refuses OWNED_BY_OTHER_PM
c9watch adopt <worker-id-or-prefix> --force  # overrides the refusal
```

`adopt` is a no-op when the worker is already `OWNED_BY_YOU`. For `ORPHANED`
workers it writes (or appends to) the caller's adoption sidecar. For
`OWNED_BY_OTHER_PM` it refuses with error `WORKER_OWNED_BY_OTHER_PM` unless
`--force` is passed; forced adoption is adversarial and leaves the previous
PM's sidecar intact.

No callback is emitted for workers spawned by a human caller (no detectable
`spawnedBy`). Humans see workers in the GUI instead.
