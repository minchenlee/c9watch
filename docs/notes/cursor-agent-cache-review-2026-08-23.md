# Cursor Agent Incremental Cache Review

Date: 2026-08-23  
Branch: `feature/cursor-agent-detection`  
Scope: `src-tauri/src/session/cursor.rs`  
Source commits: `c26dff6`, `bd263eb`  
Review note: untracked; not committed.

## Outcome

**Initial verdict: Approve with follow-up.**

The implementation fixes the targeted truncate-and-grow transcript rewrite bug. The independent review found no P0, P1, or P2 correctness issue. The two executable follow-ups, P3-1 and P3-3, were subsequently completed. P3-2 and the large-transcript benchmark remain explicitly deferred.

## Root cause

`summary_for` previously used `stamp.len > cached.offset` as the append test. If a Cursor transcript was truncated and rewritten with a total length greater than the old cached offset, the detector could resume parsing from an invalid byte position while reusing the old summary. This could retain stale messages and lifecycle state or skip new content.

## Implemented change

`CacheEntry` now stores a `prefix_hash` for the parsed byte range `[0, offset)`. Incremental parsing is allowed only when all of the following hold:

1. The file is longer than the cached parsed offset.
2. The current file prefix hash matches the cached prefix hash.

If the prefix does not match, the detector falls back to a full parse. Hash-read failures also take the safe full-parse path.

The change is confined to `src-tauri/src/session/cursor.rs` and does not alter frontend behavior, provider detection semantics, freshness rules, or the vscdb overlay.

### Follow-up completion

Commit `bd263eb` refactors the append path to reuse the verified prefix hasher. The implementation now separates prefix feeding, prefix verification, and suffix feeding so a genuine append hashes only the newly appended bytes after the cached prefix has been verified.

The same follow-up adds `partial_line_is_ignored_until_fully_written`, covering a torn JSONL write: an unterminated line does not advance the offset, and the completed line is later emitted exactly once.

## Regression evidence reported by Pi

The new test is:

`session::cursor::tests::truncated_then_rewritten_longer_matches_full_parse`

It performs the following sequence:

1. Write an initial JSONL transcript and call `detect()` to establish the cache.
2. Truncate and rewrite the same path with different content whose length is greater than the cached offset.
3. Call `detect()` again.
4. Compare the result with a fresh `CursorSessionSource` doing a full parse.

The reviewer also reported running the new test against the baseline implementation and reproducing the stale-message failure before applying the fix.

## Test and verification evidence

The following results were reported by the implementation/review Pi agents and were not independently rerun in this Codex turn:

```text
cargo test --lib session::cursor::tests::truncated_then_rewritten_longer_matches_full_parse
  baseline: FAILED as expected; stale messages remained

cargo test --lib session::cursor
  18 passed, 0 failed

cargo test --lib
  322 passed, 0 failed

cargo fmt --check
  OK

cargo clippy --lib
  2 existing warnings in cursor.rs; no warning reported as introduced by this change
```

After the follow-up commits, the reported verification was:

```text
cargo test --lib session::cursor
  19 passed, 0 failed

cargo test --lib
  323 passed, 0 failed

cargo fmt --check
  OK

cargo clippy --lib
  2 existing warnings; no new warnings reported
```

Before the follow-up commits, the Codex-side read-only inspection observed:

```text
 M src-tauri/src/session/cursor.rs
```

`git diff --check` produced no output at inspection time. No real `~/.cursor` transcript was used.

## Independent review findings

### P3-1 — Changed files hash the prefix twice — resolved

On a genuine append, the implementation hashes the cached prefix to validate the append and then hashes the new prefix again when storing the updated cache entry. This makes the I/O cost proportional to the total transcript size, with overlapping reads, even though JSON parsing remains incremental.

Resolved by `bd263eb`: the verified prefix hasher is reused and only the appended suffix is fed into the resulting hash state. The unchanged-file fast path remains intact.

### P3-2 — Same-length rewrite and coarse mtime

If a rewrite preserves the exact file length and the filesystem metadata stamp also remains unchanged, the existing fast path could still return stale data. This is considered low risk on the current macOS/APFS target and is not introduced by the prefix-hash change.

Suggested follow-up: consider a platform-appropriate file identity or stronger change signal if support for coarse-mtime filesystems becomes important. An inode alone would not cover every in-place rewrite case.

### P3-3 — Missing partial-line regression test — resolved

Resolved by `bd263eb` with `partial_line_is_ignored_until_fully_written`. The test verifies that an unterminated line does not advance the offset and that the completed message appears exactly once.

### Observation — Concurrent write tear

There is a small time-of-check/time-of-use window between metadata capture and parsing. A concurrent truncate-and-rewrite could produce a mixed summary until a later poll repairs it. The reviewer treated this as an existing, self-healing edge case rather than a blocker.

## Decision and follow-ups

The source implementation and the executable review follow-ups are now committed and suitable for manual review. Keep the following remaining items as separate follow-up work:

- [x] Avoid double prefix hashing on append (`bd263eb`).
- [x] Add a partial-line incremental parsing regression test (`bd263eb`).
- [ ] Decide whether same-length rewrite detection needs a platform-specific solution.
- [ ] Benchmark synthetic large transcripts if prefix-hash cost becomes user-visible.

Current working tree status: source changes are committed at `bd263eb`; this review note is the only untracked file. Full workspace build, Tauri integration/e2e tests, real high-frequency writes, and large real-user transcripts remain outside this review's verification scope.
