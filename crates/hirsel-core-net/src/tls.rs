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
