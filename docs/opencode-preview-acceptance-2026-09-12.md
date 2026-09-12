# OpenCode preview candidate acceptance — 2026-09-12

## Revision and authority

- Candidate: `codex/opencode-merge-ready-20260912`, based on
  `a1fe4c1a98a8e56be7581e3f29790af8a7ca8105` (PR #127's five commits).
  The commit containing this record identifies the reviewed source.
- Fresh remote checks: main `d3fe23e56bcce587030087d342d2aba477ec2f10`;
  [PR #127](https://github.com/minchenlee/c9watch/pull/127) still OPEN,
  non-draft, CLEAN, head `a1fe4c1`. Its last CI check succeeded on September 8,
  [run 34182521115](https://github.com/minchenlee/c9watch/actions/runs/34182521115).
  That CI result does **not** cover this unpushed candidate.
- Sole Maker: `opencode-maker-20260912`, shared task `OPENCODE-PREVIEW-127`.
  Worktree `/private/tmp/c9watch-opencode-ready`. No delegated agents.
- Main checkout and `/private/tmp/c9watch-messaging-ready` were not edited.
  No messaging-worker commits were imported. No push, PR update or merge.

## Review outcome and changes

Reviewed current PR body/comments, all five commits and the diff against current
main, not the historical verification paragraph. Prior providerless fallback
and local-provider polling comments were already fixed in the PR. The remaining
merge blockers addressed here were:

1. Same OpenCode ID in different directories previously shared one local key.
   IDs and parent references now include a URL-encoded directory while retaining
   provider qualification. Detail/children responses are validated against that
   identity; full CLI references can still read beyond recent discovery.
2. Per-page limits alone did not bound a whole conversation or discovery fanout.
   Added aggregate byte/time/page/part/session/directory limits, health checks,
   stricter malformed-message handling and explicit errors rather than truncation.
3. Concurrent retry/refresh/disconnect could leave unnecessary HTTP work or stale
   data. Added a fail-fast conversation/children gate, one-entry short-lived
   connection-scoped cache, cancellation and generation checks. Settings polling
   is completion-driven and ignores stale completions.
4. Synchronous native/WS discovery enrichment and subagent transcript scans could
   occupy Tokio async workers. Added bounded blocking dispatch at those entry
   points; subagent scan admission no longer queues unlimited waiters. Existing
   detector/enrichment/scanner responsibilities and background poll thread remain.

Other provider files in the original PR contain the required enum/default fields
and label exhaustiveness updates, not messaging changes. This candidate changes
no Codex/Claude messaging, widget or unrelated shared snapshot behavior. OpenCode
send, stop, rename, terminal, permission/question and other unspecified controls
remain unavailable; open/stop/rename capability flags are false.

## Automated verification

Environment: Apple M2 / Mac14,2, 16 GiB RAM, macOS 26.1 (25B78),
Rust 1.95.0, Node 26.8.1. Commands run from the candidate worktree unless stated.
Rust runs used these resource-saving profile settings, with unchanged optimization
level (debug/unoptimized):

```sh
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
npm ci --ignore-scripts --no-audit --no-fund
cargo test --manifest-path src-tauri/Cargo.toml --locked --quiet
cargo test --manifest-path src-tauri/Cargo.toml --locked --no-default-features --features cli --quiet
npm run check
npm run build
node --test scripts/test-ws.mjs scripts/test-hidden-transitions.mjs scripts/test-opencode-provider.mjs scripts/test-opencode-connection.mjs scripts/test-integration-settings.mjs
node --conditions=browser --test scripts/test-conversation-selection.mjs
node scripts/test-subagent-polling.mjs
node scripts/test-subscription-polling.mjs
node scripts/test-usage-preferences.mjs
git diff --check
```

Results: dependency install passed; final default Rust **443 unit + 3 integration
passed** (9 unit / 1 integration ignored), CLI **409 unit + 3 integration passed**
(6 unit / 1 integration ignored), doc tests completed; Svelte
**0 errors / 0 warnings**, frontend production build passed; Node **16 + 6 passed**;
all three polling/preference scripts PASS; diff check clean. Explicit fixture
benchmarks and global-service stress are ignored in normal CI and run separately
below. Remaining normal ignored tests are pre-existing environment/slow tests,
not silently counted as passes. Rust emits existing dead-code and CLI `cfg(mobile)`
warnings. Expected fixture 503/stale-error messages in Node output are assertions'
inputs, not failed tests. CI now includes the new connection polling regression.

Failure coverage is executable, not based only on source inspection:

| Contract/failure | Evidence |
| --- | --- |
| Same ID across directories | HTTP fixture → real snapshot → enrichment yields distinct keys, Working vs idle; detail/conversation reads carry the right directory; frontend parent/child grouping stays separate |
| Provider collision / unsupported actions | Rust providerless probing/collision and rename guards; frontend identical-ID OpenCode/Claude isolation and false action capabilities |
| Old-but-busy / retry retention, stale-idle / archived expiry | Original snapshot regression plus new replacement/deletion checks; old root visible natively while expired idle fixture is absent |
| Detail / children / deleted session | Correct scoped parent-child responses; 404, archived detail and wrong-directory detail explicitly rejected |
| Initial error + Retry / stale completion | Actual frontend selection state tests; serial retries, provider switch, close, late errors and reopen covered |
| 401 / 403 / 404 / 500 / 503 and recovery | Loopback health/snapshot errors preserve metadata as Connecting, omit response-body secrets, then restore healthy snapshot |
| Malformed JSON / schema / message | Invalid health JSON, unhealthy health, malformed message fields and non-string text rejected; existing message format tests retained |
| Cursor repetition / unique infinite sequence | A→B→A fails after 3 requests; unique cursors stop at 128 pages; empty/oversized cursor guards in loader |
| Page / total / count limits | Chunked >8 MiB shrinks page size; single >8 MiB rejected; 32 MiB aggregate fails on seventh 5 MiB page; server ignoring requested row count rejected; >10,000 parts rejected; 513 sessions / 33 directories rejected |
| Timeout / concurrent disconnect | Shared 40 ms deadline against 150 ms fixture; cancelled response rejected and no next request; old generation cannot replace newer snapshot |
| Cache / backpressure | 100 same-key hits produce only original detail+page HTTP requests; expiry reloads; cancelled connection cannot hit cache; 16-way public conversation stress below |

## Performance: comparative transport loader

`src-tauri/src/session/opencode_perf.rs` runs the same loader harness on PR head
and candidate: pre-serialized loopback HTTP pages, cold client construction,
ten uncached repeats and full response serialization. Every repeat fetches every
page; this is **not** a warmed-result cache benchmark. Small: 24 messages / 2
pages / 5,321 wire bytes. Large: 1,200 messages / 60 pages / 9,944,553 wire bytes,
9,939,651 serialized output bytes. Chronological first/last entries are asserted.

```sh
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib benchmark_opencode_conversation -- --ignored --nocapture --test-threads=1
```

For baseline reproduction use a separate detached worktree at `a1fe4c1`, copy
the identical `opencode_perf.rs` next to `opencode.rs`, and add only this test
module to the latter (no loader changes):

```rust
#[cfg(test)]
#[path = "opencode_perf.rs"]
mod perf;
```

The actual baseline was `/private/tmp/c9watch-opencode-perf-baseline`, with a
test-only absolute path to that identical candidate harness. Builds shared
dependencies sequentially, never concurrently. Copied test executables were
then measured in order **baseline, candidate, candidate, baseline, baseline,
candidate**, without compilation during measurement:

```sh
for bench in baseline candidate candidate baseline baseline candidate; do
  /usr/bin/time -l "src-tauri/target/opencode-${bench}-bench" benchmark_opencode_conversation --ignored --nocapture --test-threads=1
done
```

All times milliseconds; p95 is the maximum of ten repeat samples, not a claim
about production population percentiles. Peak RSS includes both synthetic HTTP
server and loader, not whole GUI app memory.

| Run | Small cold / repeat median / p95 | Large cold / repeat median / p95 | Peak RSS bytes |
| --- | --- | --- | ---: |
| Baseline 1 | 7.156 / 1.380 / 1.701 | 223.651 / 226.602 / 240.890 | 170344448 |
| Candidate 1 | 4.786 / 1.259 / 1.550 | 371.115 / 242.019 / 258.897 | 170983424 |
| Candidate 2 | 2.001 / 1.028 / 1.508 | 232.826 / 236.084 / 241.614 | 170721280 |
| Baseline 2 | 2.010 / 0.946 / 1.104 | 241.213 / 233.034 / 246.822 | 158892032 |
| Baseline 3 | 1.769 / 1.024 / 1.745 | 223.830 / 223.493 / 234.416 | 170098688 |
| Candidate 3 | 1.690 / 1.001 / 1.174 | 229.329 / 225.833 / 235.442 | 170426368 |

Median of the three large repeat medians: baseline **226.602 ms**, candidate
**236.084 ms** (+4.2%). Maximum RSS differs by 638,976 bytes (+0.38%). Retired
instructions in these runs were approximately 38.59–38.64 billion baseline vs
38.66–38.73 billion candidate. These fixtures show no sustained substantial
loader regression, but do **not** establish a release performance SLA.

Variance is not hidden: earlier baseline/candidate large medians were
229.329/222.093 ms; a later candidate run during other machine activity measured
309.397 ms median / 834.461 ms p95, and the first alternating candidate cold load
was 371.115 ms. A standalone recheck measured 230.886 ms median / 259.288 ms p95,
RSS 170344448. The machine was shared, not a controlled performance lab.
The first sandboxed `/usr/bin/time -l` wrapper returned exit 1 because macOS
`sysctl kern.clockrate` was denied despite a passing benchmark; subsequent
permitted counter runs exited 0 and supplied the RSS figures above.

## Production reader, scheduler and concurrency

```sh
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib benchmark_opencode_reader_cache_and_refresh -- --ignored --nocapture --test-threads=1
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib benchmark_transcript_scheduler -- --ignored --nocapture --test-threads=1
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib concurrent_conversation_entry -- --ignored --nocapture --test-threads=1
```

Production-reader fixture (separate workload from comparator) includes detail
validation, fresh server-side JSON serialization on every request, 1 ms accept
polling, page reads, cache population/copy and output serialization. Small cold
**7.782 ms**, refresh median **3.874 ms**, p95 **6.008 ms**, cache hit median
**0.142 ms**. Large cold **581.132 ms**, refresh median **653.389 ms**, p95
**766.131 ms**, cache hit median **252.292 ms**. Eleven full loads plus eleven
cache hits used **33 / 671 HTTP requests** for small/large; each cache hit made
**zero HTTP requests**. Large cached results still incur copying/serialization;
this is not a claim that caching eliminates CPU cost. Both workloads complete
well within the polling interval; polling is scheduled after completion anyway.

Scheduler fixture writes **46,526,670 bytes / 20,000 synthetic transcript rows**
and runs the real transcript parser and subagent scanner (both counts asserted
20,000) on a single-thread Tokio runtime with a 2 ms heartbeat. Measured inline
control: **1057.182 ms** scan, **1057.284 ms** maximum heartbeat gap (3 ticks).
Bounded blocking dispatch: **994.194 ms** scan, **3.483 ms** maximum heartbeat
gap (297 ticks). This demonstrates that the added dispatch boundary prevents
the same synchronous scan from starving that async worker; it is not a claim
about all background work in the application. A final rerun measured inline
**1230.990 ms / 1231.735 ms** scan/gap and blocking **1205.680 ms / 3.782 ms**
(358 ticks), confirming the scheduler boundary despite slower overall work.
Cancellation/admission regression
also verifies that an aborted async caller does not release its running job's
permit and that the next scan succeeds after completion.

Public conversation entry stress: **16 callers, 1 accepted, 15 immediately
rejected, peak HTTP concurrency 1, reconnect recovery OK**. The fixture joins
all request threads and asserts active requests return to zero. Discovery has
its own dedicated serial thread, so discovery plus conversation can overlap
(native fixture observed peak 2); they do not serialize unrelated providers.

Resource limits are explicit: 8 MiB/page, 32 MiB/conversation, 128 pages,
10,000 rendered parts, 400 DOM messages, one one-second cached conversation;
snapshot 512 sessions / 32 directories / 16 MiB / 10 seconds. Requests time out
within three seconds or the shorter remaining operation deadline. An in-flight
request is not forcibly interrupted at the socket level by disconnect, but is
bounded by that timeout and cannot publish or continue paging afterward.
HTTP response ownership closes/drops body streams on every exit; idle reuse is
limited to one socket per host / five seconds. No persistent SSE connection.

## Native/debug and real-server evidence

Built with `npm run build` followed by:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 npm run tauri build -- --debug --bundles app --config scripts/opencode-qa.conf.json
```

The QA config deliberately skips the duplicate frontend build; run the preceding
`npm run build` first. Launch the exact output, not `/Applications/c9watch.app`:
`src-tauri/target/debug/bundle/macos/c9watch OpenCode Ready QA.app`, bundle ID
`com.minchenlee.c9watch.opencodereadyqa`. Final production-source binary SHA-256:
`1924e61eefe2245d8bce486e0cf01512f10e2db9e04d44a3b21ca9e0cb2ba886`.
Later additions are test-only benchmarks and this record, not production changes.
This bundle is development/ad-hoc signed only; notarization was explicitly skipped.
The local root `target` symlink points to ignored `src-tauri/target` for reused
build metadata; it and benchmark executables/logs are generated artifacts and
are excluded from the candidate commit.

Run `python3 scripts/opencode-preview-fixture.py --port 64647`. In native Settings
→ Integration, connect `http://127.0.0.1:64647`; choose OpenCode filter. Controls:

```sh
curl -fsS -X POST http://127.0.0.1:64647/__fixture -H 'Content-Type: application/json' -d '{"messageError":503}'
curl -fsS -X POST http://127.0.0.1:64647/__fixture -H 'Content-Type: application/json' -d '{"messageError":0,"revision":1,"messages":26}'
curl -fsS -X POST http://127.0.0.1:64647/__fixture -H 'Content-Type: application/json' -d '{"busy":false,"fresh":true}'
curl -fsS -X POST http://127.0.0.1:64647/__fixture -H 'Content-Type: application/json' -d '{"error":503}'
curl -fsS -X POST http://127.0.0.1:64647/__fixture -H 'Content-Type: application/json' -d '{"error":0}'
curl -fsS http://127.0.0.1:64647/__metrics
```

`auth:true` requires fixture-only Basic credentials `opencode:fixture`;
`deleted:true` empties discovery; `delay` sets seconds per request.
`fresh:false,busy:true` exercises old-but-busy retention; setting busy false with
fresh false expires that old root. The expired idle fixture never appears.
Every OpenCode-shaped route is read-only; only `/__fixture` changes test state.

Observed with native Computer Use, not browser simulation:

| Evidence | Observation and provenance |
| --- | --- |
| Integration | Final rebuilt bundle: visible form, aligned right inset, URL, Connecting then AX `Connected · 3 sessions`; settings apply only to this isolated app |
| OpenCode-only / cross-directory status | Final bundle screenshots: OpenCode filter, 2 roots, Working 1 / Waiting 1; Chinese root READY, old English root WORKING; child is grouped under English root |
| English conversation | Final bundle screenshot and AX: 24 chronological messages, real visible English and Chinese glyphs, `/fixture/a`, correct Working header; no send/stop/rename/terminal controls |
| Chinese conversation / live refresh | Earlier same-source UI bundle, binary `6fc6b89c2f7b2493a1de6190d675948898df9c2df0ee9848b8b07410d0ad4e37`: visible `/fixture/中文`, 24→26 messages, new 25/26 entries and revision 1; final build afterward included only a backend scoped legacy-collision guard change |
| Initial HTTP 503 / Retry / recovery | Earlier bundle screenshot: `Could not load conversation`, `OpenCode HTTP 503`, RETRY; Retry clicked while 503 persisted and error returned correctly. After clearing failure, polling recovered. A separate attempted click after auto-recovery had a stale AX index and is not counted as successful Retry evidence |
| Tooling limits | An earlier coordinate click failed `noWindowsAvailable`; native Raise + keyboard filter selection recovered. A later Mac lock stopped interaction. After unlocking became possible, final bundle was relaunched and screenshots above succeeded. Mac locked again before final Ready/live-update, hide/show and disconnect/reconnect steps; those are not screenshot passes |

Images were returned inline by native Computer Use in this task, not exported as
PNG files. AX text is identified separately; HTTP fixture state is not pixel
evidence. In particular, setting `busy:false,fresh:true,revision:1,messages:26`
after the final English screenshot succeeded at the fixture/data layer but the
next screenshot was blocked by the locked Mac. Do not infer its visual result.

Actual installed OpenCode **1.18.20** was started on loopback port 64648 with
fresh isolated XDG config/data/state/cache directories. Verified healthy/version,
empty `/session?limit=513`, directory status `{}`, and `/doc` contracts for
health/list/detail/children/messages (`limit`/`before`/directory). Official
[server API documentation](https://opencode.ai/docs/server/) was also checked.
No user prompts, real model response or paid model call was generated. Current
nonempty conversation and error acceptance relies on the committed HTTP fixture,
not the old document's OpenCode 1.18.29/model-response claim.

## Remaining gates / verdict

**Not yet unconditional merge-ready:** implementation and automated/performance
gates passed, but the Mac lock prevents completing the requested native sequence.
After unlocking, resume only this QA bundle and fixture:

1. Observe final English root Ready and revision 1 / 26 messages; hide/show the
   overlay and verify visible content (automated transition policy already passes).
2. Settings → Integration → DISCONNECT: Not connected; OpenCode-only monitor
   becomes empty. Reconnect same URL: Connecting → Connected, both directories
   and their scoped conversations return without old data.
3. Exercise global 503 and recovery in Settings/monitor, then disconnect cleanly;
   fixture request activity must stop after the bounded in-flight timeout.

No further source blocker was found in this audit. These remaining observations
must be recorded before changing this verdict; a green prior remote CI or current
data-only fixture response is not a substitute. Then request user approval to
push the reviewed candidate to PR #127 and update its evidence, rerun remote CI,
and leave merge to the user. Signed distribution, notarization, release deployment
and unsupported capabilities are explicitly not accepted or promised here.
