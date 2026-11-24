//! One expanded packet key, with backend selection limited to the AEAD case.
use crate::{
    packet_cipher::{PacketCipher, TAG_LEN},
    packet_nonce::Nonce,
    secret_aead::AeadAlg,
    srtp_cipher::{self, SrtpCipher, SrtpSessionKey},
};
use ring::error::Unspecified;
use std::mem::MaybeUninit;

#[derive(Clone, Copy)]
pub enum Algorithm {
    Aead(AeadAlg),
    Cm,
}

impl Algorithm {
    pub fn tag_len(self) -> usize {
        match self {
            Self::Aead(_) => TAG_LEN,
            Self::Cm => srtp_cipher::TAG_LEN,
        }
    }
}

pub enum Key<'a> {
    Aead(&'a [u8]),
    Cm(&'a SrtpSessionKey),
}

pub trait Aead {
    fn expand(algorithm: AeadAlg, key: &[u8]) -> Result<Self, Unspecified>
    where
        Self: Sized;

    fn seal(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        body: &mut [u8],
    ) -> Result<[u8; TAG_LEN], Unspecified>;

    fn seal_into(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        plain: &[u8],
        sealed: &mut [MaybeUninit<u8>],
    ) -> Result<(), Unspecified>;

    fn open(&self, nonce: [u8; 12], aad: &[u8], sealed: &mut [u8]) -> Result<(), Unspecified>;

    fn open_into(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        sealed: &[u8],
        plain: &mut [MaybeUninit<u8>],
    ) -> Result<(), Unspecified>;
}

impl Aead for PacketCipher {
    fn expand(algorithm: AeadAlg, key: &[u8]) -> Result<Self, Unspecified> {
        Self::new(algorithm, key)
    }

    fn seal(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        body: &mut [u8],
    ) -> Result<[u8; TAG_LEN], Unspecified> {
        self.seal(nonce, aad, body)
    }

    fn seal_into(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        plain: &[u8],
        sealed: &mut [MaybeUninit<u8>],
    ) -> Result<(), Unspecified> {
        self.seal_into(nonce, aad, plain, sealed)
    }

    fn open(&self, nonce: [u8; 12], aad: &[u8], sealed: &mut [u8]) -> Result<(), Unspecified> {
        let n = sealed.len().checked_sub(TAG_LEN).ok_or(Unspecified)?;
        let (body, tag) = sealed.split_at_mut(n);
        self.open(nonce, aad, body, (&*tag).try_into().unwrap())
    }

    fn open_into(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        sealed: &[u8],
        plain: &mut [MaybeUninit<u8>],
    ) -> Result<(), Unspecified> {
        self.open_into(nonce, aad, sealed, plain)
    }
}

pub enum Cipher<A> {
    Aead(A),
    Cm(SrtpCipher),
}

pub type Aad<'a> = (&'a [u8], &'a [u8]);

impl<A: Aead> Cipher<A> {
    pub fn new(algorithm: Algorithm, key: Key<'_>) -> Result<Self, Unspecified> {
        match (algorithm, key) {
            (Algorithm::Aead(algorithm), Key::Aead(bytes)) => {
                A::expand(algorithm, bytes).map(Self::Aead)
            }
            (Algorithm::Cm, Key::Cm(session)) => {
                session.expand_cm().map(Self::Cm).ok_or(Unspecified)
            }
            _ => unreachable!("packet algorithm and key come from the same epoch"),
        }
    }

    pub fn seal(
        &self,
        nonce: Nonce,
        (aad, extra): Aad<'_>,
        body: &mut [u8],
    ) -> Result<[u8; TAG_LEN], Unspecified> {
        match (self, nonce) {
            (Self::Aead(cipher), Nonce::Aead(nonce)) => cipher.seal(nonce, aad, body),
            (Self::Cm(cipher), Nonce::Cm(nonce)) => {
                let mut tag = [0; TAG_LEN];
                tag[..srtp_cipher::TAG_LEN].copy_from_slice(&cipher.seal(nonce, aad, extra, body)?);
                Ok(tag)
            }
            _ => unreachable!("packet algorithm and nonce come from the same epoch"),
        }
    }

    pub fn seal_into(
        &self,
        nonce: Nonce,
        (aad, extra): Aad<'_>,
        plain: &[u8],
        sealed: &mut [MaybeUninit<u8>],
    ) -> Result<(), Unspecified> {
        match (self, nonce) {
            (Self::Aead(cipher), Nonce::Aead(nonce)) => cipher.seal_into(nonce, aad, plain, sealed),
            (Self::Cm(cipher), Nonce::Cm(nonce)) => {
                cipher.seal_into(nonce, aad, extra, plain, sealed)
            }
            _ => unreachable!("packet algorithm and nonce come from the same epoch"),
        }
    }

    pub fn open(
        &self,
        nonce: Nonce,
        (aad, extra): Aad<'_>,
        sealed: &mut [u8],
    ) -> Result<usize, Unspecified> {
        match (self, nonce) {
            (Self::Aead(cipher), Nonce::Aead(nonce)) => {
                cipher.open(nonce, aad, sealed)?;
                Ok(sealed.len() - TAG_LEN)
            }
            (Self::Cm(cipher), Nonce::Cm(nonce)) => {
                let n = sealed
                    .len()
                    .checked_sub(srtp_cipher::TAG_LEN)
                    .ok_or(Unspecified)?;
                let (body, tag) = sealed.split_at_mut(n);
                cipher.open(nonce, aad, extra, body, (&*tag).try_into().unwrap())?;
                Ok(n)
            }
            _ => unreachable!("packet algorithm and nonce come from the same epoch"),
        }
    }

    pub fn open_into(
        &self,
        nonce: Nonce,
        (aad, extra): Aad<'_>,
        sealed: &[u8],
        plain: &mut [MaybeUninit<u8>],
    ) -> Result<(), Unspecified> {
        match (self, nonce) {
            (Self::Aead(cipher), Nonce::Aead(nonce)) => cipher.open_into(nonce, aad, sealed, plain),
            (Self::Cm(cipher), Nonce::Cm(nonce)) => {
                cipher.open_into(nonce, aad, extra, sealed, plain)
            }
            _ => unreachable!("packet algorithm and nonce come from the same epoch"),
        }
    }
}
