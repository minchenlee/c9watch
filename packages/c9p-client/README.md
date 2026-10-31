> **Preview.** c9p/1 may change before v1 stable. This SDK targets minor 6 and refuses methods in `hello.breakingSince`. The package is private and has not been published to npm.

# c9p TypeScript client

Node 22 or later for Unix sockets or WebSocket. Browsers need WebSocket and WebCrypto with Ed25519. Unix transport is exported separately so browser bundles do not import `node:net`.

From the repo root, run `npm ci`, then `npm test --prefix packages/c9p-client`. To develop this package alone, run `npm install` here, then `npm test`. Build output is `dist/`. Generated source types come only from the public schema:

```sh
node scripts/gen-c9p-sdk.mjs
```

Never edit `src/types.ts` by hand. Private methods, topics and selected internal agent fields are omitted. All string enums accept unknown strings. Types describe public wire data, not an authorization grant or runtime input validation.

Node socket example (after building):

```js
import { C9pClient } from './packages/c9p-client/dist/index.js';
import { UnixTransport } from './packages/c9p-client/dist/transport-unix.js';

const client = new C9pClient(() => new UnixTransport('/absolute/core-home/engine-v1.sock'), {
  onDisconnect: (error) => console.error(error.message),
});
await client.connect();
const snapshot = await client.request('session.views', {});
const stream = await client.subscribe({ since: snapshot.cursor, topics: ['session.view', 'session.gone'] },
  async (event) => { /* apply event, then persist its cursor */ },
  (error) => console.error(error.message));
// After a disconnect, keep your state and call await client.reconnect().
// It repeats hello and resumes active streams from completed callback cursors.
await stream.unsubscribe();
client.close();
```

`request` is typed by method name. Subscription filters expand only to public topics advertised for the principal; private and unknown topics never reach callbacks. `subscribe<'session.view'>` gives that topic's typed data; with multiple topics pass their union. Callbacks run in order per subscription. `stream.cursor` advances after the callback succeeds. A callback failure ends that subscription and preserves its prior cursor. Save it externally if the whole process may restart.

`reconnect()` uses a new transport from the factory. It waits for in-flight callbacks, performs hello and resumes active streams. It never retries pending requests or mutations. Call it with your own backoff after `onDisconnect`; do not run concurrent reconnects. `cursor_expired` reaches the subscription error callback and stops that stream. Query a fresh snapshot and create a new subscription after replacing local state. The SDK never guesses a replacement cursor.

Calls time out after 10 seconds by default. At most 1,024 event callbacks wait per subscription. A slow consumer exceeding this bound stops and must resnapshot. Transport frames and outgoing write queues are bounded. A failed or timed-out mutation may have committed; inspect state or use the method's idempotency contract before retrying outside this SDK. The convenience request API currently sends no `idem` key.

Gateway transport (needs M-b):

```js
import { C9pClient } from './packages/c9p-client/dist/index.js';
import { WsTransport, pair } from './packages/c9p-client/dist/transport-ws.js';
const url = 'ws://localhost:9210/c9p';
const credentials = await pair(url, codeFromGui);
const client = new C9pClient(() => new WsTransport(url, credentials));
await client.connect();
```

The pinned server key is verified before hello. Code, token and key encodings are explicit in [gateway.md](../../docs/api/gateway.md); they still need M-b alignment. Pairing is trust on first use. `/ws` legacy bearer auth is not supported. Use an allowed HTTP(S) origin; `file://` is unsupported. Node callers can provide the `webSocket` factory option when they need a WebSocket implementation that sends the paired Origin header. No credentials enter a URL. LAN WS remains unencrypted under the preview plan.

The SDK does not start or stop core. See [API overview](../../docs/api/README.md) for socket locations and limits. npm publication and preview wording approval belong to the owner.
