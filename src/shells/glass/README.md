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
shell shows fixture-only data (demo workspaces) only while the fixture is on;
live mode lists no workspaces until the G2 derivation.

Preview (no baseline): `npx playwright test -c tests/visual/glass-preview.config.ts`.
PNGs go to `tests/visual/.cache/glass-preview/`.
