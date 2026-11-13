//! Immutable, indexed package prefixes with request-owned suffixes.
//! Only external decoding can fail during an otherwise infallible table lookup.
use crate::ContentHash;
use serde::{Deserialize, Serialize};
use std::{
    any::{Any, TypeId},
    borrow::Borrow,
    collections::BTreeMap,
    fmt, io,
    ops::{Index, IndexMut, Range},
    sync::{Arc, Mutex, OnceLock, Weak},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Descriptor {
    pub namespace: ContentHash,
    pub table: u32,
    pub len: u32,
    pub rows: ContentHash,
    pub lookup: ContentHash,
}

/// A producer's numeric rows and optional key-to-row index. The driver owns IO.
pub trait Provider<T>: Send + Sync {
    fn row(&self, index: u32) -> io::Result<T>;
    fn find(&self, _key: &[u8]) -> io::Result<Option<u32>> {
        Err(io::Error::other("published table has no key index"))
    }
}

struct CacheMiss(io::Error);
fn miss(error: io::Error) -> ! {
    // This bypasses the panic hook: it is an external cache miss, not a compiler
    // assertion. The request boundary converts it back to an ordinary IO result.
    std::panic::resume_unwind(Box::new(CacheMiss(error)))
}
pub fn catch_io<T>(f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => match payload.downcast::<CacheMiss>() {
            Ok(error) => Err(error.0),
            Err(other) => std::panic::resume_unwind(other),
        },
    }
}

struct Node<T> {
    children: [OnceLock<Box<Node<T>>>; 16],
    value: OnceLock<Box<T>>,
}
impl<T> Node<T> {
    fn branch() -> Self {
        Self {
            children: std::array::from_fn(|_| OnceLock::new()),
            value: OnceLock::new(),
        }
    }
    fn cell(&self, key: u64, shift: u32) -> &OnceLock<Box<T>> {
        let child =
            self.children[((key >> shift) & 15) as usize].get_or_init(|| Box::new(Self::branch()));
        if shift == 0 {
            &child.value
        } else {
            child.cell(key, shift - 4)
        }
    }
}
/// A fixed-height sparse cache. Opening allocates no slot per package row.
pub struct Sparse<T> {
    root: Node<T>,
    loaded: Mutex<Vec<u64>>,
}
impl<T> Default for Sparse<T> {
    fn default() -> Self {
        Self {
            root: Node::branch(),
            loaded: Mutex::new(vec![]),
        }
    }
}
impl<T> Sparse<T> {
    pub fn get_or_try_init(&self, key: u64, f: impl FnOnce() -> io::Result<T>) -> io::Result<&T> {
        let cell = self.root.cell(key, 60);
        if let Some(value) = cell.get() {
            return Ok(value);
        }
        let value = Box::new(f()?);
        let mut loaded = self.loaded.lock().unwrap();
        let installed = cell.set(value);
        if installed.is_ok() {
            loaded.push(key);
        }
        drop(loaded);
        drop(installed);
        Ok(cell.get().unwrap())
    }
    pub fn loaded(&self) -> Vec<u64> {
        self.loaded.lock().unwrap().clone()
    }
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.loaded.lock().unwrap().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct Published<T> {
    descriptor: Descriptor,
    provider: Arc<dyn Provider<T>>,
    cache: Sparse<T>,
    ranges: Sparse<Vec<T>>,
    lookups: Mutex<BTreeMap<Vec<u8>, Option<u32>>>,
}
impl<T> Published<T> {
    fn row(&self, index: usize) -> &T {
        self.cache
            .get_or_try_init(index as u64, || self.provider.row(index as u32))
            .unwrap_or_else(|e| miss(e))
    }
    fn find(&self, key: &[u8]) -> Option<u32> {
        if let Some(index) = self.lookups.lock().unwrap().get(key) {
            return *index;
        }
        let index = self.provider.find(key).unwrap_or_else(|e| miss(e));
        self.lookups.lock().unwrap().insert(key.to_vec(), index);
        index
    }
}
type Registry = BTreeMap<ContentHash, BTreeMap<(u32, TypeId), Weak<dyn Any + Send + Sync>>>;
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}
/// Retain the returned binding while an image can be decoded. Registrations are
/// weak; package/request destruction releases the demanded rows normally.
pub fn bind<T: Send + Sync + 'static>(
    descriptor: Descriptor,
    provider: Arc<dyn Provider<T>>,
) -> Arc<Published<T>> {
    let binding = Arc::new(Published {
        descriptor,
        provider,
        cache: Sparse::default(),
        ranges: Sparse::default(),
        lookups: Mutex::default(),
    });
    let erased: Arc<dyn Any + Send + Sync> = binding.clone();
    registry()
        .lock()
        .unwrap()
        .entry(descriptor.namespace)
        .or_default()
        .insert(
            (descriptor.table, TypeId::of::<T>()),
            Arc::downgrade(&erased),
        );
    binding
}
/// External record bytes, indexed within one immutable package namespace.
pub trait Source: Send + Sync {
    fn row(&self, table: Descriptor, index: u32) -> io::Result<Vec<u8>>;
    fn find(&self, table: Descriptor, key: &[u8]) -> io::Result<Option<u32>>;
}
fn sources() -> &'static Mutex<BTreeMap<ContentHash, Weak<dyn Source>>> {
    static SOURCES: OnceLock<Mutex<BTreeMap<ContentHash, Weak<dyn Source>>>> = OnceLock::new();
    SOURCES.get_or_init(Mutex::default)
}
pub fn bind_source(namespace: ContentHash, source: Arc<dyn Source>) {
    sources()
        .lock()
        .unwrap()
        .insert(namespace, Arc::downgrade(&source));
    // A reopened/copied cache binds new decoders to that reader's live store.
    registry().lock().unwrap().remove(&namespace);
}
struct Decoder<T>(Descriptor, Arc<dyn Source>, std::marker::PhantomData<T>);
impl<T: serde::de::DeserializeOwned + Send + Sync> Provider<T> for Decoder<T> {
    fn row(&self, index: u32) -> io::Result<T> {
        use bincode::Options;
        let bytes = self.1.row(self.0, index)?;
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(bytes.len() as u64)
            .reject_trailing_bytes()
            .deserialize(&bytes)
            .map_err(io::Error::other)
    }
    fn find(&self, key: &[u8]) -> io::Result<Option<u32>> {
        self.1.find(self.0, key)
    }
}
fn binding<T: serde::de::DeserializeOwned + Send + Sync + 'static>(
    descriptor: Descriptor,
) -> io::Result<Arc<Published<T>>> {
    let cached = registry()
        .lock()
        .unwrap()
        .get(&descriptor.namespace)
        .and_then(|rows| rows.get(&(descriptor.table, TypeId::of::<T>())))
        .and_then(Weak::upgrade)
        .and_then(|p| p.downcast().ok());
    if let Some(cached) = cached {
        return Ok(cached);
    }
    let source = sources()
        .lock()
        .unwrap()
        .get(&descriptor.namespace)
        .and_then(Weak::upgrade)
        .ok_or_else(|| io::Error::other("published package binding is unavailable"))?;
    Ok(bind(
        descriptor,
        Arc::new(Decoder(descriptor, source, std::marker::PhantomData)),
    ))
}

