# Claude Code hooks

Without hooks, c9watch infers **Needs Attention** from the transcript: a tool call
with no result that your `permissions.allow` rules don't cover is assumed to be
waiting on a permission prompt. That guess is wrong whenever a tool runs for a
while without prompting (a sub-agent, a long build, auto mode approving a call),
and it can't see prompts raised inside sub-agents at all.

With hooks enabled, Claude Code tells c9watch when it shows a permission prompt,
so a card shows **Needs Attention** only for a real prompt (or a question from
Claude).

Independently of hooks, Claude Code reports each session's state in
`claude agents --json`. A session reported as `waiting` (a permission prompt,
dialog or other input request) always shows **Needs Attention**. From Claude
Code 2.1.212, which reports every such prompt as `waiting`, a session reported
as `busy` or `idle` never shows **Needs Attention** from the transcript
inference or hook data alone.

## Enable

From a stable installed executable:

```sh
c9watch hooks --install
```

This backs up `settings.json` to `settings.c9watch-backup-<id>.json` and adds an
async `'<c9watch>' hooks` command hook to these events: `SessionStart`,
`SessionEnd`, `UserPromptSubmit`, `PermissionRequest`, `PermissionDenied`,
`PostToolUse`, `PostToolUseFailure`, `PostToolBatch`, `Stop`, `StopFailure`,
`SubagentStop`. Other hooks are left untouched. Running sessions pick up the
change automatically. Repeating the install is a no-op; installing from a new
executable path replaces the old entries.

```sh
c9watch hooks --uninstall
```

removes only the c9watch entries (after another backup).

Automatic installation supports macOS and Linux. Symlinked settings files are left
for manual configuration.

## How it works

The hooks are `async`, so they never delay Claude Code. They can't affect
permission decisions either. Each event updates
`~/.claude/c9watch/hooks/<session_id>.json` (or the same path under
`CLAUDE_CONFIG_DIR`), which the poller reads. Only event names, tool names and
sub-agent ids are stored; tool inputs, outputs and prompts are not.

- `PermissionRequest` records a pending prompt for the tool, tagged with the
  sub-agent id when a sub-agent raised it.
- `PostToolUse`, `PostToolUseFailure` and `PermissionDenied` clear the matching
  prompt. `PostToolBatch` and `SubagentStop` clear that agent's prompts.
  `Stop`, `StopFailure` and `UserPromptSubmit` clear the main thread's prompts.
  `SessionEnd` deletes the file.
- A pending prompt counts only while its transcript (the session's, or the
  sub-agent's under `<session>/subagents/`) still shows an unresolved call to that
  tool. Rejecting a dialog fires no hook, but it does write a tool result to the
  transcript.

A session uses hook data when the hooks are installed in the user `settings.json`
(and `disableAllHooks` isn't set there) and the session has sent at least one
event. Other sessions, such as ones started with hooks disabled, still use the
transcript inference. State files older than 7 days are pruned.
