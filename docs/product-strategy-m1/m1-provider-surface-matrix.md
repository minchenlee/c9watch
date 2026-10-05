# M1 Provider/Surface Matrix (declared acceptance boundary)

Base: `main` / `origin/main` = `1ae72b3`.
Worktree: `/private/tmp/c9watch-m1-attention-mvp`, branch `m1/attention-mvp`.
Status: **unvalidated product build** (M0 participant evidence waived; see `docs/product-strategy-m0/waiver-and-m1-entry-2026-09-23.md` in the strategy checkout).

## M1-owned providers

Claude Code (`claudeCode`), Codex (`codex`), Pi (`pi`).

Cursor and OpenCode exist in the mainline base but are **excluded from M1 acceptance**.
M1 changes are additive/optional and must not claim new Cursor/OpenCode coverage.

## Requirement IDs

| ID | Requirement (from `docs/product-strategy-m1/plan.md`) |
| --- | --- |
| M1-R1 | A declared matrix covers supported high-confidence approval/question waits and relevant non-actionable controls. |
| M1-R2 | Every attention item exposes source evidence and health/freshness explaining why it is actionable. |
| M1-R3 | Multiple observations of one session produce one item; same-ID sessions across providers and reconnect/reuse cases do not merge unrelated sessions. |
| M1-R4 | Selecting an item exposes the guaranteed target: exact tty/conversation, project only, or application only; no claim that an IDE project open selects the originating terminal. |
| M1-R5 | A high-confidence wait is handled end to end in the originating environment (synthetic dispatch alone does not prove live behavior). |
| M1-R6 | Product comparison against a participant baseline stays unmeasured; no workflow-shortening claim. |
| M1-R7 | Precision/recall/latency/four-week-use targets stay targets until measured. |
| M1-R8 | No new providers/runtimes, agent control, coordination, handoff, or usage expansion. |

## Signal matrix

### Claude Code

- Surfaces: `claudeCode` (CLI-sourced placeholder sessions; `src-tauri/src/session/detector_cli.rs:85,93`).
- High-confidence waits (`NeedsAttention`):
  - pending `AskUserQuestion` tool → immediate `NeedsAttention` (`src-tauri/src/session/status.rs:100-104`, tests at `1160-1178`).
  - assistant text question after the 20s grace period (`status.rs:106`, tests at `1097-1140`).
  - pending tool needing permission (not auto-approved) (`status.rs:262-268`, tests at `623-641`, `707-737`).
- Evidence: `pendingToolName` = tool name / `AskUserQuestion` / `Question` (`status.rs:321-381`); assistant-only `notification_preview` (`enrichment.rs:52-60`).
- Capabilities: `can_open=true` for CLI-sourced sessions; optional `openTarget` declares the existing native mechanism's guarantee (`actions.rs`, `session_open_target`).
  - macOS Terminal/iTerm2 with a resolved tty: `terminal`, exact tty/tab selection; missing tty/tab or failed script is an error, never successful app-only fallback.
  - VS Code / Cursor / Windsurf and other IDE CLI openers (including Zed): `project`, opens/reuses the project only. Multiple terminals in the same project require manual selection; pid does not select an IDE terminal.
  - JetBrains: `project` via URL scheme, no originating-terminal selection.
  - Other application fallback / Supacode: conservative `application` guarantee; no exact native-session claim.
  - No declared target / older backend: attention selection uses the exact provider-scoped conversation.
- Health: file missing + CLI-sourced → `partial`; missing + not CLI-sourced → `unavailable`; empty entries → `partial`; otherwise the transcript file's age decides: modified within 24h → `fresh`, older → `stale` (`enrichment.rs`, `health_from_last_timestamp` in `source.rs`).

### Codex

- Surfaces: `app` (Codex Desktop / vscode), `cli` (codex-tui / cli), `exec` (codex_exec), `integration`, `unknown` (`src-tauri/src/session/codex.rs:1491-1498`, `1968-2020`).
- Lifecycle: `task_started` → Working; `task_complete` / `turn_aborted` / rollback → Idle (`codex.rs:1533-1541`); enrichment maps Working→`Working`, Idle→`WaitingForInput` (`enrichment.rs:318`).
- Declared limit: the current rollout parser exposes **no dedicated approval/question wait state**; Codex attention coverage is lifecycle-level only. No new approval inference was invented for M1.
- Identity: duplicate live rollouts for one thread merge into the newest thread state (`codex.rs:997-1019`, test `duplicate_live_rollouts_merge_into_the_newest_thread_state` at `2593`).
- Return: `can_open=false` (`codex.rs:634`); exact return is the provider-scoped conversation view (see M1-R4). No wrong-target open is offered.
- Health: `health_from_last_timestamp` against the provider window — Working → 4h, otherwise 30m; older observations are `stale` (e.g. a linked parent pinned near the 24h ceiling), empty/unparseable timestamps are `partial` (`source.rs`, `enrichment.rs`).
- Internal/guardian/review agents are hidden from the visible session set (`codex.rs:591-600`-area freshness/linked-parent logic; `isHiddenInternalSession` in `src/lib/provider.ts:71-77`).

