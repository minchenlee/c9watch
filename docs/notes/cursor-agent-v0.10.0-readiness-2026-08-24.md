# c9watch v0.10.0 release readiness

As of 2026-08-24, this is a release-preparation candidate only. No merge,
push, tag, publish, or GitHub release was performed.

## Baseline

- Branch: `feature/cursor-agent-detection`
- Original review baseline: `242f987` (`feat: prepare v0.10.0 Cursor Agent support`)
- Review-fix candidate: the current HEAD, verified live with `git status` and `git log`
- v0.9.0 tag: `08b860d`; `origin/release/v0.9.0` points to the same commit.
- `origin/main`: `223b61c`.
- Known Cursor commits are present in HEAD: `13b9805`, `c26dff6`, `bd263eb`.
- The pre-existing note `docs/notes/cursor-agent-cache-review-2026-08-23.md` was preserved and committed.

## v0.10.0 scope

- Cursor Agent root and subagent discovery under the Cursor projects fixture
  layout, with optional read-only Composer metadata overlay.
- JSONL parsing for user/assistant/tool records, incomplete final lines, and
  `turn_ended` lifecycle state.
- Working/idle/connecting/waiting enrichment, parent-child hierarchy, history,
  conversation lookup, deep search, and provider-aware frontend filtering.
- Incremental cache append reuse with verified prefix hashing, full-parse
  fallback for truncation/prefix mismatch, strong Unix metadata stamps, weak
  metadata fallback, stale-entry eviction, and read-change retries.
- Cursor provider semantics in cost visualization and an explicit read-only
  history overlay.
- Provider-scoped session identity (`provider:sessionId`) across backend
  deduplication, conversation dispatch, CLI watch/cost resolution, frontend
  selection/maps, notifications, tasks, and provider hierarchy.
- Persistent Codex/Cursor source owners for CLI/web polling, plus a reverse
  JSONL tail reader that expands beyond a fixed byte window for large records.
- Persistent Codex archive cache correctness: strong ctime/identity stamps,
  cache-version invalidation, full logical-prefix hashing beyond the cheap
  anchor, and bounded retries when a rollout changes during a read.
- A shared `FileVersion`/cache-consistency primitive for file length, change
  time, and identity; append/reset/retry and provider parser/lifecycle rules
  remain provider-specific.

## Acceptance evidence

| Gate | Result | Evidence / boundary |
| --- | --- | --- |
| Focused Cursor tests | PASS | 26 passed, 0 failed; synthetic fixtures cover append reuse, bounded large-append validation, prefix/truncate-rewrite fallback, partial lines, lifecycle, delete/rename, same-length rewrite with preserved mtime, large transcripts, torn concurrent writes, and suffix-hash read failure |
| Focused Codex source tests | PASS | 39 passed, 0 failed; includes incremental hits, partial/truncation, same-length and middle rewrites, lifecycle, hierarchy, and conversation behavior |
| Focused Codex archive tests | PASS | 12 passed, 0 failed; synthetic fixtures cover partial lines, append/truncation, preserved-mtime same-length rewrite, anchor-external middle rewrite, merge/search, and token accounting |
| Provider-scoped CLI references | PASS | `cli::` focused suite: 50 passed, 0 failed; `view`, `tasks`, and `cost --session` accept `claudeCode:<id>`, `codex:<id>`, and `cursor:<id>` and disambiguate colliding raw IDs |
| Provider owner sharing tests | PASS | 6 passed, 0 failed; global and cloned owners share source state, second synthetic detection does not increment parse count, and test owners do not initialize production roots |
| Shared cache-stamp contract | PASS | 3 passed, 0 failed; shared `FileVersion` reads length/change-time/identity, weak metadata rejects unchanged fast path, and strong identity/subsecond stamp is accepted |
| Provider-aware rename protocol | PASS | 7 passed, 0 failed; explicit Codex/Cursor rename is rejected, providerless legacy requests retain their v0.9 shape, and a shared-owner collision guard rejects cross-provider IDs before any write |
| Synthetic Tauri/WebSocket collision regression | PASS | 3 passed, 0 failed; synthetic Cursor and Codex owners reject providerless rename before the Tauri writer callback or WebSocket title-write dispatch |
| Full Rust library tests | PASS | 359 passed, 1 ignored, 0 failed |
| Rust integration tests | PASS | library 359 passed/1 ignored; main 0; background test 1 ignored; parser integration 3 passed |
| Rust format | PASS | `cargo fmt --all --manifest-path src-tauri/Cargo.toml -- --check` |
| Frontend type/check | PASS | `npm run check`: 0 errors, 0 warnings |
| Frontend production build | PASS | `npm run build` completed with SvelteKit/Vite output |
| Version metadata sync | PASS | `package.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json` all report `0.10.0` |
| Rust release compile | PASS | `cargo build --release --manifest-path src-tauri/Cargo.toml` |
| Release workflow YAML parse | PASS | Ruby YAML parser loaded `.github/workflows/release.yml` successfully |
| Tauri app bundle | UNVERIFIED in this final pass | Rust release compile passed; no new native bundle run was performed |
| Tauri updater artifact | PENDING GHA evidence | Local signing requires `TAURI_SIGNING_PRIVATE_KEY`; workflow now fails closed when the secret/signature artifact is missing |
| DMG bundle | PENDING GHA evidence | Local generated `bundle_dmg.sh` previously failed; repository release path uses GHA `tauri build` |
| Apple notarization | PENDING GHA evidence | Workflow accepts Apple credentials; no local notarization was run |
| x86_64 CI/native E2E | PENDING | Workflow matrix contains x86_64; no completed GHA/native behavior evidence in this checkout |
| Diff whitespace | PASS | Review-fix commit range checked with `git diff --check HEAD^ HEAD` |

