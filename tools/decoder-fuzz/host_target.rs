//! The libFuzzer host of the package archive frame reader. It is a module of
//! `hirsel-core-driver`'s test configuration (`package_content::archive`), so
//! its one byte callback calls the crate-private owner predicate that
//! `reviewed_short_reads` calls. tools/decoder-fuzz/discover.py runs it on one
//! input per process and reads that run's raw file against the build's site map.

use super::short_reads;
use std::ffi::{CString, c_char, c_int, c_void};
use std::fs::File;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::sync::Mutex;

unsafe extern "C" {
    fn LLVMFuzzerRunDriver(
        argc: *mut c_int,
        argv: *mut *mut *mut c_char,
        callback: extern "C" fn(*const u8, usize) -> c_int,
    ) -> c_int;
    // The linker bounds this executable's inline 8-bit counters and PC table.
    static mut __start___sancov_cntrs: u8;
    static __stop___sancov_cntrs: u8;
    static __start___sancov_pcs: [usize; 2];
    static __stop___sancov_pcs: [usize; 2];
}

/// One run's raw file: the build it belongs to, then what the run reached.
struct Raw {
    file: File,
    sites: Vec<u32>,
}

static RAW: Mutex<Option<Raw>> = Mutex::new(None);

fn counters() -> &'static mut [u8] {
    // SAFETY: both symbols bound one section of this executable, which the
    // instrumented code writes one byte at a time and nothing else reads.
    unsafe {
        let start = &raw mut __start___sancov_cntrs;
        let stop = &raw const __stop___sancov_cntrs;
        std::slice::from_raw_parts_mut(start, stop as usize - start as usize)
    }
}

fn pcs() -> &'static [[usize; 2]] {
    // SAFETY: the PC table is immutable after load and holds two words per counter.
    unsafe {
        let start = &raw const __start___sancov_pcs;
        let stop = &raw const __stop___sancov_pcs;
        std::slice::from_raw_parts(start, (stop as usize - start as usize) / 16)
    }
}

/// The executable's load bias and GNU build ID.
fn identity() -> (usize, String) {
    unsafe extern "C" fn first(
        info: *mut libc::dl_phdr_info,
        _: usize,
        data: *mut c_void,
    ) -> c_int {
        // SAFETY: the loader passes a valid record; `data` is the caller's pair.
        unsafe {
            let info = &*info;
            let found = &mut *data.cast::<(usize, String)>();
            found.0 = info.dlpi_addr as usize;
            for header in std::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize) {
                if header.p_type != libc::PT_NOTE {
                    continue;
                }
                let notes = std::slice::from_raw_parts(
                    (found.0 + header.p_vaddr as usize) as *const u8,
                    header.p_memsz as usize,
                );
                let mut at = 0;
                while at + 12 <= notes.len() {
                    let word = |at: usize| {
                        u32::from_ne_bytes(notes[at..at + 4].try_into().unwrap()) as usize
                    };
                    let (name, description, kind) = (word(at), word(at + 4), word(at + 8));
                    let start = at + 12 + name.next_multiple_of(4);
                    if start + description > notes.len() {
                        break;
                    }
                    if kind == 3 && &notes[at + 12..at + 12 + name] == b"GNU\0" {
                        found.1 = notes[start..start + description]
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect();
                    }
                    at = start + description.next_multiple_of(4);
                }
            }
        }
        // The first record is the executable. Stop there.
        1
    }
    let mut found = (0, String::new());
    // SAFETY: the callback only reads the loader's records during this call.
    unsafe { libc::dl_iterate_phdr(Some(first), (&raw mut found).cast()) };
    assert!(!found.1.is_empty(), "executable has no GNU build ID");
    found
}

fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Finish the raw file: the observation or failure, each listed site's flag, the end mark.
fn finish(outcome: &str) {
    let Some(mut raw) = RAW
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    else {
        return;
    };
    let counters = counters();
    let flags: String = raw
        .sites
        .iter()
        .map(|&site| {
            if counters[site as usize] == 0 {
                '0'
            } else {
                '1'
            }
        })
        .collect();
    let line = outcome.replace('\n', " ");
    // A failed write leaves the file without its end mark, which reads as unobserved.
    let _ = write!(raw.file, "{line}\nflags {flags}\nend\n").and_then(|()| raw.file.sync_data());
}

extern "C" fn test_one_input(data: *const u8, size: usize) -> c_int {
    // SAFETY: libFuzzer passes `size` readable bytes, or a null pointer with size 0.
    let input = if size == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(data, size) }
    };
    if let Some(path) = variable("HIRSEL_DECODER_RAW") {
        let listed =
            std::fs::read(variable("HIRSEL_DECODER_SITES").expect("HIRSEL_DECODER_SITES")).unwrap();
        let sites = listed
            .chunks_exact(4)
            .map(|site| u32::from_le_bytes(site.try_into().unwrap()))
            .collect();
        // The run owns its file. An existing one belongs to another run and is left alone.
        if let Ok(mut file) = File::create_new(path) {
            let (_, build) = identity();
            if writeln!(
                file,
                "hirsel-decoder-raw 1 {build} {}\nentered",
                listed.len() / 4
            )
            .is_ok()
            {
                *RAW.lock().unwrap() = Some(Raw { file, sites });
            }
        }
        counters().fill(0);
    }
    let max_read = variable("HIRSEL_DECODER_MAX_READ").map_or(3, |value| value.parse().unwrap());
    let expect = variable("HIRSEL_DECODER_EXPECT");
    let observation = short_reads::discovered(input, max_read, expect.as_deref());
    finish(&format!("observed {observation}"));
    0
}

/// `HIRSEL_DECODER_DESCRIBE=FILE` writes the build ID and each counter's link
/// address for the site map. `HIRSEL_DECODER_FUZZ_ARGS` holds libFuzzer's
/// arguments, one per line. Without either this test does nothing.
#[test]
fn libfuzzer() {
    if let Some(path) = variable("HIRSEL_DECODER_DESCRIBE") {
        let (bias, build) = identity();
        let mut out = std::io::BufWriter::new(File::create_new(path).unwrap());
        writeln!(out, "hirsel-decoder-pcs 1 {build} {}", pcs().len()).unwrap();
        assert_eq!(pcs().len(), counters().len());
        for [pc, _] in pcs() {
            // The entry of a function the link dropped is null.
            out.write_all(&(pc.saturating_sub(bias) as u64).to_le_bytes())
                .unwrap();
        }
        out.flush().unwrap();
        return;
    }
    let Some(arguments) = std::env::var_os("HIRSEL_DECODER_FUZZ_ARGS") else {
        return;
    };
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // A panic in the callback aborts, which libFuzzer reports as a crash.
        // Record the assertion and its site first.
        let message = info.payload_as_str().unwrap_or("panic");
        let site = info
            .location()
            .map(|site| format!("{}:{}", site.file(), site.line()));
        finish(&format!(
            "failed {} {message}",
            site.as_deref().unwrap_or("unknown")
        ));
        default_hook(info);
    }));
    let mut owned: Vec<CString> = vec![c"decoder-fuzz-host".into()];
    owned.extend(
        arguments
            .as_bytes()
            .split(|&byte| byte == b'\n')
            .map(|argument| CString::new(argument).unwrap()),
    );
    let mut argv: Vec<*mut c_char> = owned
        .iter()
        .map(|argument| argument.as_ptr().cast_mut())
        .collect();
    argv.push(std::ptr::null_mut());
    let mut argc = (argv.len() - 1) as c_int;
    let mut argv = argv.as_mut_ptr();
    // SAFETY: argv outlives the call and libFuzzer only reorders its entries.
    let status = unsafe { LLVMFuzzerRunDriver(&raw mut argc, &raw mut argv, test_one_input) };
    std::process::exit(status);
}
