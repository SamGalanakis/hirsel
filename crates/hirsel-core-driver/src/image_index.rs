//! Published tables as flat sections of one paged byte space. A table is a row
//! heap behind little-endian u32 offsets; a map adds an open-addressed slot array
//! over its key bytes, so a lookup is one hash and a few slot reads, and a row is
//! a byte range. The space is cut into fixed pages, each a content-addressed
//! record named by the sections record the image root names: the image identity
//! covers every page, and a request reads and checks only the sections record
//! and the pages it touches, once.
use super::*;
use hirsel_core_ids::published::{Descriptor, Map, Publisher, Source, Table};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) const PAGE: usize = 4096;

/// Every page of an image's sections and each published table's place in them,
/// by its descriptor's table number.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Sections {
    pub pages: Vec<ContentHash>,
    pub tables: Vec<Section>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) struct Section {
    /// Position of the table's length plus one u32 row offsets, relative to `heap`.
    offsets: u64,
    heap: u64,
    /// A map's slots: `mask + 1` u64 slots, each the key hash's high word over
    /// the row number plus one; zero is empty.
    slots: Option<(u64, u32)>,
}

/// A fixed function of the key bytes: the published slots depend on it, and the
/// compiler identity in the image root fixes the compiler that computes it.
fn key_hash(key: &[u8]) -> u64 {
    const K: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut h = (key.len() as u64).wrapping_mul(K);
    let mut words = key.chunks_exact(8);
    for word in &mut words {
        h = (h.rotate_left(23) ^ u64::from_le_bytes(word.try_into().unwrap())).wrapping_mul(K);
    }
    let rest = words.remainder();
    if !rest.is_empty() {
        let mut word = [0; 8];
        word[..rest.len()].copy_from_slice(rest);
        h = (h.rotate_left(23) ^ u64::from_le_bytes(word)).wrapping_mul(K);
    }
    // The low bits pick the slot: fold the well-mixed high bits into them.
    h ^= h >> 29;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^ (h >> 32)
}

pub(crate) struct Writer {
    namespace: ContentHash,
    bytes: Vec<u8>,
    tables: Vec<Section>,
}
impl Writer {
    pub fn new(namespace: ContentHash) -> Self {
        Self {
            namespace,
            bytes: Vec::new(),
            tables: Vec::new(),
        }
    }
    fn u32(&mut self, value: usize) -> io::Result<()> {
        let value = u32::try_from(value).map_err(|_| too_large())?;
        self.bytes.extend_from_slice(&value.to_le_bytes());
        Ok(())
    }
    /// `keys[i]` is the length of row `i`'s leading key bytes, for a map.
    fn section(&mut self, rows: &[Vec<u8>], keys: Option<&[usize]>) -> io::Result<Descriptor> {
        let offsets = self.bytes.len() as u64;
        let mut at = 0;
        self.u32(0)?;
        for row in rows {
            at += row.len();
            self.u32(at)?;
        }
        let heap = self.bytes.len() as u64;
        for row in rows {
            self.bytes.extend_from_slice(row);
        }
        let slots = keys.map(|keys| {
            let count = (rows.len() * 2).max(2).next_power_of_two();
            let mask = count - 1;
            let mut slots = vec![0u64; count];
            for (row, (bytes, &key)) in rows.iter().zip(keys).enumerate() {
                let hash = key_hash(&bytes[..key]);
                let mut i = hash as usize & mask;
                while slots[i] != 0 {
                    i = (i + 1) & mask;
                }
                slots[i] = (hash & !0xffff_ffff) | (row as u64 + 1);
            }
            let at = self.bytes.len() as u64;
            for slot in slots {
                self.bytes.extend_from_slice(&slot.to_le_bytes());
            }
            (at, mask as u32)
        });
        self.tables.push(Section {
            offsets,
            heap,
            slots,
        });
        let d = Descriptor {
            namespace: self.namespace,
            table: self.tables.len() as u32 - 1,
            len: u32::try_from(rows.len()).map_err(|_| too_large())?,
        };
        Ok(d)
    }
    /// Write the pages and give the sections the image root names, with the
    /// pages as published, to install in the publishing request's index.
    pub fn finish(self, store: &Store) -> io::Result<(Sections, Vec<Arc<[u8]>>)> {
        let mut pages = Vec::new();
        let mut hashes = Vec::new();
        for page in self.bytes.chunks(PAGE) {
            hashes.push(store.put(page)?);
            pages.push(Arc::from(page));
        }
        Ok((
            Sections {
                pages: hashes,
                tables: self.tables,
            },
            pages,
        ))
    }
}
fn too_large() -> io::Error {
    io::Error::other("compiler resource refusal: a published table exceeds its 4 GiB section")
}
impl Publisher for Writer {
    fn table<T: Clone + Serialize + serde::de::DeserializeOwned + Send + Sync + 'static>(
        &mut self,
        table: &Table<T>,
    ) -> io::Result<Table<T>> {
        let rows = table.iter().map(encode).collect::<io::Result<Vec<_>>>()?;
        Table::from_descriptor(self.section(&rows, None)?)
    }
    fn map<
        K: Clone
            + Ord
            + std::hash::Hash
            + Serialize
            + serde::de::DeserializeOwned
            + Send
            + Sync
            + 'static,
        V: Clone + Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    >(
        &mut self,
        map: &Map<K, V>,
    ) -> io::Result<Map<K, V>> {
        let mut entries = map.iter().collect::<Vec<_>>();
        entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
        let mut rows = Vec::with_capacity(entries.len());
        let mut keys = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            // A row is its key's lookup bytes followed by its value's.
            let mut row = encode(key)?;
            keys.push(row.len());
            row.extend_from_slice(&encode(value)?);
            rows.push(row);
        }
        Map::from_descriptor(self.section(&rows, Some(&keys))?)
    }
}

