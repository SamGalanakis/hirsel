//! TLS-8 key derivation. This module has no language entry point. Only the
//! protocol schedules supply labels and consume the output keys.
use crate::secret_aead::AeadAlg;
use ring::{digest, hkdf, hmac};
use zeroize::Zeroizing;

pub mod dtls12;

const MAX_HASH_LEN: usize = 48;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlsHash {
    Sha256,
    Sha384,
}

impl TlsHash {
    pub fn for_aead(algorithm: AeadAlg) -> Self {
        match algorithm {
            AeadAlg::Aes128Gcm | AeadAlg::ChaCha20Poly1305 => Self::Sha256,
            AeadAlg::Aes256Gcm => Self::Sha384,
        }
    }

    pub const fn output_len(self) -> usize {
        match self {
            Self::Sha256 => 32,
            Self::Sha384 => 48,
        }
    }

    fn hmac(self) -> hmac::Algorithm {
        match self {
            Self::Sha256 => hmac::HMAC_SHA256,
            Self::Sha384 => hmac::HMAC_SHA384,
        }
    }

    fn hkdf(self) -> hkdf::Algorithm {
        match self {
            Self::Sha256 => hkdf::HKDF_SHA256,
            Self::Sha384 => hkdf::HKDF_SHA384,
        }
    }
}

struct OutputLen(usize);

impl hkdf::KeyType for OutputLen {
    fn len(&self) -> usize {
        self.0
    }
}

/// RFC 5869 extract and expand. The output buffer is supplied by the schedule.
pub fn hkdf(
    hash: TlsHash,
    salt: &[u8],
    ikm: &[u8],
    info: &[&[u8]],
    out: &mut [u8],
) -> Result<(), ring::error::Unspecified> {
    hkdf::Salt::new(hash.hkdf(), salt)
        .extract(ikm)
        .expand(info, OutputLen(out.len()))?
        .fill(out)
}

/// RFC 5246 P_hash, with a split seed so randoms need no concatenation.
pub fn tls12_prf(hash: TlsHash, secret: &[u8], label: &[u8], seed: &[&[u8]], out: &mut [u8]) {
    if out.is_empty() {
        return;
    }
    let key = hmac::Key::new(hash.hmac(), secret);
    let mut initial = hmac::Context::with_key(&key);
    initial.update(label);
    for part in seed {
        initial.update(part);
    }
    let mut a = initial.sign();
    let mut chunks = out.chunks_mut(hash.output_len()).peekable();
    while let Some(chunk) = chunks.next() {
        let mut block = hmac::Context::with_key(&key);
        block.update(a.as_ref());
        block.update(label);
        for part in seed {
            block.update(part);
        }
        chunk.copy_from_slice(&block.sign().as_ref()[..chunk.len()]);
        if chunks.peek().is_some() {
            a = hmac::sign(&key, a.as_ref());
        }
    }
}

/// DTLS 1.3 uses a different string in place of RFC 8446's `tls13 `.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tls13Labels {
    Tls,
    Dtls,
    Quic,
}

impl Tls13Labels {
    fn prefix(self) -> &'static [u8; 6] {
        match self {
            Self::Tls | Self::Quic => b"tls13 ",
            Self::Dtls => b"dtls13",
        }
    }

    /// `context` is encoded verbatim. Callers of the traffic transitions
    /// supply a transcript digest and the `derived` steps supply Hash("").
    fn expand(self, hash: TlsHash, secret: &[u8], label: &[u8], context: &[u8], out: &mut [u8]) {
        let prefix = self.prefix();
        let len = (out.len() as u16).to_be_bytes();
        let label_len = [(prefix.len() + label.len()) as u8];
        let context_len = [context.len() as u8];
        let info = [&len[..], &label_len, prefix, label, &context_len, context];
        hkdf::Prk::new_less_safe(hash.hkdf(), secret)
            .expand(&info, OutputLen(out.len()))
            .expect("the schedule's fixed output fits HKDF")
            .fill(out)
            .expect("the output has the length encoded in the label");
    }
}

struct DigestSecret {
    hash: TlsHash,
    bytes: Zeroizing<[u8; MAX_HASH_LEN]>,
}

