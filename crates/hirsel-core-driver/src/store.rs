//! Immutable byte records. The root is replaced only after its contents are published.
//! Disk bytes are external input; a damaged record is a cache miss.
use hirsel_core_ids::ContentHash;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: &[u8; 8] = b"HCORE\0\0\0";
const VERSION: u32 = 1;
const HEADER: usize = 52;
static NEXT: AtomicU64 = AtomicU64::new(0);

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
        self.root
            .join(key.0.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }
    pub fn put(&self, bytes: &[u8]) -> io::Result<ContentHash> {
        let key = ContentHash::of_bytes(bytes);
        let mut record = Vec::with_capacity(HEADER + bytes.len());
        record.extend_from_slice(MAGIC);
        record.extend_from_slice(&VERSION.to_le_bytes());
        record.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        record.extend_from_slice(&key.0);
        record.extend_from_slice(bytes);
        self.atomic(&self.path(key), &record)?;
        Ok(key)
    }
    pub fn get(&self, key: ContentHash) -> io::Result<Option<Vec<u8>>> {
        let mut file = match fs::File::open(self.path(key)) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut header = [0u8; HEADER];
        if file.read_exact(&mut header).is_err() {
            return Ok(None);
        }
        let size = u64::from_le_bytes(header[12..20].try_into().unwrap());
        if &header[..8] != MAGIC
            || header[8..12] != VERSION.to_le_bytes()
            || header[20..52] != key.0
            || file.metadata()?.len().checked_sub(HEADER as u64) != Some(size)
        {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        if bytes
            .try_reserve_exact(usize::try_from(size).map_err(io::Error::other)?)
            .is_err()
        {
            return Err(io::Error::other(
                "core store decode exceeded compiler memory",
            ));
        }
        file.read_to_end(&mut bytes)?;
        Ok((ContentHash::of_bytes(&bytes) == key).then_some(bytes))
    }
    pub fn publish_root(&self, key: ContentHash) -> io::Result<()> {
        self.atomic(&self.root.join("check-root"), &key.0)
    }
    fn atomic(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let temporary = self.root.join(format!(
            ".write-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut f = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            fs::rename(&temporary, path)?;
            fs::File::open(&self.root)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}
