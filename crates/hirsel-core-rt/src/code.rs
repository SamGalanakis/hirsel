//! Loaded code as the runtime sees it (PACK-9).
//!
//! The runtime does not link the loader. A pack host registers its loader's text band
//! and the loader's PC lookup here, once per process. The fault handler and the profile
//! sampler ask this module which image owns a code address.

use std::sync::atomic::{AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering};

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

/// A pack host's text and data bands and its lock-free lookups.
pub struct Band {
    /// The text band is `[low, high)`.
    pub low: usize,
    pub high: usize,
    /// The data band is `[data_low, data_high)`.
    pub data_low: usize,
    pub data_high: usize,
    /// The loader `resolve` and `resolve_literal` are called with.
    pub loader: *const (),
    /// The image whose text holds a code address. It must be async-signal-safe: the
    /// fault handler calls it.
    pub resolve: unsafe fn(*const (), usize) -> Option<Hit>,
    /// The independent data owner for a static literal `[ptr, ptr + len)` inside
    /// `[data_low, data_high)`, retaining one count while pinned. Allocates and
    /// admits nothing.
    pub resolve_literal: unsafe fn(*const (), usize, usize) -> Option<LiteralOwner>,
}

// SAFETY: a registered band is never changed, and `resolve` is async-signal-safe, so it
// is also safe to call from any thread.
unsafe impl Sync for Band {}

static BAND: AtomicPtr<Band> = AtomicPtr::new(std::ptr::null_mut());

/// Why [`register`] refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refused {
    /// This process already has a band.
    Registered,
    /// The fault handler could not be installed for the signals a trap in loaded code
    /// raises.
    Signals(crate::fiber::ArmError),
}

/// Make `band` this process's loaded-code band and take the signals a fault or a trap
/// in loaded code raises (PACK-9).
pub fn register(band: &'static Band) -> Result<(), Refused> {
    crate::fiber::arm_code().map_err(Refused::Signals)?;
    BAND.compare_exchange(
        std::ptr::null_mut(),
        std::ptr::from_ref(band).cast_mut(),
        Ordering::AcqRel,
        Ordering::Acquire,
    )
    .map(drop)
    .map_err(|_| Refused::Registered)
}

/// `None` when `pc` is outside the band. Inside it, the image that owns `pc`, or `None`
/// when no live image does (the text of a dead image). Async-signal-safe.
pub(crate) fn locate(pc: usize) -> Option<Option<Hit>> {
    let band = BAND.load(Ordering::Acquire);
    if band.is_null() {
        return None;
    }
    // SAFETY: a registered band is `'static`.
    let band = unsafe { &*band };
    if pc < band.low || pc >= band.high {
        return None;
    }
    // SAFETY: `resolve` takes the loader it was registered with.
    Some(unsafe { (band.resolve)(band.loader, pc) })
}

/// Resolve a non-empty static literal `[ptr, ptr + len)` against the registered
/// loader's data band (PACK-9.7). Returns `None` for host/immortal literals or
/// when no loader is registered. Allocates and admits nothing.
pub(crate) fn locate_literal(ptr: usize, len: usize) -> Option<LiteralOwner> {
    let band = BAND.load(Ordering::Acquire);
    if band.is_null() {
        return None;
    }
    // SAFETY: a registered band is `'static`.
    let band = unsafe { &*band };
    if ptr < band.data_low || ptr >= band.data_high {
        return None;
    }
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
