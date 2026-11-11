//! `authority.log@1`: segment files and a lock file in a directory.
//!
//! Levels: `process` (the entry is written to the operating system, so it
//! survives the process's crash) and `host` (the entry is synchronized, so it
//! survives the operating system or power failing while the disk survives).
//!
//! A segment, named by the position of its first entry, holds frames
//! `len:u32 check:u32 entry` where `check` is the first four bytes of the
//! SHA-256 of `len` and the entry's position, so a damaged length or a frame
//! out of place is corruption, not a torn tail. Only a frame cut short at the
//! end of the last segment is torn.
//!
//! An advance takes the lock file's open-file-description lock (`flock`),
//! compares the active segment's length with the length the writer's own last
//! append left, appends the frame with one `pwrite`, releases the lock and, at
//! `host`, issues one `fdatasync`. A different length means another claim
//! appended: `Fenced`, with no effect. It never reads.
//!
//! Whatever starts a new segment, an advance or a claim, first syncs the full
//! one, and a segment's first frame is written only once a directory sync has
//! named it, so a host failure never cuts the history short in front of an
//! entry it keeps.

use crate::envelope::{self, Chain};
use crate::{
    Advanced, Authority, AuthorityError, Claim, Claimed, Error, Level, Record, live, owner_id,
};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

pub fn levels() -> Vec<String> {
    vec!["process".to_string(), "host".to_string()]
}

const PROCESS: Level = Level(0);
const HOST: Level = Level(1);
const FRAME: u64 = 8;
/// A segment this long or longer takes no more entries.
pub const SEGMENT_MAX: u64 = 256 << 20;

/// The file-system calls that change the history's files. The log harness
/// injects its faults here and models what a host failure keeps.
mod disk {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::unix::fs::FileExt;
    use std::path::Path;

    pub(super) fn write_at(f: &File, bytes: &[u8], at: u64) -> io::Result<usize> {
        #[cfg(test)]
        if let Some(r) = crate::harness_log::write_at(f, bytes, at) {
            return r;
        }
        f.write_at(bytes, at)
    }

    pub(super) fn write_all_at(f: &File, mut bytes: &[u8], mut at: u64) -> io::Result<()> {
        while !bytes.is_empty() {
            match write_at(f, bytes, at)? {
                0 => return Err(io::ErrorKind::WriteZero.into()),
                n => {
                    bytes = &bytes[n..];
                    at += n as u64;
                }
            }
        }
        Ok(())
    }

    pub(super) fn sync_data(f: &File) -> io::Result<()> {
        #[cfg(test)]
        if let Some(r) = crate::harness_log::sync_data(f) {
            return r;
        }
        f.sync_data()
    }

    pub(super) fn set_len(f: &File, len: u64) -> io::Result<()> {
        #[cfg(test)]
        if let Some(r) = crate::harness_log::set_len(f, len) {
            return r;
        }
        f.set_len(len)
    }

    pub(super) fn create_new(path: &Path) -> io::Result<File> {
        #[cfg(test)]
        if let Some(r) = crate::harness_log::create_new(path) {
            return r;
        }
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
    }

    pub(super) fn sync_dir(dir: &Path) -> io::Result<()> {
        #[cfg(test)]
        if let Some(r) = crate::harness_log::sync_dir(dir) {
            return r;
        }
        File::open(dir).and_then(|d| d.sync_all())
    }
}

fn check(len: u32, position: u64) -> [u8; 4] {
    let mut h = Sha256::new();
    h.update(len.to_le_bytes());
    h.update(position.to_le_bytes());
    h.finalize()[..4].try_into().unwrap()
}

fn frame(position: u64, entry: &[u8]) -> Vec<u8> {
    let len = entry.len() as u32;
    let mut out = Vec::with_capacity(entry.len() + FRAME as usize);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&check(len, position));
    out.extend_from_slice(entry);
    out
}

fn io(e: std::io::Error) -> AuthorityError {
    AuthorityError::io(e.kind())
}

