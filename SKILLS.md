# c9watch Skills

c9watch ships with a Claude Code skill that teaches Claude how to use the c9watch CLI to monitor sessions, search history, track costs, and coordinate with sibling agents.

## Install the skill

### Option 1: Symlink (recommended)

If you cloned the c9watch repo, symlink the skill into your Claude Code skills directory:

```bash
ln -s /path/to/c9watch/skills/c9watch-cli ~/.claude/skills/c9watch-cli
```

### Option 2: Copy

```bash
cp -r /path/to/c9watch/skills/c9watch-cli ~/.claude/skills/c9watch-cli
```

### Option 3: Download directly

```bash
mkdir -p ~/.claude/skills/c9watch-cli
curl -fsSL https://raw.githubusercontent.com/minchenlee/c9watch/main/skills/c9watch-cli/SKILL.md \
  -o ~/.claude/skills/c9watch-cli/SKILL.md
```

## What the skill enables

Once installed, Claude Code can:

- **Monitor sibling sessions** — "are any of my other sessions stuck?"
- **Search past work** — "find that conversation where I fixed the auth bug"
- **Check costs** — "how much have I spent on Claude today?"
- **View conversations** — "show me what the other session is working on"
- **Self-identify** — "what's my session ID?" (via `c9watch self`)
- **Coordinate work** — "stop that other session", "what tasks is session X working on?"

The skill triggers automatically when you ask about other Claude Code sessions, costs, or past work. You can also invoke it explicitly with `/c9watch-cli`.

## Prerequisites

The `c9watch` CLI binary must be on your `$PATH`:

```bash
curl -fsSL https://raw.githubusercontent.com/minchenlee/c9watch/main/install-cli.sh | bash
```

## Full command reference

See the skill file at [`skills/c9watch-cli/SKILL.md`](skills/c9watch-cli/SKILL.md) for the complete command reference with all flags, output fields, and usage tips.
