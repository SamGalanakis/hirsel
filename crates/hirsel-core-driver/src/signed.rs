//! Signed mappings, kept in `signed/<keyid>/` of the user's content store.
//!
//! A signed record is a mapping another builder published: a key, the kind
//! of record it is, the record's payload and the blobs that payload names,
//! signed with the builder's Ed25519 key. A reader takes one only when its
//! signer is a key the user listed, the signature verifies over every byte
//! before it, and its kind and key are exactly the ones asked for. Only the
//! kinds G1 lets travel have a tag: release std sides, object records
//! (release parts and development unit objects) and development instances. No tag exists for a linked executable, a test
//! verdict or coverage, so no record of those can be read. The file is kept
//! as received and checked again on every read, so a listed key removed
//! from the configuration withdraws every record it signed.
//!
//! `signed/` is read only through a remote session (`crate::remote`), which only
//! a build or test request opens. The launcher and runner host open none.
use super::{Store, collect, wrote};
use hirsel_core_ids::ContentHash;
use std::io;

const MAGIC: &[u8; 8] = b"HSIGNED\0";
/// The domain the signature covers, before the record's bytes.
const DOMAIN: &[u8] = b"hirsel.signed-record.v1\0";
/// `MAGIC`, the store's `VERSION`, the kind tag, the key and the signer.
const HEAD: usize = 8 + 4 + 1 + 32 + 32;
const SIGNATURE: usize = 64;
/// The most blobs one record names.
const MOST_BLOBS: usize = 1 << 16;

/// The kinds of record that travel, in G1's order. The tag is the byte the
/// record carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    /// A release std side, as its manifest: the side's records, and its
    /// blobs as one pack.
    Std = 1,
    /// An object record (`native_object::publish`): a release program
    /// part, or a development build's unit object.
    Object = 2,
    /// A Cranelift development instance's record.
    Instance = 3,
}

/// A builder key a user listed: its Ed25519 public key and its id.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Builder {
    pub public: [u8; 32],
    pub id: ContentHash,
}
impl Builder {
    pub(crate) fn new(public: [u8; 32]) -> Self {
        Self {
            public,
            id: ContentHash::of_parts("hirsel.builder-key.v1", [&public[..]]),
        }
    }
}

/// A record whose signature and fields checked.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Signed {
    /// The id of the listed key that signed it.
    pub signer: ContentHash,
    /// Each blob the payload names: its SHA-256 and its length.
    pub blobs: Vec<(ContentHash, u64)>,
    pub payload: Vec<u8>,
}

/// The bytes of a record of `kind` under `key` by `signer`, before its
/// signature, which `sign` appends.
pub(crate) fn unsigned(
    kind: Kind,
    key: ContentHash,
    signer: ContentHash,
    blobs: &[(ContentHash, u64)],
    payload: &[u8],
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEAD + 12 + 40 * blobs.len() + payload.len() + SIGNATURE);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&super::VERSION.to_le_bytes());
    bytes.push(kind as u8);
    bytes.extend_from_slice(&key.0);
    bytes.extend_from_slice(&signer.0);
    bytes.extend_from_slice(&(blobs.len() as u32).to_le_bytes());
    for (address, len) in blobs {
        bytes.extend_from_slice(&address.0);
        bytes.extend_from_slice(&len.to_le_bytes());
    }
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

/// What a signature covers: the domain, then the record's bytes before it.
pub(crate) fn message(unsigned: &[u8]) -> Vec<u8> {
    [DOMAIN, unsigned].concat()
}

/// The record in `bytes` when it is of `kind` under `key` and signed by one
/// of `listed`; anything else is `None`.
pub(crate) fn verify(
    bytes: &[u8],
    kind: Kind,
    key: ContentHash,
    listed: &[Builder],
) -> Option<Signed> {
    let body = bytes.len().checked_sub(SIGNATURE)?;
    let (unsigned, signature) = bytes.split_at(body);
    if unsigned.len() < HEAD + 4
        || &unsigned[..8] != MAGIC
        || unsigned[8..12] != super::VERSION.to_le_bytes()
        || unsigned[12] != kind as u8
        || unsigned[13..45] != key.0
    {
        return None;
    }
    let signer = ContentHash(unsigned[45..77].try_into().unwrap());
    let builder = listed.iter().find(|builder| builder.id == signer)?;
    if !hirsel_core_net::verify_ed25519(&builder.public, &message(unsigned), signature) {
        return None;
    }
    let mut rest = &unsigned[HEAD..];
    let mut take = |n: usize| {
        let (head, tail) = rest.split_at_checked(n)?;
        rest = tail;
        Some(head)
    };
    let count = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
    if count > MOST_BLOBS {
        return None;
    }
    let mut blobs = Vec::with_capacity(count);
    for _ in 0..count {
        let address = ContentHash(take(32)?.try_into().unwrap());
        blobs.push((address, u64::from_le_bytes(take(8)?.try_into().unwrap())));
    }
    let len = usize::try_from(u64::from_le_bytes(take(8)?.try_into().unwrap())).ok()?;
    let payload = take(len)?.to_vec();
    rest.is_empty().then_some(Signed {
        signer,
        blobs,
        payload,
    })
}

impl Store {
    /// Where the record `signer` signed under `key` is kept:
    /// `signed/<signer>/<shard>/<rest>` of this content store.
    pub(crate) fn signed_path(&self, signer: ContentHash, key: ContentHash) -> std::path::PathBuf {
        let signer = crate::request::hex(signer);
        self.named(&format!("{}/{signer}", collect::REMOTE), key)
    }
    /// The record of `kind` under `key` kept here, signed by one of
    /// `listed`, when it checks.
    pub(crate) fn signed(
        &self,
        kind: Kind,
        key: ContentHash,
        listed: &[Builder],
    ) -> io::Result<Option<Signed>> {
        for builder in listed {
            let path = self.signed_path(builder.id, key);
            if let Some(bytes) = self.read_whole(&path, true, None)?
                && let Some(signed) = verify(&bytes, kind, key, listed)
            {
                return Ok(Some(signed));
            }
        }
        Ok(None)
    }
    /// Keep `bytes`, a record received for `kind` under `key`, when it
    /// checks against `listed`; returns it checked.
    pub(crate) fn keep_signed(
        &self,
        bytes: &[u8],
        kind: Kind,
        key: ContentHash,
        listed: &[Builder],
    ) -> io::Result<Option<Signed>> {
        let Some(signed) = verify(bytes, kind, key, listed) else {
            return Ok(None);
        };
        self.publish(&self.signed_path(signed.signer, key), [bytes])?;
        wrote("record");
        Ok(Some(signed))
    }
}
