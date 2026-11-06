//! Immutable compiler products, independently addressed actions and checked maps.
//!
//! Go ebe2feefc400f813aefdc8a036e66d2619da6649, cache.go:28-34,200-255:
//! an action association is separate from the content it names. Lean
//! 64942e142f2cbd3576ad89c3df41f8b6ccee91d9, compact.cpp:137,183 and
//! module.cpp:500: retain immutable shared regions and map only demanded ones.
//! Unlike Lean, references are checked portable offsets, never saved addresses.
//! Integrity is not verifier authority: this module does not issue V receipts.

#[cfg(test)]
hirsel_guard::install!();

use hirsel_syntax::ContentHash;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Write};
use std::ops::{Deref, Range};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The largest independently mapped record. Oversized queries must split their
/// output into referenced records; packing never builds another whole program.
pub const MAX_RECORD: usize = 16 << 20;
const MAGIC: &[u8; 8] = b"HIRART02";
const HEADER: usize = 64;
const ROOT: &[u8; 8] = b"HIRACT02";
const ROOT_LEN: usize = 152;

mod group;
mod index;
mod pack;
pub use pack::{Batch, Request, RetentionScope};
pub mod reapi;
mod statistics;
mod verified;
use statistics::Counter;
pub use statistics::{Statistics, StatisticsSnapshot};
pub use verified::{VerifiedRecords, VerifiedReference};

/// Stable query address, used before its previous dependencies are evaluated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Address(pub ContentHash);
/// Complete computation identity, including all observed dependency outputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Action(pub ContentHash);
/// Canonical output identity. Equal recomputation retains this identity.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct Output(pub ContentHash);

/// A content domain. Every referenced record checks both kind and schema.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct Domain {
    pub kind: u32,
    pub schema: u32,
}

/// A reference cannot supply proof authority. Producers and verifier-owned
/// associations use separate roots; imported bytes create no action association.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct Reference {
    pub output: Output,
    pub domain: Domain,
    pub len: u64,
}

/// Failure to obtain required content. Absence is distinct from an unreadable
/// backing or a request refusing the read under its existing allowance.
#[derive(Debug)]
pub enum ContentReadError {
    NotFound,
    InvalidContent,
    Unreadable(io::Error),
    Refused(&'static str),
    Admission {
        stage: &'static str,
        requested: u64,
        retained: u64,
        limit: u64,
    },
}
impl std::fmt::Display for ContentReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("content not found in the selected provider"),
            Self::InvalidContent => f.write_str("content differs from its complete reference"),
            Self::Unreadable(error) => write!(f, "content unreadable: {error}"),
            Self::Refused(reason) => write!(f, "content read refused: {reason}"),
            Self::Admission {
                stage,
                requested,
                retained,
                limit,
            } => write!(
                f,
                "{stage} admission refused: requested {requested} bytes, retained {retained}, limit {limit}"
            ),
        }
    }
}
impl std::error::Error for ContentReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unreadable(error) => Some(error),
            _ => None,
        }
    }
}

/// Content access supplies integrity, never query or verifier authority.
pub trait Contents: Send + Sync {
    fn get_content(&self, reference: Reference) -> Result<Bytes, ContentReadError>;
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference>;
}

#[derive(Default)]
pub struct Memory {
    entries: std::sync::Mutex<std::collections::HashMap<ContentHash, (Reference, Bytes)>>,
}
impl Memory {
    /// Snapshot references and shared payload views for bounded toolchain packing.
    pub fn entries(&self) -> Vec<(Reference, Bytes)> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }
}
/// Ordinary store containers embedded by the toolchain build. Entries are
/// sorted by output digest; complete container validation occurs on demand.
pub struct Embedded(pub &'static [(Reference, &'static [u8])]);
impl Contents for Embedded {
    fn get_content(&self, reference: Reference) -> Result<Bytes, ContentReadError> {
        let n = self
            .0
            .binary_search_by_key(&reference.output.0, |(r, _)| r.output.0)
            .map_err(|_| ContentReadError::NotFound)?;
        let (actual, bytes) = self.0[n];
        if actual != reference {
            return Err(ContentReadError::InvalidContent);
        }
        checked_content(Arc::new(Region::Static(bytes)), reference)
            .ok_or(ContentReadError::InvalidContent)
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        let reference = domain.reference(bytes)?;
        self.get_content(reference).ok().map(|_| reference)
    }
}
impl Contents for Memory {
    fn get_content(&self, reference: Reference) -> Result<Bytes, ContentReadError> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let (actual, bytes) = entries
            .get(&reference.output.0)
            .ok_or(ContentReadError::NotFound)?;
        if *actual != reference {
            return Err(ContentReadError::InvalidContent);
        }
        Ok(bytes.clone())
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        let reference = domain.reference(bytes)?;
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(reference.output.0, (reference, bytes.into()));
        Some(reference)
    }
}

impl Domain {
    /// The canonical reference to these bytes, without publication or authority.
    pub fn reference(self, bytes: &[u8]) -> Option<Reference> {
        (bytes.len() <= MAX_RECORD).then(|| Reference {
            output: self.output(bytes),
            domain: self,
            len: bytes.len() as u64,
        })
    }
    fn output(self, bytes: &[u8]) -> Output {
        // ContentHash::of_parts frames each part with its little-endian u64
        // length. Feed the fixed framing together, then every payload byte.
        const DOMAIN: &[u8] = b"hirsel.artifact.output.v1";
        let mut prefix = [0u8; 65];
        prefix[..8].copy_from_slice(&(DOMAIN.len() as u64).to_le_bytes());
        prefix[8..33].copy_from_slice(DOMAIN);
        prefix[33..41].copy_from_slice(&4u64.to_le_bytes());
        prefix[41..45].copy_from_slice(&self.kind.to_le_bytes());
        prefix[45..53].copy_from_slice(&4u64.to_le_bytes());
        prefix[53..57].copy_from_slice(&self.schema.to_le_bytes());
        prefix[57..65].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
        let mut hash = Sha256::new();
        hash.update(prefix);
        hash.update(bytes);
        Output(ContentHash(hash.finalize().into()))
    }
}

