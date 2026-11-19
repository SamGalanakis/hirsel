//! Owner inboxes contain only dirty payer slots. Links are prepaid in Slot;
//! the domain mutex serializes publication, forwarding and retirement.
use super::{Domain, Payer, Slot};
use std::{
    cell::{Cell, UnsafeCell},
    collections::HashMap,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) struct Inbox {
    owner: u64,
    references: Cell<usize>,
    reaping: Cell<bool>,
    next_reap: Cell<Option<NonNull<Inbox>>>,
    pending: AtomicBool,
    // Only Domain.remote reads or writes the dirty list.
    head: UnsafeCell<Option<NonNull<Slot>>>,
}
#[cfg(test)]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct InboxSnapshot {
    owner: u64,
    references: usize,
    reaping: bool,
    pending: bool,
    head: Option<usize>,
    next_reap: Option<u64>,
}
impl Inbox {
    // Only the table lease changes these counts: one per initialized live record
    // (including forwarding records) and one for the domain's bound inbox cache.
    #[cfg(test)]
    pub(super) fn debug_state(&self) -> InboxSnapshot {
        InboxSnapshot {
            owner: self.owner,
            references: self.references.get(),
            reaping: self.reaping.get(),
            pending: self.pending(),
            head: unsafe { (*self.head.get()).map(|p| p.as_ref().id) },
            next_reap: self.next_reap.get().map(|p| unsafe { p.as_ref().owner }),
        }
    }
    pub(super) fn retain(&self) {
        self.references.set(self.references.get().wrapping_add(1));
    }
    pub(super) fn release(&self, domain: &Domain) {
        self.references.set(self.references.get().wrapping_sub(1));
        self.schedule(domain);
    }
    fn schedule(&self, domain: &Domain) {
        if self.references.get() == 0 && !self.reaping.replace(true) {
            self.next_reap
                .set(domain.reap_inboxes.replace(Some(NonNull::from(self))));
        }
    }

    pub(super) fn pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
    pub(super) fn pending_word(&self) -> &AtomicBool {
        &self.pending
    }
    fn notify(&self) {
        self.pending.store(true, Ordering::Release);
    }
    fn clear(&self) {
        self.pending.store(false, Ordering::Release);
    }
}

type Inboxes = HashMap<u64, Box<Inbox>, foldhash::fast::RandomState>;

pub(super) struct Remote {
    pub(super) retired: bool,
    inboxes: Inboxes,
    // A foreign OS thread cannot inspect task-exclusive union records. The
    // next owner event routes this list under the scheduler's table lease.
    unassigned: Option<NonNull<Slot>>,
    queued: usize,
}
impl Remote {
    pub(super) fn new() -> Self {
        Self {
            retired: false,
            inboxes: Inboxes::default(),
            unassigned: None,
            queued: 0,
        }
    }
    pub(super) fn inbox(&mut self, owner: u64) -> NonNull<Inbox> {
        let inbox = self.inboxes.entry(owner).or_insert_with(|| {
            Box::new(Inbox {
                owner,
                references: Cell::new(0),
                reaping: Cell::new(false),
                next_reap: Cell::new(None),
                pending: AtomicBool::new(self.unassigned.is_some()),
                head: UnsafeCell::new(None),
            })
        });
        NonNull::from(&mut **inbox)
    }
    // Called only with both the local table lease and domain mutex. Foreign
    // publishers never inspect the task-local reference counts or reap list.
    pub(super) fn reap(&mut self, domain: &Domain) {
        let mut head = domain.reap_inboxes.take();
        while let Some(inbox) = head {
            // SAFETY: the map retains each scheduled inbox through this lock turn.
            let inbox = unsafe { inbox.as_ref() };
            head = inbox.next_reap.take();
            inbox.reaping.set(false);
            if inbox.references.get() == 0 {
                let owner = inbox.owner;
                self.inboxes.remove(&owner);
            }
        }
    }
    #[cfg(test)]
    pub(super) fn inbox_count(&self) -> usize {
        self.inboxes.len()
    }
    #[cfg(test)]
    pub(super) fn debug_state(&self) -> (bool, usize, Option<usize>, Vec<InboxSnapshot>) {
        let mut inboxes: Vec<_> = self.inboxes.values().map(|v| v.debug_state()).collect();
        inboxes.sort();
        (
            self.retired,
            self.queued,
            unsafe { self.unassigned.map(|p| p.as_ref().id) },
            inboxes,
        )
    }
    pub(super) fn pending(&self) -> bool {
        self.queued != 0
    }
    #[cfg(test)]
    pub(super) fn queued_slots(&self) -> usize {
        self.queued
    }
    fn push(inbox: NonNull<Inbox>, slot: NonNull<Slot>) {
        // SAFETY: caller owns the domain mutex; each dirty slot has one list link.
        unsafe {
            *slot.as_ref().next_pending.get() = *inbox.as_ref().head.get();
            *inbox.as_ref().head.get() = Some(slot);
        }
    }
    pub(super) fn enqueue(&mut self, payer: Payer, bytes: u64, inbox: Option<NonNull<Inbox>>) {
        self.queue(payer, inbox);
        let slot = payer.slot();
        if bytes != 0 {
            slot.pending.fetch_add(bytes, Ordering::Relaxed);
        } else {
            slot.pending_zeros.fetch_add(1, Ordering::Relaxed);
        }
        Self::notify(payer, inbox, &self.inboxes);
    }

