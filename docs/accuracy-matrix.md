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

<!-- accuracy-matrix:start -->
| Source | working | waiting | idle | killed | pid reuse | subagent |
|---|---|---|---|---|---|---|
| Claude Code (CLI backend) | ✅ Working | ✅ NeedsAttention | ✅ WaitingForInput | ✅ hidden | ✅ hidden | ✅ hidden |
| Claude Code (legacy backend) | ✅ Working | ✅ NeedsAttention | ✅ WaitingForInput | ✅ hidden | ✅ hidden | ✅ hidden |
| Codex | ✅ Working | ❌ gap (want NeedsAttention) | ✅ WaitingForInput | ❌ gap (want hidden) | ❌ gap (want hidden) | ✅ Working |
| Cursor | ✅ Working | ❌ gap (want NeedsAttention) | ✅ WaitingForInput | ❌ gap (want hidden) | ❌ gap (want hidden) | ✅ Working |
| Pi | ✅ Working | ❌ gap (want NeedsAttention) | ✅ WaitingForInput | ✅ hidden | ✅ hidden | ❌ gap (want Working) |

Gaps (each is an `#[ignore = "route1-B: …"]` test):

- Codex × waiting: Codex approval requests are not detected; needs the rollout signal and a fixture
- Codex × killed: no process evidence for Codex; a killed rollout stays Working until the 4h freshness window ends
- Codex × pid reuse: no process evidence for Codex; a killed rollout stays Working until the 4h freshness window ends
- Cursor × waiting: Cursor approval prompts are not detected; needs the transcript signal and a fixture
- Cursor × killed: no process evidence for Cursor; a killed transcript stays Working until the freshness window ends
- Cursor × pid reuse: no process evidence for Cursor; a killed transcript stays Working until the freshness window ends
- Pi × waiting: no waiting-on-user signal is known for Pi; confirm whether Pi has one, then add a fixture
- Pi × subagent: Pi subagent sessions are not modelled; confirm the transcript layout, then add a fixture
<!-- accuracy-matrix:end -->