The provider-owner synthetic suite also passes 6/6: global and cloned Codex/Cursor owners
share their mutable source and their second detection reuses the same parsed
source, independent test owners remain isolated, and both Codex and Cursor
owners detect a temporary fixture without touching a real transcript root.

The shared cache-consistency contract passes 3/3 tests: `FileVersion` exposes
the same length/change-time/identity shape to provider caches, missing,
seconds-only, or missing-identity metadata is rejected for the unchanged fast
path, while a subsecond change time with file identity is accepted. The Codex
archive cache uses the same primitive and adds a persisted FNV prefix hash so a
middle rewrite outside its 256-byte anchor is detected before append reuse.

The initial pre-correction test run is not used as privacy evidence: an
existing polling smoke test called production discovery and could inspect the
real Cursor home. It was replaced with a tempfile-backed fixture before the
final gates above. Final Cursor fixtures use explicit temporary roots. Test
`ProviderSourceOwners` instances disable lazy production-root initialization,
and a regression test covers that boundary, so state/owner tests do not call
`CursorSessionSource::new()` or read real `~/.cursor` transcripts. The
enrichment fixture may still load optional Claude custom-name/title settings;
it does not read Cursor transcripts.

## Independent review

Sol's architect review is architecture input, not independent-review
evidence. Its final choice was staged extraction: share file-version,
append/reset/retry, complete-line cursor, prefix-hash/checkpoint invariants,
and test matrices, while keeping discovery, lifecycle, parser, and history
provider-specific. Its P1 findings (Codex CLI/Web source lifetime and Claude
large-record reverse-tail reading) were implemented before this review.

A separate bounded read-only reviewer completed an initial post-fix delta with
verdict `REQUEST CHANGES`; it explicitly marked that pass as **not a full
re-review** and did not run tests or builds. Findings and current status:

- P2 provider-unqualified rename: fixed locally after the delta review.
  Explicit non-Claude providers are rejected; providerless legacy requests
  retain the v0.9 protocol shape but consult the shared Codex/Cursor owners'
  metadata/path indexes and are rejected before writing when the raw ID
  collides across providers. The TypeScript API remains backward-compatible
  with an optional provider.
- P2 non-atomic filesystem snapshots: remains an explicit limitation. Reads
  use metadata/read/retry boundaries, but no reader promises an atomic snapshot
  during every concurrent writer race.
- P2 cumulative prefix I/O: fixed in the review-fix candidate. Transcripts up to
  4 MiB use exact prefix verification on every append. Larger transcripts use
  fixed-size head/tail guards between geometrically scheduled full-prefix
  checkpoints, so normal append validation has a bounded read budget and the
  cumulative full-revalidation work is linear in transcript growth. A synthetic
  byte-budget regression covers the high-frequency large-file path.
