//! Published tables as rows packed whole into content-addressed page records.
//! A section's rows fill consecutive pages of its size class in placement
//! order, each row a length-prefixed item that never crosses a page, so a row
//! demand reads one page. A map adds a slot array sorted by key hash, packed
//! the same way, so a lookup reads a slot page and its row's page. The rows
//! other packages can name are placed first, so a dependent request reads the
//! pages of those rows. The sections record the image root names lists every
//! page's hash and where each section's items are: the image identity covers
//! every page, and a request reads and checks only the sections record and the
//! pages it touches, once.
use super::*;
use hirsel_core_ids::published::{
    Descriptor, Map, Publisher, Reader, Source, Table, decode_row, encode_row,
};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A section's page size class. A hash-probed section of small rows, and a
/// section with outward rows, is read densely and takes large pages; any
/// other is read at a few scattered rows and takes small ones.
#[derive(Clone, Copy)]
enum Class {
    Small,
    Large,
}
impl Class {
    fn size(self) -> usize {
        match self {
            Class::Small => 2048,
            Class::Large => 8192,
        }
    }
}
/// The mean encoded row size at or below which a keyed section is dense.
const DENSE_ROW: usize = 17;

/// Every page of an image's sections and each published table's place in them,
/// by its descriptor's table number.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Sections {
    pub pages: Vec<ContentHash>,
    pub tables: Vec<Section>,
}
impl Sections {
    /// The record the image root names.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        encode_row(self)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Section {
    rows: Run,
    slots: Option<Slots>,
    outward: Option<Outward>,
}

/// Items on consecutive pages. The first page is `first`, where the run starts
/// at that page's item `at`; further pages are `next`, `next + 1`, …, each
/// from its first item. `starts[k]` is the run position of page `k`'s first
/// item of the run.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Run {
    first: u32,
    at: u32,
    next: u32,
    starts: Vec<u32>,
}

/// A slot holds a hash tag above the row number plus one; zero is empty. Slots
/// are in key hash order, each at or after its key's home slot, so a search
/// from a key's home ends at an empty slot or a greater tag. Run item `i`
/// holds slots `i * per` on.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Slots {
    run: Run,
    homes: u32,
    len: u32,
    per: u32,
    wide: bool,
}
impl Slots {
    fn row_bits(&self) -> u32 {
        if self.wide { 32 } else { 16 }
    }
    fn width(&self) -> usize {
        if self.wide { 8 } else { 4 }
    }
}
/// The slot a key's search starts at: its hash's place among `homes`.
fn home(hash: u64, homes: u32) -> u32 {
    ((u128::from(hash) * u128::from(homes)) >> 64) as u32
}

/// The marked rows sit first, in row order, and the rest after them, in row
/// order. `ranks[w]` counts the marked rows before row `64 * w`.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Outward {
    bits: Vec<u8>,
    ranks: Vec<u32>,
    count: u32,
}
impl Outward {
    fn new(marks: &[bool]) -> io::Result<Self> {
        let mut bits = vec![0u8; marks.len().div_ceil(8)];
        let mut ranks = Vec::with_capacity(marks.len().div_ceil(64));
        let mut count = 0u32;
        for (row, &mark) in marks.iter().enumerate() {
            if row % 64 == 0 {
                ranks.push(count);
            }
            if mark {
                bits[row / 8] |= 1 << (row % 8);
                count = count.checked_add(1).ok_or_else(too_large)?;
            }
        }
        Ok(Self { bits, ranks, count })
    }
    /// Row `row`'s place in its section's run.
    fn position(&self, row: u32) -> Option<u32> {
        let word = row as usize / 64;
        let below = *self.ranks.get(word)?;
        let from = self.bits.get(word * 8..)?;
        let mut bytes = [0u8; 8];
        let have = from.len().min(8);
        bytes[..have].copy_from_slice(&from[..have]);
        let bits = u64::from_le_bytes(bytes);
        let bit = row % 64;
        let rank = below.checked_add((bits & ((1u64 << bit) - 1)).count_ones())?;
        if bits >> bit & 1 == 1 {
            Some(rank)
        } else {
            self.count.checked_add(row.checked_sub(rank)?)
        }
    }
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
    // The high bits pick the home slot and the tag: mix the low bits into them.
    h ^= h >> 29;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^ (h >> 32)
}

