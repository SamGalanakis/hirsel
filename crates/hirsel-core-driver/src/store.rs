//! One buffered append-only archive per request, committed by atomic rename.
//! Disk bytes are external input; a damaged record is a cache miss.
use hirsel_core_ids::ContentHash;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: &[u8; 8] = b"HCORE\0\0\0";
const VERSION: u32 = 22;
const HEADER: usize = 12;
const TRAILER: usize = 40;
static NEXT: AtomicU64 = AtomicU64::new(0);

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
struct HashedFile {
    file: File,
    hash: Sha256,
}
impl Write for HashedFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        hirsel_core_front::cost_scope!("non-check", "archive write and digest");
        let written = self.file.write(bytes)?;
        self.hash.update(&bytes[..written]);
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
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
}
impl Store {
    pub fn new(root: &Path) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        Ok(Self {
            root: root.to_owned(),
        })
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
        let temporary = self.temporary();
        let result = (|| {
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            let mut output = BufWriter::with_capacity(
                64 * 1024,
                HashedFile {
                    file,
                    hash: Sha256::new(),
                },
            );
            output.write_all(MAGIC)?;
            output.write_all(&VERSION.to_le_bytes())?;
            let mut writer = ArchiveWriter { buffer: Vec::new() };
            emit(&mut writer)?;
            output.write_all(&writer.buffer)?;
            output.write_all(&(writer.buffer.len() as u64).to_le_bytes())?;
            output.flush()?;
            let mut output = output.into_inner().map_err(|e| e.into_error())?;
            // The digest covers exactly the header, payload and length bytes written.
            let hash = ContentHash(output.hash.finalize().into());
            output.file.write_all(&hash.0)?;
            output.file.sync_all()?;
            fs::rename(&temporary, self.path(hash))?;
            File::open(&self.root)?.sync_all()?;
            Ok(hash)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
    pub fn get(&self, key: ContentHash) -> io::Result<Option<Vec<u8>>> {
        let mut file = match File::open(self.path(key)) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let size = usize::try_from(file.metadata()?.len()).map_err(io::Error::other)?;
        if size < HEADER + TRAILER {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size).map_err(io::Error::other)?;
        file.read_to_end(&mut bytes)?;
        if bytes.len() != size || &bytes[..8] != MAGIC || bytes[8..12] != VERSION.to_le_bytes() {
            return Ok(None);
        }
        let end = size - TRAILER;
        if u64::from_le_bytes(bytes[end..end + 8].try_into().unwrap()) != (end - HEADER) as u64
            || bytes[end + 8..] != key.0
            || Sha256::digest(&bytes[..end + 8])[..] != key.0
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
    pub fn publish_root(&self, key: ContentHash) -> io::Result<()> {
        self.atomic(&self.root.join("check-root"), &key.0)
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
            File::open(&self.root)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}
