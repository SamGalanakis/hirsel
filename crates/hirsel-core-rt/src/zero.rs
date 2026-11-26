//! `rt.zero_bytes` (hirsel-core-ir crate docs, "Zero fill"): a large zero fill
//! of list storage that leaves never-backed pages of a dedicated mapping alone.
//!
//! A dedicated mapping is private and anonymous, so a page with no page table
//! entry (neither present nor swapped, per the host's page map,
//! `host::sys::page_map`) reads as zero and stays zero until something writes
//! it. Only the block's owner writes it, and the owner is in this call. Every
//! other page is written as `memset` would. Without a page map (on Darwin, or
//! when it cannot be opened), or outside a dedicated mapping, the whole span is
//! written.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::ptr::NonNull;
use std::sync::OnceLock;

use crate::task_heap::{is_dedicated, page_bytes};

const PRESENT: u64 = 1 << 63;
const SWAPPED: u64 = 1 << 62;
/// Pagemap entries read per `pread`.
const BATCH: usize = 512;

fn pagemap() -> Option<&'static File> {
    static FILE: OnceLock<Option<File>> = OnceLock::new();
    FILE.get_or_init(crate::host::sys::page_map).as_ref()
}

/// Zero `len` bytes at `at`.
///
/// # Safety
/// `at..at + len` is writable and inside the live storage block whose payload
/// starts at `block`, and the caller owns that block.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_zero_bytes(block: *mut u8, at: *mut u8, len: u64) {
    let len = len as usize;
    if len == 0 {
        return;
    }
    let page = page_bytes();
    let start = at.addr();
    let end = start + len;
    let first = start.next_multiple_of(page);
    let last = end & !(page - 1);
    let skippable =
        first < last && NonNull::new(block).is_some_and(is_dedicated) && pagemap().is_some();
    if !skippable {
        // SAFETY: the caller's span is writable.
        unsafe { at.write_bytes(0, len) };
        return;
    }
    // SAFETY: the head and tail partial pages are inside the caller's span.
    unsafe {
        at.write_bytes(0, first - start);
        at.add(last - start).write_bytes(0, end - last);
    }
    let file = pagemap().unwrap();
    let mut entries = [0u64; BATCH];
    let mut next = first;
    while next < last {
        let count = ((last - next) / page).min(BATCH);
        let bytes = count * 8;
        let buffer = unsafe {
            // SAFETY: `entries` is `BATCH` initialized words, viewed as bytes.
            std::slice::from_raw_parts_mut(entries.as_mut_ptr().cast::<u8>(), bytes)
        };
        let offset = (next / page * 8) as u64;
        let read = file.read_exact_at(buffer, offset).is_ok();
        for (i, &entry) in entries[..count].iter().enumerate() {
            let backed = !read || entry & (PRESENT | SWAPPED) != 0;
            if backed {
                // SAFETY: a whole page inside the caller's span.
                unsafe { at.add(next + i * page - start).write_bytes(0, page) };
            }
        }
        next += count * page;
    }
}
