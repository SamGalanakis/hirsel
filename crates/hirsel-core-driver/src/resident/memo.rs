//! Exact derivations can be shared between resident working sets. Each set
//! owns the entries its last two requests used: after a request its older
//! entries are released (an edit's stale derivations), and under memory
//! pressure whole sets are released, least recently used first. An entry
//! stays while any set owns it.

use super::serving;
use hirsel_core_ids::ContentHash;
use hirsel_core_resolve::{ModuleId, Resolution, SourceId};
use hirsel_core_syntax::{Derive, ParseMode, ParseOutput, Symbol, Symbols};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SetId(usize);

static ACTIVE: AtomicUsize = AtomicUsize::new(0);
/// The serial number of the request being served.
static REQUEST: AtomicU64 = AtomicU64::new(0);

/// The set being served and its request's serial number.
fn current() -> (SetId, u64) {
    (
        SetId(ACTIVE.load(Ordering::Relaxed)),
        REQUEST.load(Ordering::Relaxed),
    )
}

/// The path identifies the work whose state can be evicted together. It
/// never substitutes for a memo key or validates a previous result.
#[derive(PartialEq, Eq)]
pub(super) enum Input {
    Package(PathBuf, Option<String>),
    Manifest(PathBuf, Option<String>),
    Recipe(String),
}

impl Input {
    pub(super) fn absolute(self, cwd: &Path) -> Self {
        let absolute = |path: PathBuf| {
            let path = cwd.join(path);
            path.canonicalize().unwrap_or(path)
        };
        match self {
            Self::Package(path, entry) => Self::Package(absolute(path), entry),
            Self::Manifest(path, entry) => Self::Manifest(absolute(path), entry),
            Self::Recipe(_) => self,
        }
    }
}

struct Set {
    id: SetId,
    input: Input,
    /// The serial number of the set's request before its latest one.
    previous: u64,
    latest: u64,
}

#[derive(Default)]
pub(super) struct WorkingSets {
    age: VecDeque<Set>,
    next: usize,
    requests: u64,
}

impl WorkingSets {
    fn touch(&mut self, input: Input) -> SetId {
        self.requests += 1;
        let mut set = match self.age.iter().position(|set| set.input == input) {
            Some(index) => self.age.remove(index).unwrap(),
            None => {
                self.next += 1;
                Set {
                    id: SetId(self.next),
                    input,
                    previous: 0,
                    latest: 0,
                }
            }
        };
        set.previous = set.latest;
        set.latest = self.requests;
        let id = set.id;
        self.age.push_back(set);
        id
    }

    pub(super) fn begin(&mut self, input: Input) {
        ACTIVE.store(self.touch(input).0, Ordering::Relaxed);
        REQUEST.store(self.requests, Ordering::Relaxed);
    }

    /// After the served set's request: release what it last used before its
    /// previous request, the derivations of inputs it no longer has.
    pub(super) fn settle(&mut self) {
        let (id, _) = current();
        if let Some(set) = self.age.iter().find(|set| set.id == id) {
            let before = set.previous;
            release(&|owner, request| owner == id && request < before);
        }
    }

    /// Release every set, as after a request that panicked: what it was
    /// deriving may be half made.
    pub(super) fn forget(&mut self) {
        self.age.clear();
        release(&|_, _| true);
    }

    /// Try to get below the cap, and say if even empty memos are too large.
    fn trim(
        &mut self,
        mut over_budget: impl FnMut() -> bool,
        mut evict: impl FnMut(SetId),
    ) -> bool {
        while over_budget() {
            let Some(set) = self.age.pop_front() else {
                return false;
            };
            evict(set.id);
        }
        true
    }

    pub(super) fn fit(&mut self, max_memory: u64) -> bool {
        if super::resident_bytes() <= max_memory {
            return true;
        }
        release_unused_pages();
        self.trim(
            || super::resident_bytes() > max_memory,
            |id| {
                release(&|owner, _| owner == id);
                release_unused_pages();
            },
        )
    }
}

// Link the mimalloc native library in the driver and its test harnesses.
use mimalloc as _;
unsafe extern "C" {
    fn mi_collect(force: bool);
}

