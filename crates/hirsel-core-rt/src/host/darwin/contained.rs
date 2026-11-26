//! Contained code runs on Linux only at W1; Darwin is W2 (FFI-15), and the
//! driver refuses a contained module for a Darwin target.
use crate::Context;
use crate::foreign::contained::Call;

#[derive(Default)]
pub(crate) struct Instances;

/// # Safety
/// None: no Darwin task has an instance.
pub(crate) unsafe fn end_task(_: *mut Context) {}

/// # Safety
/// None: no Darwin run makes a contained handle.
pub(crate) unsafe fn release(_: *mut Context, _: &Call, _: u64) {
    unreachable!("a contained handle on a Darwin run");
}
