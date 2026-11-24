//! A cache code store's objects, one immutable pack file per commit.
//!
//! A pack is `HEADER payload* index TRAILER`:
//!
//! - `HEADER`: `MAGIC` and the format version (16 bytes).
//! - each payload: one object's bytes, back to back.
//! - `index`: one `ENTRY`-byte row per object, sorted by kind code and
//!   name: kind code (u8, 7 zero bytes), the object's 32-byte name, its
//!   payload's offset and length, and the xxh3-64 of its payload.
//! - `TRAILER`: the index offset, the row count, the xxh3-64 of the header
//!   and index, and `END`.
//!
//! Objects are named by their SHA-256 (`ObjectKind::hash`), which recipes
//! carry and admission verifies; the pack's own checks are xxh3-64, which
//! detect damage and name nothing (`store::checksum`).
//!
//! Nothing is synced (cache, FOR-SAM #20): a pack is written whole to a
//! locked temporary of the driver store and renamed to its name in `packs/`.
//! After a power loss the renamed file may be short, zero-filled or torn.
//! A pack whose header, trailer or index check fails is ignored; a row whose
//! payload check fails is absent. Either way the object is a miss and the
//! next build writes it again; damaged bytes are never served. A row is
//! checked on its first use in a process; packs are never written in place,
//! so a checked row stays good while its file is open.
//!
//! Commits merge the newest packs (`merge_from`): every pack holds at least
//! `RATIO` times the bytes of the next newer one, and there are at most
//! `MAX_PACKS`. A merged pack is renamed in before the packs it absorbs
//! are removed; a reader that misses lists `packs/` again.
use super::ObjectKind;
use crate::store::{Store, checksum};
use hirsel_core_ids::ContentHash;
use rustc_hash::FxHashMap;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 12] = b"HIRSEL-PACK\0";
const VERSION: u32 = 1;
const HEADER: usize = 16;
const ENTRY: usize = 64;
const TRAILER: usize = 32;
const END: &[u8; 8] = b"HPACKEND";
const SUFFIX: &str = ".pack";
const MAX_PACKS: usize = 8;
const RATIO: u64 = 4;

#[derive(Clone, Copy)]
struct Entry {
    kind: u8,
    hash: [u8; 32],
    offset: u64,
    len: u64,
    check: u64,
}

impl Entry {
    fn key(&self) -> (u8, [u8; 32]) {
        (self.kind, self.hash)
    }

    fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.kind);
        out.extend_from_slice(&[0; 7]);
        out.extend_from_slice(&self.hash);
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.extend_from_slice(&self.len.to_le_bytes());
        out.extend_from_slice(&self.check.to_le_bytes());
    }

    fn decode(row: &[u8]) -> Self {
        let word = |at: usize| u64::from_le_bytes(row[at..at + 8].try_into().unwrap());
        Self {
            kind: row[0],
            hash: row[8..40].try_into().unwrap(),
            offset: word(40),
            len: word(48),
            check: word(56),
        }
    }
}

/// One pack's rows, read and checked from its file.
struct Pack {
    name: String,
    file: File,
    bytes: u64,
    entries: Vec<Entry>,
    /// Rows whose payload check passed in this process.
    checked: Vec<bool>,
}

impl Pack {
    /// The pack at `path`, or `None` when it is not a whole pack.
    fn open(path: &Path, name: String) -> io::Result<Option<Self>> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let bytes = file.metadata()?.len();
        let Some(entries) = read_index(&file, bytes)? else {
            return Ok(None);
        };
        let checked = vec![false; entries.len()];
        Ok(Some(Self {
            name,
            file,
            bytes,
            entries,
            checked,
        }))
    }

    fn find(&self, key: (u8, [u8; 32])) -> Option<usize> {
        self.entries.binary_search_by_key(&key, Entry::key).ok()
    }

    /// Row `at`'s payload, `None` when its check fails.
    fn payload(&mut self, at: usize) -> io::Result<Option<Vec<u8>>> {
        let entry = self.entries[at];
        let mut bytes = vec![0; entry.len as usize];
        match self.file.read_exact_at(&mut bytes, entry.offset) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        }
        if checksum(&bytes) != entry.check {
            return Ok(None);
        }
        self.checked[at] = true;
        Ok(Some(bytes))
    }

    /// Whether row `at` is whole, reading it on its first use.
    fn whole(&mut self, at: usize) -> io::Result<bool> {
        Ok(self.checked[at] || self.payload(at)?.is_some())
    }
}

