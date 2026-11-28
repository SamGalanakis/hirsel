//! Loaded code as the runtime sees it (PACK-9).
//!
//! The runtime does not link the loader. A pack host registers each pool's bands and
//! its loader's lookups here. The fault handler, the profile sampler and the handle
//! rows ask this module which pool and which image own an address.

use std::sync::atomic::{AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering};

#[path = "held.rs"]
mod held;
#[path = "code_ops.rs"]
mod ops;
pub use ops::{
    CodeOps, CodePin, hirsel_core_code_activate, hirsel_core_code_bind,
    hirsel_core_code_dispatch_begin, hirsel_core_code_dispatch_end, hirsel_core_code_drop,
    hirsel_core_code_identity, hirsel_core_code_load, hirsel_core_code_revision,
    hirsel_core_code_share, hirsel_core_code_slot_drop, hirsel_core_code_slot_new, register_ops,
    worker_park,
};
pub(crate) use ops::{ExecutionPin, pin_pc, promote_worker_borrows};

/// The image that owns a code address: the first 16 bytes of its module digest and the
/// address's offset in the image's text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hit {
    pub digest: [u8; 16],
    pub offset: u32,
}

/// Result of resolving a static literal pointer inside a loaded image's data band.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LiteralOwner {
    /// Retained reference to the release's [`crate::causes::CauseData`]; the
    /// caller receives one owned count.
    pub owner: *const crate::causes::CauseData,
    /// Corresponding byte address inside `owner`'s independent R/NX mapping.
    pub relocated: *const u8,
}

/// One pool's text and data bands and its loader's lock-free lookups.
pub struct Band {
    /// The text band is `[low, high)`.
    pub low: usize,
    pub high: usize,
    /// The data band is `[data_low, data_high)`.
    pub data_low: usize,
    pub data_high: usize,
    /// The loader every lookup below is called with.
    pub loader: *const (),
    /// The image whose text holds a code address. It must be async-signal-safe: the
    /// fault handler calls it.
    pub resolve: unsafe fn(*const (), usize) -> Option<Hit>,
    /// The independent data owner for a static literal `[ptr, ptr + len)` inside
    /// `[data_low, data_high)`, retaining one count while pinned. Allocates and
    /// admits nothing.
    pub resolve_literal: unsafe fn(*const (), usize, usize) -> Option<LiteralOwner>,
    /// An owning pin on the live image whose text holds a code address. It takes no
    /// lock.
    pub pin: unsafe fn(*const (), usize) -> Option<CodePin>,
    /// A counted `Code[F]` handle for the entry address and PROG-9 descriptor of a
    /// live image, or null when no live image holds the entry.
    pub bind: unsafe fn(*const (), usize, usize) -> *mut u8,
}

// SAFETY: a registered band is never changed, and `resolve` is async-signal-safe, so it
// is also safe to call from any thread.
unsafe impl Sync for Band {}

/// A registered pool. Nothing changes or frees one once the list names it, so a signal
/// handler may walk the list.
struct Pool {
    handle: u64,
    band: &'static Band,
    next: *const Pool,
}

/// The newest registered pool.
static POOLS: AtomicPtr<Pool> = AtomicPtr::new(std::ptr::null_mut());

/// Registered pools and private executions share the same never-reused namespace.
pub(crate) fn fresh_pool_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("pool identifiers exhausted")
}

/// Make `band` one of this process's pools and take the signals a fault or a trap in
/// loaded code raises (PACK-9). Gives the pool's `Loader` capability handle, which is
/// never zero.
pub fn register(band: &'static Band) -> Result<u64, crate::fiber::ArmError> {
    crate::fiber::arm_code()?;
    let pool = Box::into_raw(Box::new(Pool {
        handle: fresh_pool_id(),
        band,
        next: std::ptr::null(),
    }));
    let mut newest = POOLS.load(Ordering::Acquire);
    loop {
        // SAFETY: nothing else reaches `pool` until the exchange below names it, and a
        // registered pool is never freed.
        unsafe {
            (*pool).next = newest;
        }
        match POOLS.compare_exchange_weak(newest, pool, Ordering::AcqRel, Ordering::Acquire) {
            // SAFETY: as above.
            Ok(_) => return Ok(unsafe { (*pool).handle }),
            Err(now) => newest = now,
        }
    }
}

/// Every registered pool's handle and band, newest first. Async-signal-safe.
pub fn pools() -> impl Iterator<Item = (u64, &'static Band)> {
    let mut at = POOLS.load(Ordering::Acquire).cast_const();
    std::iter::from_fn(move || {
        // SAFETY: a registered pool is never changed or freed.
        let pool = unsafe { at.as_ref() }?;
        at = pool.next;
        Some((pool.handle, pool.band))
    })
}