enum Region {
    #[cfg(unix)]
    Map {
        file: Option<Arc<File>>,
        address: *mut libc::c_void,
        len: usize,
    },
    Owned(Arc<[u8]>),
    Buffer(Box<[u8]>),
    Static(&'static [u8]),
    Checked(Box<CheckedRegion>),
    Payload(Box<CheckedPayload>),
    Retained {
        region: Arc<Region>,
        _owner: Arc<dyn Send + Sync>,
    },
}

// Rust std 1.90.0, sync/once_lock.rs:151,309: a successful initialization
// belongs to its immutable owner. Slots only avoid repeated integrity work;
// collisions fall back to the complete SHA-256 check. No action is memoized.
struct CheckedRegion {
    region: Arc<Region>,
    records: [std::sync::OnceLock<CheckedContainer>; 16],
    statistics: Option<Arc<Statistics>>,
}
struct CheckedContainer {
    range: Range<usize>,
    reference: Reference,
}
struct CheckedPayload {
    region: Arc<Region>,
    range: Range<usize>,
    reference: Reference,
}
// SAFETY: mappings are read-only; publication replaces files rather than
// modifying a mapped inode. Every view retains an Arc to its mapping.
unsafe impl Send for Region {}
// SAFETY: no mutable mapping or pointer escapes this module.
unsafe impl Sync for Region {}

impl Region {
    fn open(path: &Path, retain_file: bool) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = usize::try_from(file.metadata()?.len()).map_err(|_| invalid())?;
        if !(HEADER..=HEADER + MAX_RECORD).contains(&len) {
            return Err(invalid());
        }
        if !file.metadata()?.is_file() {
            return Err(invalid());
        }
        #[cfg(target_os = "linux")]
        let file = if retain_file {
            Self::sealed_snapshot(file, len)?
        } else {
            file
        };
        #[cfg(all(unix, not(target_os = "linux")))]
        if retain_file {
            use std::io::Read;
            let mut bytes = Vec::new();
            file.take((len + 1) as u64).read_to_end(&mut bytes)?;
            return (bytes.len() == len)
                .then_some(Self::Owned(bytes.into()))
                .ok_or_else(invalid);
        }
        #[cfg(unix)]
        {
            let mut region = Self::map_file(Arc::new(file), len)?;
            if !retain_file && let Self::Map { file, .. } = &mut region {
                *file = None;
            }
            Ok(region)
        }
        #[cfg(not(unix))]
        {
            use std::io::Read;
            let _ = retain_file;
            let mut bytes = Vec::new();
            file.take((HEADER + MAX_RECORD + 1) as u64)
                .read_to_end(&mut bytes)?;
            (bytes.len() == len)
                .then_some(Self::Owned(bytes.into()))
                .ok_or_else(invalid)
        }
    }
    // systemd v258, src/basic/memfd-util.c:126-154: populate a private memfd,
    // seal writes/resize, then map it. Unlike systemd's in-memory input, copy
    // through the opened source descriptor with a length sentinel before any
    // integrity check. Portable targets retain owned bytes instead.
    #[cfg(target_os = "linux")]
    fn sealed_snapshot(source: File, len: usize) -> io::Result<File> {
        use std::io::Read;
        use std::os::fd::{AsRawFd, FromRawFd};
        // SAFETY: the static C string is terminated; a successful descriptor
        // is uniquely owned below and permits sealing before any mapping.
        let fd = unsafe {
            libc::memfd_create(
                c"hirsel-installation".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: memfd_create returned a fresh descriptor owned by this call.
        let mut snapshot = unsafe { File::from_raw_fd(fd) };
        if std::io::copy(&mut source.take((len + 1) as u64), &mut snapshot)? != len as u64 {
            return Err(invalid());
        }
        let seals =
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        // SAFETY: snapshot owns the live descriptor and no writable mapping
        // exists. These seals permanently prevent modification and truncation.
        if unsafe { libc::fcntl(snapshot.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(snapshot)
    }
    #[cfg(unix)]
    fn map_file(file: Arc<File>, len: usize) -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        if !(HEADER..=HEADER + MAX_RECORD).contains(&len) {
            return Err(invalid());
        }
        // SAFETY: the retained file is immutable, nonempty and read-only. This
        // region owns the exact mapping until its final byte view is dropped.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self::Map {
            file: Some(file),
            address,
            len,
        })
    }
    #[cfg(unix)]
    fn file(&self) -> Option<&Arc<File>> {
        match self {
            Self::Map { file, .. } => file.as_ref(),
            Self::Checked(checked) => checked.region.file(),
            Self::Payload(checked) => checked.region.file(),
            Self::Retained { region, .. } => region.file(),
            _ => None,
        }
    }
    fn bytes(&self) -> &[u8] {
        match self {
            #[cfg(unix)]
            Self::Map { address, len, .. } => {
                // SAFETY: this object owns the mapping; returned borrow is bounded
                // by both its length and lifetime.
                unsafe { std::slice::from_raw_parts(address.cast(), *len) }
            }
            Self::Owned(bytes) => bytes,
            Self::Buffer(bytes) => bytes,
            Self::Static(bytes) => bytes,
            Self::Checked(checked) => checked.region.bytes(),
            Self::Payload(checked) => checked.region.bytes(),
            Self::Retained { region, .. } => region.bytes(),
        }
    }
    fn checks(&self) -> Option<&CheckedRegion> {
        match self {
            Self::Checked(checked) => Some(checked),
            Self::Payload(checked) => checked.region.checks(),
            Self::Retained { region, .. } => region.checks(),
            _ => None,
        }
    }
}
impl Drop for Region {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            if let Self::Map { address, len, .. } = *self {
                // SAFETY: exactly the mapping allocated in open, with no live views.
                unsafe { libc::munmap(address, len) };
            }
        }
    }
}

