//! FORM v1 memory operations. Emitters own layout, moves and drop recipes.
//! Logical capacity refusal is recoverable; host exhaustion is a run stop.

#[cfg(test)]
hirsel_guard::install!();

pub mod abi;
pub mod abi_host;
pub mod abi_layout;
pub mod abi_tasks;
mod allocator;
pub mod block;
pub mod causes;
pub mod channel;
pub mod counted;
mod destination;
pub mod entry;
mod fiber;
pub mod limit;
pub mod os;
pub mod process;
pub mod provider;
pub mod region;
pub mod tasks;

use limit::Funding;

/// MEM-22. Also used for the error portion of C allocation results.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capacity {
    pub requested: u64,
    pub available: u64,
}

/// The hidden first argument of native functions. A task owns its context;
/// custody can carry funding records beyond this context's lifetime.
#[repr(C)]
pub struct Context {
    // Funding's first word is the one frozen current-record pointer.
    pub(crate) current: Funding,
    pub(crate) run: *const crate::process::Config,
    pub(crate) cancel_requested: bool,
    pub(crate) cancel_shield: usize,
    pub(crate) finalizing: usize,
    pub(crate) bound_depth: usize,
    pub(crate) grant_depth: usize,
    pub(crate) primary: Option<Box<causes::Cell>>,
    pub(crate) causes: causes::Causes,
    pub(crate) task_record: Option<block::Block>,
    pub(crate) active_resource: *mut causes::Resource,
}

impl Context {
    /// The task provider admits its task record and primary cell separately.
    #[inline]
    pub fn new(bytes: u64) -> Self {
        Self::with_funding(Funding::root(bytes))
    }

    pub(crate) fn with_funding(mut current: Funding) -> Self {
        current.normalize();
        Self {
            current,
            run: std::ptr::null(),
            cancel_requested: false,
            cancel_shield: 0,
            finalizing: 0,
            bound_depth: 0,
            grant_depth: usize::MAX,
            primary: None,
            causes: causes::Causes::default(),
            task_record: None,
            active_resource: std::ptr::null_mut(),
        }
    }

    #[cfg(feature = "testing")]
    pub fn reset_test_limit(&mut self, bytes: u64) {
        self.current = Funding::root(bytes);
        self.current.bind();
    }
    #[cfg(feature = "testing")]
    pub fn inject_admission(&mut self, n: usize, later: bool) {
        self.current.inject(n, later);
    }
    #[cfg(feature = "testing")]
    pub fn admission_attempts(&self) -> usize {
        self.current.attempts()
    }
    /// Native emitter pointer to the frozen current-limit prefix. Dereferencing
    /// requires the task-exclusive discipline of the C ABI.
    #[inline(always)]
    pub fn limit_record(&self) -> *mut limit::LimitRecord {
        self.current.raw_local()
    }

    #[inline]
    pub fn available(&self) -> u64 {
        // SAFETY: native entry/resume and every limit/grant transition install
        // this context's live canonical record before it can execute.
        unsafe { (*self.limit_record()).available }
    }

    #[inline]
    pub fn limit_enter(&mut self, bytes: u64) -> Result<(), Capacity> {
        self.current.enter(bytes)?;
        self.bound_depth = self.bound_depth.wrapping_add(1);
        Ok(())
    }

    #[inline]
    pub fn limit_exit(&mut self) {
        self.bound_depth = self.bound_depth.wrapping_sub(1);
        self.current.exit();
    }

    #[inline]
    pub(crate) fn payer(&self, like: Option<&limit::Payer>) -> limit::Payer {
        if self.finalizing != 0 {
            limit::Payer::from_current(self.limit_record())
        } else {
            like.copied().map_or_else(
                || limit::Payer::from_current(self.limit_record()),
                limit::Payer::canonical,
            )
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        while self.bound_depth != 0 {
            self.limit_exit();
        }
    }
}

#[cfg(test)]
mod causes_tests;
#[cfg(test)]
mod compact_tests;
#[cfg(test)]
mod counted_tests;
#[cfg(test)]
mod region_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod provider_tests;

#[cfg(test)]
mod tasks_tests;

#[cfg(test)]
mod schedule_tests;

#[cfg(test)]
mod entry_tests;
