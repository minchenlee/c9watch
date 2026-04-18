# PM Inbox — async callbacks from workers

When a PM session spawns a worker via `c9watch spawn`, the daemon writes an event
to the PM's inbox every time the worker's turn ends (success, error, or crash).
Inbox events live at `~/.claude/c9watch/inbox/<pm-session-id>/<event-id>.json`.

PMs read events on their own schedule:

```bash
c9watch inbox              # list pending events, newest first (non-blocking)
c9watch inbox --consume    # list + remove
c9watch inbox --clear      # remove all without listing
c9watch inbox --pm-id <id> # override auto-detection
```

Each event includes: `status` (done | error | crashed), `sessionId`, `finishedAt`,
`durationMs`, `numTurns`, `stopReason`, `totalCostUsd`, and a bounded `resultExcerpt`
(≤500 chars). Full worker transcripts remain at
`~/.claude/c9watch/workers/<session-id>/stdout.log`.

No callback is emitted for workers spawned by a human caller (no detectable
`spawnedBy`). Humans see workers in the GUI instead.