fn segment_name(first: u64) -> String {
    format!("{first:020}.seg")
}

/// Where the entry at a position lies.
#[derive(Clone, Copy, Debug)]
struct Spot {
    segment: u64,
    offset: u64,
    len: u32,
}

/// The frames found so far, in position order.
#[derive(Default)]
struct Index {
    spots: Vec<Spot>,
    /// Segments by first position, and how far each has been scanned.
    segments: Vec<(u64, u64)>,
    /// A cut-short frame at the end of the last segment: where it starts.
    torn: Option<u64>,
}

pub struct Log {
    dir: PathBuf,
    lock: File,
    owner: u64,
    floor: Level,
    levels: Vec<String>,
    index: Index,
    /// The records followed so far.
    chain: Chain,
    /// The writer's segment, opened for writing.
    active: Option<(u64, File)>,
    /// A segment this long or longer takes no more entries.
    pub(crate) segment_max: u64,
}

impl Log {
    pub fn open(dir: &Path, floor: Level) -> std::io::Result<Self> {
        fs::create_dir_all(dir.join("objects"))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("lock"))?;
        Ok(Log {
            dir: dir.to_path_buf(),
            lock,
            owner: owner_id(),
            floor,
            levels: levels(),
            index: Index::default(),
            chain: Chain::default(),
            active: None,
            segment_max: SEGMENT_MAX,
        })
    }

    /// Take the lock file's open-file-description lock until the guard drops.
    fn lock(&self) -> Result<Guard, AuthorityError> {
        let fd = self.lock.as_raw_fd();
        // SAFETY: flock on a descriptor this handle owns and keeps open
        // longer than the guard.
        if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
            return Err(io(std::io::Error::last_os_error()));
        }
        Ok(Guard(fd))
    }

    fn segment_path(&self, first: u64) -> PathBuf {
        self.dir.join(segment_name(first))
    }

    /// Extend the index to the end of the history as it is now.
    fn scan(&mut self) -> Result<(), Error> {
        let mut names: Vec<u64> = Vec::new();
        for e in fs::read_dir(&self.dir).map_err(|e| Error::Storage(io(e)))? {
            let e = e.map_err(|e| Error::Storage(io(e)))?;
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(stem) = name.strip_suffix(".seg")
                && stem.len() == 20
                && let Ok(n) = stem.parse::<u64>()
            {
                names.push(n);
            }
        }
        names.sort_unstable();
        for &n in &names {
            if !self.index.segments.iter().any(|&(f, _)| f == n) {
                self.index.segments.push((n, 0));
            }
        }
        self.index.torn = None;
        let count = self.index.segments.len();
        for i in 0..count {
            let (first, scanned) = self.index.segments[i];
            let next = self.index.spots.len() as u64 + 1;
            if scanned == 0 && first != next {
                return Err(Error::Corrupt(segment_name(first)));
            }
            // Only the part after what earlier scans indexed is read, and of
            // each frame only its header.
            let f = File::open(self.segment_path(first)).map_err(|e| Error::Storage(io(e)))?;
            let size = f.metadata().map_err(|e| Error::Storage(io(e)))?.len();
            let mut at = scanned;
            while at < size {
                let position = self.index.spots.len() as u64 + 1;
                if size - at < FRAME {
                    break;
                }
                let mut header = [0u8; FRAME as usize];
                f.read_exact_at(&mut header, at)
                    .map_err(|e| Error::Storage(io(e)))?;
                let len = u32::from_le_bytes(header[..4].try_into().unwrap());
                if header[4..] != check(len, position) {
                    return Err(Error::Corrupt(format!("{} at {at}", segment_name(first))));
                }
                if size - at - FRAME < len as u64 {
                    break;
                }
                self.index.spots.push(Spot {
                    segment: first,
                    offset: at + FRAME,
                    len,
                });
                at += FRAME + len as u64;
            }
            if at < size {
                if i + 1 < count {
                    return Err(Error::Corrupt(format!("{} at {at}", segment_name(first))));
                }
                self.index.torn = Some(at);
            }
            self.index.segments[i].1 = at;
        }
        Ok(())
    }

    fn entry(&self, position: u64) -> Result<Vec<u8>, Error> {
        let s = self.index.spots[position as usize - 1];
        let f = File::open(self.segment_path(s.segment)).map_err(|e| Error::Storage(io(e)))?;
        let mut out = vec![0; s.len as usize];
        f.read_exact_at(&mut out, s.offset)
            .map_err(|e| Error::Storage(io(e)))?;
        Ok(out)
    }

    fn sync_dir(&self) -> Result<(), AuthorityError> {
        disk::sync_dir(&self.dir).map_err(io)
    }

    fn create_segment(&self, first: u64) -> Result<File, AuthorityError> {
        let f = disk::create_new(&self.segment_path(first)).map_err(io)?;
        self.sync_dir()?;
        Ok(f)
    }

    fn writer(&mut self, segment: u64) -> Result<&File, AuthorityError> {
        if self.active.as_ref().map(|a| a.0) != Some(segment) {
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(self.segment_path(segment))
                .map_err(io)?;
            self.active = Some((segment, f));
        }
        Ok(&self.active.as_ref().unwrap().1)
    }

    /// Whether the history still ends where the writer's own last append left it.
    fn unchanged(&mut self, c: &Claim) -> Result<bool, AuthorityError> {
        let len = self.writer(c.segment)?.metadata().map_err(io)?.len();
        if len != c.length {
            return Ok(false);
        }
        // A claim starts a new segment only after a full one.
        Ok(c.length < self.segment_max || !self.segment_path(c.end + 1).exists())
    }
}

