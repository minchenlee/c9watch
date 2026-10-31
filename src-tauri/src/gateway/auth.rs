//! WebSocket auth primitives: per-client tokens and one-time pairing codes.
//!
//! Pure logic only. The handshake that uses these lives in `web_server`.

use rand::Rng;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use subtle::{ConditionallySelectable, ConstantTimeEq};

/// How long a pairing code stays valid after it is created.
pub const PAIRING_TTL: Duration = Duration::from_secs(5 * 60);
/// Wrong codes allowed before the active code is revoked. Counted globally,
/// not per connection, because an attacker can simply reconnect.
pub const PAIRING_MAX_FAILURES: u32 = 5;
/// 8 Crockford base32 characters = 40 bits.
pub const PAIRING_CODE_LEN: usize = 8;

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub type ClientId = String;

/// The principal a WS connection acts as: `client:<id>`. The id is the one
/// the registry returned for a verified token (or just issued for a redeemed
/// pairing code), so it is a fact about the connection, never a claim the
/// client makes. There is no other principal on this gateway: a loopback
/// peer is not trusted more than a LAN peer.
pub fn client_principal(client_id: &str) -> String {
    format!("client:{client_id}")
}

fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Paired clients. Only SHA-256 hashes of tokens are kept in memory.
#[derive(Default)]
pub struct ClientRegistry {
    clients: Vec<(ClientId, [u8; 32])>,
}