/// The header, index and trailer checks of a pack `bytes` long: its rows,
/// each within the payload area, or `None`.
fn read_index(file: &File, bytes: u64) -> io::Result<Option<Vec<Entry>>> {
    if bytes < (HEADER + TRAILER) as u64 {
        return Ok(None);
    }
    let mut trailer = [0; TRAILER];
    file.read_exact_at(&mut trailer, bytes - TRAILER as u64)?;
    let word = |at: usize| u64::from_le_bytes(trailer[at..at + 8].try_into().unwrap());
    let (index, count, check) = (word(0), word(8), word(16));
    let index_end = bytes - TRAILER as u64;
    if &trailer[24..] != END
        || index < HEADER as u64
        || index > index_end
        || count.checked_mul(ENTRY as u64) != Some(index_end - index)
    {
        return Ok(None);
    }
    let mut header = vec![0; HEADER];
    file.read_exact_at(&mut header, 0)?;
    let mut rows = vec![0; (index_end - index) as usize];
    file.read_exact_at(&mut rows, index)?;
    header.extend_from_slice(&rows);
    if header[..12] != MAGIC[..]
        || u32::from_le_bytes(header[12..16].try_into().unwrap()) != VERSION
        || checksum(&header) != check
    {
        return Ok(None);
    }
    let entries = rows
        .chunks_exact(ENTRY)
        .map(Entry::decode)
        .collect::<Vec<_>>();
    let ordered = entries.windows(2).all(|w| w[0].key() < w[1].key());
    let inside = entries.iter().all(|entry| {
        entry.offset >= HEADER as u64
            && entry
                .offset
                .checked_add(entry.len)
                .is_some_and(|end| end <= index)
    });
    Ok((ordered && inside).then_some(entries))
}