/// The pool whose text band holds `pc`. Async-signal-safe.
pub(crate) fn text(pc: usize) -> Option<&'static Band> {
    pools()
        .map(|(_, band)| band)
        .find(|band| (band.low..band.high).contains(&pc))
}

/// `None` when `pc` is outside every pool's text band. Inside one, the image that owns
/// `pc`, or `None` when no live image does (the text of a dead image).
/// Async-signal-safe.
pub(crate) fn locate(pc: usize) -> Option<Option<Hit>> {
    let band = text(pc)?;
    // SAFETY: `resolve` takes the loader it was registered with.
    Some(unsafe { (band.resolve)(band.loader, pc) })
}

/// Resolve a non-empty static literal `[ptr, ptr + len)` against the data band of the
/// pool that holds it (PACK-9.7). Returns `None` for host/immortal literals. Allocates
/// and admits nothing.
pub(crate) fn locate_literal(ptr: usize, len: usize) -> Option<LiteralOwner> {
    let band = pools()
        .map(|(_, band)| band)
        .find(|band| (band.data_low..band.data_high).contains(&ptr))?;
    // SAFETY: `resolve_literal` takes the loader it was registered with.
    unsafe { (band.resolve_literal)(band.loader, ptr, len) }
}

/// For each of `pcs` that an image owns, its index in `pcs`, its text offset and the
/// two words of its digest prefix, appended to `out` (PACK-9 item 5). Returns the count
/// of frames appended. It allocates nothing when `out` has room.
pub fn frames(pcs: &[u64], out: &mut Vec<u64>) -> usize {
    let before = out.len();
    for (index, pc) in pcs.iter().enumerate() {
        if let Some(Some(hit)) = locate(*pc as usize) {
            let (low, high) = hit.digest.split_at(8);
            out.extend_from_slice(&[
                index as u64,
                u64::from(hit.offset),
                u64::from_le_bytes(low.try_into().unwrap()),
                u64::from_le_bytes(high.try_into().unwrap()),
            ]);
        }
    }
    (out.len() - before) / 4
}

/// A hardware fault in loaded code: the faulting PC and the image that owned it, or no
/// image when the PC was in the text of a dead one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fault {
    pub pc: usize,
    pub hit: Option<Hit>,
}

/// An execution's first fault in loaded code. The fault handler writes it and the
/// reporter reads it after the pool has stopped.
pub(crate) struct Record {
    /// 0 empty, 1 being written, 2 without an image, 3 with one.
    state: AtomicU8,
    pc: AtomicUsize,
    offset: AtomicU64,
    digest: [AtomicU64; 2],
}

impl Record {
    pub(crate) const fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
            pc: AtomicUsize::new(0),
            offset: AtomicU64::new(0),
            digest: [const { AtomicU64::new(0) }; 2],
        }
    }

    /// Keep `fault` if it is the first. Async-signal-safe.
    pub(crate) fn set(&self, fault: Fault) {
        if self
            .state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        self.pc.store(fault.pc, Ordering::Relaxed);
        if let Some(hit) = fault.hit {
            let (low, high) = hit.digest.split_at(8);
            self.offset.store(u64::from(hit.offset), Ordering::Relaxed);
            self.digest[0].store(
                u64::from_le_bytes(low.try_into().unwrap()),
                Ordering::Relaxed,
            );
            self.digest[1].store(
                u64::from_le_bytes(high.try_into().unwrap()),
                Ordering::Relaxed,
            );
        }
        self.state
            .store(2 + u8::from(fault.hit.is_some()), Ordering::Release);
    }

    pub(crate) fn get(&self) -> Option<Fault> {
        let state = loop {
            match self.state.load(Ordering::Acquire) {
                0 => return None,
                1 => std::hint::spin_loop(),
                state => break state,
            }
        };
        let hit = (state == 3).then(|| {
            let mut digest = [0; 16];
            digest[..8].copy_from_slice(&self.digest[0].load(Ordering::Relaxed).to_le_bytes());
            digest[8..].copy_from_slice(&self.digest[1].load(Ordering::Relaxed).to_le_bytes());
            Hit {
                digest,
                offset: self.offset.load(Ordering::Relaxed) as u32,
            }
        });
        Some(Fault {
            pc: self.pc.load(Ordering::Relaxed),
            hit,
        })
    }
}
