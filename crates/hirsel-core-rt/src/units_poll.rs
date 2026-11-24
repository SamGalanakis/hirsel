use super::*;

/// Poll one slot, waking units with socket arrivals in event order and firing
/// due timers. `None` waits for an event and `Some(0)` is nonblocking.
pub(super) fn poll_slot(p: &Pool, ix: usize, timeout_ns: Option<u64>) {
    let slot = &p.slots[ix];
    let mut evs = [hirsel_core_net::sys::epoll_event { events: 0, u64: 0 }; 256];
    bump(S_GPOLLS);
    if timeout_ns != Some(0) {
        bump(S_GPOLL_BLOCKS);
    }
    let n = if p.uring {
        slot.poller
            .wait_slot(&mut evs, timeout_ns)
            .unwrap_or_else(|_| crate::process::stop(b"HostPoll"))
    } else {
        let timeout = if slot.poller.udp_due() {
            0
        } else {
            timeout_ns.map_or(-1, |ns| ns.div_ceil(1_000_000).min(i32::MAX as u64) as i32)
        };
        // SAFETY: a live epoll and a writable event array.
        let n = unsafe { sync::epoll_wait(slot.epfd, evs.as_mut_ptr(), 128, timeout) };
        slot.last_poll.store(now(), Ordering::Relaxed);
        slot.poller.dispatch_udp_slot(&mut evs[..n.max(0) as usize])
    };
    let mut published = false;
    // A wake carries nothing; epoll's eventfd is edge-triggered and never read.
    for e in evs.iter().take(n).filter(|e| e.u64 != WAKE_TAG) {
        bump(S_GPOLL_EVENTS);
        let tag = e.u64;
        let unit = (tag >> hirsel_core_net::SHARED_UNIT_SHIFT) as u32;
        if let Some(u) = unit_ref(unit) {
            let ident = tag & ((1 << hirsel_core_net::SHARED_UNIT_SHIFT) - 1);
            push_arrival(u, ident, e.events);
            if mark_ready(u) {
                #[cfg(feature = "testing")]
                observe::queued(u);
                // Put arrivals on this slot's queue in event order.
                push_counted(p, &p.locals[ix], UP(u));
                published = true;
            }
        }
    }
    if published {
        // One worker starts the search for the batch. The search path
        // passes the wake on if queued work remains.
        notify_worker(p);
    }
    fire_timers(p, ix);
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
    if due {
        poll_slot(p, ix, Some(0));
    } else {
        fire_timers(p, ix);
    }
}
