//! Program records (B, native rerun design §1, §3, §7). One record per
//! program address (operation, package, profile, target) answers a check,
//! build or run whose key P equals the one it was published under. Records live in
//! the authenticated domain of the installation registration that bound the
//! operation's installed header: the association is MAC'd under that
//! registration's key and names its epoch, so a copied store, a store without
//! the registration, or a record made under a replaced registration misses.
//!
//! Layout under `<state>/<generation>/programs/`:
//! - `blobs/<hex>`: content, named by `ContentHash` of its bytes.
//! - `pins/<id>`: one publication's pin set, the digests of its whole closure.
//!   The publisher holds it exclusively locked until it commits or drops; a
//!   reader holds it shared while it reads the generation it selected.
//! - `addresses/<hex>`: the address's generation journal, one MAC'd
//!   association, replaced by one rename under `addresses/<hex>.lock`.
//! - `retiring/<id>`: pin sets an association no longer names.
//! - `gc.lock`: collection holds it exclusively; publication, lookup's lease
//!   acquisition and nothing else hold it shared.
//!
//! Deferred detection: a hit opens H and D, and a run hit R. The executable is
//! hashed as it is copied out; the absent A, O, I, E, M and X sections are never
//! read.
use super::installation::{
    Authority, check_private, hex, invalid, private_directory, private_read, random,
};
use hirsel_syntax::ContentHash;
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

/// The record operations. No operation's record answers another: the
/// operation is in the address and in H, and both are compared. A reader that
/// does not know an operation's byte reads the record as absent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramOperation {
    Check = 1,
    Build = 2,
    /// A build followed by its launch: the success holds the executable and R.
    Run = 3,
}
/// What a record holds: a complete source success, or a request that
/// completed with source diagnostics (no executable).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramKind {
    Success = 1,
    SourceDiagnostics = 2,
}
impl ProgramOperation {
    fn decode(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Check),
            2 => Some(Self::Build),
            3 => Some(Self::Run),
            _ => None,
        }
    }
    /// Whether a success of this operation holds an executable.
    fn executes(self) -> bool {
        matches!(self, Self::Build | Self::Run)
    }
}
impl ProgramKind {
    fn decode(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Success),
            2 => Some(Self::SourceDiagnostics),
            _ => None,
        }
    }
}

