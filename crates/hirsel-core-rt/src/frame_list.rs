//! Allocation records for live frame storage lent to a call. Records live in
//! the same stack slots as the elements and leave the list on spill or drop.
use crate::{Context, limit::Payer};
use hirsel_core_layout::FRAME_LIST_LINK_BYTES;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ptr::NonNull,
};

struct Entry {
    next: Cell<Option<NonNull<Entry>>>,
    data: NonNull<u8>,
    payer: Payer,
}

const _: () = assert!(std::mem::size_of::<Entry>() as u64 == FRAME_LIST_LINK_BYTES);

#[derive(Default)]
pub(crate) struct FrameLists {
    first: Cell<Option<NonNull<Entry>>>,
    // Charges live outside fibers so abandoned frame links need never be walked.
    custody: RefCell<HashMap<usize, (Payer, u64)>>,
}

impl FrameLists {
    /// # Safety
    /// `data` is a live, word-aligned list frame slot with room for the link.
    /// The caller has admitted the charge against `payer`.
    pub(crate) unsafe fn insert(
        &self,
        data: NonNull<u8>,
        bytes: usize,
        payer: Payer,
        charge: Option<u64>,
    ) {
        if let Some(charge) = charge {
            self.custody
                .borrow_mut()
                .insert(data.addr().get(), (payer, charge));
        }
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
    pub(crate) fn remove(&self, data: NonNull<u8>) -> Option<(Payer, Option<u64>)> {
        let (link, entry) = self.find(data)?;
        link.set(entry.next.get());
        let charge = self
            .custody
            .borrow_mut()
            .remove(&data.addr().get())
            .map(|(_, charge)| charge);
        Some((entry.payer, charge))
    }

    pub(crate) fn contains(&self, data: NonNull<u8>) -> bool {
        self.find(data).is_some()
    }

    fn find(&self, data: NonNull<u8>) -> Option<(&Cell<Option<NonNull<Entry>>>, &Entry)> {
        let mut link = &self.first;
        while let Some(entry) = link.get() {
            // SAFETY: the task owns the chain. A registered allocation's frame
            // stays live until this path consumes the allocation.
            let current = unsafe { entry.as_ref() };
            if current.data == data {
                return Some((link, current));
            }
            link = &current.next;
        }
        None
    }
}

impl Drop for FrameLists {
    fn drop(&mut self) {
        for (_, (payer, charge)) in self.custody.get_mut().drain() {
            payer.settle(charge);
        }
    }
}

/// Keep the original charge while moving a frame-backed buffer to the heap.
/// # Safety
/// Storage, size and moved prefix come from a live sequence carrier's layout.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_promote_frame_list(
    context: *const Context,
    data: *mut u8,
    capacity: u64,
    len: u64,
    stride: usize,
) -> *mut u8 {
    let Some(old) = NonNull::new(data) else {
        return data;
    };
    // SAFETY: the caller owns the live context and carrier.
    let task = unsafe { &*context };
    let Some((payer, charge)) = task.frame_lists.remove(old) else {
        return data;
    };
    // A matching record proves both sizes fit the frame slot.
    let allocate = || {
        crate::task_heap::Heap::allocate(
            task,
            payer,
            capacity as usize * stride,
            8,
            crate::block::PLAIN,
            true,
        )
    };
    let replacement = match charge {
        Some(charge) => task.admitted(payer, charge, allocate),
        None => allocate(),
    };
    if let Some(charge) = charge {
        unsafe { crate::task_heap::Page::own_raw(replacement, charge, crate::block::free) };
    }
    unsafe { std::ptr::copy_nonoverlapping(data, replacement.as_ptr(), len as usize * stride) };
    replacement.as_ptr()
}
