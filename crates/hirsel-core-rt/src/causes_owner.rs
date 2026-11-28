//! Independently owned R/NX cause metadata for loaded releases (PACK-9.7, L2).
//!
//! Each installed module with literal bytes or a source directory gets one
//! [`CauseData`] before publication. Its mutable reference count lives on the
//! Rust heap outside the anonymous `PROT_READ` (R/NX) pages that hold the
//! module's literal bytes, source directory, and self-pointer word. It holds no
//! edge to executable text, any image handle, executable glue, slab cohort, or
//! PC-table row, so RX bytes reclaim immediately even while cause chains survive.

#[path = "causes_owner/chunks.rs"]
mod chunks;

use crate::abi_sites::{SourceFile, SourceTable};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicUsize, Ordering, fence};

/// The host/standalone image's cause data owner: zero means process-lifetime
/// (immortal) backing. Loaded `.hirsel-mod` images bind their import slot for
/// this symbol to their [`CauseData`]'s immutable self-pointer word instead.
#[unsafe(no_mangle)]
pub static hirsel_core_cause_owner: usize = 0;

/// Bit 0 of `Cell::owner` marks a pointer to an [`OwnerPair`] (site first,
/// literal second). Unset bit 0 with a nonzero word is a direct `*const CauseData`.
pub(crate) const PAIR_TAG: usize = 1;

/// Prepaid two-reference owner record for causes whose site and literal come
/// from different origins (site owner first, literal owner second).
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct OwnerPair {
    pub site: *const CauseData,
    pub literal: *const CauseData,
}

impl OwnerPair {
    pub(crate) const EMPTY: Self = Self {
        site: ptr::null(),
        literal: ptr::null(),
    };
}

/// Native owner for one loaded release's R/NX literal storage and source directory.
#[repr(C, align(8))]
pub struct CauseData {
    /// Mutable count outside the `PROT_READ` mapping.
    refs: AtomicUsize,
    /// Anonymous `PROT_READ` (R/NX) mapping: `[self_ptr: 8][literals][pad8][sites_body]`.
    chunk: Option<std::sync::Arc<chunks::Chunk>>,
    /// Address of the immutable 8-byte `self_ptr` word at the start of `mapped`.
    slot_addr: usize,
    /// Base and byte length of copied RO literal bytes inside `mapped`.
    literals_ptr: *const u8,
    literals_len: usize,
    /// Borrowable `SourceTable` pointing directly into `mapped` (`files_len == 0` if none).
    sources: SourceTable,
}

// SAFETY: `refs` is atomic; all other fields and the `PROT_READ` mapping are
// immutable after construction until the final release frees the allocation.
unsafe impl Send for CauseData {}
// SAFETY: as above.
unsafe impl Sync for CauseData {}

/// Safe counted handle to a [`CauseData`].
pub struct CauseDataRef(NonNull<CauseData>);

// SAFETY: `CauseData` is `Send + Sync`.
unsafe impl Send for CauseDataRef {}
// SAFETY: `CauseData` is `Send + Sync`.
unsafe impl Sync for CauseDataRef {}

impl Clone for CauseDataRef {
    fn clone(&self) -> Self {
        // SAFETY: `self` keeps the `CauseData` alive.
        unsafe { CauseData::retain(self.0.as_ptr()) };
        Self(self.0)
    }
}

impl Drop for CauseDataRef {
    fn drop(&mut self) {
        // SAFETY: `self` owns one count on this `CauseData`.
        unsafe { CauseData::release(self.0.as_ptr()) };
    }
}

impl CauseDataRef {
    /// Construct a handle by taking ownership of an already-retained raw pointer.
    ///
    /// # Safety
    /// `ptr` must be a valid [`CauseData`] pointer with at least one owned count
    /// transferred to this handle.
    pub unsafe fn from_raw(ptr: *const CauseData) -> Option<Self> {
        NonNull::new(ptr.cast_mut()).map(Self)
    }

    /// Retain `ptr` if non-null and return a counted handle.
    ///
    /// # Safety
    /// If non-null, `ptr` must point to a live [`CauseData`].
    pub unsafe fn retain_raw(ptr: *const CauseData) -> Option<Self> {
        let non_null = NonNull::new(ptr.cast_mut())?;
        // SAFETY: caller guarantees `ptr` is live.
        unsafe { CauseData::retain(non_null.as_ptr()) };
        Some(Self(non_null))
    }

    pub fn as_ptr(&self) -> *const CauseData {
        self.0.as_ptr()
    }

    pub fn into_raw(self) -> *const CauseData {
        let ptr = self.0.as_ptr();
        std::mem::forget(self);
        ptr
    }

    pub fn slot_addr(&self) -> usize {
        // SAFETY: `self` owns a count on the immutable `CauseData`.
        unsafe { (*self.0.as_ptr()).slot_addr }
    }

    pub fn literals(&self) -> (*const u8, usize) {
        // SAFETY: `self` owns a count on the immutable `CauseData`.
        unsafe {
            let data = &*self.0.as_ptr();
            (data.literals_ptr, data.literals_len)
        }
    }

