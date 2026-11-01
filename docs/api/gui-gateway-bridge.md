# GUI bridge and gateway integration (M-c)

The release GUI is the core binary as well (`--features engine`). The GUI
starts it on a background thread through the same spawn lock and private home
as the CLI. It keeps running after the GUI exits. `c9watch engine stop` is the
explicit stop command; restart refuses a newer or unknown engine version.

Classic uses the shared TS data layer. Only `tauri-bridge.ts` and the platform
wrappers import Tauri APIs. Core denial is never converted into a local read;
only `core_unavailable` selects the retained in-process fallback.

The Rust webview method allowlist deliberately excludes all gateway writes,
confirmer attachment, engine lifecycle, agent creation/retirement and mail.
Popover has its own ACL capability and a second Rust method check. Legacy
fallback mutations are not in the popover capability either.

## M-b integration wire choices

M-b is not required for M-c. The GUI binds its own legacy server unless a
native confirmer attachment, `gateway.open {lease:true}`, and a fresh hello
with `gateway` all succeed. The lease connection stays open in Rust. On a
core without gateway methods, none of these methods is sent.

The native GUI and core now share `engine_proto::confirmation` (the engine
re-exports it). Native dialogs and widening detection use `gateway::consent`
on both the legacy fallback and the leased path. There is one signing format
and one native dialog implementation:

- `gui.confirmer.attach {publicKey}`: lowercase hex Ed25519 public key.
- SHA-256 `paramsHash`: lowercase hex of compact JSON of params, before a
  confirmation member is added. Object keys use the serde_json default map order.
- `confirmation: {grantId,method,paramsHash,expiresAt,signature}` inside write
  params, serialized directly from the shared `Grant` type. `signature` is the
  JSON byte array produced by the shared Ed25519 signer over compact JSON claims.
  `expiresAt` is Unix milliseconds, 60 seconds after confirmation. Only Rust
  holds the secret key. The core verifier binds exact params, method, ID and
  expiry and consumes each grant once.
- `gateway.policy.get/set` return/take the policy object directly. Classic's
  four fields are merged over the complete core policy, preserving hidden
  core-only fields.
- Pair registration uses `{codeHash, expiresAt, scopes, ceiling}`; the code
  stays in Rust/Classic and never goes over c9p. Default is the `viewer`
  expanded scopes `session:read cost:read annotation:read`, ceiling `metadata`.
- `gateway.info` returns the existing `ServerInfo` shape.
- The browser's `hexGatewayAuthenticator` takes the client's own Ed25519
  private key (a WebCrypto `CryptoKey`), the pinned server key, `clientId`
  and `machineId`. It signs the `auth` message and verifies the server's
  reply, both in the encoding of [gateway.md](gateway.md#encoding): lowercase
  hex nonces, keys and signatures. The generic transport also accepts a
  reviewed authenticator for other encodings; it never sends hello until
  authentication has completed.

D15 resolved the earlier storage contradiction (hash-only token storage
against an HMAC keyed by the raw token). There is no token: core stores the
client's public key in `gateway_clients` and verifies signatures with it.
Classic never selects the `/c9p` adapter automatically; the integrator must
test both ends before enabling `/c9p` pairing.

M-b currently exposes no gateway attachment/write endpoints. Handover remains
disabled unless all advertised capabilities and lease steps succeed. Public-key
attachment and write-envelope composition still require transport admission
review when those endpoints are implemented; no endpoint or handshake is added
here. A refused attachment/write fails closed. The baseline includes no
native OS dialogs. A narrowing write does not show a dialog. Legacy GUI-owned
pairing and widening policy changes also get the native dialog.

## Native acceptance still required

The headless WebKit visual suite cannot prove WKWebView/Finder TCC identity,
native OS dialog behavior, signing/notarization, or physical phone pairing.
The owner/integrator must check those on the signed release build.