impl DigestSecret {
    fn from_tag(hash: TlsHash, tag: hmac::Tag) -> Self {
        let mut bytes = Zeroizing::new([0; MAX_HASH_LEN]);
        bytes[..hash.output_len()].copy_from_slice(tag.as_ref());
        Self { hash, bytes }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.hash.output_len()]
    }

    fn extract(hash: TlsHash, salt: &[u8], ikm: &[u8]) -> Self {
        Self::from_tag(hash, hmac::sign(&hmac::Key::new(hash.hmac(), salt), ikm))
    }

    fn expand(&self, labels: Tls13Labels, label: &[u8], context: &[u8]) -> Self {
        let mut bytes = Zeroizing::new([0; MAX_HASH_LEN]);
        labels.expand(
            self.hash,
            self.as_bytes(),
            label,
            context,
            &mut bytes[..self.hash.output_len()],
        );
        Self {
            hash: self.hash,
            bytes,
        }
    }

    fn derived(&self, labels: Tls13Labels) -> Self {
        let empty = digest::digest(self.hash.hmac().digest_algorithm(), &[]);
        self.expand(labels, b"derived", empty.as_ref())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Side {
    Client,
    Server,
}

impl Side {
    pub fn peer(self) -> Self {
        match self {
            Self::Client => Self::Server,
            Self::Server => Self::Client,
        }
    }
}

/// The packet keys computed for one send or receive epoch.
pub struct Tls13PacketKeys {
    payload: Zeroizing<[u8; 32]>,
    iv: Zeroizing<[u8; 12]>,
    header: Option<Zeroizing<[u8; 32]>>,
    key_len: usize,
}

impl Tls13PacketKeys {
    pub(super) fn new(key: &[u8], iv: [u8; 12]) -> Self {
        let mut payload = Zeroizing::new([0; 32]);
        payload[..key.len()].copy_from_slice(key);
        Self {
            payload,
            iv: Zeroizing::new(iv),
            header: None,
            key_len: key.len(),
        }
    }

    pub(super) fn import(labels: Tls13Labels, algorithm: AeadAlg, secret: &[u8]) -> Option<Self> {
        Some(Tls13Direction::new(labels, algorithm, secret)?.packet_keys())
    }

    pub(super) fn keep_header_of(&mut self, before: &mut Self) {
        self.header = before.header.take();
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.key_len]
    }

    pub fn iv(&self) -> [u8; 12] {
        *self.iv
    }

    pub fn header(&self) -> Option<&[u8]> {
        self.header.as_deref().map(|key| &key[..self.key_len])
    }
}

impl DigestSecret {
    fn packet_keys(
        &self,
        labels: Tls13Labels,
        algorithm: AeadAlg,
        updating: bool,
    ) -> Tls13PacketKeys {
        let (key_label, iv_label, header_label): (&[u8], &[u8], Option<&[u8]>) = match labels {
            Tls13Labels::Tls => (b"key", b"iv", None),
            Tls13Labels::Dtls => (b"key", b"iv", Some(b"sn")),
            Tls13Labels::Quic => (b"quic key", b"quic iv", (!updating).then_some(b"quic hp")),
        };
        let key_len = algorithm.algorithm().key_len();
        let expand = |label, output: &mut [u8]| {
            labels.expand(self.hash, self.as_bytes(), label, &[], output);
        };
        let mut payload = Zeroizing::new([0; 32]);
        let mut iv = Zeroizing::new([0; 12]);
        expand(key_label, &mut payload[..key_len]);
        expand(iv_label, iv.as_mut());
        let header = header_label.map(|label| {
            let mut key = Zeroizing::new([0; 32]);
            expand(label, &mut key[..key_len]);
            key
        });
        Tls13PacketKeys {
            payload,
            iv,
            header,
            key_len,
        }
    }
}

/// The protocol's client and server traffic secrets, in wire direction order.
pub struct Tls13Traffic {
    labels: Tls13Labels,
    secrets: [DigestSecret; 2],
}

impl Tls13Traffic {
    pub fn quic_initial(dcid: &[u8]) -> Option<Self> {
        if dcid.len() > 20 {
            return None;
        }
        // RFC 9001 §5.2, QUIC version 1.
        const SALT: [u8; 20] = [
            0x38, 0x76, 0x2c, 0xf7, 0xf5, 0x59, 0x34, 0xb3, 0x4d, 0x17, 0x9a, 0xe6, 0xa4, 0xc8,
            0x0c, 0xad, 0xcc, 0xbb, 0x7f, 0x0a,
        ];
        let labels = Tls13Labels::Quic;
        let initial = DigestSecret::extract(TlsHash::Sha256, &SALT, dcid);
        let secrets = [
            initial.expand(labels, b"client in", &[]),
            initial.expand(labels, b"server in", &[]),
        ];
        Some(Self { labels, secrets })
    }

    pub fn packet_keys(&self, sending: Side, alg: AeadAlg) -> Tls13PacketKeys {
        self.secrets[sending as usize].packet_keys(self.labels, alg, false)
    }

