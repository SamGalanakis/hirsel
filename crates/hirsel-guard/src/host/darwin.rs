//! Darwin ignores `RLIMIT_DATA`, so the limit lives in the allocator
//! (research-1124/macos-arm64 §6.6). `mimalloc`'s `override` makes it the
//! process's malloc zone, serving Rust and C++ (LLVM) alike. Before the first
//! allocation the guard reserves one arena of the limit and forbids `mimalloc`
//! any further OS memory, so an allocation past the limit returns null at its
//! call, as the data limit makes it on Linux, and the same refusal path ends
//! the process. No thread watches the footprint.
//!
//! `mimalloc`'s zone is version 10 and leaves `reinit_lock` null, the entry
//! libmalloc calls for every zone of version 9 or later in a forked child. The
//! guard fills it before the first allocation, or every `fork` would end the
//! child with SIGSEGV before `exec`.
//!
//! What the arena does not hold is not limited: a foreign library's direct
//! `mmap`, and thread stacks. Children inherit no kernel limit; each Hirsel
//! binary installs the guard and reads the `HIRSEL_MEMORY_LIMIT` it inherited.
//! Large blocks use the arena unevenly: measured on macOS 26.6.2 under a
//! 256 MiB limit, 4 KiB blocks reach 255 MiB, 4 MiB blocks 224 MiB and 16 MiB
//! blocks 128 MiB.
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// No kernel limit: children read the variable, and thread stacks bypass the
/// arena.
#[cfg(test)]
pub(crate) const KERNEL_LIMIT: bool = false;

/// `mimalloc`'s `mi_option_arena_reserve` (KiB reserved per new arena) and
/// `mi_option_disallow_os_alloc` (v3 `mimalloc.h`).
const ARENA_RESERVE: libc::c_int = 23;
const DISALLOW_OS_ALLOC: libc::c_int = 17;

// libmimalloc-sys's `override` links mimalloc's C library into the binary
// (naming the crate keeps it in the link); these are in its public API.
use libmimalloc_sys as _;
unsafe extern "C" {
    fn mi_option_set(option: libc::c_int, value: libc::c_long);
    fn mi_reserve_os_memory(size: usize, commit: bool, allow_large: bool) -> libc::c_int;
}

/// libmalloc's zone record up to its version, and its introspection table up
/// to the entry a forked child calls (`<malloc/malloc.h>`, arm64 macOS 26).
#[repr(C)]
struct Zone {
    /// Two reserved words, then size, malloc, calloc, valloc, free, realloc
    /// and destroy.
    calls: [usize; 9],
    name: *const libc::c_char,
    /// batch_malloc and batch_free.
    batch: [usize; 2],
    introspect: *mut Introspection,
    version: libc::c_uint,
}

#[repr(C)]
struct Introspection {
    /// The thirteen entries from enumerator to enumerate_discharged_pointers.
    earlier: [usize; 13],
    reinit_lock: Option<unsafe extern "C" fn(*mut Zone)>,
}

unsafe extern "C" {
    /// With task 0 and no reader, the process's own zone list, without
    /// allocating.
    fn malloc_get_all_zones(
        task: libc::c_uint,
        reader: *const libc::c_void,
        zones: *mut *mut *mut Zone,
        count: *mut libc::c_uint,
    ) -> libc::c_int;
}

/// `mimalloc` holds no zone lock: its `force_lock` and `force_unlock` do
/// nothing, and so does this.
unsafe extern "C" fn reinit_lock(_zone: *mut Zone) {}

/// Give `mimalloc`'s zone the `reinit_lock` entry a forked child calls.
fn survive_fork() {
    let mut zones = std::ptr::null_mut();
    let mut count = 0;
    // SAFETY: both outputs are live; the call reads the process's own list.
    if unsafe { malloc_get_all_zones(0, std::ptr::null(), &mut zones, &mut count) } != 0 {
        return;
    }
    for index in 0..count as usize {
        // SAFETY: the list holds `count` registered zones. Each record has a
        // name or null and, from version 9, an introspection table that
        // reaches `reinit_lock`. `mimalloc`'s table is its own writable data.
        unsafe {
            let zone = *zones.add(index);
            if !(*zone).name.is_null()
                && std::ffi::CStr::from_ptr((*zone).name) == c"mimalloc"
                && (*zone).version >= 9
                && (*(*zone).introspect).reinit_lock.is_none()
            {
                (*(*zone).introspect).reinit_lock = Some(reinit_lock);
            }
        }
    }
}

static LIMIT: AtomicU64 = AtomicU64::new(0);

/// Hold the process to `requested` bytes of `mimalloc` memory. A reservation
/// the host refuses leaves no arena, so the first allocation reports the limit.
pub(crate) fn set_limit(requested: u64) {
    survive_fork();
    // SAFETY: option setting and the reservation are mimalloc's documented
    // start-up calls; none of them allocates through the guard.
    unsafe {
        mi_option_set(ARENA_RESERVE, 0);
        mi_reserve_os_memory(
            usize::try_from(requested).unwrap_or(usize::MAX),
            false,
            false,
        );
        mi_option_set(DISALLOW_OS_ALLOC, 1);
    }
    LIMIT.store(requested, Ordering::Release);
}

/// The limit in force, or `None` before the guard set one.
pub(crate) fn limit() -> Option<u64> {
    Some(LIMIT.load(Ordering::Acquire)).filter(|&limit| limit != 0)
}

/// Nothing to lift: a child holds no limit it did not set itself.
pub(crate) fn lift_for_sanitizer(_command: &mut Command) {}

/// Thread stacks lie outside the arena, so the limit never refuses a thread.
pub(crate) fn data_used() -> Option<u64> {
    None
}

/// The process's resident peak: Darwin's `ru_maxrss` is in bytes.
pub(crate) fn resident_peak_kib() -> Option<u64> {
    // SAFETY: an all-zero rusage is a valid output record; getrusage writes one.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return None;
    }
    Some(u64::try_from(usage.ru_maxrss).ok()? >> 10)
}
