---
title: Compatibility — supported agents and terminals
description: Which coding agents c9watch supports in v0.10.0, what each one can do, and which terminals, IDEs and platforms work.
head:
  - tag: script
    attrs:
      type: application/ld+json
    content: '{"@context":"https://schema.org","@type":"BreadcrumbList","itemListElement":[{"@type":"ListItem","position":1,"name":"Home","item":"https://c9watch.mclee.dev"},{"@type":"ListItem","position":2,"name":"Compatibility","item":"https://c9watch.mclee.dev/compatibility/"}]}'
---

:::note[TL;DR]
c9watch v0.10.0 monitors **Claude Code**, **Codex** (app and CLI), **Cursor Agent** (monitor-only) and **Pi** (read-only). **OpenCode** is a preview that needs a manual server connection.
:::

*Applies to v0.10.0. Last checked against the release notes on the release date.*

## Agents

| Agent | Status | What you get |
|---|---|---|
| Claude Code | Supported | Live status, needs-attention alerts, history, search, cost, memory files, subagents, JSON CLI. An optional hook bridge reports permission prompts directly. |
| Codex (app and CLI) | Supported | Live status with a `CODEX` badge, subagents grouped under the parent, history, search, cost estimates. Shows working and idle only. There is no approval or question state. |
| Cursor Agent | Supported (monitor-only) | Root sessions and subagents in Monitor, History, search and the provider filter. c9watch does not approve prompts or jump to a Cursor session. |
| Pi | Supported (read-only) | Monitor, History, conversation, search, Cost and CLI. You cannot open, stop or rename a Pi session from c9watch. |
| OpenCode | Preview | Connect to one OpenCode server that you start with `opencode --port 4096`, in Settings → Integration. c9watch does not discover servers. See [the preview guide](https://github.com/minchenlee/c9watch/blob/main/docs/opencode-preview.md). |

If your agent is not on this list, c9watch does not see it.

## Platforms

- Desktop app: macOS (Apple silicon and Intel).
- CLI: macOS and Linux (x86_64).
- Windows: no release.

## Terminals and IDEs

c9watch finds sessions by scanning running processes. You do not need a plugin. See [Features](/features/) for the terminals and IDEs it can focus.

## Known limits

- Codex: no approval or question state.
- Pi: a session started in one project and resumed into another can show as ended until that Pi exits. An idle Pi in another folder can keep a killed card alive.
- Subscription usage meters cover Claude Code, Codex and Cursor. Claude Code needs an optional status-line bridge.
- The Claude Code hook bridge (`c9watch hooks --install`) is opt-in and not available on Windows. See [the hooks guide](https://github.com/minchenlee/c9watch/blob/main/docs/claude-hooks.md).
