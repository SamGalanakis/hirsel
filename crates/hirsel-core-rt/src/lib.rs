//! FORM v1 memory operations. Emitters own layout, moves and drop recipes.
//! Logical capacity refusal is recoverable; host exhaustion is a run stop.

#[cfg(test)]
hirsel_guard::install!();

pub mod abi;
pub mod block;
pub mod causes;
pub mod counted;
pub mod limit;
pub mod region;

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
pub struct Context {
    pub(crate) current: Funding,
    stack: Vec<Funding>,
    pub(crate) finalizing: usize,
    pub(crate) primary: Option<Box<causes::Cell>>,
    pub(crate) causes: causes::Causes,
    pub(crate) task_record: Option<block::Block>,
    pub(crate) active_resource: *mut causes::Resource,
}

impl Context {
    /// The task provider admits its task record and primary cell separately.
    pub fn new(bytes: u64) -> Self {
        Self {
            current: Funding::root(bytes),
            stack: Vec::new(),
            finalizing: 0,
            primary: None,
            causes: causes::Causes::default(),
            task_record: None,
            active_resource: std::ptr::null_mut(),
        }
    }

    /// Give a fixture's program its exact allowance after prepaying task startup.
    #[cfg(feature = "testing")]
    pub fn reset_test_limit(&mut self, bytes: u64) {
        self.current = Funding::root(bytes);
    }
    /// Refuse the nth admission, optionally also every later admission.
    #[cfg(feature = "testing")]
    pub fn inject_admission(&mut self, n: usize, later: bool) {
        self.current.inject(n, later);
    }
    #[cfg(feature = "testing")]
    pub fn admission_attempts(&self) -> usize {
        self.current.attempts()
    }

    pub fn available(&self) -> u64 {
        self.current.available()
    }

    pub fn limit_enter(&mut self, bytes: u64) -> Result<(), Capacity> {
        let child = self.current.reserve(bytes)?;
        self.stack.push(std::mem::replace(&mut self.current, child));
        Ok(())
    }

    pub fn limit_exit(&mut self) {
        self.current.end();
        self.current = self.stack.pop().unwrap();
    }

    pub(crate) fn payer(&self, like: Option<&Funding>) -> Funding {
        if self.finalizing != 0 {
            self.current.clone()
        } else {
            like.unwrap_or(&self.current).clone()
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        while !self.stack.is_empty() {
            self.limit_exit();
        }
    }
}

#[cfg(test)]
mod causes_tests;
#[cfg(test)]
mod counted_tests;
#[cfg(test)]
mod region_tests;
#[cfg(test)]
mod tests;