#[derive(Clone)]
pub struct Table<T> {
    prefix: Option<Arc<Published<T>>>,
    own: Vec<T>,
    overrides: BTreeMap<usize, T>,
}
impl<T> Default for Table<T> {
    fn default() -> Self {
        Self {
            prefix: None,
            own: vec![],
            overrides: BTreeMap::new(),
        }
    }
}
impl<T> fmt::Debug for Table<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Table")
            .field("prefix", &self.descriptor())
            .field("own", &self.own)
            .field("overrides", &self.overrides)
            .finish()
    }
}
impl<T> From<Vec<T>> for Table<T> {
    fn from(own: Vec<T>) -> Self {
        Self {
            own,
            ..Self::default()
        }
    }
}
impl<T> Table<T> {
    pub fn from_descriptor(descriptor: Descriptor) -> io::Result<Self>
    where
        T: serde::de::DeserializeOwned + Send + Sync + 'static,
    {
        Ok(Self::published(binding(descriptor)?))
    }
    pub fn published(prefix: Arc<Published<T>>) -> Self {
        Self {
            prefix: Some(prefix),
            ..Self::default()
        }
    }
    pub fn descriptor(&self) -> Option<Descriptor> {
        self.prefix.as_ref().map(|p| p.descriptor)
    }
    #[inline(always)]
    pub fn base_len(&self) -> usize {
        self.prefix
            .as_ref()
            .map_or(0, |p| p.descriptor.len as usize)
    }
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.base_len() + self.own.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn push(&mut self, value: T) {
        self.own.push(value);
    }
    pub fn extend(&mut self, values: impl IntoIterator<Item = T>) {
        self.own.extend(values);
    }
    #[inline(always)]
    pub fn get(&self, index: usize) -> Option<&T> {
        let Some(prefix) = &self.prefix else {
            return self.own.get(index);
        };
        let base = prefix.descriptor.len as usize;
        if index >= base {
            return self.own.get(index - base);
        }
        self.overrides
            .get(&index)
            .or_else(|| Some(prefix.row(index)))
    }
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + ExactSizeIterator {
        (0..self.len()).map(|i| &self[i])
    }
    pub fn own_iter(&self) -> impl Iterator<Item = &T> {
        self.own.iter()
    }
    pub fn available(&self) -> Vec<(usize, &T)> {
        let mut ids = self
            .prefix
            .as_ref()
            .map_or_else(Vec::new, |p| p.cache.loaded());
        ids.extend((self.base_len()..self.len()).map(|i| i as u64));
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
            .map(|i| (i as usize, &self[i as usize]))
            .collect()
    }
    pub fn as_ptr(&self) -> *const T {
        assert_eq!(self.base_len(), 0);
        self.own.as_ptr()
    }
    pub fn as_slice(&self) -> &[T] {
        assert_eq!(self.base_len(), 0);
        &self.own
    }
}
impl<T: Clone> Table<T> {
    pub fn resize(&mut self, len: usize, value: T) {
        self.own.resize(len - self.base_len(), value);
    }
    pub fn to_vec(&self) -> Vec<T> {
        self.iter().cloned().collect()
    }
    pub fn slice(&self, range: Range<usize>) -> &[T] {
        if range.is_empty() {
            return &[];
        }
        if range.start >= self.base_len() {
            return &self.own[range.start - self.base_len()..range.end - self.base_len()];
        }
        let prefix = self.prefix.as_ref().unwrap();
        let key = ((range.start as u64) << 32) | (range.end - range.start) as u64;
        prefix
            .ranges
            .get_or_try_init(key, || Ok(range.map(|i| self[i].clone()).collect()))
            .unwrap_or_else(|e| miss(e))
    }
}
impl<T> Index<usize> for Table<T> {
    type Output = T;
    #[inline(always)]
    fn index(&self, index: usize) -> &T {
        self.get(index).expect("published row outside table")
    }
}
impl<T: Clone> IndexMut<usize> for Table<T> {
    fn index_mut(&mut self, index: usize) -> &mut T {
        if index < self.base_len() {
            if !self.overrides.contains_key(&index) {
                self.overrides
                    .insert(index, self.prefix.as_ref().unwrap().row(index).clone());
            }
            self.overrides.get_mut(&index).unwrap()
        } else {
            let base = self.base_len();
            &mut self.own[index - base]
        }
    }
}
impl<T: Serialize> Serialize for Table<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        (self.descriptor(), &self.own, &self.overrides).serialize(s)
    }
}
impl<'de, T: serde::de::DeserializeOwned + Send + Sync + 'static> Deserialize<'de> for Table<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let (prefix, own, overrides): (Option<Descriptor>, Vec<T>, BTreeMap<usize, T>) =
            Deserialize::deserialize(d)?;
        Ok(Self {
            prefix: prefix
                .map(binding)
                .transpose()
                .map_err(serde::de::Error::custom)?,
            own,
            overrides,
        })
    }
}