struct Guard(std::os::unix::io::RawFd);

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: unlock the descriptor locked by `Log::lock`, still open.
        unsafe { libc::flock(self.0, libc::LOCK_UN) };
    }
}

impl Authority for Log {
    fn levels(&self) -> &[String] {
        &self.levels
    }
    fn floor(&self) -> Level {
        self.floor
    }
    fn claim(&mut self, attempt: [u8; 16], fresh: [u8; 32]) -> Result<Claimed, Error> {
        let _guard = self.lock().map_err(Error::Storage)?;
        self.scan()?;
        let last = self.index.segments.last().copied();
        if let (Some(torn), Some((first, _))) = (self.index.torn, last) {
            let f = OpenOptions::new()
                .write(true)
                .open(self.segment_path(first))
                .map_err(|e| Error::Storage(io(e)))?;
            disk::set_len(&f, torn)
                .and_then(|_| disk::sync_data(&f))
                .map_err(|e| Error::Storage(io(e)))?;
            self.index.torn = None;
        }
        let end = self.index.spots.len() as u64;
        self.follow_to(end)?;
        let entry = envelope::claimed(&self.chain.tip, attempt, fresh);
        let position = end + 1;
        let (segment, length) = match last {
            Some((first, len)) if len < self.segment_max => {
                // A segment holding no frame may be one whose creator's
                // directory sync failed: name it durably before its first frame.
                if len == 0 {
                    self.sync_dir().map_err(Error::Storage)?;
                }
                (first, len)
            }
            _ => {
                // The full segment's tail is synced before the next one starts,
                // as an advance does: a host failure could otherwise cut it
                // short under the claim, which is durable at `host`.
                if let Some((first, _)) = last {
                    self.writer(first)
                        .and_then(|f| disk::sync_data(f).map_err(io))
                        .map_err(Error::Storage)?;
                }
                let f = self.create_segment(position).map_err(Error::Storage)?;
                self.active = Some((position, f));
                self.index.segments.push((position, 0));
                (position, 0)
            }
        };
        let bytes = frame(position, &entry);
        let f = self.writer(segment).map_err(Error::Storage)?;
        disk::write_all_at(f, &bytes, length)
            .and_then(|_| disk::sync_data(f))
            .map_err(|e| Error::Storage(io(e)))?;
        self.chain
            .follow(&entry)
            .expect("a claim record follows its tip");
        self.index.spots.push(Spot {
            segment,
            offset: length + FRAME,
            len: entry.len() as u32,
        });
        let length = length + bytes.len() as u64;
        self.index.segments.last_mut().unwrap().1 = length;
        let mut claim = Claim::new(self.owner, attempt, &self.chain.tip, 2);
        claim.segment = segment;
        claim.length = length;
        Ok(Claimed {
            claim,
            end: position,
        })
    }

