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
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

const MAGIC: &[u8; 8] = b"HIRSELCV";
const VERSION: u32 = 1;
const END_MARK: u32 = 0x4544_4F4E;
const OUTCOME_OFFSET: i64 = 36;
const END_OFFSET: i64 = 48;

/// The open raw file, for the outcome; -1 when this run has none.
static FILE: AtomicI32 = AtomicI32::new(-1);
/// [`hirsel_core_coverage_outcome`], installed by an open that succeeded, for
/// the runtime's paths that end the process themselves with status 0 (a
/// sealed replay, the model checker's verdict) instead of returning to the
/// entry. Only [`hirsel_core_coverage_open`] names the outcome function, so a
/// plain link that drops `open` drops it too.
static OUTCOME: AtomicUsize = AtomicUsize::new(0);

/// Record `status` as the outcome before the runtime itself ends the process.
pub(crate) fn exiting(status: i32) {
    let outcome = OUTCOME.load(Ordering::Relaxed);
    if outcome != 0 {
        // SAFETY: only `open` stores, and it stores this function's address.
        let outcome: extern "C" fn(u32) = unsafe { std::mem::transmute(outcome) };
        outcome(status as u32);
    }
}

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
    // Read only beside a raw file: a run without one never looks.
    let warm = std::env::var_os(warm::VARIABLE);
    if warm.is_some() {
        // SAFETY: as above, single-threaded process entry.
        unsafe { std::env::remove_var(warm::VARIABLE) };
    }
    // Read as given and left in place: the seed is not an effect of the program.
    let seed = std::env::var("HIRSEL_TEST_SIM_SEED")
        .ok()
        .and_then(|seed| seed.parse::<u64>().ok());
    let Ok(path) = std::ffi::CString::new(path.into_vec()) else {
        if warm.is_some() {
            warm::refuse(b"the raw file's path is not a C string");
        }
        return;
    };
    // SAFETY: the caller's bounds are the section's, mapped and writable.
    let opened = unsafe { open(&path, start, length as usize, seed) };
    if let Some(warm) = warm {
        let Some(fd) = opened else {
            warm::refuse(b"the raw file did not open");
        };
        // SAFETY: the section is mapped over `fd`, and no thread has started.
        unsafe { warm::serve(fd, start, length as usize, &warm) };
    }
    if let Some(fd) = opened {
        FILE.store(fd, Ordering::Relaxed);
        let outcome: extern "C" fn(u32) = hirsel_core_coverage_outcome;
        OUTCOME.store(outcome as usize, Ordering::Relaxed);
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

/// Warm per-input feedback, a test-only worker for fuzzing a serial law that
/// does not wait, with an immutable input and no authority but its `Env`
/// grant (`tools/decoder-fuzz/native_worker.py`). With `HIRSEL_COVERAGE_WARM=R,W`
/// beside `HIRSEL_COVERAGE`, the entry maps the raw file as usual and then
/// serves cases from descriptor `R`, replying on `W`, instead of running once.
/// Each case sets the named environment variables, clears the flag bytes of
/// the records the driver selected, calls the executable's own `main` again
/// (whose open finds no raw file and returns), and replies with the exit
/// status and those flag bytes. Every call builds a fresh run, so no case
/// state carries over; the flags of other records accumulate as in an
/// ordinary run, and the selected ones are merged back at the end, so the raw
/// file reads as the cumulative coverage of every completed case, with outcome
/// passed only when every case returned 0. A case that stops the process
/// leaves no cumulative: the driver replays its saved case in a fresh child.
///
/// The worker writes `HIRSELWM`, `u32` version 1, `u32` record count, then per
/// record its 32-byte identity, `u32` flag offset in the section and `u32`
/// site count. The driver writes `u32` n and n `u32` record indexes, then
/// requests, each a `u32` tag: 1 a case (`u32` length, then `NAME=VALUE` or
/// `NAME` (unset) entries, each ending in a zero byte), answered by tag 1,
/// `i32` status and the selected flag bytes in order; 2 a sample, answered by
/// tag 2, `u32` count and that many `u64` process counters; 3 or end of input
/// to finish. All integers are little-endian. Any malformed exchange ends the
/// worker with status 70 and a line on standard error.
mod warm {
    pub(super) const VARIABLE: &str = "HIRSEL_COVERAGE_WARM";

    /// End the worker before it serves: the driver sees no greeting.
    pub(super) fn refuse(reason: &[u8]) -> ! {
        let line = [b"hirsel warm coverage: ".as_slice(), reason, b"\n"].concat();
        // SAFETY: a live buffer to the standard error descriptor; `_exit`
        // ends the process without running the program's teardown.
        unsafe {
            libc::write(2, line.as_ptr().cast(), line.len());
            libc::_exit(70)
        }
    }

    use std::ffi::{CString, OsStr, c_char};
    use std::os::unix::ffi::OsStrExt as _;

    const GREETING: &[u8; 8] = b"HIRSELWM";
    const VERSION: u32 = 1;
    const RECORD_MAGIC: u32 = 0x5643_4948;
    const CASE: u32 = 1;
    const SAMPLE: u32 = 2;
    const FINISH: u32 = 3;
    /// A case's environment entries, at most.
    const CASE_LIMIT: usize = 1 << 20;

    type Main = unsafe extern "C" fn(i32, *const *const c_char) -> i32;

    struct Record {
        identity: [u8; 32],
        flags: usize,
        count: usize,
    }

    /// Serve cases until the driver finishes, then end the process.
    ///
    /// # Safety
    /// `start..start + length` is the flag section, mapped over the raw
    /// file `file`; no other thread runs.
    pub(super) unsafe fn serve(file: i32, start: *mut u8, length: usize, descriptors: &OsStr) -> ! {
        let Some((input, output)) = pair(descriptors) else {
            refuse(b"HIRSEL_COVERAGE_WARM is not two descriptors R,W");
        };
        let Some(main) = crate::host::warm::main_entry() else {
            refuse(b"this executable's `main` is not found on this host");
        };
        // SAFETY: the address of this executable's own `main`.
        let main = unsafe { std::mem::transmute::<usize, Main>(main) };
        // SAFETY: the caller's section is mapped and readable.
        let Some(records) = (unsafe { records(start, length) }) else {
            refuse(b"the flag section holds a malformed record");
        };
        let mut greeting = GREETING.to_vec();
        greeting.extend(VERSION.to_le_bytes());
        greeting.extend((records.len() as u32).to_le_bytes());
        for record in &records {
            greeting.extend(record.identity);
            greeting.extend((record.flags as u32).to_le_bytes());
            greeting.extend((record.count as u32).to_le_bytes());
        }
        if !write_all(output, &greeting) {
            refuse(b"the reply descriptor refused the greeting");
        }
        let Some(n) = read_u32(input) else {
            refuse(b"no record selection");
        };
        let mut spans = Vec::new();
        for _ in 0..n {
            match read_u32(input).and_then(|index| records.get(index as usize)) {
                Some(record) => spans.push((record.flags, record.count)),
                None => refuse(b"the selection names no record"),
            }
        }
        let total: usize = spans.iter().map(|&(_, count)| count).sum();
        let mut cumulative = vec![0u8; total];
        let mut reply = Vec::with_capacity(8 + total);
        let mut case = Vec::new();
        let mut held = Vec::new();
        // Outcome 1 while every completed case passed, as `outcome` records.
        let mut outcome = 1u32;
        let argv: [*const c_char; 2] = [c"hirsel-warm".as_ptr(), std::ptr::null()];
        loop {
            match read_u32(input) {
                Some(CASE) => {
                    let Some(len) = read_u32(input).map(|len| len as usize) else {
                        refuse(b"a case without its length");
                    };
                    if len > CASE_LIMIT {
                        refuse(b"a case over the entry limit");
                    }
                    case.resize(len, 0);
                    if !read_exact(input, &mut case) {
                        refuse(b"a case shorter than its length");
                    }
                    // SAFETY: single-threaded between calls (quiescence).
                    if !unsafe { environment(&case, &mut held) } {
                        refuse(b"a case entry is malformed or names a coverage variable");
                    }
                    for &(at, count) in &spans {
                        // SAFETY: a selected record's flag bytes, inside
                        // the mapped section; no run is active.
                        unsafe { std::ptr::write_bytes(start.add(at), 0, count) };
                    }
                    // SAFETY: the executable's own entry; its coverage
                    // open finds no raw file and returns, and the run it
                    // starts ends before it returns.
                    let status = unsafe { main(1, argv.as_ptr()) };
                    if status != 0 {
                        outcome = 2;
                    }
                    reply.clear();
                    reply.extend(CASE.to_le_bytes());
                    reply.extend(status.to_le_bytes());
                    for &(at, count) in &spans {
                        // SAFETY: as above; the run has ended.
                        let flags = unsafe { std::slice::from_raw_parts(start.add(at), count) };
                        reply.extend_from_slice(flags);
                    }
                    for (seen, &flag) in cumulative.iter_mut().zip(&reply[8..]) {
                        *seen |= flag;
                    }
                    if !write_all(output, &reply) {
                        break;
                    }
                }
                Some(SAMPLE) => {
                    let counters = crate::host::warm::counters();
                    reply.clear();
                    reply.extend(SAMPLE.to_le_bytes());
                    reply.extend((counters.len() as u32).to_le_bytes());
                    for counter in counters {
                        reply.extend(counter.to_le_bytes());
                    }
                    if !write_all(output, &reply) {
                        break;
                    }
                }
                Some(FINISH) | None => break,
                Some(_) => refuse(b"an unknown request"),
            }
        }
        // The raw file reads as an ordinary run's: every case's flags.
        let mut at_seen = 0;
        for &(at, count) in &spans {
            for (i, &seen) in cumulative[at_seen..at_seen + count].iter().enumerate() {
                // SAFETY: as above.
                unsafe { *start.add(at + i) |= seen };
            }
            at_seen += count;
        }
        // SAFETY: `file` is the raw file this process opened.
        unsafe { super::pwrite_all(file, &outcome.to_le_bytes(), super::OUTCOME_OFFSET) };
        // SAFETY: ends the process; the flags are already in the file.
        unsafe { libc::_exit(0) }
    }

    fn pair(descriptors: &OsStr) -> Option<(i32, i32)> {
        let text = std::str::from_utf8(descriptors.as_bytes()).ok()?;
        let (input, output) = text.split_once(',')?;
        Some((input.parse().ok()?, output.parse().ok()?))
    }

    /// The records of the flag section, as `docs/coverage.md` lays them out.
    ///
    /// # Safety
    /// `start..start + length` is mapped and readable.
    unsafe fn records(start: *const u8, length: usize) -> Option<Vec<Record>> {
        // SAFETY: the caller's section.
        let section = unsafe { std::slice::from_raw_parts(start, length) };
        let word = |at: usize| u32::from_le_bytes(section[at..at + 4].try_into().unwrap());
        let mut records = Vec::new();
        let mut at = 0;
        while at + 8 <= length {
            let (magic, count) = (word(at), word(at + 4) as usize);
            if magic == 0 && count == 0 {
                at += 8;
                continue;
            }
            if magic != RECORD_MAGIC || at + 40 + count > length {
                return None;
            }
            let identity = section[at + 8..at + 40].try_into().unwrap();
            records.push(Record {
                identity,
                flags: at + 40,
                count,
            });
            at += (40 + count + 7) & !7;
        }
        Some(records)
    }

    /// Set and unset the case's variables. A set entry is `held`'s own
    /// string, placed with `putenv`: `setenv` keeps every distinct value
    /// it was given for the life of the process. The previous case's
    /// entries leave the environment before their strings are dropped.
    ///
    /// # Safety
    /// No other thread runs.
    unsafe fn environment(case: &[u8], held: &mut Vec<CString>) -> bool {
        for entry in held.drain(..) {
            let bytes = entry.as_bytes();
            let name = &bytes[..bytes
                .iter()
                .position(|&byte| byte == b'=')
                .unwrap_or(bytes.len())];
            // SAFETY: the caller's single thread; the entry is still alive.
            unsafe { std::env::remove_var(OsStr::from_bytes(name)) };
        }
        let entries = match case.split_last() {
            None => return true,
            Some((0, entries)) => entries,
            Some(_) => return false,
        };
        for entry in entries.split(|&byte| byte == 0) {
            let name = &entry[..entry
                .iter()
                .position(|&byte| byte == b'=')
                .unwrap_or(entry.len())];
            if name.is_empty() || name.starts_with(b"HIRSEL_COVERAGE") {
                return false;
            }
            // SAFETY: the caller's single thread.
            unsafe { std::env::remove_var(OsStr::from_bytes(name)) };
            if name.len() < entry.len() {
                let Ok(entry) = CString::new(entry) else {
                    return false;
                };
                // SAFETY: the caller's single thread; `held` keeps the
                // string alive until the next case removes it.
                if unsafe { libc::putenv(entry.as_ptr().cast_mut()) } != 0 {
                    return false;
                }
                held.push(entry);
            }
        }
        true
    }

    fn read_u32(fd: i32) -> Option<u32> {
        let mut word = [0u8; 4];
        read_exact(fd, &mut word).then(|| u32::from_le_bytes(word))
    }

    fn read_exact(fd: i32, mut bytes: &mut [u8]) -> bool {
        while !bytes.is_empty() {
            // SAFETY: a live buffer of its length.
            let n = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
            if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            if n <= 0 {
                return false;
            }
            bytes = &mut bytes[n as usize..];
        }
        true
    }

    fn write_all(fd: i32, mut bytes: &[u8]) -> bool {
        while !bytes.is_empty() {
            // SAFETY: a live buffer of its length.
            let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
            if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            if n <= 0 {
                return false;
            }
            bytes = &bytes[n as usize..];
        }
        true
    }
}
