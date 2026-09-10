use std::{
    collections::HashMap,
    fmt,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::http::{HeaderMap, header::AUTHORIZATION};
use subtle::ConstantTimeEq;

pub fn owner_token_matches(expected: &str, presented: &str, debug_enabled: bool) -> bool {
    if debug_enabled {
        return !presented.trim().is_empty();
    }
    !expected.is_empty()
        && !presented.is_empty()
        && bool::from(expected.as_bytes().ct_eq(presented.as_bytes()))
}

pub fn owner_bearer_matches(headers: &HeaderMap, expected: &str, debug_enabled: bool) -> bool {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .is_some_and(|(scheme, token)| {
            scheme.eq_ignore_ascii_case("bearer")
                && owner_token_matches(expected, token, debug_enabled)
        })
}

#[derive(Clone, Default)]
pub struct AuthThrottle {
    failures: Arc<Mutex<HashMap<AuthPeer, FailureRecord>>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum AuthPeer {
    WebSocket(IpAddr),
    Iroh(String),
}

impl fmt::Display for AuthPeer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WebSocket(ip) => write!(formatter, "websocket:{ip}"),
            Self::Iroh(node_id) => write!(formatter, "iroh:{node_id}"),
        }
    }
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

    pub(crate) fn record_failure(&self, peer: &AuthPeer) -> Duration {
        let now = Instant::now();
        let mut failures = self
            .failures
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let record = failures.entry(peer.clone()).or_insert(FailureRecord {
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

    pub(crate) fn record_success(&self, peer: &AuthPeer) {
        self.failures
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(peer);
    }

    #[cfg(test)]
    pub(crate) fn failure_attempts(&self, peer: &AuthPeer) -> Option<u32> {
        self.failures
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(peer)
            .map(|record| record.attempts)
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use axum::http::{HeaderMap, header::AUTHORIZATION};

    use super::{AuthPeer, AuthThrottle, owner_bearer_matches, owner_token_matches};

    #[test]
    fn debug_accepts_any_non_whitespace_owner_token() {
        assert!(owner_token_matches("configured", "anything", true));
        assert!(!owner_token_matches("configured", "", true));
        assert!(!owner_token_matches("configured", " \t", true));

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer browser-token".parse().unwrap());
        assert!(owner_bearer_matches(&headers, "configured", true));
    }

    #[test]
    fn production_requires_the_exact_owner_token() {
        assert!(owner_token_matches("configured", "configured", false));
        assert!(!owner_token_matches("configured", "anything", false));

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer anything".parse().unwrap());
        assert!(!owner_bearer_matches(&headers, "configured", false));
    }

    #[test]
    fn repeated_auth_failures_back_off_per_peer() {
        let throttle = AuthThrottle::default();
        let peer_a = AuthPeer::Iroh("peer-a".to_string());
        let peer_b = AuthPeer::Iroh("peer-b".to_string());
        let first = throttle.record_failure(&peer_a);
        let second = throttle.record_failure(&peer_a);
        assert!(second > first);
        assert_eq!(throttle.record_failure(&peer_b), first);
        throttle.record_success(&peer_a);
        assert_eq!(throttle.record_failure(&peer_a), first);

        let same_text_as_an_ip = AuthPeer::Iroh("127.0.0.1".to_string());
        let websocket_ip = AuthPeer::WebSocket(IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(throttle.record_failure(&same_text_as_an_ip), first);
        assert_eq!(throttle.record_failure(&websocket_ip), first);
    }
}