/// An immutable, borrowed range of a shared mapping. Cloning does not read pages
/// or allocate a payload. The semantic decoder decides which bytes it needs.
#[derive(Clone)]
pub struct Bytes {
    region: Arc<Region>,
    range: Range<usize>,
}
/// A non-owning view of one exact backing. It can reuse integrity only while
/// a strong reader still owns that backing and its admission reservation.
#[derive(Clone)]
pub struct WeakBytes {
    region: std::sync::Weak<Region>,
    range: Range<usize>,
}
impl WeakBytes {
    /// Number of live immutable views, including an epoch's retained view.
    pub fn strong_count(&self) -> usize {
        self.region.strong_count()
    }
    pub fn upgrade(&self) -> Option<Bytes> {
        Some(Bytes {
            region: self.region.upgrade()?,
            range: self.range.clone(),
        })
    }
    /// Allocation left alive by this weak pointer after the last reader drops.
    pub fn metadata_bytes() -> u64 {
        (size_of::<Region>() + 2 * size_of::<usize>()) as u64
    }
}

/// A private hard link to a provider inode, independent of publication and
/// eviction. Descriptors live only while checking or reading the container.
/// Each newly opened range still checks the full container and reference.
pub struct HeldContent {
    path: PathBuf,
    reference: Reference,
}
/// A provider change hint used only to discard an owned immutable snapshot.
/// It never establishes byte integrity or authorizes a provider read.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HeldVersion([u64; 7]);
impl HeldContent {
    pub fn version(&self) -> Option<HeldVersion> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = fs::metadata(&self.path).ok()?;
            Some(HeldVersion([
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.mtime() as u64,
                metadata.mtime_nsec() as u64,
                metadata.ctime() as u64,
                metadata.ctime_nsec() as u64,
            ]))
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
    pub fn get(&self) -> Result<Bytes, ContentReadError> {
        let len = usize::try_from(self.reference.len)
            .ok()
            .and_then(|len| len.checked_add(HEADER))
            .ok_or(ContentReadError::InvalidContent)?;
        let file = File::open(&self.path).map_err(ContentReadError::Unreadable)?;
        if file.metadata().map_err(ContentReadError::Unreadable)?.len() != len as u64 {
            return Err(ContentReadError::InvalidContent);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            let mut bytes = vec![0; len];
            file.read_exact_at(&mut bytes, 0)
                .map_err(ContentReadError::Unreadable)?;
            checked_container(Bytes::from(bytes), self.reference)
                .ok_or(ContentReadError::InvalidContent)
        }
        #[cfg(not(unix))]
        {
            Err(ContentReadError::Refused(
                "provider inode reads require Unix",
            ))
        }
    }
    pub fn retained_bytes(&self) -> u64 {
        self.reference.len + HEADER as u64
    }
    pub fn metadata_bytes(&self) -> u64 {
        (size_of::<Self>() + self.path.capacity()) as u64
    }
}
impl Drop for HeldContent {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl std::fmt::Debug for Bytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bytes").field("len", &self.len()).finish()
    }
}
impl PartialEq for Bytes {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}
impl Eq for Bytes {}
impl From<Arc<[u8]>> for Bytes {
    fn from(bytes: Arc<[u8]>) -> Self {
        let len = bytes.len();
        Self {
            region: Arc::new(Region::Owned(bytes)),
            range: 0..len,
        }
    }
}
impl From<Vec<u8>> for Bytes {
    fn from(bytes: Vec<u8>) -> Self {
        let len = bytes.len();
        Self {
            region: Arc::new(Region::Buffer(bytes.into_boxed_slice())),
            range: 0..len,
        }
    }
}
impl From<&[u8]> for Bytes {
    fn from(bytes: &[u8]) -> Self {
        Self::from(Arc::<[u8]>::from(bytes))
    }
}
impl Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.region.bytes()[self.range.clone()]
    }
}
impl Bytes {
    /// Allocation bound for a copied slice with an attached lifetime owner:
    /// two region Arcs and the aligned payload Arc, including their counters.
    pub fn owned_with_owner_bytes(len: u64) -> Option<u64> {
        let alignment = std::mem::align_of::<usize>() as u64;
        let payload = len.checked_add(alignment - 1)? / alignment * alignment;
        payload.checked_add((2 * size_of::<Region>() + 6 * size_of::<usize>()) as u64)
    }

    pub fn downgrade(&self) -> WeakBytes {
        WeakBytes {
            region: Arc::downgrade(&self.region),
            range: self.range.clone(),
        }
    }

    /// Check this exact payload against a content reference. A matching checked
    /// container range reuses its immutable backing's integrity check. Other
    /// views, including arbitrary provider bytes, require the complete digest.
    pub fn matches_reference(&self, reference: Reference) -> bool {
        if self.has_reference(reference) {
            return true;
        }
        self.count_content_check();
        reference.domain.reference(self) == Some(reference)
    }

    fn has_reference(&self, reference: Reference) -> bool {
        let mut region = &self.region;
        loop {
            match &**region {
                Region::Payload(checked) => {
                    if checked.range == self.range && checked.reference == reference {
                        return true;
                    }
                    region = &checked.region;
                }
                Region::Retained { region: parent, .. } => region = parent,
                _ => break,
            }
        }
        if let Some(checks) = self.region.checks() {
            let slot = &checks.records[reference.output.0.0[0] as usize % checks.records.len()];
            if let Some(checked) = slot.get()
                && checked.reference == reference
                && checked.range.start.checked_add(HEADER) == Some(self.range.start)
                && checked.range.end == self.range.end
            {
                return true;
            }
        }
        false
    }

    /// Bind a complete content check to this exact immutable payload range.
    /// The returned view retains the full backing and every admission owner.
    /// This records byte integrity only, never query or verifier acceptance.
    pub fn checked(self, reference: Reference) -> Option<Self> {
        if self.has_reference(reference) {
            return Some(self);
        }
        self.count_content_check();
        if reference.domain.reference(&self)? != reference {
            return None;
        }
        Some(self.with_reference(reference))
    }

