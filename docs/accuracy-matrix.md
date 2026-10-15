# Status accuracy matrix

What the dashboard shows for each agent source in each situation. ✅ means a
test proves it today. ❌ means a known gap: the cell's test exists but is
`#[ignore]`d until the gap is fixed.

The table is generated from `MATRIX` in
`src-tauri/src/session/accuracy_matrix.rs`; the test `docs_table_matches_matrix`
fails when this page and the tests disagree. Each cell is one test named
`<source>_<scenario>` in that module (for example `claude_cli_pid_reuse`).
Run them with:

```sh
cd src-tauri
cargo test accuracy_matrix              # passing cells
cargo test accuracy_matrix -- --ignored # gap cells (expected to fail)
```

Scenarios:

- **working**: the agent is mid-turn.
- **waiting**: the agent waits on the user (a permission prompt, or `claude agents --json` reporting `waiting`).
- **idle**: the turn ended; the agent waits for the next prompt.
- **killed**: the process is gone (hard kill), its files remain.
- **pid reuse**: the process is gone and a newer process holds its pid. For Pi, a new `pi` in the same directory started after the old transcript's last write.
- **subagent**: a subagent of a working root session; the cell is the subagent's own row. A Claude Code subagent shows in its parent's subagent panel, so "hidden" is correct there.

Status names: `Working`, `NeedsAttention` (needs you), `WaitingForInput` (idle), `hidden` (no row).

Marks: ✅ proven by a test; ⚠️ proven by a test, but the detection is a heuristic with known errors; ❌ gap (ignored test); ➖ the situation does not exist for that source (no test).

Codex and Cursor killed / pid reuse: an Agent CLI session ends when no agent process holds its transcript open and it was not written in the last 10 s (`session/agent_liveness.rs`). The cells inject the open-file evidence.

<!-- accuracy-matrix:start -->
| Source | working | waiting | idle | killed | pid reuse | subagent |
|---|---|---|---|---|---|---|
| Claude Code (CLI backend) | ✅ Working | ✅ NeedsAttention | ✅ WaitingForInput | ✅ hidden | ✅ hidden | ✅ hidden |
| Claude Code (legacy backend) | ✅ Working | ✅ NeedsAttention | ✅ WaitingForInput | ✅ hidden | ✅ hidden | ✅ hidden |
| Codex | ✅ Working | ⚠️ NeedsAttention (heuristic) | ✅ WaitingForInput | ✅ hidden | ✅ hidden | ✅ Working |
| Cursor | ✅ Working | ❌ gap (want NeedsAttention) | ✅ WaitingForInput | ✅ hidden | ✅ hidden | ✅ Working |
| Pi | ✅ Working | ➖ n/a | ✅ WaitingForInput | ✅ hidden | ✅ hidden | ➖ n/a |

Gaps (each is an `#[ignore = "route1-B: …"]` test):

- Cursor × waiting: no approval signal is known in the Cursor transcript (records are role messages and `turn_ended`); not yet checked whether a pending `tool_use` is written before the prompt

Heuristic cells (the test passes; known errors):

- Codex × waiting: an escalated tool call with no later record for 35 s (`CODEX_ESCALATION_APPROVAL_DELAY_MS`) counts as waiting for approval. Known errors: (a) after the user approves, approval delay + the command's 30 s yield can pass 35 s and show NeedsAttention for a short time; (b) approvals without a marker (`apply_patch` outside the writable roots, untrusted-mode prompts) stay Working. `request_user_input` is exact. See docs/research/codex-approval-signal-2026-10.md

Not applicable (no test):

- Pi × waiting: Pi 0.87.1 has no approval step: tools run with the process's OS permissions (pi-coding-agent docs/security.md:33, docs/how-pi-works.md:49), and the transcript has only user, assistant and toolResult messages (pi.rs:8-10). Only an extension can ask the user (docs/extensions.md:7); c9watch does not model extensions
- Pi × subagent: Pi 0.87.1 has no built-in subagents (none in its docs); every Pi session is a root (pi.rs:317)
<!-- accuracy-matrix:end -->

## Sessions Claude Code hides

An app that a Claude Code process launched (with `open` or `osascript`)
inherits `CLAUDE_CODE_CHILD_SESSION=1`. Every `claude` you start in that app
treats itself as a child session: it shows "Transcript saving is off —
inherited CLAUDE_CODE_CHILD_SESSION marker", writes only
`~/.claude/sessions/<pid>.<hash>.key`, and writes no `<pid>.json` and no
transcript. `claude agents --json` does not list it.

c9watch has nothing to read for these sessions, so it cannot show them as
cards. It counts them instead: a live Claude Code process with a `.key` file,
no `.json` file, and the marker in its environment. The Monitor tab shows one
line ("N Claude Code sessions are hidden by Claude Code itself …"; click it
for each pid and folder), and `c9watch list` reports them in
`diagnostics.hiddenClaudeSessions`. The line is not shown when there are none.

Fix: quit the app and open it again from the Dock or Finder, so it does not
inherit the marker. For one shell, `env -u CLAUDE_CODE_CHILD_SESSION -u CLAUDECODE claude`
also works.

A `.key` file without a `.json` file and without the marker is not counted;
the cause is not known.