/// The frozen section directory (§7). An absent section is unavailable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Section {
    /// Header: association, kind and status (fixed layout, `HEADER_BYTES`).
    H = 0,
    /// Structured diagnostics, proof reports and output lines.
    D = 1,
    /// Acceptance records.
    A = 2,
    /// Frozen observations with environment.
    O = 3,
    /// Instance rows.
    I = 4,
    /// Edge, component and reverse-dependency rows.
    E = 5,
    /// Module rows.
    M = 6,
    /// Execution, link and whole-program table rows.
    X = 7,
    /// The execution descriptor, `hirsel.program.execution`: what a run
    /// record needs to bind and launch its executable without the build's
    /// products. Present exactly in a run success; a run record without it
    /// never answers a run. A reader that does not know it checks its
    /// directory entry and never reads it. The driver encodes and decodes
    /// it (`hirsel-driver` `program_record::Execution`); this crate checks
    /// its digest in H.
    ///
    /// Layout (little-endian; every length is a `u32`):
    /// - `u32` inner version (2; 1 was the reserved draft, never written);
    /// - 32 bytes: platform identity (`HandedPlatform` identity), equal to
    ///   the platform row of the record's key;
    /// - 32 bytes: the bound run's identity (`ValidatedRun::identity`), which
    ///   the linker embeds in the executable;
    /// - emulator: length and the path's raw Unix `OsStr` bytes (not
    ///   necessarily UTF-8); length 0 means the host runs it;
    /// - launch binding inputs: length and the JSON of the driver's `Facts`
    ///   (program digest, entry, `LaunchKind`, roots as Io 0, Args 1, Env 2,
    ///   Unsafe 3, Test 4, code reach);
    /// - unmodeled foreign symbol: length and `user\0extern`, or length 0;
    /// - observation anchors: length and the JSON of the driver's `Tables`
    ///   (cited source files, failure rules, site ranges, anchors, keyed by
    ///   `DefPath`).
    ///
    /// The two JSON parts are frozen with this layout: their serde shape
    /// includes `hirsel_run_schema::LaunchKind` and `hirsel_syntax::DefPath`.
    /// A change to `Facts`, `Tables` or either of those types' serialized
    /// form needs a new inner version.
    R = 8,
}
const SECTIONS: usize = 9;
const MAGIC: &[u8; 8] = b"HIRSELB\x01";
const VERSION: u32 = 1;
/// Magic, version, section count, then (tag, flags, offset, length) per section.
const DIRECTORY_BYTES: usize = 16 + SECTIONS * 24;
/// operation, kind, status, has-executable, has-image, 3 reserved; address,
/// key, epoch; executable digest and length; image binding; D..R digests.
pub const HEADER_BYTES: usize = 8 + 3 * 32 + 32 + 8 + 32 + (SECTIONS - 1) * 32;
const MAX_RECORD: u64 = 64 << 20;
const MAX_EXECUTABLE: u64 = 4 << 30;
const JOURNAL_MAGIC: &[u8; 8] = b"HIRSELJ\x01";
/// magic, generation, operation, kind, 6 reserved; address, key, epoch,
/// record digest, header digest, pin id, MAC.
const JOURNAL_BYTES: usize = 8 + 8 + 8 + 7 * 32;

/// H: the fixed header of one record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgramHeader {
    pub operation: ProgramOperation,
    pub kind: ProgramKind,
    /// The request's exit-status class.
    pub status: u8,
    pub address: ContentHash,
    pub key: ContentHash,
    pub epoch: ContentHash,
    /// The executable's digest and length, for a build or run success.
    pub executable: Option<(ContentHash, u64)>,
    /// `image_binding(executable, platform identity)` when this record's
    /// executable passed `validate_image` under that platform.
    pub image: Option<ContentHash>,
    /// Digests of D, A, O, I, E, M, X and R; zero when absent.
    sections: [ContentHash; SECTIONS - 1],
}
/// The binding of `validate_image`'s authenticated result in H.
pub fn image_binding(executable: ContentHash, platform: ContentHash) -> ContentHash {
    ContentHash::of_parts(
        "hirsel.program.image-validation.v1",
        [&executable.0[..], &platform.0[..]],
    )
}
fn section_digest(section: Section, bytes: &[u8]) -> ContentHash {
    ContentHash::of_parts("hirsel.program.section.v1", [&[section as u8][..], bytes])
}
impl ProgramHeader {
    fn encode(&self) -> [u8; HEADER_BYTES] {
        let mut out = [0; HEADER_BYTES];
        out[0] = self.operation as u8;
        out[1] = self.kind as u8;
        out[2] = self.status;
        out[3] = self.executable.is_some() as u8;
        out[4] = self.image.is_some() as u8;
        let mut at = 8;
        let mut put = |bytes: &[u8]| {
            out[at..at + bytes.len()].copy_from_slice(bytes);
            at += bytes.len();
        };
        put(&self.address.0);
        put(&self.key.0);
        put(&self.epoch.0);
        let (digest, len) = self.executable.unwrap_or_default();
        put(&digest.0);
        put(&len.to_le_bytes());
        put(&self.image.unwrap_or_default().0);
        for digest in &self.sections {
            put(&digest.0);
        }
        out
    }
    fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != HEADER_BYTES || bytes[3] > 1 || bytes[4] > 1 || bytes[5..8] != [0; 3] {
            return None;
        }
        let hash = |at: usize| ContentHash(bytes[at..at + 32].try_into().unwrap());
        let executable = (
            hash(8 + 96),
            u64::from_le_bytes(bytes[8 + 128..8 + 136].try_into().ok()?),
        );
        let mut sections = [ContentHash::default(); SECTIONS - 1];
        for (i, digest) in sections.iter_mut().enumerate() {
            *digest = hash(8 + 168 + 32 * i);
        }
        Some(Self {
            operation: ProgramOperation::decode(bytes[0])?,
            kind: ProgramKind::decode(bytes[1])?,
            status: bytes[2],
            address: hash(8),
            key: hash(40),
            epoch: hash(72),
            executable: (bytes[3] == 1).then_some(executable),
            image: (bytes[4] == 1).then(|| hash(8 + 136)),
            sections,
        })
    }
}

