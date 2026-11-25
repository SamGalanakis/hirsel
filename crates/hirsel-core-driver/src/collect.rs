//! The content stores' layout and their one collector.
//!
//! A content store (`Store::cas`) keeps records in `k/`, blobs in `o/` and
//! the fallback std sides in `s/`, each under a two-hex shard. A std side is
//! a content store of its own, holding a lowering's deferred bitcode and
//! lent bodies together; it is pinned by a shared lock on its shard's
//! `.std-lock` while a build uses it, and is collected whole.
//!
//! The collector keeps a store under `CAPACITY` bytes. It evicts by file
//! time, oldest first, down to seven eighths of the capacity: a read that
//! hits touches what it read, since a relatime mount may leave a hot file's
//! access time a day old. There is no root registry. A blob or record is
//! unlinked, which leaves every open descriptor reading the whole checked
//! file, and a later open misses and rebuilds it. A side is skipped while
//! pinned, and is otherwise renamed into a scratch directory before it is
//! removed, so a crash never leaves half a side.
//!
//! A scan stats every file. On this host's 103 GB cache of 7.2M files a
//! cold stat walk took 8 minutes, so a scan never runs on a request's path:
//! publishes feed a byte estimate kept in a sparse file's length, and a
//! background thread sweeps at most once a day, or once an hour while the
//! estimate is over the capacity. A short-lived process does not wait for
//! it; a sweep it leaves unfinished is taken up by a later one.
//!
//! Opening a store whose layout stamp is not this format's deletes its old
//! layout: a content store's records and sides, whose frames no reader of
//! this format takes, and, for the per-user store, the named directories
//! of older cache formats beside it. A flat compiler store (`Store::native`)
//! drops its hex-named files, whose frames are of an older format.
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

/// Records: key to the blobs they name.
pub(super) const RECORDS: &str = "k";
/// Blobs, named by the SHA-256 of their bytes.
pub(super) const BLOBS: &str = "o";
/// Fallback std sides, each a content store.
const SIDES: &str = "s";
/// The layout stamp: the format a store's files were written in.
const LAYOUT: &str = "layout";
/// The per-user store's directory name; the dead directories of older
/// cache formats sit beside it.
const USER: &str = "cas";

/// The user's content store: every build's objects and records, beside the
/// package cache in the user's hirsel cache directory.
pub(crate) fn user() -> PathBuf {
    crate::PackageProduct::default_cache().with_file_name(USER)
}
/// Older formats' directories beside the per-user store, by name. Nothing
/// in this toolchain reads them; other tools' directories there
/// (`cargo-units`, `lean`, `n1`, `packages`, `llvm-source`) are never named.
const DEAD: &[&str] = &[
    "objects-v1",
    "objects-v2",
    "objects-v3",
    "results-v2",
    "results-v3",
    "results-v12",
    "results-v15",
    "validator-v2",
    "programs-v1",
    "core-packages/release-parts",
];
/// The byte cap: sccache's default size.
const CAPACITY: u64 = 10 << 30;
const DAILY: Duration = Duration::from_secs(24 * 60 * 60);
const OVER_CAPACITY: Duration = Duration::from_secs(60 * 60);
const BLOCK: u64 = 4096;
const REPORT_BLOCKS: u64 = 64 * 1024 * 1024 / BLOCK;

#[derive(Clone)]
pub(crate) struct Cas {
    _pin: Option<Arc<File>>,
    cache: Arc<Cache>,
}

impl Store {
    /// The content store at `root`, its old layout deleted if its stamp is
    /// not this format's.
    pub(crate) fn cas(root: &Path) -> io::Result<Self> {
        let mut store = Self::new(root)?;
        store.cas = Some(Cas {
            cache: Arc::new(Cache {
                root: root.to_owned(),
                pending: AtomicU64::new(0),
                sweep: Mutex::new(None),
            }),
            _pin: None,
        });
        if !stamped(root)? {
            for directory in [RECORDS, SIDES] {
                store.retire(&root.join(directory))?;
            }
            if root.file_name().is_some_and(|name| name == USER)
                && let Some(parent) = root.parent()
            {
                for dead in DEAD {
                    store.retire(&parent.join(dead))?;
                }
            }
            store.stamp()?;
        }
        Ok(store)
    }

