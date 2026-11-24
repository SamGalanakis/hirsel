//! Native test allocator diagnostics. The registry owns no Hirsel allocations
//! and keeps the last allocation site for each retired address.
use crate::task_heap::{Page, release_quarantined};
use std::backtrace::Backtrace;
use std::collections::{HashMap, VecDeque};
use std::io::{self, Write};
use std::ptr::NonNull;
use std::sync::{LazyLock, Mutex};

const MAX_BLOCKS: usize = 4096;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const POISON: u8 = 0xdb;

static STATE: LazyLock<Option<Mutex<State>>> = LazyLock::new(|| {
    if std::env::var_os("HIRSEL_ALLOC").as_deref() != Some(std::ffi::OsStr::new("checked")) {
        return None;
    }
    // SAFETY: the callback and registry have process lifetime.
    if unsafe { libc::atexit(report_leaks) } != 0 {
        eprintln!("checked allocator: could not register leak reporter");
        std::process::abort();
    }
    Some(Mutex::default())
});

#[derive(Default)]
struct State {
    records: HashMap<usize, Record>,
    quarantine: VecDeque<Held>,
    held_bytes: usize,
}

struct Record {
    site: Backtrace,
    live_bytes: Option<usize>,
}

pub(crate) enum Storage {
    Raw,
    Task(NonNull<Page>),
}

struct Held {
    pointer: NonNull<u8>,
    storage: Storage,
    bytes: usize,
}

// SAFETY: each entry owns the storage or the slot's outstanding page count.
// A task slot is returned through the heap's thread-safe inbox.
unsafe impl Send for Held {}

pub(crate) fn enabled() -> bool {
    STATE.is_some()
}

pub(crate) fn allocated(pointer: NonNull<u8>, bytes: usize) {
    if let Some(state) = &*STATE {
        let record = Record {
            site: Backtrace::force_capture(),
            live_bytes: Some(bytes),
        };
        state
            .lock()
            .unwrap()
            .records
            .insert(pointer.addr().get(), record);
    }
}

pub(crate) fn require_live(pointer: NonNull<u8>) {
    if let Some(state) = &*STATE {
        let mut state = state.lock().unwrap();
        state.live(pointer, "access");
    }
}

pub(crate) fn resized(pointer: NonNull<u8>, bytes: usize) {
    if let Some(state) = &*STATE {
        let mut state = state.lock().unwrap();
        state.live(pointer, "resize").live_bytes = Some(bytes);
    }
}

/// # Safety
/// For a live address, the caller transfers exclusive custody of the payload
/// with its original provenance and the matching physical storage.
pub(crate) unsafe fn retire(pointer: NonNull<u8>, storage: Storage) {
    let mut state = STATE.as_ref().expect("checked mode").lock().unwrap();
    let bytes = state.live(pointer, "free").live_bytes.take().unwrap();
    // SAFETY: the live record gives the exact writable payload, and the
    // caller just transferred its exclusive storage to this quarantine.
    unsafe { pointer.as_ptr().write_bytes(POISON, bytes) };
    while state.quarantine.len() >= MAX_BLOCKS
        || state.held_bytes > MAX_BYTES.saturating_sub(bytes)
        || bytes > MAX_BYTES
    {
        let Some(oldest) = state.quarantine.pop_front() else {
            break;
        };
        state.held_bytes -= oldest.bytes;
        oldest.release();
    }
    let held = Held {
        pointer,
        storage,
        bytes,
    };
    if bytes > MAX_BYTES {
        held.release();
    } else {
        state.held_bytes += bytes;
        state.quarantine.push_back(held);
    }
}

impl State {
    fn live(&mut self, pointer: NonNull<u8>, operation: &str) -> &mut Record {
        let address = pointer.addr().get();
        match self.records.get_mut(&address) {
            Some(record) if record.live_bytes.is_some() => record,
            record => {
                let mut stderr = io::stderr().lock();
                let _ = writeln!(
                    stderr,
                    "checked allocator: {operation} of non-live block {address:#x}"
                );
                if let Some(record) = record {
                    let _ = writeln!(stderr, "allocated at:\n{}", record.site);
                } else {
                    let _ = writeln!(stderr, "allocation site unknown");
                }
                std::process::abort();
            }
        }
    }
}

impl Held {
    fn release(self) {
        // SAFETY: the FIFO transferred sole custody of this retired storage.
        unsafe {
            match self.storage {
                Storage::Raw => super::deallocate_untracked(self.pointer),
                Storage::Task(page) => release_quarantined(page, self.pointer),
            }
        }
    }
}

extern "C" fn report_leaks() {
    let Some(state) = &*STATE else {
        return;
    };
    let state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut stderr = io::stderr().lock();
    for (&address, record) in &state.records {
        if let Some(bytes) = record.live_bytes {
            let _ = writeln!(
                stderr,
                "checked allocator: leaked {bytes} bytes at {address:#x}\nallocated at:\n{}",
                record.site
            );
        }
    }
}