/// A page is its items, each a varint length and its bytes, then the offset of
/// every `STRIDE`th item after the first as a u16, then the item count as a
/// u16: an item is found from its stride mark past fewer than `STRIDE` others.
const STRIDE: u32 = 16;
fn trailer(items: usize) -> usize {
    2 + 2 * (items.saturating_sub(1) / STRIDE as usize)
}
fn varint_len(mut value: usize) -> usize {
    let mut n = 1;
    while value >= 0x80 {
        value >>= 7;
        n += 1;
    }
    n
}
fn put_varint(out: &mut Vec<u8>, mut value: usize) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

pub(crate) struct Writer {
    reader: Arc<Reader>,
    pages: Vec<Vec<u8>>,
    /// Each page's item count and the offset of every `STRIDE`th item.
    items: Vec<(u16, Vec<u16>)>,
    /// The page still taking items in each class.
    open: [Option<u32>; 2],
    tables: Vec<Section>,
}
impl Writer {
    /// The written tables read back through `reader`, the published image's own.
    pub fn new(reader: Arc<Reader>) -> Self {
        Self {
            reader,
            pages: Vec::new(),
            items: Vec::new(),
            open: [None; 2],
            tables: Vec::new(),
        }
    }
    /// Place `items` in order. A run that fills a page starts on a fresh one;
    /// a smaller run shares the class's open page. An item larger than a page
    /// is a page of its own.
    fn run(&mut self, class: Class, items: &[&[u8]]) -> io::Result<Run> {
        let size = class.size();
        let total: usize = items.iter().map(|i| varint_len(i.len()) + i.len()).sum();
        if total >= size {
            self.open[class as usize] = None;
        }
        let mut run = Run::default();
        let mut current = None;
        for (position, item) in items.iter().enumerate() {
            let position = u32::try_from(position).map_err(|_| too_large())?;
            let need = varint_len(item.len()) + item.len();
            let fits = |p: u32| {
                let n = self.items[p as usize].0 as usize;
                n < usize::from(u16::MAX)
                    && self.pages[p as usize].len() + need + trailer(n + 1) <= size
            };
            let page = match self.open[class as usize].filter(|&p| fits(p)) {
                Some(page) => page,
                None => {
                    let page = u32::try_from(self.pages.len()).map_err(|_| too_large())?;
                    self.pages.push(Vec::new());
                    self.items.push((0, Vec::new()));
                    page
                }
            };
            self.open[class as usize] = (need + trailer(1) <= size).then_some(page);
            let bytes = &mut self.pages[page as usize];
            let (count, marks) = &mut self.items[page as usize];
            let index = u32::from(*count);
            if index > 0 && index % STRIDE == 0 {
                // A page of several items is at most a class size long.
                marks.push(bytes.len() as u16);
            }
            *count += 1;
            put_varint(bytes, item.len());
            bytes.extend_from_slice(item);
            if current.is_none() {
                run.first = page;
                run.at = index;
                run.starts.push(0);
            } else if current != Some(page) {
                if run.starts.len() == 1 {
                    run.next = page;
                }
                run.starts.push(position);
            }
            current = Some(page);
        }
        Ok(run)
    }
    /// `keys[i]` is the length of row `i`'s leading key bytes, for a map;
    /// `outward` marks the rows placed first.
    fn section(
        &mut self,
        rows: &[Vec<u8>],
        keys: Option<&[usize]>,
        outward: Option<&[bool]>,
    ) -> io::Result<Descriptor> {
        let len = u32::try_from(rows.len()).map_err(|_| too_large())?;
        let bytes: usize = rows.iter().map(Vec::len).sum();
        let class = if outward.is_some() || (keys.is_some() && bytes <= DENSE_ROW * rows.len()) {
            Class::Large
        } else {
            Class::Small
        };
        let order: Vec<&[u8]> = match outward {
            Some(marks) => {
                if marks.len() != rows.len() {
                    return Err(io::Error::other(
                        "outward marks do not match the published rows",
                    ));
                }
                let marked = |want: bool| {
                    rows.iter()
                        .zip(marks)
                        .filter(move |&(_, &m)| m == want)
                        .map(|(r, _)| &r[..])
                };
                marked(true).chain(marked(false)).collect()
            }
            None => rows.iter().map(|r| &r[..]).collect(),
        };
        let rows_run = self.run(class, &order)?;
        let slots = match keys {
            Some(keys) => Some(self.slots(class, rows, keys)?),
            None => None,
        };
        self.tables.push(Section {
            rows: rows_run,
            slots,
            outward: outward.map(Outward::new).transpose()?,
        });
        Ok(Descriptor {
            namespace: self.reader.namespace(),
            table: self.tables.len() as u32 - 1,
            len,
        })
    }
    fn slots(&mut self, class: Class, rows: &[Vec<u8>], keys: &[usize]) -> io::Result<Slots> {
        // Nine slots in ten are full: a sorted search stops at a greater tag.
        let homes = u32::try_from((rows.len() * 10).div_ceil(9).max(1)).map_err(|_| too_large())?;
        let wide = u16::try_from(rows.len()).is_err();
        let mut hashed: Vec<(u64, u64)> = rows
            .iter()
            .zip(keys)
            .enumerate()
            .map(|(row, (bytes, &key))| (key_hash(&bytes[..key]), row as u64))
            .collect();
        hashed.sort_unstable();
        let mut slots = Slots {
            run: Run::default(),
            homes,
            len: 0,
            per: 0,
            wide,
        };
        let row_bits = slots.row_bits();
        let mut words: Vec<u64> = Vec::with_capacity(homes as usize);
        for (hash, row) in hashed {
            let home = home(hash, homes) as usize;
            if words.len() < home {
                words.resize(home, 0);
            }
            words.push((hash >> (64 - row_bits)) << row_bits | (row + 1));
        }
        let width = slots.width();
        let per = (class.size() - varint_len(class.size()) - trailer(1)) / width;
        let chunks: Vec<Vec<u8>> = words
            .chunks(per)
            .map(|chunk| {
                chunk
                    .iter()
                    .flat_map(|&w| w.to_le_bytes().into_iter().take(width))
                    .collect()
            })
            .collect();
        let items: Vec<&[u8]> = chunks.iter().map(|c| &c[..]).collect();
        slots.run = self.run(class, &items)?;
        slots.len = u32::try_from(words.len()).map_err(|_| too_large())?;
        slots.per = per as u32;
        Ok(slots)
    }
    /// Write the pages and give the sections the image root names, with the
    /// pages as published, to install in the publishing request's index.
    pub fn finish(self, store: &Store) -> io::Result<(Sections, Vec<Arc<[u8]>>)> {
        let mut hashes = Vec::with_capacity(self.pages.len());
        let mut pages = Vec::with_capacity(self.pages.len());
        for (mut page, (count, marks)) in self.pages.into_iter().zip(self.items) {
            for mark in marks {
                page.extend_from_slice(&mark.to_le_bytes());
            }
            page.extend_from_slice(&count.to_le_bytes());
            hashes.push(store.put(&page)?);
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
fn encode_rows<'a, T: Serialize + 'a>(
    rows: impl Iterator<Item = &'a T>,
) -> io::Result<Vec<Vec<u8>>> {
    rows.map(encode_row).collect()
}
/// A map's rows in key order: each is its key's lookup bytes followed by its
/// value's, with the key byte lengths and the values.
fn map_rows<K: Ord + std::hash::Hash + Serialize, V: Serialize>(
    map: &Map<K, V>,
) -> io::Result<(Vec<Vec<u8>>, Vec<usize>, Vec<&V>)> {
    let mut entries = map.iter().collect::<Vec<_>>();
    entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let mut rows = Vec::with_capacity(entries.len());
    let mut keys = Vec::with_capacity(entries.len());
    let mut values = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        let mut row = hirsel_core_ids::published::with_lookup_key(key, <[u8]>::to_vec);
        keys.push(row.len());
        row.extend_from_slice(&encode_row(value)?);
        rows.push(row);
        values.push(value);
    }
    Ok((rows, keys, values))
}
impl Publisher for Writer {
    fn table<T: Clone + Serialize + serde::de::DeserializeOwned + Send + Sync + 'static>(
        &mut self,
        table: &Table<T>,
    ) -> io::Result<Table<T>> {
        let descriptor = self.section(&encode_rows(table.iter())?, None, None)?;
        self.reader.table(descriptor)
    }
    fn indexed_table<T: Clone + Serialize + serde::de::DeserializeOwned + Send + Sync + 'static>(
        &mut self,
        table: &Table<T>,
    ) -> io::Result<Table<T>> {
        let rows = encode_rows(table.iter())?;
        let keys = rows.iter().map(Vec::len).collect::<Vec<_>>();
        let descriptor = self.section(&rows, Some(&keys), None)?;
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
        let (rows, keys, _) = map_rows(map)?;
        let descriptor = self.section(&rows, Some(&keys), None)?;
        self.reader.map(descriptor)
    }
    fn outward_table<T: Clone + Serialize + serde::de::DeserializeOwned + Send + Sync + 'static>(
        &mut self,
        table: &Table<T>,
        outward: &[bool],
    ) -> io::Result<Table<T>> {
        let descriptor = self.section(&encode_rows(table.iter())?, None, Some(outward))?;
        self.reader.table(descriptor)
    }
    fn outward_map<
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
        outward: &dyn Fn(&V) -> bool,
    ) -> io::Result<Map<K, V>> {
        let (rows, keys, values) = map_rows(map)?;
        let marks: Vec<bool> = values.into_iter().map(outward).collect();
        let descriptor = self.section(&rows, Some(&keys), Some(&marks))?;
        self.reader.map(descriptor)
    }
}

