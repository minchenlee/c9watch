# PM Worker Backends

c9watch ships two PM-worker backends:

- **`bg` (default on CC >= 2.1.150)**: workers spawn via the hidden
  `claude --bg` flag and are managed by the CC supervisor daemon. They
  appear in `claude agents --json` as `kind:"background"`. Billing goes
  against your Pro/Max chat subscription quota.
- **`print` (legacy fallback)**: workers spawn via `claude --print` and
  are managed entirely by c9watch's own daemon. Billing goes against the
  Agent SDK credit pool ($20 Pro / $100 Max 5x / $200 Max 20x) at full
  API rates after Anthropic's 2026-06-15 split.

## Selection

- `C9WATCH_WORKER_BACKEND=bg` — force bg backend
- `C9WATCH_WORKER_BACKEND=print` — force print backend (escape hatch)
- `C9WATCH_WORKER_BACKEND=auto` or unset — auto-detect

Auto-detect picks `bg` when both:
1. `claude --version` >= 2.1.150
2. `/tmp/cc-daemon-{uid}/{host}/control.sock` exists (daemon has run)

## Bg backend internals

- Spawn: `claude --bg --session-id <uuid> --name <id> --permission-mode <m> [...] "<prompt>"`. Initial prompt REQUIRED (idle-spawn + later `reply` is unreliable). Pass via `c9watch spawn --prompt "<text>"`.
- Send: control.sock `{op:"reply", short, text}` one-shot RPC.
- Events: dedicated UDS per worker, `{op:"subscribe", short}` push stream. `state:"done"` and `state:"blocked"` both count as turn-end.
- Kill: control.sock `{op:"kill", short}` RPC + parallel `claude rm <short>` subprocess for jobs-dir cleanup.

## Limitations

- `claude --bg` flag is undocumented; CC may rename in future releases. We pin to >= 2.1.150 and fail-soft to print backend on probe error.
- Subscribe streams don't multiplex — N workers = N file descriptors. Acceptable at N ≤ 16.
- `BgWorkerHandle` holds the daemon state mutex while `wait_for_turn` is awaiting events. Other RPCs serialize behind a pending `--wait`. Acceptable for current single-PM dogfooding; revisit if multiple concurrent `--wait`s become common.
