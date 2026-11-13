//! Nonmoving bump chunks, with one charge accumulator per payer run.
use crate::allocator::{allocate as native_allocate, deallocate as dealloc};
use crate::{Capacity, Context, limit::Payer};
use std::alloc::Layout;
use std::ptr::{self, NonNull};

#[derive(Clone, Copy)]
struct Entry {
    payer: Payer,
    charge: u64,
}
struct Chunk {
    pointer: NonNull<u8>,
    layout: Layout,
}

/// Cursor/end and last-entry index are published for emitter inlining. A builder
/// remembers its entry index and its own retained charge, never an object header.
#[repr(C)]
pub struct Region {
    pub(crate) cursor: *mut u8,
    pub(crate) end: *mut u8,
    pub(crate) entry: usize,
    pub(crate) payer: *mut crate::limit::LimitRecord,
    pub(crate) charge: u64,
    inline: [Option<Entry>; 4],
    extra: Vec<Entry>,
    chunks: Vec<Chunk>,
    chunk_bytes: usize,
}
impl Region {
    pub fn new(chunk_bytes: usize) -> Self {
        Self {
            cursor: ptr::null_mut(),
            end: ptr::null_mut(),
            entry: usize::MAX,
            payer: ptr::null_mut(),
            charge: 0,
            inline: [None; 4],
            extra: Vec::new(),
            chunks: Vec::new(),
            chunk_bytes,
        }
    }
    #[inline(always)]
    fn entry_mut(&mut self, index: usize) -> &mut Entry {
        if index < 4 {
            self.inline[index].as_mut().unwrap()
        } else {
            &mut self.extra[index - 4]
        }
    }
    #[inline(always)]
    fn charge(&mut self, payer: Payer, charge: u64) {
        if self.charge != 0 && self.payer == payer.record_pointer() {
            self.charge = self.charge.wrapping_add(charge);
        } else {
            self.change_payer(payer, charge);
        }
    }
    #[cold]
    fn change_payer(&mut self, payer: Payer, charge: u64) {
        if !self.payer.is_null() {
            let last = Entry {
                payer: Payer::from_current(self.payer),
                charge: self.charge,
            };
            if self.entry < 4 {
                self.inline[self.entry] = Some(last);
            } else {
                self.extra.push(last);
            }
        }
        self.entry = self.entry.wrapping_add(1);
        self.payer = payer.record_pointer();
        self.charge = charge;
    }
    #[inline]
    pub fn allocate(
        &mut self,
        context: &Context,
        bytes: usize,
        align: usize,
        charge: Option<u64>,
    ) -> Result<NonNull<u8>, Capacity> {
        let payer = context.payer(None);
        payer.admit(charge)?;
        if charge != Some(0) {
            self.charge(payer, charge.unwrap());
        }
        let start = self.cursor.addr().wrapping_add(align - 1) & !(align - 1);
        let end = start.checked_add(bytes.max(1));
        if let Some(end) = end
            && end <= self.end.addr()
        {
            let pointer = self.cursor.with_addr(start);
            self.cursor = self.cursor.with_addr(end);
            // SAFETY: aligned disjoint range inside the current owned chunk.
            return Ok(unsafe { NonNull::new_unchecked(pointer) });
        }
        Ok(self.refill(bytes, align))
    }
    #[cold]
    pub(crate) fn refill(&mut self, bytes: usize, align: usize) -> NonNull<u8> {
        let layout = Layout::from_size_align(self.chunk_bytes.max(bytes.max(1)), align)
            .unwrap_or_else(|_| std::process::abort());
        // SAFETY: nonzero layout is retained for matching deallocation.
        let pointer = native_allocate(layout);
        self.chunks.push(Chunk { pointer, layout });
        self.cursor = pointer.as_ptr().wrapping_add(bytes.max(1));
        self.end = pointer.as_ptr().wrapping_add(layout.size());
        pointer
    }
    /// Refund only this builder's tail through its original payer entry.
    /// # Safety
    /// The live builder owns `previous` bytes in `entry`, and retains at most
    /// that amount. No other builder lowers this reservation.
    pub unsafe fn lower_charge(&mut self, entry: usize, previous: u64, retained: u64) {
        let refund = previous - retained;
        if refund == 0 {
            return;
        }
        if entry == self.entry {
            Payer::from_current(self.payer).refund(refund);
            self.charge = self.charge.wrapping_sub(refund);
        } else {
            let record = self.entry_mut(entry);
            record.payer.refund(refund);
            record.charge = record.charge.wrapping_sub(refund);
        }
    }
    pub fn physical_bytes(&self) -> usize {
        self.chunks.iter().map(|c| c.layout.size()).sum()
    }
}
impl Drop for Region {
    fn drop(&mut self) {
        if self.charge != 0 {
            Payer::from_current(self.payer).refund(self.charge);
        }
        for entry in self.inline.iter().flatten().chain(self.extra.iter()) {
            entry.payer.refund(entry.charge);
        }
        for chunk in self.chunks.drain(..) {
            // SAFETY: no stable locator outlives region close.
            unsafe {
                dealloc(chunk.pointer.as_ptr(), chunk.layout);
            }
        }
    }
}
