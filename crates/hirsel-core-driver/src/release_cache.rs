//! Maintenance for the shared release store. Regular entries can be unlinked
//! at any time: an open descriptor still names the checked file, and a later
//! open misses. A fallback std side keeps its deferred bitcode and lent bodies
//! together in one directory, pinned until the build has its link inputs.
use super::{Store, lock};
use hirsel_core_ids::ContentHash;
use std::fs::{self, File, FileTimes, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

const CAPACITY: u64 = 1024 * 1024 * 1024;
const AUDIT_INTERVAL: Duration = Duration::from_secs(600);
const BLOCK: u64 = 4096;
const REPORT_BLOCKS: u64 = 64 * 1024 * 1024 / BLOCK;

#[derive(Clone)]
pub(super) struct Release {
    _pin: Option<Arc<File>>,
    cache: Arc<Cache>,
}

impl Store {
    pub(crate) fn release_parts(root: &Path) -> io::Result<Self> {
        let mut store = Self::new(root)?;
        store.release = Some(Release {
            cache: Arc::new(Cache {
                root: root.to_owned(),
                pending: AtomicU64::new(0),
                sweep: Mutex::new(None),
            }),
            _pin: None,
        });
        Ok(store)
    }

    pub(crate) fn release_side(&self, key: ContentHash) -> io::Result<Self> {
        let path = self.path(key);
        let shard = path.parent().expect("a release cache shard");
        let release = self.release.as_ref().expect("the shared release cache");
        release.create_dir(shard)?;
        let pin = lock_file(&shard.join(".std-lock"), libc::LOCK_SH)?;
        release.create_dir(&path)?;
        let mut side = Self::new(&path)?;
        let _ = File::open(&path)?.set_modified(SystemTime::now());
        side.release = Some(Release {
            cache: Arc::clone(&release.cache),
            _pin: Some(Arc::new(pin)),
        });
        Ok(side)
    }
}

pub(super) fn touch(file: &File) {
    // Linux relatime can leave a frequently read file's atime a day old.
    let _ = file.set_times(FileTimes::new().set_accessed(SystemTime::now()));
}

fn lock_file(path: &Path, flags: libc::c_int) -> io::Result<File> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    if lock(&file, flags)? {
        Ok(file)
    } else {
        Err(io::ErrorKind::WouldBlock.into())
    }
}

struct Cache {
    root: PathBuf,
    pending: AtomicU64,
    sweep: Mutex<Option<JoinHandle<()>>>,
}

