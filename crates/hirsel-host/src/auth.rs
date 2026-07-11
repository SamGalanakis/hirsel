use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::http::{HeaderMap, header::AUTHORIZATION};
use subtle::ConstantTimeEq;

pub fn owner_token_matches(expected: &str, presented: &str) -> bool {
    !expected.is_empty()
        && !presented.is_empty()
        && bool::from(expected.as_bytes().ct_eq(presented.as_bytes()))
}

pub fn owner_bearer_matches(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .is_some_and(|(scheme, token)| {
            scheme.eq_ignore_ascii_case("bearer") && owner_token_matches(expected, token)
        })
}

#[derive(Clone, Default)]
pub struct AuthThrottle {
    failures: Arc<Mutex<HashMap<String, FailureRecord>>>,
}

#[derive(Clone, Copy)]
struct FailureRecord {
    attempts: u32,
    last_failure: Instant,
}

impl AuthThrottle {
    const RESET_AFTER: Duration = Duration::from_secs(60);
    const BASE_DELAY: Duration = Duration::from_millis(100);
    const MAX_DELAY: Duration = Duration::from_secs(2);

    pub fn record_failure(&self, peer: &str) -> Duration {
        let now = Instant::now();
        let mut failures = self
            .failures
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let record = failures.entry(peer.to_string()).or_insert(FailureRecord {
            attempts: 0,
            last_failure: now,
        });
        if now.duration_since(record.last_failure) >= Self::RESET_AFTER {
            record.attempts = 0;
        }
        record.attempts = record.attempts.saturating_add(1);
        record.last_failure = now;
        Self::BASE_DELAY
            .saturating_mul(1 << record.attempts.saturating_sub(1).min(4))
            .min(Self::MAX_DELAY)
    }

    pub fn record_success(&self, peer: &str) {
        self.failures
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(peer);
    }
}

#[cfg(test)]
mod tests {
    use super::AuthThrottle;

    #[test]
    fn repeated_auth_failures_back_off_per_peer() {
        let throttle = AuthThrottle::default();
        let first = throttle.record_failure("peer-a");
        let second = throttle.record_failure("peer-a");
        assert!(second > first);
        assert_eq!(throttle.record_failure("peer-b"), first);
        throttle.record_success("peer-a");
        assert_eq!(throttle.record_failure("peer-a"), first);
    }
}
