//! Explicit nonmoving chunk regions. Each allocation keeps its own payer and
//! remaining charge, including after partial trim and limit hand-over.

use crate::{Capacity, Context, limit::Funding};
use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::ptr::{self, NonNull};

struct Record {
    funding: Funding,
    charge: u64,
    next: *mut Record,
}
struct Chunk {
    pointer: NonNull<u8>,
    layout: Layout,
    used: usize,
}

pub struct Region {
    chunks: Vec<Chunk>,
    records: *mut Record,
    chunk_bytes: usize,
}

impl Region {
    pub fn new(chunk_bytes: usize) -> Self {
        Self {
            chunks: Vec::new(),
            records: ptr::null_mut(),
            chunk_bytes,
        }
    }
    pub fn allocate(
        &mut self,
        context: &Context,
        bytes: usize,
        align: usize,
        charge: Option<u64>,
    ) -> Result<NonNull<u8>, Capacity> {
        let funding = context.payer(None);
        funding.admit(charge)?;
        let (record, payload) = self.bump(bytes, align);
        // SAFETY: bump returned fresh disjoint aligned storage in an owned chunk.
        unsafe {
            record.write(Record {
                funding,
                charge: charge.unwrap(),
                next: self.records,
            });
            payload
                .as_ptr()
                .sub(size_of::<*mut Record>())
                .cast::<*mut Record>()
                .write(record);
        }
        self.records = record;
        Ok(payload)
    }
    fn bump(&mut self, bytes: usize, align: usize) -> (*mut Record, NonNull<u8>) {
        let align = align.max(align_of::<Record>());
        let prefix = size_of::<Record>() + size_of::<*mut Record>();
        if let Some(chunk) = self.chunks.last_mut() {
            let base = chunk.pointer.as_ptr().addr();
            let start = (chunk.used + align_of::<Record>() - 1) & !(align_of::<Record>() - 1);
            let address = base
                .checked_add(start)
                .and_then(|v| v.checked_add(prefix))
                .and_then(|v| v.checked_add(align - 1))
                .map(|v| v & !(align - 1));
            if let Some(offset) = address.and_then(|v| v.checked_sub(base)) {
                if let Some(end) = offset.checked_add(bytes.max(1)) {
                    if end <= chunk.layout.size() {
                        chunk.used = end;
                        // SAFETY: start and offset are aligned, disjoint from prior
                        // allocations and inside this chunk's allocation.
                        unsafe {
                            return (
                                chunk.pointer.as_ptr().add(start).cast(),
                                NonNull::new_unchecked(chunk.pointer.as_ptr().add(offset)),
                            );
                        }
                    }
                }
            }
        }
        let needed = prefix
            .checked_add(align - 1)
            .and_then(|v| v.checked_add(bytes.max(1)))
            .unwrap_or_else(|| std::process::abort());
        let layout = Layout::from_size_align(self.chunk_bytes.max(needed), align)
            .unwrap_or_else(|_| std::process::abort());
        // SAFETY: the nonzero layout is retained for matching chunk deallocation.
        let pointer =
            unsafe { NonNull::new(alloc(layout)).unwrap_or_else(|| handle_alloc_error(layout)) };
        self.chunks.push(Chunk {
            pointer,
            layout,
            used: 0,
        });
        self.bump(bytes, align)
    }
    /// Lower an allocation's retained logical charge without moving its bytes.
    /// # Safety
    /// Pointer is from a live region allocation; `retained` is no larger than
    /// its remaining charge. The writer proves that the reclaimed tail is unused.
    pub unsafe fn lower_charge(pointer: NonNull<u8>, retained: u64) {
        // SAFETY: the live allocation has a valid backpointer and exclusive writer.
        unsafe {
            let record = &mut *pointer
                .as_ptr()
                .sub(size_of::<*mut Record>())
                .cast::<*mut Record>()
                .read();
            record.funding.refund(record.charge - retained);
            record.charge = retained;
        }
    }
    /// Physical chunk bytes, excluding the region's own bookkeeping vectors.
    pub fn physical_bytes(&self) -> usize {
        self.chunks.iter().map(|c| c.layout.size()).sum()
    }
}
impl Drop for Region {
    fn drop(&mut self) {
        while !self.records.is_null() {
            // SAFETY: every record is initialized once in an owned, still-live
            // chunk; take ownership before bulk chunk deallocation.
            let record = unsafe { self.records.read() };
            self.records = record.next;
            record.funding.refund(record.charge);
        }
        for chunk in self.chunks.drain(..) {
            // SAFETY: all records are gone, no published value outlives the region.
            unsafe {
                dealloc(chunk.pointer.as_ptr(), chunk.layout);
            }
        }
    }
}
