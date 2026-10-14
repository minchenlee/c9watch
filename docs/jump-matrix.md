# Jump matrix

What happens when you click **Open** on a session card: which terminal, tab,
or pane c9watch brings to the front.

Other session monitors also jump back to the session. This page only states
what c9watch does today, where it stops, and how each cell was checked.

Code: the decision is the pure planner in
[`src-tauri/src/jump.rs`](../src-tauri/src/jump.rs) (`plan_jump`). The
runtime in [`src-tauri/src/actions.rs`](../src-tauri/src/actions.rs)
(`resolve_location`, `run_jump_plan`) collects process facts and runs the
planned steps.

## Levels

| Level | Meaning |
|---|---|
| **Tab/pane** | The exact tab, split, or multiplexer pane that runs the session. |
| **Window** | The project window of the IDE. The IDE's integrated terminal panel is not selected. |
| **App** | The app comes to the front. You pick the tab yourself. |
| **No** | The Open button is not shown. c9watch has no process for the session. |

## Matrix

Rows are where the session runs. Columns are the agent that runs it.

| Where the session runs | Claude Code | Codex CLI | Codex Desktop | Cursor Agent | Pi |
|---|---|---|---|---|---|
| Terminal.app | Tab/pane ¹ | No ⁶ | No ⁶ | No ⁶ | No ⁶ |
| iTerm2 | Tab/pane ¹ | No ⁶ | No ⁶ | No ⁶ | No ⁶ |
| Ghostty (1.3+) | Tab/pane if unique, else App ² | No ⁶ | No ⁶ | No ⁶ | No ⁶ |
| Warp | App ³ | No ⁶ | No ⁶ | No ⁶ | No ⁶ |
| kitty, WezTerm, Alacritty, Hyper | App | No ⁶ | No ⁶ | No ⁶ | No ⁶ |
| tmux (in any terminal) | Tab/pane ⁴ | No ⁶ | No ⁶ | No ⁶ | No ⁶ |
| Herdr | Tab/pane ⁵ | No ⁶ | No ⁶ | No ⁶ | No ⁶ |
| VS Code | Window ⁷ | No ⁶ | No ⁶ | No ⁶ | No ⁶ |
| Cursor | Window ⁷ | No ⁶ | No ⁶ | No ⁶ | No ⁶ |
| Zed | Window ⁷ | No ⁶ | No ⁶ | No ⁶ | No ⁶ |
| JetBrains IDEs | Window ⁸ | No ⁶ | No ⁶ | No ⁶ | No ⁶ |

Success rate: **not measured yet.** The target is 90 % or more of the
Claude Code cells. Run [jump-matrix-manual-check.md](jump-matrix-manual-check.md)
and record the result there.

## Notes and evidence

1. **Terminal.app, iTerm2.** AppleScript matches the session's tty to a tab
   (iTerm2: a session) and selects it.
   Tests: `jump::tests::terminal_and_iterm_with_tty_select_the_exact_tab`,
   `terminal_without_tty_only_activates_the_app`.
   Manual: checklist §1, §2.
2. **Ghostty.** Ghostty 1.3 has an AppleScript dictionary (`terminal`,
   `focus`, `working directory`), checked in
   `/Applications/Ghostty.app/Contents/Resources/Ghostty.sdef` (1.3.1). A
   Ghostty terminal does not expose its tty or pid, so c9watch matches the
   session's project directory. It focuses the terminal only when exactly one
   Ghostty terminal has that directory. With two or more, or none, it only
   brings Ghostty to the front. It also needs `macos-applescript = true`
   (the default).
   Tests: `ghostty_matches_a_unique_working_directory`,
   `ghostty_as_outer_terminal_only_activates`. The script compiles with
   `osacompile`.
   Manual: checklist §3.
3. **Warp.** Warp has no public interface that focuses an existing session
   by process or session identity. Its URI scheme and launch configurations
   only open new windows or tabs. c9watch does not use UI automation or
   keystrokes. Source: the 2026-09-22 investigation for issue #56.
   Test: `warp_and_unknown_terminals_only_activate`.
4. **tmux.** c9watch reads `TMUX` and `TMUX_PANE` from the session process,
   checks that the tmux server is an ancestor of the session, and runs
   `tmux -S <socket> select-window -t <pane>` and `select-pane -t <pane>`.
   When the most recent client shows another tmux session, it also runs
   `switch-client -c <client tty> -t <pane>`. Then it focuses the terminal
   that hosts the client, with the rules of that terminal's row (for
   example, the exact iTerm2 tab of the client tty). Detached tmux sessions
   have no client to show: the pane is selected, but no window comes up.
   Tests: `tmux_selects_window_and_pane_then_focuses_the_client_terminal`,
   `tmux_switches_a_client_that_shows_another_session`,
   `choose_tmux_client_prefers_one_already_on_the_session`,
   `innermost_multiplexer_follows_the_process_tree`, `tmux_env_parsing`.
   The tmux commands were checked on a private server (`tmux -L`).
   Manual: checklist §4.
5. **Herdr.** c9watch reads `HERDR_PANE_ID` and `HERDR_SOCKET_PATH` from the
   session process and runs `herdr agent focus <pane>` on that socket
   (checked with `herdr agent focus --help`, herdr 0.9.1). Then it focuses
   the terminal that runs a `herdr` client for the same Herdr session. A
   non-terminal Herdr client (for example a desktop app) is not found: the
   pane is focused inside Herdr, but no window comes up. The pane id comes
   from the pane's environment; the session id join key
   (`agent_session.value`) is not used yet.
   Tests: `herdr_focuses_the_agent_pane_on_its_socket_then_the_outer_terminal`,
   `nested_tmux_inside_herdr_selects_both_panes`, `herdr_clients_skip_servers_and_other_sessions`,
   `herdr_session_name_from_socket`.
   Manual: checklist §5.
6. **Other agents.** Codex, Codex Desktop, Cursor Agent, and Pi sessions
   are read from files. c9watch has no process id for them (`pid: 0`), so
   the Open button is hidden. Work on Codex CLI process evidence is in
   progress on another branch; this column changes only after that lands.
7. **VS Code, Cursor, Zed.** c9watch runs the editor CLI with the project
   path (`code -r -g <path>`; Zed: `cli <path>`), which raises the window
   that has the project. We found no way to select a specific integrated
   terminal tab from outside the editor without an extension.
   Test: `vscode_family_reuses_the_project_window`.
8. **JetBrains IDEs.** c9watch opens `<scheme>://open?file=<project root>`,
   which raises the project window. If that fails, it brings the app to the
   front. Test: `jetbrains_opens_the_project_url_and_falls_back_to_the_app`.

Nested multiplexers (tmux inside a Herdr pane, or Herdr inside tmux) select
the pane at each level, up to three levels.
