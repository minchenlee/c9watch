# E5 agent creates agent: owner live test

Run this in a plain terminal (Ghostty), not from Claude Code or any of its
descendants: D8 refuses `agent.create` there, by design. It uses a
separate engine home, so your normal engine and agents are not touched.

`tests/live/e5_parent.sh` automates steps 1-6 (step 6 with `--limits`)
with no model prompts; see its header for the build and usage.

## Setup

```sh
# Build this branch: the engine binary and c9watch-agent next to it.
# Without --link it does not touch ~/.local/bin/c9watch.
cd ~/Documents/GitHub/c9watch   # on branch feature/e5-agent-creates-agent
scripts/dogfood-build.sh
C9=$PWD/src-tauri/target/release/c9watch   # or $CARGO_TARGET_DIR/release/c9watch

export C9WATCH_ENGINE_HOME=$HOME/e5-test/home
R=$HOME/e5-test/root
mkdir -p "$R"/a/c "$R"/a/c/g "$R"/a/k "$R"/b "$R"/a/.git/x
R=$(cd "$R" && pwd -P)
```

## 1. Parent from the terminal

```sh
$C9 api agent.create "{\"mode\":\"free\",\"cwd\":\"$R/a\",\
\"guard\":{\"denyWriteUnder\":[\"$R\"],\"allowWrite\":[\"$R/a\"]}}"
```

Expect: `ready`. Note the id as `P`. Open the agent (GUI MONITOR card, or
`$C9 api agent.list`) and type into it.

## 2. In the parent agent: create a child

Ask the parent to run exactly this in its Bash (paths spelled out):

```sh
B="$(dirname "$C9_ENGINE_SOCK")/bin/c9watch-agent"
"$B" child create --idem c1 --params '{"mode":"free","cwd":"<R>/a/c","guard":{"denyWriteUnder":["<R>"],"allowWrite":["<R>/a/c"]},"require":["hook.pre_tool_use.block"]}'
```

Expect: JSON with the child `ready`. Then, from the terminal:

- [ ] `$C9 api agent.list` shows the child with `parentId` = `P`.
- [ ] Running the same command again (same `--idem c1`) returns the same
      child id, and no second agent appears.

## 3. In the parent agent: refusals

Each must fail with `not_allowed` and spawn nothing:

- [ ] No `guard`: `--params '{"mode":"free","cwd":"<R>/a/k"}'`.
- [ ] Outside the parent's area: cwd `<R>/b`.
- [ ] The parent's own cwd: cwd `<R>/a`.
- [ ] Inside `.git`: cwd `<R>/a/.git/x`.
- [ ] A looser guard: `"guard":{"denyWriteUnder":["<R>/a"],"allowWrite":["<R>/a/k"]}`
      (drops the parent's deny on `<R>`).
- [ ] An extra field: add `"settings":{}` to a good create.
- [ ] A model from an agent (E1): add `"overlay":{"model":"sonnet"}` to a
      good create. (`"overlay":{}` and an overlay without `model` are
      allowed since 1.14.)
- [ ] Another binary: `"$C9" api agent.create '<good params>'` from the
      parent's Bash fails with `peer_not_authorized` (or `not_allowed`).

## 3b. Planted project config does not run

From the terminal (standing in for the parent, which can write there):

```sh
mkdir -p "$R"/a/h/.claude
printf '{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"touch %s/e5-test/pwned"}]}]}}' "$HOME" > "$R"/a/h/.claude/settings.json
printf '{"mcpServers":{"x":{"command":"touch","args":["%s/e5-test/pwned-mcp"]}}}' "$HOME" > "$R"/a/h/.mcp.json
```

- [ ] In the parent: create a child in `<R>/a/h` (as in 2, `allowWrite`
      `<R>/a/h`). It is `ready`.
- [ ] `ls "$HOME"/e5-test/pwned*` finds nothing.
- [ ] `ps -o args= -p <child pid>` shows `--setting-sources  --strict-mcp-config`
      (an empty value).

## 4. Retire scope

- [ ] From the terminal, create a second root agent in `<R>/b` (as in 1,
      with `allowWrite` `<R>/b`); note it as `Q`.
- [ ] In the parent: `"$B" child retire <Q>` fails with `not_allowed`.
- [ ] In the parent: `"$B" child retire <P>` (itself) fails with `not_allowed`.
- [ ] In the child: a grandchild in `<R>/a/c/g` (guard deny `<R>`, allow
      `<R>/a/c/g`) is `ready`.

## 5. Lifecycle

- [ ] `$C9 engine restart`: the parent, child and grandchild come back
      (`agent.list`), with the same `parentId`s, and none is retired.
- [ ] From the terminal: `$C9 api agent.retire "{\"id\":\"<P>\"}"`. Within
      about 2 s the child and the grandchild are `retired`.
- [ ] Audit: `sqlite3 "$C9WATCH_ENGINE_HOME/engine.db" "select principal,
      method,decision,reason from audit where method like 'agent.%' order
      by seq"` shows each create and retire, the refusals of step 3 as
      `deny`, and `system` / `parent-gone: parent <P>` for the child and
      `parent-gone: parent <child>` for the grandchild (a child may have
      more than one such row).

## 6. Limits (optional)

From one agent, the 9th live child fails with "live children"; a chain
deeper than 3 below the root fails with "depth 4"; a tree past 16 live
agents fails with "16 live agents".

## 7. Spawn overlay (E1, c9p 1.14)

`tests/live/e5_parent.sh` runs these checks as step 7.

- [ ] From the terminal, create a root with `"overlay":{"model":"sonnet",
      "permissionsDeny":["Bash(curl:*)"],"sandboxDenyWrite":["<dir>"],
      "standingOrders":"..."}`. agent.list shows the same `overlay`.
- [ ] Its `<home>/spawn/<id>-1.json` ends `permissions.deny` and
      `sandbox.filesystem.denyWrite` with the overlay entries; the
      engine's own `.claude` entries are still there.
- [ ] Its CC command line ends with `--model sonnet`.
- [ ] The `agent.create` audit row has `overlay {...}` in its reason and
      a `payload_hash`.
- [ ] In the root agent, a child create with
      `"overlay":{"permissionsDeny":["WebFetch"]}` gives a child with the
      parent's model, the parent's denies first, then `WebFetch`.
- [ ] A child create with `"overlay":{"model":"haiku"}` is refused
      (`not_allowed`). So are `"permissionsDeny":["Read(!*.env)"]` and
      `"permissionsDeny":["Read(./secrets/**)"]`.
- [ ] After `c9watch engine restart` the root's new spawn file and
      `--resume` command line carry the same overlay.
- [ ] Not checked by the script: whether CC enforces the added rules (a
      `!` command runs outside CC's permissions and sandbox), and whether
      the caller orders reach the model (INFO: grep the transcript for
      `[end caller orders]`).

## Cleanup

```sh
$C9 api agent.retire "{\"id\":\"<Q>\"}"
$C9 engine stop
rm -rf "$HOME/e5-test"
```
