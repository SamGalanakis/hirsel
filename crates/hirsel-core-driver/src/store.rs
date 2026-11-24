//! Content-addressed records in one envelope, committed by atomic rename.
//! Disk bytes are external input; a damaged record is a cache miss.
//!
//! Every byte a read uses is covered by an xxh3-64 checksum it verifies:
//! records and selections carry theirs in the frame, a bare object's sits
//! beside its name in the record that names it (`Bare`). SHA-256 is
//! computed once, at write, only to name content; keyed records, and the
//! objects and debug rows named from their keys, hash no bytes to be named.
//! Nothing is synced: a torn,
//! short, zero-filled or missing file after a power loss fails its check or
//! is absent, so it is a miss and is rebuilt.
use hirsel_core_ids::{ContentHash, encoding};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::hash::Hasher;
use std::io::{self, IoSlice, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use twox_hash::XxHash3_64;

#[path = "release_cache.rs"]
pub(crate) mod release_cache;

const MAGIC: &[u8; 8] = b"HCORE\0\0\0";
/// A body record's envelope: `BODY_MAGIC VERSION head_len ir_len head ir_hash
/// ir prefix_check ir_check address`. The address names everything before
/// the IR, and `ir_hash` names the IR; a head read checks the prefix alone,
/// a full read checks both parts.
const BODY_MAGIC: &[u8; 8] = b"HBODY\0\0\0";
/// `BODY_MAGIC`, `VERSION` and the two lengths.
const BODY_HEADER: usize = 28;
/// The domain of a core module's hash: exactly its canonical serialization,
/// framed as one part. A body record's IR part is hashed by it, and so is a
/// module no record carries.
const IR: &str = "hirsel.core-ir.v1";
// CoreIR signatures always encode their anchors in a body record, including an empty list.
const VERSION: u32 = 42;
pub(crate) const HEADER: usize = 12;
/// The payload length, the checksum of everything before it, and the address.
const TRAILER: usize = 48;
/// A body's two checksums and its address.
const BODY_TRAILER: usize = 48;
/// A selection's fixed part: the selected address and the checksum of the
/// address and the selection's name, which the file holds between them.
const SELECTION: usize = 40;
/// Temporaries, beside the records on the same filesystem (`Store::new`).
const TMP: &str = "tmp";
static NEXT: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
thread_local! {
    /// Every file this thread published: each is a create, write and rename.
    pub(crate) static WRITES: std::cell::RefCell<Vec<Durable>> = const { std::cell::RefCell::new(vec![]) };
}
/// Test crash points: a store root whose writer dies after its next `n`
/// renames. Every later publish into it fails before its rename and leaves
/// its temporary, as a killed writer's would.
#[cfg(test)]
pub(crate) static CRASH: std::sync::Mutex<Vec<(PathBuf, usize)>> = std::sync::Mutex::new(vec![]);
#[cfg(test)]
fn crashed(root: &Path) -> bool {
    let mut crash = CRASH.lock().unwrap();
    match crash.iter_mut().find(|(r, _)| r == root) {
        Some((_, 0)) => true,
        Some((_, n)) => {
            *n -= 1;
            false
        }
        None => false,
    }
}
#[cfg(test)]
thread_local! {
    /// Sweep the store once inside this thread's next temporary's create and
    /// lock, where another opener's sweep can fall: before the writer opens
    /// what it locks, or between that open and the lock.
    static SWEEP_BEFORE_LOCK: std::cell::Cell<Option<Window>> = const { std::cell::Cell::new(None) };
}
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Debug)]
enum Window {
    BeforeOpen,
    BeforeLock,
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Durable {
    Record,
    Bare,
    Selection,
}
fn wrote(_what: &str) {
    #[cfg(test)]
    WRITES.with_borrow_mut(|w| {
        w.push(match _what {
            "record" => Durable::Record,
            "bare" => Durable::Bare,
            _ => Durable::Selection,
        })
    });
}
/// The read check of every stored byte.
pub(crate) fn checksum(bytes: &[u8]) -> u64 {
    XxHash3_64::oneshot(bytes)
}
fn word(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}
/// A bare object's name and the checksum a read verifies: the record that
/// names the object carries both, so the object's own bytes stay the plain
/// file a linker reads.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Bare {
    pub address: ContentHash,
    pub check: u64,
}

