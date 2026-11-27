//! Runtime `CodeOps` bridge joining `hirsel-core-rt` and `hirsel-code` (PACK-14).
//!
//! Manages host-issued `Loader` capability handles, reference-counted `Code[F]`
//! handles (matching `CodeHeader { entry, descriptor }`), `CodeSlot[F]` handles,
//! and thread-local `Worker` dispatch tables with borrow promotion for parked
//! continuations and worker migration.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use hirsel_code::{
    BorrowToken, CodePin as LoaderPin, Loader, RawCode, RawSlot, Refusal, Stop, Worker,
};
use hirsel_core_ids::ContentHash;
use hirsel_core_rt::code::{CodeOps, CodePin as RtPin};
use hirsel_mod::Malformed;

/// Dynamic `Code[F]` handle layout (bit 0 == 0).
/// Offset 0 MUST be `entry` and offset 8 MUST be `descriptor` (matches `hirsel_core_rt::code_ops`).
#[repr(C)]
struct DynamicCode {
    entry: usize,
    descriptor: usize,
    raw: RawCode,
}

/// Dynamic `CodeSlot[F]` handle layout.
struct DynamicSlot {
    pool_id: u64,
    slot: RawSlot,
}

static POOLS: Mutex<Option<Vec<(u64, Loader)>>> = Mutex::new(None);
static NEXT_POOL_ID: AtomicU32 = AtomicU32::new(1);
static NEXT_THREAD_ID: AtomicU32 = AtomicU32::new(1);

/// Register a [`Loader`] pool as a host-issued `Loader` capability handle (`u64`).
pub fn register_pool(loader: &Loader) -> u64 {
    let id = u64::from(NEXT_POOL_ID.fetch_add(1, Ordering::Relaxed));
    let mut pools = POOLS.lock().unwrap();
    pools
        .get_or_insert_with(Vec::new)
        .push((id, loader.clone()));
    id
}

fn lookup_pool(id: u64) -> Option<Loader> {
    let pools = POOLS.lock().unwrap();
    pools
        .as_ref()?
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, l)| l.clone())
}

fn all_pools() -> Vec<Loader> {
    let pools = POOLS.lock().unwrap();
    pools
        .as_ref()
        .map(|v| v.iter().map(|(_, l)| l.clone()).collect())
        .unwrap_or_default()
}

struct ActiveBorrow {
    id: u64,
    task: usize,
    pool_id: u64,
    token: BorrowToken,
}

struct ThreadDispatch {
    thread_tag: u64,
    seq: u64,
    workers: HashMap<u64, Worker>,
    active: Vec<ActiveBorrow>,
}

impl ThreadDispatch {
    fn new() -> Self {
        let tid = u64::from(NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed));
        Self {
            thread_tag: tid << 32,
            seq: 1,
            workers: HashMap::new(),
            active: Vec::new(),
        }
    }

    fn next_id(&mut self) -> u64 {
        let id = self.thread_tag | (self.seq & 0xffff_ffff);
        self.seq = self.seq.wrapping_add(1).max(1);
        id
    }

    fn worker_mut(&mut self, pool_id: u64) -> Option<&mut Worker> {
        if !self.workers.contains_key(&pool_id) {
            let loader = lookup_pool(pool_id)?;
            self.workers.insert(pool_id, loader.worker());
        }
        self.workers.get_mut(&pool_id)
    }
}

thread_local! {
    static DISPATCH: RefCell<ThreadDispatch> = RefCell::new(ThreadDispatch::new());
}

fn wrap_pin(pin: LoaderPin) -> RtPin {
    let boxed = Box::into_raw(Box::new(pin));
    unsafe fn drop_pin(ptr: *const ()) {
        // SAFETY: created by `Box::into_raw(Box::new(pin))` in `wrap_pin`.
        drop(unsafe { Box::from_raw(ptr.cast_mut().cast::<LoaderPin>()) });
    }
    // SAFETY: `drop_pin` matches `boxed` and runs at most once.
    unsafe { RtPin::from_raw(boxed.cast(), drop_pin) }
}

fn malformed_str(m: Malformed) -> &'static str {
    match m {
        Malformed::Magic => "malformed module magic",
        Malformed::Header => "malformed module header",
        Malformed::Sections => "malformed module sections",
        Malformed::Imports => "malformed module imports",
        Malformed::TextFixups => "malformed module text fixups",
        Malformed::DataFixups => "malformed module data fixups",
        Malformed::Entries => "malformed module entries",
        Malformed::Spans => "malformed module spans",
        Malformed::Sites => "malformed module sites",
        Malformed::Names => "malformed module names",
    }
}

