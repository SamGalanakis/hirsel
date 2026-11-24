//! TLS-8 agreement backends. Host shares use ring's SystemRandom, while
//! virtual shares consume complete draws from their provider's byte stream.
use p256::elliptic_curve::{ecdh, sec1::ToSec1Point};
use ring::{agreement, rand::SystemRandom};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret as X25519Secret};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Group {
    X25519,
    P256,
}

impl Group {
    pub const fn public_len(self) -> usize {
        match self {
            Self::X25519 => 32,
            Self::P256 => 65,
        }
    }
}

pub struct KeyShare(Key);

enum Key {
    Host(agreement::EphemeralPrivateKey),
    VirtualX25519(X25519Secret),
    VirtualP256(p256::SecretKey),
}

impl KeyShare {
    /// The caller provides exactly the group's public-key length.
    pub fn host(group: Group, public: &mut [u8]) -> Result<Self, ring::error::Unspecified> {
        let algorithm = match group {
            Group::X25519 => &agreement::X25519,
            Group::P256 => &agreement::ECDH_P256,
        };
        let key = agreement::EphemeralPrivateKey::generate(algorithm, &SystemRandom::new())?;
        let generated = key.compute_public_key()?;
        public.copy_from_slice(generated.as_ref());
        Ok(Self(Key::Host(key)))
    }

    /// An exhausted tape is returned to the adapter as a fault. The draw
    /// owns its cursor, which moves only after it has filled a whole block.
    pub fn from_draws<E>(
        group: Group,
        mut draw: impl FnMut(&mut [u8; 32]) -> Result<(), E>,
        public: &mut [u8],
    ) -> Result<Self, E> {
        match group {
            Group::X25519 => {
                let mut candidate = Zeroizing::new([0; 32]);
                draw(&mut candidate)?;
                let key = X25519Secret::from(*candidate);
                public.copy_from_slice(X25519Public::from(&key).as_bytes());
                Ok(Self(Key::VirtualX25519(key)))
            }
            Group::P256 => {
                let key = draw_p256(draw)?;
                public.copy_from_slice(key.public_key().to_sec1_point(false).as_bytes());
                Ok(Self(Key::VirtualP256(key)))
            }
        }
    }

    /// Consumes the ephemeral share. For P-256, TLS uses the X coordinate
    /// of the agreed point, with no prefix. The schedule takes the result.
    pub fn agree(self, peer: &[u8]) -> Option<Zeroizing<[u8; 32]>> {
        match self.0 {
            Key::Host(key) => {
                let algorithm = key.algorithm();
                let peer = agreement::UnparsedPublicKey::new(algorithm, peer);
                agreement::agree_ephemeral(key, &peer, |shared| {
                    Zeroizing::new(shared.try_into().unwrap())
                })
                .ok()
            }
            Key::VirtualX25519(key) => {
                let peer = X25519Public::from(<[u8; 32]>::try_from(peer).ok()?);
                let shared = key.diffie_hellman(&peer);
                shared
                    .was_contributory()
                    .then(|| Zeroizing::new(*shared.as_bytes()))
            }
            Key::VirtualP256(key) => {
                if peer.len() != Group::P256.public_len() || peer[0] != 4 {
                    return None;
                }
                let peer = p256::PublicKey::from_sec1_bytes(peer).ok()?;
                let shared = ecdh::diffie_hellman(key.to_nonzero_scalar(), peer.as_affine());
                Some(Zeroizing::new(
                    shared.raw_secret_bytes().as_slice().try_into().unwrap(),
                ))
            }
        }
    }
}

pub(super) fn draw_p256<E>(
    mut draw: impl FnMut(&mut [u8; 32]) -> Result<(), E>,
) -> Result<p256::SecretKey, E> {
    let mut candidate = Zeroizing::new([0; 32]);
    loop {
        draw(&mut candidate)?;
        if let Ok(key) = p256::SecretKey::from_slice(&candidate[..]) {
            return Ok(key);
        }
        candidate.zeroize();
    }
}

#[cfg(test)]
mod tests;