/// A complete record to publish: H's fields and the present sections.
pub struct ProgramRecord<'a> {
    pub operation: ProgramOperation,
    pub kind: ProgramKind,
    pub status: u8,
    pub address: ContentHash,
    pub key: ContentHash,
    pub image: Option<ContentHash>,
    pub diagnostics: &'a [u8],
    pub executable: Option<&'a [u8]>,
    /// R, for a run success.
    pub execution: Option<&'a [u8]>,
}

/// The authenticated domain of one operation: every registration it
/// selected. The records live under the first root's generation and are
/// MAC'd under its key; the epoch covers every selected registration, so
/// replacing any one of them advances it.
#[derive(Clone, Debug)]
pub struct ProgramAuthority {
    primary: Authority,
    epoch: ContentHash,
}
impl ProgramAuthority {
    pub fn new(mut authorities: Vec<Authority>) -> Option<Self> {
        authorities.sort_by(|a, b| a.root().cmp(b.root()));
        let parts = authorities
            .iter()
            .flat_map(|a| {
                [
                    a.root().as_os_str().as_encoded_bytes().to_vec(),
                    a.epoch().0.to_vec(),
                ]
            })
            .collect::<Vec<_>>();
        let epoch = ContentHash::of_parts(
            "hirsel.program.domain-epoch.v1",
            parts.iter().map(Vec::as_slice),
        );
        Some(Self {
            primary: authorities.into_iter().next()?,
            epoch,
        })
    }
    pub fn epoch(&self) -> ContentHash {
        self.epoch
    }
    /// Open (creating) this domain's record store.
    pub fn records(&self) -> io::Result<ProgramRecords> {
        let root = self.primary.directory.join("programs");
        // A directory this call creates is synced into its parent, so a
        // commit's names survive a power loss with the directories that hold them.
        for directory in ["", "blobs", "pins", "addresses", "retiring"] {
            let path = root.join(directory);
            let created = fs::symlink_metadata(&path).is_err();
            private_directory(&path)?;
            if created {
                sync_directory(path.parent().expect("a records directory has a parent"))?;
            }
        }
        Ok(ProgramRecords {
            root,
            authority: self.clone(),
        })
    }
}

/// One domain's record store.
pub struct ProgramRecords {
    root: PathBuf,
    authority: ProgramAuthority,
}

/// A lookup's outcome. A miss names the journal generation it saw, which is
/// the generation a publication of this address must replace (CAS).
pub enum Lookup {
    Hit(ProgramHit),
    Miss { generation: u64 },
}
/// A selected record, held under a reader lease on its pin set until drop.
pub struct ProgramHit {
    pub header: ProgramHeader,
    pub diagnostics: Vec<u8>,
    /// R, for a run hit.
    pub execution: Option<Vec<u8>>,
    executable: Option<PathBuf>,
    _lease: fs::File,
}
/// A publication's result. A loser changed nothing but its own pin set.
#[derive(Debug, Eq, PartialEq)]
pub enum Published {
    Committed { generation: u64 },
    Lost,
}