    /// A compiler's flat store at `root`: its executables and its request
    /// records. A store last written in another format loses its hex-named
    /// files, which no reader of this format takes, and its old code packs.
    pub(crate) fn native(root: &Path) -> io::Result<Self> {
        let store = Self::new(root)?;
        if !stamped(root)? {
            for entry in fs::read_dir(root)? {
                let entry = entry?;
                if hex_name(&entry, 64) && entry.file_type()?.is_file() {
                    present(fs::remove_file(entry.path()))?;
                }
            }
            store.retire(&root.join("code"))?;
            store.stamp()?;
        }
        Ok(store)
    }

    /// The std side under `key`, a content store pinned until dropped.
    pub(crate) fn side(&self, key: ContentHash) -> io::Result<Self> {
        let cas = self.cas.as_ref().expect("a content store");
        let path = self.named(SIDES, key);
        let shard = path.parent().expect("a side shard");
        cas.create_dir(shard.parent().expect("the sides directory"))?;
        cas.create_dir(shard)?;
        let pin = lock_file(&shard.join(".std-lock"), libc::LOCK_SH)?;
        cas.create_dir(&path)?;
        let mut side = Self::new(&path)?;
        let _ = File::open(&path)?.set_modified(SystemTime::now());
        side.cas = Some(Cas {
            cache: Arc::clone(&cas.cache),
            _pin: Some(Arc::new(pin)),
        });
        Ok(side)
    }

    /// Remove `path`, a directory or file, by renaming it into a scratch
    /// directory first, so a crash leaves a temporary, never half of it.
    fn retire(&self, path: &Path) -> io::Result<()> {
        if present(fs::symlink_metadata(path))?.is_none() {
            return Ok(());
        }
        let trash = self.scratch()?;
        present(fs::rename(path, trash.path().join("retired")))?;
        Ok(())
    }

    /// The file `name` in the store's root, held under an exclusive lock
    /// until dropped.
    pub(crate) fn hold(&self, name: &str) -> io::Result<File> {
        lock_file(&self.root.join(name), libc::LOCK_EX)
    }

    fn stamp(&self) -> io::Result<()> {
        self.publish_to(|file| {
            io::Write::write_all(file, &layout())?;
            Ok((self.root.join(LAYOUT), ()))
        })
    }
}

fn layout() -> Vec<u8> {
    format!("hirsel store {}\n", super::VERSION).into_bytes()
}

fn stamped(root: &Path) -> io::Result<bool> {
    Ok(present(fs::read(root.join(LAYOUT)))? == Some(layout()))
}