unsafe fn write_static_str(out_bytes: *mut u8, s: &'static str) {
    // SAFETY: `out_bytes` has at least 24 bytes for a Hirsel `String` carrier `{ptr, len, cap=0}`.
    unsafe {
        out_bytes
            .cast::<[u64; 3]>()
            .write([s.as_ptr().addr() as u64, s.len() as u64, 0]);
    }
}

fn alloc_dynamic_code(raw: RawCode) -> *mut u8 {
    let handle = Arc::new(DynamicCode {
        entry: raw.entry(),
        descriptor: raw.descriptor(),
        raw,
    });
    let ptr = Arc::into_raw(handle).cast_mut().cast::<u8>();
    debug_assert_eq!(
        ptr.addr() & 1,
        0,
        "DynamicCode pointer must be 8-byte aligned"
    );
    ptr
}

unsafe fn load_by_key(
    loader_id: u64,
    key: *const [u8; 32],
    entry_id: *const [u8; 32],
    sig: *const [u8; 32],
    waits: u8,
    out_code: *mut *mut u8,
    out_word: *mut u64,
    out_bytes: *mut u8,
) -> u8 {
    let Some(loader) = lookup_pool(loader_id) else {
        return 1; // NotAdmitted
    };
    // SAFETY: caller passes valid 32-byte digest pointers.
    let (key, entry_id, sig) =
        unsafe { (ContentHash(*key), ContentHash(*entry_id), ContentHash(*sig)) };
    match loader.load_by_key(&key, &entry_id, &sig, waits != 0, true) {
        Ok(raw) => {
            // SAFETY: caller provides writable `out_code`.
            unsafe { *out_code = alloc_dynamic_code(raw) };
            0
        }
        Err(Refusal::NotAdmitted) => 1,
        Err(Refusal::Malformed(m)) => {
            // SAFETY: caller provides 32-byte `out_bytes`.
            unsafe { write_static_str(out_bytes, malformed_str(m)) };
            2
        }
        Err(Refusal::Mismatch(msg)) => {
            // SAFETY: caller provides 32-byte `out_bytes`.
            unsafe { write_static_str(out_bytes, msg) };
            3
        }
        Err(Refusal::Import(row)) => {
            // SAFETY: caller provides writable `out_word`.
            unsafe { *out_word = u64::from(row) };
            4
        }
        Err(Refusal::Unbound(hash)) => {
            // SAFETY: caller provides 32-byte `out_bytes`.
            unsafe { ptr::copy_nonoverlapping(hash.0.as_ptr(), out_bytes, 32) };
            5
        }
        Err(Refusal::Fixup(index)) => {
            // SAFETY: caller provides writable `out_word`.
            unsafe { *out_word = u64::from(index) };
            6
        }
        Err(Refusal::NoEntry) => 7,
        Err(Refusal::Capacity) => 8,
        Err(Refusal::Quarantined) => 9,
        Err(Refusal::NotPrelinked) => 10,
        Err(Refusal::Unsupported) => 11,
    }
}

unsafe fn bind_loaded(entry: usize, descriptor: usize) -> *mut u8 {
    for loader in all_pools() {
        if let Some(raw) = loader.bind_loaded(entry, descriptor) {
            return alloc_dynamic_code(raw);
        }
    }
    ptr::null_mut()
}

unsafe fn pin_pc(pc: usize) -> Option<RtPin> {
    for loader in all_pools() {
        if let Some(pin) = loader.pin_pc(pc) {
            return Some(wrap_pin(pin));
        }
    }
    None
}

unsafe fn code_share(code: *mut u8) -> *mut u8 {
    debug_assert_eq!(code.addr() & 1, 0);
    // SAFETY: `code` is a live `Arc::into_raw` pointer to `DynamicCode`.
    unsafe { Arc::increment_strong_count(code.cast::<DynamicCode>()) };
    code
}

unsafe fn code_drop(code: *mut u8) {
    debug_assert_eq!(code.addr() & 1, 0);
    // SAFETY: `code` owns one `Arc<DynamicCode>` reference.
    drop(unsafe { Arc::from_raw(code.cast::<DynamicCode>()) });
}