/// Position in an archive payload, independent of its file envelope.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Record {
    pub start: u64,
    pub len: u64,
}
impl Record {
    pub fn bytes(self, archive: &[u8]) -> Option<&[u8]> {
        let start = usize::try_from(self.start).ok()?;
        let len = usize::try_from(self.len).ok()?;
        archive.get(start..start.checked_add(len)?)
    }
}
/// A verified body record: its whole file and the range of its IR. The
/// head is read by `Store::get_body_head`.
pub(crate) struct Body {
    bytes: Vec<u8>,
    ir: std::ops::Range<usize>,
    pub ir_hash: ContentHash,
}
impl Body {
    fn split(
        bytes: Vec<u8>,
        key: ContentHash,
        image: Option<&crate::meter::ImageRead>,
    ) -> Option<Self> {
        let size = bytes.len();
        if size < BODY_HEADER || &bytes[..8] != BODY_MAGIC || bytes[8..12] != VERSION.to_le_bytes()
        {
            return None;
        }
        let length = |at: usize| {
            usize::try_from(u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())).ok()
        };
        let (head_len, ir_len) = (length(12)?, length(20)?);
        let hash_at = BODY_HEADER.checked_add(head_len)?;
        let ir_at = hash_at.checked_add(32)?;
        let end = ir_at.checked_add(ir_len)?;
        if end.checked_add(BODY_TRAILER)? != size || bytes[end + 16..] != key.0 {
            return None;
        }
        let check = |at: usize, part: &[u8]| {
            let sum = || checksum(part);
            word(&bytes, at) == image.map_or_else(sum, |image| image.digest(part.len(), sum))
        };
        if !check(end, &bytes[..ir_at]) || !check(end + 8, &bytes[ir_at..end]) {
            return None;
        }
        let ir_hash = ContentHash(bytes[hash_at..ir_at].try_into().unwrap());
        Some(Self {
            bytes,
            ir: ir_at..end,
            ir_hash,
        })
    }
    pub fn ir(&self) -> &[u8] {
        &self.bytes[self.ir.clone()]
    }
}
/// A body record's head, read without its IR (`Store::get_body_head`).
pub(crate) struct Head {
    bytes: Vec<u8>,
    head: std::ops::Range<usize>,
    pub ir_hash: ContentHash,
}
impl Head {
    pub fn head(&self) -> &[u8] {
        &self.bytes[self.head.clone()]
    }
}
pub(crate) struct ArchiveWriter {
    // The binary output arena is appended once and written as one request batch.
    buffer: Vec<u8>,
}
impl Write for ArchiveWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buffer.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl ArchiveWriter {
    pub fn record<T: serde::Serialize>(&mut self, row: &T) -> io::Result<Record> {
        hirsel_core_front::cost_scope!("non-check", "archive binary encoding");
        let start = self.buffer.len() as u64;
        encoding::write_fixed(&mut self.buffer, row)?;
        Ok(Record {
            start,
            len: self.buffer.len() as u64 - start,
        })
    }
}

#[derive(Clone)]
pub struct Store {
    root: PathBuf,
    image: bool,
    release: Option<release_cache::Release>,
}

pub(crate) fn default_path(directory: &Path) -> PathBuf {
    directory.join(".hirsel-core")
}

