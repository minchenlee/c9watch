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

The plan defines the confirmation claims but does not fix their encoding.
This branch uses the following concrete encoding, for integration review:

- `gui.confirmer.attach {publicKey}`: lowercase hex Ed25519 public key.
- SHA-256 `paramsHash`: lowercase hex of compact JSON of params, before a
  confirmation member is added. Object keys are recursively sorted (the
  serde_json default map order).
- `confirmation: {claims: {grantId,method,paramsHash,expiresAt}, signature}`
  inside write params. `signature` is lowercase hex Ed25519 over the compact,
  sorted JSON claims. `expiresAt` is Unix milliseconds, 60 seconds after
  confirmation. Only Rust holds the secret key.
- `gateway.policy.get/set` return/take the policy object directly. Classic's
  four fields are merged over the complete core policy, preserving hidden
  core-only fields.
- Pair registration uses `{codeHash, expiresAt, scopes, ceiling}`; the code
  stays in Rust/Classic and never goes over c9p. Default is the `viewer`
  expanded scopes `session:read cost:read annotation:read`, ceiling `metadata`.
- `gateway.info` returns the existing `ServerInfo` shape.
- The browser's `hexGatewayAuthenticator` uses 32-byte hex server/client
  nonces, a 32-byte hex token as the HMAC key, a hex pinned Ed25519 public
  key and a hex signature. The HMAC input is decoded nonce bytes followed
  by UTF-8 machineId; the signed input is the decoded nonce pair. The
  generic transport also accepts a reviewed authenticator for other
  encodings; it never sends hello until authentication has completed.

M-b's report also records an unresolved protocol/storage contradiction:
a server storing only SHA-256(token) cannot verify HMAC keyed by the raw token.
The browser adapter follows the current plan's raw-token HMAC; Classic never
selects it automatically. The owner must resolve that contract, then the
integrator must align and test both ends before enabling /c9p pairing.

If M-b uses a different encoding, align these adapters before enabling
handover. A refused attachment/write fails closed. The baseline includes no
native OS dialogs. A narrowing write does not show a dialog. Legacy GUI-owned
pairing and widening policy changes also get the native dialog.

## Native acceptance still required

The headless WebKit visual suite cannot prove WKWebView/Finder TCC identity,
native OS dialog behavior, signing/notarization, or physical phone pairing.
The owner/integrator must check those on the signed release build.
