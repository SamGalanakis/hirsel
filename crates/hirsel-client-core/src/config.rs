use thiserror::Error;

/// Reconnect timing expressed as primitive values for a future UniFFI record.
#[derive(Debug, Clone, PartialEq)]
pub struct ReconnectPolicy {
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    /// Symmetric randomization around the exponential delay, from `0.0` to `1.0`.
    pub jitter_ratio: f64,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial_delay_ms: 1_000,
            max_delay_ms: 30_000,
            jitter_ratio: 0.2,
        }
    }
}

impl ReconnectPolicy {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.initial_delay_ms == 0 {
            return Err(ConfigError::ZeroInitialDelay);
        }
        if self.max_delay_ms < self.initial_delay_ms {
            return Err(ConfigError::InvalidMaximumDelay);
        }
        if !(0.0..=1.0).contains(&self.jitter_ratio) || !self.jitter_ratio.is_finite() {
            return Err(ConfigError::InvalidJitterRatio);
        }
        Ok(())
    }

    pub(crate) fn delay_ms(&self, attempt: u32) -> u64 {
        let exponent = attempt.min(63);
        let base = self
            .initial_delay_ms
            .saturating_mul(2_u64.saturating_pow(exponent))
            .min(self.max_delay_ms);
        if self.jitter_ratio == 0.0 {
            return base;
        }

        let low = 1.0 - self.jitter_ratio;
        let high = 1.0 + self.jitter_ratio;
        let factor = rand::random::<f64>() * (high - low) + low;
        (((base as f64) * factor).round().max(1.0) as u64).min(self.max_delay_ms)
    }
}

/// Owned connection settings suitable for conversion from a future UniFFI record.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientConfig {
    /// Host and optional port, or a complete `ws://` / `wss://` base URL.
    pub host: String,
    pub token: String,
    pub reconnect: ReconnectPolicy,
}

impl ClientConfig {
    pub fn new(host: String, token: String) -> Self {
        Self {
            host,
            token,
            reconnect: ReconnectPolicy::default(),
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.host.trim().is_empty() {
            return Err(ConfigError::EmptyHost);
        }
        self.reconnect.validate()
    }

    pub(crate) fn websocket_url(&self) -> String {
        let host = self.host.trim().trim_end_matches('/');
        let base = if host.starts_with("ws://") || host.starts_with("wss://") {
            host.to_owned()
        } else {
            format!("ws://{host}")
        };
        if base.ends_with("/ws") {
            base
        } else {
            format!("{base}/ws")
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ConfigError {
    #[error("host must not be empty")]
    EmptyHost,
    #[error("initial reconnect delay must be greater than zero")]
    ZeroInitialDelay,
    #[error("maximum reconnect delay must be at least the initial delay")]
    InvalidMaximumDelay,
    #[error("jitter ratio must be finite and between 0.0 and 1.0")]
    InvalidJitterRatio,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_canonical_websocket_url() {
        assert_eq!(
            ClientConfig::new("localhost:3089".into(), "token".into()).websocket_url(),
            "ws://localhost:3089/ws"
        );
        assert_eq!(
            ClientConfig::new("wss://example.test/ws".into(), "token".into()).websocket_url(),
            "wss://example.test/ws"
        );
    }

    #[test]
    fn backoff_doubles_and_caps_without_jitter() {
        let policy = ReconnectPolicy {
            initial_delay_ms: 10,
            max_delay_ms: 25,
            jitter_ratio: 0.0,
        };
        assert_eq!(policy.delay_ms(0), 10);
        assert_eq!(policy.delay_ms(1), 20);
        assert_eq!(policy.delay_ms(2), 25);
        assert_eq!(policy.delay_ms(40), 25);
    }

    #[test]
    fn jitter_never_exceeds_configured_bounds() {
        let policy = ReconnectPolicy {
            initial_delay_ms: 100,
            max_delay_ms: 250,
            jitter_ratio: 0.2,
        };
        for _ in 0..100 {
            assert!((80..=120).contains(&policy.delay_ms(0)));
            assert!(policy.delay_ms(20) <= 250);
        }
    }
}