/// A loaded page: its bytes, where its items end, and how many there are.
struct Page {
    bytes: Arc<[u8]>,
    end: usize,
    count: u32,
}
impl Page {
    fn parse(bytes: Arc<[u8]>) -> Option<Self> {
        let at = bytes.len().checked_sub(2)?;
        let count = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        let end = at.checked_sub(trailer(usize::from(count)) - 2)?;
        Some(Self {
            bytes,
            end,
            count: u32::from(count),
        })
    }
    #[inline]
    fn varint(items: &[u8], at: usize) -> Option<(usize, usize)> {
        let first = *items.get(at)?;
        if first < 0x80 {
            return Some((usize::from(first), at + 1));
        }
        let mut len = usize::from(first & 0x7f);
        let mut at = at + 1;
        let mut shift = 7;
        loop {
            let byte = *items.get(at)?;
            at += 1;
            len |= usize::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some((len, at));
            }
            shift += 7;
            if shift > 28 {
                return None;
            }
        }
    }
    #[inline]
    fn item(&self, index: u32) -> Option<&[u8]> {
        if index >= self.count {
            return None;
        }
        let items = &self.bytes[..self.end];
        let mark = (index / STRIDE) as usize;
        let mut at = match mark {
            0 => 0,
            _ => {
                let m = self.end + 2 * (mark - 1);
                usize::from(u16::from_le_bytes([self.bytes[m], self.bytes[m + 1]]))
            }
        };
        for _ in 0..index % STRIDE {
            let (len, start) = Self::varint(items, at)?;
            at = start.checked_add(len)?;
        }
        let (len, start) = Self::varint(items, at)?;
        items.get(start..start.checked_add(len)?)
    }
}