### Pi

- Surfaces: `cli` (`src-tauri/src/session/pi.rs:260`).
- Lifecycle: pending tool call, trailing user message, or recent activity → Working, else Idle (`pi.rs:550-558`); enrichment maps Working→`Working` (empty→`Connecting`), Idle→`WaitingForInput` (`enrichment.rs:511-519`).
- Evidence: `pending_tool_name` = latest unresulted tool call (`pi.rs:538-544`, tests at `1682-1692`, `1847-1848`).
- Declared limit: a pending Pi tool call means in-flight work (Working), not a user approval wait; it is exposed as evidence, not promoted to `NeedsAttention`.
- Return: `can_open=false` (`pi.rs:268`); exact return is the provider-scoped conversation view. No wrong-target open is offered.
- Health: `health_from_last_timestamp` against the Pi window — Working → 4h, Idle → 30m (`pi.rs:32-34`); the detector additionally drops transcripts older than those windows (`pi.rs:199-233`).

## Identity / deduplication contract (M1-R3)

- Identity is provider-scoped: `provider:sessionId` (`src-tauri/src/session/source.rs:88-110`).
- Backend collapses duplicate provider-scoped identities newest-wins by parsed instant (`dedup_newest_wins` in `enrichment.rs`; RFC3339 offsets are normalized, unparseable stamps always lose, exact ties keep the first observation); same-ID Codex/Cursor/Pi sessions keep distinct keys (test `provider_scoped_identity_keeps_same_id_from_codex_and_cursor`); reconnect/reuse duplicates resolve to the newest observation regardless of arrival order (test `duplicate_observation_keeps_newest_session`).
- Frontend re-derives keys from `provider + id` and never trusts a stale serialized `sessionKey` (`src/lib/provider.ts:21-26`).
- The inbox dedups across *all* observations newest-first (malformed timestamps sort as oldest, key tiebreak), then keeps the winner only if it still needs attention — a stale attention record can never shadow a newer resolved observation (`sessions.ts`).
- Providerless conversation lookup rejects cross-provider collisions instead of guessing (`src-tauri/src/session/conversation.rs:182-195`, tests at `288-321`).
- High-confidence inbox (`src/lib/stores/sessions.ts`): `NeedsAttention` only, deduped by provider-scoped key with newest-observation-wins (store array is copied before sorting; malformed timestamps sort as oldest); each item carries provider, surface, health, observed time, reason, and `returnKind` (`native` = exact tty focus, `project` = project only, `application` = app only, `conversation` = exact provider-scoped conversation view). The monitor header badge dispatches the newest item through its `returnKind` (`jumpToAttention` in `+page.svelte`).

## Declared return guarantee (M1-R4)

- Expansion sets provider-qualified selection (`+page.svelte:380`); conversation fetch passes provider explicitly (`+page.svelte:354`, `src/lib/api.ts:28-44`).
- Stale/foreign responses are discarded when the returned `provider:sessionId` does not match the selection (`+page.svelte:360`).
- Backend conversation lookup is provider-namespaced with ambiguity rejection (`conversation.rs:196-232`).
- Native open is offered only where `canSessionAction(session,'open')` is true; exact native return additionally requires `openTarget=terminal`. Codex/Pi offer no native open control.
- Project/application attention selection also expands the exact conversation and labels the weaker native target. IDE CLI success proves only project opening, never that session A rather than session B is selected.
- Unknown/older payloads degrade to exact conversation selection. Live OS focus remains unverified; the declared mechanism is not a measured success rate.

## Source health contract (M1-R2)

- `SourceHealth`: fresh / stale / partial / unavailable / unknown (`source.rs:52-65`).
- Serialized as optional camelCase `sourceHealth`; older backends omit it (`enrichment.rs`, `src/lib/types.ts`). The CLI contract (`insert_session_contract` in `cli/mod.rs`) also carries it.
- Rendered per session card with explanatory tooltip (`SessionCard.svelte`).

## Explicitly unmeasured (M1-R5–R7)

- Live provider detection quality: **unmeasured** (no live providers run).
- OS notification presentation: **unmeasured** (synthetic/static checks only).
- End-to-end handling in the originating environment: **unproven**.
- Participant baseline / workflow-shortening: **unmeasured** (M0 waiver).
- Precision / recall / latency / four-week-use targets: **targets, not results**.