#[derive(Clone)]
pub struct Map<K: Ord + std::hash::Hash, V> {
    prefix: Option<Arc<Published<(K, V)>>>,
    own: rustc_hash::FxHashMap<K, V>,
}
impl<K: Ord + std::hash::Hash, V> Default for Map<K, V> {
    fn default() -> Self {
        Self {
            prefix: None,
            own: rustc_hash::FxHashMap::default(),
        }
    }
}
impl<K: Ord + std::hash::Hash + fmt::Debug, V: fmt::Debug> fmt::Debug for Map<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Map")
            .field("prefix", &self.prefix.as_ref().map(|p| p.descriptor))
            .field("own", &self.own)
            .finish()
    }
}
impl<K: Ord + std::hash::Hash, V> Map<K, V> {
    pub fn from_descriptor(descriptor: Descriptor) -> io::Result<Self>
    where
        K: serde::de::DeserializeOwned + Send + Sync + 'static,
        V: serde::de::DeserializeOwned + Send + Sync + 'static,
    {
        Ok(Self::published(binding(descriptor)?))
    }
    pub fn index_of<Q: Ord + std::hash::Hash + Serialize + ?Sized>(&self, key: &Q) -> Option<usize>
    where
        K: Borrow<Q>,
    {
        if self.own.contains_key(key) {
            return Some(
                self.prefix
                    .as_ref()
                    .map_or(0, |p| p.descriptor.len as usize)
                    + self.own.keys().position(|k| k.borrow() == key).unwrap(),
            );
        }
        self.prefix
            .as_ref()?
            .find(&bincode::serialize(key).expect("lookup encoding"))
            .map(|i| i as usize)
    }
    pub fn published(prefix: Arc<Published<(K, V)>>) -> Self {
        Self {
            prefix: Some(prefix),
            own: rustc_hash::FxHashMap::default(),
        }
    }
    pub fn get<Q: Ord + std::hash::Hash + Serialize + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        if let Some(value) = self.own.get(key) {
            return Some(value);
        }
        let prefix = self.prefix.as_ref()?;
        let bytes = bincode::serialize(key).expect("published lookup key encoding");
        let i = prefix.find(&bytes)?;
        Some(&prefix.row(i as usize).1)
    }
    pub fn contains_key<Q: Ord + std::hash::Hash + Serialize + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.get(key).is_some()
    }
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.own.insert(key, value)
    }
    pub fn entry(&mut self, key: K) -> std::collections::hash_map::Entry<'_, K, V> {
        self.own.entry(key)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.prefix
            .iter()
            .flat_map(|p| (0..p.descriptor.len).map(|i| p.row(i as usize)))
            .filter(|(k, _)| !self.own.contains_key(k))
            .map(|(k, v)| (k, v))
            .chain(self.own.iter())
    }
    pub fn own_iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.own.iter()
    }
    pub fn available(&self) -> Vec<(&K, &V)> {
        self.prefix
            .iter()
            .flat_map(|p| p.cache.loaded().into_iter().map(|i| p.row(i as usize)))
            .filter(|(k, _)| !self.own.contains_key(k))
            .map(|(k, v)| (k, v))
            .chain(self.own.iter())
            .collect()
    }
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(k, _)| k)
    }
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, v)| v)
    }
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.prefix
            .as_ref()
            .map_or(0, |p| p.descriptor.len as usize)
            + self.own.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
