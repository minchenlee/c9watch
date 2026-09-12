# Codex Desktop messaging / interactions: candidate evidence

Status: **NOT merge-ready: updated-main integration and remaining native acceptance are blocked.** Automated gates and
the synthetic performance checks below pass. This is a local candidate, not an
updated PR or a release approval. No push, PR mutation, merge, signing, or
notarization was performed.

## Provenance and scope

- Branch: `codex/messaging-merge-ready-20260912`.
- Worktree: `/private/tmp/c9watch-messaging-ready`.
- Base: PR #128 head `f906bff8291398fa1896a08d26a333c7f6f41692`.
- Remote rechecked on 2026-09-12: OPEN, non-draft, UNSTABLE; remote head still
  `f906bff`, base `d3fe23e56bcce587030087d342d2aba477ec2f10`.
- Shared task: `CODEX-MESSAGING-128`, owner `codex-messaging-maker-20260912`.
- No delegation. No OpenCode, Claude messaging, widget, or unrelated status
  snapshot changes were copied. The existing mixed checkout and earlier
  messaging-sync worktree were read-only throughout this work.
- Main checkout tracked-diff SHA-256 before/after:
  `db4423caafe815a43efe35ca4c346304d7fa50c868bb0abd36e0c9e2a6ca3de9`.
  Messaging-sync tracked-diff hash before/after:
  `2b9b399733f4b068663941353004ea67e7e5010c0c8094127b4b398f26dac778`.
  These hashes do not establish byte identity for untracked files. Another
  task owns promotion work in the main checkout; this candidate does not.

## Changes

1. FIFO root cause: `attach_codex_images` previously compiled normalization out
   of CLI builds. The CLI therefore returned literal generated image markup.
   Normalization now runs in every build; GUI still checks regular files before
   open, uses `O_NONBLOCK`, rechecks the descriptor, and bounds reads. The FIFO
   regression remains enabled in both Unix GUI and CLI builds with its original
   fallback assertion and deadline.
2. Image safety: reject header-only, corrupt, animated, oversized, or excessive
   raster images using limited PNG/JPEG/WebP decoders. Validate before composer
   preview and again before sending. Validated content uses a bounded SHA-256
   cache, never a path/mtime-only cache. Image decoding runs off async workers.
3. Active sends use `turn/steer` with the observed `expectedTurnId`, including
   stale active snapshots. Idle sends use `turn/start`. A rejected steer never
   becomes start; only the matching acknowledgement is accepted. An uncertain
   or late acknowledgement remains unknown with no automatic retry.
4. Foreground/visibility refresh shares the selection's in-flight guard, retains
   provider/request identity, cleans up listeners, and preserves loaded history
   on errors. A scoped refresh error is visible rather than silently stale.
5. Native scans share four blocking-worker admission slots. WebSocket
   `GetSessions` and conversation reads use the same gate, preserving the wire
   shape and provider interfaces. Cancellation does not release a slot while
   its blocking job is still running. No provider-specific behavior was changed.
6. Conversation lookup does not wait for the optional archive-cache mutex. It
   falls back to the authoritative filename walk while an archive rebuild owns
   the lock. Preview record/file/segment limits return explicit errors; they do
   not silently claim a complete truncated history. Per-file temporary parsed
   caches are dropped rather than retaining redundant image/transcript copies.
7. Bridge writes and the initial WebSocket handshake have three-second
   deadlines. Stalled peers close and clean up, retaining unknown-delivery
   semantics. Composer stop retention is bounded without evicting uncertain
   identities. Added zero-field MCP confirmation regression and a native fixture
   with a fake messaging owner; it never contacts a model or writes transcripts.

