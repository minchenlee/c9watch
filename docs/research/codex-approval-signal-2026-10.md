# Codex approval signal and Codex/Cursor process evidence (2026-10-14)

Ticket: c9-route1-accuracy (route 1B). Codex version: `codex-cli 0.162.1`
(source: `openai/codex` tag `rust-v0.162.1`, commit `7a078f6`). Cursor Agent CLI
`2026.10.01-e373342`.

## Conclusion

1. Codex writes **no** rollout record for an approval request. The events
   `ExecApprovalRequest`, `ApplyPatchApprovalRequest`, `RequestPermissions`,
   `RequestUserInput` and `ElicitationRequest` are in the "transient,
   non-durable" list of the rollout policy.
2. Codex writes the **tool call** that causes the request, before the tool
   runs. While the user decides, that call is the last record in the rollout
   (only a `token_usage_record` follows it). Its output is written only after
   the decision.
3. Usable signal, with limits:
   - A `request_user_input` or `request_permissions` call with no later record
     means "waiting for the user" by definition. Reliable at once.
   - An `exec_command` call with `sandbox_permissions: "require_escalated"`
     and no later record is ambiguous for a short time. It is either waiting
     for approval, or approved and running. After approval the output must
     arrive within the yield clamp (`MAX_YIELD_TIME_MS = 30 s`). So a call
     that has no later record after 35 s is waiting for approval.
   - Not covered: approval for an `apply_patch` outside the writable roots,
     and approvals in `untrusted` mode for commands without
     `require_escalated`. The call carries no marker. These stay "Working"
     (current behavior).
4. Codex 0.162 runs a plain `codex` TUI **inside the shared managed
   `codex app-server` daemon**. The TUI process is only a client. If the TUI
   exits or is killed, the thread stays loaded and a running turn continues.
   With any `-c` override, `--profile`, `--oss` or `--no-daemon`, the TUI runs
   the thread in its own process (it holds the rollout file open).
5. Cursor Agent CLI is a `node` process with `argv[0]` =
   `…/.local/bin/cursor-agent` (or `agent`). Each CLI chat has
   `~/.cursor/chats/<md5(realpath cwd)>/<session-id>/store.db`. IDE chats do
   not. This gives an exact CLI test and an exact cwd key.

## Source evidence (rust-v0.162.1, paths under `codex-rs/`)

| Fact | Location |
|---|---|
| Approval / user-input events are not persisted | `rollout/src/policy.rs:160-223` (`ExecApprovalRequest` :195, `RequestPermissions` :196, `RequestUserInput` :197, `ElicitationRequest` :198, `ApplyPatchApprovalRequest` :199, `ExecCommandBegin` :192, `ItemStarted` :211) |
| Persisted event_msg set | `rollout/src/policy.rs:131-158` (`TurnStarted`, `TurnComplete`, `TurnAborted`, `ThreadRolledBack`, `TokenCount`, `ItemCompleted`, …) |
| Tool-call response items are persisted | `rollout/src/policy.rs:80-100` (`FunctionCall`, `CustomToolCall`, `LocalShellCall`, outputs) |
| Tool call is recorded before the tool runs | `core/src/stream_events_utils.rs:324-354` ("persist the item immediately, and queue the tool execution") |
| Yield clamp after the process starts | `core/src/unified_exec/mod.rs:73-77` (`MIN_YIELD_TIME_MS = 250`, `MAX_YIELD_TIME_MS = 30_000`), `:218-224` |
| `write_stdin` polls may wait up to the background timeout | `core/src/unified_exec/mod.rs:78` (`DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS = 300_000`), `core/src/unified_exec/process_manager.rs:1013-1021` |
| Tool names | `core/src/tools/handlers/request_user_input_spec.rs:9`, `core/src/tools/handlers/request_permissions.rs:32` |
| TUI uses the managed daemon unless excluded | `tui/src/startup_orchestration.rs:189-196, 509-568`; exclusions `tui/src/daemon_startup.rs:65-100` |
| "Disconnected from this task. Any running work continues." | `tui/src/app/exit_summary.rs:95-97` |
| `PermissionRequest` hook exists | `hooks/src/lib.rs:25`, `protocol/src/protocol.rs:1595-1597` |
| Default `approvals_reviewer` is `user`; `guardian_subagent` is an alias of `auto_review` | `protocol/src/config_types.rs:183-190` |

## Live test (2026-10-14, herdr pane, cwd = scratchpad `codex-probe`)

Run A: `codex -a on-request -s read-only` with the owner's config
(`approvals_reviewer = "guardian_subagent"`). Prompt: `touch probe.txt`.
Rollout `rollout-2026-10-14T21-58-29-01a13ab5-199f-….jsonl`:

