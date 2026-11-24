use super::{
    Ordering, Pool, S_CREATE_NS, S_CREATED, S_GPOLL_BLOCKS, S_GPOLL_EVENTS, S_GPOLLS, S_LIFO,
    S_NOTIFIED_REQUEUE, S_OS_WAKES, S_PARKS, S_READY_TO_PARK_NS, S_RESUMES, S_STEALS,
    S_TIMER_FIRES, S_YIELDS, USTATS,
};

pub(super) fn print(p: &Pool) {
    p.memory.report();
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
        "untied-stats workers={} units={} resumes={} parks={} yields={} steals={} os_wakes={} gpolls={} gpoll_blocks={} gpoll_events={} timer_fires={} notified_requeue={} create_ns_avg={} ready_to_park_ns_avg={} unit_epoll_waits={} lifo={} net_uring={}{tape_stats}",
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
    );
}
