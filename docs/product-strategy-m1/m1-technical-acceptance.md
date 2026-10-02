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
- Round 2: 1 Critical (`pending_tool_input` via Tauri/WS serialization) + 4 warnings — all fixed.
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
- Privacy: `pending_tool_input` is `#[serde(skip)]` — tool arguments leave through
  no serialization path; CLI contract carries `sourceHealth`; docs updated.
- Cursor/OpenCode changes are additive health plumbing only, excluded from M1
  acceptance per the matrix.

## Out-of-scope follow-up fix (same branch)

- Cursor cold-start stall: `CursorSessionSource` did a synchronous full-table
  scan of Cursor's global `state.vscdb` (observed at 1.5GB) on every detect,
  blocking first paint for minutes. The composer overlay now loads in a
  background thread with stamp-based caching and a 300ms grace wait
  (`cursor.rs` `ComposerCache`): first paint never waits on the database,
  titles backfill on the next poll. Existing cursor overlay tests pass
  unchanged (small test DBs resolve within the grace wait).

## Explicitly not claimed

- Live provider detection quality, OS notification presentation, end-to-end
  originating-environment handling (M1-R5).
- Participant baseline, workflow-shortening, precision/recall/latency/four-week-use
  (M1-R6/R7 — held open, no numbers reported).
- The strategy plan's progress checkboxes in the separate strategy checkout were
  not edited here; this report is the M1 final-evidence artifact on the
  implementation branch.
