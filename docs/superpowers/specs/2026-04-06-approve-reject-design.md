# Approve/Reject from Dashboard — Design Spec

> Phase 1: tmux + iTerm2 support
> Scope: Dashboard GUI only (no CLI commands)

## Overview

Add the ability to approve or reject tool-use permission prompts directly from the c9watch dashboard, without switching to the terminal. This works by detecting which terminal hosts the Claude Code session and injecting the appropriate keystroke (Enter for approve, Escape for reject).

Phase 1 supports **tmux** and **iTerm2** — the two most common environments for multi-session Claude Code users. Both support background keystroke injection (no focus steal required).

## Architecture: Detect-then-Route

When the user clicks Approve or Reject:

1. `find_parent_app(pid)` — detect terminal type (existing function)
2. `get_session_tty(pid)` — get TTY (existing function)
3. Route to adapter:
   - **All terminals:** Check tmux first via `check_tmux_pane(tty)` — a session in any terminal might also be inside tmux
   - **tmux pane found:** `send_keystroke_tmux(pane_id, keystroke)`
   - **No tmux + iTerm2 detected:** `send_keystroke_iterm2(tty, keystroke)`
   - **No tmux + unsupported terminal:** Return error naming the detected terminal

tmux is checked first for all terminals because:
- It's the most reliable method (direct pane targeting, no focus needed)
- A session inside iTerm2 might also be inside tmux
- The check is cheap (~5ms for `tmux list-panes`)

## Backend: New Tauri Commands

**File: `src-tauri/src/lib.rs`**

```rust
#[tauri::command]
async fn approve_session(pid: u32) -> Result<(), String>

#[tauri::command]
async fn reject_session(pid: u32) -> Result<(), String>
```

Both delegate to a shared function in `actions.rs`.

## Backend: Keystroke Injection Adapters

**File: `src-tauri/src/actions.rs`**

### Core routing function

```
send_keystroke(pid: u32, keystroke: Keystroke) -> Result<(), String>
  where Keystroke = Approve | Reject

  1. find_parent_app(pid) → terminal type
  2. get_session_tty(pid) → tty string
  3. check_tmux_pane(tty) → Option<pane_id>
  4. If tmux pane found → send_keystroke_tmux(pane_id, keystroke)
  5. Else if iTerm2 → send_keystroke_iterm2(tty, keystroke)
  6. Else → Err("Approve/reject not yet supported for {terminal}. Supported: tmux, iTerm2")
```

### tmux adapter

```
check_tmux_pane(tty: &str) -> Option<String>
  - Run: tmux list-panes -a -F "#{pane_tty}\t#{pane_id}"
  - Parse output lines, match TTY → return pane_id
  - If tmux not running or TTY not found → return None

send_keystroke_tmux(pane_id: &str, keystroke: Keystroke) -> Result<(), String>
  - Approve: Command::new("tmux").args(["send-keys", "-t", pane_id, "Enter"])
  - Reject:  Command::new("tmux").args(["send-keys", "-t", pane_id, "Escape"])
```

### iTerm2 adapter

```
send_keystroke_iterm2(tty: &str, keystroke: Keystroke) -> Result<(), String>
```

Approve AppleScript (reuses existing TTY matching pattern from `open_session`):
```applescript
tell application "iTerm2"
  repeat with w in windows
    repeat with t in tabs of w
      repeat with s in sessions of t
        if tty of s ends with "{tty}" then
          tell s to write text ""
        end if
      end repeat
    end repeat
  end repeat
end tell
```

Reject AppleScript — same structure but:
```applescript
tell s to write text (ASCII character 27)
```

No `activate` call — works entirely in the background. No focus steal.

## Frontend: UI Changes

### SessionCard.svelte

When `status === NeedsAttention`:

**Approve/Reject buttons:**
- Add Approve (checkmark icon, 14x14 SVG) and Reject (X icon, 14x14 SVG) next to existing STOP button
- Approve calls `approveSession(pid)` via `api.ts`
- Reject calls `rejectSession(pid)` via `api.ts`

**Tool input summary** next to status label:
- Current: `"Approval Required"`
- New: `"Approval Required — Bash: git add ."` (truncated to ~60 chars)
- Extraction logic from `pendingToolInput`:
  - Bash → `command` field
  - Write/Edit → `file_path` field
  - MCP tools → tool name
  - Other → tool name only

**Button states:**
- Default: visible, normal styling
- After click: disabled/spinner for 1-2s until next poll detects status change
- On error (unsupported terminal): toast notification explaining which terminal was detected

### ExpandedCardOverlay.svelte

- Add Approve/Reject buttons in the header, next to existing Stop/Open buttons
- Same behavior as SessionCard buttons

### Tray Popover (+page.svelte in routes/popover/)

- Add small Approve/Reject buttons for NeedsAttention sessions
- This is the most common glance-and-act surface

### api.ts

```typescript
export async function approveSession(pid: number): Promise<void> {
  return invoke('approve_session', { pid });
}

export async function rejectSession(pid: number): Promise<void> {
  return invoke('reject_session', { pid });
}
```

## WebSocket / Mobile Client

**File: `src-tauri/src/web_server.rs`**

Add two new request-response message types (same pattern as existing `stopSession`):

```
Client → Server: { type: "approveSession", pid: 1234, token: "..." }
Client → Server: { type: "rejectSession", pid: 1234, token: "..." }
Server → Client: { type: "actionResult", success: true }
                  or { type: "actionResult", success: false, error: "..." }
```

Mobile/web client can also approve/reject via the same auth token mechanism.

## Error Handling

| Scenario | Behavior |
|---|---|
| tmux pane found | Send keystroke, return success |
| iTerm2 session found by TTY | Send keystroke, return success |
| TTY detection fails | Error: "Could not detect terminal TTY for this session" |
| Unsupported terminal | Error: "Approve/reject not yet supported for {terminal}. Supported: tmux, iTerm2" |
| tmux not running + non-iTerm2 | Same unsupported error |
| Session no longer NeedsAttention (race) | Harmless — Enter on idle prompt is no-op, Escape on idle is no-op |
| PID no longer exists | Error: "Session process not found" |

No destructive edge cases. The worst case is a wasted keystroke.

## Files to Modify

| File | Changes |
|---|---|
| `src-tauri/src/actions.rs` | Add `send_keystroke()`, `check_tmux_pane()`, `send_keystroke_tmux()`, `send_keystroke_iterm2()` |
| `src-tauri/src/lib.rs` | Add `approve_session`, `reject_session` commands + register |
| `src/lib/api.ts` | Add `approveSession()`, `rejectSession()` wrappers |
| `src/lib/components/SessionCard.svelte` | Add Approve/Reject buttons, tool input summary |
| `src/lib/components/ExpandedCardOverlay.svelte` | Add Approve/Reject in header |
| `src-tauri/src/web_server.rs` | Add WebSocket handlers for approve/reject |
| `src/routes/popover/+page.svelte` | Add Approve/Reject buttons for NeedsAttention sessions |

No new files — all changes extend existing modules.

## Out of Scope (Phase 1)

- CLI commands (`c9watch approve/reject`)
- VS Code / Cursor / Windsurf extension
- kitty, WezTerm, Terminal.app, Ghostty, Warp, Alacritty support
- Hook-based event capture / tool call observability
- Subagent hierarchy tracking

## Future Phases

- **Phase 2:** VS Code companion extension (PID matching via Extension API + `terminal.sendText()`)
- **Phase 3:** kitty (Unix socket IPC), WezTerm (CLI), Terminal.app (AppleScript + focus)