/// Return the allocator's free pages to the system, as an idle server does.
pub(super) fn release_unused_pages() {
    // SAFETY: mimalloc can collect its unused pages at any time. The CLI
    // uses it as its allocator, and this call takes no pointers.
    unsafe { mi_collect(true) };
}

struct Value<V> {
    value: V,
    /// Each set that used it, with the request that last did.
    owners: Vec<(SetId, u64)>,
}

impl<V> Value<V> {
    fn new(value: V, (owner, request): (SetId, u64)) -> Self {
        Self {
            value,
            owners: vec![(owner, request)],
        }
    }

    fn touch(&mut self, (id, request): (SetId, u64)) {
        match self.owners.iter_mut().find(|(owner, _)| *owner == id) {
            Some(owner) => owner.1 = request,
            None => self.owners.push((id, request)),
        }
    }

    /// Drop the ownerships `drop` names; false when none remains.
    fn release(&mut self, drop: &dyn Fn(SetId, u64) -> bool) -> bool {
        self.owners
            .retain(|&(owner, request)| !drop(owner, request));
        !self.owners.is_empty()
    }
}

/// A pure derivation, keyed by every input it reads. Outside a server it
/// remembers nothing.
pub(crate) struct Memo<K, V> {
    entries: LazyLock<Mutex<HashMap<K, Value<V>>>>,
}

impl<K: std::hash::Hash + Eq, V: Clone> Memo<K, V> {
    pub(crate) const fn new() -> Self {
        Self {
            entries: LazyLock::new(Default::default),
        }
    }

    /// The value under `key`, made by `make` unless remembered. A None key
    /// is a derivation this memo does not key.
    pub(crate) fn get_or<E>(
        &self,
        key: Option<K>,
        make: impl FnOnce() -> Result<V, E>,
    ) -> Result<V, E> {
        let Some(key) = key.filter(|_| serving()) else {
            return make();
        };
        let owner = current();
        if let Some(entry) = self.entries.lock().unwrap().get_mut(&key) {
            entry.touch(owner);
            return Ok(entry.value.clone());
        }
        let value = make()?;
        self.remember(key, value.clone(), owner);
        Ok(value)
    }

    pub(crate) fn get(&self, key: &K) -> Option<V> {
        if !serving() {
            return None;
        }
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.get_mut(key)?;
        entry.touch(current());
        Some(entry.value.clone())
    }

    pub(crate) fn insert(&self, key: K, value: V) {
        if serving() {
            self.remember(key, value, current());
        }
    }

    fn remember(&self, key: K, value: V, owner: (SetId, u64)) {
        use std::collections::hash_map::Entry;
        match self.entries.lock().unwrap().entry(key) {
            Entry::Occupied(mut entry) => {
                let entry = entry.get_mut();
                entry.value = value;
                entry.touch(owner);
            }
            Entry::Vacant(entry) => {
                entry.insert(Value::new(value, owner));
            }
        }
    }

    fn release(&self, drop: &dyn Fn(SetId, u64) -> bool) {
        let Some(entries) = LazyLock::get(&self.entries) else {
            return;
        };
        let mut entries = entries.lock().unwrap();
        let before = entries.len();
        entries.retain(|_, entry| entry.release(drop));
        if entries.len() < before {
            entries.shrink_to_fit();
        }
    }
}

/// A separate previous result for each set, with the supplied keys intact.
pub(crate) struct WorkingMemo<K, V>(Memo<(SetId, K), V>);

impl<K: std::hash::Hash + Eq, V: Clone> WorkingMemo<K, V> {
    pub(crate) const fn new() -> Self {
        Self(Memo::new())
    }

    pub(crate) fn get_or<E>(
        &self,
        key: Option<K>,
        make: impl FnOnce() -> Result<V, E>,
    ) -> Result<V, E> {
        self.0.get_or(key.map(|key| (current().0, key)), make)
    }
}

impl<T: Default> WorkingMemo<(), Arc<Mutex<T>>> {
    pub(crate) fn previous(&self) -> Option<Arc<Mutex<T>>> {
        self.0.get(&(current().0, ()))
    }