pub(crate) fn touch(file: &File) {
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

impl Cas {
    pub(super) fn create_dir(&self, path: &Path) -> io::Result<()> {
        match fs::create_dir(path) {
            Ok(()) => self.wrote(BLOCK as usize),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.create_dir(path.parent().expect("a store directory"))?;
                return self.create_dir(path);
            }
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
            .name("hirsel-collect".into())
            .spawn(move || {
                // Collection is opportunistic. It doesn't determine whether the
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
        // The estimate is kept; a sweep is never waited for on exit.
        let pending = *self.pending.get_mut();
        if pending != 0 {
            let _ = estimate(&self.root, pending);
        }
    }
}

/// Add `written` blocks to the estimate kept in the length of a sparse
/// file, in 4 KiB units. Rewrites overcount until the next sweep.
fn estimate(root: &Path, written: u64) -> io::Result<u64> {
    let budget = lock_file(&root.join(".budget"), libc::LOCK_EX)?;
    let estimate = budget.metadata()?.len().saturating_add(written);
    budget.set_len(estimate)?;
    Ok(estimate)
}

fn due(stamp: Option<&Metadata>, estimate: u64, capacity: u64) -> io::Result<bool> {
    let Some(stamp) = stamp.filter(|stamp| stamp.len() != 0) else {
        return Ok(true);
    };
    let age = stamp.modified()?.elapsed().unwrap_or_default();
    Ok(age >= DAILY || (estimate > capacity / BLOCK && age >= OVER_CAPACITY))
}

fn maintain(root: &Path, capacity: u64, written: u64) -> io::Result<()> {
    let budget_path = root.join(".budget");
    let stamp_path = root.join(".sweep");
    let estimated = estimate(root, written)?;
    if !due(
        present(fs::metadata(&stamp_path))?.as_ref(),
        estimated,
        capacity,
    )? {
        return Ok(());
    }
    let stamp = match lock_file(&stamp_path, libc::LOCK_EX | libc::LOCK_NB) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
        Err(e) => return Err(e),
    };
    let estimated = fs::metadata(&budget_path)?.len();
    if !due(Some(&stamp.metadata()?), estimated, capacity)? {
        return Ok(());
    }
    let remaining = sweep(root, capacity)?;
    let budget = lock_file(&budget_path, libc::LOCK_EX)?;
    // A concurrent writer may already be counted in the scan. Preserve all
    // reports since it started so that missed publishes are also counted.
    let concurrent = budget.metadata()?.len().saturating_sub(estimated);
    budget.set_len(remaining.div_ceil(BLOCK).saturating_add(concurrent))?;
    stamp.set_len(1)?;
    stamp.set_modified(SystemTime::now())
}

/// Scan the store at `root` and, when it holds more than `capacity` bytes,
/// evict the least recently used entries down to seven eighths of it.
/// Returns the bytes left.
pub(crate) fn sweep(root: &Path, capacity: u64) -> io::Result<u64> {
    let mut entries = Vec::new();
    let mut size = 0;
    for directory in [RECORDS, BLOBS, SIDES] {
        let Some(shards) = present(fs::read_dir(root.join(directory)))? else {
            continue;
        };
        for shard in shards {
            let shard = shard?;
            if !hex_name(&shard, 2) || !shard.file_type()?.is_dir() {
                continue;
            }
            size += allocated(&shard.metadata()?);
            let Some(names) = present(fs::read_dir(shard.path()))? else {
                continue;
            };
            for entry in names {
                let entry = entry?;
                if !hex_name(&entry, 62) {
                    continue;
                }
                let Some(metadata) = present(entry.metadata())? else {
                    continue;
                };
                let side = metadata.is_dir();
                if side != (directory == SIDES) || !(side || metadata.is_file()) {
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
            let shard = self.path.parent().expect("a side shard");
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
            Store::new(root)?.retire(&self.path)?;
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
    for directory in [RECORDS, BLOBS] {
        let Some(shards) = present(fs::read_dir(path.join(directory)))? else {
            continue;
        };
        for shard in shards {
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

/// The files under `directory` of the content store at `root`, each with the
/// name its shard and file spell.
#[cfg(test)]
fn named(root: &Path, directory: &str) -> Vec<(PathBuf, ContentHash)> {
    let mut found = Vec::new();
    let Ok(shards) = fs::read_dir(root.join(directory)) else {
        return found;
    };
    for shard in shards {
        let shard = shard.unwrap();
        for file in fs::read_dir(shard.path()).unwrap() {
            let file = file.unwrap();
            let hex = [shard.file_name(), file.file_name()]
                .map(|n| n.into_string().unwrap())
                .concat();
            let mut name = [0; 32];
            for (i, byte) in name.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
            }
            found.push((file.path(), ContentHash(name)));
        }
    }
    found.sort();
    found
}

/// Each record in the content store at `root`: its path and key.
#[cfg(test)]
pub(crate) fn records(root: &Path) -> Vec<(PathBuf, ContentHash)> {
    named(root, RECORDS)
}

/// Each blob in the content store at `root`: its path and address.
#[cfg(test)]
pub(crate) fn blobs(root: &Path) -> Vec<(PathBuf, ContentHash)> {
    named(root, BLOBS)
}

/// Each object record in the content store at `root`, by its key, with its
/// object's bytes.
#[cfg(test)]
pub(crate) fn cached_objects(root: &Path) -> std::collections::BTreeMap<ContentHash, Vec<u8>> {
    let store = Store::cas(root).unwrap();
    records(root)
        .into_iter()
        .filter_map(|(_, key)| {
            Some((
                key,
                crate::native_object::load(&store, key).unwrap()?.1.bytes,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::Barrier;

    /// Law: eviction never makes a build read wrong bytes. A blob a build
    /// holds open reads whole after it is evicted, and a later read misses;
    /// a std side a build has pinned stays whole through a sweep and goes
    /// once unpinned; concurrent publishes and reads see each blob whole or
    /// not at all while sweeps run. Bugs: an eviction that truncated or
    /// rewrote in place would serve the open reader short bytes; a sweep
    /// ignoring the side's pin would remove a running build's bitcode.
    #[test]
    fn concurrent_parts_are_whole_or_miss_during_eviction() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let store = Store::cas(root).unwrap();
        let key = ContentHash::of_bytes(b"evict while reading");
        let value = b"checked before eviction";
        let bare = store.put_keyed_blob(key, value).unwrap();
        let path = store.blob_path(bare.address);
        assert_eq!(path.strip_prefix(root).unwrap().components().count(), 3);
        let mut already_open = File::open(&path).unwrap();

        let side_key = ContentHash::of_bytes(b"fallback std");
        let side = store.side(side_key).unwrap();
        side.put_keyed_blob(key, value).unwrap();
        sweep(root, 0).unwrap();
        assert_eq!(store.get_blob(bare).unwrap(), None);
        assert_eq!(store.get_keyed_blob(key).unwrap(), None);
        let mut read = Vec::new();
        already_open.read_to_end(&mut read).unwrap();
        assert_eq!(read, value, "the open blob reads whole after eviction");
        assert_eq!(
            side.get_keyed_blob(key)
                .unwrap()
                .map(|(_, bytes)| bytes)
                .as_deref(),
            Some(value.as_slice()),
            "the pinned side survives the sweep"
        );
        let side_path = side.root().to_owned();
        drop(side);
        sweep(root, 0).unwrap();
        assert!(!side_path.exists(), "the unpinned side is collected");

        const CAP: u64 = 1024 * 1024;
        let fixture = |index: u8| {
            let value = vec![index; 128 * 1024];
            let bare = super::super::Bare {
                address: ContentHash(<sha2::Sha256 as sha2::Digest>::digest(&value).into()),
                check: super::super::checksum(&value),
            };
            (bare, value)
        };
        let start = Barrier::new(5);
        thread::scope(|scope| {
            for writer in 0..4 {
                let start = &start;
                scope.spawn(move || {
                    let store = Store::cas(root).unwrap();
                    start.wait();
                    for index in 0..24 {
                        let (bare, bytes) = fixture(writer * 24 + index);
                        assert_eq!(store.put_blob(&bytes).unwrap(), bare);
                        let (other, expected) = fixture((writer * 24 + index + 24) % 96);
                        if let Some(actual) = store.get_blob(other).unwrap() {
                            assert_eq!(actual, expected);
                        }
                        // Inspect the published file too. A direct write
                        // could leave a partial file that the check rejects.
                        match fs::read(store.blob_path(other.address)) {
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

    /// Law: opening a store written in an older format deletes that layout
    /// and nothing else: a content store's records and sides, the named
    /// dead directories beside the per-user store, a flat compiler store's
    /// hex-named files and old packs. Blobs, which are named by their bytes,
    /// stay, and so do other tools' directories. Bug: a pattern sweep of
    /// `*-v<N>` siblings would delete another tool's cache.
    #[test]
    fn opening_an_old_layout_deletes_it_and_keeps_the_rest() {
        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path();
        let root = cache.join(USER);
        let store = Store::cas(&root).unwrap();
        let key = ContentHash::of_bytes(b"old record");
        let bare = store.put_keyed_blob(key, b"bytes").unwrap();
        drop(store.side(key).unwrap());
        for name in [
            "objects-v2",
            "results-v15",
            "core-packages/release-parts",
            "lean",
            "cargo-units",
            "llvm-source-v2",
        ] {
            fs::create_dir_all(cache.join(name)).unwrap();
        }
        fs::write(root.join(LAYOUT), b"hirsel store 1\n").unwrap();
        let store = Store::cas(&root).unwrap();
        assert_eq!(store.get_keyed_blob(key).unwrap(), None, "old records go");
        assert!(!root.join(SIDES).exists(), "old sides go");
        assert!(store.get_blob(bare).unwrap().is_some(), "blobs stay");
        for gone in ["objects-v2", "results-v15", "core-packages/release-parts"] {
            assert!(!cache.join(gone).exists(), "{gone} is deleted");
        }
        for kept in ["lean", "cargo-units", "llvm-source-v2", "core-packages"] {
            assert!(cache.join(kept).exists(), "{kept} is kept");
        }

        let native = cache.join("native");
        fs::create_dir_all(native.join("code/packs")).unwrap();
        let old = native.join(format!("{:064x}", 7));
        fs::write(&old, b"old frame").unwrap();
        fs::write(native.join("resident.lock"), b"").unwrap();
        Store::native(&native).unwrap();
        assert!(!old.exists() && !native.join("code").exists());
        assert!(native.join("resident.lock").exists());
        // Stamped, a store keeps what it writes.
        fs::write(&old, b"new frame").unwrap();
        Store::native(&native).unwrap();
        assert!(old.exists());
    }
}
