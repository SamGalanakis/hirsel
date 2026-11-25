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
//! A cache store's packs are blobs of the per-user content store, named by
//! the SHA-256 of their bytes, and its pack set is a record there listing
//! each pack with its sequence number. A durable store's packs, if it holds
//! any, are files `packs/<sequence>-<xxh3>.pack`, which it only reads.
//!
//! Nothing is synced (cache, FOR-SAM #20): a pack is written whole to a
//! locked temporary of the content store and renamed to its blob name, and
//! the pack set is rewritten under the store's `packs.lock`. After a power
//! loss the renamed file may be short, zero-filled or torn, or a pack the set
//! names may be gone (collected).
//! A pack whose header, trailer or index check fails is ignored; a row whose
//! payload check fails is absent. Either way the object is a miss and the
//! next build writes it again; damaged bytes are never served. A row is
//! checked on its first use in a process; packs are never written in place,
//! so a checked row stays good while its file is open.
//!
//! Commits merge the newest packs (`merge_from`): every pack holds at least
//! `RATIO` times the bytes of the next newer one, and there are at most
//! `MAX_PACKS`. A merged pack is written before the set stops naming the
//! packs it absorbs, which the collector removes once unused; a reader that
//! misses reads the set again.
use super::ObjectKind;
use crate::store::{Bare, Store, checksum};
use hirsel_core_ids::ContentHash;
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::hash::Hasher;
use std::io::{self, BufWriter, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use twox_hash::XxHash3_64;

const MAGIC: &[u8; 12] = b"HIRSEL-PACK\0";
const VERSION: u32 = 1;
const HEADER: usize = 16;
const ENTRY: usize = 64;
const TRAILER: usize = 32;
const END: &[u8; 8] = b"HPACKEND";
const SUFFIX: &str = ".pack";
const MAX_PACKS: usize = 8;
const RATIO: u64 = 4;
type Key = (u8, [u8; 32]);

#[derive(Clone, Copy)]
struct Entry {
    kind: u8,
    hash: [u8; 32],
    offset: u64,
    len: u64,
    check: u64,
}

impl Entry {
    fn key(&self) -> Key {
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
    sequence: u64,
    /// The pack's blob, in a cache store.
    blob: Bare,
    file: File,
    bytes: u64,
    entries: Vec<Entry>,
    /// Rows whose payload check passed in this process.
    checked: Vec<bool>,
}

impl Pack {
    /// The pack at `path`, or `None` when it is not a whole pack.
    fn open(path: &Path, name: String, sequence: u64, blob: Bare) -> io::Result<Option<Self>> {
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
        crate::store::collect::touch(&file);
        Ok(Some(Self {
            name,
            sequence,
            blob,
            file,
            bytes,
            entries,
            checked,
        }))
    }

    fn find(&self, key: Key) -> Option<usize> {
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

/// Write sorted, unique rows directly to the unpublished pack, a blob of
/// `store`. Keep the index in memory and hash the payloads as they are
/// written.
fn write_pack<B: AsRef<[u8]>>(
    store: &Store,
    sequence: u64,
    rows: impl Iterator<Item = io::Result<(Key, B)>>,
) -> io::Result<Pack> {
    let (path, blob, bytes, entries) = store.publish_to(|file| {
        let mut file = BufWriter::new(file);
        let mut hash = XxHash3_64::default();
        let mut name = Sha256::new();
        let mut write = |bytes: &[u8]| {
            file.write_all(bytes)?;
            hash.write(bytes);
            name.update(bytes);
            Ok::<(), io::Error>(())
        };
        let mut index = Vec::new();
        index.extend_from_slice(MAGIC);
        index.extend_from_slice(&VERSION.to_le_bytes());
        write(&index)?;
        let mut offset = HEADER as u64;
        let mut entries = Vec::new();
        for row in rows {
            let ((kind, hash), bytes) = row?;
            let bytes = bytes.as_ref();
            let entry = Entry {
                kind,
                hash,
                offset,
                len: bytes.len() as u64,
                check: checksum(bytes),
            };
            offset += entry.len;
            entry.encode(&mut index);
            entries.push(entry);
            write(bytes)?;
        }
        write(&index[HEADER..])?;
        write(&offset.to_le_bytes())?;
        write(&(entries.len() as u64).to_le_bytes())?;
        write(&checksum(&index).to_le_bytes())?;
        write(END)?;
        file.flush()?;
        let bytes = offset + (index.len() - HEADER + TRAILER) as u64;
        let blob = Bare {
            address: ContentHash(name.finalize().into()),
            check: hash.finish(),
        };
        let path = store.blob_path(blob.address);
        Ok((path.clone(), (path, blob, bytes, entries)))
    })?;
    let file = File::open(&path)?;
    Ok(Pack {
        name: blob_name(blob),
        sequence,
        blob,
        file,
        bytes,
        checked: vec![true; entries.len()],
        entries,
    })
}

fn blob_name(blob: Bare) -> String {
    crate::native::selection(blob.address)
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
    rows: FxHashMap<Key, Arc<[u8]>>,
}

/// Where a store's packs are listed.
enum Source {
    /// A durable store's `packs/` directory, read only.
    Directory(PathBuf),
    /// A cache store's pack set: a record of the content store under `set`.
    Set { store: Store, set: ContentHash },
}

/// The packs of one code store, as this process has read them, and the
/// objects it has put but not committed.
pub(crate) struct Packs {
    source: Source,
    listed: bool,
    packs: Vec<Pack>,
    pending: Pending,
}

const SET_SCHEMA: u32 = 1;

impl Packs {
    /// A durable store's packs, in `root/packs`.
    pub(super) fn new(root: &Path) -> Self {
        Self::of(Source::Directory(root.join("packs")))
    }

    /// The cache store `name`'s packs in the content store `store`.
    pub(super) fn cache(store: Store, name: &str) -> Self {
        let set = ContentHash::of_parts("hirsel.code-pack-set.v1", [name.as_bytes()]);
        Self::of(Source::Set { store, set })
    }

    fn of(source: Source) -> Self {
        Self {
            source,
            listed: false,
            packs: Vec::new(),
            pending: Pending::default(),
        }
    }

    /// List the packs again on the next lookup: another process may have
    /// committed or merged since this one last listed them.
    pub(crate) fn relist(&mut self) {
        self.listed = false;
    }

    /// The packs listed now: name, sequence, file and blob.
    fn listing(&self) -> io::Result<Vec<(String, u64, PathBuf, Bare)>> {
        let mut listed = Vec::new();
        match &self.source {
            Source::Directory(directory) => {
                let rows = match fs::read_dir(directory) {
                    Ok(rows) => rows,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(listed),
                    Err(error) => return Err(error),
                };
                for row in rows {
                    if let Ok(name) = row?.file_name().into_string()
                        && let Some(sequence) = sequence(&name)
                    {
                        let path = directory.join(&name);
                        listed.push((name, sequence, path, Bare::default()));
                    }
                }
            }
            Source::Set { store, set } => {
                let set = store
                    .get(*set)?
                    .and_then(|bytes| crate::decode::<(u32, Vec<(u64, Bare)>)>(&bytes))
                    .filter(|(schema, _)| *schema == SET_SCHEMA)
                    .map(|(_, packs)| packs)
                    .unwrap_or_default();
                for (sequence, blob) in set {
                    listed.push((
                        blob_name(blob),
                        sequence,
                        store.blob_path(blob.address),
                        blob,
                    ));
                }
            }
        }
        Ok(listed)
    }

    /// Read the packs listed that are not read yet, in sequence order, and
    /// let go of those a merge absorbed. A merge writes its pack before the
    /// listing stops naming the packs it absorbs, so their rows are listed.
    fn list(&mut self) -> io::Result<()> {
        self.listed = true;
        let listed = self.listing()?;
        self.packs
            .retain(|pack| listed.iter().any(|(name, ..)| *name == pack.name));
        let mut names = listed
            .into_iter()
            .filter(|(name, ..)| !self.packs.iter().any(|pack| pack.name == *name))
            .collect::<Vec<_>>();
        names.sort_by_key(|(_, sequence, ..)| *sequence);
        for (name, sequence, path, blob) in names {
            if let Some(pack) = Pack::open(&path, name, sequence, blob)? {
                self.packs.push(pack);
            }
        }
        self.packs.sort_by_key(|pack| pack.sequence);
        Ok(())
    }

    /// The whole row for `key`, newest pack first: (pack, row).
    fn find(&mut self, key: Key) -> io::Result<Option<(usize, usize)>> {
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

    /// The bytes of the `kind` object named `hash`; a miss lists the packs
    /// again, since a merge may have replaced the packs read so far.
    pub(super) fn read(
        &mut self,
        kind: ObjectKind,
        hash: ContentHash,
    ) -> io::Result<Option<Vec<u8>>> {
        let key = (kind.code(), hash.0);
        if let Some(bytes) = self.pending.rows.get(&key) {
            return Ok(Some(bytes.to_vec()));
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
    pub(super) fn put(
        &mut self,
        kind: ObjectKind,
        hash: ContentHash,
        bytes: impl FnOnce() -> Arc<[u8]>,
    ) {
        self.pending
            .rows
            .entry((kind.code(), hash.0))
            .or_insert_with(bytes);
    }

    /// Write the pending objects as one pack blob, merge the newest packs
    /// and rewrite the pack set, under the content store's `packs.lock`.
    /// Nothing is synced. A durable store's objects are written by `put`.
    pub(super) fn commit(&mut self) -> io::Result<()> {
        let Source::Set { store, set } = &self.source else {
            return Ok(());
        };
        if self.pending.rows.is_empty() {
            return Ok(());
        }
        let (store, set) = (store.clone(), *set);
        let _held = store.hold("packs.lock")?;
        self.listed = false;
        self.list()?;
        let mut rows = self.pending.rows.iter().collect::<Vec<_>>();
        rows.sort_unstable_by_key(|(key, _)| **key);
        let sequence = self
            .packs
            .iter()
            .map(|pack| pack.sequence)
            .max()
            .map_or(0, |last| last + 1);
        let pack = write_pack(
            &store,
            sequence,
            rows.into_iter().map(|(&key, bytes)| Ok((key, bytes))),
        )?;
        self.pending.rows.clear();
        self.packs.push(pack);
        self.merge(&store)?;
        let listed: Vec<(u64, Bare)> = self.packs.iter().map(|p| (p.sequence, p.blob)).collect();
        store.put_keyed(set, &crate::encode(&(SET_SCHEMA, listed))?)
    }

    /// Merge the newest packs into one while the pack before them holds less
    /// than `RATIO` times their bytes, or there are more than
    /// `MAX_PACKS`. Damaged rows are left out; the set stops naming the
    /// absorbed packs once the merged pack is in place.
    fn merge(&mut self, store: &Store) -> io::Result<()> {
        let from = merge_from(&self.packs.iter().map(|pack| pack.bytes).collect::<Vec<_>>());
        if from + 1 >= self.packs.len() {
            return Ok(());
        }
        let newest = self.packs.last().unwrap().sequence;
        let packs = &mut self.packs[from..];
        let keys = packs
            .iter()
            .flat_map(|pack| pack.entries.iter().map(Entry::key))
            .collect::<std::collections::BTreeSet<_>>();
        let mut payload = |key| {
            for pack in packs.iter_mut().rev() {
                if let Some(row) = pack.find(key)
                    && let Some(bytes) = pack.payload(row)?
                {
                    return Ok(Some((key, bytes)));
                }
            }
            Ok(None)
        };
        let rows = keys.into_iter().filter_map(|key| payload(key).transpose());
        let merged = write_pack(store, newest, rows)?;
        self.packs.truncate(from);
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
