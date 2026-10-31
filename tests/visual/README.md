# Classic visual harness

This suite proves that Classic stays pixel-identical. It is the M-c0 gate of the core-public plan.

## What it does

- Playwright renders the built Svelte frontend in WebKit. It runs headless only.
- No Tauri runs. `@tauri-apps/api/mocks` answers `c9p_request` / `c9p_subscribe` and the GUI-local platform commands. Session fixtures enter as c9p `SessionView` results through the real shared data layer.
- The answers come from recorded fixtures in `classic/fixtures/index.ts`. Demo mode uses the fixture transport in `src/lib/data/demo.ts`.
- The page clock is fixed at 2026-03-10 15:30 UTC. It stays paused, so app timers do not fire.
- CSS animations and transitions are off. A test-only init script adds this style. App code is unchanged.
- The tests wait for every bundled font in `static/fonts`.
- Each screenshot allows 0 changed pixels.
- A test fails when the app calls a command that has no fixture.

## Files

- `playwright.config.ts`: WebKit, viewport, clock zone, and the `vite build` + `vite preview` server.
- `harness/ipc-mock.ts`: the init script with `mockIPC`. It is bundled before the run.
- `harness/classic.ts`: open a page, settle it, take a screenshot.
- `classic/*.spec.ts`: one test per screen and state.
- `classic/__screenshots__/`: the baselines. Only CI makes them.

## Run it

```sh
npm run test:visual          # compare with the baselines
npm run test:visual:update   # write new snapshots
```

The first run needs WebKit: `npx playwright install webkit`.

## Baselines

- Never commit a baseline that you made on your machine. Fonts and WebKit builds differ between machines.
- The CI job `visual-classic` runs on `macos-15`. Playwright is pinned exactly, so WebKit is pinned too.
- With no baselines, the job makes them, runs the suite a second time against them, and uploads the artifact `classic-baseline`.
- The owner checks the images. Then a PR adds them to `classic/__screenshots__/` and deletes that path from `tests/visual/.gitignore`.

The committed 39 PNGs came from `visual-classic`, main run `41216712918`
at `14a548a`. They were copied unchanged, with matching SHA-256 digests.
The owner still needs to eyeball the supplied images and the real WKWebView app.

## Make the job blocking

- With baselines in the repo, any changed pixel fails the job.
- To block merges, add `visual-classic` to the required status checks of `main`.

## Local stability check

Run the suite twice into two temporary folders. Then compare the folders.

```sh
C9_VISUAL_SNAPSHOT_DIR=/tmp/a npm run test:visual:update
C9_VISUAL_SNAPSHOT_DIR=/tmp/b npm run test:visual:update
C9_VISUAL_SNAPSHOT_DIR=/tmp/a npm run test:visual   # 0 changed pixels expected
```

## Add a screen

1. Add the fixture data to `classic/fixtures/index.ts`. Use fixed times relative to `FIXED_NOW`.
2. Add the command to `harness/ipc-mock.ts` if it is new.
3. Add a test that calls `openClassic`, then `snap`.
