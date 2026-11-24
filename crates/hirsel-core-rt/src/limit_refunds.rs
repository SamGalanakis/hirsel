//! Owner inboxes list the payer roots that have dirty slots, and each root
//! keeps its own dirty-slot list. Links are prepaid in Slot. Only the domain's
//! own Unit touches Remote (one worker runs a Unit at a time); another Unit's
//! refunds arrive as `Post`s on the domain's mailbox, which that Unit folds in.
//!
//! Routing invariant: every queued slot that is not unassigned is on the list
//! of its current union-find root, and a root's list is nonempty exactly when
//! the root is linked on one inbox, its live inbox. A root that changes inbox
//! (`relink`) or a union (`unite`) therefore moves whole lists in O(1), and
//! never walks another root's slots.
use super::{Domain, Funding, Payer, Slot};
use std::{
    cell::{Cell, UnsafeCell},
    collections::HashMap,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

/// A record's dirty-slot list and its link in an inbox's root list. Only
/// the owner's Remote reads or writes it, and only on a union-find root's record
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
    // Only the owner's Remote reads or writes the root list.
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
    fn notify(&self) {
        self.pending.store(true, Ordering::Release);
    }
    fn clear(&self) {
        self.pending.store(false, Ordering::Release);
    }
}

type Inboxes = HashMap<u64, Box<Inbox>, foldhash::fast::RandomState>;