    pub(super) fn enqueue_handover(&mut self, payer: Payer, unused: u64, inbox: NonNull<Inbox>) {
        self.queue(payer, Some(inbox));
        payer.slot().handover.set(Some(unused));
        Self::notify(payer, Some(inbox), &self.inboxes);
    }

    fn queue(&mut self, payer: Payer, inbox: Option<NonNull<Inbox>>) {
        let slot = payer.slot();
        if slot.pending.load(Ordering::Relaxed) == 0
            && slot.handover.get().is_none()
            && slot.pending_zeros.load(Ordering::Relaxed) == 0
        {
            let pointer = NonNull::from(slot);
            self.queued += 1;
            if let Some(inbox) = inbox {
                Self::push(inbox, pointer);
            } else {
                // SAFETY: domain mutex owns this slot's unpublished list link.
                unsafe { *slot.next_pending.get() = self.unassigned };
                self.unassigned = Some(pointer);
            }
        }
    }

    fn notify(payer: Payer, inbox: Option<NonNull<Inbox>>, inboxes: &Inboxes) {
        if let Some(inbox) = inbox {
            // SAFETY: live limit metadata retains this domain-owned inbox.
            unsafe { inbox.as_ref().notify() };
        } else {
            for inbox in inboxes.values() {
                inbox.notify();
            }
        }
        payer.domain().pending.store(true, Ordering::Release);
    }
    pub(super) fn route_unassigned(&mut self) {
        let mut head = self.unassigned.take();
        while let Some(slot) = head {
            // SAFETY: queued charges/zero pins retain the slot; the caller has
            // both the table lease and domain mutex for links and canonicalization.
            unsafe {
                head = *slot.as_ref().next_pending.get();
                let payer = Payer(slot).root();
                let inbox = (*payer.links()).live.inbox;
                Self::push(inbox, slot);
                inbox.as_ref().notify();
            }
        }
    }
    pub(super) fn reroute(&mut self, inbox: NonNull<Inbox>) {
        self.route_unassigned();
        // SAFETY: the domain mutex owns this queue. Clear before relinking so
        // entries retaining this owner can be pushed back to the same inbox.
        let mut head = unsafe { (*inbox.as_ref().head.get()).take() };
        // SAFETY: the domain owns the pinned inbox until all charges are gone.
        unsafe { inbox.as_ref().clear() };
        while let Some(slot) = head {
            // SAFETY: each queued node stays pinned until delivery, not routing.
            unsafe {
                head = *slot.as_ref().next_pending.get();
                let payer = Payer(slot).root();
                let target = (*payer.links()).live.inbox;
                Self::push(target, slot);
                target.as_ref().notify();
            }
        }
    }
    pub(super) fn pop(&mut self, owner: Option<u64>) -> Option<NonNull<Slot>> {
        let inbox = match owner {
            Some(owner) => &**self.inboxes.get(&owner)?,
            None => &**self.inboxes.values().find(|inbox| {
                // SAFETY: caller owns the domain mutex; only it mutates queue links.
                unsafe { (*inbox.head.get()).is_some() }
            })?,
        };
        // SAFETY: the domain mutex owns links; clearing the last head also
        // clears its owner's notification, before any foreign publisher can run.
        unsafe {
            let Some(slot) = *inbox.head.get() else {
                inbox.clear();
                return None;
            };
            *inbox.head.get() = (*slot.as_ref().next_pending.get()).take();
            if (*inbox.head.get()).is_none() {
                inbox.clear();
            }
            self.queued -= 1;
            Some(slot)
        }
    }
    #[cfg(test)]
    pub(super) fn pending_for(&self, owner: u64) -> bool {
        self.inboxes
            .get(&owner)
            .is_some_and(|inbox| inbox.pending())
    }
}
