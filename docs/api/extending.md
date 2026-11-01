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

Annotation export requires a core that advertises `annotation.list` (1.9 or later). Older cores report `unsupported`; the CLI does not invent an empty result. String annotation values are omitted unless `--include-conversation` is supplied.

NDJSON contains one JSON object per line. CSV has a header from the union of row keys, in sorted order. Nested arrays and objects are JSON text in quoted cells. Quotes, commas and newlines are escaped. An empty result has no NDJSON rows and an empty CSV header. Cells preserve source text; spreadsheet users should import them as text.

`--since` accepts an RFC 3339 time or a UTC date (`YYYY-MM-DD`). Its time bound is inclusive. `--until` is exclusive. Only events accept a cursor for `--since`. Other exports reject a cursor because current-state queries have no historical cursor semantics. Rows with no valid time are omitted from a requested time range.

Event export captures core's head when it starts. It reads only through that head and exits. Without a cursor, it probes c9p for the oldest retained position. A time filter applies to the retained events; it cannot recover pruned data. If retention moves past the export while it runs, the command fails with `cursor_expired`. It does not silently skip the gap. A stream that fails to reach the captured head also fails. Completed data is collected before anything is written.

History and event exports omit conversation-class fields by default. Add `--include-conversation` to include prompts, messages, titles and tool input. Public schema sensitivity markers drive the filter; unknown fields and private topics are dropped. The flag does not grant access that core has denied. Sessions and cost exports contain their normal c9p shapes, including session names or snippets that those shapes carry.

`--out` creates a new file with mode 0600. It refuses to replace an existing file or follow a symlink. Without `--out`, data goes to stdout. Errors go to stderr and exit nonzero. Export files can contain personal data; choose their destination accordingly.

For simpler scripts, existing commands remain: [`watch`](../cli-events.md) emits its own NDJSON with `schema:1`; `status` and `list` print JSON; `cost` prints cost data. Their output is not a c9p frame.

## L1: annotations (preview, c9p 1.9)

Attach small typed values to a live session using its provider-scoped `sessionKey` from `session.views`. Classic never reads or displays annotations. Annotation-aware clients can render the values generically, sorted by namespace/name, and fall back to the plain value for unknown format hints.

```sh
c9watch annotate set 'claudeCode:session-id' ci/tests '{"type":"number","v":42}' --label Tests
c9watch annotate list --session-key 'claudeCode:session-id' --namespace ci
c9watch annotate delete 'claudeCode:session-id' ci/tests
```

These shortcuts print the JSON result object, without the c9p response frame: list prints `{annotations,cursor}`, set prints `{annotation}`, and delete prints `{deleted}`. Use `c9watch api annotation.list '{}'` for the complete protocol response.

These commands require an engine-enabled build. They use c9p and the same admission rules as any other client. SQLite is not a write interface.

### Patches and sequences

`annotation.patch` changes several keys atomically. Omitted keys stay; `null` clears a key. `source` is the namespace. Each entry in `values` is a tagged value, rather than a display row.

```json
{"target":{"kind":"session","sessionKey":"claudeCode:session-id"},"source":"ci","values":{"tests":{"type":"number","v":42},"building":{"type":"bool","v":false},"old":null},"ttlMs":60000,"seq":10}
```

The result is `{applied,target,source,values}`, where `values` contains the resulting source map of full annotation rows. With `seq`, a sequence less than or equal to the last accepted sequence for that source and target succeeds with `applied:false`, changes nothing and emits no event. The last sequence survives key clears, expiry and engine restarts. Without `seq`, the latest write wins, leaving the sequence watermark intact. Sequences are nonnegative signed 64-bit integers; JavaScript clients should stay within their exact integer range.

`annotation.set {target,key,value,label?,unit?,format?,ttlSeconds?}` and `annotation.delete {target,key}` are one-key shortcuts over this store. They return `{annotation}` and `{deleted}`. Set replaces its display fields; patch preserves display fields on existing keys. A missing TTL makes a touched value persistent. A delete of an absent key succeeds with `deleted:false`.

### Namespaces and admission

| Caller | Namespace |
| --- | --- |
| Local process | Explicit caller-chosen source/key namespace; prefixes `agent.`, `client.` and `c9watch` are reserved. Requires mutation-peer verification. |
| Engine agent | `agent.<stamped-id>` only, with the ID in lowercase; source may be omitted on patch. |
| Paired client | `client.<authenticated-id>` only, with the ID in lowercase; requires `annotation:write`. Source may be omitted on patch. |

Engine agent IDs are `agt_` plus a 26-character Crockford base32 ULID in uppercase. Gateway client IDs are `cli_` plus a 26-character ULID in lowercase. Crockford base32 is case-insensitive, so the namespace uses the lowercase form of the ID: agent `agt_01JB8Z…` owns `agent.agt_01jb8z…`. Use that exact form in a key or `source`; another spelling is refused. Both forms fit the 40-byte namespace cap. An ID that does not fit the namespace rules fails closed with `bad_request`. IDs are never silently shortened.