pub(super) struct Remote {
    inboxes: Inboxes,
    // Folded mailbox posts wait here, unrouted, until the next owner event
    // routes them under the scheduler's table lease.
    unassigned: Option<NonNull<Slot>>,
    queued: usize,
    #[cfg(feature = "record")]
    retirements: HashMap<NonNull<Slot>, Vec<crate::tape::Retirement>>,
}
impl Remote {
    pub(super) fn new() -> Self {
        Self {
            inboxes: Inboxes::default(),
            unassigned: None,
            queued: 0,
            #[cfg(feature = "record")]
            retirements: HashMap::default(),
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
    // Called only by the owner, with the local table lease. Mailbox posters
    // never inspect the task-local reference counts or reap list.
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
    pub(super) fn debug_state(&self) -> (usize, Option<usize>, Vec<InboxSnapshot>) {
        let mut inboxes: Vec<_> = self.inboxes.values().map(|v| v.debug_state()).collect();
        inboxes.sort();
        (
            self.queued,
            unsafe { self.unassigned.map(|p| p.as_ref().id) },
            inboxes,
        )
    }
    pub(super) fn pending(&self) -> bool {
        self.queued != 0
    }
    /// The record's queue, under the same single-consumer invariant as
    /// `Domain::remote`: only the owner Unit, or `Funding::drop` under `retire`
    /// once `posters` is zero, dereferences it. Each site takes one `&mut` for
    /// a scope that ends before any other `&mut` to the same queue begins,
    /// including the ones `link` and `unlink` take.
    /// # Safety
    /// The record is live.
    unsafe fn queue_of(record: NonNull<Slot>) -> *mut Queue {
        unsafe { record.as_ref().queue.get() }
    }
    /// The live inbox of a union-find root.
    fn live_inbox(root: NonNull<Slot>) -> NonNull<Inbox> {
        // SAFETY: the owner Unit owns routing; a queued root is pinned.
        unsafe { (*root.as_ref().links.get()).live.inbox }
    }
    fn link(root: NonNull<Slot>, inbox: NonNull<Inbox>) {
        // SAFETY: owner Unit; the root is unlinked and its list nonempty.
        unsafe {
            debug_assert!({
                let q = &*Self::queue_of(root);
                q.inbox.is_none() && q.head.is_some()
            });
            let first = (*inbox.as_ref().head.get()).replace(root);
            if let Some(first) = first {
                (*Self::queue_of(first)).prev = Some(root);
            }
            let q = &mut *Self::queue_of(root);
            q.next = first;
            q.prev = None;
            q.inbox = Some(inbox);
            inbox.as_ref().notify();
        }
    }
    fn unlink(root: NonNull<Slot>) -> NonNull<Inbox> {
        // SAFETY: owner Unit; the root is linked on q.inbox.
        unsafe {
            let (inbox, prev, next) = {
                let q = &mut *Self::queue_of(root);
                (q.inbox.take().unwrap(), q.prev.take(), q.next.take())
            };
            match prev {
                Some(prev) => (*Self::queue_of(prev)).next = next,
                None => *inbox.as_ref().head.get() = next,
            }
            if let Some(next) = next {
                (*Self::queue_of(next)).prev = prev;
            }
            inbox
        }
    }
    /// Queue a clean slot on its root's list, linking the root on its live
    /// inbox if the list was empty.
    fn push(root: NonNull<Slot>, slot: NonNull<Slot>) {
        // SAFETY: owner Unit; each dirty slot has one list link.
        unsafe {
            let linked = {
                let q = &mut *Self::queue_of(root);
                *slot.as_ref().next_pending.get() = q.head;
                if q.head.replace(slot).is_none() {
                    q.tail = Some(slot);
                    None
                } else {
                    q.inbox
                }
            };
            match linked {
                Some(inbox) => inbox.as_ref().notify(),
                None => Self::link(root, Self::live_inbox(root)),
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
                // SAFETY: owner Unit owns this slot's unpublished list link.
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
            // the table lease on the owner Unit for links and canonicalization.
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
        // SAFETY: the map retains the inbox until the next reap; owner Unit.
        unsafe {
            if (*inbox.as_ref().head.get()).is_none() && self.unassigned.is_none() {
                inbox.as_ref().clear();
            }
        }
    }
    /// Put a root's nonempty list on its live inbox.
    fn follow(&self, root: NonNull<Slot>) {
        let target = Self::live_inbox(root);
        // SAFETY: owner Unit owns the root's queue.
        let linked = unsafe { (*Self::queue_of(root)).inbox };
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
        // SAFETY: owner Unit owns both queues; the loser is no longer a root.
        unsafe {
            let (head, tail) = {
                let l = &mut *Self::queue_of(loser);
                let Some(head) = l.head.take() else {
                    return;
                };
                (head, l.tail.take().unwrap())
            };
            let old = Self::unlink(loser);
            let linked = {
                let w = &mut *Self::queue_of(winner);
                let linked = w.tail.is_some().then(|| w.inbox.unwrap());
                match w.tail {
                    Some(last) => *last.as_ref().next_pending.get() = Some(head),
                    None => w.head = Some(head),
                }
                w.tail = Some(tail);
                linked
            };
            match linked {
                Some(inbox) => inbox.as_ref().notify(),
                None => Self::link(winner, Self::live_inbox(winner)),
            }
            self.vacate(old);
        }
    }
    pub(super) fn pop(&mut self, owner: Option<u64>) -> Option<NonNull<Slot>> {
        let inbox = match owner {
            Some(owner) => &**self.inboxes.get(&owner)?,
            None => &**self.inboxes.values().find(|inbox| {
                // SAFETY: the caller runs on the owner Unit; only it mutates queue links.
                unsafe { (*inbox.head.get()).is_some() }
            })?,
        };
        // SAFETY: the owner Unit owns links; clearing the last root also
        // clears its owner's notification.
        unsafe {
            let Some(root) = *inbox.head.get() else {
                inbox.clear();
                return None;
            };
            let (slot, emptied) = {
                let q = &mut *Self::queue_of(root);
                let slot = q.head.unwrap();
                q.head = (*slot.as_ref().next_pending.get()).take();
                if q.head.is_none() {
                    q.tail = None;
                }
                (slot, q.head.is_none())
            };
            if emptied {
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
                // SAFETY: the test reads on the owner Unit.
                unsafe {
                    let mut root = *inbox.head.get();
                    while let Some(r) = root {
                        let q = &*Self::queue_of(r);
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

/// One refund posted from another Unit: `bytes` (or a zero pin when 0) for `payer`.
pub(super) struct Post {
    payer: Payer,
    bytes: u64,
    next: *mut Post,
    #[cfg(feature = "record")]
    retirement: Option<crate::tape::Retirement>,
}
/// The mailbox head after retirement. Never a node address (Post is 8-aligned).
pub(super) fn closed() -> *mut Post {
    std::ptr::without_provenance_mut(1)
}
impl Domain {
    /// The owner's routing. Single-consumer invariant: only the owner Unit
    /// dereferences this pointer (Law 1 keeps one worker inside it at a time),
    /// or `Funding::drop` under `retire` after `close_mail` has seen `posters`
    /// reach zero. Foreign posters touch only `mail`, `pending` and `posters`.
    /// Each site takes one `&mut` for a scope that ends before any other
    /// site's can begin, so no two references to the cell are live at once.
    #[inline(always)]
    pub(super) fn remote(&self) -> *mut Remote {
        self.remote.get()
    }
    #[cfg(test)]
    pub(super) fn remote_for_test(&self) -> &Remote {
        // SAFETY: tests read routing on the owner's thread, between operations.
        unsafe { &*self.remote() }
    }
    /// Feed a mailbox batch, newest first, into the owner's unassigned list in
    /// posting order. The owner Unit only.
    pub(super) fn fold(remote: &mut Remote, mut batch: *mut Post) {
        let mut fifo: *mut Post = std::ptr::null_mut();
        while !batch.is_null() {
            // SAFETY: the swap gave this Unit sole custody of the batch.
            unsafe {
                let next = (*batch).next;
                (*batch).next = fifo;
                fifo = batch;
                batch = next;
            }
        }
        while !fifo.is_null() {
            // SAFETY: as above; each node was boxed by its poster.
            let post = unsafe { Box::from_raw(fifo) };
            fifo = post.next;
            remote.enqueue(post.payer, post.bytes, None);
            #[cfg(feature = "record")]
            if let Some(retirement) = post.retirement {
                remote
                    .retirements
                    .entry(post.payer.0)
                    .or_default()
                    .push(retirement);
            }
        }
    }
    /// Whether a bound owner has refunds to fold: its inbox's, or any mail.
    #[inline(always)]
    pub(super) fn bound_pending(&self, inbox: &Inbox) -> bool {
        inbox.pending() || !self.mail.load(Ordering::Acquire).is_null()
    }
    /// Close the mailbox at retirement and take its last batch. Later posts
    /// find CLOSED and wait for `retire`, which the caller holds. Returns once
    /// no poster whose push made the batch can still touch this Domain.
    pub(super) fn close_mail(&self) -> *mut Post {
        let batch = self.mail.swap(closed(), Ordering::AcqRel);
        let mut spins = 0u32;
        while self.posters.load(Ordering::Acquire) != 0 {
            spins += 1;
            if spins < 64 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
        batch
    }
}

impl Payer {
    /// Post a refund from another Unit. The unapplied bytes (or zero pin) keep
    /// the slot and Domain alive until the owner folds the post; `posters`
    /// keeps the Domain alive from the push to this call's last touch.
    #[cold]
    pub(super) fn remote_refund(self, bytes: u64) {
        self.post_refund(
            bytes,
            #[cfg(feature = "record")]
            None,
        );
    }

    #[cfg(feature = "record")]
    pub(crate) fn is_current_owner(self, local: Payer) -> bool {
        self.same_domain(local)
            && self.forest().record(self.root()).live.owner == crate::tasks::current_id()
    }

    #[cfg(feature = "record")]
    pub(crate) fn post_retirement(self, bytes: u64, retirement: crate::tape::Retirement) {
        self.post_refund(bytes, Some(retirement));
    }

    fn post_refund(
        self,
        bytes: u64,
        #[cfg(feature = "record")] retirement: Option<crate::tape::Retirement>,
    ) {
        let d = self.domain();
        d.posters.fetch_add(1, Ordering::SeqCst);
        let post = Box::into_raw(Box::new(Post {
            payer: self,
            bytes,
            next: std::ptr::null_mut(),
            #[cfg(feature = "record")]
            retirement,
        }));
        let mut head = d.mail.load(Ordering::Acquire);
        loop {
            if head == closed() {
                // SAFETY: never published.
                drop(unsafe { Box::from_raw(post) });
                d.posters.fetch_sub(1, Ordering::Release);
                return self.retired_refund(bytes);
            }
            // SAFETY: the node is unpublished until the exchange succeeds.
            unsafe { (*post).next = head };
            match d
                .mail
                .compare_exchange_weak(head, post, Ordering::SeqCst, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(now) => head = now,
            }
        }
        // After the push: the owner lowers `pending` before rechecking `mail`.
        d.pending.store(true, Ordering::SeqCst);
        d.posters.fetch_sub(1, Ordering::Release);
    }
    /// A refund after the last handle dropped: no owner remains to fold it.
    #[cold]
    fn retired_refund(self, bytes: u64) {
        let d = self.domain();
        let guard = d.retire.lock().unwrap();
        let f = self.forest();
        f.refund(self, bytes);
        f.retired_bytes -= bytes;
        let empty = f.retired_bytes == 0 && f.zeros == 0;
        let domain = self.slot().domain;
        drop(guard);
        if empty {
            // SAFETY: all handles, zero leases and outstanding bytes are gone.
            unsafe {
                drop(Box::from_raw(domain.as_ptr()));
            }
        }
    }
}

impl Funding {
    /// Apply only the running owner's ordered refunds. No admission.
    #[inline(always)]
    pub fn drain(&self) {
        if self.pending() {
            self.drain_notified();
        } else {
            #[cfg(feature = "record")]
            self.replay_retirements();
        }
    }
    /// Owner-event delivery only. Routing and folding share the domain mutex.
    /// A union may move the current representative.
    #[cold]
    pub(crate) fn drain_notified(&self) {
        self.drain_pending();
        #[cfg(feature = "record")]
        self.replay_retirements();
        self.normalize();
    }
    #[cfg(feature = "record")]
    fn replay_retirements(&self) {
        if let Some(group) = crate::tasks::current_group() {
            group.replay_retirements(self.payer.get());
        }
    }

    #[cold]
    pub(super) fn drain_pending(&self) {
        let domain = self.payer.get().domain();
        // SAFETY: drains run on the owner Unit.
        let remote = unsafe { &mut *domain.remote() };
        let batch = domain.mail.swap(std::ptr::null_mut(), Ordering::Acquire);
        debug_assert!(batch != closed(), "a drain after retirement");
        Domain::fold(remote, batch);
        self.drain_locked(remote, false);
        // Lower, then recheck the mailbox: a post pushed after the swap either
        // shows here or raises `pending` after this store (both SeqCst).
        domain.pending.store(remote.pending(), Ordering::SeqCst);
        if !domain.mail.load(Ordering::SeqCst).is_null() {
            domain.pending.store(true, Ordering::Relaxed);
        }
        remote.reap(domain);
    }
    pub(super) fn drain_locked(&self, remote: &mut Remote, retired: bool) {
        remote.route_unassigned();
        let owner = (!retired).then(crate::tasks::current_id);
        let f = self.payer.get().forest();
        while let Some(slot) = remote.pop(owner) {
            // SAFETY: the dirty node retains a charge/zero pin until delivery;
            // pop cleared its intrusive link under the same domain mutex.
            let payer = Payer(slot);
            let slot = payer.slot();
            if let Some(unused) = slot.handover.take() {
                let original_parent = f.record_mut(payer).parent.take().unwrap();
                let (winner, loser) = f.merge(payer, original_parent, unused);
                remote.unite(winner.0, loser.0);
                f.release(payer);
            }
            let bytes = slot.pending.swap(0, Ordering::Relaxed);
            let zeros = slot.pending_zeros.swap(0, Ordering::Relaxed);
            if bytes != 0 {
                f.refund(payer, bytes);
                if retired {
                    f.retired_bytes -= bytes;
                }
            }
            for _ in 0..zeros {
                f.refund(payer, 0);
            }
            #[cfg(feature = "record")]
            if let Some(events) = remote.retirements.remove(&payer.0)
                && !retired
            {
                for event in events {
                    event.record();
                }
            }
        }
    }
}
