//! The coverage raw file (`docs/coverage.md`, "The raw file"). A coverage
//! executable's entry calls [`hirsel_core_coverage_open`] before user code and
//! before any thread starts. When `HIRSEL_COVERAGE` names a path, the file gets
//! a header page and a copy of the pages that hold the `hirsel_cov` section, and
//! those pages are then mapped over the file, shared and fixed, so every flag
//! store lands in the file with no exit hook and no flush. Any failure leaves
//! the section ordinary memory and the run unchanged; a file without the end
//! mark reads as `unobserved`.
//!
//! Only a coverage executable's entry references these functions, so a plain
//! link has no caller of them.
use std::os::unix::ffi::OsStringExt as _;
use std::sync::atomic::{AtomicI32, Ordering};

const MAGIC: &[u8; 8] = b"HIRSELCV";
const VERSION: u32 = 1;
const END_MARK: u32 = 0x4544_4F4E;
const OUTCOME_OFFSET: i64 = 36;
const END_OFFSET: i64 = 48;

/// The open raw file, for the outcome; -1 when this run has none.
static FILE: AtomicI32 = AtomicI32::new(-1);

/// Open the raw file named by `HIRSEL_COVERAGE` over the flag section
/// `start..start + length`, and remove the variable from the environment.
///
/// # Safety
/// The entry calls this once, single-threaded, before user code, with the
/// linker-defined bounds of the `hirsel_cov` section.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_coverage_open(start: *mut u8, length: u64) {
    let path = std::env::var_os("HIRSEL_COVERAGE");
    // SAFETY: the entry calls this at single-threaded process entry.
    unsafe { std::env::remove_var("HIRSEL_COVERAGE") };
    let Some(path) = path else {
        return;
    };
    // Read as given and left in place: the seed is not an effect of the program.
    let seed = std::env::var("HIRSEL_TEST_SIM_SEED")
        .ok()
        .and_then(|seed| seed.parse::<u64>().ok());
    let Ok(path) = std::ffi::CString::new(path.into_vec()) else {
        return;
    };
    // SAFETY: the caller's bounds are the section's, mapped and writable.
    if let Some(fd) = unsafe { open(&path, start, length as usize, seed) } {
        FILE.store(fd, Ordering::Relaxed);
    }
}

/// Record the run's outcome from the exit status the entry returns: 1 when
/// it is 0, else 2. A run that ends another way leaves 0, "not recorded".
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_coverage_outcome(status: u32) {
    let fd = FILE.load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    let outcome: u32 = if status == 0 { 1 } else { 2 };
    // SAFETY: `fd` is the raw file this process opened and never closes.
    unsafe { pwrite_all(fd, &outcome.to_le_bytes(), OUTCOME_OFFSET) };
}

/// The header page: every field but the end mark, which is written last.
fn header(page: usize, section_offset: u64, length: u64, seed: Option<u64>) -> Vec<u8> {
    let mut header = vec![0u8; page];
    header[0..8].copy_from_slice(MAGIC);
    header[8..12].copy_from_slice(&VERSION.to_le_bytes());
    header[12..16].copy_from_slice(&(page as u32).to_le_bytes());
    header[16..24].copy_from_slice(&section_offset.to_le_bytes());
    header[24..32].copy_from_slice(&length.to_le_bytes());
    header[32..36].copy_from_slice(&u32::from(seed.is_some()).to_le_bytes());
    header[40..48].copy_from_slice(&seed.unwrap_or(0).to_le_bytes());
    header
}

/// # Safety
/// `start..start + length` is mapped and writable, and no other thread runs.
unsafe fn open(
    path: &std::ffi::CStr,
    start: *mut u8,
    length: usize,
    seed: Option<u64>,
) -> Option<i32> {
    // SAFETY: sysconf has no preconditions.
    let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
    if page < 64 || !page.is_power_of_two() || length == 0 {
        return None;
    }
    let low = start as usize & !(page - 1);
    let high = (start as usize).checked_add(length)?.next_multiple_of(page);
    let span = high - low;
    // Everything that allocates happens before the pages are copied.
    let header = header(
        page,
        (page + (start as usize - low)) as u64,
        length as u64,
        seed,
    );
    // SAFETY: a valid C path; the flags create a new file only.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_CREAT | libc::O_EXCL | libc::O_RDWR | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return None;
    }
    let fail = || {
        // SAFETY: `fd` is ours and unused elsewhere.
        unsafe { libc::close(fd) };
        None
    };
    // SAFETY: `fd` is an open regular file; `header` is a live buffer.
    if unsafe { libc::ftruncate(fd, (page + span) as libc::off_t) } != 0
        || !unsafe { pwrite_all(fd, &header, 0) }
    {
        return fail();
    }
    // From the copy to the map, nothing but these calls touches memory, so the
    // file holds the pages' bytes as the mapping replaces them.
    // SAFETY: `low..high` covers the caller's mapped, readable section pages.
    let pages = unsafe { std::slice::from_raw_parts(low as *const u8, span) };
    // SAFETY: as above; `fd` is open for writing.
    if !unsafe { pwrite_all(fd, pages, page as i64) } {
        return fail();
    }
    // SAFETY: `low..high` is page-aligned, within this process's writable
    // data, and the file is at least `page + span` bytes long.
    let mapped = unsafe {
        libc::mmap(
            low as *mut libc::c_void,
            span,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED | libc::MAP_FIXED,
            fd,
            page as libc::off_t,
        )
    };
    if mapped == libc::MAP_FAILED {
        return fail();
    }
    // Without the end mark the file reads as `unobserved`; the flags still land.
    // SAFETY: `fd` is open for writing.
    unsafe { pwrite_all(fd, &END_MARK.to_le_bytes(), END_OFFSET) };
    Some(fd)
}

/// Write all of `bytes` at `offset`, retrying short writes and interrupts.
///
/// # Safety
/// `fd` is open for writing.
unsafe fn pwrite_all(fd: i32, mut bytes: &[u8], mut offset: i64) -> bool {
    while !bytes.is_empty() {
        // SAFETY: `bytes` is a live buffer of its length.
        let n = unsafe {
            libc::pwrite(
                fd,
                bytes.as_ptr().cast(),
                bytes.len(),
                offset as libc::off_t,
            )
        };
        if n < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return false;
        }
        if n == 0 {
            return false;
        }
        bytes = &bytes[n as usize..];
        offset += n as i64;
    }
    true
}
