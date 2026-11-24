//! TLS 1.2 AEAD keys and the DTLS-SRTP exporter.
use super::{Side, TlsHash, tls12_prf};
use crate::{
    secret_aead::AeadAlg,
    srtp_cipher::{SrtpKeys, SrtpProfile},
};
use zeroize::Zeroizing;

pub struct Dtls12Keys {
    hash: TlsHash,
    master: Zeroizing<[u8; 48]>,
    block: Zeroizing<[u8; 88]>,
    randoms: [[u8; 32]; 2],
    key_len: usize,
    iv_len: usize,
    srtp_issued: bool,
}

impl Dtls12Keys {
    pub fn new(
        alg: AeadAlg,
        shared: &[u8; 32],
        client: [u8; 32],
        server: [u8; 32],
        session_hash: Option<&[u8]>,
    ) -> Option<Self> {
        let (hash, iv_len) = match alg {
            AeadAlg::Aes128Gcm => (TlsHash::Sha256, 4),
            AeadAlg::Aes256Gcm => (TlsHash::Sha384, 4),
            AeadAlg::ChaCha20Poly1305 => (TlsHash::Sha256, 12),
        };
        let key_len = alg.algorithm().key_len();
        let mut master = Zeroizing::new([0; 48]);
        match session_hash {
            Some(session_hash) if session_hash.len() == hash.output_len() => tls12_prf(
                hash,
                shared,
                b"extended master secret",
                &[session_hash],
                master.as_mut(),
            ),
            Some(_) => return None,
            None => tls12_prf(
                hash,
                shared,
                b"master secret",
                &[&client, &server],
                master.as_mut(),
            ),
        }
        let mut block = Zeroizing::new([0; 88]);
        let block_len = 2 * (key_len + iv_len);
        tls12_prf(
            hash,
            master.as_ref(),
            b"key expansion",
            &[&server, &client],
            &mut block[..block_len],
        );
        Some(Self {
            hash,
            master,
            block,
            randoms: [client, server],
            key_len,
            iv_len,
            srtp_issued: false,
        })
    }

    pub fn payload(&self, sending: Side) -> &[u8] {
        let offset = sending as usize * self.key_len;
        &self.block[offset..offset + self.key_len]
    }

    pub fn iv(&self, sending: Side) -> [u8; 12] {
        let offset = 2 * self.key_len + sending as usize * self.iv_len;
        let mut iv = [0; 12];
        iv[..self.iv_len].copy_from_slice(&self.block[offset..offset + self.iv_len]);
        iv
    }

    pub fn finished(&self, sending: Side, transcript_hash: &[u8]) -> Option<[u8; 12]> {
        if transcript_hash.len() != self.hash.output_len() {
            return None;
        }
        let label: &[u8] = match sending {
            Side::Client => b"client finished",
            Side::Server => b"server finished",
        };
        let mut out = [0; 12];
        tls12_prf(
            self.hash,
            self.master.as_ref(),
            label,
            &[transcript_hash],
            &mut out,
        );
        Some(out)
    }

    /// RFC 5764 gives the client and server their own master keys and salts.
    /// The caller pre-admits the child's storage before taking the exporter.
    pub fn take_srtp(&mut self, profile: SrtpProfile, local: Side) -> Option<[SrtpKeys; 2]> {
        if self.srtp_issued {
            return None;
        }
        let (key, salt) = profile.master_widths();
        let mut exported = Zeroizing::new([0; 88]);
        self.srtp_export(&mut exported[..2 * (key + salt)]);
        let local = local as usize;
        let make = |side: usize, send| {
            let start = 2 * key + side * salt;
            SrtpKeys::from_master(
                profile,
                send,
                &exported[side * key..(side + 1) * key],
                &exported[start..start + salt],
            )
        };
        let child = [make(local, true), make(1 - local, false)];
        self.srtp_issued = true;
        Some(child)
    }

    pub fn srtp_export(&self, out: &mut [u8]) {
        tls12_prf(
            self.hash,
            self.master.as_ref(),
            b"EXTRACTOR-dtls_srtp",
            &[&self.randoms[0], &self.randoms[1]],
            out,
        );
    }
}
