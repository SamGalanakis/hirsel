//! A queued slot remains live until a consumer takes its return. Its payload's
//! first word links the queue, so publishing needs no allocation.

use super::{Page, page_of};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicUsize, Ordering};

const CLOSED: usize = 1;
const _: () = assert!(align_of::<usize>() >= align_of::<AtomicUsize>());

#[cfg(test)]
thread_local! {
    pub(super) static BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Returns that outlive their context. Idle purge or a mapping miss retires
/// them. Open heaps have one consumer, but this queue allows many consumers.
pub(super) static ORPHANED: Queue = Queue::new();

pub(super) fn purge() {
    // SAFETY: this queue stays open and only receives slots after their heap
    // has closed.
    unsafe { ORPHANED.take().retire() };
}

pub(super) struct Queue(AtomicUsize);

impl Queue {
    pub(super) const fn new() -> Self {
        Self(AtomicUsize::new(0))
    }

    pub(super) fn pending(&self) -> bool {
        self.0.load(Ordering::Relaxed) != 0
    }

    /// # Safety
    /// The caller owns the cleaned payload, and its live slot pins this queue.
    #[cold]
    pub(super) unsafe fn publish(&self, payload: NonNull<u8>) {
        let address = payload.as_ptr().expose_provenance();
        let mut head = self.0.load(Ordering::Acquire);
        #[cfg(test)]
        if let Some(pause) = BEFORE_PUBLISH.take() {
            pause();
        }
        loop {
            if head == CLOSED {
                // SAFETY: the acquire sees the context's final writes to
                // the slot counts. The global queue remains open forever.
                unsafe { ORPHANED.publish(payload) };
                return;
            }
            // SAFETY: this payload stays exclusively held by the publisher
            // until a CAS succeeds. The previous address is never dereferenced.
            unsafe {
                #[cfg(feature = "memcheck")]
                super::allocation_check::link_access(payload, true);
                payload.cast::<usize>().write(head);
                #[cfg(feature = "memcheck")]
                super::allocation_check::link_access(payload, false);
            }
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
            Batch(self.0.swap(0, Ordering::Acquire))
        } else {
            Batch(0)
        }
    }

    /// # Safety
    /// The context custodian is the sole consumer and calls this once, after
    /// its last non-atomic access to any live slot count.
    pub(super) unsafe fn close(&self) -> Batch {
        Batch(self.0.swap(CLOSED, Ordering::AcqRel))
    }
}

pub(super) struct Batch(usize);

impl Batch {
    pub(super) fn pop(&mut self) -> Option<NonNull<u8>> {
        // Addresses carry exposed provenance because another producer may
        // see a queue head detach and return at the same address. Every
        // address in the detached batch denotes a currently pinned slot.
        let payload = NonNull::new(ptr::with_exposed_provenance_mut(self.0))?;
        // SAFETY: only this batch can access its payload link. Publishing
        // initialized the link before the acquire that detached the batch.
        unsafe {
            #[cfg(feature = "memcheck")]
            super::allocation_check::link_access(payload, true);
            self.0 = payload.cast::<usize>().read();
            #[cfg(feature = "memcheck")]
            super::allocation_check::link_access(payload, false);
        }
        Some(payload)
    }

    /// # Safety
    /// Every slot belongs to a sealed heap whose custodian has stopped
    /// accessing its counts or page links.
    pub(super) unsafe fn retire(mut self) {
        while let Some(payload) = self.pop() {
            // SAFETY: each batch entry still owns its live count. The seal
            // orders all former plain accesses to live before atomic ones.
            // The last decrement acquires all earlier returns before destroy.
            unsafe {
                let page = page_of(payload);
                let live = &raw mut (*page.as_ref().slots.get()).live;
                if AtomicUsize::from_ptr(live).fetch_sub(1, Ordering::AcqRel) == 1 {
                    Page::destroy(page);
                }
            }
        }
    }
}