struct Pages {
    sections: Sections,
    pages: Box<[OnceLock<Page>]>,
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
            // A page that does not parse stays unloaded and reads as damaged.
            pages: written
                .into_iter()
                .map(|bytes| Page::parse(bytes).map_or_else(OnceLock::new, OnceLock::from))
                .collect(),
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
                decode_row::<Sections>(&bytes).ok()
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
    fn page<'a>(&self, pages: &'a Pages, n: u32) -> io::Result<&'a Page> {
        let cell = pages.pages.get(n as usize).ok_or_else(|| damaged(pages))?;
        if let Some(page) = cell.get() {
            return Ok(page);
        }
        self.load_page(pages, n as usize, cell)
    }
    #[cold]
    fn load_page<'a>(
        &self,
        pages: &Pages,
        n: usize,
        cell: &'a OnceLock<Page>,
    ) -> io::Result<&'a Page> {
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
        let page = Page::parse(Arc::from(bytes)).ok_or_else(|| damaged(pages))?;
        Ok(cell.get_or_init(|| page))
    }
    /// The item at `position` of `run`.
    #[inline]
    fn item<'a>(&self, pages: &'a Pages, run: &Run, position: u32) -> io::Result<&'a [u8]> {
        let k = match run.starts.as_slice() {
            [] => 0,
            [only] => usize::from(*only <= position),
            starts => starts.partition_point(|&s| s <= position),
        };
        let place = match k {
            0 => None,
            1 => position.checked_add(run.at).map(|index| (run.first, index)),
            k => run
                .next
                .checked_add(k as u32 - 2)
                .map(|page| (page, position - run.starts[k - 1])),
        };
        let (page, index) = place.ok_or_else(|| damaged(pages))?;
        self.page(pages, page)?
            .item(index)
            .ok_or_else(|| damaged(pages))
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
/// Row `index`'s bytes, which the caller has checked is below the table's length.
fn row_bytes<'a>(
    index: &DiskIndex,
    pages: &'a Pages,
    section: &Section,
    row: u32,
) -> io::Result<&'a [u8]> {
    let position = match &section.outward {
        None => row,
        Some(outward) => outward.position(row).ok_or_else(|| damaged(pages))?,
    };
    index.item(pages, &section.rows, position)
}
impl Source for DiskIndex {
    fn row(&self, table: Descriptor, index: u32) -> io::Result<Vec<u8>> {
        let _image = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Row);
        let pages = self.installed()?;
        if index >= table.len {
            return Err(damaged(pages));
        }
        let bytes = row_bytes(self, pages, section(pages, table)?, index)?.to_vec();
        self.rows.fetch_add(1, Ordering::Relaxed);
        Ok(bytes)
    }
    /// A row starts with its key's encoding. Every query encodes as the table's
    /// key type, whose encoding is prefix-free, so a matching prefix is an
    /// exact key match.
    fn find(&self, table: Descriptor, key: &[u8]) -> io::Result<Option<u32>> {
        let _image = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Probe);
        let pages = self.installed()?;
        let section = section(pages, table)?;
        let slots = section
            .slots
            .as_ref()
            .ok_or_else(|| io::Error::other("published table has no key index"))?;
        if slots.per == 0 {
            return Err(damaged(pages));
        }
        let hash = key_hash(key);
        let row_bits = slots.row_bits();
        let row_mask = (1u64 << row_bits) - 1;
        let tag = hash >> (64 - row_bits);
        let width = slots.width();
        let mut i = home(hash, slots.homes);
        while i < slots.len {
            // Slot `i` and the rest of its chunk, scanned as fixed-width words.
            let n = i / slots.per;
            let bytes = self.item(pages, &slots.run, n)?;
            let first = (i - n * slots.per) as usize;
            let count = (slots.len - n * slots.per).min(slots.per) as usize;
            let words = bytes
                .get(first * width..count * width)
                .ok_or_else(|| damaged(pages))?;
            for word in words.chunks_exact(width) {
                let slot = match *word {
                    [a, b, c, d] => u64::from(u32::from_le_bytes([a, b, c, d])),
                    [a, b, c, d, e, f, g, h] => u64::from_le_bytes([a, b, c, d, e, f, g, h]),
                    _ => return Err(damaged(pages)),
                };
                if slot == 0 {
                    return Ok(None);
                }
                match (slot >> row_bits).cmp(&tag) {
                    std::cmp::Ordering::Greater => return Ok(None),
                    std::cmp::Ordering::Less => {}
                    std::cmp::Ordering::Equal => {
                        let row = ((slot & row_mask) as u32)
                            .checked_sub(1)
                            .filter(|&row| row < table.len)
                            .ok_or_else(|| damaged(pages))?;
                        if row_bytes(self, pages, section, row)?.starts_with(key) {
                            return Ok(Some(row));
                        }
                    }
                }
            }
            i = (n + 1) * slots.per;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ids::{DefPath, published::decode_row};

    /// Two string keys whose hashes agree in the compact tag, so in a small map
    /// they share a home slot and a tag and only the row bytes tell them apart.
    fn colliding() -> (String, String) {
        let mut seen = std::collections::HashMap::new();
        for i in 0u64.. {
            let key = format!("k{i}");
            let hash = key_hash(&encode_row(&key).unwrap());
            if let Some(other) = seen.insert(hash >> 48, key.clone()) {
                return (other, key);
            }
        }
        unreachable!()
    }

    /// Law: a query encodes as the stored key it names, for every published
    /// key type and the type it is queried by; a new pair belongs here.
    #[test]
    fn queries_encode_as_their_stored_keys() {
        use bincode::Options;
        use hirsel_core_front::{TyId, TyKind};
        use hirsel_core_ids::published::with_lookup_key;
        use hirsel_core_resolve::ModuleId;
        use hirsel_core_syntax::Symbol;
        fn same<K: Serialize, Q: Serialize + ?Sized>(stored: &K, query: &Q) {
            let bytes = bincode::DefaultOptions::new().serialize(stored).unwrap();
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
        let big = vec![7u8; 3 * Class::Large.size() + 5];
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
        let spellings = [b.as_str(), "aa", "", a.as_str(), "a"];
        let names = writer
            .indexed_table(&Table::from(spellings.map(Arc::<str>::from).to_vec()))
            .unwrap();
        // Outward rows move ahead of the others within and across words of 64
        // marks, and an entry is found by its key wherever its row is placed.
        let marks: Vec<bool> = (0..200)
            .map(|i| i % 3 == 0 || (64..70).contains(&i))
            .collect();
        let marked: Vec<Vec<u8>> = (0..200).map(|i| vec![i as u8; 1 + i % 40]).collect();
        let outward = writer
            .outward_table(&Table::from(marked.clone()), &marks)
            .unwrap();
        let outward_map = writer
            .outward_map(
                &numbers.iter().cloned().collect::<Map<_, _>>(),
                &|v: &u64| v % 5 == 0,
            )
            .unwrap();
        let (sections, _) = writer.finish(&store).unwrap();
        let hash = store.put(&sections.encode().unwrap()).unwrap();
        // Read back through the store, as an opened image is.
        index.open(hash, hash);
        let lens: Vec<u32> = [0, strings.len(), 2, numbers.len(), 1, 0, rows.len(), 1]
            .map(|n| n as u32)
            .into();
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
                let found = index
                    .find(d, &encode_row(query).unwrap())
                    .unwrap()
                    .map(|i| {
                        decode_row::<(K, V)>(&index.row(d, i).unwrap())
                            .expect("a published row decodes")
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
            assert_eq!(index.row(d(6), i as u32).unwrap(), encode_row(row).unwrap());
        }
        assert_eq!(index.row(d(7), 0).unwrap(), encode_row(&big).unwrap());
        for (ordinal, spelling) in spellings.iter().enumerate() {
            assert_eq!(names.find_published(*spelling), Some(ordinal as u32));
            assert_eq!(&*names[ordinal], *spelling);
        }
        assert_eq!(names.find_published("ab"), None);
        for (i, row) in marked.iter().enumerate() {
            assert_eq!(&outward[i], row, "outward row {i}");
        }
        for &(key, value) in numbers.iter().step_by(7) {
            assert_eq!(outward_map.get(&key), Some(&value));
        }
        assert_eq!(outward_map.get(&1), None);
    }

    #[test]
    fn a_wide_slot_preserves_a_row_ordinal_above_the_compact_range() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::image(dir.path()).unwrap();
        let namespace = ContentHash::of_bytes(b"wide-slot-law");
        let index = Arc::new(DiskIndex::new(dir.path()).unwrap());
        let reader = Reader::new(namespace, index.clone());
        let mut writer = Writer::new(reader);
        let high_row = u32::from(u16::MAX);
        let map = writer
            .map(&(0..=high_row).map(|key| (key, key * 3)).collect())
            .unwrap();
        let (sections, _) = writer.finish(&store).unwrap();
        let hash = store.put(&sections.encode().unwrap()).unwrap();
        index.open(hash, hash);
        assert_eq!(map.get(&0), Some(&0));
        assert_eq!(map.get(&high_row), Some(&(high_row * 3)));
        assert_eq!(map.get(&(high_row + 1)), None);
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
        let hash = store.put(&sections.encode().unwrap()).unwrap();
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
