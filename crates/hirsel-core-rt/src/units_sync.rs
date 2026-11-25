//! The worker pool's synchronisation and sleeping calls, in one place. An
//! ordinary build re-exports the standard and libc items themselves, so the
//! pool compiles to the same code as when it named them directly. A test
//! build swaps in the interleaving checker's stand-ins, which make every call
//! a scheduling point inside a checker run and pass straight through outside
//! one (see `units_check.rs`).
#[cfg(not(test))]
pub(crate) use crate::os::monotonic_ns;
/// A slot's wait on its readiness set; an error reports no events.
#[cfg(not(test))]
#[inline(always)]
pub(crate) fn wait(
    handle: hirsel_core_net::ready::Handle,
    events: &mut [hirsel_core_net::ready::Event],
    timeout_ns: Option<u64>,
) -> usize {
    handle.wait(events, timeout_ns).unwrap_or(0)
}
#[cfg(not(test))]
#[inline(always)]
pub(crate) fn wake(waker: hirsel_core_net::ready::Waker) {
    waker.wake()
}
#[cfg(not(test))]
pub(crate) use std::sync::{
    Mutex, MutexGuard,
    atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize},
};

#[cfg(test)]
pub(crate) use super::check::seam::*;
