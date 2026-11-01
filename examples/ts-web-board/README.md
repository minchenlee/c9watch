> **Preview. Needs M-b.** The core `/c9p` gateway is being built separately. This example is coded to the plan's challenge-response contract. Pairing-frame and byte encodings must be aligned with M-b before real use.

# Web session board

This static page pairs with a one-time code, verifies the pinned server signature, reads `session.views`, and applies `session.view` and `session.gone`. Session text enters the DOM through `textContent`. Credentials stay in memory and are forgotten on reload.

From the repo root, after `npm ci`:

```sh
npx vite examples/ts-web-board --host 127.0.0.1 --port 5174 --strictPort
```

Open `http://localhost:5174`. Add that full origin to core's `allowedOrigins` using the GUI's native confirmation. Enable the gateway and obtain a fresh pairing code through the GUI. Use `ws://localhost:9210/c9p` on macOS, or the configured gateway port on Linux. `file://` is unsupported. A page served on a remote host must use HTTPS for WebCrypto. Browsers can also block insecure WS from an HTTPS page; use a secured gateway channel where required.

The SDK's public-key pairing profile is documented in [gateway.md](../../docs/api/gateway.md). There is no bearer token and no fallback to bearer auth. Initial pairing is trust on first use; compare the returned key through a trusted local path before relying on it. LAN plain WS does not encrypt session data.

The page displays disconnect or stream errors. Pair again to reconnect with a fresh snapshot. The SDK separately provides cursor resume for clients that retain state. This page deliberately keeps its lifecycle small.

Build a static bundle:

```sh
npx tsc -p examples/ts-web-board/tsconfig.json
npx vite build examples/ts-web-board --outDir dist
```

Serve `examples/ts-web-board/dist/` from the same allowed origin.

Fixture checks compile the shared `board.ts` reducer and run `smoke.mjs` with the real c9p fixture engine over a Unix transport. SDK tests exercise the signed WS handshake independently. These checks do not establish real gateway or browser acceptance; that still needs M-b.
