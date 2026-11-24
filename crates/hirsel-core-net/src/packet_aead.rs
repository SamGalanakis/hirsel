//! Shared packet validation and runtime result encoding for AEAD formats.
use crate::{packet_nonce::CounterError, packet_protection::Algorithm, secret_aead::AeadAlg};

#[derive(Clone, Copy)]
pub enum PacketError {
    Reused,
    Range,
    TooLong,
    Tampered,
    Capacity { requested: u64, available: u64 },
}

impl From<CounterError> for PacketError {
    fn from(error: CounterError) -> Self {
        match error {
            CounterError::Range => Self::Range,
            CounterError::Reused => Self::Reused,
        }
    }
}

pub fn outcome(answer: Result<usize, PacketError>) -> (u64, u8, u64) {
    match answer {
        Ok(bytes) => (bytes as u64, 0, 0),
        Err(PacketError::Reused) => (0, 1, 0),
        Err(PacketError::Range) => (0, 2, 0),
        Err(PacketError::TooLong) => (0, 3, 0),
        Err(PacketError::Tampered) => (0, 4, 0),
        Err(PacketError::Capacity {
            requested,
            available,
        }) => (requested, 5, available),
    }
}

pub struct PacketLengths {
    body: usize,
    header: usize,
    extra: usize,
}

impl PacketLengths {
    pub fn new(body: usize, header: usize, extra: usize) -> Self {
        Self {
            body,
            header,
            extra,
        }
    }

    pub fn check(&self, algorithm: Algorithm) -> Result<(), PacketError> {
        let (max_body, max_aad) = match algorithm {
            Algorithm::Aead(algorithm) => {
                let max_aad = match algorithm {
                    AeadAlg::Aes128Gcm | AeadAlg::Aes256Gcm => (1 << 61) - 1,
                    AeadAlg::ChaCha20Poly1305 => u64::MAX,
                };
                (algorithm.max_input(), max_aad)
            }
            Algorithm::Cm => (1 << 20, u64::MAX),
        };
        let aad = self
            .header
            .checked_add(self.extra)
            .ok_or(PacketError::TooLong)?;
        if self.body as u64 > max_body || aad as u64 > max_aad {
            return Err(PacketError::TooLong);
        }
        Ok(())
    }

    pub fn scratch(&self, algorithm: Algorithm) -> usize {
        if self.header == 0 || self.extra == 0 || matches!(algorithm, Algorithm::Cm) {
            0
        } else {
            self.header + self.extra
        }
    }

    pub fn spare(&self, requested: usize, available: usize) -> Result<(), PacketError> {
        fits(requested as u64, available)
    }
}

fn fits(requested: u64, available: usize) -> Result<(), PacketError> {
    if requested > available as u64 {
        return Err(PacketError::Capacity {
            requested,
            available: available as u64,
        });
    }
    Ok(())
}

pub fn sealed_len(plain: usize, tag_len: usize, available: usize) -> Result<usize, PacketError> {
    let requested = plain.checked_add(tag_len).ok_or(PacketError::Capacity {
        requested: u64::MAX,
        available: available as u64,
    })?;
    fits(requested as u64, available)?;
    Ok(requested)
}

pub fn sample_at(sample: &[u8], at: u64) -> Option<&[u8; 16]> {
    let at = usize::try_from(at).ok()?;
    sample.get(at..at.checked_add(16)?)?.try_into().ok()
}