    pub fn key_update(&mut self, side: Side) {
        let secret = &mut self.secrets[side as usize];
        *secret = secret.updated(self.labels);
    }

    pub fn update_packet_keys(&mut self, side: Side, algorithm: AeadAlg) -> Tls13PacketKeys {
        self.key_update(side);
        self.secrets[side as usize].packet_keys(self.labels, algorithm, true)
    }
}

impl DigestSecret {
    fn updated(&self, labels: Tls13Labels) -> Self {
        let label: &[u8] = match labels {
            Tls13Labels::Quic => b"quic ku",
            Tls13Labels::Dtls | Tls13Labels::Tls => b"traffic upd",
        };
        self.expand(labels, label, &[])
    }
}

/// A single imported application traffic direction.
pub(super) struct Tls13Direction {
    labels: Tls13Labels,
    algorithm: AeadAlg,
    secret: DigestSecret,
}

impl Tls13Direction {
    pub(super) fn new(labels: Tls13Labels, algorithm: AeadAlg, secret: &[u8]) -> Option<Self> {
        let hash = TlsHash::for_aead(algorithm);
        if secret.len() != hash.output_len() {
            return None;
        }
        let mut bytes = Zeroizing::new([0; MAX_HASH_LEN]);
        bytes[..secret.len()].copy_from_slice(secret);
        Some(Self {
            labels,
            algorithm,
            secret: DigestSecret { hash, bytes },
        })
    }

    pub(super) fn packet_keys(&self) -> Tls13PacketKeys {
        self.secret.packet_keys(self.labels, self.algorithm, false)
    }

    pub(super) fn key_update(&mut self) -> Tls13PacketKeys {
        self.secret = self.secret.updated(self.labels);
        self.secret.packet_keys(self.labels, self.algorithm, true)
    }
}

/// Handshake state for a TLS 1.3 family exchange without a PSK.
/// Traffic secrets are confined to the schedule's protocol operations.
pub struct Tls13Handshake {
    secret: Option<DigestSecret>,
    labels: Tls13Labels,
    traffic: Option<Tls13Traffic>,
    finished_keys: Option<[DigestSecret; 2]>,
}

impl Tls13Handshake {
    pub fn new(
        hash: TlsHash,
        labels: Tls13Labels,
        shared: &[u8; 32],
        hello_hash: &[u8],
    ) -> Option<Self> {
        if hello_hash.len() != hash.output_len() {
            return None;
        }
        let zero = &[0; MAX_HASH_LEN][..hash.output_len()];
        let early = DigestSecret::extract(hash, zero, zero).derived(labels);
        let handshake = DigestSecret::extract(hash, early.as_bytes(), shared);
        let secrets = [
            handshake.expand(labels, b"c hs traffic", hello_hash),
            handshake.expand(labels, b"s hs traffic", hello_hash),
        ];
        let finished_keys = secrets
            .each_ref()
            .map(|secret| secret.expand(labels, b"finished", &[]));
        Some(Self {
            secret: Some(handshake),
            labels,
            traffic: Some(Tls13Traffic { labels, secrets }),
            finished_keys: Some(finished_keys),
        })
    }

    pub fn traffic(&self) -> &Tls13Traffic {
        self.traffic
            .as_ref()
            .expect("handshake traffic is available before retirement")
    }

    pub fn retire(&mut self) {
        self.traffic = None;
        self.finished_keys = None;
    }

    pub fn finished(&self, side: Side, transcript_hash: &[u8], out: &mut [u8]) -> Option<()> {
        let secret = &self.finished_keys.as_ref()?[side as usize];
        let hash = secret.hash;
        if transcript_hash.len() != hash.output_len() || out.len() != hash.output_len() {
            return None;
        }
        let key = hmac::Key::new(hash.hmac(), secret.as_bytes());
        out.copy_from_slice(hmac::sign(&key, transcript_hash).as_ref());
        Some(())
    }

    pub fn application(&mut self, finished_hash: &[u8]) -> Option<Tls13Traffic> {
        let hash = self.secret.as_ref()?.hash;
        if finished_hash.len() != hash.output_len() {
            return None;
        }
        let labels = self.labels;
        let derived = self.secret.take()?.derived(labels);
        let zero = &[0; MAX_HASH_LEN][..hash.output_len()];
        let master = DigestSecret::extract(hash, derived.as_bytes(), zero);
        let secrets = [
            master.expand(labels, b"c ap traffic", finished_hash),
            master.expand(labels, b"s ap traffic", finished_hash),
        ];
        Some(Tls13Traffic { labels, secrets })
    }
}

#[cfg(test)]
mod tests;
