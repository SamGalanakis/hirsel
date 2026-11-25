use super::*;
use hirsel_core_net::ready::Event;
use std::cell::RefCell;

thread_local! {
    /// The slot thread's event buffer. A wait reports the prefix it wrote,
    /// so the buffer is filled once per thread, not on every poll: 256 packed
    /// events are 3 KiB, about 600 stores per poll when built on the stack.
    static EVENTS: RefCell<[Event; 256]> = const { RefCell::new([Event::EMPTY; 256]) };
}

/// Poll one slot and return the number of units queued by socket arrivals.
/// `None` waits for an event and `Some(0)` is nonblocking.
fn poll_slot(p: &Pool, ix: usize, timeout_ns: Option<u64>) -> usize {
    EVENTS.with_borrow_mut(|evs| poll_slot_into(p, ix, timeout_ns, evs))
}

fn poll_slot_into(p: &Pool, ix: usize, timeout_ns: Option<u64>, evs: &mut [Event; 256]) -> usize {
    let slot = &p.slots[ix];
    bump(S_GPOLLS);
    if timeout_ns != Some(0) {
        bump(S_GPOLL_BLOCKS);
    }
    let n = if p.uring {
        slot.poller
            .wait_slot(evs, timeout_ns)
            .unwrap_or_else(|_| crate::process::stop(b"HostPoll"))
    } else {
        let timeout_ns = if slot.poller.udp_due() {
            Some(0)
        } else {
            timeout_ns
        };
        let n = sync::wait(slot.handle, &mut evs[..128], timeout_ns);
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
    published
}

/// Wait for a wake, socket readiness, or the earliest timer. A wake sent
/// after the slot joins the idle list stays pending until the poll returns.
/// Return whether a notifier handed this slot the search role.
pub(super) fn park_slot(p: &Pool, ix: usize) -> bool {
    {
        let mut idle = p.idle.lock().unwrap();
        p.nsleep.fetch_add(1, Ordering::SeqCst);
        if p.queued.load(Ordering::SeqCst) != 0 || p.stop.load(Ordering::SeqCst) {
            p.nsleep.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        idle.push(ix);
    }
    let nt = p.slots[ix].next_timer.load(Ordering::SeqCst);
    let timeout = (nt != u64::MAX).then(|| nt.saturating_sub(now()));
    let arrivals = poll_slot(p, ix, timeout);
    // A notifier that already removed the slot gave it the search role.
    let mut idle = p.idle.lock().unwrap();
    if let Some(at) = idle.iter().position(|&w| w == ix) {
        idle.swap_remove(at);
        p.nsleep.fetch_sub(1, Ordering::SeqCst);
    }
    drop(idle);
    let handed = p.handed[ix].swap(false, Ordering::AcqRel);
    // This slot can take one arrival. A handed slot already counts as searching.
    if arrivals > 1 {
        notify_worker(p, usize::from(!handed));
    }
    fire_timers(p, ix);
    handed
}

/// The owning thread's check of its slot between units: take whatever its
/// poller holds without blocking (an io_uring ring says so without a
/// syscall; epoll is asked every `poll_every_ns` once sockets exist), then
/// fire the slot's due timers.
pub(super) fn check_slot(p: &Pool, ix: usize) {
    let due = if p.uring {
        p.slots[ix].poller.due()
    } else {
        p.slots[ix].poller.udp_due()
            || p.net_used.load(Ordering::Relaxed)
                && now().saturating_sub(p.slots[ix].last_poll.load(Ordering::Relaxed))
                    > p.poll_every_ns
    };
    if due && poll_slot(p, ix, Some(0)) > 0 {
        notify_worker(p, 0);
    }
    fire_timers(p, ix);
}
