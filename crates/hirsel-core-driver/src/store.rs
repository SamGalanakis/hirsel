//! Content-addressed records in one envelope, committed by atomic rename.
//! Disk bytes are external input; a damaged record is a cache miss.
use hirsel_core_ids::ContentHash;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: &[u8; 8] = b"HCORE\0\0\0";
/// A body record's envelope: `BODY_MAGIC VERSION head_len ir_len head ir_hash
/// ir address`. The address covers everything before the IR; a read checks
/// the IR against `ir_hash`, so each IR byte is hashed once either way.
const BODY_MAGIC: &[u8; 8] = b"HBODY\0\0\0";
/// `BODY_MAGIC`, `VERSION` and the two lengths.
const BODY_HEADER: usize = 28;
/// The domain of a core module's hash: exactly its canonical serialization,
/// framed as one part. A body record's IR part is hashed by it, and so is a
/// module no record carries.
const IR: &str = "hirsel.core-ir.v1";
const VERSION: u32 = 29;
const HEADER: usize = 12;
const TRAILER: usize = 40;
static NEXT: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
thread_local! {
    /// Every durable write this thread made: each is a create, write, file
    /// sync and rename, or a directory sync.
    pub(crate) static WRITES: std::cell::RefCell<Vec<Durable>> = const { std::cell::RefCell::new(vec![]) };
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Durable {
    Record,
    Bare,
    Selection,
    DirectorySync,
}
fn wrote(_what: &str) {
    #[cfg(test)]
    WRITES.with_borrow_mut(|w| {
        w.push(match _what {
            "record" => Durable::Record,
            "bare" => Durable::Bare,
            "selection" => Durable::Selection,
            _ => Durable::DirectorySync,
        })
    });
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
/// A verified body record: its whole file and the ranges of its two parts.
pub(crate) struct Body {
    bytes: Vec<u8>,
    head: std::ops::Range<usize>,
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
        if end.checked_add(32)? != size || bytes[end..] != key.0 {
            return None;
        }
        let digest = || Sha256::digest(&bytes[..ir_at]);
        let address = match image {
            Some(image) => image.digest(ir_at, digest),
            None => digest(),
        };
        if address[..] != key.0 {
            return None;
        }
        let ir_hash = match image {
            Some(image) => image.digest(ir_len, || Store::ir_hash(&bytes[ir_at..end])),
            None => Store::ir_hash(&bytes[ir_at..end]),
        };
        if ir_hash.0[..] != bytes[hash_at..ir_at] {
            return None;
        }
        Some(Self {
            bytes,
            head: BODY_HEADER..hash_at,
            ir: ir_at..end,
            ir_hash,
        })
    }
    pub fn head(&self) -> &[u8] {
        &self.bytes[self.head.clone()]
    }
    pub fn ir(&self) -> &[u8] {
        &self.bytes[self.ir.clone()]
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
        // serialize_into visits the borrowed tables once. serialize() first counts
        // their encoded size and would add a complete second traversal.
        bincode::serialize_into(&mut self.buffer, row).map_err(io::Error::other)?;
        Ok(Record {
            start,
            len: self.buffer.len() as u64 - start,
        })
    }
}

pub struct Store {
    root: PathBuf,
    image: bool,
}
impl Store {
    pub fn new(root: &Path) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        Ok(Self {
            root: root.to_owned(),
            image: false,
        })
    }
    pub(crate) fn image(root: &Path) -> io::Result<Self> {
        let mut store = Self::new(root)?;
        store.image = true;
        Ok(store)
    }
    pub fn path(&self, key: ContentHash) -> PathBuf {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut name = [0; 64];
        for (i, byte) in key.0.into_iter().enumerate() {
            name[i * 2] = HEX[(byte >> 4) as usize];
            name[i * 2 + 1] = HEX[(byte & 15) as usize];
        }
        self.root
            .join(std::str::from_utf8(&name).expect("hex digits are ASCII"))
    }
    pub fn put(&self, bytes: &[u8]) -> io::Result<ContentHash> {
        self.archive(|writer| writer.write_all(bytes))
    }
    /// Publish an ELF/object payload under its raw content hash. Unlike an
    /// archive record, the file is passed directly to native linkers by path.
    pub(crate) fn put_bare(&self, bytes: &[u8]) -> io::Result<ContentHash> {
        let hash = ContentHash(sha2::Sha256::digest(bytes).into());
        let path = self.path(hash);
        if path.is_file() {
            return Ok(hash);
        }
        let temporary = self.temporary();
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)?;
            wrote("bare");
            wrote("directory");
            File::open(&self.root)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map(|()| hash)
    }
    pub(crate) fn archive(
        &self,
        emit: impl FnOnce(&mut ArchiveWriter) -> io::Result<()>,
    ) -> io::Result<ContentHash> {
        let mut writer = ArchiveWriter { buffer: Vec::new() };
        emit(&mut writer)?;
        let (hash, bytes) = Self::frame(&writer.buffer);
        self.write_framed(hash, &bytes)?;
        self.sync_directory()?;
        Ok(hash)
    }
    /// The envelope of one payload and its address: the digest of the header,
    /// payload and length bytes, computed once while framing.
    pub(crate) fn frame(payload: &[u8]) -> (ContentHash, Vec<u8>) {
        Self::framed(HEADER + payload.len() + TRAILER, |bytes| {
            bytes.extend_from_slice(payload);
            Ok(())
        })
        .expect("a copied payload cannot fail")
    }
    /// The envelope of the payload `fill` appends, written in place after
    /// the header: a payload encoded here is never copied before its digest.
    fn framed(
        capacity: usize,
        fill: impl FnOnce(&mut Vec<u8>) -> io::Result<()>,
    ) -> io::Result<(ContentHash, Vec<u8>)> {
        let mut bytes = Vec::with_capacity(capacity);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        fill(&mut bytes)?;
        hirsel_core_front::cost_scope!("non-check", "archive write and digest");
        let payload = (bytes.len() - HEADER) as u64;
        bytes.extend_from_slice(&payload.to_le_bytes());
        let hash = ContentHash(Sha256::digest(&bytes).into());
        bytes.extend_from_slice(&hash.0);
        Ok((hash, bytes))
    }
    /// The hash of exactly a module's serialization (`IR`).
    pub(crate) fn ir_hash(ir: &[u8]) -> ContentHash {
        hirsel_core_front::cost_scope!("non-check", "archive write and digest");
        ContentHash::of_parts(IR, [ir])
    }
    /// A body record: `head`, then the IR `fill` appends in place. Returns
    /// its address, the IR's hash and the envelope.
    pub(crate) fn frame_body(
        head: &[u8],
        fill: impl FnOnce(&mut Vec<u8>) -> io::Result<()>,
    ) -> io::Result<(ContentHash, ContentHash, Vec<u8>)> {
        let mut bytes = Vec::with_capacity(BODY_HEADER + head.len() + 32 + 64 + 32);
        bytes.extend_from_slice(BODY_MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&(head.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(head);
        let hash_at = bytes.len();
        bytes.extend_from_slice(&[0; 32]);
        let ir_at = bytes.len();
        fill(&mut bytes)?;
        let ir_hash = Self::ir_hash(&bytes[ir_at..]);
        let ir_len = (bytes.len() - ir_at) as u64;
        bytes[20..28].copy_from_slice(&ir_len.to_le_bytes());
        bytes[hash_at..ir_at].copy_from_slice(&ir_hash.0);
        hirsel_core_front::cost_scope!("non-check", "archive write and digest");
        let address = ContentHash(Sha256::digest(&bytes[..ir_at]).into());
        bytes.extend_from_slice(&address.0);
        Ok((address, ir_hash, bytes))
    }
    /// A body record read back under its address. Nothing escapes until its
    /// address and its IR hash both check; a short, long, foreign or damaged
    /// file is `None`.
    pub(crate) fn get_body(&self, key: ContentHash) -> io::Result<Option<Body>> {
        let image = crate::meter::ImageRead::start(self.image);
        let Some(bytes) = self.read_whole(key, image.as_ref())? else {
            return Ok(None);
        };
        Ok(Body::split(bytes, key, image.as_ref()))
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
        if let Some(image) = image {
            image.opened();
        }
        let size = usize::try_from(file.metadata()?.len()).map_err(io::Error::other)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size).map_err(io::Error::other)?;
        file.read_to_end(&mut bytes)?;
        if let Some(image) = image {
            image.bytes(bytes.len());
        }
        Ok((bytes.len() == size).then_some(bytes))
    }
    /// Write and sync one framed record under its address. The directory entry
    /// is made durable by a later `sync_directory`, once per publication.
    pub(crate) fn write_framed(&self, hash: ContentHash, bytes: &[u8]) -> io::Result<()> {
        let temporary = self.temporary();
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            wrote("record");
            fs::rename(&temporary, self.path(hash))
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
    pub(crate) fn sync_directory(&self) -> io::Result<()> {
        wrote("directory");
        File::open(&self.root)?.sync_all()
    }
    pub fn get(&self, key: ContentHash) -> io::Result<Option<Vec<u8>>> {
        let image = crate::meter::ImageRead::start(self.image);
        let Some(mut bytes) = self.read_whole(key, image.as_ref())? else {
            return Ok(None);
        };
        let size = bytes.len();
        if size < HEADER + TRAILER || &bytes[..8] != MAGIC || bytes[8..12] != VERSION.to_le_bytes()
        {
            return Ok(None);
        }
        let end = size - TRAILER;
        if u64::from_le_bytes(bytes[end..end + 8].try_into().unwrap()) != (end - HEADER) as u64
            || bytes[end + 8..] != key.0
            || image.as_ref().map_or_else(
                || Sha256::digest(&bytes[..end + 8]),
                |image| image.digest(end + 8, || Sha256::digest(&bytes[..end + 8])),
            )[..]
                != key.0
        {
            return Ok(None);
        }
        bytes.drain(..HEADER);
        bytes.truncate(end - HEADER);
        Ok(Some(bytes))
    }
    pub(crate) fn get_bare(&self, key: ContentHash) -> io::Result<Option<Vec<u8>>> {
        let path = self.path(key);
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if ContentHash(sha2::Sha256::digest(&bytes).into()) != key {
            return Ok(None);
        }
        Ok(Some(bytes))
    }
    pub(crate) fn selection(&self, name: &str) -> io::Result<Option<ContentHash>> {
        match fs::read(self.root.join(name)) {
            Ok(b) => Ok(<[u8; 32]>::try_from(b.as_slice()).ok().map(ContentHash)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    pub(crate) fn select(&self, name: &str, key: ContentHash) -> io::Result<()> {
        self.atomic(&self.root.join(name), &key.0)
    }
    fn temporary(&self) -> PathBuf {
        self.root.join(format!(
            ".write-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }
    pub(crate) fn atomic(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let temporary = self.temporary();
        let result = (|| {
            let mut f = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            fs::rename(&temporary, path)?;
            wrote("selection");
            wrote("directory");
            File::open(&self.root)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}
