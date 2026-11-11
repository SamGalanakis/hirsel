//! A disk that faults the file-system calls a provider makes and models what a
//! host failure keeps, for the log's `disk` seam and the [`DirObjects`] seam
//! ([`hirsel_object_storage::Disk`]).
//!
//! A faulted call fails with nothing done: a write with no byte written or
//! short, `fdatasync`, creation, truncation, the directory sync, a rename, an
//! unlink or a link. A host failure ([`fail_host`]) puts every directory under
//! a root back to the names its last directory sync saw (an entry no sync
//! named is gone, and an entry a rename or an unlink took away since is back,
//! as the inode it named then), and cuts every file to its synced bytes and
//! any prefix of the rest. Inodes a namespace names are kept alive in an
//! attic beside the root, so a rollback can name them again. Directories
//! themselves survive.
//!
//! [`DirObjects`]: hirsel_object_storage::DirObjects

use crate::harness::Rng;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub(crate) const WRITE: usize = 0;
pub(crate) const SHORT: usize = 1;
pub(crate) const SYNC: usize = 2;
pub(crate) const CREATE: usize = 3;
pub(crate) const DIR_SYNC: usize = 4;
pub(crate) const SET_LEN: usize = 5;
pub(crate) const RENAME: usize = 6;
pub(crate) const REMOVE: usize = 7;
pub(crate) const LINK: usize = 8;
pub(crate) const KINDS: usize = 9;

struct Disk {
    rng: Rng,
    /// One call in `odds[kind]` fails that way; 0 never.
    odds: [u64; KINDS],
    /// Each file's synced length, by inode.
    synced: HashMap<u64, u64>,
    /// Each directory's entries (name to inode) as its last sync saw them.
    names: HashMap<PathBuf, HashMap<OsString, u64>>,
    /// Holds a link to every inode a namespace names, named by the inode.
    attic: tempfile::TempDir,
}

thread_local! {
    static DISK: RefCell<Option<Disk>> = const { RefCell::new(None) };
}

/// Fault this thread's calls with `odds`, drawing from `seed`.
pub(crate) fn install(seed: u64, odds: [u64; KINDS]) {
    let attic = tempfile::tempdir().unwrap();
    DISK.with(|d| {
        *d.borrow_mut() = Some(Disk {
            rng: Rng(seed | 1),
            odds,
            synced: HashMap::new(),
            names: HashMap::new(),
            attic,
        })
    });
}

/// Back to real calls.
pub(crate) fn remove() {
    DISK.with(|d| *d.borrow_mut() = None);
}

fn with<R>(f: impl FnOnce(&mut Disk) -> R) -> Option<R> {
    DISK.with(|d| d.borrow_mut().as_mut().map(f))
}

fn strikes(d: &mut Disk, kind: usize) -> bool {
    d.odds[kind] != 0 && d.rng.below(d.odds[kind]) == 0
}

fn errno(n: i32) -> io::Error {
    io::Error::from_raw_os_error(n)
}

pub(crate) fn write_at(f: &File, bytes: &[u8], at: u64) -> Option<io::Result<usize>> {
    with(|d| {
        if strikes(d, WRITE) {
            return Err(errno(libc::ENOSPC));
        }
        let n = if bytes.len() > 1 && strikes(d, SHORT) {
            1 + d.rng.below(bytes.len() as u64 - 1) as usize
        } else {
            bytes.len()
        };
        f.write_all_at(&bytes[..n], at).map(|_| n)
    })
}

pub(crate) fn sync_data(f: &File) -> Option<io::Result<()>> {
    with(|d| {
        if strikes(d, SYNC) {
            return Err(errno(libc::EIO));
        }
        let m = f.metadata()?;
        d.synced.insert(m.ino(), m.len());
        Ok(())
    })
}

pub(crate) fn set_len(f: &File, len: u64) -> Option<io::Result<()>> {
    with(|d| {
        if strikes(d, SET_LEN) {
            return Err(errno(libc::EIO));
        }
        f.set_len(len)?;
        if let Some(s) = d.synced.get_mut(&f.metadata()?.ino()) {
            *s = (*s).min(len);
        }
        Ok(())
    })
}