unsafe fn slot_new(loader_id: u64) -> *mut u8 {
    let Some(loader) = lookup_pool(loader_id) else {
        return ptr::null_mut();
    };
    let slot = Box::new(DynamicSlot {
        pool_id: loader_id,
        slot: loader.raw_slot(),
    });
    Box::into_raw(slot).cast()
}

unsafe fn slot_drop(slot: *mut u8) {
    // SAFETY: created by `slot_new`.
    drop(unsafe { Box::from_raw(slot.cast::<DynamicSlot>()) });
}

unsafe fn slot_activate(slot: *mut u8, expected: u64, code: *mut u8, out_version: *mut u64) -> u8 {
    // SAFETY: `slot` is a live `DynamicSlot`.
    let slot = unsafe { &*slot.cast::<DynamicSlot>() };
    let raw = if code.addr() & 1 != 0 {
        let desc = (code.addr() & !1) as *const usize;
        // SAFETY: tagged static descriptor has `entry` at word 0.
        let entry = unsafe { desc.read() };
        RawCode::static_entry(entry, desc.addr())
    } else {
        // SAFETY: untagged handle is a live `DynamicCode`.
        unsafe { (&*code.cast::<DynamicCode>()).raw.clone() }
    };
    match slot.slot.activate(expected, &raw) {
        Ok(version) => {
            // SAFETY: caller provides writable `out_version`.
            unsafe { *out_version = version };
            0
        }
        Err(Stop::WrongPool) => 1,
        Err(Stop::Stale(current)) => {
            // SAFETY: caller provides writable `out_version`.
            unsafe { *out_version = current };
            2
        }
        Err(Stop::Capacity) => 3,
    }
}

unsafe fn dispatch_begin(slot: *mut u8, task: usize, out_token: *mut u64) -> *mut u8 {
    // SAFETY: `slot` is a live `DynamicSlot`.
    let slot = unsafe { &*slot.cast::<DynamicSlot>() };
    DISPATCH.with(|cell| {
        let mut state = cell.borrow_mut();
        let Some(worker) = state.worker_mut(slot.pool_id) else {
            return ptr::null_mut();
        };
        let Some((entry, token)) = worker.begin_dispatch(&slot.slot) else {
            return ptr::null_mut();
        };
        let id = state.next_id();
        state.active.push(ActiveBorrow {
            id,
            task,
            pool_id: slot.pool_id,
            token,
        });
        // SAFETY: caller provides writable `out_token`.
        unsafe { *out_token = id };
        entry as *mut u8
    })
}

unsafe fn dispatch_end(token: u64) -> bool {
    DISPATCH.with(|cell| {
        let mut state = cell.borrow_mut();
        let pos = if state.active.last().is_some_and(|b| b.id == token) {
            Some(state.active.len() - 1)
        } else {
            state.active.iter().rposition(|b| b.id == token)
        };
        let Some(pos) = pos else {
            return false;
        };
        let borrow = state.active.remove(pos);
        if let Some(worker) = state.workers.get_mut(&borrow.pool_id) {
            worker.end_dispatch(borrow.token);
        }
        true
    })
}

unsafe fn promote_borrows(task: usize, out: &mut Vec<(u64, RtPin)>) {
    DISPATCH.with(|cell| {
        let mut state = cell.borrow_mut();
        if state.active.is_empty() {
            return;
        }
        let mut i = 0;
        while i < state.active.len() {
            if state.active[i].task == task {
                let mut borrow = state.active.remove(i);
                if let Some(worker) = state.workers.get_mut(&borrow.pool_id) {
                    let pin = worker.promote(&mut borrow.token);
                    out.push((borrow.id, wrap_pin(pin)));
                }
            } else {
                i += 1;
            }
        }
    });
}

unsafe fn worker_park() {
    DISPATCH.with(|cell| {
        let mut state = cell.borrow_mut();
        for worker in state.workers.values_mut() {
            worker.park();
        }
    });
}

/// Active worker borrow count on the current OS thread (for white-box pinning laws).
pub fn current_thread_active_borrows() -> u32 {
    DISPATCH.with(|cell| {
        cell.borrow()
            .workers
            .values()
            .map(Worker::active_borrows)
            .sum()
    })
}

static OPS: CodeOps = CodeOps {
    load_by_key,
    bind_loaded,
    pin_pc,
    code_share,
    code_drop,
    slot_new,
    slot_drop,
    slot_activate,
    dispatch_begin,
    dispatch_end,
    promote_borrows,
    worker_park,
};

pub(crate) fn install_ops() {
    hirsel_core_rt::code::register_ops(&OPS);
}
