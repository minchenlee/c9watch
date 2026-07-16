# Product TODO

## Codex native monitoring

Do not begin the implementation tasks below until the native-mechanism investigation is
complete. First determine whether Codex provides a supported interface comparable to
`claude --agents`—through the Codex CLI, App Server, hooks, notifications, or another
structured event mechanism—and whether it works for both Codex App and Codex CLI sessions.

**Decision (2026-07-13): use a hybrid architecture.** Codex has no external equivalent to
`claude --agents --json`, and a separate App Server cannot observe another process's
in-memory live state. Keep cached/incremental rollout polling as the zero-configuration
source of truth for Codex App and CLI sessions. Keep optional, user-installed Codex lifecycle
hooks as a future low-latency enhancement, reconciled with rollout metadata rather than used
as a second source of truth. Do not attach to Codex Desktop's private IPC socket.

- [x] Investigate the Codex native solution first and document whether it can replace or
  strengthen rollout-file polling for live root-session and subagent monitoring. Verdict:
  hooks strengthen polling, but no supported native mechanism replaces it for arbitrary
  existing Codex App and CLI sessions.
- [x] Revise the design with explicit `provider`, `surface`, and `agent_kind` fields.
- [x] Implement the shared cached rollout parser and App/CLI detector, if still required
  after the native-mechanism investigation.
- [x] Add visible `CLAUDE CODE` and `CODEX` badges plus a shared, persisted
  `All | Claude Code | Codex` provider filter across all tabs.
- [x] Add Codex subagent grouping, with normal subagents nested under their parent and
  internal guardian/review agents hidden by default.
- [x] Extend HISTORY and COST so their provider filters operate on real Claude Code and
  Codex data rather than acting as cosmetic filters.
- [ ] Optionally add an installable Codex notification adapter if supported lifecycle events
  become richer than turn-complete notifications; rollout polling remains the baseline.