pub(crate) fn create_new(path: &Path) -> Option<io::Result<File>> {
    with(|d| {
        if strikes(d, CREATE) {
            return Err(errno(libc::ENOSPC));
        }
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(path)
    })
}

pub(crate) fn rename(from: &Path, to: &Path) -> Option<io::Result<()>> {
    with(|d| {
        if strikes(d, RENAME) {
            return Err(errno(libc::EIO));
        }
        fs::rename(from, to)
    })
}

pub(crate) fn remove_file(path: &Path) -> Option<io::Result<()>> {
    with(|d| {
        if strikes(d, REMOVE) {
            return Err(errno(libc::EIO));
        }
        fs::remove_file(path)
    })
}

pub(crate) fn hard_link(from: &Path, to: &Path) -> Option<io::Result<()>> {
    with(|d| {
        if strikes(d, LINK) {
            return Err(errno(libc::ENOSPC));
        }
        fs::hard_link(from, to)
    })
}

pub(crate) fn sync_dir(dir: &Path) -> Option<io::Result<()>> {
    with(|d| {
        if strikes(d, DIR_SYNC) {
            return Err(errno(libc::EIO));
        }
        record(d, dir)
    })
}

/// Make `dir`'s current entries the ones a host failure keeps.
fn record(d: &mut Disk, dir: &Path) -> io::Result<()> {
    let mut names = HashMap::new();
    for e in fs::read_dir(dir)? {
        let e = e?;
        if !e.file_type()?.is_file() {
            continue;
        }
        let ino = e.metadata()?.ino();
        let kept = d.attic.path().join(ino.to_string());
        if !kept.exists() {
            fs::hard_link(e.path(), &kept)?;
        }
        names.insert(e.file_name(), ino);
    }
    d.names.insert(dir.to_path_buf(), names);
    Ok(())
}

/// Every directory under `root`, `root` included.
fn directories(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let mut i = 0;
    while i < out.len() {
        for e in fs::read_dir(&out[i]).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                out.push(e.path());
            }
        }
        i += 1;
    }
    out
}

/// The host fails: each directory under `root` holds the entries its last sync
/// named, each file its synced bytes and any prefix of the rest. Afterwards
/// everything on disk is durable.
pub(crate) fn fail_host(root: &Path) {
    with(|d| {
        let dirs = directories(root);
        for dir in &dirs {
            let durable = d.names.get(dir).cloned().unwrap_or_default();
            for e in fs::read_dir(dir).unwrap() {
                let e = e.unwrap();
                if e.file_type().unwrap().is_file()
                    && durable.get(&e.file_name()) != Some(&e.metadata().unwrap().ino())
                {
                    fs::remove_file(e.path()).unwrap();
                }
            }
            for (name, ino) in &durable {
                let path = dir.join(name);
                if !path.exists() {
                    fs::hard_link(d.attic.path().join(ino.to_string()), &path).unwrap();
                }
            }
        }
        for dir in &dirs {
            for e in fs::read_dir(dir).unwrap() {
                let e = e.unwrap();
                if !e.file_type().unwrap().is_file() {
                    continue;
                }
                let m = e.metadata().unwrap();
                let synced = d.synced.get(&m.ino()).copied().unwrap_or(0).min(m.len());
                let keep = synced + d.rng.below(m.len() - synced + 1);
                OpenOptions::new()
                    .write(true)
                    .open(e.path())
                    .and_then(|f| f.set_len(keep))
                    .unwrap();
            }
        }
        for e in fs::read_dir(d.attic.path()).unwrap() {
            fs::remove_file(e.unwrap().path()).unwrap();
        }
        d.synced.clear();
        d.names.clear();
        for dir in &dirs {
            record(d, dir).unwrap();
            for e in fs::read_dir(dir).unwrap() {
                let m = e.unwrap().metadata().unwrap();
                if m.is_file() {
                    d.synced.insert(m.ino(), m.len());
                }
            }
        }
    });
}

/// The [`hirsel_object_storage::Disk`] a [`DirObjects`] test installs: this
/// thread's faulted calls when a disk is installed, the real ones otherwise.
///
/// [`DirObjects`]: hirsel_object_storage::DirObjects
#[derive(Debug)]
pub(crate) struct Modelled;