/// A file in `<store>/tmp`, exclusively locked from its create until it is
/// renamed into place or removed. Opening a store removes every temporary it
/// can lock without blocking, so a crashed writer's leftovers go and a live
/// writer's stay.
struct Temporary {
    path: PathBuf,
    file: File,
}
fn lock(file: &File, flags: libc::c_int) -> io::Result<bool> {
    loop {
        // SAFETY: `file` owns an open descriptor for the whole call.
        if unsafe { libc::flock(file.as_raw_fd(), flags) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EWOULDBLOCK) => return Ok(false),
            _ => return Err(error),
        }
    }
}
/// True when `path` still names the inode `file` holds: a store opened
/// between a temporary's create and its lock may have removed it.
fn still_named(path: &Path, file: &File) -> io::Result<bool> {
    let held = file.metadata()?;
    Ok(match fs::symlink_metadata(path) {
        Ok(named) => named.ino() == held.ino() && named.dev() == held.dev(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    })
}
/// A scratch directory in `<store>/tmp`, locked like a `Temporary` and
/// removed with everything in it when dropped.
pub(crate) struct Scratch {
    path: PathBuf,
    _lock: File,
}
impl Scratch {
    pub fn path(&self) -> &Path {
        &self.path
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
impl Store {
    pub fn new(root: &Path) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let store = Self {
            root: root.to_owned(),
            image: false,
            release: None,
        };
        store.sweep()?;
        Ok(store)
    }
    pub(crate) fn image(root: &Path) -> io::Result<Self> {
        let mut store = Self::new(root)?;
        store.image = true;
        Ok(store)
    }
    /// Remove each temporary no live writer holds. `tmp` is normally empty or
    /// absent, so this is one directory read.
    fn sweep(&self) -> io::Result<()> {
        let entries = match fs::read_dir(self.root.join(TMP)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let path = entry?.path();
            let Ok(file) = File::open(&path) else {
                continue;
            };
            if !lock(&file, libc::LOCK_EX | libc::LOCK_NB)? || !still_named(&path, &file)? {
                continue;
            }
            let removed = if file.metadata()?.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            match removed {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }
    fn tmp_name(&self) -> PathBuf {
        self.root.join(TMP).join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }
    /// Create `make(path)` under `tmp` and lock what `open(path)` returns,
    /// retrying when a concurrent sweep removed it before the lock: a sweep
    /// may remove it before `open` finds it, or before the lock holds it.
    fn locked<T>(
        &self,
        make: impl Fn(&Path) -> io::Result<T>,
        open: impl Fn(&Path, &T) -> io::Result<File>,
    ) -> io::Result<(PathBuf, T, File)> {
        loop {
            let path = self.tmp_name();
            let made = match make(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir_all(self.root.join(TMP))?;
                    continue;
                }
                made => made?,
            };
            #[cfg(test)]
            let window = SWEEP_BEFORE_LOCK.take();
            #[cfg(test)]
            if window == Some(Window::BeforeOpen) {
                self.sweep()?;
            }
            let file = match open(&path, &made) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                file => file?,
            };
            #[cfg(test)]
            if window == Some(Window::BeforeLock) {
                self.sweep()?;
            }
            lock(&file, libc::LOCK_EX)?;
            if still_named(&path, &file)? {
                return Ok((path, made, file));
            }
        }
    }
    fn temporary(&self) -> io::Result<Temporary> {
        let (path, file, _) = self.locked(
            |path| OpenOptions::new().create_new(true).write(true).open(path),
            |_, file| file.try_clone(),
        )?;
        Ok(Temporary { path, file })
    }
    /// A locked scratch directory under `tmp`, removed when dropped.
    pub(crate) fn scratch(&self) -> io::Result<Scratch> {
        let (path, (), lock) =
            self.locked(|path| fs::create_dir(path), |path, ()| File::open(path))?;
        Ok(Scratch { path, _lock: lock })
    }
    /// Write the byte slices to a temporary and rename it to `path`. Nothing is
    /// synced (module documentation).
    fn publish<const N: usize>(&self, path: &Path, parts: [&[u8]; N]) -> io::Result<()> {
        self.publish_to(|file| {
            if let [bytes] = parts.as_slice() {
                file.write_all(bytes)?;
            } else {
                let mut buffers = parts.map(IoSlice::new);
                let mut remaining = buffers.as_mut_slice();
                while !remaining.is_empty() {
                    match file.write_vectored(remaining) {
                        Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                        Ok(written) => IoSlice::advance_slices(&mut remaining, written),
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => return Err(error),
                    }
                }
            }
            Ok((path, ()))
        })?;
        if let Some(release) = &self.release {
            release.wrote(parts.iter().map(|part| part.len()).sum());
        }
        Ok(())
    }
    /// Write a cache file to a locked temporary. The writer names the
    /// destination after it has written the payload.
    pub(crate) fn publish_to<P: AsRef<Path>, T>(
        &self,
        write: impl FnOnce(&mut File) -> io::Result<(P, T)>,
    ) -> io::Result<T> {
        let mut temporary = self.temporary()?;
        let written = write(&mut temporary.file);
        #[cfg(test)]
        if written.is_ok() && crashed(&self.root) {
            return Err(io::Error::other("the test writer crashed"));
        }
        let result = written.and_then(|(path, value)| {
            let path = path.as_ref();
            match (&self.release, fs::rename(&temporary.path, path)) {
                (Some(release), Err(e)) if e.kind() == io::ErrorKind::NotFound => {
                    release.create_dir(path.parent().expect("a release cache shard"))?;
                    fs::rename(&temporary.path, path)?;
                }
                (_, result) => result?,
            }
            Ok(value)
        });
        if result.is_err() {
            let _ = fs::remove_file(&temporary.path);
        }
        result
    }
    /// Whether `path` names a file of this store.
    /// The store's directory.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn holds(&self, path: &Path) -> bool {
        path.parent() == Some(&*self.root)
    }
    pub fn path(&self, key: ContentHash) -> PathBuf {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut name = [0; 64];
        for (i, byte) in key.0.into_iter().enumerate() {
            name[i * 2] = HEX[(byte >> 4) as usize];
            name[i * 2 + 1] = HEX[(byte & 15) as usize];
        }
        let name = std::str::from_utf8(&name).expect("hex digits are ASCII");
        if self.release.is_some() {
            self.root.join(&name[..2]).join(&name[2..])
        } else {
            self.root.join(name)
        }
    }
    pub fn put(&self, bytes: &[u8]) -> io::Result<ContentHash> {
        self.archive(|writer| writer.write_all(bytes))
    }
    /// Publish an ELF/object payload under `name`, which its caller derives
    /// from the key of the record that names it, never from its bytes. The
    /// file is passed directly to native linkers by path, so it carries no
    /// frame: its checksum travels in that record, and a file at the name
    /// whose bytes are not the record's fails the read check, a miss.
    /// The bytes always replace the file by a rename: a file already at the
    /// path may be one whose check just failed, and an existing name says
    /// nothing about its bytes.
    pub(crate) fn put_bare(&self, name: ContentHash, bytes: &[u8]) -> io::Result<Bare> {
        let bare = Bare {
            address: name,
            check: checksum(bytes),
        };
        self.publish(&self.path(bare.address), [bytes])?;
        wrote("bare");
        Ok(bare)
    }
    pub(crate) fn archive(
        &self,
        emit: impl FnOnce(&mut ArchiveWriter) -> io::Result<()>,
    ) -> io::Result<ContentHash> {
        let mut writer = ArchiveWriter { buffer: Vec::new() };
        emit(&mut writer)?;
        let (hash, bytes) = Self::frame(&writer.buffer);
        self.write_framed(hash, &bytes)?;
        Ok(hash)
    }
    /// The envelope of one payload and its address: the digest of the header,
    /// payload and length bytes, computed once while framing.
    pub(crate) fn frame(payload: &[u8]) -> (ContentHash, Vec<u8>) {
        let mut bytes = Vec::with_capacity(HEADER + payload.len() + TRAILER);
        bytes.extend_from_slice(&Self::header());
        bytes.extend_from_slice(payload);
        hirsel_core_front::cost_scope!("non-check", "archive write and digest");
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        let hash = ContentHash(Sha256::digest(&bytes).into());
        let check = checksum(&bytes);
        bytes.extend_from_slice(&check.to_le_bytes());
        bytes.extend_from_slice(&hash.0);
        (hash, bytes)
    }
    /// Publish `payload` framed under `key`, an address its caller derives
    /// from the payload's inputs, never from its bytes: the frame's checksum
    /// guards the bytes on every read and no digest of them is computed. A
    /// key's payload is a function of its inputs, so a rewrite replaces the
    /// file by a rename and a reader sees one whole frame or none. The
    /// frame is written around the borrowed payload without copying it.
    pub(crate) fn put_keyed(&self, key: ContentHash, payload: &[u8]) -> io::Result<()> {
        let header = Self::header();
        let mut trailer = [0; TRAILER];
        trailer[..8].copy_from_slice(&(payload.len() as u64).to_le_bytes());
        {
            hirsel_core_front::cost_scope!("non-check", "archive write and digest");
            let mut check = XxHash3_64::new();
            check.write(&header);
            check.write(payload);
            check.write(&trailer[..8]);
            trailer[8..16].copy_from_slice(&check.finish().to_le_bytes());
        }
        trailer[16..].copy_from_slice(&key.0);
        self.publish(&self.path(key), [&header, payload, &trailer])?;
        wrote("record");
        Ok(())
    }
    fn header() -> [u8; HEADER] {
        let mut header = [0; HEADER];
        header[..8].copy_from_slice(MAGIC);
        header[8..].copy_from_slice(&VERSION.to_le_bytes());
        header
    }
    /// The hash of exactly a module's serialization (`IR`).
    pub(crate) fn ir_hash(ir: &[u8]) -> ContentHash {
        hirsel_core_front::cost_scope!("non-check", "archive write and digest");
        ContentHash::of_parts(IR, [ir])
    }
    /// A body record: `head`, then the encoded IR `ir`. Returns
    /// its address, the IR's hash and the envelope.
    pub(crate) fn frame_body(head: &[u8], ir: &[u8]) -> (ContentHash, ContentHash, Vec<u8>) {
        let mut bytes = Vec::with_capacity(BODY_HEADER + head.len() + 32 + ir.len() + BODY_TRAILER);
        bytes.extend_from_slice(BODY_MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&(head.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(head);
        let hash_at = bytes.len();
        bytes.extend_from_slice(&[0; 32]);
        let ir_at = bytes.len();
        bytes.extend_from_slice(ir);
        let ir_hash = Self::ir_hash(&bytes[ir_at..]);
        let ir_len = (bytes.len() - ir_at) as u64;
        bytes[20..28].copy_from_slice(&ir_len.to_le_bytes());
        bytes[hash_at..ir_at].copy_from_slice(&ir_hash.0);
        hirsel_core_front::cost_scope!("non-check", "archive write and digest");
        let address = ContentHash(Sha256::digest(&bytes[..ir_at]).into());
        let checks = [checksum(&bytes[..ir_at]), checksum(&bytes[ir_at..])];
        for check in checks {
            bytes.extend_from_slice(&check.to_le_bytes());
        }
        bytes.extend_from_slice(&address.0);
        (address, ir_hash, bytes)
    }
    /// A body record read back under its address. Nothing escapes until both
    /// its checksums check; a short, long, foreign or damaged file is `None`.
    pub(crate) fn get_body(&self, key: ContentHash) -> io::Result<Option<Body>> {
        let image = crate::meter::ImageRead::start(self.image);
        let Some(bytes) = self.read_whole(key, image.as_ref())? else {
            return Ok(None);
        };
        if self.image {
            crate::meter::count(crate::meter::Count::ImageBodyBytes, bytes.len() as u64);
        }
        Ok(Body::split(bytes, key, image.as_ref()))
    }
    /// A body record's head and `ir_hash`, read under its address without
    /// its IR.
    ///
    /// A head read checks BODY_MAGIC, VERSION, both lengths, the exact file
    /// length, the trailer address and the prefix checksum over exactly the
    /// head bytes. It returns only fields the prefix covers, and nothing
    /// derived from IR. `get_body` alone checks the IR and decodes it.
    pub(crate) fn get_body_head(&self, key: ContentHash) -> io::Result<Option<Head>> {
        let image = crate::meter::ImageRead::start(self.image);
        let mut file = match File::open(self.path(key)) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        crate::meter::count(crate::meter::Count::FilesOpened, 1);
        if let Some(image) = &image {
            image.opened();
        }
        let size = file.metadata()?.len();
        let mut header = [0u8; BODY_HEADER];
        if size < BODY_HEADER as u64 || file.read_exact(&mut header).is_err() {
            return Ok(None);
        }
        if &header[..8] != BODY_MAGIC || header[8..12] != VERSION.to_le_bytes() {
            return Ok(None);
        }
        let (head_len, ir_len) = (word(&header, 12), word(&header, 20));
        let Some(prefix) = (BODY_HEADER as u64)
            .checked_add(head_len)
            .and_then(|n| n.checked_add(32))
        else {
            return Ok(None);
        };
        if prefix
            .checked_add(ir_len)
            .and_then(|n| n.checked_add(BODY_TRAILER as u64))
            != Some(size)
        {
            return Ok(None);
        }
        let Ok(prefix) = usize::try_from(prefix) else {
            return Ok(None);
        };
        let mut trailer = [0u8; BODY_TRAILER];
        file.seek(SeekFrom::Start(size - BODY_TRAILER as u64))?;
        file.read_exact(&mut trailer)?;
        if trailer[16..] != key.0 {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(prefix).map_err(io::Error::other)?;
        bytes.extend_from_slice(&header);
        bytes.resize(prefix, 0);
        file.seek(SeekFrom::Start(BODY_HEADER as u64))?;
        file.read_exact(&mut bytes[BODY_HEADER..])?;
        if let Some(image) = &image {
            image.bytes(BODY_TRAILER + (prefix - BODY_HEADER));
        }
        let sum = || checksum(&bytes);
        let check = match &image {
            Some(image) => image.digest(prefix, sum),
            None => sum(),
        };
        if check != word(&trailer, 0) {
            return Ok(None);
        }
        let ir_hash = ContentHash(bytes[prefix - 32..].try_into().unwrap());
        bytes.truncate(prefix - 32);
        Ok(Some(Head {
            bytes,
            head: BODY_HEADER..prefix - 32,
            ir_hash,
        }))
    }
    /// Remove a record whose IR failed its check behind a valid head, so the
    /// next request misses it and checks the body again.
    pub(crate) fn remove_damaged(&self, key: ContentHash) -> io::Result<()> {
        match fs::remove_file(self.path(key)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
    fn read_whole(
        &self,
        key: ContentHash,
        image: Option<&crate::meter::ImageRead>,
    ) -> io::Result<Option<Vec<u8>>> {
        let mut file = match File::open(self.path(key)) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        crate::meter::count(crate::meter::Count::FilesOpened, 1);
        if let Some(image) = image {
            image.opened();
        }
        let size = usize::try_from(file.metadata()?.len()).map_err(io::Error::other)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size).map_err(io::Error::other)?;
        file.read_to_end(&mut bytes)?;
        if self.release.is_some() && bytes.len() == size {
            release_cache::touch(&file);
        }
        if let Some(image) = image {
            image.bytes(bytes.len());
        }
        Ok((bytes.len() == size).then_some(bytes))
    }
    /// Publish one framed record under its address.
    pub(crate) fn write_framed(&self, hash: ContentHash, bytes: &[u8]) -> io::Result<()> {
        self.publish(&self.path(hash), [bytes])?;
        wrote("record");
        Ok(())
    }
    pub fn get(&self, key: ContentHash) -> io::Result<Option<Vec<u8>>> {
        let Some((mut bytes, end)) = self.checked(key)? else {
            return Ok(None);
        };
        bytes.truncate(end);
        bytes.drain(..HEADER);
        Ok(Some(bytes))
    }
    /// A record's payload in one shared copy, never moved within its read
    /// buffer first.
    pub(crate) fn get_shared(&self, key: ContentHash) -> io::Result<Option<Arc<[u8]>>> {
        Ok(self
            .checked(key)?
            .map(|(bytes, end)| Arc::from(&bytes[HEADER..end])))
    }
    /// A framed record's bytes when they pass their check, and where its
    /// payload ends.
    fn checked(&self, key: ContentHash) -> io::Result<Option<(Vec<u8>, usize)>> {
        let image = crate::meter::ImageRead::start(self.image);
        let Some(bytes) = self.read_whole(key, image.as_ref())? else {
            return Ok(None);
        };
        let size = bytes.len();
        if size < HEADER + TRAILER || &bytes[..8] != MAGIC || bytes[8..12] != VERSION.to_le_bytes()
        {
            return Ok(None);
        }
        let end = size - TRAILER;
        let sum = || checksum(&bytes[..end + 8]);
        if word(&bytes, end) != (end - HEADER) as u64
            || bytes[end + 16..] != key.0
            || word(&bytes, end + 8)
                != image
                    .as_ref()
                    .map_or_else(sum, |image| image.digest(end + 8, sum))
        {
            return Ok(None);
        }
        Ok(Some((bytes, end)))
    }
    /// A bare object's bytes when they pass their check.
    pub(crate) fn get_bare(&self, bare: Bare) -> io::Result<Option<Vec<u8>>> {
        let mut file = match File::open(self.path(bare.address)) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        crate::meter::count(crate::meter::Count::FilesOpened, 1);
        crate::meter::count(crate::meter::Count::ObjectsOpened, 1);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if checksum(&bytes) != bare.check {
            return Ok(None);
        }
        if self.release.is_some() {
            release_cache::touch(&file);
        }
        Ok(Some(bytes))
    }
    pub(crate) fn selection(&self, name: &str) -> io::Result<Option<ContentHash>> {
        let bytes = match fs::read(self.root.join(name)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        crate::meter::count(crate::meter::Count::FilesOpened, 1);
        let end = 32 + name.len();
        if bytes.len() != SELECTION + name.len()
            || &bytes[32..end] != name.as_bytes()
            || word(&bytes, end) != checksum(&bytes[..end])
        {
            return Ok(None);
        }
        Ok(Some(ContentHash(bytes[..32].try_into().unwrap())))
    }
    /// Select `address` under `name`. The file holds its own name, so a
    /// selection moved or copied under another name (another key's) is a
    /// miss, never a hit of the record selected for that other key.
    pub(crate) fn select(&self, name: &str, address: ContentHash) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(SELECTION + name.len());
        bytes.extend_from_slice(&address.0);
        bytes.extend_from_slice(name.as_bytes());
        let check = checksum(&bytes);
        bytes.extend_from_slice(&check.to_le_bytes());
        self.publish(&self.root.join(name), [&bytes])?;
        wrote("selection");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Store;
    /// Law: opening a store removes every temporary no writer holds, file or
    /// scratch directory, and keeps each one a live writer holds.
    #[test]
    fn opening_removes_leftover_temporaries_and_keeps_live_ones() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("tmp");
        let left = || {
            let mut names: Vec<_> = std::fs::read_dir(&tmp)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        };
        let store = Store::new(dir.path()).unwrap();
        let live = store.temporary().unwrap();
        let scratch = store.scratch().unwrap();
        std::fs::write(scratch.path().join("program"), b"linking").unwrap();
        // A crashed writer's file and scratch directory: nothing holds them.
        std::fs::write(tmp.join("1-0"), b"torn").unwrap();
        std::fs::create_dir(tmp.join("1-1")).unwrap();
        std::fs::write(tmp.join("1-1/entry.o"), b"").unwrap();
        Store::new(dir.path()).unwrap();
        let name = |p: &std::path::Path| p.file_name().unwrap().to_str().unwrap().to_owned();
        let mut live_names = vec![name(&live.path), name(scratch.path())];
        live_names.sort();
        assert_eq!(
            left(),
            live_names,
            "the live writer's temporaries alone stay"
        );
        assert!(scratch.path().join("program").exists());
        // Its writer gone without a rename, the temporary is a leftover.
        drop(live);
        drop(scratch);
        Store::new(dir.path()).unwrap();
        assert_eq!(left(), Vec::<String>::new());
    }

    /// Law: a sweep that falls between a temporary's create and its lock
    /// (another opener of the store) never fails the writer: in either
    /// window, before the writer opens what it locks or between that open
    /// and the lock, it gets a live temporary, file or scratch directory,
    /// which a later sweep keeps. The sweep removes only what it holds
    /// locked; the writer, once locked, checks the path still names what it
    /// locked and otherwise starts again. Bugs: the scratch directory,
    /// reopened by path, was gone (NotFound, seen as
    /// `parallel_builds_launch_at_once` failing in a shared store); a writer
    /// that skipped the check would hold an unlinked directory. Buck target:
    /// hirsel-core-driver unittest.
    #[test]
    fn a_sweep_before_the_lock_leaves_the_writer_a_live_temporary() {
        for window in [super::Window::BeforeOpen, super::Window::BeforeLock] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::new(dir.path()).unwrap();
            super::SWEEP_BEFORE_LOCK.set(Some(window));
            let scratch = store.scratch().unwrap();
            super::SWEEP_BEFORE_LOCK.set(Some(window));
            let file = store.temporary().unwrap();
            Store::new(dir.path()).unwrap();
            assert!(
                scratch.path().is_dir(),
                "{window:?}: the scratch directory is live"
            );
            assert!(file.path.is_file(), "{window:?}: the temporary is live");
        }
    }
}
