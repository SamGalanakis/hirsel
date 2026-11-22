//! Process-wide allocation count for instrumented benchmark archives.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

#[inline]
pub(crate) fn allocated() {
    ALLOCATIONS.fetch_add(1, Relaxed);
}

#[unsafe(no_mangle)]
extern "C" fn hirsel_bench_allocation_count() -> u64 {
    ALLOCATIONS.load(Relaxed)
}
