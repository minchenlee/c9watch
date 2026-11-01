//! Gateway public-key pairing and reconnect proofs (D15). Pure logic only;
//! the `/c9p` listener that sends these frames is not wired here.
//!
//! Every wire byte string is lowercase hex of an exact length: nonces and
//! keys are 32 bytes, signatures 64. Anything else fails closed. A signed
//! message is `label || 0x00 || serverNonce || clientNonce || fields`, where
//! each string field is one length byte and its UTF-8 bytes. The four labels
//! differ, so no signature can be replayed as another step's proof.
//! No step uses a shared secret, so core stores only client public keys.
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand::RngCore;
use serde::Deserialize;

pub const NONCE_LEN: usize = 32;
pub const KEY_LEN: usize = 32;
pub const SIG_LEN: usize = 64;

const PAIR: &str = "c9p-gateway pair v1";
const PAIRED: &str = "c9p-gateway paired v1";
const AUTH_CLIENT: &str = "c9p-gateway auth-client v1";
const AUTH_SERVER: &str = "c9p-gateway auth-server v1";

/// Why a handshake frame failed. The listener maps every variant to close
/// code 4400 (malformed) or 4401 (rejected); none is retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    /// A field is not lowercase hex of the exact length, or a string field
    /// is not in its fixed format.
    Malformed,
    /// A key is a small-order point.
    WeakKey,
    /// The signature does not verify under the expected key.
    BadSignature,
}

/// `{pair:{code,publicKey,clientNonce,sig}}`, sent after the challenge.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairRequest {
    pub code: String,
    pub public_key: String,
    pub client_nonce: String,
    pub sig: String,
}

/// `{auth:{clientId,clientNonce,sig}}`, sent after the challenge.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthRequest {
    pub client_id: String,
    pub client_nonce: String,
    pub sig: String,
}

/// Decode exactly `N` bytes of lowercase hex. Uppercase, odd length, a
/// wrong length or any other character is `None`.
pub fn parse_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.as_bytes();
    if s.len() != N * 2 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for (i, pair) in s.chunks_exact(2).enumerate() {
        out[i] = digit(pair[0])? << 4 | digit(pair[1])?;
    }
    Some(out)
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A fresh server nonce for `{challenge}`.
pub fn new_nonce() -> [u8; NONCE_LEN] {
    let mut n = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut n);
    n
}

/// A client public key: lowercase hex, a valid point, not small-order.
pub fn parse_key(s: &str) -> Result<VerifyingKey, AuthError> {
    key_from_bytes(&parse_hex::<KEY_LEN>(s).ok_or(AuthError::Malformed)?)
}

/// A client public key from raw bytes: a valid point, not small-order.
pub fn key_from_bytes(bytes: &[u8; KEY_LEN]) -> Result<VerifyingKey, AuthError> {
    let key = VerifyingKey::from_bytes(bytes).map_err(|_| AuthError::Malformed)?;
    if key.is_weak() {
        return Err(AuthError::WeakKey);
    }
    Ok(key)
}

/// The machine id is the machine key in lowercase hex (`machine.rs`).
fn valid_machine_id(s: &str) -> bool {
    parse_hex::<KEY_LEN>(s).is_some()
}

/// `cli_` plus a 26-character lowercase Crockford ULID (`tokens.rs`).
pub fn valid_client_id(s: &str) -> bool {
    s.len() == 30
        && s.strip_prefix("cli_").is_some_and(|body| {
            body.bytes()
                .all(|c| b"0123456789abcdefghjkmnpqrstvwxyz".contains(&c))
        })
}

fn message(label: &str, server: &[u8; 32], client: &[u8; 32], fields: &[&[u8]]) -> Vec<u8> {
    let mut m = Vec::with_capacity(label.len() + 1 + 64 + 128);
    m.extend_from_slice(label.as_bytes());
    m.push(0);
    m.extend_from_slice(server);
    m.extend_from_slice(client);
    for f in fields {
        m.push(u8::try_from(f.len()).expect("field fits one length byte"));
        m.extend_from_slice(f);
    }
    m
}

/// What the client signs in `pair`: proof that it holds the key it registers.
pub fn pair_message(server: &[u8; 32], client: &[u8; 32], client_key: &[u8; 32]) -> Vec<u8> {
    message(PAIR, server, client, &[client_key])
}