```
1  13:58:40.363Z session_meta  originator=codex-tui source=vscode   <- daemon-run TUI thread
11 13:58:50.636Z response_item custom_tool_call exec {cmd:"touch probe.txt"}             (sandbox denied)
18 13:58:58.761Z response_item custom_tool_call exec {…sandbox_permissions:"require_escalated"…}
19 13:58:58.816Z token_usage_record
   -- nothing for 9.4 s: the guardian sub-agent reviews in its own rollout
      (…01a13ab5-1d8e-….jsonl, source {"subagent":{"other":"guardian"}}) --
20 13:59:08.198Z event_msg item_completed CommandExecution
21 13:59:08.244Z response_item custom_tool_call_output
```

Run B: same flags plus `-c approvals_reviewer=user` (human approval).
Rollout `rollout-2026-10-14T22-00-22-01a13ab6-d1d4-….jsonl`:

```
1  14:00:31.036Z session_meta  originator=codex-tui source=cli      <- in-process TUI (has -c)
11 14:00:37.675Z response_item custom_tool_call call_fd58… exec {cmd:"touch probe2.txt",sandbox_permissions:"require_escalated",…,"yield_time_ms":10000}
12 14:00:37.701Z token_usage_record
   -- approval dialog open on screen; file mtime stayed 22:00:37 (local) at 22:00:46 --
13 14:01:01.516Z event_msg item_completed CommandExecution   (approved at ~14:01:01)
14 14:01:01.540Z response_item custom_tool_call_output call_fd58… "Script completed\nWall time 23.7 seconds"
```

The 10 s script yield did not fire during the 23.7 s approval wait.

Run C (same session): escalated `sleep 45 && touch probe3.txt`, approved
within 1 s:

```
30 14:01:17.029Z custom_tool_call call_930f… {…require_escalated…,"yield_time_ms":30000}
32 14:01:48.082Z custom_tool_call_output call_930f… "Script running with cell ID 2\nWall time 31.0 seconds"
```

After approval, output arrived after the 30 s yield, as the clamp predicts.

Process table during the test:

```
56798  codex app-server --listen unix:// --analytics-default-enabled --managed-daemon   (since Oct 10; parent of run A's MCP child)
56197  codex -a on-request -s read-only -c approvals_reviewer=user   cwd=<codex-probe>  (run B; holds the rollout fd 48u)
```

## Rule chosen for c9watch

- `NeedsAttention` when the turn is Working and the last tool call has no later
  record (other than `token_usage_record`) and either:
  - it calls `request_user_input` / `request_permissions` (at once), or
  - it carries `require_escalated` and is older than 35 s.
- This cell is a **heuristic**. Constant:
  `CODEX_ESCALATION_APPROVAL_DELAY_MS` in `src-tauri/src/session/codex.rs`.
- Known false positive 1: the 35 s clock starts when the call is written, not
  when the user approves. If approval delay + the command's yield (up to
  30 s) is over 35 s, the card shows NeedsAttention for a short time after the
  user already approved. It returns to Working at the next record.
- Known false positive 2: a code-mode `exec` script that runs an escalated
  command and then keeps polling it inside the same script can stay silent
  longer than 35 s after approval. Same recovery.
- Known false negative: approvals without a marker (`apply_patch` outside the
  writable roots, `untrusted`-style prompts for commands without
  `require_escalated`), and the first 35 s of an escalated approval. These
  stay Working.

## Alternatives (not implemented)

| Option | What it gives | Cost |
|---|---|---|
| Codex `PermissionRequest` hook (`[hooks]` in `~/.codex/config.toml`) | Exact, immediate signal for every approval kind | c9watch must install and own a hook in the user's Codex config, and keep it working across Codex updates. Sync hooks also block the approval path. |
| `notify` config | Only `agent-turn-complete` | Does not cover approvals. |
| Connect to the app-server daemon socket as a client | Live `ExecApprovalRequest` events | Undocumented internal protocol; only covers daemon-run threads. |

## Process evidence (step 3)

- Codex: a live `codex` process whose `argv[1]` is not `app-server` (and not
  another non-session subcommand) attaches to sessions in its cwd, like
  `pi_liveness.rs`. Applies only to root rollouts with
  `originator` = `codex-tui` or `codex_exec`. Desktop (`Codex Desktop`) and
  IDE threads keep the mtime rule. Note: a daemon-run TUI thread records
  `source: "vscode"`, so the originator, not the source, decides.
- Because a daemon-run turn continues after its TUI exits, a Working
  rollout keeps a longer grace than an Idle one.
- Cursor: a `node` process whose argv[0] basename is `cursor-agent` or
  `agent` attaches to CLI transcripts under `chats/<md5(cwd)>/`. IDE
  transcripts keep the mtime rule.
