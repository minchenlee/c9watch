# M1 Technical Acceptance Report

Date: 2026-09-24
Base: `main` / `origin/main` = `1ae72b3`
Worktree: `/private/tmp/c9watch-m1-attention-mvp`, branch `m1/attention-mvp`
Status: **unvalidated product build** (M0 participant evidence waived).
Requirements: `m1-provider-surface-matrix.md` (M1-R1..M1-R8).

## Verdict

**CONFORMS WITH GAPS** (independent verifier, final round).

| ID | Status |
| --- | --- |
| M1-R1 declared matrix | PASS |
| M1-R2 evidence + health | PASS |
| M1-R3 identity / dedup | PASS |
| M1-R4 declared return guarantee | PASS for capability/selection contract; exact live native focus UNVERIFIED |
| M1-R5 live end-to-end handling | UNVERIFIED (not authorized: no live providers) |
| M1-R6 participant baseline comparison | PASS (correctly held open, no claim made) |
| M1-R7 metric targets | PASS (kept as targets, no claim made) |
| M1-R8 no scope expansion | PASS |

Live M1-R4 native focus and M1-R5 remain unverified without running live
providers / OS sessions. IDE project open cannot satisfy exact terminal return:
its declared guarantee is project only, with manual terminal selection.
The historical verification runs below are evidence from that candidate,
not acceptance of subsequent fixes or live behavior.

## Verification runs (final round, observed by verifier)

- `cargo test --locked --offline --lib -- --test-threads=1`: **492 passed, 0 failed, 9 ignored** (501 total)
- `npm run check`: **0 errors, 0 warnings**
- `npm run build`: **passed**
- `cargo check --no-default-features --features cli`: **passed** (existing warnings only)
- `cargo check --examples`: **passed** (existing warnings only)
- `git diff --check`: **passed**
- No installs, live providers, participants, commit, or push.

## Independent review history

- Round 1: 1 Critical (example fixture) + 5 warnings — all fixed.
- Round 2: 4 warnings — all fixed.
- Round 3: 1 Critical (both-unparseable dedup fallback) + test/doc gaps — all fixed.
- Round 4 (final): **zero Critical findings**; one test-comment quality warning, fixed and re-tested.

## What M1 changed (candidate diff)

- `SourceHealth` contract (fresh/stale/partial/unavailable/unknown), derived per
  provider from that provider's own freshness windows; rendered on session cards.
- Provider-scoped `attentionInbox`: `NeedsAttention` only, newest-observation-wins
  across all observations, each item carrying provider/surface/health/observed
  time/reason/`returnKind`; monitor header badge dispatches the newest item.
- Backend duplicate identities collapse newest-wins by parsed instant.
- Return guarantees: provider-qualified conversation selection with stale/foreign
  response rejection. Terminal/iTerm2 on macOS use exact tty selection and fail
  when the tty/tab cannot be selected. VS Code/Cursor/Windsurf/Zed CLI and
  JetBrains URL openers target only the project, never a specific IDE terminal.
  Other native fallback surfaces declare application only. Project/application
  attention actions also select the exact conversation and disclose manual
  terminal selection. Unknown/older capabilities and Codex/Pi use conversation
  only; no unreliable terminal-selection mechanism was added.
- Cursor/OpenCode changes are additive health plumbing only, excluded from M1
  acceptance per the matrix.

## Out-of-scope follow-up fix (same branch)

- Cursor cold-start stall: `CursorSessionSource` did a synchronous full-table
  scan of Cursor's global `state.vscdb` (observed at 1.5GB) on every detect,
  blocking first paint for minutes. The composer overlay now loads in a
  background thread with stamp-based caching and a 300ms grace wait
  (`cursor.rs` `ComposerCache`): no full-table scan blocks first paint;
  detection can wait up to 300ms for the overlay before returning. Titles
  backfill on the next poll if the background load is still pending.
  Existing cursor overlay tests pass
  unchanged (small test DBs resolve within the grace wait).

## Pi liveness fix (F1, same branch)

- Problem: a killed Pi session (for example `kill -9` during a tool call)
  stayed LIVE and Working for up to the 4h freshness window, because Pi
  has no pid file, lock or session variable.
- Fix: process evidence (`src-tauri/src/session/pi_liveness.rs`). Only
  while a Pi transcript is fresh, the detector lists live `pi` processes
  (sysinfo reports `name()` = `node`; argv[0] = `pi` after pi's title
  rewrite) with cwd and start time. Per exact header cwd, each live
  process keeps one fresh transcript modified after its start (10 s
  grace). Processes left over after that per-cwd pass may each keep one
  more transcript from another cwd, so a session resumed from another
  project stays alive. Other transcripts end and leave the monitor like
  expired ones;
  history keeps them. Listing failure, an unreadable pi cwd/argv, or a
  lossy decoded cwd keep the previous mtime behaviour. The 4h / 30m
  windows stay the upper limit.
- Verification (2026-10-06, this machine, after the cross-project resume
  fix): `cargo test --lib` **523 passed, 0 failed, 10 ignored**; `npm run check` **0 errors, 0
  warnings**; `git diff --check` passed. Manual probe with a stand-in
  process (`node` with `process.title = 'pi'`, 654 processes listed):
  first gated scan 81 ms, second 66 ms, then 7–9.5 ms per poll.
- Known limits: a session resumed with `pi -c` / `--resume` shows as
  ended until its next write (pi 0.87.1 loads the transcript without
  writing to it); transcripts are matched to processes by cwd and time
  only, and one process keeps at most one transcript; a killed card can
  flicker back for one poll when an unreadable node/bun row makes that
  poll fall back to the mtime windows; live acceptance with a real Pi
  session was not run (needs owner approval for model calls).

## Explicitly not claimed

- Live provider detection quality, OS notification presentation, end-to-end
  originating-environment handling (M1-R5).
- Participant baseline, workflow-shortening, precision/recall/latency/four-week-use
  (M1-R6/R7 — held open, no numbers reported).
- The strategy plan's progress checkboxes in the separate strategy checkout were
  not edited here; this report is the M1 final-evidence artifact on the
  implementation branch.