A supplied foreign namespace is refused with `not_allowed`. Local patches require `source`; the engine does not guess a local identity. Two allowed local processes can overwrite the same local namespace: `local-unverified` is not an identity boundary. Namespace and name use `[a-z0-9._-]`, at most 40 and 32 UTF-8 bytes respectively.

`annotation.list` and topics require `annotation:read` for paired clients. The default viewer preset includes this read scope. Granting `annotation:write` widens authority and follows the native confirmation/subset rule. The gateway `/c9p` handshake is currently unavailable; these are the authority and redaction contracts for its eventual activation.

### Values and limits

| Value | Shape | Validation |
| --- | --- | --- |
| Number | `{type:"number",v}` | Finite f64. |
| String | `{type:"string",v}` | Trimmed, control characters removed, truncated to 80 Unicode characters and 256 UTF-8 bytes. |
| Boolean | `{type:"bool",v}` | JSON boolean. |
| Duration | `{type:"duration",ms}` | Nonnegative unsigned 64-bit integer. |
| Money | `{type:"money",amount,currency}` | Decimal string (optional minus sign, no exponent), at most 128 bytes; uppercase ISO 4217 code from the bundled SIX list. |

Labels are trimmed, stripped of control characters and truncated at 32 characters. Units have at most 12 characters and no controls. Formats use a namespace-style identifier of at most 32 bytes; known hints include `integer`, `percent`, `bytes`, `relative-time` and `badge`.

Limits are 32 keys per source per target, 128 keys per target, and 2,000 keys per source across targets. One patch touches 1–32 keys; limits are checked on the resulting map, so a clear and replacement can fit at capacity. Writes share a principal budget of 10/s with a burst of 20, across connections and sources. Count and rate limits return `limit_exceeded`. Invalid shapes and TTLs return `bad_request`; a target without a live session returns `unknown_session`.

Source records (including sequence watermarks with no keys) also have limits: 128 per target and 2,000 per source across targets. At capacity, creating a new record returns `limit_exceeded`; existing sources can still update or clear keys. Unsequenced empty sources are removed after clears/expiry, and absent-key deletes without a sequence create no source record. Accepted sequence watermarks are retained until target cleanup, so clearing/expiry/restart never admits stale sequences.

Patch `ttlMs` is 1 ms through 24 hours, independently assigned to each touched key. Set `ttlSeconds` is 60 seconds through 24 hours. List excludes expired keys immediately; each detection tick sweeps expired rows, even if provider observation fails. Persistent rows are removed 30 days after their target disappears. Return before that deadline cancels cleanup; disappearance times survive restarts.

### Query, watch and privacy

`annotation.list {target?,namespace?}` returns `{annotations,cursor}`. Each row carries `target`, `key`, typed `value`, display fields, stamped `writer`, millisecond `createdAt`/`updatedAt`, nullable `expiresAt`, and `sensitivity`. Subscribe from that cursor with topics `annotation.changed` and `annotation.removed` to avoid losing writes between the query and subscription. On `cursor_expired`, replace the snapshot and resubscribe.

`annotation.changed {target,source,values}` contains the complete resulting map for one source on one target. Apply it by replacing that source's old map; an empty map clears it. Each applied patch emits one change event, including explicit clears. Expiry emits one change event per affected source; target cleanup emits `annotation.removed {target,key,reason:"target_gone"}`. Stale sequences emit no event.

String values are always conversation-class. Numeric, boolean, duration and money values, plus key, label, unit and format, are metadata-class. Below a value's class, gateway projection omits the entire `value` and adds `redacted:true`. Unknown value variants are omitted even at the conversation ceiling. Short labels can carry text to metadata viewers; this is an accepted preview limitation.

Every successful write and refusal leaves one audit row with principal, method, known target ID, validated keys, decision and SHA-256 of admitted params. Values and display text never enter audit columns. Success audit, state and event are atomic. Refusals are logged by the central dispatcher. Public/metadata list reads do not create audit rows.

A minimal Python patch, using the socket path printed by your local engine tooling:

```python
import json, socket
s = socket.socket(socket.AF_UNIX)
s.connect(socket_path)
f = s.makefile("rwb")
def call(method, params):
    f.write((json.dumps({"v": 1, "id": method, "method": method, "params": params}) + "\n").encode())
    f.flush()
    answer = json.loads(f.readline())
    if not answer["ok"]:
        raise RuntimeError(answer["error"])
    return answer["result"]
hello = call("hello", {"apiMajor": 1, "apiMinor": 9})
assert "annotation.patch" in hello["methods"]
call("annotation.patch", {"target": {"kind": "session", "sessionKey": session_key},
                          "source": "ci", "values": {"tests": {"type": "number", "v": 42}}})
```
