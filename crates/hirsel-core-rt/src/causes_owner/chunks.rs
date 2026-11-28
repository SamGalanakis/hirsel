//! Append immutable, eight-byte-aligned records to a read-only file mapping.
//! Only the unissued tail can be written. The allocator keeps a weak reference
//! so releasing the last record closes the file and unmaps its chunk.
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::ptr::{self, NonNull};
use std::sync::{Arc, Mutex, Weak};

const CHUNK_BYTES: usize = 64 * 1024;
static TAIL: Mutex<(Weak<Chunk>, usize)> = Mutex::new((Weak::new(), 0));

pub(super) struct Chunk {
    file: File,
    base: NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping has no writable view. Reservations are disjoint, and a
// record is exposed only after all writes to that reservation have completed.
unsafe impl Send for Chunk {}
unsafe impl Sync for Chunk {}

impl Drop for Chunk {
    fn drop(&mut self) {
        // SAFETY: the last Arc owns the complete mapping.
        unsafe { libc::munmap(self.base.as_ptr().cast(), self.len) };
    }
}

impl Chunk {
    fn new(len: usize) -> Option<Arc<Self>> {
        let file = crate::host::literal_file().ok()?;
        file.set_len(len as u64).ok()?;
        // SAFETY: len is nonzero, the backing file has that extent, and the
        // file is never truncated. MAP_SHARED makes positional writes visible.
        let mapped = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if mapped == libc::MAP_FAILED {
            return None;
        }
        Some(Arc::new(Self {
            file,
            base: NonNull::new(mapped.cast()).expect("non-null mmap address"),
            len,
        }))
    }

    fn reserve(bytes: usize) -> Option<(Arc<Self>, usize)> {
        let bytes = bytes.checked_next_multiple_of(8)?;
        if bytes > CHUNK_BYTES {
            return Some((Self::new(bytes)?, 0));
        }
        let mut tail = TAIL.lock().unwrap_or_else(|e| e.into_inner());
        if tail.1 + bytes <= CHUNK_BYTES
            && let Some(chunk) = tail.0.upgrade()
        {
            let offset = tail.1;
            tail.1 += bytes;
            return Some((chunk, offset));
        }
        let chunk = Self::new(CHUNK_BYTES)?;
        *tail = (Arc::downgrade(&chunk), bytes);
        Some((chunk, 0))
    }

    pub(super) fn append(
        owner: usize,
        literals: &[u8],
        sites: &[u8],
        sites_off: usize,
    ) -> Option<(Arc<Self>, *mut u8)> {
        let (chunk, offset) = Self::reserve(sites_off.checked_add(sites.len())?)?;
        for (at, bytes) in [
            (0, &owner.to_le_bytes()[..]),
            (8, literals),
            (sites_off, sites),
        ] {
            chunk.file.write_all_at(bytes, (offset + at) as u64).ok()?;
        }
        // SAFETY: this reservation lies inside the mapped file and is now immutable.
        let base = unsafe { chunk.base.as_ptr().add(offset) };
        Some((chunk, base))
    }
}
