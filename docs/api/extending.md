> **Preview.** Exports and c9p shapes may change before stable v1. SQLite files are never a public interface.

# Extend c9watch with data

## L0: read and aggregate

Build a dashboard or report outside c9watch. Query `session.views`, `cost.get`, `history.list` and `agent.list`. Subscribe with a cursor for updates. Store the cursor only after processing succeeds. On `cursor_expired`, rebuild from query results.

`session.view` replay keeps latest state, not every transition. A daily time-in-status report needs a live collector that records the transitions it receives. Retention and compaction prevent the event log from serving as an audit archive.

Export data without writing a client:

```sh
c9watch export sessions
c9watch export cost --format csv --since 2026-11-01 --until 2026-11-02 --out daily-cost.csv
c9watch export history --out history.ndjson
c9watch export history --include-conversation --out private-history.ndjson
c9watch export events --since 'machine:epoch:42'
c9watch export annotations
```

The command reads through c9p and starts core when needed. It never reads the database or provider files itself. It requires an engine-enabled build. `api schema` also works in a CLI-only build.

| What | One NDJSON row | Time field |
| --- | --- | --- |
| `sessions` | A `SessionView` from `session.views` | `modified` |
| `cost` | A `SessionCostRecord` from `cost.get.dailyCosts[].sessions[]` | `timestamp` |
| `history` | A `HistoryEntry`, with all pages read | `timestamp` (Unix milliseconds) |
| `events` | A public c9p event frame | `ts` (Unix milliseconds) |
| `annotations` | An annotation from `annotation.list` | Time filters unsupported until a public timestamp contract exists |

Annotations are not in c9p 1.6. That export reports `unsupported` until core advertises `annotation.list`. It does not invent an empty result.

NDJSON contains one JSON object per line. CSV has a header from the union of row keys, in sorted order. Nested arrays and objects are JSON text in quoted cells. Quotes, commas and newlines are escaped. An empty result has no NDJSON rows and an empty CSV header. Cells preserve source text; spreadsheet users should import them as text.

`--since` accepts an RFC 3339 time or a UTC date (`YYYY-MM-DD`). Its time bound is inclusive. `--until` is exclusive. Only events accept a cursor for `--since`. Other exports reject a cursor because current-state queries have no historical cursor semantics. Rows with no valid time are omitted from a requested time range.

Event export captures core's head when it starts. It reads only through that head and exits. Without a cursor, it probes c9p for the oldest retained position. A time filter applies to the retained events; it cannot recover pruned data. If retention moves past the export while it runs, the command fails with `cursor_expired`. It does not silently skip the gap. A stream that fails to reach the captured head also fails. Completed data is collected before anything is written.

History and event exports omit conversation-class fields by default. Add `--include-conversation` to include prompts, messages, titles and tool input. Public schema sensitivity markers drive the filter; unknown fields and private topics are dropped. The flag does not grant access that core has denied. Sessions and cost exports contain their normal c9p shapes, including session names or snippets that those shapes carry.

`--out` creates a new file with mode 0600. It refuses to replace an existing file or follow a symlink. Without `--out`, data goes to stdout. Errors go to stderr and exit nonzero. Export files can contain personal data; choose their destination accordingly.

For simpler scripts, existing commands remain: [`watch`](../cli-events.md) emits its own NDJSON with `schema:1`; `status` and `list` print JSON; `cost` prints cost data. Their output is not a c9p frame.

## L1: annotations (later milestone)

L1 attaches small typed values to a session. The planned methods are `annotation.set`, `annotation.delete` and `annotation.list`, with topics `annotation.changed` and `annotation.removed`. They are absent from 1.6 and from this SDK's generated method types.

A target is `{kind:"session",sessionKey}`. Keys use `<namespace>/<name>`. Values are tagged numbers, strings, booleans, durations or money. An optional TTL expires a value. A client must display an unknown format hint as a plain value. Annotation writes are restricted to the writer's namespace, with authorization and rate limits.

Use discovery and an updated schema before adding L1 support. Do not write annotation rows into SQLite.