/// The pack bytes of `rows` (kind code, name, payload), sorted and unique.
fn encode(rows: &[(u8, [u8; 32], &[u8])]) -> Vec<u8> {
    let payload = rows.iter().map(|(_, _, bytes)| bytes.len()).sum::<usize>();
    let mut out = Vec::with_capacity(HEADER + payload + rows.len() * ENTRY + TRAILER);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    let mut entries = Vec::with_capacity(rows.len());
    for &(kind, hash, bytes) in rows {
        entries.push(Entry {
            kind,
            hash,
            offset: out.len() as u64,
            len: bytes.len() as u64,
            check: checksum(bytes),
        });
        out.extend_from_slice(bytes);
    }
    let index = out.len();
    let mut checked = out[..HEADER].to_vec();
    for entry in &entries {
        entry.encode(&mut out);
    }
    checked.extend_from_slice(&out[index..]);
    out.extend_from_slice(&(index as u64).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    out.extend_from_slice(&checksum(&checked).to_le_bytes());
    out.extend_from_slice(END);
    out
}

/// A pack name, `<sequence>-<check>.pack`: commits number their packs in
/// order, and a merged pack takes the sequence of the newest it absorbs.
fn pack_name(sequence: u64, bytes: &[u8]) -> String {
    format!("{sequence:016x}-{:016x}{SUFFIX}", checksum(bytes))
}

fn sequence(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(SUFFIX)?;
    let (sequence, check) = stem.split_once('-')?;
    (sequence.len() == 16 && check.len() == 16)
        .then(|| u64::from_str_radix(sequence, 16).ok())
        .flatten()
        .filter(|_| u64::from_str_radix(check, 16).is_ok())
}

/// Objects put since the last commit.
#[derive(Default)]
struct Pending {
    rows: FxHashMap<(u8, [u8; 32]), Vec<u8>>,
}

/// The packs of one code store, as this process has read them, and the
/// objects it has put but not committed.
pub(super) struct Packs {
    directory: PathBuf,
    listed: bool,
    packs: Vec<Pack>,
    pending: Pending,
}

impl Packs {
    pub(super) fn new(root: &Path) -> Self {
        Self {
            directory: root.join("packs"),
            listed: false,
            packs: Vec::new(),
            pending: Pending::default(),
        }
    }

    /// Read the packs named in `packs/` that are not read yet, in sequence
    /// order. A pack removed by a merge stays readable while held.
    fn list(&mut self) -> io::Result<()> {
        self.listed = true;
        let rows = match fs::read_dir(&self.directory) {
            Ok(rows) => rows,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let mut names = Vec::new();
        for row in rows {
            if let Ok(name) = row?.file_name().into_string()
                && let Some(sequence) = sequence(&name)
                && !self.packs.iter().any(|pack| pack.name == name)
            {
                names.push((sequence, name));
            }
        }
        names.sort();
        for (_, name) in names {
            if let Some(pack) = Pack::open(&self.directory.join(&name), name)? {
                self.packs.push(pack);
            }
        }
        self.packs.sort_by_key(|pack| sequence(&pack.name));
        Ok(())
    }

    /// The whole row for `key`, newest pack first: (pack, row).
    fn find(&mut self, key: (u8, [u8; 32])) -> io::Result<Option<(usize, usize)>> {
        if !self.listed {
            self.list()?;
        }
        for at in (0..self.packs.len()).rev() {
            if let Some(row) = self.packs[at].find(key)
                && self.packs[at].whole(row)?
            {
                return Ok(Some((at, row)));
            }
        }
        Ok(None)
    }

    /// Whether this store holds a whole `kind` object named `hash` of `len`
    /// bytes, committed or pending.
    pub(super) fn contains(
        &mut self,
        kind: ObjectKind,
        hash: ContentHash,
        len: u64,
    ) -> io::Result<bool> {
        let key = (kind.code(), hash.0);
        if let Some(bytes) = self.pending.rows.get(&key) {
            return Ok(bytes.len() as u64 == len);
        }
        Ok(self
            .find(key)?
            .is_some_and(|(pack, row)| self.packs[pack].entries[row].len == len))
    }

    /// The bytes of the `kind` object named `hash`; a miss lists `packs/`
    /// again, since a merge may have replaced the packs read so far.
    pub(super) fn read(
        &mut self,
        kind: ObjectKind,
        hash: ContentHash,
    ) -> io::Result<Option<Vec<u8>>> {
        let key = (kind.code(), hash.0);
        if let Some(bytes) = self.pending.rows.get(&key) {
            return Ok(Some(bytes.clone()));
        }
        let mut found = self.find(key)?;
        if found.is_none() {
            self.list()?;
            found = self.find(key)?;
        }
        match found {
            Some((pack, row)) => self.packs[pack].payload(row),
            None => Ok(None),
        }
    }

    /// Hold `bytes` as the `kind` object `hash` until the next commit.
    pub(super) fn put(&mut self, kind: ObjectKind, hash: ContentHash, bytes: &[u8]) {
        self.pending
            .rows
            .entry((kind.code(), hash.0))
            .or_insert_with(|| bytes.to_vec());
    }

    /// Write the pending objects as one pack through `store`'s temporaries,
    /// then merge the newest packs. Nothing is synced.
    pub(super) fn commit(&mut self, store: &Store) -> io::Result<()> {
        if self.pending.rows.is_empty() {
            return Ok(());
        }
        fs::create_dir_all(&self.directory)?;
        self.listed = false;
        self.list()?;
        let pending = std::mem::take(&mut self.pending.rows);
        let mut rows = pending
            .iter()
            .map(|(&(kind, hash), bytes)| (kind, hash, bytes.as_slice()))
            .collect::<Vec<_>>();
        rows.sort_unstable_by_key(|&(kind, hash, _)| (kind, hash));
        let bytes = encode(&rows);
        let sequence = self
            .packs
            .iter()
            .filter_map(|pack| sequence(&pack.name))
            .max()
            .map_or(0, |last| last + 1);
        let name = pack_name(sequence, &bytes);
        store.publish_to(&self.directory.join(&name), &bytes)?;
        let file = File::open(self.directory.join(&name))?;
        let entries = rows
            .iter()
            .enumerate()
            .map(|(at, _)| {
                Entry::decode(&bytes[bytes.len() - TRAILER - (rows.len() - at) * ENTRY..])
            })
            .collect::<Vec<_>>();
        self.packs.push(Pack {
            name,
            file,
            bytes: bytes.len() as u64,
            checked: vec![true; entries.len()],
            entries,
        });
        self.merge(store)
    }

    /// Merge the newest packs into one while the pack before them holds less
    /// than `RATIO` times their bytes, or there are more than
    /// `MAX_PACKS`. Damaged rows are left out; the absorbed packs are
    /// removed after the merged pack is in place.
    fn merge(&mut self, store: &Store) -> io::Result<()> {
        let from = merge_from(&self.packs.iter().map(|pack| pack.bytes).collect::<Vec<_>>());
        if from + 1 >= self.packs.len() {
            return Ok(());
        }
        let mut rows = std::collections::BTreeMap::new();
        for pack in self.packs[from..].iter_mut().rev() {
            for row in 0..pack.entries.len() {
                let key = pack.entries[row].key();
                if !rows.contains_key(&key)
                    && let Some(bytes) = pack.payload(row)?
                {
                    rows.insert(key, bytes);
                }
            }
        }
        let rows = rows
            .iter()
            .map(|(&(kind, hash), bytes)| (kind, hash, bytes.as_slice()))
            .collect::<Vec<_>>();
        let bytes = encode(&rows);
        let newest = sequence(&self.packs.last().unwrap().name).unwrap();
        let name = pack_name(newest, &bytes);
        store.publish_to(&self.directory.join(&name), &bytes)?;
        let merged = Pack::open(&self.directory.join(&name), name.clone())?
            .ok_or_else(|| io::Error::other(format!("merged pack {name} does not read back")))?;
        for pack in self.packs.drain(from..) {
            if pack.name != name {
                match fs::remove_file(self.directory.join(&pack.name)) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                    _ => {}
                }
            }
        }
        self.packs.push(merged);
        Ok(())
    }
}

/// The first pack of the newest run to merge, given every pack's size
/// oldest first: the run grows while the pack before it is smaller than
/// `RATIO` times the run, or while more than `MAX_PACKS` would remain.
fn merge_from(sizes: &[u64]) -> usize {
    let mut from = sizes.len().saturating_sub(1);
    let mut run = sizes.last().copied().unwrap_or(0);
    while from > 0 && (sizes[from - 1] < RATIO.saturating_mul(run) || from + 1 > MAX_PACKS) {
        from -= 1;
        run += sizes[from];
    }
    from
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// Each record boundary of a pack: the header's end, each payload's
    /// start and end, each index row's start, the trailer's start, the end.
    pub(in crate::code_store) fn boundaries(bytes: &[u8]) -> Vec<usize> {
        let index_end = bytes.len() - TRAILER;
        let index =
            u64::from_le_bytes(bytes[index_end..index_end + 8].try_into().unwrap()) as usize;
        let mut cuts = vec![0, HEADER, index_end, bytes.len()];
        for row in bytes[index..index_end].chunks_exact(ENTRY) {
            let entry = Entry::decode(row);
            cuts.extend([entry.offset as usize, (entry.offset + entry.len) as usize]);
        }
        cuts.extend((index..index_end).step_by(ENTRY));
        cuts.sort_unstable();
        cuts.dedup();
        cuts
    }

    /// The merge rule keeps at most `MAX_PACKS` packs, each at least
    /// `RATIO` times the next newer one, over a run of commits of varied
    /// sizes. Oracle: the invariant itself, checked after each commit; a
    /// merged pack is the sum of its parts.
    #[test]
    fn merged_packs_stay_few_and_geometric() {
        let mut sizes: Vec<u64> = Vec::new();
        for commit in 0..2000u64 {
            sizes.push(1 + (commit * 7919) % 97);
            let from = merge_from(&sizes);
            let merged = sizes.drain(from..).sum();
            sizes.push(merged);
            assert!(sizes.len() <= MAX_PACKS, "{sizes:?}");
            for pair in sizes.windows(2) {
                assert!(pair[0] >= RATIO * pair[1], "commit {commit}: {sizes:?}");
            }
        }
    }
}
