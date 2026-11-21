//! Owner inboxes list the payer roots that have dirty slots, and each root
//! keeps its own dirty-slot list. Links are prepaid in Slot; the domain mutex
//! serializes publication, forwarding and retirement.
//!
//! Routing invariant: every queued slot that is not unassigned is on the list
//! of its current union-find root, and a root's list is nonempty exactly when
//! the root is linked on one inbox, its live inbox. A root that changes inbox
//! (`relink`) or a union (`unite`) therefore moves whole lists in O(1), and
//! never walks another root's slots.
use super::{Domain, Payer, Slot};
use std::{
    cell::{Cell, UnsafeCell},
    collections::HashMap,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

/// A record's dirty-slot list and its link in an inbox's root list. Only
/// Domain.remote reads or writes it, and only on a union-find root's record
/// is it nonempty.
#[derive(Clone, Copy)]
pub(super) struct Queue {
    head: Option<NonNull<Slot>>,
    tail: Option<NonNull<Slot>>,
    /// The inbox whose root list holds this record; Some exactly when head is.
    inbox: Option<NonNull<Inbox>>,
    prev: Option<NonNull<Slot>>,
    next: Option<NonNull<Slot>>,
}
impl Queue {
    pub(super) const EMPTY: Self = Self {
        head: None,
        tail: None,
        inbox: None,
        prev: None,
        next: None,
    };
    pub(super) fn is_empty(&self) -> bool {
        self.head.is_none()
    }
    #[cfg(test)]
    pub(super) fn debug_state(&self) -> (Option<usize>, Option<usize>, Option<u64>, Option<usize>) {
        let id = |p: Option<NonNull<Slot>>| p.map(|p| unsafe { p.as_ref().id });
        (
            id(self.head),
            id(self.tail),
            self.inbox.map(|p| unsafe { p.as_ref().owner }),
            id(self.next),
        )
    }
}

pub(super) struct Inbox {
    owner: u64,
    references: Cell<usize>,
    reaping: Cell<bool>,
    next_reap: Cell<Option<NonNull<Inbox>>>,
    pending: AtomicBool,
    // Only Domain.remote reads or writes the root list.
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
    /// The record's queue. SAFETY: the caller owns the domain mutex (or the
    /// sequencer turn every Remote method runs in), and the record is live.
    #[allow(clippy::mut_from_ref)]
    unsafe fn queue_of<'a>(record: NonNull<Slot>) -> &'a mut Queue {
        unsafe { &mut *record.as_ref().queue.get() }
    }
    /// The live inbox of a union-find root.
    fn live_inbox(root: NonNull<Slot>) -> NonNull<Inbox> {
        // SAFETY: the sequencer owns routing; a queued root is pinned.
        unsafe { (*root.as_ref().links.get()).live.inbox }
    }
    fn link(root: NonNull<Slot>, inbox: NonNull<Inbox>) {
        // SAFETY: domain mutex; the root is unlinked and its list nonempty.
        unsafe {
            let q = Self::queue_of(root);
            debug_assert!(q.inbox.is_none() && q.head.is_some());
            let first = (*inbox.as_ref().head.get()).replace(root);
            if let Some(first) = first {
                Self::queue_of(first).prev = Some(root);
            }
            q.next = first;
            q.prev = None;
            q.inbox = Some(inbox);
            inbox.as_ref().notify();
        }
    }
    fn unlink(root: NonNull<Slot>) -> NonNull<Inbox> {
        // SAFETY: domain mutex; the root is linked on q.inbox.
        unsafe {
            let q = Self::queue_of(root);
            let inbox = q.inbox.take().unwrap();
            match q.prev.take() {
                Some(prev) => Self::queue_of(prev).next = q.next,
                None => *inbox.as_ref().head.get() = q.next,
            }
            if let Some(next) = q.next.take() {
                Self::queue_of(next).prev = q.prev;
            }
            inbox
        }
    }
    /// Queue a clean slot on its root's list, linking the root on its live
    /// inbox if the list was empty.
    fn push(root: NonNull<Slot>, slot: NonNull<Slot>) {
        // SAFETY: domain mutex; each dirty slot has one list link.
        unsafe {
            let q = Self::queue_of(root);
            *slot.as_ref().next_pending.get() = q.head;
            if q.head.replace(slot).is_none() {
                q.tail = Some(slot);
                Self::link(root, Self::live_inbox(root));
            } else {
                q.inbox.unwrap().as_ref().notify();
            }
        }
    }
    pub(super) fn enqueue(&mut self, payer: Payer, bytes: u64, root: Option<Payer>) {
        self.queue(payer, root);
        let slot = payer.slot();
        if bytes != 0 {
            slot.pending.fetch_add(bytes, Ordering::Relaxed);
        } else {
            slot.pending_zeros.fetch_add(1, Ordering::Relaxed);
        }
        Self::notify(payer, root, &self.inboxes);
    }

    pub(super) fn enqueue_handover(&mut self, payer: Payer, unused: u64, root: Payer) {
        self.queue(payer, Some(root));
        payer.slot().handover.set(Some(unused));
        Self::notify(payer, Some(root), &self.inboxes);
    }

    fn queue(&mut self, payer: Payer, root: Option<Payer>) {
        let slot = payer.slot();
        if slot.pending.load(Ordering::Relaxed) == 0
            && slot.handover.get().is_none()
            && slot.pending_zeros.load(Ordering::Relaxed) == 0
        {
            let pointer = NonNull::from(slot);
            self.queued += 1;
            if let Some(root) = root {
                Self::push(root.0, pointer);
            } else {
                // SAFETY: domain mutex owns this slot's unpublished list link.
                unsafe { *slot.next_pending.get() = self.unassigned };
                self.unassigned = Some(pointer);
            }
        }
    }

    fn notify(payer: Payer, root: Option<Payer>, inboxes: &Inboxes) {
        if let Some(root) = root {
            // SAFETY: live limit metadata retains this domain-owned inbox.
            unsafe { Self::live_inbox(root.0).as_ref().notify() };
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
                Self::push(Payer(slot).root().0, slot);
            }
        }
    }
    /// Move a root's list to its live inbox after `task_owner` replaced it.
    /// Only this root's queued slots move; the old inbox keeps every other
    /// root's list and loses its notification once it has none.
    pub(super) fn relink(&mut self, root: NonNull<Slot>, old: NonNull<Inbox>) {
        self.route_unassigned();
        self.follow(root);
        self.vacate(old);
    }
    /// Lower an inbox's notification once it lists no root and no unassigned
    /// entry can still route to it, so its owner sees no stale post.
    fn vacate(&self, inbox: NonNull<Inbox>) {
        // SAFETY: the map retains the inbox until the next reap; domain mutex.
        unsafe {
            if (*inbox.as_ref().head.get()).is_none() && self.unassigned.is_none() {
                inbox.as_ref().clear();
            }
        }
    }
    /// Put a root's nonempty list on its live inbox.
    fn follow(&self, root: NonNull<Slot>) {
        let target = Self::live_inbox(root);
        // SAFETY: domain mutex owns the root's queue.
        let linked = unsafe { Self::queue_of(root).inbox };
        if linked.is_some_and(|inbox| inbox != target) {
            let old = Self::unlink(root);
            Self::link(root, target);
            self.vacate(old);
        }
    }
    /// After a union made `loser` a child of `winner`, give the winner the
    /// loser's queued slots and put the joined list on the winner's live
    /// inbox (a union can replace the winner's live record).
    pub(super) fn unite(&mut self, winner: NonNull<Slot>, loser: NonNull<Slot>) {
        self.follow(winner);
        // SAFETY: domain mutex owns both queues; the loser is no longer a root.
        unsafe {
            let l = Self::queue_of(loser);
            let Some(head) = l.head.take() else {
                return;
            };
            let tail = l.tail.take().unwrap();
            let old = Self::unlink(loser);
            let w = Self::queue_of(winner);
            match w.tail {
                Some(last) => {
                    *last.as_ref().next_pending.get() = Some(head);
                    w.tail = Some(tail);
                    w.inbox.unwrap().as_ref().notify();
                }
                None => {
                    w.head = Some(head);
                    w.tail = Some(tail);
                    Self::link(winner, Self::live_inbox(winner));
                }
            }
            self.vacate(old);
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
        // SAFETY: the domain mutex owns links; clearing the last root also
        // clears its owner's notification, before any foreign publisher can run.
        unsafe {
            let Some(root) = *inbox.head.get() else {
                inbox.clear();
                return None;
            };
            let q = Self::queue_of(root);
            let slot = q.head.unwrap();
            q.head = (*slot.as_ref().next_pending.get()).take();
            if q.head.is_none() {
                q.tail = None;
                Self::unlink(root);
                if (*inbox.head.get()).is_none() {
                    inbox.clear();
                }
            }
            self.queued -= 1;
            Some(slot)
        }
    }
    /// Each inbox's queued slot ids by root, for routing laws.
    #[cfg(test)]
    pub(super) fn routed(&self) -> Vec<(u64, Vec<(usize, Vec<usize>)>)> {
        let mut out: Vec<_> = self
            .inboxes
            .values()
            .map(|inbox| {
                let inbox: &Inbox = inbox;
                let mut roots = vec![];
                // SAFETY: test reads under the domain mutex.
                unsafe {
                    let mut root = *inbox.head.get();
                    while let Some(r) = root {
                        let q = Self::queue_of(r);
                        // The routing invariant: a linked list belongs to a
                        // root on its live inbox, and holds only that root's slots.
                        assert_eq!(q.inbox, Some(NonNull::from(inbox)));
                        assert!((*r.as_ref().links.get()).parent.is_none());
                        assert_eq!(Self::live_inbox(r), NonNull::from(inbox));
                        let mut slots = vec![];
                        let mut slot = q.head;
                        while let Some(s) = slot {
                            assert_eq!(Payer(s).root().0, r);
                            slots.push(s.as_ref().id);
                            slot = *s.as_ref().next_pending.get();
                        }
                        assert_eq!(q.tail.map(|t| t.as_ref().id), slots.last().copied());
                        slots.sort();
                        roots.push((r.as_ref().id, slots));
                        root = q.next;
                    }
                }
                roots.sort();
                (inbox.owner, roots)
            })
            .filter(|(_, roots)| !roots.is_empty())
            .collect();
        let mut unassigned = 0;
        let mut slot = self.unassigned;
        while let Some(s) = slot {
            unassigned += 1;
            // SAFETY: as above.
            slot = unsafe { *s.as_ref().next_pending.get() };
        }
        let listed: usize = out
            .iter()
            .flat_map(|(_, roots)| roots.iter().map(|(_, slots)| slots.len()))
            .sum();
        assert_eq!(listed + unassigned, self.queued);
        out.sort();
        out
    }
    #[cfg(test)]
    pub(super) fn pending_for(&self, owner: u64) -> bool {
        self.inboxes
            .get(&owner)
            .is_some_and(|inbox| inbox.pending())
    }
}