    fn advance(&mut self, c: &mut Claim, from: u64, payload: Vec<u8>, at: Level) -> Advanced {
        if !live(self.owner, c) || from != c.end {
            c.dead = true;
            return Advanced::Fenced;
        }
        if crate::oversized(&payload) {
            return Advanced::Refused(AuthorityError::io(ErrorKind::InvalidInput));
        }
        let position = from + 1;
        let entry = envelope::appended(&c.tip(), &payload);
        let bytes = frame(position, &entry);
        {
            let _guard = match self.lock() {
                Ok(g) => g,
                Err(e) => return Advanced::Refused(e),
            };
            match self.unchanged(c) {
                Ok(true) => {}
                Ok(false) => {
                    c.dead = true;
                    return Advanced::Fenced;
                }
                Err(e) => return Advanced::Refused(e),
            }
            if c.length >= self.segment_max {
                // Everything in the full segment becomes durable at `host`
                // before the next one starts, so `durable` syncs one file.
                if c.durable[HOST.0 as usize] < c.end {
                    let synced = self
                        .writer(c.segment)
                        .and_then(|f| disk::sync_data(f).map_err(io));
                    if let Err(e) = synced {
                        c.dead = true;
                        return Advanced::Undecided(e);
                    }
                    c.durable[HOST.0 as usize] = c.end;
                }
                match self.create_segment(position) {
                    Ok(f) => self.active = Some((position, f)),
                    Err(AuthorityError::Io(5)) => {
                        c.dead = true;
                        return Advanced::Fenced;
                    }
                    Err(e) => return Advanced::Refused(e),
                }
                c.segment = position;
                c.length = 0;
            }
            let f = match self.writer(c.segment) {
                Ok(f) => f,
                Err(e) => return Advanced::Refused(e),
            };
            match disk::write_at(f, &bytes, c.length) {
                Ok(n) if n == bytes.len() => {}
                result => {
                    // Settle by the file's length: unchanged means no effect.
                    let len = f.metadata().map(|m| m.len());
                    let e = match result {
                        Err(e) => io(e),
                        Ok(_) => AuthorityError::io(ErrorKind::WriteZero),
                    };
                    if matches!(len, Ok(l) if l == c.length) {
                        return Advanced::Refused(e);
                    }
                    c.dead = true;
                    return Advanced::Undecided(e);
                }
            }
        }
        c.length += bytes.len() as u64;
        c.end = position;
        c.digest = envelope::digest(&entry);
        c.durable[PROCESS.0 as usize] = position;
        if at >= HOST {
            let synced = self
                .writer(c.segment)
                .and_then(|f| disk::sync_data(f).map_err(io));
            if let Err(e) = synced {
                c.dead = true;
                return Advanced::Undecided(e);
            }
            c.durable[HOST.0 as usize] = position;
        }
        Advanced::Advanced(position)
    }

    fn durable(&mut self, c: &mut Claim, at: Level) -> Result<u64, Error> {
        if !live(self.owner, c) {
            return Err(Error::Fenced);
        }
        {
            let _guard = self.lock().map_err(Error::Storage)?;
            if !self.unchanged(c).map_err(Error::Storage)? {
                c.dead = true;
                return Err(Error::Fenced);
            }
        }
        let i = at.0 as usize;
        if at >= HOST && c.durable[i] < c.end {
            if let Err(e) = self
                .writer(c.segment)
                .and_then(|f| disk::sync_data(f).map_err(io))
            {
                c.dead = true;
                return Err(Error::Storage(e));
            }
            c.durable[i] = c.end;
        }
        Ok(c.durable[i])
    }

