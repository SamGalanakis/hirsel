//! REAPI blob identity and the content-addressed storage seam.
//!
//! Remote Execution API v2, `build/bazel/remote/execution/v2/remote_execution.proto`
//! (nativelink-proto copy): a blob is named by `Digest { hash, size_bytes }`
//! (`:1074-1081`), the SHA-256 of its raw bytes as 64 lowercase hex digits and
//! its length. The size is part of the digest (`:1043-1047`). Servers behave as
//! if the empty blob is always present (`:333-335`). Batch updates and reads
//! report one status per blob (`:1782-1793`, `:1826-1842`). An `ActionResult`
//! is found by action digest (`:157-196`); its blobs should be present when it
//! is returned (`:160-165`). Hirsel's framed [`Output`](crate::Output) hashes a
//! domain-separated stream, so it is a different name from the blob digest of
//! the same bytes; neither converts into the other.
//!
//! [`Cas`] and [`ActionCache`] are the whole remote surface. Batch limits,
//! ByteStream for oversized blobs, `instance_name` and digest-function
//! negotiation are properties of an implementation's construction, never of a
//! call. A cache hit carries integrity only: callers validate everything an
//! [`ActionResult`] names before use, and it never supplies verifier authority.
//!
//! [`DirCas`] keeps both stores under one directory. Its layout follows
//! nativelink `nativelink-store/src/filesystem_store.rs:186-201,733-737`
//! (content files named `{hash}-{size}`, written to a temporary file and
//! renamed into place). Its action-cache read refuses a result whose blobs are
//! missing, as nativelink's `completeness_checking_store.rs:40-60` does.
//! Deviations: no eviction or size accounting (callers own retention), reads
//! recheck the full digest because local files are not trusted, and an action
//! update requires the action blob and every output blob to be present first,
//! the precondition REAPI states for `UpdateActionResult` (`:176-190`).

use crate::{Bytes, Domain, Output, Reference};
use hirsel_syntax::ContentHash;
use sha2::{Digest as _, Sha256};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// REAPI `Digest` under `DigestFunction.SHA256`: the hash of the raw bytes and
/// their length.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BlobDigest {
    pub sha256: [u8; 32],
    pub size: u64,
}

impl BlobDigest {
    /// The digest of exactly these bytes, with no framing.
    pub fn of(bytes: &[u8]) -> Self {
        Self {
            sha256: Sha256::digest(bytes).into(),
            size: bytes.len() as u64,
        }
    }

    /// REAPI `Digest.hash`: 64 lowercase hex digits.
    pub fn hash(&self) -> String {
        let mut text = String::with_capacity(64);
        for byte in self.sha256 {
            text.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
            text.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
        }
        text
    }

    /// Parse a wire `Digest`. Uppercase hex, other lengths and negative sizes
    /// are refused, as the proto requires.
    pub fn from_reapi(hash: &str, size_bytes: i64) -> Option<Self> {
        let hash = hash.as_bytes();
        if hash.len() != 64 {
            return None;
        }
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        let mut sha256 = [0; 32];
        for (byte, pair) in sha256.iter_mut().zip(hash.chunks_exact(2)) {
            *byte = nibble(pair[0])? << 4 | nibble(pair[1])?;
        }
        Some(Self {
            sha256,
            size: u64::try_from(size_bytes).ok()?,
        })
    }

    /// REAPI `Digest.size_bytes`. Every digest made here fits.
    pub fn size_bytes(&self) -> i64 {
        i64::try_from(self.size).unwrap_or(i64::MAX)
    }

    pub fn is_empty(&self) -> bool {
        *self == EMPTY
    }

    fn file_name(&self) -> String {
        format!("{}-{}", self.hash(), self.size)
    }
}

/// The empty blob, present in every CAS.
pub const EMPTY: BlobDigest = BlobDigest {
    sha256: [
        0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9,
        0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52,
        0xb8, 0x55,
    ],
    size: 0,
};

/// The REAPI digest of Hirsel's canonical action bytes. Callers upload those
/// bytes to the [`Cas`] before [`ActionCache::update`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ActionDigest(pub BlobDigest);