    pub(crate) fn previous_or_default(&self) -> Arc<Mutex<T>> {
        self.get_or(Some(()), || {
            Ok::<_, std::convert::Infallible>(Arc::default())
        })
        .unwrap()
    }
}

/// Materialised instances, by identity and definition record.
pub(crate) static MATERIALISED: Memo<
    (
        hirsel_core_ir::Identity,
        Option<hirsel_core_ids::ContentHash>,
        hirsel_core_layout::Target,
    ),
    crate::instances::Materialised,
> = Memo::new();

/// The last accepted recipe header walk for an entry and its check inputs.
pub(crate) static RECIPE_REACH: WorkingMemo<
    ContentHash,
    Arc<Mutex<Option<crate::reachability::RecipeGraph>>>,
> = WorkingMemo::new();

/// Address plans for entries of a local build.
pub(crate) static RECIPE_GROUPS: WorkingMemo<
    ContentHash,
    Arc<Mutex<Option<crate::native::recipe_groups::Groups>>>,
> = WorkingMemo::new();

/// The site items each link input references, by the input's content key.
pub(crate) static REFERENCED: Memo<
    (hirsel_core_ids::ContentHash, u64),
    crate::native_entry::Referenced,
> = Memo::new();

/// The part of a remembered source map a link's inputs reference, and whether
/// they spawn units, by the map's key and the digest of what each input
/// references.
pub(crate) static RUNTIME_SOURCES: Memo<
    (
        hirsel_core_ids::ContentHash,
        Vec<hirsel_core_ids::ContentHash>,
    ),
    (Arc<hirsel_core_ir::source::SourceMap>, bool),
> = Memo::new();

/// Where each location of a shared product's debug rows lands in a source
/// map (`native_debug::side`), by the map's key and the rows' address; the
/// entry holds the rows, so that address names them while it is remembered.
pub(crate) static DEBUG_POSITIONS: Memo<
    (ContentHash, usize),
    (
        Arc<hirsel_core_ir::source::NativeDebug>,
        crate::native_debug::Locations,
    ),
> = Memo::new();

/// Each stored object record's key-determined parts, by its key alone: a
/// key determines its object whichever store holds it.
/// Each computed constant's value or refusal cause, by its initialiser's code
/// address (PROG-10): an unchanged initialiser is not run again. The run reads
/// nothing but the code the address names, so targets whose code agrees share it.
pub(crate) static CONSTANTS: Memo<ContentHash, Result<Arc<crate::comptime::Value>, Arc<str>>> =
    Memo::new();
pub(crate) static OBJECT_RECORDS: Memo<ContentHash, crate::native_object::Shared> = Memo::new();

#[derive(Clone)]
struct SourceHash {
    bytes: Arc<[u8]>,
    hash: ContentHash,
}

static SOURCE_HASHES: Memo<PathBuf, SourceHash> = Memo::new();

/// Hash a captured file once per content change. The path selects the
/// previous capture; an exact byte comparison confirms the hit.
pub(crate) fn source_hash(path: &Path, bytes: &Arc<[u8]>) -> ContentHash {
    if !serving() {
        return ContentHash::of_bytes(bytes);
    }
    let owner = current();
    let mut entries = SOURCE_HASHES.entries.lock().unwrap();
    if let Some(prior) = entries.get_mut(path)
        && prior.value.bytes == *bytes
    {
        prior.touch(owner);
        return prior.value.hash;
    }
    let hash = ContentHash::of_bytes(bytes);
    entries.insert(
        path.to_owned(),
        Value::new(
            SourceHash {
                bytes: Arc::clone(bytes),
                hash,
            },
            owner,
        ),
    );
    hash
}

#[derive(Clone)]
struct Parsed {
    output: Arc<ParseOutput>,
    added: Arc<[String]>,
    after: ContentHash,
}

static PARSED: Memo<ContentHash, Parsed> = Memo::new();

