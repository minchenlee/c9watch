# Jump matrix: manual check

These steps open and focus real windows, so an agent must not run them.
Run them yourself when you have a few minutes. Each step has one action and
one expected result. Write **pass** or **fail** in the last column.

Use a c9watch build from branch `feature/jump-matrix`. Before each section,
click another app (for example Finder) so that the target is not already in
front.

Count the result at the end: passed steps ÷ all steps. The target is 90 % or
more. Copy the number into [jump-matrix.md](jump-matrix.md).

## §1 Terminal.app

| # | Action | Expected result | Result |
|---|---|---|---|
| 1.1 | Open two Terminal.app windows. In the second window, open a second tab. | Three tabs in total. | |
| 1.2 | In the second tab of the second window, `cd` to a test project and run `claude`. | c9watch shows a new Claude Code card. | |
| 1.3 | Click the first tab of the first window. | The first window is in front. | |
| 1.4 | Minimize the second window. | The second window is in the Dock. | |
| 1.5 | Click Finder. | Finder is in front. | |
| 1.6 | In c9watch, click **Open** on the card. | The second window comes back from the Dock, with the `claude` tab selected. | |

## §2 iTerm2

| # | Action | Expected result | Result |
|---|---|---|---|
| 2.1 | In iTerm2, open a window with two tabs. Split the second tab vertically. | Two tabs; the second has two panes. | |
| 2.2 | In the right pane of the second tab, run `claude`. | c9watch shows a new card. | |
| 2.3 | Select the first tab, then click Finder. | Finder is in front. | |
| 2.4 | In c9watch, click **Open** on the card. | iTerm2 comes to the front with the second tab and the right pane active. | |

## §3 Ghostty

| # | Action | Expected result | Result |
|---|---|---|---|
| 3.1 | In Ghostty, open two tabs. In tab 2, `cd ~/tmp/jump-a` and run `claude`. | c9watch shows a new card. | |
| 3.2 | Select tab 1 (any other directory), then click Finder. | Finder is in front. | |
| 3.3 | In c9watch, click **Open** on the card. | Ghostty comes to the front with tab 2 selected. | |
| 3.4 | In tab 1, `cd ~/tmp/jump-a` too. Select tab 1, then click Finder. | Two Ghostty tabs are in the same directory. | |
| 3.5 | In c9watch, click **Open** on the card. | Ghostty comes to the front. The tab does not change (two tabs match, so c9watch does not guess). | |
| 3.6 | If macOS asks to let c9watch control Ghostty, note it here. | One prompt the first time only, or none. | |

## §4 tmux

| # | Action | Expected result | Result |
|---|---|---|---|
| 4.1 | In iTerm2 or Terminal.app, run `tmux new -s jump-a`. Create a second window (`Ctrl-b c`) and run `claude` there. | c9watch shows a new card. | |
| 4.2 | Switch to tmux window 0 (`Ctrl-b 0`). | Window 0 is visible. | |
| 4.3 | Run `tmux new -d -s jump-b`, then `tmux switch-client -t jump-b`. | The client shows session `jump-b`. | |
| 4.4 | Click Finder. | Finder is in front. | |
| 4.5 | In c9watch, click **Open** on the card. | The terminal tab with the tmux client comes to the front, it shows session `jump-a`, window 1, and the `claude` pane. | |
| 4.6 | Detach (`Ctrl-b d`), then click **Open** again. | No window comes up, and c9watch shows no crash. (A detached session has no client to show.) | |

## §5 Herdr

Use a separate test session so that your normal Herdr layout does not move.

| # | Action | Expected result | Result |
|---|---|---|---|
| 5.1 | In Ghostty, run `herdr --session jump-test`. | A Herdr client starts. | |
| 5.2 | Create a second tab in Herdr and run `claude` in it. | c9watch shows a new card. | |
| 5.3 | Select the first Herdr tab, then click Finder. | Finder is in front. | |
| 5.4 | In c9watch, click **Open** on the card. | Ghostty comes to the front, and Herdr shows the tab and pane with `claude`. | |
| 5.5 | Quit the client (keep the server), then click **Open** again. | No window comes up. In a new `herdr --session jump-test` client, the `claude` pane is focused. | |

## §6 Warp, kitty, WezTerm (app only)

| # | Action | Expected result | Result |
|---|---|---|---|
| 6.1 | In Warp, open two tabs and run `claude` in the second tab. | c9watch shows a new card. | |
| 6.2 | Select the first tab, then click Finder. | Finder is in front. | |
| 6.3 | In c9watch, click **Open** on the card. | Warp comes to the front. The tab does not change (expected: app level only). | |

## §7 VS Code, Cursor, Zed

| # | Action | Expected result | Result |
|---|---|---|---|
| 7.1 | Open two VS Code windows with different projects. In project B's integrated terminal, run `claude`. | c9watch shows a new card. | |
| 7.2 | Click the project A window, then click Finder. | Finder is in front. | |
| 7.3 | In c9watch, click **Open** on the card. | The project B window comes to the front. The terminal panel is not selected (expected). | |
| 7.4 | Repeat 7.1–7.3 in Cursor. | Same result in Cursor. | |
| 7.5 | Repeat 7.1–7.3 in Zed. | Same result in Zed. | |

## §8 JetBrains

| # | Action | Expected result | Result |
|---|---|---|---|
| 8.1 | Open two projects in one JetBrains IDE. In project B's terminal, run `claude`. | c9watch shows a new card. | |
| 8.2 | Click the project A window, then click Finder. | Finder is in front. | |
| 8.3 | In c9watch, click **Open** on the card. | The project B window comes to the front. | |

## Result

| Passed | Total | Rate | Date | c9watch commit |
|---|---|---|---|---|
| | | | | |
