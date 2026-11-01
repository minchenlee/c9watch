> **Preview. Needs M-b.** This is the target `/c9p` contract. The 1.6 core in this checkout does not serve it. The SDK has signed fixture tests; real gateway and browser acceptance must follow integration with M-b.

# WebSocket gateway

The gateway is off by default. An enabled gateway serves `/c9p` for c9p frames and `/ws` for the older mobile protocol. The two protocols are different. Do not send a legacy bearer-token frame to `/c9p`.

The planned macOS port is 9210. The Linux default is `20000 + (uid % 10000)` and is recorded in core's `gateway.json`. A local GUI may open a lease that closes when the GUI disconnects. Core itself can keep running.

## Pairing and origins

Serve the example page over HTTPS or from localhost. Add its full `scheme://host:port` origin to `allowedOrigins` with native GUI confirmation. `file://` is not supported. A token is bound to the origin where it paired. Adding an origin is a widening write. It does not replace the Host check against DNS rebinding.

A user gets a one-time code from the GUI. Core receives only its hash through `gateway.pair.register`. Redeeming the code yields `{clientId,token,serverKey,machineId}`. `serverKey` pins core's Ed25519 public key; `machineId` is the token audience. `clientId` is `cli_` plus a 26-character lowercase Crockford base32 ULID; treat it as opaque. Store credentials as secrets. Do not put them in a URL or log. The sample keeps them in memory and forgets them on reload.

The plan fixes the signed challenge flow below. It does not fix pairing-frame or byte encodings. The SDK's integration profile uses `{pair:{code}}` after the server challenge, with a direct credentials object as the reply. It uses hex MACs, signatures, keys and client nonces; concatenated strings are UTF-8 and the token's UTF-8 bytes form the HMAC key. **M-b must confirm or align this profile before release.** The plan also combines hash-only token storage with HMAC keyed by the token. M-b must reconcile how the server verifies that MAC; this SDK follows the stated HMAC formula. These details are explicit so a failed integration cannot silently fall back to weaker auth.

Pairing is trust on first use. Compare the returned public key with the core's key through a trusted local path when identity matters. An active attacker during pairing can substitute a key. Challenge-response protects later reconnects to the pinned key. It does not encrypt application data.

## Reconnect authentication

1. Server sends `{challenge:serverNonce}`.
2. Client makes a fresh 32-byte random nonce. It sends `{auth:{clientId,clientNonce,mac}}`, where `mac = HMAC-SHA256(token, serverNonce || clientNonce || machineId)`.
3. Core checks the token audience, origin, expiry and MAC. A token for a different machine is rejected.
4. Server replies `{sig:Ed25519(serverNonce || clientNonce)}`.
5. Client verifies the signature with its pinned `serverKey`. Only then may it send c9p `hello`.

The token never crosses the wire on reconnect. The SDK refuses to send c9p before signature verification. It does not downgrade to `/ws` or bearer authentication. Browser use requires WebCrypto, including Ed25519 support. Node 22 or later supplies WebSocket and WebCrypto.

## Scopes and data ceilings

Each token stores explicit `resource:verb` scopes. Read covers query and stream access. A ceiling is `public`, `metadata` or `conversation`. Unknown fields are dropped by gateway redaction. The default ceiling is `metadata`.

| Pairing preset | Scopes | Ceiling |
| --- | --- | --- |
| `viewer` (default) | `session:read cost:read annotation:read` | metadata |
| `viewer+conversation` | viewer plus `conversation:read` | conversation |
| `operator` | viewer+conversation plus `session:stop session:rename` | conversation |

Other read scopes include `agent:read`, `history:read` and `memory:read`. A token needs both scope and an adequate ceiling. Machine policy can disable conversation, history or memory access for every token. Turning policy back on does not grant scopes a token lacks. Token widening needs a new native confirmation. A client cannot grant itself or another client access.

Gateway clients cannot create or retire agents, send agent mail, search history, recheck detection, change providers, change gateway policy or call hooks in preview. No scope can open those methods. `hello.methods` and `auth.can` explain available calls and missing grants after M-b lands.

Tokens expire after 30 idle days or 180 absolute days. Expiry removes the whole token; it does not lower its ceiling. Revocation takes effect for live connections. A client must pair again after expiry or revocation.

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
| 4400 | Missing, late or malformed handshake; fix the client |
| 4401 | Token, origin, audience or identity rejected; stop automatic reconnect and pair again |
| 4403 | Pairing code rejected; get a new code |
| 1012 | Service restart; reconnect with backoff and resume from the last processed cursor |

A refused method returns a c9p error such as `not_allowed` with missing-access details. Do not retry a refused mutation in a loop. On `cursor_expired`, query a fresh snapshot and replace state.

LAN access uses plain WS in this preview plan. Other LAN participants can read session data in transit. Use a trusted network or a separately secured channel. Challenge-response does not supply encryption. TLS is required before stable v1. Legacy `/ws` additionally transmits its bearer credential and is weaker.