impl ActionDigest {
    pub fn of(action: &[u8]) -> Self {
        Self(BlobDigest::of(action))
    }
}

/// One member of a component closure: its framed reference, which the reader
/// checks, and the blob that carries its canonical bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActionOutput {
    pub reference: Reference,
    pub blob: BlobDigest,
}

/// The outputs of one action: the component closure, in the producer's order.
/// It names content and grants no authority.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActionResult {
    pub outputs: Vec<ActionOutput>,
}

impl ActionResult {
    /// The closure member for these canonical bytes.
    pub fn output(domain: Domain, bytes: &[u8]) -> Option<ActionOutput> {
        Some(ActionOutput {
            reference: domain.reference(bytes)?,
            blob: BlobDigest::of(bytes),
        })
    }
}

/// REAPI `ContentAddressableStorage`. The outer error is a failed call; the
/// inner results are per-blob statuses.
pub trait Cas: Send + Sync {
    /// The requested digests that are absent, in request order, without
    /// duplicates. The empty blob is never missing.
    fn find_missing(&self, digests: &[BlobDigest]) -> io::Result<Vec<BlobDigest>>;
    /// Store each blob. A blob whose bytes do not match its digest fails with
    /// `InvalidInput` and stores nothing; the others are unaffected.
    fn batch_update(&self, blobs: &[(BlobDigest, Bytes)]) -> io::Result<Vec<io::Result<()>>>;
    /// Read each blob: `Ok(None)` is missing, `Err` is a failed read, including
    /// stored bytes that no longer match their digest (`InvalidData`).
    fn batch_read(&self, digests: &[BlobDigest]) -> io::Result<Vec<io::Result<Option<Bytes>>>>;
}

/// REAPI `ActionCache`.
pub trait ActionCache: Send + Sync {
    /// A cached result whose output blobs are all present, or `None`.
    fn get(&self, action: &ActionDigest) -> io::Result<Option<ActionResult>>;
    fn update(&self, action: &ActionDigest, result: &ActionResult) -> io::Result<()>;
}

/// A [`Cas`] and [`ActionCache`] under one local directory.
#[derive(Clone, Debug)]
pub struct DirCas {
    root: PathBuf,
}

const RESULT_MAGIC: &[u8; 8] = b"HIRAC001";
const RESULT_ENTRY: usize = 32 + 4 + 4 + 8 + 32 + 8;
static TEMPORARY: AtomicU64 = AtomicU64::new(0);

impl DirCas {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        for directory in ["cas", "ac", "tmp"] {
            fs::create_dir_all(root.join(directory))?;
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn blob_path(&self, digest: &BlobDigest) -> PathBuf {
        let name = digest.file_name();
        self.root.join("cas").join(&name[..2]).join(name)
    }

    fn result_path(&self, action: &ActionDigest) -> PathBuf {
        self.root.join("ac").join(action.0.file_name())
    }

