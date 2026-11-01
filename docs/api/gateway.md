> **Preview. Needs M-b.** This is the target `/c9p` contract. The 1.6 core in this checkout does not serve it. The SDK has signed fixture tests; real gateway and browser acceptance must follow integration with M-b.

# WebSocket gateway

The gateway is off by default. An enabled gateway serves `/c9p` for c9p frames and `/ws` for the older mobile protocol. The two protocols are different. `/c9p` uses public-key authentication only. Do not send a legacy bearer-token frame to `/c9p`.

The planned macOS port is 9210. The Linux default is `20000 + (uid % 10000)` and is recorded in core's `gateway.json`. A local GUI may open a lease that closes when the GUI disconnects. Core itself can keep running.

## Pairing and origins

Serve the example page over HTTPS or from localhost. Add its full `scheme://host:port` origin to `allowedOrigins` with native GUI confirmation. `file://` is not supported. A client is bound to the origin where it paired. Adding an origin is a widening write. It does not replace the Host check against DNS rebinding.

A client is its own Ed25519 key pair. Core stores the client's public key, never a secret, so a copy of core's db cannot be used to sign in. There is no bearer token and no HMAC on `/c9p`.

A user gets a one-time code from the GUI. Core receives only its hash through `gateway.pair.register`. The client creates a fresh key pair and redeems the code with the public key. The reply is `{clientId,serverKey,machineId}`. `serverKey` pins core's Ed25519 public key; `machineId` is the audience of every later proof. `clientId` is `cli_` plus a 26-character lowercase Crockford base32 ULID. Keep the private key non-extractable where the platform allows it (WebCrypto: `extractable:false`, stored in IndexedDB). Do not put any of these values in a URL or log. The sample keeps them in memory and forgets them on reload.

Pairing is trust on first use. Compare the returned public key with the core's key through a trusted local path when identity matters. An active attacker during pairing can substitute a key. Challenge-response protects later reconnects to the pinned key. It does not encrypt application data.

## Encoding

Every nonce, key and signature on the wire is lowercase hex of an exact length: 32-byte nonces and keys (64 characters) and 64-byte signatures (128 characters). Uppercase, a wrong length or any other character is malformed. Both sides fail closed on a malformed field; there is no fallback. Unknown fields in `pair` and `auth` are malformed.

Each signature covers one message:

```
label || 0x00 || serverNonce || clientNonce || field1 || field2 ...
```

`label` is ASCII. The nonces are their 32 decoded bytes. Each field is one length byte and then its bytes: a key as 32 raw bytes, `machineId` and `clientId` as UTF-8. The labels differ per step, so a signature for one step is never valid for another.

| Step | Signer | Label | Fields |
| --- | --- | --- | --- |
| `pair` | client | `c9p-gateway pair v1` | client public key |
| `paired` | core | `c9p-gateway paired v1` | client public key, machineId, clientId |
| `auth` | client | `c9p-gateway auth-client v1` | machineId, clientId |
| reply to `auth` | core | `c9p-gateway auth-server v1` | machineId, clientId |

Core verifies with strict Ed25519 verification and refuses small-order public keys. The reference implementation and fixed test vectors are in `src-tauri/src/gateway/pubkey.rs`; the SDK and GUI tests assert the same bytes.

## Pairing handshake

1. Server sends `{challenge:serverNonce}`.
2. Client creates a key pair and a fresh 32-byte random nonce. It sends `{pair:{code,publicKey,clientNonce,sig}}`. `sig` proves that the client holds the private key.
3. Core checks the code, then the signature. One public key pairs once.
4. Core replies `{paired:{clientId,serverKey,machineId,sig}}`. The client verifies `sig` with the returned `serverKey` and then pins it.

## Reconnect authentication

1. Server sends `{challenge:serverNonce}`.
2. Client makes a fresh 32-byte random nonce. It sends `{auth:{clientId,clientNonce,sig}}`.
3. Core checks the client's audience, origin and expiry, then `sig` with the client's stored public key. A proof for a different machine is rejected.
4. Server replies `{sig}`.
5. Client verifies the signature with its pinned `serverKey`. Only then may it send c9p `hello`.

No secret crosses the wire. A server nonce is used once, so a recorded `auth` frame is worthless. The SDK refuses to send c9p before signature verification. It does not downgrade to `/ws` or bearer authentication. Browser use requires WebCrypto, including Ed25519 support. Node 22 or later supplies WebSocket and WebCrypto.

Key rotation is pairing again with a new key; then revoke the old client. Revocation removes the stored public key.

## Scopes and data ceilings

Each paired client stores explicit `resource:verb` scopes. Read covers query and stream access. A ceiling is `public`, `metadata` or `conversation`. Unknown fields are dropped by gateway redaction. The default ceiling is `metadata`.

| Pairing preset | Scopes | Ceiling |
| --- | --- | --- |
| `viewer` (default) | `session:read cost:read annotation:read` | metadata |
| `viewer+conversation` | viewer plus `conversation:read` | conversation |
| `operator` | viewer+conversation plus `session:stop session:rename` | conversation |

Other read scopes include `agent:read`, `history:read` and `memory:read`. A client needs both scope and an adequate ceiling. Machine policy can disable conversation, history or memory access for every client. Turning policy back on does not grant scopes a client lacks. Widening a client needs a new native confirmation. A client cannot grant itself or another client access.

Gateway clients cannot create or retire agents, send agent mail, search history, recheck detection, change providers, change gateway policy or call hooks in preview. No scope can open those methods. `hello.methods` and `auth.can` explain available calls and missing grants after M-b lands.

Paired clients expire after 30 idle days or 180 absolute days. Expiry removes the whole client; it does not lower its ceiling. Revocation takes effect for live connections. A client must pair again after expiry or revocation.

## Framing and limits

After authentication, each text message carries exactly one c9p object. No newline is needed. The gateway accepts requests up to 64 KiB. Authentication frames are limited to 4 KiB and the handshake must finish within 5 seconds.

| Limit | Planned value |
| --- | --- |
| Connections per client | 4 |
| Request rate | 20/second, burst 40 |
| Streams per connection | 16 |
| Failed pairing attempts | Per IP: 5, then exponential backoff from 1 to 60 seconds; global: 20 per code |

Close codes:

| Code | Meaning and client action |
| --- | --- |
| 4400 | Missing, late or malformed handshake (including non-hex or uppercase nonces); fix the client |
| 4401 | Client, signature, origin, audience or identity rejected; stop automatic reconnect and pair again |
| 4403 | Pairing code rejected; get a new code |
| 1012 | Service restart; reconnect with backoff and resume from the last processed cursor |

A refused method returns a c9p error such as `not_allowed` with missing-access details. Do not retry a refused mutation in a loop. On `cursor_expired`, query a fresh snapshot and replace state.

LAN access uses plain WS in this preview plan. Other LAN participants can read session data in transit. Use a trusted network or a separately secured channel. Challenge-response does not supply encryption. TLS is required before stable v1. Legacy `/ws` additionally transmits its bearer credential and is weaker.