/// What core signs in the `paired` reply.
pub fn paired_message(
    server: &[u8; 32],
    client: &[u8; 32],
    client_key: &[u8; 32],
    machine_id: &str,
    client_id: &str,
) -> Vec<u8> {
    message(
        PAIRED,
        server,
        client,
        &[client_key, machine_id.as_bytes(), client_id.as_bytes()],
    )
}

/// What the client signs in `auth`. `machine_id` is the audience.
pub fn auth_client_message(
    server: &[u8; 32],
    client: &[u8; 32],
    machine_id: &str,
    client_id: &str,
) -> Vec<u8> {
    message(
        AUTH_CLIENT,
        server,
        client,
        &[machine_id.as_bytes(), client_id.as_bytes()],
    )
}

/// What core signs in the `{sig}` reply to `auth`.
pub fn auth_server_message(
    server: &[u8; 32],
    client: &[u8; 32],
    machine_id: &str,
    client_id: &str,
) -> Vec<u8> {
    message(
        AUTH_SERVER,
        server,
        client,
        &[machine_id.as_bytes(), client_id.as_bytes()],
    )
}

fn verify(key: &VerifyingKey, msg: &[u8], sig: &str) -> Result<(), AuthError> {
    let sig = Signature::from_bytes(&parse_hex::<SIG_LEN>(sig).ok_or(AuthError::Malformed)?);
    key.verify_strict(msg, &sig)
        .map_err(|_| AuthError::BadSignature)
}

/// Check a `pair` frame's proof of possession. Returns the client key to
/// store and the client nonce. The pairing code is checked separately.
pub fn verify_pair(
    server_nonce: &[u8; 32],
    req: &PairRequest,
) -> Result<(VerifyingKey, [u8; 32]), AuthError> {
    let key = parse_key(&req.public_key)?;
    let client = parse_hex::<NONCE_LEN>(&req.client_nonce).ok_or(AuthError::Malformed)?;
    verify(
        &key,
        &pair_message(server_nonce, &client, key.as_bytes()),
        &req.sig,
    )?;
    Ok((key, client))
}

/// Check an `auth` frame against the client's stored key. Returns the client
/// nonce for the server's reply. Audience, origin and expiry are the store's.
pub fn verify_auth(
    server_nonce: &[u8; 32],
    machine_id: &str,
    key: &VerifyingKey,
    req: &AuthRequest,
) -> Result<[u8; 32], AuthError> {
    if !valid_machine_id(machine_id) || !valid_client_id(&req.client_id) {
        return Err(AuthError::Malformed);
    }
    let client = parse_hex::<NONCE_LEN>(&req.client_nonce).ok_or(AuthError::Malformed)?;
    verify(
        key,
        &auth_client_message(server_nonce, &client, machine_id, &req.client_id),
        &req.sig,
    )?;
    Ok(client)
}

/// Core's `paired.sig`, in lowercase hex.
pub fn sign_paired(
    machine: &SigningKey,
    server: &[u8; 32],
    client: &[u8; 32],
    client_key: &[u8; 32],
    machine_id: &str,
    client_id: &str,
) -> String {
    to_hex(
        &machine
            .sign(&paired_message(
                server, client, client_key, machine_id, client_id,
            ))
            .to_bytes(),
    )
}

