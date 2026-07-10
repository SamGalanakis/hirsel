use crate::ConfigError;

/// Generates a new persistent iroh client identity as a lowercase hex string.
///
/// The returned secret must be stored securely and reused for pairing and every
/// later connection made with the issued device token.
pub fn generate_iroh_identity() -> String {
    serialize_iroh_identity(&iroh::SecretKey::generate())
}

pub(crate) fn parse_iroh_identity(value: &str) -> Result<iroh::SecretKey, ConfigError> {
    value.parse().map_err(|_| ConfigError::InvalidIrohSecretKey)
}

fn serialize_iroh_identity(secret_key: &iroh::SecretKey) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in secret_key.to_bytes() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_identity_round_trips_as_lowercase_hex() {
        let identity = generate_iroh_identity();

        assert_eq!(identity.len(), 64);
        assert!(
            identity
                .chars()
                .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
        );
        let parsed = parse_iroh_identity(&identity).unwrap();
        assert_eq!(serialize_iroh_identity(&parsed), identity);
    }
}