    fn count_content_check(&self) {
        if let Some(statistics) = self
            .region
            .checks()
            .and_then(|checks| checks.statistics.as_ref())
        {
            statistics.add(Counter::ContentChecks, 1);
        }
    }

    fn with_reference(self, reference: Reference) -> Self {
        Self {
            region: Arc::new(Region::Payload(Box::new(CheckedPayload {
                region: self.region,
                range: self.range.clone(),
                reference,
            }))),
            range: self.range,
        }
    }

    /// Visit the complete physical backing retained by this view. Identities
    /// are process-local allocation keys for the existing shared-charge policy;
    /// they never enter portable records. A child range retains the same charges.
    /// An attached admission owner is already charged by its reservation.
    pub fn visit_retained_allocations(&self, mut visit: impl FnMut(usize, usize)) {
        let mut region = &self.region;
        loop {
            visit(
                Arc::as_ptr(region) as usize,
                std::mem::size_of::<Region>() + 2 * std::mem::size_of::<usize>(),
            );
            match &**region {
                #[cfg(unix)]
                Region::Map { file, address, len } => {
                    visit(*address as usize, *len);
                    if let Some(file) = file {
                        visit(
                            Arc::as_ptr(file) as usize,
                            size_of::<File>() + 2 * size_of::<usize>(),
                        );
                    }
                }
                Region::Owned(bytes) => visit(
                    bytes.as_ptr() as usize,
                    bytes.len() + 2 * std::mem::size_of::<usize>(),
                ),
                Region::Buffer(bytes) => visit(bytes.as_ptr() as usize, bytes.len()),
                Region::Static(_) => (),
                Region::Checked(checked) => {
                    visit(
                        &**checked as *const CheckedRegion as usize,
                        std::mem::size_of::<CheckedRegion>(),
                    );
                    region = &checked.region;
                    continue;
                }
                Region::Payload(checked) => {
                    visit(
                        &**checked as *const CheckedPayload as usize,
                        std::mem::size_of::<CheckedPayload>(),
                    );
                    region = &checked.region;
                    continue;
                }
                Region::Retained { region: parent, .. } => {
                    region = parent;
                    continue;
                }
            }
            break;
        }
    }

    /// Retain an admission reservation or other lifetime owner through clones
    /// and child ranges without copying the underlying immutable bytes.
    pub fn retain_owner(self, owner: Arc<dyn Send + Sync>) -> Self {
        Self {
            region: Arc::new(Region::Retained {
                region: self.region,
                _owner: owner,
            }),
            range: self.range,
        }
    }

    /// A checked child range; both the offset and length are portable integers.
    /// Alignment describes the archive layout, not a cast to a Rust heap type.
    pub fn range(&self, offset: u64, len: u64, alignment: u64) -> Option<Self> {
        let absolute = (self.range.start as u64).checked_add(offset)?;
        if alignment == 0 || !alignment.is_power_of_two() || absolute % alignment != 0 {
            return None;
        }
        let start = usize::try_from(offset).ok()?;
        let end = usize::try_from(offset.checked_add(len)?).ok()?;
        if end > self.len() {
            return None;
        }
        Some(Self {
            region: self.region.clone(),
            range: self.range.start.checked_add(start)?..self.range.start.checked_add(end)?,
        })
    }
}

