//! TLS-1 input validation shared by both provider call forms.

use crate::{Name, TlsTrust};
use rustls::{
    RootCertStore,
    pki_types::{CertificateDer, pem::PemObject},
};

pub fn verification_name(bytes: &[u8]) -> Option<Name> {
    Name::parse(std::str::from_utf8(bytes).ok()?)
}

pub fn valid_protocols(protocols: &[impl AsRef<[u8]>]) -> bool {
    protocols.len() <= 16
        && protocols.iter().enumerate().all(|(i, protocol)| {
            let protocol = protocol.as_ref();
            (1..=255).contains(&protocol.len())
                && !protocols[..i].iter().any(|p| p.as_ref() == protocol)
        })
}

pub fn trust(tag: u8, pem: &[u8]) -> Option<(TlsTrust, Option<RootCertStore>)> {
    match tag {
        0 => Some((TlsTrust::System, None)),
        1 => {
            let mut store = RootCertStore::empty();
            for certificate in CertificateDer::pem_slice_iter(pem) {
                store.add(certificate.ok()?).ok()?;
            }
            if store.is_empty() {
                return None;
            }
            let digest = ring::digest::digest(&ring::digest::SHA256, pem);
            Some((
                TlsTrust::Roots(digest.as_ref().try_into().unwrap()),
                Some(store),
            ))
        }
        _ => None,
    }
}

pub fn peer_words(digest: [u8; 32]) -> [u64; 4] {
    let (words, _) = digest.as_chunks::<8>();
    std::array::from_fn(|i| u64::from_be_bytes(words[i]))
}

/// The largest Certificate message body `tls.connect` reads (TLS-9, check 2):
/// its unbuffered rustls client joins a handshake body of at most 0xffff
/// bytes. rustls's buffered connection, which `tls.connect` does not use,
/// caps its join buffer at 0xffff record bytes and so reads less.
pub const CHAIN_MESSAGE_MAX: usize = 65_535;

/// TLS-9 check 2: 1 to 8 entries, each exactly one DER `Certificate`
/// SEQUENCE with no trailing bytes, and a TLS 1.3 Certificate message body of
/// 4 + Σ(DER length + 5) bytes (RFC 8446 §4.4.2) within [`CHAIN_MESSAGE_MAX`].
/// Only the outer structure is parsed.
pub fn valid_chain(chain: &[impl AsRef<[u8]>]) -> bool {
    (1..=8).contains(&chain.len())
        && chain.iter().all(|entry| one_sequence(entry.as_ref()))
        && 4 + chain
            .iter()
            .map(|entry| entry.as_ref().len() + 5)
            .sum::<usize>()
            <= CHAIN_MESSAGE_MAX
}

/// One definite-length DER SEQUENCE spanning all of `bytes`.
fn one_sequence(bytes: &[u8]) -> bool {
    let [0x30, length, tail @ ..] = bytes else {
        return false;
    };
    let (len, content) = match *length {
        n if n < 0x80 => (usize::from(n), tail),
        0x81..=0x83 => {
            let size = usize::from(length & 0x7f);
            let Some((encoded, content)) = tail.split_at_checked(size) else {
                return false;
            };
            // DER: the shortest length encoding, so no leading zero byte and
            // no long form for a length below 128.
            let len = encoded
                .iter()
                .fold(0usize, |n, &b| (n << 8) | usize::from(b));
            if encoded[0] == 0 || len < 0x80 {
                return false;
            }
            (len, content)
        }
        _ => return false,
    };
    content.len() == len
}
