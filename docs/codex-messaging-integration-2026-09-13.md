# Codex messaging: authorized main integration and evidence

## Decision

Local implementation and automated/fixture gates pass; **PR #128 is not yet
merge-ready**. The tested candidate has resolved the current main conflicts,
but has not been pushed. Post-disconnect native Recheck, live history refresh,
image preview and repeat history opens are verified. The authorized native
comparison below includes 520 successful backend reads, all slow samples and
lifecycle-checked RSS. It is **not an unqualified native performance pass**:
live-host tail/RSS variability and app-only cold-start timing remain unresolved.
Real Desktop/model compatibility
and release acceptance are explicitly unverified, not inferred from fixtures.

This supersedes the integration and image-picker blockers in
[the previous evidence report](codex-messaging-readiness-2026-09-12.md).

## Provenance and edit boundary

- Branch: `codex/messaging-merge-ready-20260912`.
- Worktree: `/private/tmp/c9watch-messaging-ready`.
- Tested implementation/local merge commit:
  `000043a1fc31959a012d65522b0289e036897a53`.
- Parents: candidate `ce4e4eabbdc20dc29867e28cdcae613511f2a157` and current main
  `69e2288b7d103a957a97d9ff341e3c11f1fc0bf5` (accepted OpenCode PR #127).
- The user explicitly authorized shared scan/refresh/settings integration in
  this isolated candidate. This is a **local merge of main**, not a PR merge.
- All six conflicts resolved; no unmerged index entries; `git diff --check`
  passed; `git merge-base --is-ancestor origin/main HEAD` exited 0.
- OpenCode provider, its performance/regression modules, connection component,
  and safe transitions are byte-identical to main. No dirty worktree content
  from OpenCode, Claude messaging, widgets, or shared snapshots was imported.
- Main checkout remained read-only. Its tracked dirty diff SHA-256 before/after
  is `db4423caafe815a43efe35ca4c346304d7fa50c868bb0abd36e0c9e2a6ca3de9`.
  This does not hash untracked file contents. No agent delegation occurred.
- GitHub read-only recheck on September 13: PR #128 OPEN/non-draft at
  `f906bff8291398fa1896a08d26a333c7f6f41692`, CONFLICTING/DIRTY, old check FAILURE
  completed September 8. GitHub main ref independently resolves to `69e2288b`;
  PR metadata still reports old base `d3fe23e`. No remote writes occurred.

## Integration contracts

1. One shared blocking implementation: main's single-slot `DISCOVERY` remains
   separate from four-slot `SESSION_IO`. Native and WebSocket conversation
   reads use the same session gate. Archive/history/search/cost/image work is
   off async workers. Cancellation retains the permit until blocking I/O ends.
   Main's separate subagent gates remain intact.
2. OpenCode alone retains serial two-second HTTP polling. Codex refreshes on
   provider-qualified `[sessionKey, modified, messageCount]` changes, foreground
   wake, or explicit retry. Unchanged monitor snapshots do not reparse history.
   One hundred revisions during a read coalesce into one trailing read.
3. Manual retry shares the same in-flight guard. Selection/request/provider and
   tools-loaded identity guards remain; stale results cannot replace another
   session. Refresh errors retain old content and expose a scoped Retry.
4. Integration settings contain both Codex support and unchanged OpenCode
   connection UI; main's native-safe transitions are retained.
5. Existing FIFO fallback, limited image decoding, archive-lock avoidance,
   request identity, unknown/no-retry, and bounded transport/drafts are retained.

Revision limitation: `modified` is the session summary timestamp, not a new
filesystem-content fingerprint. An edit that leaves both timestamp and count
unchanged is not guaranteed an immediate passive refresh; foreground wake or
manual retry rereads it. Backend edited/paginated-chain correctness is tested.

## Fresh verification of the integrated tree

Host: Apple Silicon/macOS 26.1, Rust/Cargo 1.95.0. Frontend rerun uses
Node **20.20.2** and npm 11.19.0. Temporary runtime is under
`/private/tmp/c9watch-ci-node20/node_modules/.bin`; no global runtime/settings
were changed. This matches CI's Node major, not an identical GitHub runner.

```sh
cd /private/tmp/c9watch-messaging-ready
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
cargo check --locked --offline --manifest-path src-tauri/Cargo.toml
cargo test --locked --offline --manifest-path src-tauri/Cargo.toml
cargo check --locked --offline --manifest-path src-tauri/Cargo.toml --no-default-features --features cli
cargo test --locked --offline --manifest-path src-tauri/Cargo.toml --no-default-features --features cli
export PATH="/private/tmp/c9watch-ci-node20/node_modules/.bin:$PATH"
npm ci
npm run check
node --test scripts/test-ws.mjs scripts/test-hidden-transitions.mjs scripts/test-opencode-provider.mjs scripts/test-opencode-connection.mjs scripts/test-integration-settings.mjs
node --conditions=browser --test scripts/test-conversation-selection.mjs scripts/test-codex-composer.mjs scripts/test-codex-interactions.mjs
node scripts/test-subagent-polling.mjs
node scripts/test-subscription-polling.mjs
node scripts/test-usage-preferences.mjs
npm run build
git diff --check
```

| Gate | Actual result |
|---|---|
| Rust GUI | 485 passed, 0 failed, 13 ignored; 3 integration passed |
| Rust CLI | 417 passed, 0 failed, 7 ignored; 3 integration passed |
| Rust check GUI / CLI | Both passed; existing 13 / 48 warnings retained |
| Background end-to-end test | One existing ignored test per feature set; not claimed verified |
| Node 20 feature regressions | 38 passed, 0 failed |
| WS/provider/settings/transition regressions | 17 passed, 0 failed |
| Three polling/usage scripts | All PASS |
| Svelte check / production frontend build | 0 errors, 0 warnings / passed |

No regression was removed, relaxed or newly ignored to obtain green results.
Five opt-in performance/stress diagnostics were explicitly run separately below.
`npm ci` reported skipped dependency install scripts under the existing npm
policy; platform dependencies, checks and build succeeded. Node's optional
runtime installer was similarly skipped, so the official platform package
`node-bin-darwin-arm64@20.20.2` was installed in the same temporary prefix with
`--ignore-scripts`. No security policy was disabled.

Logs: `/private/tmp/c9watch-integration-{gui,cli,check,ci-node20}.log`.
The original FIFO CLI failure was reproduced before its root fix in the earlier
report; its original fallback/deadline regression passes in both fresh suites.

Failure-mode coverage rerun includes duplicate send/stop, late ACK, lost ACK and
unknown/no retry, stale steer/no fallback start, selection races, provider
isolation, edited chains/second edits/abandoned tails, unloaded vs duplicate
owners, approval token races, MCP zero-field/zero/false/typed choice, explicit
URL completion, FIFO/invalid/oversized images and bounded preview input.
These are automated/fake-owner results, not claims of real model execution.

## Reproducible performance evidence

Diagnostics use the profile flags above and the GUI test executable emitted by
`cargo test --locked --offline --manifest-path src-tauri/Cargo.toml --lib --no-run`.
Run each executable directly under `/usr/bin/time -l` to exclude cargo RSS:

```sh
QA_TEST='src-tauri/target/debug/deps/c9watch_lib-40bbf52ebb0dfb09'
/usr/bin/time -l "$QA_TEST" messaging_archive_measurements --ignored --nocapture --test-threads=1
/usr/bin/time -l "$QA_TEST" image_validation_measurements --ignored --nocapture --test-threads=1
"$QA_TEST" benchmark_transcript_scheduler --ignored --nocapture --test-threads=1
"$QA_TEST" session::opencode::regression:: --ignored --nocapture --test-threads=1
```

An initial unprivileged `time -l` could not query macOS process accounting even
though its test passed. The full measurement was rerun with accounting access;
no failed regression was bypassed. Executable hashes/names are build-local.

### Alternating archive before/after, three pairs

Before: pre-integration implementation `d77e20d`, existing executable
`c9watch_lib-0257917e177f9e5e`. After: integrated `000043a`, executable above.
Alternate before then after for each of three iterations, no build/test running
in parallel. Fixture: 96 sessions x 768 rows, 83,387,136 transcript bytes; 20 warm
reads per run. Values below are medians of the three run-level measurements.

| Metric | Before | Integrated | Change |
|---|---:|---:|---:|
| Cold archive | 5131.835 ms | 5447.917 ms | +6.16% |
| Warm p95 | 1.332 ms | 1.470 ms | +0.138 ms |
| First conversation | 22.829 ms | 24.421 ms | +1.592 ms |
| Conversation under 500 ms archive mutex hold | 18.989 ms | 19.549 ms | +0.560 ms |
| Process-cache-cleared disk reload | 1606.847 ms | 1666.048 ms | +3.68% |
| Peak process RSS | 319,078,400 B | 305,168,384 B | -4.36% |

Before cold samples: 5495.013, 5107.370, 5131.835 ms. After: 5447.917,
5166.480, **6897.692 ms**. The slow 6.90 s sample is not discarded. Other user
apps remained active. This small debug single-host sample does not establish
statistical equivalence or a native SLA. No material new algorithmic regression
was identified in these fixtures, but it would be incorrect to claim a general
native performance guarantee from them.

Retained bodies stay 67,098,490 B under the existing 64 MiB cap. Cache file is
71,305,145 B and unchanged by warm reads/process-cache-cleared reload. The prior
root fix's 534 ms -> approximately 20 ms lock-contention benefit is preserved.
Logs: `/private/tmp/c9watch-integration-{before,after}-{1,2,3}.log`.

### Scheduler, images, OpenCode and backpressure

- Real synthetic transcript parse: 46,526,670 B. Inline control took 1355.019 ms,
  max timer gap 1355.116 ms (3 ticks); bounded blocking took 1300.822 ms,
  max gap **9.426 ms (372 ticks)**. Full-suite cancellation test also verifies
  four held workers, independent discovery admission, actual WS busy dispatch,
  no early permit release and eventual recovery.
- 3840x2160 PNG, 100,408 B encoded / 33,177,600 B raster: validation cold
  58.564 ms, warm median/p95 **3.141/3.716 ms** (20 reads), peak RSS 71,876,608 B.
  Same pixels encoded as 15,632,178 B are rejected by the 4 MiB input cap.
- Unchanged OpenCode: 24-message cold 28.462 ms, cache median 0.116 ms,
  refresh median/p95 3.739/5.110 ms. 1200-message cold 484.490 ms, cache median
  174.985 ms, refresh median/p95 479.133/538.779 ms. Cache hits issue **zero HTTP
  requests**. Sixteen concurrent callers accept 1/reject 15, peak HTTP 1,
  recovery succeeds. These are integrated observations, not a new OpenCode SLA.
- Exact GUI-binary stalled stdout/handshake probes exit in **3.571/3.146 s**,
  expected failure code 1, peak bridge RSS **19,456/14,896 KiB**; owned runtime
  directory removed in both cases. Ten-second late-ACK test preserves unknown,
  observes one delivery and connection cleanup.

Logs: `/private/tmp/c9watch-integration-{scheduler-perf,image-perf,opencode-perf,backpressure}.log`.

## Exact native artifact and QA

Bundle: `/private/tmp/c9watch-messaging-ready/src-tauri/target/debug/bundle/macos/c9watch Messaging Integrated Candidate.app`.
Identifier: `com.minchenlee.c9watch.messagingintegrated13`.
Executable SHA-256:
`479faaffe9e37004a78d6d96867ae702ce1c5534c8fd53275f2abd091118278d`.

```sh
npm run tauri -- build --debug --bundles app --no-sign --config '{"identifier":"com.minchenlee.c9watch.messagingintegrated13","productName":"c9watch Messaging Integrated Candidate","bundle":{"createUpdaterArtifacts":false}}'
QA_BINARY='/private/tmp/c9watch-messaging-ready/src-tauri/target/debug/bundle/macos/c9watch Messaging Integrated Candidate.app/Contents/MacOS/c9watch'
python3 scripts/experimental/test-codex-interactions.py "$QA_BINARY"
python3 scripts/experimental/test-codex-backpressure.py "$QA_BINARY"
python3 scripts/experimental/test-desktop-bridge.py --rust-binary "$QA_BINARY" --kill-bridge
python3 scripts/experimental/codex-interaction-ui-fixture.py 01a0958d-91c9-7730-ab96-f80741a67a2b
```

Use the same Rust profile flags and Node 20 PATH. Build and all three probes
passed. Installed-Codex probe verifies inherited config, fragmented UTF-8/CRLF,
unchanged approval frames, incomplete EOF rejection, two client ID spaces,
0700/0600 permissions and its own server's SIGKILL cleanup. Logs:
`/private/tmp/c9watch-integration-{bundle,owner,installed}.log`.

Native UI observed on this exact bundle, all synthetic/no-model:

- Working monitor and 27-message parent history. Integration settings contain
  both providers. Launch with support refuses safely while ChatGPT is running;
  no real Desktop restart or persistent settings change occurred.
- Unicode draft survives closing/reopening the detail. Switching to an unloaded
  other session shows no original draft/composer and explains that history alone
  does not load an app-server thread. Returning restores the draft.
- One active `turn/steer` with `expectedTurnId: native-ui-fixture`, then one idle
  `turn/start`. Each ACK clears only its own draft. No real model call.
- Multi-question answer (`Local`, `integrated QA 中文`), command Reject,
  reviewed-file Approve, permissions with only `read:0`, typed MCP (`0`, `false`,
  `integrated fixture`, `local`), zero-field `{}`, and explicit URL completion
  each clear the corresponding card. Stop removes Stop and shows Stopped.
- **Native image preview passed**: own 2196-byte 32x32 PNG visibly rendered
  with Remove control. Image-bearing start recorded one text plus one PNG data
  URL (2950 characters), then ACK cleared attachment/text. Total fixture log
  contains exactly three messages (one steer, two starts), no duplicates.
- Picker initially returned a tiny/empty screenshot and a selected row with
  Open disabled. Cancelling, restoring ordinary window size, reopening and
  keyboard selection produced enabled Open and the visible preview. No source
  workaround or assertion relaxation was applied; native evidence now replaces
  the previous unaccepted picker result.
- Own fixture PID 70086 terminated with SIGTERM, exit 0; its ready/server.sock/
  interactions.sock were removed. Candidate changed to Disconnected/status may
  be outdated. Recheck after remount was **not completed**: Computer Use reported
  the Mac locked and required manual unlock. No other process was stopped.

Synthetic payload evidence: `/tmp/c9watch-codex-501/9c1cb558-6c2f-4605-ad6c-b93b23262233/`.
The fixture logged six peer disconnect exceptions without WebSocket close
frames. It still exited and cleaned up; this is not clean closing-handshake
evidence. Fake payload files remain for review; only its three runtime endpoints
were removed. No user transcript was changed for QA.

### Native memory boundary

Candidate PID 69833 and launch-correlated WebKit PIDs 69838/69839/69840/69846:

| Process age | Candidate RSS (KiB) | WebKit-inclusive sum (KiB) |
|---|---:|---:|
| 39 s | 319,360 | 486,192 |
| 5m49s | 111,104 | 256,224 |
| 11m58s, after image send | 126,048 | 257,360 |

WebKit attribution is launch-time correlation here, not yet confirmed by this
bundle's full exit lifecycle. Shared pages may be double-counted. No monotonic
growth is established by these few samples, and neither is a hard RSS bound or
long-duration leak freedom. The initial app acquisition tool took 47.666 s and
initially returned an empty window; it includes automation/activation time and
is **not app-only startup latency**. Native controlled cold/warm comparison
against an equivalent baseline remains unverified.

## Remaining gates / smallest next actions

1. Post-disconnect Recheck, live history and close/reopen QA completed after
   unlock; see the exact-bundle evidence below. Real provider/model acceptance
   is not implied.
2. Action-time confirmation was subsequently obtained for both unsigned local
   QA bundles; the native comparison is recorded below. Preserve the observed
   slow tails and varying RSS as limitations. A controlled app-only cold-start,
   matched-age memory and long-duration/real-provider run is still needed for
   an unrestricted native performance claim; do not relabel these gates green.
3. User decides when real Desktop tasks may be stopped/relaunched for a real
   approval/model compatibility gate. Current running Desktop was not disturbed.
4. Obtain separate explicit permission to push this candidate to PR #128's
   source branch, then obtain fresh GitHub CI/review. Never merge automatically.
5. Signing/notarization, release packaging and release gate remain unverified
   and separate from local development-preview acceptance.

## Unlock follow-up: September 13, 08:58-09:05 Asia/Taipei

User reported the Mac unlocked. Candidate HEAD `9b66f1b` was clean; implementation
remains `000043a` and executable hash remains `479faaffe9e37004a78d6d96867ae702ce1c5534c8fd53275f2abd091118278d`.
Main tracked dirty diff hash was again unchanged. No source or runtime policy
changed for these checks, and there was no push/PR mutation.

### Newly accepted on the exact integrated native bundle

- Reopened the same bundle by full path (old PID 69833 no longer existed).
  New PID 12134 showed a working monitor and the 27-message parent history.
  Tool acquisition took 8454 ms; a subsequent observation at 31,317 ms showed
  the working monitor. This is a coarse observation interval, **not** measured
  app-only startup latency or a completed before/after comparison.
- With no fake owner, Recheck completed at 08:59:01 and explicitly remained
  not connected; no composer/send action appeared and no thread was resumed.
- Started the existing no-model fixture for parent thread
  `01a0958d-91c9-7730-ab96-f80741a67a2b`. Recheck on the open view discovered it
  and showed the composer plus seven synthetic pending cards.
- Stopped only the freshly verified fixture PID 12452 with SIGTERM. All
  answer/form controls became disabled, approval actions were replaced with
  connection-lost/stale notices, and Stop became disabled/disconnected.
- Escape closed the detail. Reopening it and pressing Recheck completed at
  09:00:40 with explicit not-connected status. No delivery was attempted and
  no automatic retry/resume occurred.
- Opened the active Tesla child through its native parent sidebar and jumped
  to the latest user message, `解鎖了`. The 57-message history included the
  current turn's first two commentary messages. While the detail remained
  open, the newly emitted commentary beginning `斷線後的卡片與 Stop 已失效` appeared
  in the native accessibility tree without another navigation, manual retry,
  close/reopen or transcript edit. This witnesses the integrated live-refresh
  path (not just the earlier bundle's history behavior).
- Native Cmd-Q ended candidate PID 12134 and all four launch-correlated WebKit
  helpers 12137/12138/12139/12140. The post-quit UI lookup reported procNotFound,
  and read-only `ps` confirmed all five absent; other pre-existing WebKit
  processes remained. This establishes this bundle's exit cleanup.

Fixture command: `python3 scripts/experimental/codex-interaction-ui-fixture.py
01a0958d-91c9-7730-ab96-f80741a67a2b`. Local diagnostic directory:
`/tmp/c9watch-codex-501/6925e527-26e4-4a92-9369-b649ec19e774/`.
It exited 0, removed its sockets/ready marker, and retained only its image
fixture. No message/answer/decision log was produced. Its one no-close-frame
exception is not a clean WebSocket closing-handshake claim.

RSS observations for PID 12134 plus the four lifecycle-verified helpers (KiB):

| Process age | Main | GPU | Networking | WebContent 1 | WebContent 2 | Sum |
|---|---:|---:|---:|---:|---:|---:|
| 25 s | 112,144 | 23,968 | 8,496 | 50,000 | 17,568 | 212,176 |
| 3m31s, active history | 111,632 | 63,872 | 11,120 | 93,680 | 44,944 | 325,248 |

Different UI content makes these observations unsuitable for a leak slope or
native non-regression conclusion. Shared pages may be counted more than once.

### New action-time permission boundary

Before starting the controlled alternating comparison, the integrated candidate
was fully closed. The local baseline hash was verified as
`a874666c4837d4d38a361d5994b11d93a1cb0e31515b6888fddc35f37da0556e` at
`/private/tmp/c9watch-messaging-ready/src-tauri/target/debug/bundle/macos/c9watch Messaging Candidate.app`.
Computer Use rejected its launch as an unsigned locally built executable from
an unrecognized source and required **confirmation at action time**. This is
a safety-review boundary despite earlier general candidate-QA authorization,
not a baseline startup/performance failure. No fallback launcher was used.

At that point, the next approval needed to explicitly cover launching both the baseline and
`c9watch Messaging Integrated Candidate.app` from the same bundle directory for
local no-model comparison. This grants neither real Desktop/model execution,
push/PR update, nor signing/release authority. The user subsequently gave that
specific confirmation; the next section records the authorized work.

## Authorized native comparison: September 13, 09:55-10:25 Asia/Taipei

The user's action-time confirmation covered both exact unsigned bundles above.
Both executable hashes were verified again after testing and are unchanged.
Implementation remains `000043a`; no production source was changed in this
follow-up. The only new executable source is the read-only diagnostic
`scripts/experimental/benchmark-native-codex-history.py`.

### Method and reproducibility

- Alternate the baseline and integrated apps, one at a time. Before every
  measurement, use `lsof -nP -iTCP:9210 -sTCP:LISTEN` and `ps` to verify that the
  listener belongs to the exact authorized bundle, not another c9watch app.
- Open the same existing, read-only Codex history in the native UI first:
  thread `01a0958d-91c9-7730-ab96-f80741a67a2b`, 27 messages, approximately
  4.8 MiB on disk. Its file was not edited; the normal user workspace remained
  live. Obtain the app's ephemeral token from its native Mobile panel.
- The probe sends only provider-qualified `getConversation`, serially, through
  `ws://127.0.0.1:9210`. It checks request/provider/session identity and hashes
  canonical response data. It never sends a turn, answer, approval or stop.
- Bounds: 2-100 samples, one request in flight, 10 s/request, 90 s total,
  5 s connect, 2 s close, 16 MiB received frame, four-frame receive queue.
  Output contains no token or conversation body. The script requires Python's
  `websockets` package (already installed on this host).
- These measurements cover native backend parsing, serialization and local
  transport, **not WebKit painting, a cold OS page cache or real model work**.
  First request means first probe request after UI warm-up, not cold history.

```sh
cd /private/tmp/c9watch-messaging-ready
# Launch only with action-time approval; verify exact listener PID first.
lsof -nP -iTCP:9210 -sTCP:LISTEN
# Set C9WATCH_QA_WS_TOKEN to the current native Mobile-panel token locally.
# Do not put the token in a report, checked-in script or shared command log.
python3 scripts/experimental/benchmark-native-codex-history.py \
  01a0958d-91c9-7730-ab96-f80741a67a2b --samples 20
python3 scripts/experimental/benchmark-native-codex-history.py \
  01a0958d-91c9-7730-ab96-f80741a67a2b --samples 100
# Diagnostic, only for the two explicitly labelled profiled runs below:
/usr/bin/sample VERIFIED_QA_PID 5 1 -file /private/tmp/c9watch-native-history.sample.txt
```

All **520/520** requests succeeded, 27 messages / 12,191 canonical JSON bytes
each, identical SHA-256
`e680eb8649ccdc72ce313fb63f1b354eea2df8ef2d68fdb8c4afea8b738a782c`.
No automatic retries are implemented. The connection context exited each run;
the final baseline listener had no remaining probe connection before app quit.
This does not erase the separate fake-owner no-close-frame limitation above.

### Every run, including slow tails

Rows are in measurement order. Warm statistics exclude the first request only;
no slow sample is discarded. p95 uses nearest rank (19 warm samples in a
20-request run therefore make p95 equal the maximum).

| Run | Samples | First ms | Warm median ms | Warm p95 ms | Warm max ms | Reads >500 ms |
|---|---:|---:|---:|---:|---:|---:|
| Integrated after1 | 20 | 122.369 | 70.649 | 130.121 | 130.121 | 0 |
| Baseline before2 | 20 | 75.631 | 69.308 | 152.517 | 152.517 | 0 |
| Integrated after2 | 20 | 74.449 | 73.369 | 758.228 | 758.228 | 5 |
| Baseline before3 | 20 | 76.760 | 71.324 | 96.369 | 96.369 | 0 |
| Integrated after3 | 20 | 938.927 | 82.501 | 1006.497 | 1006.497 | 9 |
| Integrated after3, partly profiled | 100 | 693.915 | 594.141 | 1284.971 | 2198.641 | 94 |
| Baseline before4 | 20 | 650.511 | 82.767 | 1095.410 | 1095.410 | 6 |
| Baseline before4, partly profiled | 100 | 507.804 | 616.918 | 853.488 | 961.312 | 83 |
| Integrated after4, no profiler | 100 | 73.455 | 69.896 | 78.309 | 91.763 | 0 |
| Baseline before5, no profiler | 100 | 70.463 | 69.590 | 74.020 | 77.828 | 0 |

The last unprofiled pair differs by +0.306 ms median (+0.44%) and +4.289 ms
p95 (+5.79%). The slow 100-read control also degrades to 617 ms median versus
594 ms integrated, but integrated's p95/max are worse. This is evidence of
large live-host/debug variability, **not proof that all tail regressions are
absent**. Profiling only overlapped part of each labelled run; it is not proven
to explain the entire slowdown. Neither App Nap nor thermal throttling was
established as the cause. `pmset -g therm` recorded no warning. Other user apps,
including an unrelated older QA app, were left untouched.

Read-only five-second profiles show Codex conversation parsing and filename
walks on `tokio::runtime::blocking::pool`, and monitor detection on its separate
polling thread. Some threads are named `tokio-runtime-worker` despite the
blocking-pool stack; they must not be mistaken for core async workers. The
integrated profile also contains background subagent parsing in the blocking
pool. No dominant archive-mutex wait was identified. `codex.rs` differs from
the baseline only by the added `opencode_summary: None` output field; the
conversation parser is unchanged. These observations do not justify an
unrelated provider rewrite or silently broadening this feature's scope.

Logs: `/private/tmp/c9watch-native-{after1,before2,after2,before3,after3,before4}-ws.json`,
`/private/tmp/c9watch-native-{after3,before4}-stress-ws.json`,
`/private/tmp/c9watch-native-{after4,before5}-unprofiled-ws.json`, and
`/private/tmp/c9watch-native-{after3,before4}-history.sample.txt`.

### Native UI, RSS and process lifecycle

The fixed history's actual message body, not only its monitor-card summary,
was observed in both bundles. Integrated after4 then passed three consecutive
close/reopen cycles, with the actual body visible in the first post-click
observation: **1146, 790, 818 ms**. Those are automation-inclusive observation
bounds, not app-only render timings. Earlier integrated opens briefly showed
Loading and subsequently completed. An earlier baseline pilot stayed at
`27 EARLIER MESSAGES` without a body after a rapid reopen; that pilot is not
counted as a successful warm-read timing and was not observed in these three
integrated reopens.

Initial app acquisition ranged roughly 3-10 s in this campaign, sometimes
returning an empty window or a zero-session monitor. Later monitor observations
included variable tool/reasoning gaps (one was 58.8 s). They cannot establish
app-only startup latency; **no 58.8 s startup regression is asserted**. No
page-cache purge, other-app termination or real Desktop restart was performed.

RSS is KiB, summed across the QA process and four launch-correlated WebKit
helpers. Exit checks confirmed these helpers disappear with that exact app;
pre-existing helpers were excluded. Shared pages can still be double-counted.

| Run / point | App age | App RSS | WebKit-inclusive sum |
|---|---:|---:|---:|
| Baseline before2, after 20 | 1m09s | 127,664 | 266,464 |
| Integrated after2, after 20 | 1m10s | 149,440 | 287,696 |
| Baseline before3, after 20 | 58s | 109,296 | 212,864 |
| Integrated after3, after 20 | 1m43s | 131,008 | 263,648 |
| Integrated after3, after 100 | 4m22s | 108,992 | 234,688 |
| Baseline before4, after 100 | 5m48s | 110,848 | 197,936 |
| Integrated after4, after 100 | 1m07s | 154,544 | 307,136 |
| Baseline before5, after 100 | 1m26s | 117,920 | 241,952 |

The last unmatched-age sum is 26.9% higher integrated; it is **not normalized
away**. Other initial snapshots were higher for baseline (398,768 KiB app at
12 s versus integrated 318,240 at 22 s). Profiles reported main-process peak
physical footprint 293.9 MiB baseline / 291.2 MiB integrated, a different metric
from RSS. There is no demonstrated monotonic growth in these short observations,
but neither a hard native RSS bound, leak-freedom proof nor a reliable matched-age
memory non-regression conclusion. Retained-cache bounds and isolated peak RSS
are established separately by the fixture tests, not by these native snapshots.

Every campaign app was quit through native Cmd-Q. Read-only `ps` verified each
app and its four helpers absent: baseline 23461, 27820, 28813, 30534, 33909;
integrated 25708, 28356, 29235, 33072. The last listener on 9210 was gone.
No unrelated app was stopped. Runtime tokens expired with their owning apps.

The new probe passes `py_compile` and `--help`; missing token exits 1 without
leaking details, and `--samples 101` exits 2 before connecting. Its real positive
path is covered by the 520 measurements above. Production Rust/frontend source
and exact bundle hashes did not change, so the earlier full integrated suites
remain tied to the same implementation; this follow-up does not claim new full
suite execution.

### Current handoff boundary

Read-only GitHub recheck after the campaign still reports PR #128 OPEN,
non-draft, CONFLICTING/DIRTY at `f906bff`, with the September 8 failed check;
main remains `69e2288b`. The local candidate contains both refs as ancestors and
can be pushed by fast-forward, without force. Main's tracked dirty diff hash
remained unchanged. **No push, remote PR update, merge, real-model/approval
execution, signing or release was performed.** Separate push authorization is
needed to obtain fresh remote CI; the native limitations and real-provider /
release gates must remain visible even if that CI passes.