- P3 Cursor suffix-hash errors being swallowed: fixed. Hash/read failure now
  returns a parse error instead of persisting an incomplete hash, with a
  synthetic regression test.
- P3 frontend raw subagent key: fixed. `ExpandedCardOverlay` now keys and
  compares subagents with provider-qualified keys.
- Missing behavior-level frontend/native E2E and mutation/retry-exhaustion
  integration coverage remain open. Frontend behavior/native E2E is intentionally
  deferred to the user; `npm run check` and `npm run build` validate
  types/compilation only. The large-append validation budget is now covered by a
  synthetic regression rather than an unbounded benchmark claim.

The delta reviewer did not perform a final full re-review after the
collision-aware compatibility fix. A resumed independent reviewer then
completed a full current-delta review of the collision/owner scope with
verdict **PASS**: no confirmed P0/P1/P2/P3 findings. It verified that
providerless rename is rejected before the Claude custom-title write and that
the shared owners plus Codex/Cursor path checks are coherent. Two other
bounded contexts were stopped after they did not return a verdict; they are
not counted as review evidence.

The independent reviewer identified a missing regression: synthetic
Tauri/WebSocket end-to-end tests for both Codex and Cursor collisions that
assert no title/custom-title write occurs. That regression is now covered by
three tests: two Tauri rename-writer preflight tests (Cursor and Codex) and one
synthetic WebSocket dispatch test covering both providers. The tests use
tempfile-backed owners and never write a real title file. Frontend/native
behavior E2E remains a separate user-owned gate.

The provider/session ID collision paths are covered by `SessionIdentity`,
provider-aware conversation dispatch, collision-aware legacy rename validation,
provider-scoped frontend maps, Claude-only task/subagent lookup, CLI watch/cost
resolution, provider-scoped cost breakdown filtering, ambiguous legacy
conversation rejection, and provider-scoped notification keys. The GUI
`DetectorState`, CLI/Web enrichment, and rename collision guard obtain
Codex/Cursor sources from the process-wide `ProviderSourceOwners` registry;
cloned owners share mutable source state and synthetic tests cover that
lifecycle. Claude remains separate because its discovery/lifecycle contract is
different.

The unchanged fast path requires file identity plus subsecond change time.
Coarse or weak metadata deliberately falls back to content validation/full
parsing, retaining correctness at the cost of additional I/O. The popover now
uses provider-scoped keys for hierarchy filtering and keyed rendering, and
frontend `sessionKeyOf` derives `provider:id` from canonical fields rather than
trusting a stale serialized key.

## Release workflow review

`.github/workflows/release.yml` triggers on `v*` tags and builds macOS
`aarch64` and `x86_64`. A serial `source-gates` job now blocks both release
build matrices until diff hygiene, Rust format/tests, and frontend check/build
pass. The macOS job fails closed when updater signing or Apple signing and
notarization credentials are absent, then verifies the built app with
`codesign`, `stapler`, and `spctl`, validates the DMG with `hdiutil`, and checks
that the signed updater archive and signature exist. The workflow preserves the
exact Tauri-generated `.app.tar.gz` bytes that were signed, renames those
artifacts (without re-tarring the `.app`), renames DMGs to
`c9watch_<tag>_<arch>.dmg`, and generates `latest.json` URLs matching the
published archive names. The updater endpoint and public key remain configured
in `src-tauri/tauri.conf.json`; `createUpdaterArtifacts` is still `v1Compatible`.
The workflow changes have not yet been exercised by a GitHub Actions run in this
checkout.

## Decision

- Ready for PR: **pending final re-review** — the two P2 findings and two P3
  findings from the full independent review have been addressed in the
  review-fix candidate, but the same reviewer must confirm the fixes.
- Ready for release: **no** — local DMG packaging, updater signing,
  notarization, cross-architecture GHA evidence, and native app E2E are not
  all green; no release authorization was given.
- Smallest next action: hand the frontend/native E2E gate to the user, then
  rely on the repository GHA workflow for DMG, signing, notarization, and
  architecture evidence.
