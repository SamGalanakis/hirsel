use hirsel_proto::HelloAuth;
use thiserror::Error;

use crate::identity::parse_iroh_identity;

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
    /// Canonical iroh endpoint ticket. When present, the client uses iroh instead of WebSocket.
    pub iroh_ticket: Option<String>,
    /// Persisted iroh secret key. Required whenever `iroh_ticket` is present.
    pub iroh_secret_key: Option<String>,
    pub auth: HelloAuth,
    pub reconnect: ReconnectPolicy,
}

impl ClientConfig {
    pub fn new(host: String, token: String) -> Self {
        Self {
            host,
            iroh_ticket: None,
            iroh_secret_key: None,
            auth: HelloAuth::StaticToken(token),
            reconnect: ReconnectPolicy::default(),
        }
    }

    pub fn new_iroh(ticket: String, device_token: String, iroh_secret_key: String) -> Self {
        Self {
            host: String::new(),
            iroh_ticket: Some(ticket),
            iroh_secret_key: Some(iroh_secret_key),
            auth: HelloAuth::DeviceToken(device_token),
            reconnect: ReconnectPolicy::default(),
        }
    }

    pub fn new_iroh_pairing(
        ticket: String,
        code: String,
        device_label: String,
        iroh_secret_key: String,
    ) -> Self {
        Self {
            host: String::new(),
            iroh_ticket: Some(ticket),
            iroh_secret_key: Some(iroh_secret_key),
            auth: HelloAuth::PairingCode { code, device_label },
            reconnect: ReconnectPolicy::default(),
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        match self.iroh_ticket.as_deref() {
            Some(ticket) if ticket.trim().is_empty() => return Err(ConfigError::EmptyIrohTicket),
            Some(_) => {
                let secret_key = self
                    .iroh_secret_key
                    .as_deref()
                    .ok_or(ConfigError::MissingIrohSecretKey)?;
                parse_iroh_identity(secret_key)?;
            }
            None if self.host.trim().is_empty() => return Err(ConfigError::EmptyHost),
            None => {}
        }
        self.reconnect.validate()
    }

    pub(crate) fn websocket_url(&self) -> String {
        let host = self.host.trim().trim_end_matches('/');
        let base = if let Some(host) = host.strip_prefix("http://") {
            format!("ws://{host}")
        } else if let Some(host) = host.strip_prefix("https://") {
            format!("wss://{host}")
        } else if host.starts_with("ws://") || host.starts_with("wss://") {
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

    pub(crate) fn transport_target(&self) -> TransportTarget {
        self.iroh_ticket.as_ref().map_or_else(
            || TransportTarget::WebSocket(self.websocket_url()),
            |ticket| TransportTarget::Iroh(ticket.trim().to_owned()),
        )
    }

    pub(crate) fn parsed_iroh_secret_key(&self) -> Result<Option<iroh::SecretKey>, ConfigError> {
        self.iroh_secret_key
            .as_deref()
            .map(parse_iroh_identity)
            .transpose()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TransportTarget {
    WebSocket(String),
    Iroh(String),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ConfigError {
    #[error("host must not be empty")]
    EmptyHost,
    #[error("iroh ticket must not be empty")]
    EmptyIrohTicket,
    #[error("iroh secret key is required for iroh transport")]
    MissingIrohSecretKey,
    #[error("iroh secret key is invalid")]
    InvalidIrohSecretKey,
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
        assert_eq!(
            ClientConfig::new("http://10.0.2.2:3090".into(), "token".into()).websocket_url(),
            "ws://10.0.2.2:3090/ws"
        );
        assert_eq!(
            ClientConfig::new("https://example.test".into(), "token".into()).websocket_url(),
            "wss://example.test/ws"
        );
    }

    #[test]
    fn iroh_ticket_selects_iroh_transport_without_a_websocket_host() {
        let identity = crate::generate_iroh_identity();
        let config =
            ClientConfig::new_iroh("endpointticket".into(), "token".into(), identity.clone());
        assert_eq!(
            config.transport_target(),
            TransportTarget::Iroh("endpointticket".into())
        );
        assert_eq!(config.validate(), Ok(()));
        assert_eq!(config.auth, HelloAuth::DeviceToken("token".into()));
        assert_eq!(config.iroh_secret_key, Some(identity));
    }

    #[test]
    fn pairing_constructor_carries_code_and_label() {
        let config = ClientConfig::new_iroh_pairing(
            "endpointticket".into(),
            "pairing-code".into(),
            "Owner phone".into(),
            crate::generate_iroh_identity(),
        );
        assert_eq!(
            config.auth,
            HelloAuth::PairingCode {
                code: "pairing-code".into(),
                device_label: "Owner phone".into(),
            }
        );
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn iroh_transport_requires_a_valid_secret_key() {
        let mut config = ClientConfig::new_iroh(
            "endpointticket".into(),
            "token".into(),
            "not-a-secret-key".into(),
        );
        assert_eq!(config.validate(), Err(ConfigError::InvalidIrohSecretKey));

        config.iroh_secret_key = None;
        assert_eq!(config.validate(), Err(ConfigError::MissingIrohSecretKey));
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