struct Pages {
    sections: Sections,
    pages: Box<[OnceLock<Arc<[u8]>>]>,
    image: String,
}
pub(crate) struct DiskIndex {
    store: Store,
    /// The image and its sections record, once the image has a root.
    root: OnceLock<(ContentHash, ContentHash)>,
    pages: OnceLock<Pages>,
    checked: AtomicUsize,
    rows: AtomicUsize,
}
fn image_name(identity: ContentHash) -> String {
    identity.0.iter().map(|b| format!("{b:02x}")).collect()
}
impl DiskIndex {
    pub fn new(path: &Path) -> io::Result<Self> {
        Ok(Self {
            store: Store::image(path)?,
            root: OnceLock::new(),
            pages: OnceLock::new(),
            checked: AtomicUsize::new(0),
            rows: AtomicUsize::new(0),
        })
    }
    /// An opened image's sections are read on the first table access.
    pub fn open(&self, identity: ContentHash, sections: ContentHash) {
        let _ = self.root.set((identity, sections));
    }
    /// The sections this process just published, with their pages.
    pub fn publish(&self, identity: ContentHash, sections: Sections, written: Vec<Arc<[u8]>>) {
        let _ = self.pages.set(Pages {
            pages: written.into_iter().map(OnceLock::from).collect(),
            sections,
            image: image_name(identity),
        });
    }
    fn installed(&self) -> io::Result<&Pages> {
        if let Some(pages) = self.pages.get() {
            return Ok(pages);
        }
        let &(identity, hash) = self.root.get().ok_or_else(|| {
            io::Error::other("core image sections are read before their publication")
        })?;
        // The root names this record by hash, so damage refuses the request.
        let sections = self
            .store
            .get(hash)?
            .and_then(|bytes| decode::<Sections>(&bytes))
            .ok_or_else(|| {
                io::Error::other(format!(
                    "core image {} is damaged: its sections record is missing or fails its hash check; rebuild the image",
                    image_name(identity)
                ))
            })?;
        let pages = (0..sections.pages.len()).map(|_| OnceLock::new()).collect();
        Ok(self.pages.get_or_init(|| Pages {
            sections,
            pages,
            image: image_name(identity),
        }))
    }
    fn page<'a>(&self, pages: &'a Pages, n: usize) -> io::Result<&'a [u8]> {
        let cell = pages.pages.get(n).ok_or_else(|| damaged(pages))?;
        if let Some(page) = cell.get() {
            return Ok(page);
        }
        // The page's hash is in the sections record the image root names, so a
        // page that reads back different bytes refuses the request: there is no
        // other path to its rows.
        let bytes = self.store.get(pages.sections.pages[n])?.ok_or_else(|| {
            io::Error::other(format!(
                "core image {} is damaged: section page {n} is missing or fails its hash check; rebuild the image",
                pages.image
            ))
        })?;
        self.checked.fetch_add(1, Ordering::Relaxed);
        Ok(cell.get_or_init(|| Arc::from(bytes)))
    }
    fn read(&self, pages: &Pages, at: u64, out: &mut [u8]) -> io::Result<()> {
        let mut at = usize::try_from(at).map_err(|_| damaged(pages))?;
        let mut done = 0;
        while done < out.len() {
            let page = self.page(pages, at / PAGE)?;
            let bytes = page.get(at % PAGE..).ok_or_else(|| damaged(pages))?;
            let n = bytes.len().min(out.len() - done);
            if n == 0 {
                return Err(damaged(pages));
            }
            out[done..done + n].copy_from_slice(&bytes[..n]);
            done += n;
            at += n;
        }
        Ok(())
    }
    fn u32(&self, pages: &Pages, at: u64) -> io::Result<u32> {
        let mut bytes = [0; 4];
        self.read(pages, at, &mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }
    fn u64(&self, pages: &Pages, at: u64) -> io::Result<u64> {
        let mut bytes = [0; 8];
        self.read(pages, at, &mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }
    fn span(&self, pages: &Pages, section: &Section, index: u32) -> io::Result<(u64, usize)> {
        let start = self.u32(pages, section.offsets + 4 * u64::from(index))?;
        let end = self.u32(pages, section.offsets + 4 * u64::from(index) + 4)?;
        let len = end.checked_sub(start).ok_or_else(|| damaged(pages))?;
        Ok((section.heap + u64::from(start), len as usize))
    }
    pub fn checked_pages(&self) -> usize {
        self.checked.load(Ordering::Relaxed)
    }
    pub fn decoded_rows(&self) -> usize {
        self.rows.load(Ordering::Relaxed)
    }
}
fn damaged(pages: &Pages) -> io::Error {
    io::Error::other(format!(
        "core image {} is damaged: a section reference is outside its pages; rebuild the image",
        pages.image
    ))
}
fn section(pages: &Pages, table: Descriptor) -> io::Result<&Section> {
    pages
        .sections
        .tables
        .get(table.table as usize)
        .ok_or_else(|| damaged(pages))
}
impl Source for DiskIndex {
    fn row(&self, table: Descriptor, index: u32) -> io::Result<Vec<u8>> {
        let pages = self.installed()?;
        if index >= table.len {
            return Err(damaged(pages));
        }
        let (at, len) = self.span(pages, section(pages, table)?, index)?;
        let mut bytes = vec![0; len];
        self.read(pages, at, &mut bytes)?;
        self.rows.fetch_add(1, Ordering::Relaxed);
        Ok(bytes)
    }
    fn find(&self, table: Descriptor, key: &[u8]) -> io::Result<Option<u32>> {
        let pages = self.installed()?;
        let section = section(pages, table)?;
        let (slots, mask) = section
            .slots
            .ok_or_else(|| io::Error::other("published table has no key index"))?;
        let hash = key_hash(key);
        let mut i = hash as u32 & mask;
        let mut candidate = vec![0; key.len()];
        for _ in 0..=mask {
            let slot = self.u64(pages, slots + 8 * u64::from(i))?;
            if slot == 0 {
                return Ok(None);
            }
            if slot >> 32 == hash >> 32 {
                let row = (slot as u32).checked_sub(1).ok_or_else(|| damaged(pages))?;
                if row >= table.len {
                    return Err(damaged(pages));
                }
                let (at, len) = self.span(pages, section, row)?;
                if len >= key.len() {
                    self.read(pages, at, &mut candidate)?;
                    if candidate == key {
                        return Ok(Some(row));
                    }
                }
            }
            i = (i + 1) & mask;
        }
        Ok(None)
    }
}

// Old-against-new differential for F-image (temporary; not committed).