    pub fn ref_count(&self) -> usize {
        // SAFETY: `self` keeps the `CauseData` live.
        unsafe { CauseData::ref_count(self.0.as_ptr()) }
    }
}

impl CauseData {
    /// Build a new independent R/NX data owner from a module's literal bytes
    /// (the read-only data after import slots) and validated section-8 `sites` bytes.
    /// Returns `None` if both are empty or if `sites` is malformed.
    pub fn new(literals: &[u8], sites_section: &[u8]) -> Option<CauseDataRef> {
        let (file_count, sites_body) =
            hirsel_core_layout::source_positions::validate_source_table(sites_section).ok()?;
        if literals.is_empty() && file_count == 0 {
            return None;
        }
        // Layout inside the anonymous `PROT_READ` mapping:
        //   0..8:                         immutable `self_ptr` word
        //   8..8 + literals.len():        copied literal bytes
        //   sites_off..sites_off + len:   8-byte-aligned `SourceFile` rows + path/position bytes
        let literals_off = 8usize;
        let sites_off = literals_off
            .checked_add(literals.len())?
            .checked_next_multiple_of(8)?;

        let mut boxed = Box::new(Self {
            refs: AtomicUsize::new(1),
            chunk: None,
            slot_addr: 0,
            literals_ptr: ptr::null(),
            literals_len: 0,
            sources: SourceTable {
                files: ptr::null(),
                files_len: 0,
            },
        });
        let raw = &raw mut *boxed;
        let (chunk, base) = chunks::Chunk::append(raw.addr(), literals, sites_body, sites_off)?;
        // SAFETY: the chunk pins this aligned, fully initialized record.
        unsafe {
            let data = &mut *boxed;
            data.chunk = Some(chunk);
            data.slot_addr = base.addr();
            data.literals_ptr = if literals.is_empty() {
                ptr::null()
            } else {
                base.add(literals_off)
            };
            data.literals_len = literals.len();
            data.sources = SourceTable {
                files: if file_count == 0 {
                    ptr::null()
                } else {
                    base.add(sites_off).cast::<SourceFile>()
                },
                files_len: file_count,
            };
        }
        // SAFETY: `raw` is non-null and initialized with count 1.
        Some(CauseDataRef(NonNull::from(Box::leak(boxed))))
    }

    /// Increment the reference count of `ptr`.
    ///
    /// # Safety
    /// `ptr` must point to a live [`CauseData`].
    #[inline]
    pub unsafe fn retain(ptr: *const Self) {
        // SAFETY: caller keeps `ptr` pinned for this increment.
        let prev = unsafe { (*ptr).refs.fetch_add(1, Ordering::Relaxed) };
        if prev == usize::MAX {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"CountOverflow"));
        }
    }

    /// Decrement the reference count of `ptr`, unmapping its R/NX storage and
    /// freeing the header when the last count drops.
    ///
    /// # Safety
    /// Caller transfers one owned count on `ptr`.
    #[inline]
    pub unsafe fn release(ptr: *const Self) {
        // SAFETY: caller owns one count on `ptr`.
        if unsafe { (*ptr).refs.fetch_sub(1, Ordering::Release) } == 1 {
            fence(Ordering::Acquire);
            // SAFETY: this was the final count; no other thread can touch `ptr`.
            unsafe {
                drop(Box::from_raw(ptr.cast_mut()));
            }
        }
    }

    /// Current live reference count (for diagnostics and tests).
    ///
    /// # Safety
    /// `ptr` must point to a live [`CauseData`].
    pub unsafe fn ref_count(ptr: *const Self) -> usize {
        // SAFETY: caller guarantees `ptr` is live.
        unsafe { (*ptr).refs.load(Ordering::Acquire) }
    }

    /// Borrow this owner's `SourceTable`, or null if this release carries no sites.
    #[inline]
    pub fn source_table(&self) -> *const SourceTable {
        if self.sources.files_len == 0 {
            ptr::null()
        } else {
            &raw const self.sources
        }
    }

    /// Base address of the `PROT_READ` mapping (for protection verification in tests).
    pub fn mapped_addr(&self) -> usize {
        self.slot_addr
    }
}

/// Pair `(data, meta)` returned by [`hirsel_core_static_frozen`].
#[repr(C)]
pub struct StaticFrozen {
    pub data: *mut u8,
    pub meta: u64,
}

/// Relocate a loaded image's owned static `String` literal `[ptr, ptr + len)`
/// into its independent R/NX [`CauseData`] mapping and retain one count.
/// Host/immortal pointers pass through unchanged.
///
/// # Safety
/// `ptr` points to `len` valid immutable bytes (or is null when `len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_static_string(ptr: *const u8, len: usize) -> StaticFrozen {
    // SAFETY: the two literal constructors take the same pinned byte range.
    let mut pair = unsafe { hirsel_core_static_frozen(ptr, len) };
    pair.meta = if pair.meta == hirsel_core_layout::FROZEN_STATIC_UNCHARGED {
        0
    } else {
        // CauseData has eight-byte alignment. Compress its address so the tag
        // fits below the capacity niche even for a full-width pointer.
        hirsel_core_layout::STRING_LITERAL_TAG | ((pair.meta & !1) >> 2)
    };
    pair
}