impl ClientRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a new client. The plaintext token is returned once and never stored.
    pub fn issue(&mut self) -> (ClientId, String) {
        let bytes: [u8; 32] = rand::thread_rng().gen();
        let token: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
        let client_id = uuid::Uuid::new_v4().to_string();
        self.clients.push((client_id.clone(), hash_token(&token)));
        (client_id, token)
    }

    /// Return the client that owns `token`. Every stored hash is compared,
    /// and the matching index is picked with a branchless constant-time
    /// select, so the loop does the same work whether and where it matches.
    /// The one clone happens after the loop. Only SHA-256 hashes are
    /// compared, so timing cannot reveal the token itself.
    pub fn verify(&self, token: &str) -> Option<ClientId> {
        let candidate = hash_token(token);
        let mut found = u64::MAX;
        for (i, (_, hash)) in self.clients.iter().enumerate() {
            let hit = hash.ct_eq(&candidate) & found.ct_eq(&u64::MAX);
            found = u64::conditional_select(&found, &(i as u64), hit);
        }
        usize::try_from(found)
            .ok()
            .and_then(|i| self.clients.get(i))
            .map(|(id, _)| id.clone())
    }

    pub fn forget_all(&mut self) {
        self.clients.clear();
    }

    pub fn len(&self) -> usize {
        self.clients.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedeemError {
    /// No code is active (never created, already used, or revoked).
    NoActiveCode,
    Expired,
    /// Wrong code; counts toward `PAIRING_MAX_FAILURES`.
    Mismatch,
}

struct ActiveCode {
    code: String,
    expires_at: Instant,
    failures: u32,
}

/// At most one live pairing code. The clock is passed in so tests can control it.
#[derive(Default)]
pub struct Pairing {
    active: Option<ActiveCode>,
}

impl Pairing {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a fresh code. Any previous code is revoked immediately.
    pub fn create(&mut self, now: Instant) -> (String, Instant) {
        let mut rng = rand::thread_rng();
        let code: String = (0..PAIRING_CODE_LEN)
            .map(|_| CROCKFORD[rng.gen_range(0..CROCKFORD.len())] as char)
            .collect();
        let expires_at = now + PAIRING_TTL;
        self.active = Some(ActiveCode {
            code: code.clone(),
            expires_at,
            failures: 0,
        });
        (code, expires_at)
    }

    /// Void the active code, if any.
    pub fn revoke(&mut self) {
        self.active = None;
    }

    /// Consume the active code. Succeeds at most once per code.
    pub fn redeem(&mut self, code: &str, now: Instant) -> Result<(), RedeemError> {
        let active = self.active.as_mut().ok_or(RedeemError::NoActiveCode)?;
        if now >= active.expires_at {
            self.active = None;
            return Err(RedeemError::Expired);
        }
        let given = normalize_code(code);
        // ct_eq on slices of different length is false, without early exit on content.
        if bool::from(given.as_bytes().ct_eq(active.code.as_bytes())) {
            self.active = None;
            return Ok(());
        }
        active.failures += 1;
        if active.failures >= PAIRING_MAX_FAILURES {
            self.active = None;
        }
        Err(RedeemError::Mismatch)
    }
}

/// Crockford decoding rules: case-insensitive, hyphens ignored, I/L read as 1
/// and O read as 0.
fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_token_verifies() {
        let mut reg = ClientRegistry::new();
        let (id, token) = reg.issue();
        assert_eq!(token.len(), 64);
        assert_eq!(reg.verify(&token), Some(id));
    }

    #[test]
    fn principal_is_client_prefix_and_id() {
        let mut reg = ClientRegistry::new();
        let (id, token) = reg.issue();
        let verified = reg.verify(&token).unwrap();
        assert_eq!(client_principal(&verified), format!("client:{id}"));
    }

    #[test]
    fn token_of_different_length_fails() {
        let mut reg = ClientRegistry::new();
        let (_, token) = reg.issue();
        assert_eq!(reg.verify(&token[..63]), None);
        assert_eq!(reg.verify(&format!("{token}0")), None);
        assert_eq!(reg.verify(""), None);
    }

    #[test]
    fn token_differing_in_last_byte_fails() {
        let mut reg = ClientRegistry::new();
        let (_, token) = reg.issue();
        let mut wrong = token[..63].to_string();
        wrong.push(if token.ends_with('0') { '1' } else { '0' });
        assert_eq!(reg.verify(&wrong), None);
    }

    #[test]
    fn verify_picks_the_right_client_and_forget_all_clears() {
        let mut reg = ClientRegistry::new();
        let (id_a, tok_a) = reg.issue();
        let (id_b, tok_b) = reg.issue();
        assert_ne!(id_a, id_b);
        assert_eq!(reg.verify(&tok_a), Some(id_a));
        assert_eq!(reg.verify(&tok_b), Some(id_b));
        reg.forget_all();
        assert!(reg.is_empty());
        assert_eq!(reg.verify(&tok_a), None);
    }

    #[test]
    fn registry_does_not_store_plaintext_token() {
        let mut reg = ClientRegistry::new();
        let (_, token) = reg.issue();
        assert!(reg
            .clients
            .iter()
            .all(|(_, h)| *h != token.as_bytes()[..32]));
        assert_eq!(reg.clients[0].1, hash_token(&token));
    }

    #[test]
    fn code_redeems_once() {
        let mut p = Pairing::new();
        let now = Instant::now();
        let (code, _) = p.create(now);
        assert_eq!(p.redeem(&code, now), Ok(()));
        assert_eq!(p.redeem(&code, now), Err(RedeemError::NoActiveCode));
    }

    #[test]
    fn expired_code_fails() {
        let mut p = Pairing::new();
        let now = Instant::now();
        let (code, expires_at) = p.create(now);
        assert_eq!(expires_at, now + PAIRING_TTL);
        assert_eq!(p.redeem(&code, expires_at), Err(RedeemError::Expired));
        assert_eq!(p.redeem(&code, now), Err(RedeemError::NoActiveCode));
    }

    #[test]
    fn code_just_before_expiry_works() {
        let mut p = Pairing::new();
        let now = Instant::now();
        let (code, expires_at) = p.create(now);
        let almost = expires_at - Duration::from_millis(1);
        assert_eq!(p.redeem(&code, almost), Ok(()));
    }

    #[test]
    fn five_failures_revoke_even_the_right_code() {
        let mut p = Pairing::new();
        let now = Instant::now();
        let (code, _) = p.create(now);
        let wrong = if code == "00000000" {
            "11111111"
        } else {
            "00000000"
        };
        for _ in 0..PAIRING_MAX_FAILURES {
            assert_eq!(p.redeem(wrong, now), Err(RedeemError::Mismatch));
        }
        assert_eq!(p.redeem(&code, now), Err(RedeemError::NoActiveCode));
    }

    #[test]
    fn four_failures_still_allow_the_right_code() {
        let mut p = Pairing::new();
        let now = Instant::now();
        let (code, _) = p.create(now);
        let wrong = if code == "00000000" {
            "11111111"
        } else {
            "00000000"
        };
        for _ in 0..PAIRING_MAX_FAILURES - 1 {
            assert_eq!(p.redeem(wrong, now), Err(RedeemError::Mismatch));
        }
        assert_eq!(p.redeem(&code, now), Ok(()));
    }

    #[test]
    fn new_code_revokes_the_old_one() {
        let mut p = Pairing::new();
        let now = Instant::now();
        let (old, _) = p.create(now);
        let (new, _) = p.create(now);
        if old != new {
            assert_eq!(p.redeem(&old, now), Err(RedeemError::Mismatch));
        }
        assert_eq!(p.redeem(&new, now), Ok(()));
    }

    #[test]
    fn code_uses_crockford_alphabet() {
        let mut p = Pairing::new();
        for _ in 0..200 {
            let (code, _) = p.create(Instant::now());
            assert_eq!(code.len(), PAIRING_CODE_LEN);
            assert!(code.bytes().all(|b| CROCKFORD.contains(&b)), "{code}");
            assert!(!code.contains(['I', 'L', 'O', 'U']), "{code}");
        }
    }

    #[test]
    fn code_match_is_case_insensitive() {
        let mut p = Pairing::new();
        let now = Instant::now();
        let (code, _) = p.create(now);
        assert_eq!(p.redeem(&code.to_ascii_lowercase(), now), Ok(()));
    }

    #[test]
    fn normalize_follows_crockford_decoding() {
        assert_eq!(normalize_code("abcd-efgh"), "ABCDEFGH");
        assert_eq!(normalize_code("oIl0"), "0110");
    }
}