/// The parse stores numeric symbols. The key names all earlier assignments,
/// and replaying the parse's additions gives later files the same numbers.
pub(crate) fn parse_file(
    prefix: &mut ContentHash,
    symbols: &mut Symbols,
    hash: ContentHash,
    bytes: &[u8],
    mode: ParseMode,
    derive: Option<&Derive>,
) -> io::Result<(ContentHash, ParseOutput, bool)> {
    let style = derive.map(|d| (&d.codec, &d.path));
    let key = crate::native::key("hirsel.resident-parse.v1", &(*prefix, hash, mode, style))?;
    let start = symbols.len();
    let mut miss = false;
    let parsed = PARSED.get_or(Some(key), || {
        miss = true;
        let output = hirsel_core_resolve::parse_module(bytes, mode, symbols, derive);
        let added: Arc<[String]> = symbol_suffix(symbols, start).into();
        let after = if added.is_empty() {
            *prefix
        } else {
            crate::native::key("hirsel.resident-symbols.v1", &(*prefix, &added))?
        };
        Ok::<_, io::Error>(Parsed {
            output: Arc::new(output),
            added,
            after,
        })
    })?;
    if !miss {
        for name in parsed.added.iter() {
            symbols.intern(name);
        }
    }
    *prefix = parsed.after;
    Ok((key, (*parsed.output).clone(), miss))
}

pub(crate) fn symbol_suffix(symbols: &Symbols, start: usize) -> Vec<String> {
    (start..symbols.len())
        .map(|i| symbols.text(Symbol(i as u32)).to_owned())
        .collect()
}

/// File syntax plus its occurrence, the whole request's index shape and
/// final symbol assignments. Diagnostics with cross-file sites are rebuilt
/// against the current index.
pub(crate) static RESOLVED: Memo<
    (ContentHash, ContentHash, ContentHash, ModuleId, SourceId),
    Vec<Arc<Resolution>>,
> = Memo::new();

/// Drop, from every memo, the ownerships `drop` names.
fn release(drop: &dyn Fn(SetId, u64) -> bool) {
    MATERIALISED.release(drop);
    RECIPE_REACH.0.release(drop);
    RECIPE_GROUPS.0.release(drop);
    REFERENCED.release(drop);
    OBJECT_RECORDS.release(drop);
    DEBUG_POSITIONS.release(drop);
    RUNTIME_SOURCES.release(drop);
    SOURCE_HASHES.release(drop);
    PARSED.release(drop);
    RESOLVED.release(drop);
    crate::incremental_link::RESIDENT.0.release(drop);
    crate::code_store::PACKS.release(drop);
    crate::source_delta::STATE.0.release(drop);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn pressure_evicts_whole_working_sets_in_lru_order() {
        let mut sets = WorkingSets::default();
        let input = |name: &str| Input::Package(PathBuf::from(name), None);
        let a = sets.touch(input("A"));
        let b = sets.touch(input("B"));
        assert_eq!(sets.touch(input("A")), a);
        assert_eq!(sets.touch(input("B")), b);
        let c = sets.touch(input("C"));
        assert_eq!(sets.touch(input("A")), a);

        let parses: Memo<u32, &str> = Memo::new();
        let links: Memo<u32, &str> = Memo::new();
        {
            let mut parses = parses.entries.lock().unwrap();
            parses.insert(1, Value::new("A and B", (a, 0)));
            parses.get_mut(&1).unwrap().touch((b, 0));
            parses.insert(2, Value::new("B only", (b, 0)));
            parses.insert(3, Value::new("C only", (c, 0)));
        }
        {
            let mut links = links.entries.lock().unwrap();
            links.insert(4, Value::new("B link", (b, 0)));
            links.insert(5, Value::new("A link", (a, 0)));
        }

        assert!(sets.trim(|| false, |_| panic!("evicted under budget")));
        let evictions = RefCell::new(Vec::new());
        assert!(sets.trim(
            || evictions.borrow().len() < 2,
            |id| {
                evictions.borrow_mut().push(id);
                parses.release(&|owner, _| owner == id);
                links.release(&|owner, _| owner == id);
            },
        ));
        assert_eq!(evictions.into_inner(), [b, c]);
        let parses = parses.entries.lock().unwrap();
        assert_eq!(parses.len(), 1);
        assert_eq!(parses[&1].value, "A and B");
        let links = links.entries.lock().unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[&5].value, "A link");
    }
}