impl<K: Ord + std::hash::Hash + Serialize, V> Index<&K> for Map<K, V> {
    type Output = V;
    fn index(&self, key: &K) -> &V {
        self.get(key).expect("published key outside index")
    }
}
impl<K: Ord + std::hash::Hash, V> FromIterator<(K, V)> for Map<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(i: I) -> Self {
        Self {
            prefix: None,
            own: i.into_iter().collect(),
        }
    }
}
impl<K: Ord + std::hash::Hash + Serialize, V: Serialize> Serialize for Map<K, V> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        (self.prefix.as_ref().map(|p| p.descriptor), &self.own).serialize(s)
    }
}
impl<
    'de,
    K: Ord + std::hash::Hash + serde::de::DeserializeOwned + Send + Sync + 'static,
    V: serde::de::DeserializeOwned + Send + Sync + 'static,
> Deserialize<'de> for Map<K, V>
{
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let (prefix, own): (Option<Descriptor>, rustc_hash::FxHashMap<K, V>) =
            Deserialize::deserialize(d)?;
        Ok(Self {
            prefix: prefix
                .map(binding)
                .transpose()
                .map_err(serde::de::Error::custom)?,
            own,
        })
    }
}

/// The image writer freezes these component types without knowing their owners.
pub trait Publisher {
    fn table<T: Clone + Serialize + serde::de::DeserializeOwned + Send + Sync + 'static>(
        &mut self,
        table: &Table<T>,
    ) -> io::Result<Table<T>>;
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
    ) -> io::Result<Map<K, V>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Missing;
    impl Provider<u64> for Missing {
        fn row(&self, _: u32) -> io::Result<u64> {
            Err(io::Error::other("damaged external row"))
        }
    }
    #[test]
    fn external_row_failure_returns_io_without_masking_compiler_panics() {
        let descriptor = Descriptor {
            namespace: ContentHash::of_bytes(b"missing"),
            table: 0,
            len: 1,
            rows: ContentHash::default(),
            lookup: ContentHash::default(),
        };
        let rows = Table::published(bind(descriptor, Arc::new(Missing)));
        let error = catch_io(|| Ok(rows[0])).unwrap_err();
        assert_eq!(error.to_string(), "damaged external row");
        assert!(std::panic::catch_unwind(|| catch_io::<()>(|| panic!("compiler bug"))).is_err());
    }
}
