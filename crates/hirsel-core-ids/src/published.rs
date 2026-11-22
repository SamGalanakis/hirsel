//! Immutable, indexed package prefixes with request-owned suffixes.
//! Only external decoding can fail during an otherwise infallible table lookup.
use crate::ContentHash;
use bincode::Options;
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
}

/// An optional, allocation-free observer for published-prefix work.
/// The driver owns the counter and restores the prior observer when its meter ends.
pub const FRONT_OBSERVER_AVAILABLE: bool = cfg!(feature = "front-observer");
pub type Observe = fn() -> Option<Observation>;
pub type Observer = (
    std::rc::Rc<hirsel_intern::instructions::InstructionCounter>,
    Observe,
);
pub struct Observation {
    state: [u64; 4],
    finish: fn([u64; 4]),
}
impl Observation {
    pub fn new(state: [u64; 4], finish: fn([u64; 4])) -> Self {
        Self { state, finish }
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        OBSERVE.with_borrow(|observer| {
            if let Some((counter, _)) = observer {
                // The driver records any refused counter control in the callback.
                let _ = counter.pause();
                (self.finish)(self.state);
                let _ = counter.resume();
            }
        });
    }
}
thread_local! { static OBSERVE: std::cell::RefCell<Option<Observer>> = const { std::cell::RefCell::new(None) }; }
pub fn observe_with(observer: Option<Observer>) -> Option<Observer> {
    OBSERVE.with_borrow_mut(|active| std::mem::replace(active, observer))
}
/// Also used once by the driver's observer calibration, with no lookup work.
pub fn observe_prefix() -> Option<Observation> {
    OBSERVE.with_borrow(|observer| {
        let (counter, enter) = observer.as_ref()?;
        let _ = counter.pause();
        let scope = enter();
        let _ = counter.resume();
        scope
    })
}
#[inline(never)]
pub fn observed<T>(f: impl FnOnce() -> T) -> T {
    let _scope = observe();
    f()
}