/// A code digest for sending to core. The raw code stays in the GUI process.
pub fn pairing_hash(code: &str) -> [u8; 32] {
    Sha256::digest(normalize_code(code).as_bytes()).into()
}
struct Pending<T> {
    hash: [u8; 32],
    expires_at: Instant,
    value: T,
}
struct Failures {
    count: u32,
    until: Instant,
}
/// The core-side one-time code holds only its digest and the confirmed grant.
/// Per-IP backoff is independent of the global twenty-failure revocation cap.
pub struct HashPairing<T> {
    active: Option<Pending<T>>,
    failures: std::collections::HashMap<std::net::IpAddr, Failures>,
    global: u32,
}
impl<T> Default for HashPairing<T> {
    fn default() -> Self {
        Self {
            active: None,
            failures: Default::default(),
            global: 0,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairError {
    Rejected,
    Backoff,
}
impl<T> HashPairing<T> {
    pub fn register(
        &mut self,
        hash: [u8; 32],
        expires_at: Instant,
        value: T,
        now: Instant,
    ) -> Result<(), PairError> {
        if expires_at <= now || expires_at > now + PAIRING_TTL {
            return Err(PairError::Rejected);
        }
        self.active = Some(Pending {
            hash,
            expires_at,
            value,
        });
        self.failures.clear();
        self.global = 0;
        Ok(())
    }
    pub fn revoke(&mut self) {
        self.active = None;
    }
    pub fn redeem(
        &mut self,
        code: &str,
        ip: std::net::IpAddr,
        now: Instant,
    ) -> Result<T, PairError> {
        let ip = ip.to_canonical();
        if self.failures.get(&ip).is_some_and(|f| f.until > now) {
            return Err(PairError::Backoff);
        }
        let active = self.active.as_ref().ok_or(PairError::Rejected)?;
        if now >= active.expires_at {
            self.active = None;
            return Err(PairError::Rejected);
        }
        if bool::from(pairing_hash(code).ct_eq(&active.hash)) {
            return Ok(self.active.take().expect("checked active code").value);
        }
        self.global += 1;
        // Keep attacker-controlled address storage bounded. Full capacity
        // refuses unknown sources instead of evicting a blocked address.
        if self.failures.len() >= 1024 && !self.failures.contains_key(&ip) {
            return Err(PairError::Backoff);
        }
        let failure = self.failures.entry(ip).or_insert(Failures {
            count: 0,
            until: now,
        });
        failure.count += 1;
        if failure.count >= 5 {
            let secs = (1u64 << (failure.count - 5).min(6)).min(60);
            failure.until = now + Duration::from_secs(secs);
        }
        if self.global >= 20 {
            self.active = None;
        }
        Err(PairError::Rejected)
    }
}

#[cfg(test)]
mod hash_pairing_tests {
    use super::*;
    #[test]
    fn five_bad_codes_from_a_lan_peer_do_not_revoke_the_code() {
        let now = Instant::now();
        let ip = "192.168.1.2".parse().unwrap();
        let mut pairing = HashPairing::default();
        pairing
            .register(pairing_hash("ABCDEFGH"), now + PAIRING_TTL, "viewer", now)
            .unwrap();
        for _ in 0..5 {
            assert_eq!(
                pairing.redeem("WRONG123", ip, now),
                Err(PairError::Rejected)
            );
        }
        assert_eq!(pairing.redeem("ABCDEFGH", ip, now), Err(PairError::Backoff));
        assert_eq!(
            pairing.redeem("ABCDEFGH", "192.168.1.3".parse().unwrap(), now),
            Ok("viewer")
        );
        assert_eq!(
            pairing.redeem("ABCDEFGH", ip, now + Duration::from_secs(1)),
            Err(PairError::Rejected)
        );
    }
    #[test]
    fn global_failures_and_expiry_revoke_the_hashed_code() {
        let now = Instant::now();
        let mut pairing = HashPairing::default();
        pairing
            .register(pairing_hash("ABCDEFGH"), now + PAIRING_TTL, (), now)
            .unwrap();
        for n in 1..=20 {
            assert!(pairing
                .redeem("WRONG123", std::net::IpAddr::from([192, 168, 1, n]), now)
                .is_err());
        }
        assert!(pairing
            .redeem("ABCDEFGH", "127.0.0.1".parse().unwrap(), now)
            .is_err());
        pairing
            .register(pairing_hash("ABCDEFGH"), now + PAIRING_TTL, (), now)
            .unwrap();
        assert!(pairing
            .redeem("ABCDEFGH", "127.0.0.1".parse().unwrap(), now + PAIRING_TTL)
            .is_err());
    }
}