impl Release {
    pub(super) fn create_dir(&self, path: &Path) -> io::Result<()> {
        match fs::create_dir(path) {
            Ok(()) => self.wrote(BLOCK as usize),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        Ok(())
    }

    pub(super) fn wrote(&self, bytes: usize) {
        let blocks = (bytes as u64).div_ceil(BLOCK).max(1);
        let prior = self.cache.pending.fetch_add(blocks, Ordering::Relaxed);
        if prior + blocks < REPORT_BLOCKS {
            return;
        }
        let mut worker = self.cache.sweep.lock().unwrap();
        if worker.as_ref().is_some_and(|thread| !thread.is_finished()) {
            return;
        }
        if let Some(done) = worker.take() {
            let _ = done.join();
        }
        let root = self.cache.root.clone();
        let blocks = self.cache.pending.swap(0, Ordering::Relaxed);
        *worker = thread::Builder::new()
            .name("hirsel-release-cache".into())
            .spawn(move || {
                // Cleanup is opportunistic. It doesn't determine whether the
                // compilation's checked inputs can be used.
                let _ = maintain(&root, CAPACITY, blocks);
            })
            .ok();
        if worker.is_none() {
            self.cache.pending.fetch_add(blocks, Ordering::Relaxed);
        }
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        // A short-lived CLI still lets its worker finish, after emission.
        if let Some(worker) = self.sweep.get_mut().unwrap().take() {
            let _ = worker.join();
        }
        let pending = *self.pending.get_mut();
        if pending != 0 {
            let _ = maintain(&self.root, CAPACITY, pending);
        }
    }
}

fn audit_due(stamp: &Metadata) -> io::Result<bool> {
    Ok(stamp.len() == 0 || stamp.modified()?.elapsed().unwrap_or_default() >= AUDIT_INTERVAL)
}

fn maintain(root: &Path, capacity: u64, written: u64) -> io::Result<()> {
    let budget_path = root.join(".budget");
    let stamp_path = root.join(".sweep");
    let budget = lock_file(&budget_path, libc::LOCK_EX)?;
    // Store an estimate in the length of a sparse file, in 4 KiB units.
    // Rewrites overcount until the next sweep.
    let estimate = budget.metadata()?.len().saturating_add(written);
    budget.set_len(estimate)?;
    drop(budget);
    let stamp = present(fs::metadata(&stamp_path))?;
    if estimate <= capacity / BLOCK
        && let Some(stamp) = stamp
        && !audit_due(&stamp)?
    {
        return Ok(());
    }
    let stamp = match lock_file(&stamp_path, libc::LOCK_EX | libc::LOCK_NB) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
        Err(e) => return Err(e),
    };
    let estimate = fs::metadata(&budget_path)?.len();
    if estimate <= capacity / BLOCK && !audit_due(&stamp.metadata()?)? {
        return Ok(());
    }
    let remaining = sweep(root, capacity)?;
    let budget = lock_file(&budget_path, libc::LOCK_EX)?;
    // A concurrent writer may already be counted in the scan. Preserve all
    // reports since it started so that missed publishes are also counted.
    let concurrent = budget.metadata()?.len().saturating_sub(estimate);
    budget.set_len(remaining.div_ceil(BLOCK).saturating_add(concurrent))?;
    stamp.set_len(1)?;
    stamp.set_modified(SystemTime::now())
}

fn sweep(root: &Path, capacity: u64) -> io::Result<u64> {
    let mut entries = Vec::new();
    let mut size = 0;
    for shard in fs::read_dir(root)? {
        let shard = shard?;
        if !hex_name(&shard, 2) || !shard.file_type()?.is_dir() {
            continue;
        }
        size += allocated(&shard.metadata()?);
        for entry in fs::read_dir(shard.path())? {
            let entry = entry?;
            if !hex_name(&entry, 62) {
                continue;
            }
            let Some(metadata) = present(entry.metadata())? else {
                continue;
            };
            let side = metadata.is_dir();
            if !side && !metadata.is_file() {
                continue;
            }
            let path = entry.path();
            let bytes = if side {
                side_size(&path, &metadata)?
            } else {
                allocated(&metadata)
            };
            size += bytes;
            entries.push(Entry {
                last_used: last_used(&metadata, side)?,
                path,
                side,
                bytes,
            });
        }
    }
    if size > capacity {
        // Leave room for publishes concurrent with the scan and for the next
        // batch of builds. A pinned side can temporarily push over capacity.
        let target = capacity - capacity / 8;
        entries.sort_unstable_by_key(|entry| entry.last_used);
        for entry in entries {
            if size <= target {
                break;
            }
            if entry.evict(root)? {
                size = size.saturating_sub(entry.bytes);
            }
        }
    }
    Ok(size)
}

struct Entry {
    last_used: SystemTime,
    path: PathBuf,
    side: bool,
    bytes: u64,
}

impl Entry {
    fn evict(&self, root: &Path) -> io::Result<bool> {
        let _pin = if self.side {
            let shard = self.path.parent().expect("a release cache shard");
            match lock_file(&shard.join(".std-lock"), libc::LOCK_EX | libc::LOCK_NB) {
                Ok(pin) => Some(pin),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(e) => return Err(e),
            }
        } else {
            None
        };
        let Some(metadata) = present(fs::symlink_metadata(&self.path))? else {
            return Ok(true);
        };
        if last_used(&metadata, self.side)? > self.last_used {
            return Ok(false);
        }
        if self.side {
            // Retire the directory atomically. A crash during recursive
            // deletion leaves a temporary, never half a published std side.
            let trash = Store::new(root)?.scratch()?;
            fs::rename(&self.path, trash.path().join("side"))?;
            drop(_pin);
        } else {
            present(fs::remove_file(&self.path))?;
        }
        Ok(true)
    }
}

