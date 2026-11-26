use super::*;

pub(crate) fn has_retaining() -> bool {
    slabs::retaining()
        || returns::orphaned_pending()
        || RETAINING_HEAPS.load(Ordering::Relaxed) != 0
}

/// The poll timeout for a worker about to wait `timeout` ms (-1: no limit), and the
/// time after which a wait that finds nothing releases retained pages and stacks.
pub(crate) fn idle_wait(timeout: i32, warm_stacks: bool) -> (i32, Option<u64>) {
    if !(warm_stacks || has_retaining() || crate::fiber::surplus())
        || (0..=RELEASE_DELAY_MS).contains(&timeout)
    {
        (timeout, None)
    } else {
        let due = crate::os::monotonic_ns() + RELEASE_DELAY_MS as u64 * 1_000_000;
        (RELEASE_DELAY_MS, Some(due))
    }
}

/// Return every retained slab's and dedicated page's native pages to the
/// host. A worker calls this after an `idle_wait` that found nothing, or
/// before it parks without a poller.
pub(crate) fn purge() {
    returns::purge();
    slabs::purge();
    if RETAINING_HEAPS.load(Ordering::Relaxed) == 0 {
        return;
    }
    let heaps = std::mem::take(&mut *RETAINING.lock().unwrap());
    RETAINING_HEAPS.fetch_sub(heaps.len(), Ordering::Relaxed);
    // Returns list the heap while holding its page cache, so release
    // the list lock before taking any cache lock.
    for heap in heaps {
        let mut cache = heap.cache().lock().unwrap();
        cache.listed = false;
        cache.purge_slabs();
        while let Some(oldest) = cache.dedicated.remove_oldest() {
            cache.dedicated.purge(oldest);
        }
    }
}