    fn has(&self, digest: &BlobDigest) -> io::Result<bool> {
        if digest.is_empty() {
            return Ok(true);
        }
        match fs::metadata(self.blob_path(digest)) {
            Ok(metadata) => Ok(metadata.len() == digest.size),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn publish(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self.root.join("tmp").join(format!(
            "{}-{}",
            std::process::id(),
            TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let written = (|| {
            let mut file = fs::File::create(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, path)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        written
    }

    fn read_blob(&self, digest: &BlobDigest) -> io::Result<Option<Bytes>> {
        if digest.is_empty() {
            return Ok(Some(Vec::new().into()));
        }
        let bytes = match fs::read(self.blob_path(digest)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if BlobDigest::of(&bytes) != *digest {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("CAS blob {} does not match its digest", digest.hash()),
            ));
        }
        Ok(Some(bytes.into()))
    }
}

impl Cas for DirCas {
    fn find_missing(&self, digests: &[BlobDigest]) -> io::Result<Vec<BlobDigest>> {
        let mut missing = Vec::new();
        for digest in digests {
            if !missing.contains(digest) && !self.has(digest)? {
                missing.push(*digest);
            }
        }
        Ok(missing)
    }

    fn batch_update(&self, blobs: &[(BlobDigest, Bytes)]) -> io::Result<Vec<io::Result<()>>> {
        Ok(blobs
            .iter()
            .map(|(digest, bytes)| {
                if BlobDigest::of(bytes) != *digest {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("blob does not match digest {}", digest.hash()),
                    ));
                }
                if digest.is_empty() || self.read_blob(digest).is_ok_and(|b| b.is_some()) {
                    return Ok(());
                }
                self.publish(&self.blob_path(digest), bytes)
            })
            .collect())
    }

    fn batch_read(&self, digests: &[BlobDigest]) -> io::Result<Vec<io::Result<Option<Bytes>>>> {
        Ok(digests
            .iter()
            .map(|digest| self.read_blob(digest))
            .collect())
    }
}

impl ActionCache for DirCas {
    fn get(&self, action: &ActionDigest) -> io::Result<Option<ActionResult>> {
        let bytes = match fs::read(self.result_path(action)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let result = decode_result(&bytes).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("action result {} is corrupt", action.0.hash()),
            )
        })?;
        let blobs: Vec<_> = result.outputs.iter().map(|output| output.blob).collect();
        Ok(self.find_missing(&blobs)?.is_empty().then_some(result))
    }

    fn update(&self, action: &ActionDigest, result: &ActionResult) -> io::Result<()> {
        if result
            .outputs
            .iter()
            .any(|output| output.reference.len != output.blob.size)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "action output reference and blob differ in length",
            ));
        }
        let mut required = vec![action.0];
        required.extend(result.outputs.iter().map(|output| output.blob));
        let missing = self.find_missing(&required)?;
        if let Some(first) = missing.first() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "action result names {} blob(s) absent from the CAS, first {}",
                    missing.len(),
                    first.hash()
                ),
            ));
        }
        self.publish(&self.result_path(action), &encode_result(result))
    }
}

fn encode_result(result: &ActionResult) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16 + result.outputs.len() * RESULT_ENTRY + 32);
    bytes.extend(RESULT_MAGIC);
    bytes.extend((result.outputs.len() as u64).to_le_bytes());
    for output in &result.outputs {
        let reference = output.reference;
        bytes.extend(reference.output.0.0);
        bytes.extend(reference.domain.kind.to_le_bytes());
        bytes.extend(reference.domain.schema.to_le_bytes());
        bytes.extend(reference.len.to_le_bytes());
        bytes.extend(output.blob.sha256);
        bytes.extend(output.blob.size.to_le_bytes());
    }
    let check: [u8; 32] = Sha256::digest(&bytes).into();
    bytes.extend(check);
    bytes
}

fn decode_result(bytes: &[u8]) -> Option<ActionResult> {
    let (body, check) = bytes.split_at_checked(bytes.len().checked_sub(32)?)?;
    let header = body.get(..16)?;
    if header[..8] != *RESULT_MAGIC || Sha256::digest(body).as_slice() != check {
        return None;
    }
    let count = usize::try_from(u64::from_le_bytes(header[8..16].try_into().ok()?)).ok()?;
    let entries = &body[16..];
    if entries.len() != count.checked_mul(RESULT_ENTRY)? {
        return None;
    }
    let outputs = entries
        .chunks_exact(RESULT_ENTRY)
        .map(|entry| {
            let word = |at: usize| u32::from_le_bytes(entry[at..at + 4].try_into().unwrap());
            let number = |at: usize| u64::from_le_bytes(entry[at..at + 8].try_into().unwrap());
            ActionOutput {
                reference: Reference {
                    output: Output(ContentHash(entry[..32].try_into().unwrap())),
                    domain: Domain {
                        kind: word(32),
                        schema: word(36),
                    },
                    len: number(40),
                },
                blob: BlobDigest {
                    sha256: entry[48..80].try_into().unwrap(),
                    size: number(80),
                },
            }
        })
        .collect();
    Some(ActionResult { outputs })
}

#[cfg(test)]
mod tests;