fn last_used(metadata: &Metadata, side: bool) -> io::Result<SystemTime> {
    let modified = metadata.modified()?;
    if side {
        // Scanning a directory itself can update its atime.
        Ok(modified)
    } else {
        Ok(metadata.accessed()?.max(modified))
    }
}

fn allocated(metadata: &Metadata) -> u64 {
    metadata.blocks() * 512
}

fn hex_name(entry: &fs::DirEntry, length: usize) -> bool {
    let name = entry.file_name();
    let bytes = name.as_encoded_bytes();
    bytes.len() == length && bytes.iter().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

fn side_size(path: &Path, metadata: &Metadata) -> io::Result<u64> {
    let mut size = allocated(metadata);
    for shard in fs::read_dir(path)? {
        let shard = shard?;
        if !hex_name(&shard, 2) || !shard.file_type()?.is_dir() {
            continue;
        }
        size += allocated(&shard.metadata()?);
        for entry in fs::read_dir(shard.path())? {
            let entry = entry?;
            if hex_name(&entry, 62)
                && let Some(metadata) = present(entry.metadata())?
            {
                size += allocated(&metadata);
            }
        }
    }
    Ok(size)
}

fn present<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
pub(crate) fn cached_objects(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    use std::collections::BTreeMap;
    fn collect(root: &Path, directory: &Path, found: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                if entry.file_name() != "tmp" {
                    collect(root, &path, found);
                }
            } else if let Ok(bytes) = std::fs::read(&path)
                && bytes.starts_with(b"\x7fELF")
            {
                found.insert(path.strip_prefix(root).unwrap().to_owned(), bytes);
            }
        }
    }
    let mut found = BTreeMap::new();
    collect(root, root, &mut found);
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::Barrier;

    #[test]
    fn concurrent_parts_are_whole_or_miss_during_eviction() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let store = Store::release_parts(root).unwrap();
        let key = ContentHash::of_bytes(b"evict while reading");
        let value = b"checked before eviction";
        let bare = store.put_bare(key, value).unwrap();
        let path = store.path(key);
        assert_eq!(path.strip_prefix(root).unwrap().components().count(), 2);
        let mut already_open = File::open(&path).unwrap();

        let side_key = ContentHash::of_bytes(b"fallback std");
        let side = store.release_side(side_key).unwrap();
        side.put_keyed(key, value).unwrap();
        sweep(root, 0).unwrap();
        assert_eq!(store.get_bare(bare).unwrap(), None);
        let mut read = Vec::new();
        already_open.read_to_end(&mut read).unwrap();
        assert_eq!(read, value);
        assert_eq!(side.get(key).unwrap().as_deref(), Some(value.as_slice()));
        drop(side);
        sweep(root, 0).unwrap();
        assert!(!store.path(side_key).exists());

        const CAP: u64 = 1024 * 1024;
        let fixture = |index: u8| {
            let key = ContentHash::of_bytes(&[index]);
            let value = vec![index; 128 * 1024];
            let bare = super::super::Bare {
                address: key,
                check: super::super::checksum(&value),
            };
            (bare, value)
        };
        let start = Barrier::new(5);
        thread::scope(|scope| {
            for writer in 0..4 {
                let start = &start;
                scope.spawn(move || {
                    let store = Store::release_parts(root).unwrap();
                    start.wait();
                    for index in 0..24 {
                        let (bare, bytes) = fixture(writer * 24 + index);
                        store.put_bare(bare.address, &bytes).unwrap();
                        let (other, expected) = fixture((writer * 24 + index + 24) % 96);
                        if let Some(actual) = store.get_bare(other).unwrap() {
                            assert_eq!(actual, expected);
                        }
                        // Inspect the published file too. A direct write
                        // could leave a partial file that the check rejects.
                        match fs::read(store.path(other.address)) {
                            Ok(actual) => assert_eq!(actual, expected),
                            Err(e) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
                        }
                    }
                });
            }
            start.wait();
            for _ in 0..16 {
                sweep(root, CAP).unwrap();
            }
        });
        assert!(sweep(root, CAP).unwrap() <= CAP);
    }
}
