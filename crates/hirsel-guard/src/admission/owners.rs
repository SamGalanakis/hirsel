//! Live-block ownership follows dhat-rs/src/lib.rs:1228-1318 (nnethercote/dhat-rs).
//! Differences: no backtraces/time; reserve before allocation; keep each owner
//! until its last allocation frees; charge the tracking table's own backing.
use super::{OwnerKind, host_budget};
use std::{
    alloc::Layout,
    cell::RefCell,
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

static INSTALLED: AtomicBool = AtomicBool::new(false);
pub(crate) fn allocator_installed() {
    INSTALLED.store(true, Ordering::Relaxed);
}
struct Account {
    kind: OwnerKind,
    bytes: AtomicU64,
}
#[derive(Clone)]
pub struct Owner(Arc<Account>);
impl Owner {
    pub fn new(kind: OwnerKind) -> Self {
        host_budget();
        assert!(
            INSTALLED.load(Ordering::Relaxed),
            "canonical owner admission requires the guarded allocator"
        );
        let _scope = Scope::enter(State::Ledger);
        Self(Arc::new(Account {
            kind,
            bytes: AtomicU64::new(0),
        }))
    }
    pub fn enter(&self) -> OwnerScope {
        OwnerScope {
            _scope: Scope::enter(State::Owner(self.0.clone())),
        }
    }
    pub fn bytes(&self) -> u64 {
        self.0.bytes.load(Ordering::Relaxed)
    }
    pub fn owns(&self, address: usize, bytes: usize) -> bool {
        if bytes == 0 {
            return true;
        }
        let _scope = Scope::enter(State::Metadata);
        blocks()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&address)
            .is_some_and(|record| {
                record.bytes == bytes
                    && matches!(&record.owner, Tag::Owner(owner) if Arc::ptr_eq(owner, &self.0))
            })
    }
    pub fn owns_arc<T: ?Sized>(&self, value: &T) -> bool {
        let (layout, offset) = Layout::new::<[usize; 2]>()
            .extend(Layout::for_value(value))
            .expect("existing Arc layout");
        self.owns(
            value as *const T as *const () as usize - offset,
            layout.pad_to_align().size(),
        )
    }
    /// Adopt an existing allocation once, before a canonical row retains it.
    /// Existing canonical owners keep the charge through shared aliases.
    pub fn adopt(&self, address: usize, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let _scope = Scope::enter(State::Metadata);
        let mut blocks = blocks().lock().unwrap_or_else(|e| e.into_inner());
        if blocks.contains_key(&address) {
            return;
        }
        let record = Record {
            owner: Tag::Owner(self.0.clone()),
            bytes,
        };
        record.owner.reserve(bytes);
        blocks.insert(address, record);
    }
    pub fn adopt_arc<T: ?Sized>(&self, value: &T) {
        let (layout, offset) = Layout::new::<[usize; 2]>()
            .extend(Layout::for_value(value))
            .expect("existing Arc layout");
        self.adopt(
            value as *const T as *const () as usize - offset,
            layout.pad_to_align().size(),
        );
    }
}
pub struct OwnerScope {
    _scope: Scope,
}
#[derive(Clone)]
enum State {
    None,
    Owner(Arc<Account>),
    Ledger,
    Metadata,
}
thread_local! { static ACTIVE: RefCell<State> = const { RefCell::new(State::None) }; }
fn active() -> State {
    ACTIVE
        .try_with(|slot| slot.borrow().clone())
        .unwrap_or(State::None)
}
struct Scope {
    previous: Option<State>,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl Scope {
    fn enter(state: State) -> Self {
        match ACTIVE.try_with(|slot| slot.replace(state)) {
            Ok(previous) => Self {
                previous: Some(previous),
                _thread: std::marker::PhantomData,
            },
            Err(_) => crate::owner_exhausted(OwnerKind::Ledger, 0),
        }
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take()
            && let Ok(current) = ACTIVE.try_with(|slot| slot.replace(previous))
        {
            drop(current);
        }
    }
}
#[derive(Clone)]
enum Tag {
    Owner(Arc<Account>),
    Ledger,
}
impl Tag {
    fn kind(&self) -> OwnerKind {
        match self {
            Self::Owner(owner) => owner.kind,
            Self::Ledger => OwnerKind::Ledger,
        }
    }
    fn reserve(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        if !host_budget().0.retain_owner(self.kind(), bytes as u64) {
            crate::owner_exhausted(self.kind(), bytes);
        }
        if let Self::Owner(owner) = self {
            owner.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        }
    }
    fn release(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        if let Self::Owner(owner) = self {
            let before = owner.bytes.fetch_sub(bytes as u64, Ordering::Relaxed);
            assert!(before >= bytes as u64, "live allocation owns its charge");
        }
        host_budget().0.release_owner(self.kind(), bytes as u64);
    }
}
pub(crate) struct Record {
    owner: Tag,
    bytes: usize,
}
static READY: AtomicBool = AtomicBool::new(false);
static BLOCKS: OnceLock<Mutex<HashMap<usize, Record>>> = OnceLock::new();
fn blocks() -> &'static Mutex<HashMap<usize, Record>> {
    let result = BLOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    READY.store(true, Ordering::Release);
    result
}
pub(crate) enum Allocation {
    None,
    Recorded(Record),
    Metadata,
}
pub(crate) fn before_alloc(bytes: usize) -> Allocation {
    let owner = match active() {
        State::None => return Allocation::None,
        State::Metadata => {
            Tag::Ledger.reserve(bytes);
            return Allocation::Metadata;
        }
        State::Ledger => Tag::Ledger,
        State::Owner(owner) => Tag::Owner(owner),
    };
    owner.reserve(bytes);
    Allocation::Recorded(Record { owner, bytes })
}
pub(crate) fn allocated(address: usize, allocation: Allocation) {
    if let Allocation::Recorded(record) = allocation {
        let _scope = Scope::enter(State::Metadata);
        let previous = blocks()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(address, record);
        assert!(previous.is_none(), "live allocation addresses are unique");
    }
}
fn remove(address: usize) -> Option<Record> {
    if !READY.load(Ordering::Acquire) {
        return None;
    }
    // Removal neither allocates nor frees table backing. Avoid touching a
    // destructible TLS scope here: other TLS destructors may still free blocks.
    blocks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&address)
}
pub(crate) fn deallocated(address: usize, bytes: usize) {
    if matches!(active(), State::Metadata) {
        Tag::Ledger.release(bytes);
        return;
    }
    if let Some(record) = remove(address) {
        assert_eq!(
            record.bytes, bytes,
            "deallocation uses its original requested layout"
        );
        record.owner.release(bytes);
    }
}
pub(crate) fn reallocated(address: usize, old_bytes: usize, new_bytes: usize) -> Allocation {
    if matches!(active(), State::Metadata) {
        if new_bytes > old_bytes {
            Tag::Ledger.reserve(new_bytes - old_bytes);
        } else {
            Tag::Ledger.release(old_bytes - new_bytes);
        }
        return Allocation::Metadata;
    }
    match remove(address) {
        Some(mut record) => {
            assert_eq!(record.bytes, old_bytes);
            if new_bytes > old_bytes {
                record.owner.reserve(new_bytes - old_bytes);
            } else {
                record.owner.release(old_bytes - new_bytes);
            }
            record.bytes = new_bytes;
            Allocation::Recorded(record)
        }
        None => before_alloc(new_bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_allocation_releases_its_original_owner_on_another_thread() {
        let owner = Owner::new(OwnerKind::SessionTypes);
        let block = {
            let _scope = owner.enter();
            std::hint::black_box(Box::new([7u8; 4096]))
        };
        assert_eq!(owner.bytes(), 4096);
        std::thread::spawn(move || drop(block)).join().unwrap();
        assert_eq!(owner.bytes(), 0);
    }
    #[test]
    fn adoption_and_reallocation_preserve_one_charge_until_the_last_alias() {
        let owner = Owner::new(OwnerKind::SessionTypes);
        let mut bytes = Vec::<u64>::with_capacity(16);
        owner.adopt(bytes.as_ptr() as usize, bytes.capacity() * size_of::<u64>());
        owner.adopt(bytes.as_ptr() as usize, bytes.capacity() * size_of::<u64>());
        assert_eq!(owner.bytes(), 16 * size_of::<u64>() as u64);
        bytes.resize(128, 9);
        assert_eq!(owner.bytes(), (bytes.capacity() * size_of::<u64>()) as u64);
        drop(bytes);
        assert_eq!(owner.bytes(), 0);
        let shared = Arc::new([1u64; 64]);
        owner.adopt_arc(&*shared);
        let charged = owner.bytes();
        assert!(charged >= size_of_val(&*shared) as u64);
        let alias = shared.clone();
        owner.adopt_arc(&*alias);
        assert_eq!(owner.bytes(), charged);
        drop(shared);
        assert_eq!(owner.bytes(), charged);
        drop(alias);
        assert_eq!(owner.bytes(), 0);
    }
}
