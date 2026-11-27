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
        }
        // SAFETY: `raw` is non-null and initialized with count 1.
        Some(CauseDataRef(unsafe { NonNull::new_unchecked(raw) }))
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