/// A point to stop publication at, as a crash there would (tests).
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Boundary {
    AfterPin,
    AfterContent,
    AfterJournalWrite,
    AfterCommit,
}
#[cfg(test)]
thread_local! {
    pub(super) static CRASH: std::cell::Cell<Option<Boundary>> = const { std::cell::Cell::new(None) };
}
macro_rules! crash_at {
    ($boundary:ident) => {
        #[cfg(test)]
        if CRASH.with(|crash| crash.get()) == Some(Boundary::$boundary) {
            return Err(io::Error::other(concat!("crash ", stringify!($boundary))));
        }
    };
}

#[derive(Clone, Copy)]
struct Association {
    generation: u64,
    operation: ProgramOperation,
    kind: ProgramKind,
    address: ContentHash,
    key: ContentHash,
    epoch: ContentHash,
    record: ContentHash,
    header: ContentHash,
    pin: [u8; 32],
}
impl ProgramRecords {
    pub fn epoch(&self) -> ContentHash {
        self.authority.epoch
    }
    fn journal(&self, address: ContentHash) -> PathBuf {
        self.root.join("addresses").join(hex(&address.0))
    }
    fn blob(&self, digest: ContentHash) -> PathBuf {
        self.root.join("blobs").join(hex(&digest.0))
    }
    fn pin(&self, id: &[u8; 32]) -> PathBuf {
        self.root.join("pins").join(hex(id))
    }
    fn gc_lock(&self) -> io::Result<fs::File> {
        super::installation::private_lock(&self.root.join("gc.lock"))
    }
    fn encode(&self, a: &Association) -> [u8; JOURNAL_BYTES] {
        let mut out = [0; JOURNAL_BYTES];
        out[..8].copy_from_slice(JOURNAL_MAGIC);
        out[8..16].copy_from_slice(&a.generation.to_le_bytes());
        out[16] = a.operation as u8;
        out[17] = a.kind as u8;
        let mut at = 24;
        for part in [
            &a.address.0,
            &a.key.0,
            &a.epoch.0,
            &a.record.0,
            &a.header.0,
            &a.pin,
        ] {
            out[at..at + 32].copy_from_slice(part);
            at += 32;
        }
        let tag = self.authority.primary.mac(&out[..at]);
        out[at..].copy_from_slice(&tag);
        out
    }
    /// The address's committed association, if its MAC holds under this
    /// domain's key. Anything else (absent, short, forged, foreign) is None.
    fn current(&self, address: ContentHash) -> Option<Association> {
        let bytes = private_read(&self.journal(address), JOURNAL_BYTES).ok()?;
        if bytes.len() != JOURNAL_BYTES || &bytes[..8] != JOURNAL_MAGIC || bytes[18..24] != [0; 6] {
            return None;
        }
        let body = JOURNAL_BYTES - 32;
        let tag = self.authority.primary.mac(&bytes[..body]);
        if tag
            .iter()
            .zip(&bytes[body..])
            .fold(0, |v, (a, b)| v | (a ^ b))
            != 0
        {
            return None;
        }
        let hash = |i: usize| ContentHash(bytes[24 + 32 * i..56 + 32 * i].try_into().unwrap());
        Some(Association {
            generation: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            operation: ProgramOperation::decode(bytes[16])?,
            kind: ProgramKind::decode(bytes[17])?,
            address: hash(0),
            key: hash(1),
            epoch: hash(2),
            record: hash(3),
            header: hash(4),
            pin: hash(5).0,
        })
    }
    /// Select `address`'s record if its association authenticates in this
    /// domain and epoch and `key(operation, kind)` is the key it was
    /// published under. A build success compares against full build P and a
    /// build failure against check-P; the caller's `key` decides. Every
    /// failure to read or check is a miss.
    pub fn lookup(
        &self,
        address: ContentHash,
        operation: ProgramOperation,
        key: impl FnOnce(ProgramKind) -> Option<ContentHash>,
    ) -> Lookup {
        let gc = match self
            .gc_lock()
            .and_then(|lock| lock.lock_shared().map(|()| lock))
        {
            Ok(lock) => lock,
            Err(_) => return Lookup::Miss { generation: 0 },
        };
        let Some(association) = self.current(address) else {
            // A journal that does not authenticate is replaced as if absent;
            // the CAS then compares against what the file holds now.
            return Lookup::Miss {
                generation: self.generation(address),
            };
        };
        let miss = Lookup::Miss {
            generation: association.generation,
        };
        if association.address != address
            || association.operation != operation
            || association.epoch != self.authority.epoch
            || key(association.kind) != Some(association.key)
        {
            return miss;
        }
        // The reader lease: the selected generation's pin set, shared, taken
        // while collection is excluded, so its closure outlives this hit.
        let Ok(lease) = fs::File::open(self.pin(&association.pin)) else {
            return miss;
        };
        if lease.lock_shared().is_err() {
            return miss;
        }
        drop(gc);
        match self.read(&association) {
            Some((header, diagnostics, execution)) => Lookup::Hit(ProgramHit {
                executable: header.executable.map(|(digest, _)| self.blob(digest)),
                header,
                diagnostics,
                execution,
                _lease: lease,
            }),
            None => miss,
        }
    }
    /// The journal generation an unauthenticated or absent journal counts as.
    fn generation(&self, address: ContentHash) -> u64 {
        private_read(&self.journal(address), JOURNAL_BYTES)
            .ok()
            .filter(|bytes| bytes.len() == JOURNAL_BYTES)
            .map_or(0, |bytes| {
                u64::from_le_bytes(bytes[8..16].try_into().unwrap())
            })
    }
    /// H, D and (for a run success) R of the associated record,
    /// bounds-checked against its directory; H must restate the association
    /// and D's and R's digests.
    #[allow(clippy::type_complexity)]
    fn read(&self, association: &Association) -> Option<(ProgramHeader, Vec<u8>, Option<Vec<u8>>)> {
        use std::os::unix::fs::FileExt;
        let path = self.blob(association.record);
        check_private(&path).ok()?;
        let file = fs::File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        if len > MAX_RECORD || len < DIRECTORY_BYTES as u64 {
            return None;
        }
        let mut directory = [0; DIRECTORY_BYTES];
        file.read_exact_at(&mut directory, 0).ok()?;
        if &directory[..8] != MAGIC
            || directory[8..12] != VERSION.to_le_bytes()
            || directory[12..16] != (SECTIONS as u32).to_le_bytes()
        {
            return None;
        }
        let entry = |section: Section| -> Option<Option<(u64, u64)>> {
            let at = 16 + section as usize * 24;
            let field =
                |i: usize| u64::from_le_bytes(directory[at + i..at + i + 8].try_into().unwrap());
            let tag = u32::from_le_bytes(directory[at..at + 4].try_into().unwrap());
            let flags = u32::from_le_bytes(directory[at + 4..at + 8].try_into().unwrap());
            let (offset, length) = (field(8), field(16));
            if tag != section as u32 || flags > 1 {
                return None;
            }
            if flags == 0 {
                return (offset == 0 && length == 0).then_some(None);
            }
            (offset >= DIRECTORY_BYTES as u64 && offset.checked_add(length)? <= len)
                .then_some(Some((offset, length)))
        };
        let section = |section: Section| -> Option<Vec<u8>> {
            let (offset, length) = entry(section)??;
            let mut bytes = vec![0; usize::try_from(length).ok()?];
            file.read_exact_at(&mut bytes, offset).ok()?;
            Some(bytes)
        };
        let h = section(Section::H)?;
        if section_digest(Section::H, &h) != association.header {
            return None;
        }
        let header = ProgramHeader::decode(&h)?;
        if header.operation != association.operation
            || header.kind != association.kind
            || header.address != association.address
            || header.key != association.key
            || header.epoch != association.epoch
            || header.executable.is_some()
                != (header.kind == ProgramKind::Success && header.operation.executes())
        {
            return None;
        }
        let d = section(Section::D)?;
        if section_digest(Section::D, &d) != header.sections[0] {
            return None;
        }
        // A run success answers only with its execution descriptor.
        let execution =
            if header.operation == ProgramOperation::Run && header.kind == ProgramKind::Success {
                let r = section(Section::R)?;
                if section_digest(Section::R, &r) != header.sections[Section::R as usize - 1] {
                    return None;
                }
                Some(r)
            } else {
                entry(Section::R)?;
                None
            };
        // Later lands' sections stay unread; each must still be well-formed.
        for later in [
            Section::A,
            Section::O,
            Section::I,
            Section::E,
            Section::M,
            Section::X,
        ] {
            entry(later)?;
        }
        Some((header, d, execution))
    }
    /// Publish `record` as `address`'s next generation if the journal still
    /// holds `expected` (the generation the caller's lookup saw): content,
    /// then the pin set, durably, then one commit.
    pub fn publish(&self, record: &ProgramRecord<'_>, expected: u64) -> io::Result<Published> {
        let executable = record
            .executable
            .map(|bytes| (ContentHash::of_bytes(bytes), bytes.len() as u64));
        if executable.is_some_and(|(_, len)| len > MAX_EXECUTABLE)
            || executable.is_some()
                != (record.kind == ProgramKind::Success && record.operation.executes())
            || record.execution.is_some()
                != (record.kind == ProgramKind::Success
                    && record.operation == ProgramOperation::Run)
            || (record.diagnostics.len() + record.execution.map_or(0, <[u8]>::len)) as u64
                > MAX_RECORD / 2
        {
            return Err(invalid());
        }
        let mut sections = [ContentHash::default(); SECTIONS - 1];
        sections[0] = section_digest(Section::D, record.diagnostics);
        if let Some(execution) = record.execution {
            sections[Section::R as usize - 1] = section_digest(Section::R, execution);
        }
        let header = ProgramHeader {
            operation: record.operation,
            kind: record.kind,
            status: record.status,
            address: record.address,
            key: record.key,
            epoch: self.authority.epoch,
            executable,
            image: record.image,
            sections,
        };
        let h = header.encode();
        let mut bytes = Vec::with_capacity(DIRECTORY_BYTES + h.len() + record.diagnostics.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&(SECTIONS as u32).to_le_bytes());
        let mut offset = DIRECTORY_BYTES as u64;
        for tag in 0..SECTIONS as u32 {
            let present = match tag {
                0 => Some(&h[..]),
                1 => Some(record.diagnostics),
                8 => record.execution,
                _ => None,
            };
            let length = present.map_or(0, |bytes| bytes.len() as u64);
            let present = present.is_some();
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&(present as u32).to_le_bytes());
            bytes.extend_from_slice(&(if present { offset } else { 0 }).to_le_bytes());
            bytes.extend_from_slice(&length.to_le_bytes());
            offset += length;
        }
        bytes.extend_from_slice(&h);
        bytes.extend_from_slice(record.diagnostics);
        bytes.extend_from_slice(record.execution.unwrap_or_default());
        let digest = ContentHash::of_bytes(&bytes);
        let association = Association {
            generation: expected.checked_add(1).ok_or_else(invalid)?,
            operation: record.operation,
            kind: record.kind,
            address: record.address,
            key: record.key,
            epoch: self.authority.epoch,
            record: digest,
            header: section_digest(Section::H, &h),
            pin: random()?,
        };
        let gc = self.gc_lock()?;
        gc.lock_shared()?;
        // (1) The pin set, the whole closure, before any content: collection
        // never sees this publication's content unpinned.
        let closure: Vec<ContentHash> = std::iter::once(digest)
            .chain(executable.map(|(digest, _)| digest))
            .collect();
        let pin = self.write_pin(&association.pin, &closure)?;
        crash_at!(AfterPin);
        // (2) Content, each durable before the commit can name it.
        self.write_blob(digest, &bytes)?;
        if let (Some((digest, _)), Some(executable)) = (executable, record.executable) {
            self.write_blob(digest, executable)?;
        }
        crash_at!(AfterContent);
        // (3) One CAS commit on the address's generation journal.
        let lock = super::installation::private_lock(
            &self.journal(record.address).with_extension("lock"),
        )?;
        lock.lock()?;
        let previous = self.current(record.address);
        let current = previous.map_or_else(|| self.generation(record.address), |a| a.generation);
        if current != expected {
            drop(pin);
            let _ = fs::remove_file(self.pin(&association.pin));
            return Ok(Published::Lost);
        }
        let journal = self.journal(record.address);
        let temporary = journal.with_extension(format!("{}.tmp", hex(&association.pin)));
        write_durable(&temporary, &self.encode(&association))?;
        crash_at!(AfterJournalWrite);
        fs::rename(&temporary, &journal)?;
        sync_directory(&self.root.join("addresses"))?;
        crash_at!(AfterCommit);
        // (4) The replaced generation retires; collection frees it once no
        // reader lease names it.
        if let Some(previous) = previous {
            write_durable(&self.root.join("retiring").join(hex(&previous.pin)), &[])?;
            sync_directory(&self.root.join("retiring"))?;
        }
        drop(pin);
        drop(gc);
        // (5) Collection, unless another process is publishing or selecting:
        // the replaced generation, once no reader holds it, and whatever an
        // abandoned publication left. A failed sweep keeps the commit.
        let _ = self.collect();
        Ok(Published::Committed {
            generation: association.generation,
        })
    }
    fn write_pin(&self, id: &[u8; 32], closure: &[ContentHash]) -> io::Result<fs::File> {
        let path = self.pin(id);
        let bytes: Vec<u8> = closure.iter().flat_map(|d| d.0).collect();
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        file.lock()?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        sync_directory(&self.root.join("pins"))?;
        Ok(file)
    }
    fn write_blob(&self, digest: ContentHash, bytes: &[u8]) -> io::Result<()> {
        let path = self.blob(digest);
        // Equal bytes are reused, but only once durable: the writer that
        // renamed them may not have synced them or `blobs/` yet.
        if let Ok(existing) = private_read(&path, bytes.len())
            && existing == bytes
        {
            fs::File::open(&path)?.sync_all()?;
            synced(&path);
            return sync_directory(&self.root.join("blobs"));
        }
        let temporary = path.with_extension(format!("{}.tmp", hex(&random()?)));
        write_durable(&temporary, bytes)?;
        fs::rename(&temporary, &path)?;
        sync_directory(&self.root.join("blobs"))
    }
    /// Free every pin set no committed association names and no publisher or
    /// reader holds, then every blob no remaining pin set names. Each
    /// committed publication runs it; it frees nothing while any publication
    /// or selection holds `gc.lock`.
    fn collect(&self) -> io::Result<()> {
        let gc = self.gc_lock()?;
        match gc.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => return Ok(()),
            Err(fs::TryLockError::Error(error)) => return Err(error),
        }
        let mut named = std::collections::BTreeSet::new();
        for entry in fs::read_dir(self.root.join("addresses"))? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".tmp") {
                let _ = fs::remove_file(entry.path());
                continue;
            }
            if name.len() != 64 {
                continue;
            }
            if let Some(association) =
                private_read(&entry.path(), JOURNAL_BYTES)
                    .ok()
                    .and_then(|_| {
                        let hash = ContentHash(
                            (0..32)
                                .map(|i| u8::from_str_radix(&name[2 * i..2 * i + 2], 16))
                                .collect::<Result<Vec<_>, _>>()
                                .ok()?
                                .try_into()
                                .ok()?,
                        );
                        self.current(hash)
                    })
            {
                named.insert(hex(&association.pin));
            }
        }
        let mut live = std::collections::BTreeSet::new();
        for entry in fs::read_dir(self.root.join("pins"))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let file = fs::File::open(entry.path())?;
            // Committed and current, or held by a publisher or reader: live.
            let free = !named.contains(&name) && file.try_lock().is_ok();
            if free {
                fs::remove_file(entry.path())?;
                let _ = fs::remove_file(self.root.join("retiring").join(&name));
                continue;
            }
            let mut bytes = vec![];
            (&file).take(64 << 10).read_to_end(&mut bytes)?;
            for digest in bytes.chunks_exact(32) {
                live.insert(hex(digest));
            }
        }
        for entry in fs::read_dir(self.root.join("retiring"))? {
            let entry = entry?;
            if !self.root.join("pins").join(entry.file_name()).exists() {
                fs::remove_file(entry.path())?;
            }
        }
        for entry in fs::read_dir(self.root.join("blobs"))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !live.contains(&name) {
                fs::remove_file(entry.path())?;
            }
        }
        sync_directory(&self.root.join("pins"))?;
        sync_directory(&self.root.join("blobs"))
    }
}
impl ProgramHit {
    /// Copy the recorded executable to `destination` with materialization's
    /// rules: an existing regular file with exactly these bytes and an
    /// execute bit is kept (its mtime too); otherwise the bytes are hashed
    /// against H as they are written to a temporary file in the same
    /// directory, made executable, synced and renamed into place, and the
    /// parent synced.
    pub fn materialize(&self, destination: &Path) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let (Some(source), Some((digest, length))) = (&self.executable, self.header.executable)
        else {
            return Err(invalid());
        };
        let copy = |from: &mut fs::File, to: &mut dyn Write| -> io::Result<bool> {
            let observed = ContentHash::of_stream("hirsel.text.v1", length, |hash| {
                let mut buffer = vec![0; 1 << 20];
                let mut left = length;
                while left > 0 {
                    let n = from.read(&mut buffer[..left.min(1 << 20) as usize])?;
                    if n == 0 {
                        return Err(io::ErrorKind::UnexpectedEof.into());
                    }
                    hash.write_all(&buffer[..n])?;
                    to.write_all(&buffer[..n])?;
                    left -= n as u64;
                }
                Ok(())
            });
            Ok(observed.ok() == Some(digest) && from.read(&mut [0])? == 0)
        };
        if let Ok(mut file) = fs::File::open(destination)
            && let Ok(meta) = file.metadata()
            && meta.is_file()
            && meta.len() == length
            && meta.permissions().mode() & 0o111 != 0
            && copy(&mut file, &mut io::sink())?
        {
            return Ok(());
        }
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(
            ".hirsel-executable-{}-{}.tmp",
            std::process::id(),
            hex(&random()?[..8])
        ));
        let result = (|| {
            let mut from = fs::File::open(source)?;
            let mut to = fs::File::options()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            if !copy(&mut from, &mut to)? {
                return Err(io::Error::other("recorded executable failed its digest"));
            }
            to.set_permissions(fs::Permissions::from_mode(0o755))?;
            to.sync_all()?;
            fs::rename(&temporary, destination)?;
            sync_directory(parent)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}
fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    super::installation::private_write(path, bytes)?;
    synced(path);
    Ok(())
}
fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()?;
    synced(path);
    Ok(())
}
#[cfg(test)]
thread_local! {
    /// Every path this thread synced, in order.
    pub(super) static SYNCS: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(vec![]) };
}
fn synced(_path: &Path) {
    #[cfg(test)]
    SYNCS.with(|syncs| syncs.borrow_mut().push(_path.to_owned()));
}

#[cfg(test)]
mod tests;
