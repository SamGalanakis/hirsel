//! Allocation records for live frame storage lent to a call. Records live in
//! the same stack slots as the elements and leave the list on spill or drop.
use crate::limit::Payer;
use hirsel_core_layout::FRAME_LIST_LINK_BYTES;
use std::{cell::Cell, ptr::NonNull};

struct Entry {
    next: Cell<Option<NonNull<Entry>>>,
    data: NonNull<u8>,
    payer: Payer,
}

const _: () = assert!(std::mem::size_of::<Entry>() as u64 == FRAME_LIST_LINK_BYTES);

#[derive(Default)]
pub(crate) struct FrameLists {
    first: Cell<Option<NonNull<Entry>>>,
}

impl FrameLists {
    /// # Safety
    /// `data` is a live, word-aligned list frame slot with room for the link.
    /// The caller has admitted the charge against `payer`.
    pub(crate) unsafe fn insert(&self, data: NonNull<u8>, bytes: usize, payer: Payer) {
        // SAFETY: the frame slot holds the payload followed by this aligned link.
        let entry = unsafe { data.add(bytes.next_multiple_of(8)).cast::<Entry>() };
        unsafe {
            entry.as_ptr().write(Entry {
                next: Cell::new(self.first.get()),
                data,
                payer,
            });
        }
        self.first.set(Some(entry));
    }

    /// Remove the allocation record if this carrier owns frame storage.
    pub(crate) fn remove(&self, data: NonNull<u8>) -> Option<Payer> {
        let mut link = &self.first;
        while let Some(entry) = link.get() {
            // SAFETY: the task owns the chain. A registered allocation's frame
            // stays live until this path consumes the allocation.
            let current = unsafe { entry.as_ref() };
            if current.data == data {
                link.set(current.next.get());
                return Some(current.payer);
            }
            link = &current.next;
        }
        None
    }
}