/// Core's `{sig}` reply to `auth`, in lowercase hex.
pub fn sign_auth(
    machine: &SigningKey,
    server: &[u8; 32],
    client: &[u8; 32],
    machine_id: &str,
    client_id: &str,
) -> String {
    to_hex(
        &machine
            .sign(&auth_server_message(server, client, machine_id, client_id))
            .to_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT_ID: &str = "cli_01jbz3k6x8q2m9v4r7t5w0y1zc";

    fn keys() -> (SigningKey, SigningKey) {
        (
            SigningKey::from_bytes(&[7; 32]),
            SigningKey::from_bytes(&[9; 32]),
        )
    }
    fn sign_hex(k: &SigningKey, m: &[u8]) -> String {
        to_hex(&k.sign(m).to_bytes())
    }

    /// The same bytes are asserted by the SDK and GUI tests
    /// (`packages/c9p-client/test/transports.test.mjs`,
    /// `scripts/test-data-client.mjs`), so the encodings cannot drift.
    #[test]
    fn cross_language_vectors() {
        let (machine, client) = keys();
        let mid = to_hex(machine.verifying_key().as_bytes());
        let (s, c) = ([0xb2; 32], [0xc3; 32]);
        assert_eq!(
            to_hex(&auth_client_message(&s, &c, &mid, CLIENT_ID)),
            format!(
                "{}00{}{}40{}1e{}",
                to_hex(AUTH_CLIENT.as_bytes()),
                "b2".repeat(32),
                "c3".repeat(32),
                to_hex(mid.as_bytes()),
                to_hex(CLIENT_ID.as_bytes())
            )
        );
        assert_eq!(
            mid,
            "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"
        );
        assert_eq!(
            to_hex(client.verifying_key().as_bytes()),
            "fd1724385aa0c75b64fb78cd602fa1d991fdebf76b13c58ed702eac835e9f618"
        );
        assert_eq!(
            sign_hex(&client, &auth_client_message(&s, &c, &mid, CLIENT_ID)),
            AUTH_CLIENT_SIG
        );
        assert_eq!(
            sign_auth(&machine, &s, &c, &mid, CLIENT_ID),
            AUTH_SERVER_SIG
        );
    }
    const AUTH_CLIENT_SIG: &str = "c8434983a6da58624add2bdc334f4f3f1f306c5a4177da50e8d1df4352e033d587bb5d3a26027cc0690fe4bcdf66276bc41a9c0474111fd6f7f168424a73e901";
    const AUTH_SERVER_SIG: &str = "d7558ccadd82cef22380a242aaa3f97b882f9deff62fbc47d0ff75eafa5fe321fc83dd5fd6b348980d39afb32447f104426e5f2926542f002a30cae58ecc5b0d";

    #[test]
    fn pair_and_auth_round_trip() {
        let (machine, client) = keys();
        let mid = to_hex(machine.verifying_key().as_bytes());
        let server = new_nonce();
        let cn = [5u8; 32];
        let ck = *client.verifying_key().as_bytes();
        let pair = PairRequest {
            code: "ABCDEFGH".into(),
            public_key: to_hex(&ck),
            client_nonce: to_hex(&cn),
            sig: sign_hex(&client, &pair_message(&server, &cn, &ck)),
        };
        let (key, got) = verify_pair(&server, &pair).unwrap();
        assert_eq!((key, got), (client.verifying_key(), cn));
        let reply = sign_paired(&machine, &server, &cn, &ck, &mid, CLIENT_ID);
        assert!(verify(
            &machine.verifying_key(),
            &paired_message(&server, &cn, &ck, &mid, CLIENT_ID),
            &reply
        )
        .is_ok());

        let server = new_nonce();
        let auth = AuthRequest {
            client_id: CLIENT_ID.into(),
            client_nonce: to_hex(&cn),
            sig: sign_hex(&client, &auth_client_message(&server, &cn, &mid, CLIENT_ID)),
        };
        assert_eq!(verify_auth(&server, &mid, &key, &auth), Ok(cn));
        // A fresh challenge makes the same proof worthless.
        assert_eq!(
            verify_auth(&new_nonce(), &mid, &key, &auth),
            Err(AuthError::BadSignature)
        );
        // Wrong audience.
        let other = to_hex(&[1; 32]);
        assert_eq!(
            verify_auth(&server, &other, &key, &auth),
            Err(AuthError::BadSignature)
        );
        // A different client's key.
        assert_eq!(
            verify_auth(&server, &mid, &machine.verifying_key(), &auth),
            Err(AuthError::BadSignature)
        );
    }

    #[test]
    fn signatures_do_not_cross_steps() {
        let (machine, client) = keys();
        let mid = to_hex(machine.verifying_key().as_bytes());
        let (s, c) = ([1; 32], [2; 32]);
        // A server proof cannot be replayed as a client proof, even when the
        // client key equals the machine key.
        let server_sig = sign_auth(&machine, &s, &c, &mid, CLIENT_ID);
        let auth = AuthRequest {
            client_id: CLIENT_ID.into(),
            client_nonce: to_hex(&c),
            sig: server_sig,
        };
        assert_eq!(
            verify_auth(&s, &mid, &machine.verifying_key(), &auth),
            Err(AuthError::BadSignature)
        );
        // A pair proof cannot be used to authenticate.
        let ck = *client.verifying_key().as_bytes();
        let auth = AuthRequest {
            sig: sign_hex(&client, &pair_message(&s, &c, &ck)),
            ..auth
        };
        assert_eq!(
            verify_auth(&s, &mid, &client.verifying_key(), &auth),
            Err(AuthError::BadSignature)
        );
    }

    #[test]
    fn malformed_fields_fail_closed() {
        let (machine, client) = keys();
        let mid = to_hex(machine.verifying_key().as_bytes());
        let s = [1; 32];
        // Has letters, so the uppercase case differs.
        let c = [0xabu8; 32];
        let good_sig = sign_hex(&client, &auth_client_message(&s, &c, &mid, CLIENT_ID));
        let key = client.verifying_key();
        let auth = |id: &str, nonce: String, sig: String| AuthRequest {
            client_id: id.into(),
            client_nonce: nonce,
            sig,
        };
        let cases = [
            auth(CLIENT_ID, to_hex(&c).to_uppercase(), good_sig.clone()),
            auth(CLIENT_ID, to_hex(&c)[..62].into(), good_sig.clone()),
            auth(CLIENT_ID, format!("{}00", to_hex(&c)), good_sig.clone()),
            auth(CLIENT_ID, to_hex(&c).replace('a', "g"), good_sig.clone()),
            auth(CLIENT_ID, to_hex(&c), good_sig.to_uppercase()),
            auth(CLIENT_ID, to_hex(&c), good_sig[..126].into()),
            auth(CLIENT_ID, to_hex(&c), String::new()),
            auth(&CLIENT_ID.to_uppercase(), to_hex(&c), good_sig.clone()),
            auth("cli_short", to_hex(&c), good_sig.clone()),
            auth(
                "550e8400-e29b-41d4-a716-446655440000",
                to_hex(&c),
                good_sig.clone(),
            ),
        ];
        for req in &cases {
            assert_eq!(
                verify_auth(&s, &mid, &key, req),
                Err(AuthError::Malformed),
                "{req:?}"
            );
        }
        assert_eq!(
            verify_auth(&s, &mid.to_uppercase(), &key, &cases[0]),
            Err(AuthError::Malformed)
        );
        assert_eq!(
            verify_auth(&s, &mid, &key, &auth(CLIENT_ID, to_hex(&c), good_sig)),
            Ok(c)
        );
        // Unknown frame fields are refused, not ignored.
        assert!(serde_json::from_str::<AuthRequest>(
            r#"{"clientId":"x","clientNonce":"y","sig":"z","mac":"m"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<PairRequest>(
            r#"{"code":"x","publicKey":"y","clientNonce":"z","sig":"s","token":"t"}"#
        )
        .is_err());
    }

    #[test]
    fn weak_and_invalid_client_keys_are_refused() {
        // The identity point is small-order.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert_eq!(parse_key(&to_hex(&identity)), Err(AuthError::WeakKey));
        assert_eq!(parse_key(&"00".repeat(32)), Err(AuthError::WeakKey));
        assert_eq!(parse_key(&"ab".repeat(31)), Err(AuthError::Malformed));
        // Not on the curve.
        let mut off = [0u8; 32];
        off[0] = 2;
        assert!(parse_key(&to_hex(&off)).is_err());
        let (_, client) = keys();
        assert!(parse_key(&to_hex(client.verifying_key().as_bytes())).is_ok());
        assert!(parse_key(&to_hex(client.verifying_key().as_bytes()).to_uppercase()).is_err());

        let s = [3; 32];
        let pair = PairRequest {
            code: "ABCDEFGH".into(),
            public_key: to_hex(&identity),
            client_nonce: "04".repeat(32),
            sig: "00".repeat(64),
        };
        assert_eq!(verify_pair(&s, &pair), Err(AuthError::WeakKey));
        // A valid key that signed a different challenge.
        let ck = *client.verifying_key().as_bytes();
        let pair = PairRequest {
            public_key: to_hex(&ck),
            sig: sign_hex(&client, &pair_message(&[4; 32], &[4; 32], &ck)),
            ..pair
        };
        assert_eq!(verify_pair(&s, &pair), Err(AuthError::BadSignature));
    }

    #[test]
    fn parse_hex_is_exact_and_lowercase() {
        assert_eq!(parse_hex::<2>("0aff"), Some([0x0a, 0xff]));
        assert_eq!(parse_hex::<2>("0AFF"), None);
        assert_eq!(parse_hex::<2>("0af"), None);
        assert_eq!(parse_hex::<2>("0aff00"), None);
        assert_eq!(parse_hex::<2>("+aff"), None);
        assert_eq!(parse_hex::<1>("é"), None);
        #[cfg(feature = "engine")]
        assert!(valid_client_id(&crate::gateway::tokens::new_client_id()));
    }
}
