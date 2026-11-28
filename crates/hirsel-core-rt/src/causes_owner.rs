//! Independently owned R/NX cause metadata for loaded releases (PACK-9.7, L2).
//!
//! Each installed module with literal bytes or a source directory gets one
//! [`CauseData`] before publication. Its mutable reference count lives on the
//! Rust heap outside the anonymous `PROT_READ` (R/NX) pages that hold the
//! module's literal bytes, source directory, and self-pointer word. It holds no
//! edge to executable text, any image handle, executable glue, slab cohort, or
//! PC-table row, so RX bytes reclaim immediately even while cause chains survive.

use crate::abi_sites::{SourceFile, SourceTable};
use std::ptr::{self, NonNull};
use std::sync::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering, fence};

#[derive(Clone, Copy)]
struct LiteralRange {
    start: usize,
    end: usize,
    owner: *const CauseData,
}

// SAFETY: `CauseData` is `Send + Sync` and remains live while listed in `LITERAL_RANGES`.
unsafe impl Send for LiteralRange {}
// SAFETY: as above.
unsafe impl Sync for LiteralRange {}

static LIVE_LITERAL_OWNERS: AtomicUsize = AtomicUsize::new(0);
static LITERAL_RANGES: RwLock<Vec<LiteralRange>> = RwLock::new(Vec::new());

fn register_literal_range(ptr: *const u8, len: usize, owner: *const CauseData) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let start = ptr.addr();
    let end = start + len;
    let mut ranges = LITERAL_RANGES.write().unwrap_or_else(|e| e.into_inner());
    let at = ranges.partition_point(|r| r.start < start);
    ranges.insert(at, LiteralRange { start, end, owner });
    LIVE_LITERAL_OWNERS.fetch_add(1, Ordering::Release);
}

fn unregister_literal_range(ptr: *const u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let start = ptr.addr();
    let mut ranges = LITERAL_RANGES.write().unwrap_or_else(|e| e.into_inner());
    if let Ok(at) = ranges.binary_search_by_key(&start, |r| r.start) {
        ranges.remove(at);
        LIVE_LITERAL_OWNERS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cold]
#[inline(never)]
fn find_literal_owner_slow(addr: usize) -> Option<*const CauseData> {
    let ranges = LITERAL_RANGES.read().unwrap_or_else(|e| e.into_inner());
    let idx = ranges.partition_point(|r| r.start <= addr);
    let range = idx.checked_sub(1).map(|i| ranges[i])?;
    (addr < range.end).then_some(range.owner)
}

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
    mapped: *mut u8,
    mapped_len: usize,
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

fn page_size() -> usize {
    // SAFETY: sysconf reads one constant integer.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size > 0 { size as usize } else { 4096 }
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
        let sites_off = (literals_off + literals.len()).next_multiple_of(8);
        let total = sites_off + sites_body.len();
        let mapped_len = total.next_multiple_of(page_size());

        let boxed = Box::new(Self {
            refs: AtomicUsize::new(1),
            mapped: ptr::null_mut(),
            mapped_len: 0,
            slot_addr: 0,
            literals_ptr: ptr::null(),
            literals_len: 0,
            sources: SourceTable {
                files: ptr::null(),
                files_len: 0,
            },
        });
        let raw = Box::into_raw(boxed);

        // SAFETY: anonymous private mapping, populated while RW then locked to PROT_READ.
        let mapped = unsafe {
            libc::mmap(
                ptr::null_mut(),
                mapped_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if mapped == libc::MAP_FAILED {
            // SAFETY: `raw` has not been shared yet.
            unsafe { drop(Box::from_raw(raw)) };
            return None;
        }
        let base = mapped.cast::<u8>();
        // SAFETY: `mapped_len >= total`, all ranges are disjoint and in bounds.
        unsafe {
            ptr::copy_nonoverlapping((raw.addr()).to_le_bytes().as_ptr(), base, 8);
            if !literals.is_empty() {
                ptr::copy_nonoverlapping(literals.as_ptr(), base.add(literals_off), literals.len());
            }
            if !sites_body.is_empty() {
                ptr::copy_nonoverlapping(
                    sites_body.as_ptr(),
                    base.add(sites_off),
                    sites_body.len(),
                );
            }
            if libc::mprotect(mapped, mapped_len, libc::PROT_READ) != 0 {
                libc::munmap(mapped, mapped_len);
                drop(Box::from_raw(raw));
                return None;
            }
            let data = &mut *raw;
            data.mapped = base;
            data.mapped_len = mapped_len;
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
            register_literal_range(data.literals_ptr, data.literals_len, raw);
        }
        // SAFETY: `raw` is non-null and initialized with count 1.
        Some(CauseDataRef(unsafe { NonNull::new_unchecked(raw) }))
    }

    /// Look up the live [`CauseData`] whose independent R/NX literal mapping
    /// contains `ptr`, or `None` for null / host immortal pointers.
    #[inline]
    pub(crate) fn find_literal_owner(ptr: *const u8) -> Option<*const Self> {
        if ptr.is_null() || LIVE_LITERAL_OWNERS.load(Ordering::Acquire) == 0 {
            return None;
        }
        find_literal_owner_slow(ptr.addr())
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
                let boxed = Box::from_raw(ptr.cast_mut());
                unregister_literal_range(boxed.literals_ptr, boxed.literals_len);
                if !boxed.mapped.is_null() && boxed.mapped_len != 0 {
                    libc::munmap(boxed.mapped.cast(), boxed.mapped_len);
                }
                drop(boxed);
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
        self.mapped.addr()
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
pub unsafe extern "C" fn hirsel_core_static_string(ptr: *const u8, len: usize) -> *mut u8 {
    if ptr.is_null() || len == 0 {
        return ptr.cast_mut();
    }
    if let Some(hit) = crate::code::locate_literal(ptr.addr(), len) {
        crate::code::custody::Owner::Literal.acquire(hit.owner);
        return hit.relocated.cast_mut();
    }
    ptr.cast_mut()
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
            meta: (hit.owner.addr() as u64) | 1,
        };
    }
    StaticFrozen {
        data: ptr.cast_mut(),
        meta: hirsel_core_layout::FROZEN_STATIC_UNCHARGED,
    }
}

/// Release one [`CauseData`] count owned by a `cap == 0` byte carrier if `ptr`
/// points into a loaded release's independent R/NX literal mapping.
///
/// # Safety
/// Caller transfers any single [`CauseData`] count associated with `ptr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_release_static_bytes(ptr: *const u8) {
    if let Some(owner) = CauseData::find_literal_owner(ptr) {
        // SAFETY: caller transfers one owned count on `owner`.
        crate::code::custody::Owner::Literal.transfer(owner);
        unsafe { CauseData::release(owner) };
    }
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
