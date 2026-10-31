//! Transport limits shared by GUI and core.
use std::time::Duration;
pub const WS_PORT: u16 = 9210;
pub const MAX_UNAUTHENTICATED: usize = 16;
pub const MAX_AUTH_FRAME_BYTES: usize = 4 * 1024;
pub const MAX_WS_MESSAGE_BYTES: usize = 64 * 1024;
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
pub const CLOSE_BAD_HANDSHAKE: u16 = 4400;
pub const CLOSE_UNAUTHORIZED: u16 = 4401;
pub const CLOSE_PAIR_REJECTED: u16 = 4403;
pub const CLOSE_POLICY_CHANGED: u16 = 1012;

/// Deterministic token bucket. One instance belongs to one principal, across
/// its sockets; callers hold the same mutex while checking and consuming.
pub struct Bucket {
    tokens: f64,
    at: std::time::Instant,
    rate: f64,
    burst: f64,
}
impl Bucket {
    pub fn requests(now: std::time::Instant) -> Self {
        Self {
            tokens: 40.0,
            at: now,
            rate: 20.0,
            burst: 40.0,
        }
    }
    pub fn annotations(now: std::time::Instant) -> Self {
        Self {
            tokens: 20.0,
            at: now,
            rate: 10.0,
            burst: 20.0,
        }
    }
    pub fn take(&mut self, now: std::time::Instant) -> bool {
        self.tokens = (self.tokens
            + now.saturating_duration_since(self.at).as_secs_f64() * self.rate)
            .min(self.burst);
        self.at = self.at.max(now);
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forty_first_request_in_a_burst_is_refused_and_refills_at_twenty_per_second() {
        let now = std::time::Instant::now();
        let mut b = Bucket::requests(now);
        for _ in 0..40 {
            assert!(b.take(now));
        }
        assert!(!b.take(now));
        assert!(!b.take(now + Duration::from_millis(49)));
        assert!(b.take(now + Duration::from_millis(50)));
        assert!(!b.take(now + Duration::from_millis(50)));
    }
}

/// Four authenticated connections per token. Share one instance across sockets.
pub struct ClientLimits {
    slots: std::sync::Arc<tokio::sync::Semaphore>,
    requests: std::sync::Mutex<Bucket>,
    annotations: std::sync::Mutex<Bucket>,
}
impl ClientLimits {
    pub fn new(now: std::time::Instant) -> Self {
        Self {
            slots: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
            requests: std::sync::Mutex::new(Bucket::requests(now)),
            annotations: std::sync::Mutex::new(Bucket::annotations(now)),
        }
    }
    pub fn enter(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.slots.clone().try_acquire_owned().ok()
    }
    pub fn request(&self, now: std::time::Instant) -> bool {
        self.requests
            .lock()
            .map(|mut b| b.take(now))
            .unwrap_or(false)
    }
    pub fn annotation(&self, now: std::time::Instant) -> bool {
        self.annotations
            .lock()
            .map(|mut b| b.take(now))
            .unwrap_or(false)
    }
}
#[cfg(test)]
mod client_tests {
    use super::*;
    #[test]
    fn connections_and_buckets_are_shared_by_all_sockets_of_one_client() {
        let now = std::time::Instant::now();
        let limits = ClientLimits::new(now);
        let mut permits = Vec::new();
        for _ in 0..4 {
            permits.push(limits.enter().unwrap());
        }
        assert!(limits.enter().is_none());
        permits.pop();
        assert!(limits.enter().is_some());
        for _ in 0..40 {
            assert!(limits.request(now));
        }
        assert!(!limits.request(now));
        for _ in 0..20 {
            assert!(limits.annotation(now));
        }
        assert!(!limits.annotation(now));
    }
}
