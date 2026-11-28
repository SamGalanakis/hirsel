use super::{Ordering, Pool, sync::AtomicU64, worker_ix, xstats_enabled};
use std::cell::Cell;

/// Counts from outside the worker pool and from workers that have exited.
pub(crate) static USTATS: [AtomicU64; STATS] = [const { AtomicU64::new(0) }; STATS];
thread_local! {
    static WORKER_STATS: [Cell<u64>; STATS] = [const { Cell::new(0) }; STATS];
}
pub(super) const S_RESUMES: usize = 0;
pub(super) const S_PARKS: usize = 1;
pub(super) const S_YIELDS: usize = 2;
pub(super) const S_STEALS: usize = 3;
pub(super) const S_OS_WAKES: usize = 4;
pub(super) const S_GPOLLS: usize = 5;
pub(super) const S_GPOLL_BLOCKS: usize = 6;
pub(super) const S_TIMER_FIRES: usize = 7;
pub(super) const S_CREATED: usize = 8;
pub(super) const S_CREATE_NS: usize = 9;
pub(super) const S_READY_TO_PARK_NS: usize = 10;
pub(super) const S_GPOLL_EVENTS: usize = 11;
pub(super) const S_NOTIFIED_REQUEUE: usize = 12;
pub(super) const S_LIFO: usize = 13;
const STATS: usize = 14;
#[inline]
pub(super) fn bump(i: usize) {
    if xstats_enabled() {
        add_stat(i, 1);
    }
}
/// `bump` on a worker's own host stack (`find`, `run_unit`). That stack
/// always resumes on its own thread, so the counter is read in place.
#[inline(always)]
pub(super) fn bump_host(i: usize) {
    if xstats_enabled() {
        WORKER_STATS.with(|stats| stats[i].set(stats[i].get().wrapping_add(1)));
    }
}

// A unit may resume on another worker. Resolve this worker's TLS in the
// callee so the caller cannot cache its address across a stack switch.
#[inline(never)]
pub(super) fn add_stat(i: usize, n: u64) {
    if worker_ix() == usize::MAX {
        USTATS[i].fetch_add(n, Ordering::Relaxed);
    } else {
        WORKER_STATS.with(|stats| stats[i].set(stats[i].get().wrapping_add(n)));
    }
}

pub(super) fn flush_worker() {
    if xstats_enabled() {
        WORKER_STATS.with(|stats| {
            for (total, local) in USTATS.iter().zip(stats) {
                total.fetch_add(local.get(), Ordering::Relaxed);
            }
        });
    }
}

pub(super) fn print(p: &Pool) {
    p.memory.report();
    let (host_peak, host_queue_peak, host_deferred) = p.helpers.report();
    let g = |i: usize| USTATS[i].load(Ordering::Relaxed);
    #[cfg(feature = "record")]
    let tape_stats = format!(
        " tape_writes={} tape_bytes={} tape_payload={} tape_skipped={} tape_worker_ns={} tape_truncated={} tape_writer_ns={} tape_writer_calls={} tape_write_failed={}",
        crate::tape::TAPE_COUNTS[0].load(Ordering::Relaxed),
        crate::tape::TAPE_COUNTS[1].load(Ordering::Relaxed),
        crate::tape::TAPE_COUNTS[2].load(Ordering::Relaxed),
        crate::tape::TAPE_COUNTS[3].load(Ordering::Relaxed),
        crate::tape::TAPE_COUNTS[4].load(Ordering::Relaxed),
        crate::tape::TAPE_COUNTS[5].load(Ordering::Relaxed),
        crate::tape::TAPE_COUNTS[6].load(Ordering::Relaxed),
        crate::tape::TAPE_COUNTS[7].load(Ordering::Relaxed),
        crate::tape::TAPE_COUNTS[8].load(Ordering::Relaxed),
    );
    #[cfg(not(feature = "record"))]
    let tape_stats = "";
    eprintln!(
        "untied-stats workers={} units={} resumes={} parks={} yields={} steals={} os_wakes={} gpolls={} gpoll_blocks={} gpoll_events={} timer_fires={} notified_requeue={} create_ns_avg={} ready_to_park_ns_avg={} unit_epoll_waits={} lifo={} net_uring={} max_units={} host_threads_cap={} host_threads_peak={host_peak} host_queued_peak={host_queue_peak} host_spawn_deferred={host_deferred}{tape_stats}",
        p.w,
        g(S_CREATED),
        g(S_RESUMES),
        g(S_PARKS),
        g(S_YIELDS),
        g(S_STEALS),
        g(S_OS_WAKES),
        g(S_GPOLLS),
        g(S_GPOLL_BLOCKS),
        g(S_GPOLL_EVENTS),
        g(S_TIMER_FIRES),
        g(S_NOTIFIED_REQUEUE),
        g(S_CREATE_NS) / g(S_CREATED).max(1),
        g(S_READY_TO_PARK_NS) / g(S_CREATED).max(1),
        hirsel_core_net::EPOLL_WAITS.load(Ordering::Relaxed),
        g(S_LIFO),
        u8::from(p.uring),
        p.plan.max_units,
        p.plan.max_host_threads,
    );
}
