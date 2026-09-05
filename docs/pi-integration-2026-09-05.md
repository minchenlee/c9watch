# Pi integration and performance review — 2026-09-05

PR #121 is integrated on main after #120, #123 and #124. Pi remains read-only, with provider-qualified identities across Monitor, History, Cost, search and conversation views. Existing Codex buffer limits, response-item parsing and stable conversation selection are preserved.

## Changes made during integration

- Restored Pi CLI namespace lookup, detector fields and owner fixtures that the old stacked base assumed already existed.
- Monitor checks the four-hour maximum freshness window before parsing files, prunes deleted/expired entries, and retains at most 128 summary entries. Time-dependent lifecycle is still recomputed on cache hits.
- Summary cache validation uses the shared file version (size, modification time, change time and inode), with up to three retries if a file changes during parsing. Platforms without strong metadata take the conservative parse path.
- Header lookup stops after finding the cwd; summary, conversation, search and Cost scans stream lines instead of retaining whole transcript strings.
- First-prompt/latest-snippet summary text is capped at 400 characters. Full conversations and deep search still use the original text.
- Conversation parsing omits hidden tool rows before constructing their display text, and no longer clones assistant content arrays. Thinking and ordinary messages remain visible.

## Validation

All-feature Rust suite: 498 passed, 3 ignored; CLI-only suite: 386 passed, 3 ignored; parser integration: 3 passed; background integration: 1 ignored. Existing PM fixture tests require filesystem access outside the workspace and passed when rerun with that access. Frontend checks reported zero errors/warnings; production build and WebSocket/conversation-selection regressions passed. New cases cover cache expiry/deletion, preserved-mtime rewrites, cache/text bounds, header-only reads, hidden tools and invalid UTF-8.

## Scoped measurement

The local Pi directory contained 52 files totaling 52.57 MiB, with a largest file of 7.85 MiB (metadata only).

A synthetic probe creates 12 expired files totaling 101,450,112 bytes. Replaying the original parse-before-freshness order using the **new streaming parser** took 404.45 ms; the new early freshness gate took 0.193 ms and parsed zero files. This is a debug-mode, single-process measurement of expired-file polling, not an old-binary comparison or a whole-app speedup.

Reproduce with synthetic files only:

```sh
cargo test --locked --manifest-path src-tauri/Cargo.toml \
  --no-default-features --features cli expired_archive_polling_probe \
  -- --ignored --nocapture
```

Active files still reparse when they change. History and Cost remain on-demand streaming scans; this change does not introduce a persistent Pi archive index. Memory scales with the largest JSONL record and requested conversation output, not solely with the summary-cache budget. Native Pi UI and release signing/notarization have not been revalidated in this integration pass. Separate Claude polling/Cost worker changes and Widget work are excluded.
