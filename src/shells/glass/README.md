# Glass shell

`ui.shell = glass` (localStorage) swaps the root `<slot/>` for `Shell.svelte`.
The root layout reads `ui.shell` before the first render, so only one shell
mounts. The `/popover` window stays Classic in either shell.

Switching shells at run time: glass → classic is immediate (glass stops its
feeds on unmount). Classic → glass needs a reload: Classic's session listeners
have no teardown, so once Classic ran in a window it stays until the reload.

Rules:

- Glass imports only `src/lib/data/`, `src/lib/c9p-1.ts` and its own files.
  It never imports Classic components or `$lib/stores/*`.
- Glass starts its own feeds (`data.ts`); Classic's `(app)/+layout.svelte` does not run.
- Pages are URL hashes (`nav.ts`): `#/needs`, `#/today`, `#/ws/<id>`,
  `#/ext/<extId>/<pageId>`, `#/usage`, `#/settings`.

Demo data: one fixture in c9p shapes, `src/lib/data/demo/glass.ts`, on when
`demoMode = 'true'` and `c9watch.demoGlass = '1'`. Data that c9p does not have yet
(goals, needs verbs, seat caps, rules) is in `model/gaps-demo.ts` only. The
shell shows fixture-only data (demo workspaces) only while the fixture is on.

View model (`model/`, pure, `scripts/test-glass-model.mjs`): `data.ts` exports
`model`, derived from sessions, agents and `agent.attention`. Workspaces are
repo roots (worktrees fold into their repo; id = hash of the root). A row is a
lead tree (`parentId`) or one talk session; demo goals only rename lead rows.
A tree that spans repos shows a row in each repo, all naming the same lead.
Needs are derived from c9p facts (sessions, open `agent.attention`, mail to a
human from the shared `$lib/data/stores/mail`); the gaps file is passed only
while the fixture is on. `agent.list` is read again on each `agent.*` event;
without that stream, on session changes (at most every 2 s).

Preview (no baseline): `npx playwright test -c tests/visual/glass-preview.config.ts`.
PNGs go to `tests/visual/.cache/glass-preview/`.

G1 frame:

- `tokens.css`: every glass token, scoped to `.glass-root`. `data-glass` =
  `off` / `subtle` / `full` (from `localStorage['ui.glass']`, `prefs.ts`).
  `scripts/test-glass-tokens.mjs` checks text contrast ≥ 4.5:1.
- `keys.ts`: the only keyboard handler (R7-SPEC §A.2). Pages register their
  keys with `usePageKeys`; a receipt page passes `receipt: true`.
  `scripts/test-glass-keys.mjs` covers text fields, IME and receipts.
- `Sidebar.svelte`: 需要你 → 今天 → workspaces (lead and talk rows) →
  extension pages → 用量 / 設定. `[` collapses it to a
  「需要你 · N」 button. ⌘N shows a `c9watch spawn` hint (no delegate in v1).

G6 extension host (`ext/`, glass build plan §3, manifest per owner D41 R1–R12):

- `manifest.ts`: validates `c9ext.json` (strict JSON, `c9ext: 1`,
  `ui.contract: "c9ui/1"`, id = directory name, ≤ 64 KiB, ≤ 16 pages).
  Status: `ok` / `manifest_error` / `version_mismatch` / `shadowed` /
  `dev_only`. The `core` block is never run. Tests: `scripts/test-glass-ext.mjs`.
- `registry.ts`: Tauri asks `localCommand('ext_list')` (G7; until then the
  list is empty), `vite dev` asks `/c9ext/_list`
  (`scripts/vite-c9ext-dev.mjs`: repo `extensions/` first, then
  `$C9WATCH_ENGINE_HOME/extensions/`). Both send raw manifest text
  (`ExtCandidate`); the host validates.
- `ExtPages.svelte` + `Mount.svelte`: `import()` the page entry into a
  shadow root, `mount(root, host)`; 5 s without `ready()` shows
  「extension 沒有回應」, a throw shows 「extension 停止了」. The last page
  stays mounted (`suspend` / `resume`). On an extension page J / K step
  `context.needsItemId` through the needs queue.
- Keys: a key from inside an extension goes to the extension first, except
  ⌘K, ⌘N and Esc; it sends unused keys back with `host.keys.forward`.
- `extensions/hello/` is `dev: true`: dev builds only. Tests:
  `npx playwright test -c tests/glass/playwright.config.ts` (vite dev, port 4181).