impl hirsel_object_storage::Disk for Modelled {
    fn create_new(&self, path: &Path) -> io::Result<File> {
        create_new(path).unwrap_or_else(|| File::create_new(path))
    }
    fn write_all(&self, f: &File, mut bytes: &[u8]) -> io::Result<()> {
        let mut at = 0;
        while !bytes.is_empty() {
            let n = match write_at(f, bytes, at) {
                Some(r) => r?,
                None => f.write_at(bytes, at)?,
            };
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            bytes = &bytes[n..];
            at += n as u64;
        }
        Ok(())
    }
    fn sync(&self, f: &File) -> io::Result<()> {
        sync_data(f).unwrap_or_else(|| f.sync_all())
    }
    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        hard_link(from, to).unwrap_or_else(|| fs::hard_link(from, to))
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        rename(from, to).unwrap_or_else(|| fs::rename(from, to))
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        remove_file(path).unwrap_or_else(|| fs::remove_file(path))
    }
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        sync_dir(dir).unwrap_or_else(|| File::open(dir).and_then(|h| h.sync_all()))
    }
}

/// One `objects.dir@1` schedule over three keys (one nested) of put-if-absent,
/// conditional replace, delete and get, with every call faulted and host
/// failures between; `Err` names the first broken law. Each key may hold its
/// last acknowledged value or that of any write that failed since, and
/// nothing else: an acknowledged write survives a host failure, a failed one
/// is whole or absent, and a read never sees a value no write made.
fn dir_objects_schedule(seed: u64) -> Result<(), String> {
    use hirsel_object_storage::{Condition, DirObjects, ObjectStorage, Put, Replaced, content_tag};
    use std::sync::Arc;
    let mut rng = Rng(seed | 1);
    let mut odds = [0; KINDS];
    for o in &mut odds {
        *o = if rng.below(4) == 0 {
            0
        } else {
            6 + rng.below(40)
        };
    }
    install(seed.wrapping_mul(0x9E37_79B9), odds);
    let dir = tempfile::tempdir().unwrap();
    let open = || DirObjects::open_on(dir.path(), Arc::new(Modelled)).unwrap();
    let mut objects = open();
    const KEYS: [&str; 3] = ["a", "b", "nested/c"];
    // Per key: the values it may hold (None: absent); the first is the last
    // acknowledged one.
    let mut may: Vec<Vec<Option<Vec<u8>>>> = vec![vec![None]; KEYS.len()];
    for step in 0..200u64 {
        let k = rng.below(KEYS.len() as u64) as usize;
        let key = KEYS[k];
        let fresh = format!("{seed}/{step}").into_bytes();
        let fail =
            |what: &str, e: &dyn std::fmt::Debug| format!("step {step}, {what} {key}: {e:?}");
        match rng.below(5) {
            0 => match objects.put_new(key, &fresh) {
                Ok(Put::Created) => may[k] = vec![Some(fresh)],
                Ok(Put::Exists) => {}
                Err(_) => may[k].push(Some(fresh)),
            },
            1 => {
                let condition = match &may[k][0] {
                    Some(b) => Condition::Matches(content_tag(b)),
                    None => Condition::Absent,
                };
                match objects.replace(key, &fresh, condition) {
                    Ok(Replaced::Replaced(_)) => may[k] = vec![Some(fresh)],
                    Ok(Replaced::Mismatch) => {}
                    Err(_) => may[k].push(Some(fresh)),
                }
            }
            2 => match objects.delete(key) {
                Ok(()) => may[k] = vec![None],
                Err(_) => may[k].push(None),
            },
            3 => match objects.get(key) {
                Ok(v) if may[k].contains(&v) => {}
                other => return Err(fail("a read of", &other)),
            },
            _ => {
                fail_host(dir.path());
                objects = open();
                for (k, key) in KEYS.iter().enumerate() {
                    match objects.get(key) {
                        Ok(v) if may[k].contains(&v) => may[k] = vec![v],
                        other => {
                            return Err(format!(
                                "step {step}, after a host failure {key} is {other:?}, not one of {:?}",
                                may[k]
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn dir_objects_keep_what_they_acknowledged_through_disk_faults_and_host_failures() {
    let found = (1..=300).find_map(|seed| {
        dir_objects_schedule(seed * 7919)
            .err()
            .map(|e| format!("seed {seed}: {e}"))
    });
    remove();
    assert_eq!(found, None);
}