fn observe() -> Option<Observation> {
    observe_prefix()
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
/// Sparse stable cells, partitioned by key width. Opening allocates no slot per package row.
pub struct Sparse<T> {
    roots: [OnceLock<Box<Node<T>>>; 16],
    loaded: Mutex<Vec<u64>>,
}
impl<T> Default for Sparse<T> {
    fn default() -> Self {
        Self {
            roots: std::array::from_fn(|_| OnceLock::new()),
            loaded: Mutex::new(vec![]),
        }
    }
}
impl<T> Sparse<T> {
    pub fn get_or_try_init(&self, key: u64, f: impl FnOnce() -> io::Result<T>) -> io::Result<&T> {
        let level = (63 - key.max(1).leading_zeros()) / 4;
        let root = self.roots[level as usize].get_or_init(|| Box::new(Node::branch()));
        let cell = root.cell(key, level * 4);
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

/// Encode a published row. Its small ordinals and lengths take fewer bytes
/// than the compiler's fixed-width record encoding.
pub fn encode_row<T: Serialize + ?Sized>(row: &T) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(64);
    bincode::DefaultOptions::new()
        .with_varint_encoding()
        .serialize_into(&mut bytes, row)
        .map_err(io::Error::other)?;
    Ok(bytes)
}

/// Decode exactly one published row, rejecting trailing bytes.
pub fn decode_row<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    bincode::DefaultOptions::new()
        .with_varint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(io::Error::other)
}

// Keys share the row encoding, written once into a reused per-thread buffer.
/// A key's lookup bytes, as the image writer stores them and a lookup probes.
pub fn with_lookup_key<R>(key: &(impl Serialize + ?Sized), f: impl FnOnce(&[u8]) -> R) -> R {
    thread_local! {
        static KEY: std::cell::Cell<Vec<u8>> = const { std::cell::Cell::new(Vec::new()) };
    }
    // Taken, not borrowed: a lookup inside `f` gets its own buffer.
    let mut bytes = KEY.take();
    bytes.clear();
    bincode::DefaultOptions::new()
        .with_varint_encoding()
        .serialize_into(&mut bytes, key)
        .expect("published lookup key encoding");
    let result = f(&bytes);
    KEY.set(bytes);
    result
}
#[cfg(test)]
fn encode_lookup_key(key: &(impl Serialize + ?Sized)) -> Vec<u8> {
    with_lookup_key(key, <[u8]>::to_vec)
}

/// A published table's decoded rows, one slot per row, allocated on the table's
/// first row demand: a hit is one index, never a walk.
struct Rows<T> {
    slots: OnceLock<Box<[OnceLock<Box<T>>]>>,
    loaded: Mutex<Vec<u64>>,
}
impl<T> Rows<T> {
    fn new() -> Self {
        Self {
            slots: OnceLock::new(),
            loaded: Mutex::new(vec![]),
        }
    }
    #[inline]
    fn get_or_try_init(
        &self,
        len: u32,
        index: usize,
        f: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<&T> {
        let slots = self
            .slots
            .get_or_init(|| (0..len).map(|_| OnceLock::new()).collect());
        let cell = slots
            .get(index)
            .ok_or_else(|| io::Error::other("published row outside its table"))?;
        if let Some(value) = cell.get() {
            return Ok(value);
        }
        let value = Box::new(f()?);
        let mut loaded = self.loaded.lock().unwrap();
        if cell.set(value).is_ok() {
            loaded.push(index as u64);
        }
        drop(loaded);
        Ok(cell.get().unwrap())
    }
    fn loaded(&self) -> Vec<u64> {
        self.loaded.lock().unwrap().clone()
    }
}

pub struct Published<T> {
    #[cfg(feature = "front-observer")]
    observed: bool,
    descriptor: Descriptor,
    provider: Arc<dyn Provider<T>>,
    cache: Rows<T>,
    ranges: Sparse<Vec<T>>,
}
impl<T> Published<T> {
    fn row(&self, index: usize) -> &T {
        #[cfg(feature = "front-observer")]
        if self.observed {
            return observed(|| self.row_plain(index));
        }
        self.row_plain(index)
    }
    #[inline(always)]
    fn row_plain(&self, index: usize) -> &T {
        self.cache
            .get_or_try_init(self.descriptor.len, index, || {
                self.provider.row(index as u32)
            })
            .unwrap_or_else(|e| miss(e))
    }
    /// A key's row by one probe; its caller observes it.
    #[inline(always)]
    fn find(&self, key: &(impl Serialize + ?Sized)) -> Option<u32> {
        with_lookup_key(key, |key| {
            self.provider.find(key).unwrap_or_else(|e| miss(e))
        })
    }
}
/// A published table over its provider, owned by its caller and shared with no
/// other table. An image's tables come from its [`Reader`].
pub fn bind<T: Send + Sync + 'static>(
    descriptor: Descriptor,
    provider: Arc<dyn Provider<T>>,
) -> Arc<Published<T>> {
    Arc::new(Published {
        #[cfg(feature = "front-observer")]
        observed: OBSERVE.with_borrow(|active| active.is_some()),
        descriptor,
        provider,
        cache: Rows::new(),
        ranges: Sparse::default(),
    })
}
/// External record bytes, indexed within one immutable package namespace.
pub trait Source: Send + Sync {
    fn row(&self, table: Descriptor, index: u32) -> io::Result<Vec<u8>>;
    fn find(&self, table: Descriptor, key: &[u8]) -> io::Result<Option<u32>>;
}
type Bindings = BTreeMap<(u32, TypeId), Weak<dyn Any + Send + Sync>>;
/// One opened image: its live source and the tables decoded from it. A
/// descriptor resolves only through the reader decoding it, so opening or
/// dropping another reader of the same namespace never reaches this one.
pub struct Reader {
    namespace: ContentHash,
    source: Arc<dyn Source>,
    bindings: Mutex<Bindings>,
}
thread_local! {
    static DECODING: std::cell::RefCell<Option<Arc<Reader>>> =
        const { std::cell::RefCell::new(None) };
}
impl Reader {
    pub fn new(namespace: ContentHash, source: Arc<dyn Source>) -> Arc<Self> {
        Arc::new(Self {
            namespace,
            source,
            bindings: Mutex::default(),
        })
    }
    pub fn namespace(&self) -> ContentHash {
        self.namespace
    }
    /// Run a decode whose published descriptors resolve to this reader's tables.
    pub fn decode<R>(self: &Arc<Self>, f: impl FnOnce() -> R) -> R {
        struct Restore(Option<Arc<Reader>>);
        impl Drop for Restore {
            fn drop(&mut self) {
                let previous = self.0.take();
                DECODING.with_borrow_mut(|active| *active = previous);
            }
        }
        let _restore = Restore(DECODING.with_borrow_mut(|active| active.replace(Arc::clone(self))));
        f()
    }
    pub fn table<T: serde::de::DeserializeOwned + Send + Sync + 'static>(
        self: &Arc<Self>,
        descriptor: Descriptor,
    ) -> io::Result<Table<T>> {
        Ok(Table::published(self.binding(descriptor)?))
    }
    pub fn map<K, V>(self: &Arc<Self>, descriptor: Descriptor) -> io::Result<Map<K, V>>
    where
        K: Ord + std::hash::Hash + serde::de::DeserializeOwned + Send + Sync + 'static,
        V: serde::de::DeserializeOwned + Send + Sync + 'static,
    {
        Ok(Map::published(self.binding(descriptor)?))
    }
    fn binding<T: serde::de::DeserializeOwned + Send + Sync + 'static>(
        self: &Arc<Self>,
        descriptor: Descriptor,
    ) -> io::Result<Arc<Published<T>>> {
        if descriptor.namespace != self.namespace {
            return Err(io::Error::other(
                "a published table names another image's namespace than the reader decoding it",
            ));
        }
        let key = (descriptor.table, TypeId::of::<T>());
        let mut bindings = self.bindings.lock().unwrap();
        if let Some(bound) = bindings
            .get(&key)
            .and_then(Weak::upgrade)
            .and_then(|p| p.downcast().ok())
        {
            return Ok(bound);
        }
        let bound = bind(
            descriptor,
            Arc::new(Decoder(
                descriptor,
                Arc::clone(self),
                std::marker::PhantomData,
            )),
        );
        let erased: Arc<dyn Any + Send + Sync> = bound.clone();
        bindings.insert(key, Arc::downgrade(&erased));
        Ok(bound)
    }
}
struct Decoder<T>(Descriptor, Arc<Reader>, std::marker::PhantomData<T>);
impl<T: serde::de::DeserializeOwned + Send + Sync> Provider<T> for Decoder<T> {
    fn row(&self, index: u32) -> io::Result<T> {
        let bytes = self.1.source.row(self.0, index)?;
        self.1.decode(|| decode_row(&bytes))
    }
    fn find(&self, key: &[u8]) -> io::Result<Option<u32>> {
        self.1.source.find(self.0, key)
    }
}
fn binding<T: serde::de::DeserializeOwned + Send + Sync + 'static>(
    descriptor: Descriptor,
) -> io::Result<Arc<Published<T>>> {
    DECODING
        .with_borrow(Clone::clone)
        .ok_or_else(|| io::Error::other("a published table is decoded outside its image's reader"))?
        .binding(descriptor)
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
    pub fn published(prefix: Arc<Published<T>>) -> Self {
        Self {
            prefix: Some(prefix),
            ..Self::default()
        }
    }
    pub fn descriptor(&self) -> Option<Descriptor> {
        self.prefix.as_ref().map(|p| p.descriptor)
    }
    /// Look up a row number in a prefix written by `Publisher::indexed_table`.
    /// The appended rows and overrides are owned by the caller.
    pub fn find_published<Q: Serialize + ?Sized>(&self, key: &Q) -> Option<u32>
    where
        T: Borrow<Q>,
    {
        let prefix = self.prefix.as_ref()?;
        #[cfg(feature = "front-observer")]
        if prefix.observed {
            return observed(|| prefix.find(key));
        }
        prefix.find(key)
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
    /// The appended rows, excluding the immutable published prefix.
    #[inline]
    pub fn own_slice(&self) -> &[T] {
        &self.own
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

pub struct Map<K: Ord + std::hash::Hash, V> {
    prefix: Option<Arc<Published<(K, V)>>>,
    own: rustc_hash::FxHashMap<K, V>,
    /// The prefix's row for each key this clone asked, present or absent, by
    /// the key itself: a repeat neither encodes nor probes. Only a clone has
    /// one, so the image's own maps keep no request's keys. It lives as long
    /// as whatever holds the clone, a retained product included, and is
    /// bounded by the names that product's one immutable snapshot can ask.
    answers: Option<Box<Mutex<rustc_hash::FxHashMap<K, Option<u32>>>>>,
}
impl<K: Ord + std::hash::Hash + Clone, V: Clone> Clone for Map<K, V> {
    fn clone(&self) -> Self {
        Self {
            prefix: self.prefix.clone(),
            own: self.own.clone(),
            answers: self.prefix.as_ref().map(|_| Box::default()),
        }
    }
}
impl<K: Ord + std::hash::Hash, V> Default for Map<K, V> {
    fn default() -> Self {
        Self {
            prefix: None,
            own: rustc_hash::FxHashMap::default(),
            answers: None,
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
    pub fn index_of<Q: Ord + std::hash::Hash + Serialize + ToOwned + ?Sized>(
        &self,
        key: &Q,
    ) -> Option<usize>
    where
        K: Borrow<Q>,
        Q::Owned: Into<K>,
    {
        if self.own.contains_key(key) {
            return Some(
                self.prefix
                    .as_ref()
                    .map_or(0, |p| p.descriptor.len as usize)
                    + self.own.keys().position(|k| k.borrow() == key).unwrap(),
            );
        }
        self.prefix.as_ref()?;
        self.find_published(key).map(|i| i as usize)
    }
    #[inline(never)]
    fn find_published<Q: Ord + std::hash::Hash + Serialize + ToOwned + ?Sized>(
        &self,
        key: &Q,
    ) -> Option<u32>
    where
        K: Borrow<Q>,
        Q::Owned: Into<K>,
    {
        #[cfg(feature = "front-observer")]
        if self.prefix.as_ref().unwrap().observed {
            return observed(|| self.find_published_plain(key));
        }
        self.find_published_plain(key)
    }
    #[inline(always)]
    fn find_published_plain<Q: Ord + std::hash::Hash + Serialize + ToOwned + ?Sized>(
        &self,
        key: &Q,
    ) -> Option<u32>
    where
        K: Borrow<Q>,
        Q::Owned: Into<K>,
    {
        let prefix = self.prefix.as_ref().unwrap();
        let Some(answers) = &self.answers else {
            return prefix.find(key);
        };
        if let Some(&row) = answers.lock().unwrap().get(key) {
            return row;
        }
        // An unwinding source error leaves no answer.
        let row = prefix.find(key);
        answers.lock().unwrap().insert(key.to_owned().into(), row);
        row
    }
    pub fn published(prefix: Arc<Published<(K, V)>>) -> Self {
        Self {
            prefix: Some(prefix),
            own: rustc_hash::FxHashMap::default(),
            answers: None,
        }
    }
    #[inline]
    pub fn get<Q: Ord + std::hash::Hash + Serialize + ToOwned + ?Sized>(
        &self,
        key: &Q,
    ) -> Option<&V>
    where
        K: Borrow<Q>,
        Q::Owned: Into<K>,
    {
        if let Some(value) = self.own.get(key) {
            return Some(value);
        }
        self.prefix.as_ref()?;
        self.get_published(key)
    }
    #[inline(never)]
    fn get_published<Q: Ord + std::hash::Hash + Serialize + ToOwned + ?Sized>(
        &self,
        key: &Q,
    ) -> Option<&V>
    where
        K: Borrow<Q>,
        Q::Owned: Into<K>,
    {
        #[cfg(feature = "front-observer")]
        if self.prefix.as_ref().unwrap().observed {
            return observed(|| self.get_published_plain(key));
        }
        self.get_published_plain(key)
    }
    #[inline(always)]
    fn get_published_plain<Q: Ord + std::hash::Hash + Serialize + ToOwned + ?Sized>(
        &self,
        key: &Q,
    ) -> Option<&V>
    where
        K: Borrow<Q>,
        Q::Owned: Into<K>,
    {
        let i = self.find_published_plain(key)?;
        Some(&self.prefix.as_ref().unwrap().row_plain(i as usize).1)
    }
    pub fn contains_key<Q: Ord + std::hash::Hash + Serialize + ToOwned + ?Sized>(
        &self,
        key: &Q,
    ) -> bool
    where
        K: Borrow<Q>,
        Q::Owned: Into<K>,
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
    /// The value appended over the published prefix, if any.
    pub fn own_get<Q: Eq + std::hash::Hash + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.own.get(key)
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
impl<K: Ord + std::hash::Hash + Clone + Serialize, V> Index<&K> for Map<K, V> {
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
            answers: None,
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
        let prefix = prefix
            .map(binding)
            .transpose()
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            prefix,
            own,
            answers: None,
        })
    }
}

/// The image writer freezes these component types without knowing their owners.
pub trait Publisher {
    fn table<T: Clone + Serialize + serde::de::DeserializeOwned + Send + Sync + 'static>(
        &mut self,
        table: &Table<T>,
    ) -> io::Result<Table<T>>;
    /// Index the encoded rows themselves, with their original ordinals as values.
    fn indexed_table<T: Clone + Serialize + serde::de::DeserializeOwned + Send + Sync + 'static>(
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
    /// As [`Publisher::table`], for a table whose rows `outward` marks are the
    /// ones other packages can name: a dependent request reads those, so the
    /// writer may keep them together. Row ordinals are unchanged.
    fn outward_table<T: Clone + Serialize + serde::de::DeserializeOwned + Send + Sync + 'static>(
        &mut self,
        table: &Table<T>,
        outward: &[bool],
    ) -> io::Result<Table<T>>;
    /// As [`Publisher::map`], with `outward` marking the entries other packages
    /// can name, as for [`Publisher::outward_table`].
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
    ) -> io::Result<Map<K, V>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lookup_key_encoding_visits_once_and_keeps_the_compact_bytes() {
        struct Counted<'a>(&'a std::cell::Cell<u32>);
        impl Serialize for Counted<'_> {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                self.0.set(self.0.get() + 1);
                42u32.serialize(s)
            }
        }
        let count = std::cell::Cell::new(0);
        assert_eq!(encode_lookup_key(&Counted(&count)), [42]);
        assert_eq!(count.get(), 1);
        let path = crate::DefPath::from_full("library@1/parent/child/answer").unwrap();
        let long = "long\0borrowed\u{00e9}".repeat(64);
        let key = (&path, long.as_str(), vec![0u32, 42, u32::MAX]);
        assert_eq!(
            encode_lookup_key(&key),
            bincode::DefaultOptions::new().serialize(&key).unwrap()
        );
        assert_eq!(
            encode_lookup_key("borrowed"),
            bincode::DefaultOptions::new()
                .serialize("borrowed")
                .unwrap()
        );
    }

    #[test]
    fn sparse_cells_cover_all_key_widths_and_keep_references() {
        let rows = Sparse::default();
        let first = rows
            .get_or_try_init(0, || Ok(String::from("zero")))
            .unwrap();
        let address = first.as_ptr();
        for bit in 0..64 {
            let key = 1u64 << bit;
            assert_eq!(
                rows.get_or_try_init(key, || Ok(key.to_string())).unwrap(),
                &key.to_string()
            );
        }
        assert_eq!(first, "zero");
        assert_eq!(
            rows.get_or_try_init(0, || panic!("cached"))
                .unwrap()
                .as_ptr(),
            address
        );
        assert_eq!(rows.len(), 65);
        assert!(
            rows.get_or_try_init(u64::MAX, || Err(io::Error::other("retry")))
                .is_err()
        );
        assert_eq!(rows.len(), 65);
        assert_eq!(
            rows.get_or_try_init(u64::MAX, || Ok(String::from("last")))
                .unwrap(),
            "last"
        );
    }
    #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    struct Prefix;
    impl Provider<u32> for Prefix {
        fn row(&self, index: u32) -> io::Result<u32> {
            assert!(index < 2, "row {index} is past the published prefix");
            Ok(10 + index)
        }
    }
    impl Provider<(u32, u32)> for Prefix {
        fn row(&self, index: u32) -> io::Result<(u32, u32)> {
            assert_eq!(index, 0);
            Ok((7, 42))
        }
        fn find(&self, key: &[u8]) -> io::Result<Option<u32>> {
            Ok((key == [7]).then_some(0))
        }
    }
    /// Law: an id at or past the published prefix is a local row, and a local
    /// entry wins in the map that holds it, never in a clone made before it.
    #[test]
    fn ids_past_the_prefix_and_owned_entries_stay_local() {
        let descriptor = Descriptor {
            namespace: ContentHash::of_bytes(b"prefix-rows"),
            table: 0,
            len: 2,
        };
        let provider: Arc<dyn Provider<u32>> = Arc::new(Prefix);
        let mut rows = Table::published(bind(descriptor, provider));
        rows.push(20);
        rows.push(21);
        let got: Vec<_> = (0..5).map(|i| rows.get(i).copied()).collect();
        assert_eq!(got, [Some(10), Some(11), Some(20), Some(21), None]);
        let provider: Arc<dyn Provider<(u32, u32)>> = Arc::new(Prefix);
        let map = Map::published(bind(
            Descriptor {
                table: 1,
                len: 1,
                ..descriptor
            },
            provider,
        ));
        let mut other = map.clone();
        // The clone's answers for 7 (present) and 8 (absent) precede its own.
        assert_eq!((other.get(&7), other.get(&8)), (Some(&42), None));
        assert_eq!(other.index_of(&8), None);
        other.insert(8, 1);
        assert_eq!((other.get(&8), other.index_of(&8)), (Some(&1), Some(1)));
        other.insert(7, 99);
        assert_eq!((other.get(&7), other.get(&8)), (Some(&99), Some(&1)));
        assert_eq!((map.get(&7), map.get(&8)), (Some(&42), None));
        assert_eq!((map.index_of(&7), map.index_of(&8)), (Some(0), None));
    }
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
        };
        let rows = Table::published(bind(descriptor, Arc::new(Missing)));
        let error = catch_io(|| Ok(rows[0])).unwrap_err();
        assert_eq!(error.to_string(), "damaged external row");
        assert!(std::panic::catch_unwind(|| catch_io::<()>(|| panic!("compiler bug"))).is_err());
    }

    /// Rows of four same-typed tables per image: two maps holding key 7 at
    /// different rows, and two plain tables. Every value names its image and table.
    struct Image(u32);
    impl Image {
        fn rows(&self, table: u32) -> Vec<Vec<u8>> {
            let tag = self.0 * 100 + table * 10;
            match table {
                0 => vec![
                    bincode::DefaultOptions::new()
                        .serialize(&(7u32, tag))
                        .unwrap(),
                    bincode::DefaultOptions::new()
                        .serialize(&(9u32, tag + 1))
                        .unwrap(),
                ],
                1 => vec![
                    bincode::DefaultOptions::new()
                        .serialize(&(9u32, tag))
                        .unwrap(),
                    bincode::DefaultOptions::new()
                        .serialize(&(7u32, tag + 1))
                        .unwrap(),
                ],
                _ => vec![bincode::DefaultOptions::new().serialize(&tag).unwrap()],
            }
        }
    }
    impl Source for Image {
        fn row(&self, table: Descriptor, index: u32) -> io::Result<Vec<u8>> {
            Ok(self.rows(table.table)[index as usize].clone())
        }
        fn find(&self, table: Descriptor, key: &[u8]) -> io::Result<Option<u32>> {
            Ok(self
                .rows(table.table)
                .iter()
                .position(|row| row.starts_with(key))
                .map(|i| i as u32))
        }
    }
    /// Law: a lookup answers from the table it names, in the image it names. Two
    /// same-typed tables of one image, or the same table of two images, share
    /// no row and no answer, whether opened directly or decoded from a value.
    #[test]
    fn same_typed_tables_of_two_images_answer_apart() {
        let open = |tag: u32| {
            let namespace = ContentHash::of_bytes(&tag.to_le_bytes());
            (Reader::new(namespace, Arc::new(Image(tag))), namespace)
        };
        let ((one, ns1), (two, ns2)) = (open(1), open(2));
        let at = |namespace, table, len| Descriptor {
            namespace,
            table,
            len,
        };
        for (reader, namespace, tag) in [(&one, ns1, 100), (&two, ns2, 200)] {
            let first: Map<u32, u32> = reader.map(at(namespace, 0, 2)).unwrap();
            let second: Map<u32, u32> = reader.map(at(namespace, 1, 2)).unwrap();
            let (first, second) = (first.clone(), second.clone());
            assert_eq!((first.get(&7), first.index_of(&7)), (Some(&tag), Some(0)));
            assert_eq!(
                (second.get(&7), second.index_of(&7)),
                (Some(&(tag + 11)), Some(1))
            );
            let rows: Vec<Table<u32>> = [2, 3]
                .map(|t| reader.table(at(namespace, t, 1)).unwrap())
                .into();
            assert_eq!((rows[0][0], rows[1][0]), (tag + 20, tag + 30));
            // A value's descriptor decodes into the reader decoding it.
            let wire = bincode::serialize(&second).unwrap();
            let decoded: Map<u32, u32> = reader.decode(|| bincode::deserialize(&wire)).unwrap();
            assert_eq!(decoded.get(&9), Some(&(tag + 10)));
        }
        let wire = bincode::serialize(&one.map::<u32, u32>(at(ns1, 0, 2)).unwrap()).unwrap();
        assert!(
            two.decode(|| bincode::deserialize::<Map<u32, u32>>(&wire))
                .is_err()
        );
        assert!(bincode::deserialize::<Map<u32, u32>>(&wire).is_err());
    }

    /// A key whose encoding asks another map, and panics when it is 13.
    #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    struct Asking(u32);
    thread_local! {
        static ASKED: Map<u32, u32> = Map::published(bind(
            Descriptor { namespace: ContentHash::of_bytes(b"asked"), table: 1, len: 1 },
            Arc::new(Prefix) as Arc<dyn Provider<(u32, u32)>>,
        ))
        .clone();
    }
    impl Serialize for Asking {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            assert_eq!(ASKED.with(|asked| asked.get(&7).copied()), Some(42));
            assert_ne!(self.0, 13, "a key that cannot encode");
            self.0.serialize(s)
        }
    }
    /// Its rows, whose index asks another map while it probes.
    struct Asker;
    impl Provider<(Asking, u32)> for Asker {
        fn row(&self, index: u32) -> io::Result<(Asking, u32)> {
            Ok((Asking(7 + index), 70 + index))
        }
        fn find(&self, key: &[u8]) -> io::Result<Option<u32>> {
            assert_eq!(ASKED.with(|asked| asked.index_of(&7)), Some(0));
            Ok(match key {
                [7] => Some(0),
                [8] => Some(1),
                _ => None,
            })
        }
    }
    /// Law: a lookup made while encoding a key or probing an index answers as
    /// it would alone and leaves the outer key's bytes intact; a lookup that
    /// unwinds leaves the next one unharmed.
    #[test]
    fn lookups_inside_a_lookup_and_after_an_unwind_answer_exactly() {
        let map = Map::published(bind(
            Descriptor {
                namespace: ContentHash::of_bytes(b"asker"),
                table: 0,
                len: 2,
            },
            Arc::new(Asker) as Arc<dyn Provider<(Asking, u32)>>,
        ))
        .clone();
        assert_eq!(
            (map.get(&Asking(8)), map.get(&Asking(7))),
            (Some(&71), Some(&70))
        );
        assert_eq!(map.get(&Asking(9)), None);
        let unwound =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| map.get(&Asking(13))));
        assert!(unwound.is_err());
        assert_eq!(
            (map.get(&Asking(8)), map.index_of(&Asking(9))),
            (Some(&71), None)
        );
        assert_eq!(
            (map.get(&Asking(10)), map.index_of(&Asking(7))),
            (None, Some(0))
        );
        assert_eq!(encode_lookup_key(&Asking(8)), [8]);
    }

    /// An index and rows that each fail once before answering.
    #[derive(Default)]
    struct Flaky {
        finds: std::sync::atomic::AtomicU32,
        rows: std::sync::atomic::AtomicU32,
    }
    impl Provider<(u32, u32)> for Flaky {
        fn row(&self, index: u32) -> io::Result<(u32, u32)> {
            match self.rows.fetch_add(1, std::sync::atomic::Ordering::Relaxed) {
                0 => Err(io::Error::other("row failed")),
                _ => Ok((7, 42 + index)),
            }
        }
        fn find(&self, key: &[u8]) -> io::Result<Option<u32>> {
            match self
                .finds
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            {
                0 => Err(io::Error::other("find failed")),
                _ => Ok((key == [7]).then_some(0)),
            }
        }
    }
    /// Law: a source error is that lookup's error, never its answer; the next
    /// lookup of the same key asks the source again.
    #[test]
    fn a_failed_lookup_leaves_no_answer() {
        let map = Map::published(bind(
            Descriptor {
                namespace: ContentHash::of_bytes(b"flaky"),
                table: 0,
                len: 1,
            },
            Arc::new(Flaky::default()) as Arc<dyn Provider<(u32, u32)>>,
        ))
        .clone();
        let ask = || catch_io(|| Ok(map.get(&7).copied())).map_err(|e| e.to_string());
        assert_eq!(ask(), Err("find failed".into()));
        assert_eq!(ask(), Err("row failed".into()));
        assert_eq!(ask(), Ok(Some(42)));
    }
}
