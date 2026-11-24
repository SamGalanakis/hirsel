//! The worker pool's synchronisation and sleeping calls, in one place. An
//! ordinary build re-exports the standard and libc items themselves, so the
//! pool compiles to the same code as when it named them directly. A test
//! build swaps in the interleaving checker's stand-ins, which make every call
//! a scheduling point inside a checker run and pass straight through outside
//! one (see `units_check.rs`).
#[cfg(not(test))]
pub(crate) use crate::os::monotonic_ns;
#[cfg(not(test))]
pub(crate) use hirsel_core_net::sys::{epoll_wait, eventfd_write as write};
#[cfg(not(test))]
pub(crate) use std::sync::{
    Mutex, MutexGuard,
    atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize},
};

#[cfg(test)]
pub(crate) use super::check::seam::*;
