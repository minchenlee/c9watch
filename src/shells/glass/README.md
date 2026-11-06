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

G3 workspace page (`#/ws/<id>`, `pages/Workspace.svelte`):

- Data: the `model` store only (workspace → rows → members) plus one read
  each of `history.list`, `cost.get` and `memory.list`, filtered by repo root
  (`pages/workspace-extras.ts`). Demo rules show only while the fixture is on.
- `ui/`: the classic card (`SessionCard`), `PixelStatus`, `PixelBar` and
  `AnnotationChips` (direction §10, core plan §8.2). Pure helpers in
  `ui/format.ts`; `scripts/test-glass-ui.mjs` covers them.
- Orange only for a needs-queue item. Cards have no approve / reject (G5).

G4 需要你 page (read-only):

- `pages/Needs.svelte`: the queue (280px, `$model.needs`, longest wait first
  until a lead reports blocked counts) next to the handling panel.
- `handling/HandlingPanel.svelte`: the one handling panel for every page
  (R7-SPEC §A.7). `handling/detail.ts` turns a `Need` into what the panel
  shows (pure, `scripts/test-glass-handling.mjs`). Live items show only c9p
  facts; reasons, options, evidence and shots come from
  `DEMO_NEED_DETAILS` in the gaps file, only while the fixture is on.
- 允許 cannot be answered from c9watch yet (Gap P): its main button is
  「在終端機回答 ⌘O」, which calls `openSession` (it sends nothing).
- 「以後直接做」 (D9 = C, 允許 only): hidden in live mode until core can
  answer permissions and keep rules; with the fixture it shows as
  「設計預覽，不會儲存」.
- Disconnect banner: on a feed error (`dataError`). Demo: set
  `localStorage['c9watch.demoGlassOffline'] = '1'` with the fixture on.
- A send button without a handler is off and reads 「預覽版：還不能從這裡送出」.

G5 actions (`handling/actions.ts`, pure, `scripts/test-glass-handling.mjs`):

- Answers go only to managed agents (engine, PTY): `msg.send` through the
  shared `$lib/data/stores/mail` (`replyTo` for a 決定 that came as mail).
  Observed sessions and 允許 get ⌘O only (`openSession`).
- 10 s undo = delayed send (`SendQueue`): nothing is sent before the timer
  ends. Each send has its own timer and toast (`UndoToasts.svelte`); ⌘Z
  undoes the newest, each toast has its own 復原. A failed send is not
  sent again; the item goes back to the queue with an error notice.
- Leaving the 需要你 page or closing the window sends nothing, and the
  toast says so. Leaving: the page cancels every waiting send on destroy.
  Closing only hides the window (src-tauri `lib.rs`), so the page also
  cancels on `tauri://close-requested`, `visibilitychange` = hidden and
  `pagehide`, and the queue checks `canSend` again just before each send.
  The item goes back to the queue; the draft stays in memory (module
  state) until c9watch quits. Send ids are unique for the window lifetime.
- Stop (managed agents only): `session.stop` after a second click, then a
  receipt. The stop lock is per session (`handling/handled.ts`), taken
  before the call, so switching items cannot start a second stop. The receipt does not take ⌘↵; J goes to the 「下一件」 it names.
- 給: c9watch does not send keys (a key sent as mail stays in the mail
  log); 「我沒有這把 key」 can be sent.
- Offline: nothing is kept for later. The main button is off and reads
  「斷線了，連上後再送」.
- Demo: the glass fixture answers `msg.send` and `session.stop` in
  `demoTransport` (`$lib/data/demo.ts`), so demo never reaches a core.
- Handled items (`handled` in `handling/handled.ts`) leave the queue, J / K
  and the side bar count. Marks, drafts and receipts use the request
  instance (`Need.instance`: the msgId, or the attention event, or the
  generation), so a new request with the same text is a new item.
- A mail request counts as answered when a mail names it in `replyTo`,
  comes from the asked side (a human or the recipient agent) and did not
  fail. Any `kind` counts: the engine writes a sent answer as `info`.
- `tests/glass/needs.spec.ts`: the 7 handoff §3 interaction tests.

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
  「extension 沒有回應」, a throw shows 「extension 停止了」. The timeout
  covers async hangs only: a synchronous loop freezes the window, the same
  as any trusted npm package (D31: same document, no iframe or worker). The last page
  stays mounted (`suspend` / `resume`). On an extension page J / K step
  `context.needsItemId` through the needs queue.
- Keys: a key from inside an extension goes to the extension first, except
  ⌘K, ⌘N and Esc; it sends unused keys back with `host.keys.forward`.
- `extensions/hello/` is `dev: true`: dev builds only. Tests:
  `npx playwright test -c tests/glass/playwright.config.ts` (vite dev, port 4181).
