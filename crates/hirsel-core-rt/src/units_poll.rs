use super::*;
use hirsel_core_net::ready::Event;
use std::cell::RefCell;

thread_local! {
    /// The slot thread's event buffer. A wait reports the prefix it wrote,
    /// so the buffer is filled once per thread, not on every poll.
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
}

/// Poll one slot and return the number of units queued by socket arrivals.
/// `None` waits for an event and `Some(0)` is nonblocking.
struct SlotPoll {
    published: usize,
    events: usize,
}

fn poll_slot(p: &Pool, ix: usize, timeout_ns: Option<u64>) -> SlotPoll {
    EVENTS.with_borrow_mut(|evs| {
        evs.resize(2 * p.plan.reactor.poll_batch as usize, Event::EMPTY);
        poll_slot_into(p, ix, timeout_ns, evs)
    })
}

fn poll_slot_into(p: &Pool, ix: usize, timeout_ns: Option<u64>, evs: &mut [Event]) -> SlotPoll {
    let slot = &p.slots[ix];
    bump(S_GPOLLS);
    if timeout_ns != Some(0) {
        bump(S_GPOLL_BLOCKS);
    }
    let n = if p.uring {
        match slot.poller.wait_slot(evs, timeout_ns) {
            Ok(n) => n,
            Err(_) => {
                crate::run_stop::record(crate::run_stop::Stop::from_static(b"HostPoll"));
                return SlotPoll {
                    published: 0,
                    events: 0,
                };
            }
        }
    } else {
        let timeout_ns = if slot.poller.udp_due() {
            Some(0)
        } else {
            timeout_ns
        };
        let n = sync::wait(
            slot.handle,
            &mut evs[..p.plan.reactor.poll_batch as usize],
            timeout_ns,
        );
        slot.last_poll.store(now(), Ordering::Relaxed);
        slot.poller.dispatch_udp_slot(&mut evs[..n])
    };
    let mut published = 0;
    // A wake carries nothing; epoll's eventfd is edge-triggered and never read.
    for e in evs.iter().take(n).filter(|e| e.token() != WAKE_TAG) {
        bump(S_GPOLL_EVENTS);
        let tag = e.token();
        let unit = (tag >> hirsel_core_net::SHARED_UNIT_SHIFT) as u32;
        if let Some(u) = unit_ref(unit) {
            let ident = tag & ((1 << hirsel_core_net::SHARED_UNIT_SHIFT) - 1);
            push_arrival(u, ident, e.ready());
            if mark_ready(u) {
                #[cfg(feature = "testing")]
                observe::queued(u);
                // Put arrivals on this slot's queue in event order.
                push_counted(p, &p.locals[ix], UP(u));
                published += 1;
            }
        }
    }
    SlotPoll {
        published,
        events: n,
    }
}

/// Wait for a wake, socket readiness, or the earliest timer. A wake sent
/// after the slot joins the idle list stays pending until the poll returns.
/// Return whether a notifier handed this slot the search role.
pub(super) fn park_slot(p: &Pool, ix: usize) -> bool {
    let last = {
        let mut idle = p.idle.lock().unwrap();
        let sleepers = p.nsleep.fetch_add(1, Ordering::SeqCst) + 1;
        if p.queued.load(Ordering::SeqCst) != 0 || p.stop.load(Ordering::SeqCst) {
            p.nsleep.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        idle.push(ix);
        sleepers == p.w
    };
    let nt = p.slots[ix].next_timer.load(Ordering::SeqCst);
    let purge_at = (last && (p.memory.has_retained() || crate::task_heap::has_retaining()))
        .then(|| now().saturating_add(p.plan.idle_release_ms * 1_000_000));
    let deadline = purge_at.map_or(nt, |at| at.min(nt));
    let timeout = (deadline != u64::MAX).then(|| deadline.saturating_sub(now()));
    let polled = poll_slot(p, ix, timeout);
    // A notifier that already removed the slot gave it the search role.
    let mut idle = p.idle.lock().unwrap();
    let purge = polled.events == 0
        && purge_at.is_some_and(|at| now() >= at)
        && idle.len() == p.w
        && p.queued.load(Ordering::SeqCst) == 0;
    if let Some(at) = idle.iter().position(|&w| w == ix) {
        idle.swap_remove(at);
        p.nsleep.fetch_sub(1, Ordering::SeqCst);
    }
    drop(idle);
    if purge {
        p.memory.request_idle_purge();
    }
    let handed = p.handed[ix].swap(false, Ordering::AcqRel);
    // This slot can take one arrival. A handed slot already counts as searching.
    if polled.published > 1 {
        notify_worker(p, usize::from(!handed));
    }
    fire_timers(p, ix);
    handed
}

/// The owning thread's check of its slot between units: take whatever its
/// poller holds without blocking (an io_uring ring says so without a
/// syscall; epoll is asked every `POLL_EVERY_NS` once sockets exist), then
/// fire the slot's due timers.
pub(super) fn check_slot(p: &Pool, ix: usize) {
    let due = if p.uring {
        p.slots[ix].poller.due()
    } else {
        p.slots[ix].poller.udp_due()
            || p.net_used.load(Ordering::Relaxed)
                && now().saturating_sub(p.slots[ix].last_poll.load(Ordering::Relaxed))
                    > super::POLL_EVERY_NS
    };
    if due && poll_slot(p, ix, Some(0)).published > 0 {
        notify_worker(p, 0);
    }
    fire_timers(p, ix);
}

pub(super) fn push_arrival(u: &Unit, id: u64, flags: Ready) {
    let mut q = u.xready.lock().unwrap();
    q.push((id, flags));
    u.xpending.store(true, Ordering::Release);
}

/// Readiness queued for the running unit, as (reactor identity, flags), in
/// the order it reached the unit. A push after the check wakes the unit, so
/// it is taken at the unit's next switch. Nobody else takes the readiness of
/// the slot this unit runs on, so each scheduler turn takes it here first
/// rather than going dry, parking and resuming for readiness already there.
pub(crate) fn take_xready(mut each: impl FnMut(u64, Ready)) {
    if untied() {
        (pool_hooks().take_slot)();
    }
    // SAFETY: called by the running unit's scheduler.
    let u = unsafe { &*cur() };
    if !u.xpending.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: only the running unit touches its spare buffer.
    let batch = unsafe { &mut *u.xspare.get() };
    {
        let mut q = u.xready.lock().unwrap();
        u.xpending.store(false, Ordering::Relaxed);
        std::mem::swap(&mut *q, batch);
    }
    for (id, flags) in batch.drain(..) {
        each(id, flags);
    }
    if batch.capacity() > pool().plan.ready_scratch_bytes / size_of::<(u64, Ready)>() {
        *batch = Vec::new();
    }
}
