//! Indexed package records: bounded-depth numeric arrays and bounded hash buckets.
//! Every pointer and absence is covered by immutable content-addressed roots.
use super::*;
use hirsel_core_ids::published::{Descriptor, Map, Publisher, Source, Table};
use std::sync::Mutex;
const BUCKET_LIMIT: usize = 32;
const ROWS_PER_PAGE: usize = 16;
const PAGE_HEADER: usize = (ROWS_PER_PAGE + 1) * 8;

fn row_pages(store: &Store, rows: &[Vec<u8>]) -> io::Result<ContentHash> {
    let mut pages = Vec::new();
    for (i, chunk) in rows.chunks(ROWS_PER_PAGE).enumerate() {
        let mut bytes = vec![0; PAGE_HEADER];
        for (slot, row) in chunk.iter().enumerate() {
            let start = bytes.len() as u64;
            bytes[slot * 8..(slot + 1) * 8].copy_from_slice(&start.to_le_bytes());
            bytes.extend_from_slice(row);
        }
        let end = bytes.len() as u64;
        bytes[chunk.len() * 8..(chunk.len() + 1) * 8].copy_from_slice(&end.to_le_bytes());
        pages.push((i as u32, store.put(&bytes)?));
    }
    array(store, &pages, array_shift(pages.last().map_or(0, |p| p.0)))
}
// The private index codec is fixed-width little-endian bincode. Four words
// have exactly the old 32-byte representation, with fewer serde visits.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct IndexHash([u64; 4]);
impl From<ContentHash> for IndexHash {
    fn from(hash: ContentHash) -> Self {
        Self(std::array::from_fn(|i| {
            u64::from_le_bytes(hash.0[i * 8..i * 8 + 8].try_into().unwrap())
        }))
    }
}
impl From<IndexHash> for ContentHash {
    fn from(hash: IndexHash) -> Self {
        let mut bytes = [0; 32];
        for (chunk, word) in bytes.chunks_exact_mut(8).zip(hash.0) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        Self(bytes)
    }
}
#[derive(Serialize, Deserialize)]
enum Node {
    Array(Box<[Option<IndexHash>; 16]>),
    Names {
        buckets: IndexHash,
        mask: u32,
        salt: u32,
    },
    Bucket(Vec<(IndexHash, u32)>),
}
fn array_shift(maximum: u32) -> u32 {
    (31 - maximum.max(1).leading_zeros()) / 4 * 4
}
fn array(store: &Store, entries: &[(u32, ContentHash)], shift: u32) -> io::Result<ContentHash> {
    let mut children = [None; 16];
    for (n, child) in children.iter_mut().enumerate() {
        let rows: Vec<_> = entries
            .iter()
            .copied()
            .filter(|&(i, _)| ((i >> shift) & 15) as usize == n)
            .collect();
        if !rows.is_empty() {
            *child = Some(if shift == 0 {
                rows[0].1
            } else {
                array(store, &rows, shift - 4)?
            });
        }
    }
    store.put(&encode(&Node::Array(Box::new(
        children.map(|h| h.map(IndexHash::from)),
    )))?)
}
fn bucket_index(key: ContentHash, mask: u32, salt: u32) -> u32 {
    let hash = if salt == 0 {
        key
    } else {
        ContentHash::of_parts("hirsel.name-index.salt", [&key.0[..], &salt.to_le_bytes()])
    };
    u32::from_le_bytes(hash.0[..4].try_into().unwrap()) & mask
}
fn names(store: &Store, entries: &[(ContentHash, u32)]) -> io::Result<ContentHash> {
    let maximum = entries.len().max(1).next_power_of_two();
    let mut count = entries.len().div_ceil(8).max(1).next_power_of_two();
    loop {
        let mask = (count - 1) as u32;
        // Hash construction is a producer operation. A fixed bucket bound makes
        // every consumer lookup independent of the number of library declarations.
        for salt in 0..128 {
            let mut groups = vec![vec![]; count];
            for &(key, value) in entries {
                groups[bucket_index(key, mask, salt) as usize].push((key, value));
            }
            if groups.iter().any(|g| g.len() > BUCKET_LIMIT) {
                continue;
            }
            let mut rows = vec![];
            for (i, group) in groups.into_iter().enumerate() {
                if !group.is_empty() {
                    rows.push((
                        i as u32,
                        store.put(&encode(&Node::Bucket(
                            group.into_iter().map(|(h, i)| (h.into(), i)).collect(),
                        ))?)?,
                    ));
                }
            }
            let buckets = array(store, &rows, array_shift(mask))?;
            return store.put(&encode(&Node::Names {
                buckets: buckets.into(),
                mask,
                salt,
            })?);
        }
        if count == maximum {
            return Err(io::Error::other(
                "compiler resource refusal: package hash-index construction exhausted",
            ));
        }
        count *= 2;
    }
}
pub(crate) struct DiskIndex {
    pub store: Store,
    nodes: Mutex<BTreeMap<ContentHash, Arc<Node>>>,
    pages: Mutex<BTreeMap<ContentHash, Arc<Vec<u8>>>>,
    reads: std::sync::atomic::AtomicUsize,
    rows: std::sync::atomic::AtomicUsize,
}
impl DiskIndex {
    pub fn new(path: &Path) -> io::Result<Self> {
        Ok(Self {
            store: Store::new(path)?,
            nodes: Mutex::default(),
            pages: Mutex::default(),
            reads: Default::default(),
            rows: Default::default(),
        })
    }
    fn node(&self, hash: ContentHash) -> io::Result<Arc<Node>> {
        if let Some(node) = self.nodes.lock().unwrap().get(&hash).cloned() {
            return Ok(node);
        }
        let bytes = self.store.get(hash)?.ok_or_else(miss)?;
        let node = Arc::new(decode::<Node>(&bytes).ok_or_else(miss)?);
        self.nodes.lock().unwrap().insert(hash, Arc::clone(&node));
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(node)
    }
    fn at(&self, mut root: ContentHash, index: u32, shift: u32) -> io::Result<Option<ContentHash>> {
        // Omit leading zero digits. At most eight pages cover every u32 index.
        for shift in (0..=shift).rev().step_by(4) {
            let node = self.node(root)?;
            let Node::Array(children) = node.as_ref() else {
                return Err(miss());
            };
            let Some(next) = children[((index >> shift) & 15) as usize] else {
                return Ok(None);
            };
            root = next.into();
        }
        Ok(Some(root))
    }
    pub fn decoded_nodes(&self) -> usize {
        self.reads.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn decoded_rows(&self) -> usize {
        self.rows.load(std::sync::atomic::Ordering::Relaxed)
    }
}
impl Source for DiskIndex {
    fn row(&self, table: Descriptor, index: u32) -> io::Result<Vec<u8>> {
        if index >= table.len {
            return Err(miss());
        }
        let hash = self
            .at(
                table.rows,
                index / ROWS_PER_PAGE as u32,
                array_shift((table.len - 1) / ROWS_PER_PAGE as u32),
            )?
            .ok_or_else(miss)?;
        let cached = self.pages.lock().unwrap().get(&hash).cloned();
        let page = if let Some(page) = cached {
            page
        } else {
            let page = Arc::new(self.store.get(hash)?.ok_or_else(miss)?);
            self.pages.lock().unwrap().insert(hash, Arc::clone(&page));
            page
        };
        let slot = index as usize % ROWS_PER_PAGE;
        let offset = |slot: usize| -> io::Result<usize> {
            let bytes = page.get(slot * 8..(slot + 1) * 8).ok_or_else(miss)?;
            usize::try_from(u64::from_le_bytes(bytes.try_into().unwrap())).map_err(|_| miss())
        };
        let start = offset(slot)?;
        let end = offset(slot + 1)?;
        if start < PAGE_HEADER || end < start {
            return Err(miss());
        }
        let bytes = page.get(start..end).ok_or_else(miss)?.to_vec();
        self.rows.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(bytes)
    }
    fn find(&self, table: Descriptor, key: &[u8]) -> io::Result<Option<u32>> {
        let key = ContentHash::of_bytes(key);
        let node = self.node(table.lookup)?;
        let Node::Names {
            buckets,
            mask,
            salt,
        } = node.as_ref()
        else {
            return Err(miss());
        };
        let Some(hash) = self.at(
            (*buckets).into(),
            bucket_index(key, *mask, *salt),
            array_shift(*mask),
        )?
        else {
            return Ok(None);
        };
        let bucket = self.node(hash)?;
        let Node::Bucket(rows) = bucket.as_ref() else {
            return Err(miss());
        };
        let needle = IndexHash::from(key);
        Ok(rows
            .iter()
            .find(|&&(k, _)| k == needle)
            .map(|&(_, value)| value))
    }
}
fn miss() -> io::Error {
    io::Error::other("core image cache miss: damaged or missing indexed record; rebuild the image")
}
pub(crate) struct Writer<'a> {
    store: &'a Store,
    namespace: ContentHash,
    next: u32,
}
impl<'a> Writer<'a> {
    pub fn new(store: &'a Store, namespace: ContentHash) -> Self {
        Self {
            store,
            namespace,
            next: 0,
        }
    }
    fn descriptor(&mut self, len: usize, rows: ContentHash, lookup: ContentHash) -> Descriptor {
        let descriptor = Descriptor {
            namespace: self.namespace,
            table: self.next,
            len: len as u32,
            rows,
            lookup,
        };
        self.next += 1;
        descriptor
    }
}
impl Publisher for Writer<'_> {
    fn table<T: Clone + Serialize + serde::de::DeserializeOwned + Send + Sync + 'static>(
        &mut self,
        table: &Table<T>,
    ) -> io::Result<Table<T>> {
        let mut rows = vec![];
        for row in table.iter() {
            rows.push(encode(row)?);
        }
        let root = row_pages(self.store, &rows)?;
        let empty = names(self.store, &[])?;
        Table::from_descriptor(self.descriptor(table.len(), root, empty))
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
        let mut rows = vec![];
        let mut keys = vec![];
        let mut entries = map.iter().collect::<Vec<_>>();
        entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (i, (key, value)) in entries.into_iter().enumerate() {
            rows.push(encode(&(key, value))?);
            keys.push((ContentHash::of_bytes(&encode(key)?), i as u32));
        }
        let len = rows.len();
        let rows = row_pages(self.store, &rows)?;
        let keys = names(self.store, &keys)?;
        Map::from_descriptor(self.descriptor(len, rows, keys))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_record_uses_no_leading_zero_directory_pages() {
        let dir = tempfile::tempdir().unwrap();
        let source = DiskIndex::new(dir.path()).unwrap();
        let rows = row_pages(&source.store, &[encode(&19u64).unwrap()]).unwrap();
        let table = Descriptor {
            namespace: ContentHash::default(),
            table: 0,
            len: 1,
            rows,
            lookup: ContentHash::default(),
        };
        assert_eq!(
            decode::<u64>(&Source::row(&source, table, 0).unwrap()),
            Some(19)
        );
        assert!(
            source.decoded_nodes() <= 1,
            "a single row needs only one directory page"
        );
    }
    #[test]
    fn index_digest_words_preserve_the_existing_fixed_bytes() {
        let hash = ContentHash(std::array::from_fn(|i| i as u8));
        assert_eq!(encode(&IndexHash::from(hash)).unwrap(), hash.0);
        let children = std::array::from_fn(|i| {
            if i % 3 == 0 {
                Some(IndexHash::from(hash))
            } else {
                None
            }
        });
        let node = Node::Array(Box::new(children));
        // Old enum tag 0, u8 Option tags, then exactly 32 digest bytes.
        let mut expected = 0u32.to_le_bytes().to_vec();
        for i in 0..16 {
            expected.push(u8::from(i % 3 == 0));
            if i % 3 == 0 {
                expected.extend_from_slice(&hash.0);
            }
        }
        assert_eq!(encode(&node).unwrap(), expected);
        let names = Node::Names {
            buckets: hash.into(),
            mask: 7,
            salt: 11,
        };
        let mut names_bytes = 1u32.to_le_bytes().to_vec();
        names_bytes.extend_from_slice(&hash.0);
        names_bytes.extend_from_slice(&7u32.to_le_bytes());
        names_bytes.extend_from_slice(&11u32.to_le_bytes());
        assert_eq!(encode(&names).unwrap(), names_bytes);
        let bucket = Node::Bucket(vec![(hash.into(), 7)]);
        let mut bucket_bytes = 2u32.to_le_bytes().to_vec();
        bucket_bytes.extend_from_slice(&1u64.to_le_bytes());
        bucket_bytes.extend_from_slice(&hash.0);
        bucket_bytes.extend_from_slice(&7u32.to_le_bytes());
        assert_eq!(encode(&bucket).unwrap(), bucket_bytes);
        let Node::Array(decoded) = decode::<Node>(&expected).unwrap() else {
            panic!("array")
        };
        for (i, row) in decoded.iter().enumerate() {
            assert_eq!(row.map(ContentHash::from), (i % 3 == 0).then_some(hash));
        }
    }
    #[test]
    fn malformed_record_page_offsets_are_external_cache_misses() {
        let dir = tempfile::tempdir().unwrap();
        let source = DiskIndex::new(dir.path()).unwrap();
        for (start, end) in [(0u64, 136u64), (140, 139), (136, 145)] {
            let mut bytes = vec![0; PAGE_HEADER];
            bytes[..8].copy_from_slice(&start.to_le_bytes());
            bytes[8..16].copy_from_slice(&end.to_le_bytes());
            let page = source.store.put(&bytes).unwrap();
            let rows = array(&source.store, &[(0, page)], 0).unwrap();
            let table = Descriptor {
                namespace: ContentHash::default(),
                table: 0,
                len: 1,
                rows,
                lookup: ContentHash::default(),
            };
            assert!(Source::row(&source, table, 0).is_err());
        }
    }
}
