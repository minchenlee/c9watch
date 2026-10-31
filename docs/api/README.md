> **Preview.** c9p/1 may change before v1 stable. A preview minor can break a method. Read the changelog and check `hello.breakingSince` before calling it.

# c9p/1 public API

c9p connects scripts and clients to the c9watch core. Core detects sessions and serves their state. Clients do not scan provider files.

This guide targets **c9p 1.6**. It describes the API in this checkout. The WebSocket gateway described in [gateway.md](gateway.md) needs M-b. Annotations need the later L1 milestone. Neither is available in the 1.6 golden schema.

Start core with an engine-enabled build:

```sh
cargo build --manifest-path src-tauri/Cargo.toml --no-default-features --features engine
/path/to/c9watch engine
```

`c9watch api <method> '<json>'` and `c9watch export` start core when needed. Third-party socket clients connect to a running core. They do not start one.

The socket is private to your OS user. Find `engine-v1.sock` under:

| System | Production core home |
| --- | --- |
| macOS | `~/Library/Application Support/c9watch/engine/` |
| Linux | `$XDG_RUNTIME_DIR/c9watch/engine-v1.sock` when the runtime dir is set; else `$XDG_STATE_HOME/c9watch/engine/` (or `~/.local/state/c9watch/engine/`) |

Development builds use `engine-dev/`. `C9WATCH_ENGINE_HOME` overrides the whole home with an absolute path. Do not mix development and production sockets. On Linux the default socket lives in the runtime directory; see [Linux](linux.md). A custom home keeps its socket inside the home.

Send one JSON object per line over a Unix stream socket. Send `hello` first:

```json
{"v":1,"id":"1","method":"hello","params":{"apiMajor":1,"apiMinor":6,"client":{"kind":"script","name":"my-board"},"want":[]}}
```

Read its response. Then call `session.views` for a snapshot and subscribe to `session.view` and `session.gone` from the snapshot's cursor. This closes the gap between reading state and starting the stream.

Read next:

- [Protocol](c9p-1.md): framing, discovery, versions, errors and retention.
- [Topics](topics.md): every public event and its data type.
- [Gateway](gateway.md): pairing, origins, scopes and authentication. Needs M-b.
- [Extending](extending.md): exports and external aggregation.
- [Changelog](CHANGELOG.md): preview changes by minor.
- [Linux](linux.md): headless core install, socket paths and the optional systemd service.
- [TypeScript SDK](../../packages/c9p-client/README.md): Node socket and browser/Node WebSocket transports.
- [Python watch](../../examples/python-watch/README.md) and [web board](../../examples/ts-web-board/README.md): runnable clients.

Export the contract offline, including from a CLI-only build:

```sh
c9watch api schema --public > c9p-public.json
```

The checked-in [full schema](c9p-1.json) also contains private methods for internal contract tests. Public export removes internal methods, topics, fields and unused types. It keeps actual wire error names, including the legacy `unsupported_in_c1a` name. That name awaits the separate cleanup minor.

**SQLite is not an interface.** Do not query `engine.db`, its tables or its outbox on disk. They can change in any minor. Use c9p queries, streams or `c9watch export`.

SDK publication is a separate owner decision. This checkout contains a private `0.x` package. It has not been published to npm. Preview wording and gateway documentation still need owner review before announcement.
