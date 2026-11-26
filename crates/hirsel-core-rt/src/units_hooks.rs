//! Pool callbacks: the tied scheduler reaches the unit pool only through
//! these, so an executable that never enables the pool keeps none of it.
use super::host_calls::{HostCall, run_off_slot};
use super::{
    SharedMemory, check_slot, park_unit, pool, pool_memory, reply, sync::AtomicBool,
    wake_unit_by_id, worker_ix, yield_unit_if_needed,
};
use std::sync::{OnceLock, atomic::Ordering};

pub(super) static UNTIED_ON: AtomicBool = AtomicBool::new(false);
#[inline]
pub(crate) fn untied() -> bool {
    UNTIED_ON.load(Ordering::Relaxed)
}

pub(super) struct PoolHooks {
    pub(super) memory: fn() -> Option<&'static dyn SharedMemory>,
    pub(super) park: fn(Option<u64>),
    pub(super) maybe_yield: fn(),
    pub(super) wake: fn(u32),
    pub(super) off_slot: fn(HostCall, &mut (dyn FnMut() + Send)) -> bool,
    pub(super) take_slot: fn(),
    pub(super) uses_file_ring: fn() -> bool,
    pub(super) file_request: unsafe fn(hirsel_core_net::FileEntry) -> std::io::Result<i32>,
}

static POOL_HOOKS: OnceLock<PoolHooks> = OnceLock::new();

pub(super) fn enable_pool() {
    let _ = POOL_HOOKS.set(PoolHooks {
        memory: pool_memory,
        park: park_unit,
        maybe_yield: yield_unit_if_needed,
        wake: wake_unit_by_id,
        off_slot: run_off_slot,
        take_slot: take_running_slot,
        uses_file_ring: reply::uses_file_ring_pool,
        file_request: reply::file_request_pool,
    });
    UNTIED_ON.store(true, Ordering::SeqCst);
}

// A launcher installs these callbacks before any worker starts. A tied
// scheduler does not name the pool's records or queues through its wait path.
pub(super) fn pool_hooks() -> &'static PoolHooks {
    POOL_HOOKS.get().unwrap()
}

fn take_running_slot() {
    let (p, ix) = (pool(), worker_ix());
    if ix < p.w {
        check_slot(p, ix);
    }
}