/// One physical store. A namespace is an authority boundary for associations,
/// not part of content addressing: the same immutable bytes may be shared.
#[derive(Clone)]
pub struct Store {
    root: PathBuf,
    durability: Durability,
    packed: Arc<std::sync::Mutex<pack::State>>,
    statistics: Arc<std::sync::OnceLock<Arc<Statistics>>>,
    regions: Arc<std::sync::Mutex<RegionCache>>,
    collection_cap: Arc<AtomicU64>,
    cache_cap: Arc<AtomicU64>,
    disk_estimate: Arc<AtomicU64>,
}
const REGION_BYTES: usize = 32 << 20;
const REGION_COUNT: usize = 64;
#[derive(Default)]
struct RegionCache {
    active: bool,
    scope: Option<Arc<()>>,
    bytes: usize,
    entries: std::collections::BTreeMap<PathBuf, Arc<Region>>,
}
impl RegionCache {
    fn reset(&mut self, active: bool) {
        self.active = active;
        self.scope = None;
        self.entries.clear();
        self.bytes = 0;
    }
    fn insert(&mut self, path: PathBuf, region: Arc<Region>) {
        let len = region.bytes().len();
        if len > REGION_BYTES {
            return;
        }
        while self.entries.len() >= REGION_COUNT || self.bytes + len > REGION_BYTES {
            let Some((_, evicted)) = self.entries.pop_first() else {
                return;
            };
            self.bytes -= evicted.bytes().len();
        }
        if let Some(previous) = self.entries.insert(path, region) {
            self.bytes -= previous.bytes().len();
        }
        self.bytes += len;
    }
}
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Durability {
    Strict,
    Rebuildable,
}
impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}
impl Contents for Store {
    fn get_content(&self, reference: Reference) -> Result<Bytes, ContentReadError> {
        checked_container(self.raw_container(reference)?, reference)
            .ok_or(ContentReadError::InvalidContent)
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        self.put(domain, bytes).ok()
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GcReport {
    pub entries: u64,
    pub bytes: u64,
    pub removed: u64,
    pub removed_bytes: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Association {
    pub action: Action,
    pub reference: Reference,
}
/// A routing association and its already-open output container. Integrity is
/// checked by `get`; query actions and verifier authority remain caller-owned.
pub struct OpenedAssociation {
    pub association: Association,
    container: Bytes,
}
impl OpenedAssociation {
    pub fn get(self) -> Option<Bytes> {
        checked_container(self.container, self.association.reference)
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid artifact store record")
}
fn hex(hash: ContentHash) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in hash.0 {
        output.push(HEX[usize::from(byte >> 4)] as char);
        output.push(HEX[usize::from(byte & 0xf)] as char);
    }
    output
}
fn number(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

impl Store {
    /// Start an independent operation over the same physical namespace. Only
    /// clones of this handle join its epochs and pins. Independent operations
    /// share capacity policy and counters, never finish or abort authority.
    pub fn for_operation(&self) -> Self {
        Self {
            packed: Arc::default(),
            regions: Arc::default(),
            ..self.clone()
        }
    }
    pub fn new(root: PathBuf) -> Self {
        Self::with_durability(root, Durability::Strict)
    }
    /// A derivable cache: complete writes and atomic replacement, without file
    /// or directory sync. Losing any publication must be a checked cache miss.
    /// Code distribution and sole-copy storage must use `new` instead.
    pub fn rebuildable(root: PathBuf) -> Self {
        Self::with_durability(root, Durability::Rebuildable)
    }
    fn with_durability(root: PathBuf, durability: Durability) -> Self {
        Self {
            root,
            durability,
            packed: Arc::default(),
            statistics: Arc::default(),
            regions: Arc::default(),
            collection_cap: Arc::new(AtomicU64::new(u64::MAX)),
            cache_cap: Arc::new(AtomicU64::new(u64::MAX)),
            disk_estimate: Arc::new(AtomicU64::new(u64::MAX)),
        }
    }
    /// Opt into counters on this physical owner and all of its existing clones.
    /// Normal providers do not allocate or update counters.
    pub fn enable_statistics(&self) -> Arc<Statistics> {
        self.statistics
            .get_or_init(|| Arc::new(Statistics::new()))
            .clone()
    }
    pub fn statistics(&self) -> Option<Arc<Statistics>> {
        self.statistics.get().cloned()
    }
    fn count(&self, counter: Counter, value: u64) {
        if let Some(statistics) = self.statistics.get() {
            statistics.add(counter, value);
        }
    }
    fn open_region(&self, path: &Path, kind: Counter) -> io::Result<Arc<Region>> {
        self.load_region(path, kind, None, false, || Region::open(path, false))
    }
    fn open_verification_region(&self, path: &Path, kind: Counter) -> io::Result<Arc<Region>> {
        self.load_region(path, kind, None, true, || Region::open(path, true))
    }
    fn load_region(
        &self,
        path: &Path,
        kind: Counter,
        #[cfg(unix)] pinned: Option<(&Arc<File>, &Arc<()>)>,
        #[cfg(not(unix))] _pinned: Option<(&Arc<File>, &Arc<()>)>,
        require_file: bool,
        load: impl FnOnce() -> io::Result<Region>,
    ) -> io::Result<Arc<Region>> {
        let mut cache = self.regions.lock().unwrap_or_else(|e| e.into_inner());
        // Index generations retain their own backing. Payload paths are
        // immutable during a scope; a later scope starts with fresh handles.
        #[cfg(unix)]
        let same_scope = pinned.is_none_or(|(_, scope)| {
            cache
                .scope
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(scope, active))
        });
        #[cfg(not(unix))]
        let same_scope = true;
        let retain = cache.active && same_scope && !matches!(kind, Counter::IndexOpens);
        if retain && let Some(region) = cache.entries.get(path) {
            #[cfg(unix)]
            let same_file = (!require_file || region.file().is_some())
                && pinned.is_none_or(|(file, _)| {
                    region
                        .file()
                        .is_some_and(|actual| Arc::ptr_eq(file, actual))
                });
            #[cfg(not(unix))]
            let same_file = {
                let _ = require_file;
                true
            };
            if same_file {
                self.count(Counter::RegionCacheHits, 1);
                return Ok(region.clone());
            }
        }
        self.count(kind, 1);
        let mut region = Arc::new(load()?);
        let bytes = region.bytes().len() as u64;
        if let Some(statistics) = self.statistics.get() {
            statistics.add(Counter::RegionLoads, 1);
            statistics.add(Counter::RegionBytesLoaded, bytes);
            region = Arc::new(Region::Retained {
                region,
                _owner: Arc::new(statistics::Backing::new(statistics.clone(), bytes)),
            });
        }
        if !matches!(kind, Counter::IndexOpens) {
            region = Arc::new(Region::Checked(Box::new(CheckedRegion {
                region,
                records: std::array::from_fn(|_| std::sync::OnceLock::new()),
                statistics: self.statistics.get().cloned(),
            })));
        }
        if retain {
            cache.insert(path.to_owned(), region.clone());
            if let Some(statistics) = self.statistics.get() {
                statistics.peak(Counter::RetainedRegionPeakBytes, cache.bytes as u64);
                statistics.peak(Counter::RetainedRegionPeakCount, cache.entries.len() as u64);
            }
        }
        Ok(region)
    }
    fn publish(&self, path: &Path, parts: &[&[u8]]) -> io::Result<()> {
        self.count(Counter::PublicationAttempts, 1);
        let result = publish(
            path,
            parts,
            self.durability,
            self.statistics.get().map(Arc::as_ref),
        );
        if result.is_ok() {
            self.record_growth(parts.iter().map(|part| part.len() as u64).sum());
        }
        if result.is_err() {
            self.count(Counter::SkippedPersistence, 1);
        }
        result
    }
    fn resident_equals(&self, path: &Path, parts: &[&[u8]]) -> bool {
        let cache = self.regions.lock().unwrap_or_else(|e| e.into_inner());
        let Some(region) = cache.entries.get(path).filter(|_| cache.active) else {
            return false;
        };
        let mut remaining = region.bytes();
        for part in parts {
            if !remaining.starts_with(part) {
                return false;
            }
            remaining = &remaining[part.len()..];
        }
        remaining.is_empty()
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn output_path(&self, reference: Reference) -> PathBuf {
        if let Some(path) = self.packed_path(0, reference.output.0) {
            path
        } else if reference.len <= pack::SMALL as u64 {
            self.root.join("missing-output")
        } else {
            self.private_output_path(reference.output.0)
                .unwrap_or_else(|| self.path(reference.output.0, "outputs"))
        }
    }
    /// The immutable backing segment for an association, for local corruption
    /// quarantine. A segment may hold several records; dropping it evicts them.
    pub fn association_path(&self, address: Address) -> Option<PathBuf> {
        self.packed_path(1, address.0)
    }
    /// Cache recency is eviction policy, never source freshness or proof.
    pub fn touch(&self, _address: Address, _reference: Reference) {
        // Recency is per immutable segment publication. Per-record filesystem
        // touches would recreate the small-record syscall amplification.
    }
    fn path(&self, hash: ContentHash, kind: &str) -> PathBuf {
        self.count(Counter::PathBuilds, 1);
        let hex = hex(hash);
        self.root.join(kind).join(&hex[..2]).join(hex)
    }
    /// Publish a bounded immutable output. No action/verification claim is
    /// accepted here: imported content gets the same integrity checks.
    pub fn put(&self, domain: Domain, bytes: &[u8]) -> io::Result<Reference> {
        if bytes.len() > MAX_RECORD {
            return Err(invalid());
        }
        let reference = Reference {
            output: domain.output(bytes),
            domain,
            len: bytes.len() as u64,
        };
        let mut header = Vec::with_capacity(HEADER);
        header.extend(MAGIC);
        header.extend(domain.kind.to_le_bytes());
        header.extend(domain.schema.to_le_bytes());
        header.extend(reference.len.to_le_bytes());
        header.extend(reference.output.0.0);
        header.extend([0; 8]);
        if bytes.len() <= pack::SMALL {
            header.extend(bytes);
            self.packed_put(0, reference.output.0, header.into())?;
        } else {
            let path = self.path(reference.output.0, "outputs");
            if self.resident_equals(&path, &[&header, bytes]) {
                self.count(Counter::IdenticalUpdates, 1);
            } else {
                self.put_large(reference, &[&header, bytes])?;
            }
        }
        Ok(reference)
    }
    /// Map a demanded record and validate its complete content at this boundary.
    /// Mapping alone reads no payload pages; integrity checking necessarily does.
    pub fn get(&self, reference: Reference) -> Option<Bytes> {
        Contents::get_content(self, reference).ok()
    }
    /// Lease a standalone provider inode under a caller-owned private path.
    /// Small packed records and cross-filesystem links use the caller's spool.
    /// Rust 1.97.1 std fs.rs:2837-2880 hard_link retains the inode without copying it;
    /// opening the link before checking binds validation to the retained inode.
    pub fn hold_content(&self, reference: Reference, path: PathBuf) -> Option<HeldContent> {
        if reference.len <= pack::SMALL as u64 || reference.len > MAX_RECORD as u64 {
            return None;
        }
        #[cfg(unix)]
        {
            fs::hard_link(self.output_path(reference), &path).ok()?;
            let held = HeldContent { path, reference };
            if !fs::symlink_metadata(&held.path).ok()?.is_file() {
                return None;
            }
            let file = File::open(&held.path).ok()?;
            let len = usize::try_from(reference.len).ok()?.checked_add(HEADER)?;
            if file.metadata().ok()?.len() != len as u64 {
                return None;
            }
            checked_content(
                Arc::new(Region::map_file(Arc::new(file), len).ok()?),
                reference,
            )?;
            Some(held)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
    /// Copy an untrusted container before checking it. Consumers needing an
    /// owned immutable snapshot can validate that snapshot once, rather than
    /// checking a mapping and then checking its copied payload again.
    pub fn get_owned(&self, reference: Reference) -> Result<Bytes, ContentReadError> {
        let bytes = self.raw_container(reference)?;
        self.count(Counter::ContentChecks, 1);
        if reference.len > MAX_RECORD as u64
            || usize::try_from(reference.len)
                .ok()
                .and_then(|len| len.checked_add(HEADER))
                != Some(bytes.len())
        {
            return Err(ContentReadError::InvalidContent);
        }
        checked_container(Bytes::from(bytes.to_vec()), reference)
            .ok_or(ContentReadError::InvalidContent)
    }
    fn raw_container(&self, reference: Reference) -> Result<Bytes, ContentReadError> {
        if reference.len <= pack::SMALL as u64 {
            return self.packed_get(0, reference.output.0);
        }
        let path = self
            .private_output_path(reference.output.0)
            .unwrap_or_else(|| self.path(reference.output.0, "outputs"));
        match self.open_region(&path, Counter::StandaloneOpens) {
            Ok(region) => {
                let len = region.bytes().len();
                Ok(Bytes {
                    region,
                    range: 0..len,
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.packed_get(0, reference.output.0)
            }
            Err(error) => Err(ContentReadError::Unreadable(error)),
        }
    }
    /// A checked ordinary container, including its header, for embedding the
    /// same representation in a toolchain build. No second codec is involved.
    pub fn container(&self, reference: Reference) -> Option<Bytes> {
        let bytes = self.raw_container(reference).ok()?;
        checked_container(bytes.clone(), reference)?;
        Some(bytes)
    }

    /// Read a previous association before demanding its output bytes. The query
    /// validates its recorded reads/action before calling get(reference).
    pub fn lookup(&self, address: Address) -> Option<Association> {
        self.lookup_open(address).map(|opened| opened.association)
    }
    /// Check an owned association snapshot. Its output is demanded separately
    /// by complete reference, so no mutable output view crosses this boundary.
    pub fn lookup_owned(&self, address: Address) -> Option<Association> {
        let bytes = self.packed_get(1, address.0).ok()?;
        if bytes.len() != ROOT_LEN {
            return None;
        }
        checked_association(&bytes.to_vec(), address)
    }
    /// Carry the same opened output through action checking to consumption,
    /// avoiding an output reopen between association lookup and validation.
    pub fn lookup_open(&self, address: Address) -> Option<OpenedAssociation> {
        let bytes = self.packed_get(1, address.0).ok()?;
        let association = checked_association(&bytes, address)?;
        self.raw_container(association.reference)
            .ok()
            .filter(|bytes| {
                bytes.len() as u64
                    == association
                        .reference
                        .len
                        .checked_add(HEADER as u64)
                        .unwrap_or(u64::MAX)
            })
            .map(|container| OpenedAssociation {
                association,
                container,
            })
    }
    /// Atomically expose an association after every referenced output exists.
    /// Callers own verification authority and must not publish imported claims.
    pub fn associate(
        &self,
        address: Address,
        action: Action,
        reference: Reference,
    ) -> io::Result<()> {
        if self.raw_container(reference).map_or(true, |bytes| {
            bytes.len() as u64 != reference.len.checked_add(HEADER as u64).unwrap_or(u64::MAX)
        }) {
            return Err(invalid());
        }
        let mut bytes = Vec::with_capacity(ROOT_LEN);
        bytes.extend(ROOT);
        bytes.extend(address.0.0);
        bytes.extend(action.0.0);
        bytes.extend(reference.output.0.0);
        bytes.extend(reference.domain.kind.to_le_bytes());
        bytes.extend(reference.domain.schema.to_le_bytes());
        bytes.extend(reference.len.to_le_bytes());
        bytes.extend(ContentHash::of_parts("hirsel.artifact.association.v1", [&bytes[..]]).0);
        self.packed_put(1, address.0, bytes.into())
    }
    /// Bounded collection scratch. A cache may evict referenced content: a later
    /// lookup becomes a miss, while existing mappings keep their inode alive.
    /// Schedule upkeep without flushing a producer or stealing live staging.
    /// The outer finished/aborted scope performs one attempt after releasing
    /// its publication state. No scope means collection can run immediately.
    pub fn schedule_collection(&self, cap: u64) {
        self.collection_cap.fetch_min(cap, Ordering::Relaxed);
        let idle = self.packed.lock().unwrap_or_else(|e| e.into_inner()).depth == 0;
        if idle {
            self.run_scheduled_collection();
        }
    }
    /// Configure this physical cache owner's retained-disk budget. Payload,
    /// index and maintenance writes share its accounting; clones share policy.
    /// Actual collection waits for a safe owner boundary.
    pub fn set_cache_cap(&self, cap: u64) {
        self.cache_cap.store(cap, Ordering::Relaxed);
        self.collection_cap.fetch_min(cap, Ordering::Relaxed);
    }
    fn record_growth(&self, bytes: u64) {
        self.count(Counter::PublicationAdmissionBytes, bytes);
        let estimate = self
            .disk_estimate
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                Some(old.saturating_add(bytes))
            })
            .unwrap_or_else(|old| old)
            .saturating_add(bytes);
        let cap = self.cache_cap.load(Ordering::Relaxed);
        if cap != u64::MAX && estimate > cap {
            self.collection_cap.fetch_min(cap, Ordering::Relaxed);
        }
    }
    fn run_scheduled_collection(&self) {
        let cap = self.collection_cap.swap(u64::MAX, Ordering::Relaxed);
        if cap != u64::MAX && self.try_collect(cap).is_none() {
            self.collection_cap.fetch_min(cap, Ordering::Relaxed);
        }
    }
    /// Explicit collection never commits pending work. Missing cache files
    /// remain permissible misses; held byte views retain their backing.
    pub fn collect(&self, cap: u64) -> GcReport {
        self.try_collect(cap).unwrap_or_default()
    }
    fn try_collect(&self, cap: u64) -> Option<GcReport> {
        use std::collections::BinaryHeap;
        use std::time::SystemTime;
        const BATCH: usize = 1024;
        // Collection cannot publish another scope's pending writes. A live
        // lease also protects payloads already installed but not yet indexed.
        let Ok(_publication) = pack::Lock::try_root(&self.root, true) else {
            return None;
        };
        if self.staging_is_live() {
            return None;
        }
        let mut report = GcReport::default();
        loop {
            self.count(Counter::GcScans, 1);
            let mut oldest: BinaryHeap<(SystemTime, u64, PathBuf)> = BinaryHeap::new();
            let mut consider = |file: &fs::DirEntry, modified, len| {
                // A max heap holds the oldest bounded batch. Younger entries
                // cannot enter it; avoid allocating their paths or repairing
                // the heap. Equal timestamps/sizes still compare full paths.
                if oldest.len() == BATCH
                    && oldest
                        .peek()
                        .is_some_and(|(time, bytes, _)| (modified, len) > (*time, *bytes))
                {
                    return;
                }
                oldest.push((modified, len, file.path()));
                if oldest.len() > BATCH {
                    oldest.pop();
                }
            };
            let mut entries = 0;
            let mut total = 0u64;
            for kind in ["packs", "outputs", "actions"] {
                for shard in fs::read_dir(self.root.join(kind))
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    if !shard.file_type().is_ok_and(|kind| kind.is_dir()) {
                        continue;
                    }
                    for file in fs::read_dir(shard.path()).into_iter().flatten().flatten() {
                        if !file.file_type().is_ok_and(|kind| kind.is_file()) {
                            continue;
                        }
                        let Ok(meta) = file.metadata() else { continue };
                        if !meta.is_file() || file.file_name().to_string_lossy().starts_with('.') {
                            continue;
                        }
                        entries += 1;
                        self.count(Counter::GcEntries, 1);
                        total = total.saturating_add(meta.len());
                        consider(
                            &file,
                            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                            meta.len(),
                        );
                    }
                }
            }
            let protected: Vec<_> = index::current_layer_ids_locked(&self.root)
                .ok()?
                .into_iter()
                .map(|id| format!("L-{}", hex(id)))
                .collect();
            for file in fs::read_dir(self.root.join("index"))
                .into_iter()
                .flatten()
                .flatten()
            {
                let name = file.file_name().to_string_lossy().into_owned();
                if name != "MANIFEST" && !name.starts_with("L-") {
                    continue;
                }
                let Ok(meta) = file.metadata() else {
                    continue;
                };
                if !meta.is_file() {
                    continue;
                }
                entries += 1;
                self.count(Counter::GcEntries, 1);
                total = total.saturating_add(meta.len());
                // Current layer names remain protected until an explicit root
                // eviction. Next bounded scan can then reclaim those orphans.
                if protected.contains(&name) {
                    continue;
                }
                consider(
                    &file,
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    meta.len(),
                );
            }
            if report.entries == 0 {
                report.entries = entries;
                report.bytes = total;
            }
            if total <= cap {
                self.disk_estimate.store(total, Ordering::Relaxed);
                return Some(report);
            }
            let before = total;
            for (_, len, path) in oldest.into_sorted_vec() {
                if total <= cap {
                    break;
                }
                if fs::remove_file(path).is_ok() {
                    self.count(Counter::GcUnlinks, 1);
                    total = total.saturating_sub(len);
                    report.removed += 1;
                    report.removed_bytes += len;
                }
            }
            if total == before {
                self.disk_estimate.store(total, Ordering::Relaxed);
                return Some(report);
            }
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IoEvent {
    FileSync,
    Rename,
    DirectorySync,
}
#[cfg(test)]
thread_local! {
    static IO_JOURNAL: std::cell::RefCell<Vec<(IoEvent, PathBuf)>> = const { std::cell::RefCell::new(Vec::new()) };
}
#[cfg(test)]
fn journal(event: IoEvent, path: &Path) {
    IO_JOURNAL.with(|journal| journal.borrow_mut().push((event, path.to_path_buf())));
}
/// Drains the test-only durable-publication journal recorded on this thread.
#[cfg(test)]
pub(crate) fn io_journal() -> Vec<(IoEvent, PathBuf)> {
    IO_JOURNAL.with(|journal| std::mem::take(&mut *journal.borrow_mut()))
}

/// The durable commit every Strict publication shares: sync the fully written
/// `staged` inode, rename it over `path`, then sync the parent directory. Data
/// before the committed name; the name before any later barrier.
fn commit_staged(
    staged: &Path,
    path: &Path,
    durability: Durability,
    statistics: Option<&Statistics>,
) -> io::Result<()> {
    let parent = path.parent().ok_or_else(invalid)?;
    if durability == Durability::Strict {
        if let Some(statistics) = statistics {
            statistics.add(Counter::FileSyncs, 1);
        }
        File::open(staged)?.sync_all()?;
        #[cfg(test)]
        journal(IoEvent::FileSync, path);
    }
    fs::rename(staged, path)?;
    #[cfg(test)]
    journal(IoEvent::Rename, path);
    if let Some(statistics) = statistics {
        statistics.add(Counter::Publications, 1);
    }
    if durability == Durability::Strict {
        if let Some(statistics) = statistics {
            statistics.add(Counter::DirectorySyncs, 1);
        }
        File::open(parent)?.sync_all()?;
        #[cfg(test)]
        journal(IoEvent::DirectorySync, parent);
    }
    Ok(())
}

/// Publish through a fresh inode. Concurrent readers keep the prior immutable
/// inode, and a crash before rename cannot publish a partial root or chunk.
fn publish(
    path: &Path,
    parts: &[&[u8]],
    durability: Durability,
    statistics: Option<&Statistics>,
) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().ok_or_else(invalid)?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        for part in parts {
            file.write_all(part)?;
        }
        commit_staged(&temporary, path, durability, statistics)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests;

fn checked_content(region: Arc<Region>, reference: Reference) -> Option<Bytes> {
    let len = region.bytes().len();
    checked_container(
        Bytes {
            region,
            range: 0..len,
        },
        reference,
    )
}
fn checked_association(bytes: &[u8], address: Address) -> Option<Association> {
    if bytes.len() != ROOT_LEN
        || bytes[..8] != *ROOT
        || bytes[8..40] != address.0.0
        || ContentHash::of_parts("hirsel.artifact.association.v1", [&bytes[..120]]).0
            != bytes[120..]
    {
        return None;
    }
    Some(Association {
        action: Action(ContentHash(bytes[40..72].try_into().ok()?)),
        reference: Reference {
            output: Output(ContentHash(bytes[72..104].try_into().ok()?)),
            domain: Domain {
                kind: word(bytes, 104)?,
                schema: word(bytes, 108)?,
            },
            len: number(bytes, 112)?,
        },
    })
}
fn checked_container(bytes: Bytes, reference: Reference) -> Option<Bytes> {
    let checks = bytes.region.checks();
    let slot = checks
        .map(|checks| &checks.records[reference.output.0.0[0] as usize % checks.records.len()]);
    if let Some(checked) = slot.and_then(std::sync::OnceLock::get)
        && checked.range == bytes.range
        && checked.reference == reference
    {
        if let Some(statistics) = checks.and_then(|checks| checks.statistics.as_ref()) {
            statistics.add(Counter::ContentReuses, 1);
        }
        return bytes.range(HEADER as u64, reference.len, 1);
    }
    if let Some(statistics) = checks.and_then(|checks| checks.statistics.as_ref()) {
        statistics.add(Counter::ContentChecks, 1);
    }
    if reference.len > MAX_RECORD as u64
        || bytes.get(..8)? != MAGIC
        || word(&bytes, 8)? != reference.domain.kind
        || word(&bytes, 12)? != reference.domain.schema
        || number(&bytes, 16)? != reference.len
        || bytes.get(24..56)? != reference.output.0.0
        || bytes.get(56..64)? != [0; 8]
        || usize::try_from(reference.len).ok()?.checked_add(HEADER)? != bytes.len()
        || reference.domain.output(&bytes[HEADER..]) != reference.output
    {
        return None;
    }
    if let Some(slot) = slot {
        let _ = slot.set(CheckedContainer {
            range: bytes.range.clone(),
            reference,
        });
    }
    let payload = bytes.range(HEADER as u64, reference.len, 1)?;
    Some(if payload.has_reference(reference) {
        payload
    } else {
        payload.with_reference(reference)
    })
}