    fn read(&mut self, from: u64, at: Level, max: u64) -> Result<Vec<Record>, Error> {
        {
            // Under the lock no append is half done, so every frame header the
            // scan sees is whole and a failed check is corruption.
            let _guard = self.lock().map_err(Error::Storage)?;
            self.scan()?;
        }
        let end = self.index.spots.len() as u64;
        let last = from.saturating_add(max).min(end);
        if at >= HOST {
            // Every frame the index holds lies within the length scanned, so
            // after the sync none of them is one a host crash could lose.
            let mut synced: Vec<u64> = Vec::new();
            for p in from + 1..=last {
                let s = self.index.spots[p as usize - 1].segment;
                if !synced.contains(&s) {
                    File::open(self.segment_path(s))
                        .and_then(|f| disk::sync_data(&f))
                        .map_err(|e| Error::Storage(io(e)))?;
                    synced.push(s);
                }
            }
        }
        // Each record is followed the first time it is read and checked against
        // its digest every later time.
        let mut out = Vec::new();
        for p in from.min(self.chain.end()) + 1..=last {
            let bytes = self.entry(p)?;
            let record = if p > self.chain.end() {
                self.chain.follow(&bytes)
            } else {
                self.chain.check(p, &bytes)
            }
            .map_err(|b| crate::bad(b, p))?;
            if p > from {
                out.push(record);
            }
        }
        Ok(out)
    }

    fn get(&mut self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        let path = self.object_path(name)?;
        match fs::read(path) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Storage(io(e))),
        }
    }

    fn put(&mut self, name: &str, data: Vec<u8>) -> Result<(), Error> {
        let path = self.object_path(name)?;
        let parent = path.parent().unwrap().to_path_buf();
        fs::create_dir_all(&parent).map_err(|e| Error::Storage(io(e)))?;
        let tmp = parent.join(format!(".tmp-{}-{}", std::process::id(), owner_id()));
        let created = (|| {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o644)
                .open(&tmp)?;
            f.write_all(&data)?;
            f.sync_data()?;
            fs::hard_link(&tmp, &path)
        })();
        let _ = fs::remove_file(&tmp);
        match created {
            Ok(()) => File::open(&parent)
                .and_then(|d| d.sync_all())
                .map_err(|e| Error::Storage(io(e))),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => match fs::read(&path) {
                Ok(old) if old == data => Ok(()),
                Ok(_) => Err(Error::Corrupt(name.to_string())),
                Err(e) => Err(Error::Storage(io(e))),
            },
            Err(e) => Err(Error::Storage(io(e))),
        }
    }

    fn reclaim(&mut self, c: &mut Claim) -> Result<(), Error> {
        // Retention entries arrive with retention; until then nothing is
        // below a floor, and removing nothing is always within the rule.
        self.durable(c, PROCESS).map(|_| ())
    }
}

impl Log {
    /// Follow the history's records up to position `end`.
    fn follow_to(&mut self, end: u64) -> Result<(), Error> {
        while self.chain.end() < end {
            let p = self.chain.end() + 1;
            let entry = self.entry(p)?;
            self.chain.follow(&entry).map_err(|b| crate::bad(b, p))?;
        }
        Ok(())
    }

    fn object_path(&self, name: &str) -> Result<PathBuf, Error> {
        let ok = |s: &str| {
            !s.is_empty()
                && !s.starts_with('.')
                && s.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
                })
        };
        if !name.split('/').all(ok) {
            return Err(Error::Refused(AuthorityError::io(ErrorKind::InvalidInput)));
        }
        Ok(self.dir.join("objects").join(name))
    }
}
