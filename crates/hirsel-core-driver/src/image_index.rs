//! Published tables as flat sections of one paged byte space. A table is a row
//! heap behind little-endian u32 offsets; a map adds an open-addressed slot array
//! over its key bytes, so a lookup is one hash and a few slot reads, and a row is
//! a byte range. The space is cut into fixed pages, each a content-addressed
//! record named by the sections record the image root names: the image identity
//! covers every page, and a request reads and checks only the sections record
//! and the pages it touches, once.
use super::*;
use hirsel_core_ids::published::{Descriptor, Map, Publisher, Reader, Source, Table};
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
    reader: Arc<Reader>,
    bytes: Vec<u8>,
    tables: Vec<Section>,
}
impl Writer {
    /// The written tables read back through `reader`, the published image's own.
    pub fn new(reader: Arc<Reader>) -> Self {
        Self {
            reader,
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
            // Keep a quarter of the slots empty for terminating linear probes.
            let count = (rows.len() + rows.len().div_ceil(3))
                .max(2)
                .next_power_of_two();
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
            namespace: self.reader.namespace(),
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
        let descriptor = self.section(&rows, None)?;
        self.reader.table(descriptor)
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
            let mut row = hirsel_core_ids::published::with_lookup_key(key, <[u8]>::to_vec);
            keys.push(row.len());
            row.extend_from_slice(&encode(value)?);
            rows.push(row);
        }
        let descriptor = self.section(&rows, Some(&keys))?;
        self.reader.map(descriptor)
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
    #[inline]
    fn installed(&self) -> io::Result<&Pages> {
        if let Some(pages) = self.pages.get() {
            return Ok(pages);
        }
        self.load_sections()
    }
    #[cold]
    fn load_sections(&self) -> io::Result<&Pages> {
        let &(identity, hash) = self.root.get().ok_or_else(|| {
            io::Error::other("core image sections are read before their publication")
        })?;
        // The root names this record by hash, so damage refuses the request.
        let sections = self
            .store
            .get(hash)?
            .and_then(|bytes| {
                crate::meter::count(crate::meter::Count::ImagePageBytes, bytes.len() as u64);
                let _decode = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Decode);
                decode::<Sections>(&bytes)
            })
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
    #[inline]
    fn page<'a>(&self, pages: &'a Pages, n: usize) -> io::Result<&'a [u8]> {
        let cell = pages.pages.get(n).ok_or_else(|| damaged(pages))?;
        if let Some(page) = cell.get() {
            return Ok(page);
        }
        self.load_page(pages, n, cell)
    }
    #[cold]
    fn load_page<'a>(
        &self,
        pages: &Pages,
        n: usize,
        cell: &'a OnceLock<Arc<[u8]>>,
    ) -> io::Result<&'a [u8]> {
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
        crate::meter::count(crate::meter::Count::ImagePageBytes, bytes.len() as u64);
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
    /// Whether the bytes at `at` begin with `key`, compared in place.
    fn starts_with(&self, pages: &Pages, at: u64, key: &[u8]) -> io::Result<bool> {
        let mut at = usize::try_from(at).map_err(|_| damaged(pages))?;
        let mut done = 0;
        while done < key.len() {
            let page = self.page(pages, at / PAGE)?;
            let bytes = page.get(at % PAGE..).ok_or_else(|| damaged(pages))?;
            let n = bytes.len().min(key.len() - done);
            if n == 0 {
                return Err(damaged(pages));
            }
            if bytes[..n] != key[done..done + n] {
                return Ok(false);
            }
            done += n;
            at += n;
        }
        Ok(true)
    }
    #[inline]
    fn u64(&self, pages: &Pages, at: u64) -> io::Result<u64> {
        let offset = usize::try_from(at).map_err(|_| damaged(pages))?;
        let start = offset % PAGE;
        if let Some(bytes) = self.page(pages, offset / PAGE)?.get(start..start + 8) {
            return Ok(u64::from_le_bytes(bytes.try_into().unwrap()));
        }
        let mut bytes = [0; 8];
        self.read(pages, at, &mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }
    fn span(&self, pages: &Pages, section: &Section, index: u32) -> io::Result<(u64, usize)> {
        let offsets = self.u64(pages, section.offsets + 4 * u64::from(index))?;
        let start = offsets as u32;
        let end = (offsets >> 32) as u32;
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
#[cold]
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
        let _image = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Row);
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
    /// A map row is its key's encoding followed by its value's, a table has one
    /// key type, every query type encodes as that key type does, and an encoding
    /// of one type is prefix-free: a row that starts with the query's bytes has
    /// exactly that key.
    fn find(&self, table: Descriptor, key: &[u8]) -> io::Result<Option<u32>> {
        let _image = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Probe);
        let pages = self.installed()?;
        let section = section(pages, table)?;
        let (slots, mask) = section
            .slots
            .ok_or_else(|| io::Error::other("published table has no key index"))?;
        let hash = key_hash(key);
        let mut i = hash as u32 & mask;
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
                if len >= key.len() && self.starts_with(pages, at, key)? {
                    return Ok(Some(row));
                }
            }
            i = (i + 1) & mask;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ids::DefPath;

    /// Two string keys whose slot hashes agree in the tag word and the low two
    /// bits, so in a two-entry map they share a first slot and a tag.
    fn colliding() -> (String, String) {
        let mut seen = std::collections::HashMap::new();
        for i in 0u64.. {
            let key = format!("k{i}");
            let hash = key_hash(&encode(&key).unwrap());
            if let Some(other) = seen.insert((hash >> 32, hash & 3), key.clone()) {
                return (other, key);
            }
        }
        unreachable!()
    }

    /// Law: a query encodes as the stored key it names, for every published
    /// key type and the type it is queried by; a new pair belongs here.
    #[test]
    fn queries_encode_as_their_stored_keys() {
        use hirsel_core_front::{TyId, TyKind};
        use hirsel_core_ids::published::with_lookup_key;
        use hirsel_core_resolve::ModuleId;
        use hirsel_core_syntax::Symbol;
        fn same<K: Serialize, Q: Serialize + ?Sized>(stored: &K, query: &Q) {
            let bytes = bincode::serialize(stored).unwrap();
            assert_eq!(with_lookup_key(stored, <[u8]>::to_vec), bytes);
            assert_eq!(with_lookup_key(query, <[u8]>::to_vec), bytes);
        }
        let text: Arc<str> = Arc::from("library\0é");
        same(&text, &*text);
        let path = DefPath::from_full("library@1/parent/child").unwrap();
        same(&path, &path);
        same(&ModuleId(3), &ModuleId(3));
        same(&Symbol(9), &Symbol(9));
        same(&(ModuleId(3), Symbol(9)), &(ModuleId(3), Symbol(9)));
        same(&(ModuleId(3), 4u32), &(ModuleId(3), 4u32));
        same(&(Symbol(1), Symbol(2)), &(Symbol(1), Symbol(2)));
        let kind = TyKind::Tuple(vec![TyId(1), TyId(2)]);
        same(&kind, &kind);
    }

    /// Law: every query against the published sections answers what a linear
    /// scan of the published rows answers: the row with that key, or none.
    #[test]
    fn section_lookups_agree_with_a_linear_scan_of_the_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::image(dir.path()).unwrap();
        let namespace = ContentHash::of_bytes(b"section-lookup-law");
        let index = Arc::new(DiskIndex::new(dir.path()).unwrap());
        let reader = Reader::new(namespace, index.clone());
        let (a, b) = colliding();
        let big = vec![7u8; 3 * PAGE + 5];
        let strings: Vec<(String, Vec<u8>)> = ["a", "ab", "", "a\0"]
            .into_iter()
            .map(String::from)
            .chain([a.clone(), b.clone()])
            .enumerate()
            .map(|(i, k)| {
                (
                    k,
                    if i == 1 {
                        big.clone()
                    } else {
                        vec![i as u8; 900]
                    },
                )
            })
            .collect();
        let parent = DefPath::from_full("library@1/parent").unwrap();
        let child = DefPath::from_full("library@1/parent/child").unwrap();
        let paths = vec![(parent.clone(), 1u32), (child.clone(), 2)];
        let numbers: Vec<(u64, u64)> = (0..3000).map(|i| (i * 7919, i)).collect();
        let lone = vec![(a.clone(), vec![1u8])];
        let rows: Vec<Vec<u8>> = (0..40).map(|i| vec![i as u8; 1000 + i]).collect();

        let mut writer = Writer::new(reader.clone());
        writer.map::<String, Vec<u8>>(&Map::default()).unwrap();
        writer
            .map(&strings.iter().cloned().collect::<Map<_, _>>())
            .unwrap();
        writer
            .map(&paths.iter().cloned().collect::<Map<_, _>>())
            .unwrap();
        writer
            .map(&numbers.iter().cloned().collect::<Map<_, _>>())
            .unwrap();
        writer
            .map(&lone.iter().cloned().collect::<Map<_, _>>())
            .unwrap();
        writer.table(&Table::<Vec<u8>>::default()).unwrap();
        writer.table(&Table::from(rows.clone())).unwrap();
        writer.table(&Table::from(vec![big.clone()])).unwrap();
        let bytes = writer.bytes.clone();
        let (sections, _) = writer.finish(&store).unwrap();
        let lens: Vec<u32> = [0, strings.len(), 2, numbers.len(), 1, 0, rows.len(), 1]
            .map(|n| n as u32)
            .into();
        let hash = store.put(&encode(&sections).unwrap()).unwrap();
        // Read back through the store, as an opened image is.
        index.open(hash, hash);
        let d = |table: usize| Descriptor {
            namespace,
            table: table as u32,
            len: lens[table],
        };

        fn check<K, V>(index: &DiskIndex, d: Descriptor, rows: &[(K, V)], queries: &[K])
        where
            K: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
            V: serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
        {
            for query in rows.iter().map(|(k, _)| k).chain(queries) {
                let expected = rows.iter().find(|(k, _)| k == query).map(|(_, v)| v);
                let found = index.find(d, &encode(query).unwrap()).unwrap().map(|i| {
                    decode::<(K, V)>(&index.row(d, i).unwrap()).expect("a published row decodes")
                });
                match (&found, expected) {
                    (Some((k, v)), Some(e)) => assert!(k == query && v == e, "{query:?}"),
                    (None, None) => {}
                    _ => panic!("{query:?}: found {found:?}, a scan finds {expected:?}"),
                }
            }
        }
        let absent: Vec<String> = ["b", "abc", "\0", "aa"].map(String::from).into();
        check::<String, Vec<u8>>(&index, d(0), &[], &absent);
        check(&index, d(1), &strings, &absent);
        let other = DefPath::from_full("library@1/parent/other").unwrap();
        check(&index, d(2), &paths, &[other]);
        check(&index, d(3), &numbers, &[1, 7918, u64::MAX]);
        check(&index, d(4), &lone, &[b.clone()]);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(index.row(d(6), i as u32).unwrap(), encode(row).unwrap());
        }
        assert_eq!(index.row(d(7), 0).unwrap(), encode(&big).unwrap());
        let pages = index.installed().unwrap();
        for at in 0..=bytes.len() + 8 {
            let word = bytes.get(at..at + 8);
            let expected = word.map(|b| u64::from_le_bytes(b.try_into().unwrap()));
            assert_eq!(index.u64(pages, at as u64).ok(), expected, "u64 at {at}");
            let expected_span = word.and_then(|b| {
                let start = u32::from_le_bytes(b[..4].try_into().unwrap());
                let end = u32::from_le_bytes(b[4..].try_into().unwrap());
                end.checked_sub(start)
                    .map(|len| (u64::from(start), len as usize))
            });
            let section = Section {
                offsets: at as u64,
                heap: 0,
                slots: None,
            };
            assert_eq!(
                index.span(pages, &section, 0).ok(),
                expected_span,
                "span at {at}"
            );
        }
        assert!(index.u64(pages, u64::MAX).is_err());
    }

    /// Law: lookups racing from several threads on a cold image answer what
    /// the published rows say, the first decode of a row winning for all,
    /// whether they ask the image's map or race one request's clone of it.
    #[test]
    fn concurrent_lookups_on_a_cold_image_answer_as_the_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::image(dir.path()).unwrap();
        let namespace = ContentHash::of_bytes(b"concurrent-lookup-law");
        let index = Arc::new(DiskIndex::new(dir.path()).unwrap());
        let reader = Reader::new(namespace, index.clone());
        let numbers: Vec<(u64, u64)> = (0..3000).map(|i| (i * 7919, i * i)).collect();
        let names: Vec<String> = (0..500).map(|i| format!("name {i}")).collect();
        let mut writer = Writer::new(reader.clone());
        let map = writer
            .map(&numbers.iter().cloned().collect::<Map<_, _>>())
            .unwrap();
        let table = writer.table(&Table::from(names.clone())).unwrap();
        let (sections, _) = writer.finish(&store).unwrap();
        let hash = store.put(&encode(&sections).unwrap()).unwrap();
        index.open(hash, hash);
        let expected: Vec<Option<u64>> = (0..3000 * 7919)
            .step_by(7919 / 3)
            .map(|k| numbers.iter().find(|(n, _)| *n == k).map(|(_, v)| *v))
            .collect();
        let request = map.clone();
        std::thread::scope(|scope| {
            for t in 0..8 {
                let map = if t % 2 == 0 { &request } else { &map };
                let (table, expected, names) = (&table, &expected, &names);
                scope.spawn(move || {
                    let keys: Vec<u64> = (0..3000 * 7919).step_by(7919 / 3).collect();
                    for n in 0..keys.len() {
                        let i = (n + t * 997) % keys.len();
                        assert_eq!(map.get(&keys[i]).copied(), expected[i], "key {}", keys[i]);
                        let row = (n * 7 + t) % names.len();
                        assert_eq!(table[row], names[row]);
                    }
                });
            }
        });
        assert!(expected.iter().any(Option::is_some) && expected.iter().any(Option::is_none));
    }
}
