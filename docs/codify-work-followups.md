# Codify-work follow-ups

This note records two follow-up candidates identified after PR #116. They are
intentionally documented rather than implemented until the recurrence or the
inputs become stable.

## Memory/CPU regression benchmark

**Status:** Watch for repeat.

### Evidence

- The memory benchmark had to be rerun several times during the fix.
- An intermediate implementation completed the same CLI workload in about
  48.9 seconds, exposing a performance regression before the final correction.
- The final controlled run measured 925,286,400 bytes of baseline RSS versus
  316,194,816 bytes after the fix (65.8% lower), with a 6.05-second runtime.
- The repository CI currently checks compilation, tests, Svelte diagnostics, and
  the frontend build, but does not run a memory or RSS benchmark.

### Proposed next slice

If another memory or CPU regression occurs, add
`scripts/benchmark-codex-memory.sh` with a synthetic JSONL fixture rather than
reading a developer's real session history. It should emit a machine-readable
before/after report and start as a report-only check. A hard CI threshold should
wait until the fixture and cross-machine variance are understood.

### Trigger and acceptance

Revisit this when a second performance regression or another performance-focused
PR requires manual `/usr/bin/time -l` comparisons. The first useful acceptance
check is repeatable parsing throughput and bounded allocation on the same fixture;
only then consider a non-flaky CI budget.

## Local macOS release build command

**Status:** Watch for repeat.

### Evidence

- A local `npm run tauri build -- --bundles app` compiled and bundled the app but
  exited while generating updater artifacts because
  `TAURI_SIGNING_PRIVATE_KEY` was not available.
- The app itself was successfully built after a one-time override that disabled
  updater artifacts for the local build.
- The release workflow intentionally expects the signing key from GitHub Actions
  secrets, so the local and CI release paths have different prerequisites.

### Proposed next slice

If this happens again, add a clearly named `build:macos:local` command backed by a
local Tauri config that targets the `.app` and disables updater artifacts. Keep
the signed CI release command unchanged; the local command must not look like a
publishable release.

### Trigger and acceptance

The trigger is a second local release build failure caused only by missing signing
credentials, or a request from another contributor for a repeatable unsigned
`.app` build. Acceptance is a successful local `.app` build with the CI release
configuration and updater signing behavior left unchanged.