The steering contract was checked against [official Codex App Server docs](https://learn.chatgpt.com/docs/app-server#steer-an-active-turn).

## Resource boundaries

| Resource | Boundary |
|---|---|
| Native/WebSocket scans and attachment validation | Four admitted blocking jobs; fifth gets a busy error; permit remains with cancelled job |
| Messaging send preparation | Four concurrent sends; one per session |
| Owner discovery | Four concurrent checks, at most eight bridge endpoints plus daemon; three-second per-endpoint deadline |
| Messaging/bridge frames | 8 MiB; original bounded control queue/connection handling retained |
| Interaction queue / pending requests | Eight controls / 64 pending requests |
| Transport write / handshake / message ACK | 3 s / 3 s / 10 s |
| Composer drafts | 20 provider-qualified drafts; 32 KiB text each; 16 MiB encoded images per window |
| Composer attachments | Four static images, 4 MiB decoded-file bytes total |
| Individual image | 4096 px per side; browser RGBA estimate and decoder output each at most 32 MiB |
| Decoder allocation budget | 64 MiB where codec-supported, not a claim about total process RSS |
| Validation cache | 128 SHA-256/mime records, no pixel or original-byte buffers |
| Conversation preview images | Eight images / 12 MiB encoded across returned segments, with explicit unavailable fallback |
| Conversation preview input | 8 MiB record, 128 MiB aggregate, 512 segments; over limit is an error |
| Archive retained message bodies | Existing 64 MiB cap retained and exercised |
| Stop identity retention | 1024 entries; at capacity use Codex to stop, never evict unknown identities to retry |

## Fresh verification

Host: Apple Silicon, macOS 26.1 (25B78), Rust/Cargo 1.95.0, Node 26.8.1,
npm 11.19.0. Installed Codex: `0.154.0-alpha.6.2`. CI uses Node 20, so this is
CI-equivalent command coverage, **not** a fresh GitHub CI run or identical Node
environment. Existing Rust warnings remain (GUI 13, CLI 47); no warning cleanup
outside this feature was attempted.

Rust commands used `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`,
`CARGO_INCREMENTAL=0`. Cached dependencies allowed `--offline`; the final lock
file is retained. `cargo test` was also rerun after the image dependency/cache
changes. No old document's passed count was used as current evidence.

```sh
cd /private/tmp/c9watch-messaging-ready
npm ci
npm run check
npm run build
node --test scripts/test-ws.mjs
node --conditions=browser --test scripts/test-conversation-selection.mjs scripts/test-codex-composer.mjs scripts/test-codex-interactions.mjs
node scripts/test-subagent-polling.mjs
node scripts/test-subscription-polling.mjs
node scripts/test-usage-preferences.mjs
cargo check --locked --offline --manifest-path src-tauri/Cargo.toml
cargo test --locked --offline --manifest-path src-tauri/Cargo.toml
cargo check --locked --offline --manifest-path src-tauri/Cargo.toml --no-default-features --features cli
cargo test --locked --offline --manifest-path src-tauri/Cargo.toml --no-default-features --features cli
git diff --check
```

- Baseline FIFO test reproduced the reported CLI failure before changing code.
- GUI: **465 passed, 0 failed, 9 ignored**, plus **3 integration tests passed**.
- CLI: **398 passed, 0 failed, 4 ignored**, plus **3 integration tests passed**.
- Separate background end-to-end test remains ignored in both feature sets.
- Two newly ignored tests are opt-in synthetic measurement diagnostics, both
  explicitly executed separately. No failing regression was ignored or relaxed.
  Other existing live/manual ignored tests remain unverified.
- Frontend: **33 feature regressions**, **3 WebSocket regressions**, and all three
  polling/usage scripts pass. Svelte: **0 errors, 0 warnings**. Build passes.
- Dependency installation first encountered a sandbox/network timeout and
  succeeded on the authorized retry. A later bridge test initially used the
  CLI-only `target/debug/c9watch` overwritten by CLI tests; rerunning with the
  hash-identified GUI bundle passed. Always use the GUI bundle for bridge tests.

Final local logs: `/private/tmp/c9watch-messaging-{gui-final,cli-final,check-final}.log`.
Frontend command output is retained in this task's tool results.

## Failure-mode evidence

| Case | Evidence |
|---|---|
| Duplicate send, late result, session switch, unknown delivery | Actual composer-handler regression; original session draft alone changes, no automatic retry |
| Active/stale steering, mismatched ACK, rejected steer | Composer regression plus fake WebSocket Rust test; exact turn identity, no fallback wire request |
| Late ACK / cleanup | Ten-second ACK timeout test observes one message, unknown result and closed connection |
| Duplicate stop / late stop receipt | Composer handler and registry tests; exact turn key, terminal state event-driven |
| Edited chain / second edit / abandoned tail / missing ancestor | Existing Codex paginated-history regressions rerun in GUI and CLI |
| Unloaded vs duplicate owner | Loaded-thread transport tests and frontend unloaded-endpoint vs live approval regression |
| Request identity / provider isolation | Owner request-id/race tests, WebSocket correlation tests, conversation provider/selection regressions |
| Question / command / file / permissions / MCP forms | Actual Rust bridge plus fake owner exercises both race orders, clearing and typed payloads |
| MCP zero-field / explicit zero / false | Backend zero-field object validation and frontend form regressions |
| URL completion | URL-scheme tests; native fixture opening alone did not complete, explicit completion cleared card |
| Approval race / changed proposal | Original token invalidation and once-only decision tests, both owner/c9watch race orders |
| FIFO / malformed / missing / directory / excessive images | GUI and CLI fallback tests; full decoder and composer pre-preview validation tests |
| Huge history / compressed raster | Ordinary and paginated record/file limits; small encoded image cannot bypass raster limits |
| Polling bursts / cancellation | 100-trigger coalescing tests; four held scans keep async timer live and retain permits after cancellation |

Actual GUI-binary transport commands (no model):

```sh
QA_BINARY='/private/tmp/c9watch-messaging-ready/src-tauri/target/debug/bundle/macos/c9watch Messaging Candidate.app/Contents/MacOS/c9watch'
python3 scripts/experimental/test-codex-interactions.py "$QA_BINARY"
python3 scripts/experimental/test-codex-backpressure.py "$QA_BINARY"
python3 scripts/experimental/test-desktop-bridge.py --rust-binary "$QA_BINARY" --kill-bridge
```

Owner harness passes questions, multi-answer payloads, both race orders,
resolved events, command/file/permission/MCP decisions, stop/plan/diff,
cancellation and EOF cleanup. Installed-Codex probe passes argument/config
inheritance, fragmented UTF-8/CRLF, unchanged approval-decision frames,
incomplete EOF rejection, separate client request-ID spaces, 0700/0600
permissions, and cleanup of **its own** server after SIGKILL. No live model or
real Desktop approval was invoked. Latest logs:
`/private/tmp/c9watch-messaging-{final-owner,backpressure-final,installed-final}.log`.

## Performance evidence

Synthetic archive: 96 sessions × 768 rows, 83,387,136 transcript bytes. Each warm
series is 20 reads. Debug single-host measurements are indicative, not a
statistical SLA or release-performance certification.

| Metric | Baseline lock behavior | Final candidate |
|---|---:|---:|
| Cold archive | 5659.8 ms | 5791.8 ms |
| Warm median / p95 | 1.263 / 1.336 ms | 1.325 / 1.501 ms |
| First conversation | 19.36 ms | 28.11 ms |
| Conversation with 500 ms archive lock held | 533.78 ms | 20.01 ms |
| Process-cache-cleared disk reload | Not measured | 1561.3 ms |
| Retained message bodies | 67,098,490 B | 67,098,490 B (under 64 MiB) |

Cache file: 71,305,145 B; warm reads and simulated process restart do not rewrite
it. Final candidate archive measurement peak RSS: 317,685,760 B (303.0 MiB).
Prior candidate runs measured 215,416,832–319,209,472 B; allocator/OS accounting and run
conditions vary. This is not evidence of a hard total-RSS cap.

Four held blocking jobs let the current-thread async timer fire in **17.69 ms**;
both native and real WebSocket dispatch reject a fifth scan. Cancelling callers
does not free the busy slots until the I/O jobs finish.

Bridge stall tests: baseline did not exit within the seven-second observation
budget. Latest GUI candidate exits in **3.602 s** for blocked stdout and
**3.205 s** for a stalled handshake, removes its runtime directory, and peaks
at **18,928 / 14,800 KiB** bridge RSS. Exit code 1 with a timeout is the expected
safe failure. The owned child and sockets are not leaked.

4K PNG decoder measurement: same 3840×2160 pixels encoded as a 15,632,178-byte
file are rejected by the 4 MiB limit; a 100,408-byte losslessly encoded version
is accepted. Before validation caching, full validation median/p95 was
**56.50 / 57.49 ms**, peak process RSS **71,434,240 B**. With SHA-256 validation
caching: cold **57.71 ms**, warm median/p95 **3.02 / 3.19 ms** across 20 reads,
peak process RSS **71,761,920 B**. Warm reads hash all bytes but do not decode
the pixels again. The cache holds only 128 small success records.

Reproduce diagnostics (use the environment flags above; `--no-run` prints the
current test executable for an isolated `/usr/bin/time -l` RSS run):

```sh
cargo test --locked --offline --manifest-path src-tauri/Cargo.toml --lib --no-run
cargo test --locked --offline --manifest-path src-tauri/Cargo.toml --lib messaging_archive_measurements -- --ignored --nocapture --test-threads=1
cargo test --locked --offline --manifest-path src-tauri/Cargo.toml --lib image_validation_measurements -- --ignored --nocapture --test-threads=1
cargo test --locked --offline --manifest-path src-tauri/Cargo.toml --lib native_scans_leave_async_timers_live_and_bound_cancelled_work -- --nocapture
```

Evidence: `/private/tmp/c9watch-messaging-{perf-baseline,archive-cache-final,image-cache-final,scan-final}.log`.
Candidate repeats put cold archive at 5.18–5.79 s, warm listing below 2 ms, and
first conversation at 18.99–28.11 ms. The final first-conversation sample was
8.75 ms slower than baseline; do not conceal this or interpret one run as a
statistical non-regression proof. Lock contention improves substantially. These
bounded fixture results do **not** establish native
Desktop/model performance, browser raster RSS, or long-duration leak freedom.

## Native QA and remaining gates

Final unsigned artifact (subsequently launched and partially accepted below):
`/private/tmp/c9watch-messaging-ready/src-tauri/target/debug/bundle/macos/c9watch Messaging Candidate.app`.
Binary SHA-256: `a874666c4837d4d38a361d5994b11d93a1cb0e31515b6888fddc35f37da0556e`.
The no-model transport/cleanup probes above were rerun against this exact binary.
Build command:

```sh
npm run tauri -- build --debug --bundles app --no-sign --config '{"identifier":"com.minchenlee.c9watch.messagingcandidate12","productName":"c9watch Messaging Candidate","bundle":{"createUpdaterArtifacts":false}}'
```

Build log: `/private/tmp/c9watch-messaging-candidate-bundle.log`. Use the Rust
profile environment flags from above. This distinct identifier avoids treating
an already-running older QA app as the final candidate.

All synthetic controls explicitly said QA/no real Codex operation. No user
transcript was edited, no real turn was stopped, and no real approval was sent.

- Earlier unsigned `c9watch Messaging QA 20260912` bundle, binary SHA-256
  `86c674e971fa80612fe4ca0a40ad1f0263a0f5a1a7eff22b70ecb06e56246b0f`:
  native multi-question Unicode answer, command Reject, reviewed-file Approve,
  read-only permission selection, MCP zero/false/typed choice, zero-field form,
  URL open + explicit completion, and clearing cards were observed. The parent
  history rendered 22 messages. Recheck connected the fake owner. Desktop
  support correctly refused to relaunch while ChatGPT was running.
- Its raw synthetic answer/decision evidence remains under
  `/tmp/c9watch-codex-501/deb6eb14-0aae-40d8-84e3-e9a13d582a8e/`.
- Later `c9watch Messaging Ready QA` bundle, SHA-256
  `1783b7839e88b643ab8f903239bbdf9de7620d0aaef6ea6b0ba12d83e0a6ca88`:
  main UI rendered without opening the inspector, fake-owner seven cards and
  composer were visible, and history progress reached 37%. Valid PNG selection
  enabled Open. After Open, the UI tool returned a tiny/empty panel, then
  `noWindowsAvailable`; repeated attachment preview confirmation was impossible.
- Native getApp once took **618.45 seconds**. A process check shortly after its
  return showed the new process only 42 seconds old, so this is not a measured
  618-second app startup. Old-app/Dock tool calls also timed out. Neither
  attributing all failures to app code nor dismissing them as tool-only is
  justified. Main RSS snapshots were 153,728 KiB then 89,760 KiB about 12 minutes
  after launch; these exclude WebKit child processes and are not a soak test.

Remaining gates at initial handoff (see the superseding follow-up below):

1. Reliable native access to the final candidate: confirm image preview, actual
   synthetic composer send/steer and Stop, recheck/disconnect, refreshed history,
   close/reopen and ordinary cold startup. Do the stop/send checks before opening
   the file picker so a picker failure does not obscure other evidence.
2. Repeat representative native cold/warm and WebKit-inclusive RSS measurements;
   fixture numbers above do not substitute for this gate.
3. Real Desktop approval/model compatibility remains separately unverified.
   The user must decide when to stop their Desktop tasks and run that gate.
4. After native acceptance, ask permission before pushing or updating PR #128,
   then obtain fresh remote CI. Do not merge automatically.
5. Signing, notarization and release acceptance remain separate and unverified.

Use `scripts/experimental/codex-interaction-ui-fixture.py VISIBLE_CODEX_THREAD_UUID`
to create fresh local synthetic controls. It exits after 20 minutes or SIGTERM,
removes only its own sockets/ready marker, and keeps synthetic evidence. It does
not load or resume that thread in Codex. Recheck is required after starting it.

## Final-bundle follow-up: native evidence and integration boundary

On 2026-09-12 at approximately 23:42–23:52 Asia/Taipei, the user confirmed the
candidate was operable and explicitly authorized native launch/control. The
worktree was clean at implementation commit `d77e20d`; the binary hash matched
the final artifact above. No implementation changed during this follow-up.

### Accepted on the exact final bundle

- A single composer send cleared its draft and displayed "Accepted by the
  current Codex turn." The fixture recorded exactly **one** `turn/steer`,
  `expectedTurnId: native-ui-fixture`, correct thread identity, and the complete
  text `QA_ONLY native candidate steer 中文 20260912`. No real model was contacted.
  After a clipboard-tool timeout, an AX read verified the full draft; there was
  no blind retry or duplicate submission.
- The multi-question answer preserved `Local` and `native candidate QA 中文`;
  its card cleared. Command Reject, file Approve after the review checkbox, and
  permission Approve with **only** `read:0` each cleared their synthetic card.
  No command, file change, or actual permission grant occurred.
- Typed MCP recorded integer `0`, boolean `false`, string `candidate fixture`,
  and choice `local`; zero-field MCP recorded `{}`. Both cards cleared.
- Opening the URL retained its request and explicitly said opening did not
  approve it. Selecting completion and confirming cleared the card.
- Stop changed the fake turn to interrupted/idle, removed Stop, and displayed
  a disabled send control with "Stopped". No real turn was stopped. Native
  duplicate-stop delivery was not injected; prior automated tests cover it.
- Open history refreshed from 24 to 25 to 26 messages, showing independently
  appended parent-task messages without closing the preview. The monitor showed
  27 after restart. No transcript was edited for this evidence.

Synthetic payloads remain under
`/tmp/c9watch-codex-501/2fe950f0-924a-4724-aea9-499026ac3658/` in
`messages.jsonl`, `answer.json`, and `decisions.jsonl`. Fixture PID 53805 was
stopped with SIGTERM; it exited 0 and its sockets/ready marker were removed.
Its output also logged two WebSocket peer disconnects without close frames;
these are not clean WebSocket closing-handshake evidence. The message log still
contains exactly one delivery. No live-model output is stored there.

### Picker/restart and native memory boundary

The valid 2196-byte 32×32 PNG was selected. Initially an AX-selected row left
Open disabled; keyboard navigation selected it with Open enabled. Confirming
produced `noWindowsAvailable`/`timeoutReached`, including reacquisition of the
running candidate. Raise and refreshed AX state did not resolve it. **Image
preview and image-bearing send remain unaccepted.**

The process remained alive. `/usr/bin/sample 50878 1 1 -file
/private/tmp/c9watch-candidate-picker.sample.txt` showed 400/415 main-thread
samples in the normal AppKit event-loop wait; the remainder was WebKit/
RunningBoard IPC. No synchronous archive scan occupied this thread in that
one-second sample. This does not prove UI usability or rule out a WebKit or
automation issue.

Native Cmd-Q succeeded: PID 50878 and WebKit PIDs 50892/50893/50894/50897 exited.
Relaunching the same app path created PID 55310 and WebKit PIDs
55313/55314/55315/55316. The initial AX window returned after 6540 ms without app
content yet; a later read showed the working monitor. **6540 ms is not a measured
time-to-ready or app-only startup time.** The temporary bundle ID did not resolve
after quit; its known full path did. No other app was stopped.

Read-only `ps -o pid,rss,etime,command` snapshots (KiB):

| Observation | Candidate RSS | WebKit-inclusive sum |
|---|---:|---:|
| Original process at ~6m26s | 104,560 | 222,144 |
| After picker at ~11m06s | 129,936 | 342,560 |
| Restart at ~22s | 303,296 | 445,584 |
| Restart at ~56s | 153,280 | 285,040 |

The four WebKit processes are attributed by matching launch/termination
lifecycle, not a privileged kernel responsibility query. The sums include GPU,
networking, and both content processes and may double-count shared pages.
Independent WebContent `vmmap -summary` reported 318.2 MiB physical footprint /
594.3 MiB peak with probabilistic guard malloc enabled; footprint is not RSS and
is not added to the table. These observations establish neither a long-run
memory cap nor native non-regression against a baseline. Image preview/send,
idle start, session-switch draft QA, post-disconnect Recheck, stable time-to-ready,
and controlled native memory/latency comparison remain open.

### New main conflicts: stop before crossing feature ownership

OpenCode PR #127 was squash-merged while QA resumed. GitHub's main ref resolved
to `69e2288b7d103a957a97d9ff341e3c11f1fc0bf5`. Fetch changed only local Git refs;
no main-checkout files or remote refs were written. The following diagnostic
created only Git objects, not a merge commit, index changes, or conflict files:

```sh
git fetch origin main
git merge-tree --write-tree --name-only origin/main d77e20d
```

It exited **1**, with conflicts in:

- `src-tauri/src/lib.rs`: main's shared `blocking::scan(DISCOVERY, ...)` versus
  candidate admission; both providers' Tauri command registrations.
- `src-tauri/src/web_server.rs`: competing shared scan dispatch.
- `src/routes/(app)/+page.svelte`: main polls only OpenCode and uses local error
  state/manual retry; candidate adds scoped error state and foreground/Codex polling.
- `scripts/test-conversation-selection.mjs`: those incompatible polling/error contracts.
- `src/lib/components/SettingsTab.svelte`: Integration versus Codex support sections.
- `src/lib/components/ExpandedCardOverlay.svelte`: transition import and retry/error styles.

Semantic review beyond conflict-marker removal is required: for example, both
error representations occur in the automatic merge. No conflict was resolved
and no OpenCode code was copied into the candidate. Per the user's explicit
stop condition, shared dispatch, refresh, and settings integration requires a
new scope decision before editing.

PR #128 remained OPEN at `f906bff`, now **CONFLICTING / DIRTY**. Its metadata
still reported base OID `d3fe23e`; the separately queried main ref and local
merge-tree verified the new conflict. No push, PR update, or merge occurred.
The earlier passing test counts apply to `d77e20d`, **not** an integrated tree.

Next: authorize isolated integration with current main while preserving
OpenCode's accepted behavior; resolve both contracts, rerun affected gates,
rebuild, and complete native QA before asking for any push/PR update. Real
model/Desktop approval, signing, and release remain separate gates.
