//! Indexed package records: fixed-depth numeric arrays and bounded hash buckets.
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
    array(store, &pages, 28)
}
#[derive(Serialize, Deserialize)]
enum Node {
    Array(Box<[Option<ContentHash>; 16]>),
    Names {
        buckets: ContentHash,
        mask: u32,
        salt: u32,
    },
    Bucket(Vec<(ContentHash, u32)>),
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
    store.put(&encode(&Node::Array(Box::new(children)))?)
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
                    rows.push((i as u32, store.put(&encode(&Node::Bucket(group))?)?));
                }
            }
            let buckets = array(store, &rows, 28)?;
            return store.put(&encode(&Node::Names {
                buckets,
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
    fn at(&self, mut root: ContentHash, index: u32) -> io::Result<Option<ContentHash>> {
        // Eight 4-bit pages address every possible u32 row. No binary search,
        // full-prefix digest or variable-depth walk over a package's size.
        for shift in (0..=28).rev().step_by(4) {
            let node = self.node(root)?;
            let Node::Array(children) = node.as_ref() else {
                return Err(miss());
            };
            let Some(next) = children[((index >> shift) & 15) as usize] else {
                return Ok(None);
            };
            root = next;
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
            .at(table.rows, index / ROWS_PER_PAGE as u32)?
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
        let Some(hash) = self.at(*buckets, bucket_index(key, *mask, *salt))? else {
            return Ok(None);
        };
        let bucket = self.node(hash)?;
        let Node::Bucket(rows) = bucket.as_ref() else {
            return Err(miss());
        };
        Ok(rows
            .iter()
            .find(|&&(k, _)| k == key)
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
    fn malformed_record_page_offsets_are_external_cache_misses() {
        let dir = tempfile::tempdir().unwrap();
        let source = DiskIndex::new(dir.path()).unwrap();
        for (start, end) in [(0u64, 136u64), (140, 139), (136, 145)] {
            let mut bytes = vec![0; PAGE_HEADER];
            bytes[..8].copy_from_slice(&start.to_le_bytes());
            bytes[8..16].copy_from_slice(&end.to_le_bytes());
            let page = source.store.put(&bytes).unwrap();
            let rows = array(&source.store, &[(0, page)], 28).unwrap();
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