pub(super) fn string_owner(capacity: u64) -> Option<*const CauseData> {
    let tag = hirsel_core_layout::STRING_LITERAL_TAG;
    hirsel_core_layout::is_string_literal_capacity(capacity)
        .then(|| ptr::with_exposed_provenance(((capacity & !tag) << 2) as usize))
}

#[cfg(test)]
#[test]
fn string_owner_tags_exclude_zero_sized_list_capacity() {
    let tag = hirsel_core_layout::STRING_LITERAL_TAG;
    let owner = 0x1238;
    assert_eq!(
        string_owner(tag | (owner >> 2)).map(|p| p.addr()),
        Some(owner as usize)
    );
    for capacity in [0, 1, tag - 1, u64::MAX] {
        assert!(string_owner(capacity).is_none(), "{capacity:#x}");
    }
}

/// Relocate a loaded image's owned static `FrozenBytes` literal `[ptr, ptr + len)`
/// into its independent R/NX [`CauseData`] mapping and return `(relocated, owner | 1)`.
/// Host/immortal pointers return `(ptr, FROZEN_STATIC_UNCHARGED)`.
///
/// # Safety
/// `ptr` points to `len` valid immutable bytes (or is null when `len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_static_frozen(ptr: *const u8, len: usize) -> StaticFrozen {
    if ptr.is_null() || len == 0 {
        return StaticFrozen {
            data: ptr.cast_mut(),
            meta: hirsel_core_layout::FROZEN_STATIC_UNCHARGED,
        };
    }
    if let Some(hit) = crate::code::locate_literal(ptr.addr(), len) {
        crate::code::custody::Owner::Literal.acquire(hit.owner);
        debug_assert_eq!(hit.owner.addr() & 1, 0);
        return StaticFrozen {
            data: hit.relocated.cast_mut(),
            meta: (hit.owner.expose_provenance() as u64) | 1,
        };
    }
    StaticFrozen {
        data: ptr.cast_mut(),
        meta: hirsel_core_layout::FROZEN_STATIC_UNCHARGED,
    }
}

/// Release an owned loaded literal transferred by a String carrier.
///
/// # Safety
/// Any encoded owner must be live and the caller transfers its count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_release_static_bytes(capacity: u64) {
    if let Some(owner) = string_owner(capacity) {
        crate::code::custody::Owner::Literal.transfer(owner);
        unsafe { CauseData::release(owner) };
    }
}

/// Drop the contiguous String leaves of an immutable literal or inline array.
///
/// # Safety
/// Each of the `count` initialized carriers transfers its literal count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_drop_static_strings(carriers: *const u8, count: usize) {
    for index in 0..count {
        let cap = unsafe { carriers.add(index * 24 + 16).cast::<u64>().read_unaligned() };
        unsafe { hirsel_core_release_static_bytes(cap) };
    }
}

/// Retain a shareable native static FrozenBytes carrier.
///
/// # Safety
/// A nonzero owner in the tagged metadata is pinned by the source carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_share_static_frozen(metadata: u64) -> bool {
    if metadata & 1 == 0 {
        return false;
    }
    if metadata != hirsel_core_layout::FROZEN_STATIC_UNCHARGED {
        let owner = ptr::with_exposed_provenance::<CauseData>((metadata & !1) as usize);
        unsafe { CauseData::retain(owner) };
        crate::code::custody::Owner::Literal.acquire(owner);
    }
    true
}

/// Drop one initialized `FrozenBytes` carrier, releasing either its tagged
/// `CauseData` static-literal count or its heap block charge.
///
/// # Safety
/// The caller owns this live carrier exclusively, in a live task context.
pub unsafe fn drop_frozen_bytes(context: *const crate::Context, carrier: *const u8) {
    // SAFETY: all three words are initialized; inline carriers may be unaligned.
    let capacity = unsafe { carrier.add(16).cast::<u64>().read_unaligned() };
    let metadata = unsafe { carrier.add(8).cast::<u64>().read_unaligned() };
    if capacity == 0 {
        return;
    }
    if metadata & 1 != 0 {
        let owner_addr = (metadata & !1) as usize;
        if owner_addr != 0 {
            let owner: *const CauseData = ptr::with_exposed_provenance(owner_addr);
            // SAFETY: non-zero tagged static metadata owns one CauseData count.
            crate::code::custody::Owner::Literal.transfer(owner);
            unsafe { CauseData::release(owner) };
        }
        return;
    }
    // SAFETY: a nonempty heap carrier owns a live, unique block.
    let pointer = unsafe { NonNull::new_unchecked(carrier.cast::<*mut u8>().read_unaligned()) };
    unsafe { crate::block::free_in(&*context, pointer, capacity.next_multiple_of(8)) };
}
