//! A returned slot stays live until its custodian collects it. Producers
//! link slots in their page's inbox. The first producer links the page into
//! the heap's notification queue. Neither publication allocates.

use super::{Page, PagePointer};
use std::cell::UnsafeCell;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

const CLOSED: *mut Page = ptr::without_provenance_mut(1);
const _: () = assert!(align_of::<*mut Page>() >= align_of::<AtomicPtr<Page>>());

#[cfg(test)]
thread_local! {
    pub(super) static BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Arrivals after the heap seals go here. Many workers may take batches;
/// only their atomic live-count decrements touch a sealed page.
static ORPHANED: Queue = Queue::new();

pub(super) fn orphaned_pending() -> bool {
    ORPHANED.pending()
}

pub(super) fn purge() {
    // SAFETY: the orphan inbox stays open and serves sealed heaps.
    unsafe { ORPHANED.take().retire() };
}

pub(super) struct Inbox {
    slots: AtomicPtr<u8>,
    next: UnsafeCell<*mut Page>,
}

impl Inbox {
    pub(super) const fn new() -> Self {
        Self {
            slots: AtomicPtr::new(ptr::null_mut()),
            next: UnsafeCell::new(ptr::null_mut()),
        }
    }

    /// # Safety
    /// The caller exclusively owns this cleaned slot, which pins its page.
    /// The heap queue is the one retained by the page.
    #[cold]
    #[inline(never)]
    pub(super) unsafe fn publish(&self, heap: &Queue, page: PagePointer, payload: NonNull<u8>) {
        let address = payload.as_ptr();
        let mut head = self.slots.load(Ordering::Acquire);
        #[cfg(test)]
        if let Some(pause) = BEFORE_PUBLISH.take() {
            pause();
        }
        loop {
            // SAFETY: until the CAS succeeds, only this producer can access
            // the returned slot's link. It never dereferences the previous link.
            unsafe {
                #[cfg(feature = "memcheck")]
                super::allocation_check::link_access(payload, true);
                payload.cast::<*mut u8>().write(head);
                #[cfg(feature = "memcheck")]
                super::allocation_check::link_access(payload, false);
            }
            match self.slots.compare_exchange_weak(
                head,
                address,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(prev) if prev.is_null() => {
                    // SAFETY: the first slot owns publication of the page.
                    // It pins the page until a notification consumer takes it.
                    unsafe { heap.notify(page, self.next.get()) };
                    return;
                }
                Ok(_) => return,
                Err(current) => head = current,
            }
        }
    }
}

pub(super) struct Queue(AtomicPtr<Page>);

impl Queue {
    pub(super) const fn new() -> Self {
        Self(AtomicPtr::new(ptr::null_mut()))
    }

    pub(super) fn pending(&self) -> bool {
        !self.0.load(Ordering::Relaxed).is_null()
    }

    /// # Safety
    /// This producer owns the page's notification link, which is embedded
    /// in the page and pinned by its nonempty slot list.
    unsafe fn notify(&self, page: PagePointer, next: *mut *mut Page) {
        let address = page.as_ptr();
        let mut head = self.0.load(Ordering::Acquire);
        loop {
            if ptr::eq(head, CLOSED) {
                // SAFETY: this acquire sees the custodian's final slot counts.
                // The producer still owns the link and the orphan queue is open.
                unsafe { ORPHANED.notify(page, next) };
                return;
            }
            // SAFETY: the previous consumer reads the link before it releases
            // the empty slot list that grants this producer custody of the link.
            unsafe { next.write(head) };
            match self
                .0
                .compare_exchange_weak(head, address, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(current) => head = current,
            }
        }
    }

    /// # Safety
    /// The queue stays open throughout this call.
    pub(super) unsafe fn take(&self) -> Batch {
        if self.pending() {
            Batch(self.0.swap(ptr::null_mut(), Ordering::Acquire))
        } else {
            Batch(ptr::null_mut())
        }
    }

    /// # Safety
    /// The custodian calls this once after its last plain access to any live count.
    pub(super) unsafe fn close(&self) -> Batch {
        Batch(self.0.swap(CLOSED, Ordering::AcqRel))
    }
}

pub(super) struct Batch(*mut Page);

impl Batch {
    pub(super) fn pop(&mut self) -> Option<(PagePointer, Slots)> {
        // The detached notification pins the descriptor until this consumer
        // takes its outstanding slots.
        let page = NonNull::new(self.0)?;
        // SAFETY: only this consumer can read the page link while its slots
        // list stays nonempty. Clearing the list allows a new notification.
        let slots = unsafe {
            let inbox = &page.as_ref().returned;
            self.0 = inbox.next.get().read();
            inbox.slots.swap(ptr::null_mut(), Ordering::AcqRel)
        };
        Some((page, Slots(slots)))
    }

    /// # Safety
    /// The batch belongs to sealed heaps whose former custodians no longer
    /// access their slot counts or page links.
    pub(super) unsafe fn retire(mut self) {
        while let Some((page, mut slots)) = self.pop() {
            while slots.pop().is_some() {
                // SAFETY: each slot owns one count. The seal precedes atomic
                // access. The final decrement acquires all previous returns.
                unsafe {
                    let live = &raw mut (*page.as_ref().slots.get()).live;
                    if AtomicUsize::from_ptr(live).fetch_sub(1, Ordering::AcqRel) == 1 {
                        Page::destroy(page);
                    }
                }
            }
        }
    }
}

pub(super) struct Slots(*mut u8);

impl Slots {
    pub(super) fn pop(&mut self) -> Option<NonNull<u8>> {
        let payload = NonNull::new(self.0)?;
        // SAFETY: the acquire detach gave this consumer the slot and its link.
        // The original payer may have retired. Only the physical link is read.
        unsafe {
            #[cfg(feature = "memcheck")]
            super::allocation_check::link_access(payload, true);
            self.0 = payload.cast::<*mut u8>().read();
            #[cfg(feature = "memcheck")]
            super::allocation_check::link_access(payload, false);
        }
        Some(payload)
    }
}
