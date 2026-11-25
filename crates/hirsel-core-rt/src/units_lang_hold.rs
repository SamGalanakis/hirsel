//! Test hooks on the running task's unit: hold its arrivals, and let the
//! oldest held one reach it (the ledger's hold hook, for RL8). In-crate laws
//! call them directly; a native law (feature `testing`) calls
//! `hirsel_core_test_hold` from a linked C object. Production builds have
//! neither.
/// # Safety
/// In a task of a language unit.
pub(crate) unsafe fn hook() {
    // SAFETY: by the contract.
    unsafe { super::unit().ctx.hook() }
}
/// # Safety
/// As `hook`.
pub(crate) unsafe fn unhook() {
    // SAFETY: by the contract.
    unsafe { super::unit().ctx.unhook() }
}
/// # Safety
/// As `hook`.
pub(crate) unsafe fn reach_oldest() -> bool {
    // SAFETY: by the contract.
    unsafe { super::unit().ctx.reach_oldest() }
}
/// The scheduler's pump of the unit (`capture_wakes`), between events.
/// In-crate laws only: natively the scheduler pumps by itself.
/// # Safety
/// As `hook`, between operations of the task.
#[cfg(test)]
pub(crate) unsafe fn pump() {
    // SAFETY: by the contract; no operation of the unit is under way.
    unsafe { super::unit().pump_ready() }
}

/// RL8 natively: one hold step on the running task's unit, from a C object
/// linked into a test program (between two of the program's runtime calls,
/// so no operation of the unit is under way). `step` 0 holds every arrival
/// published from now on, 1 lets every held one reach the unit and stops
/// holding, 2 lets the oldest held one reach it and returns false when none
/// was published. It records nothing: no frame, event or take-point position.
/// # Safety
/// Called on the thread of a running task of a language unit, outside any
/// runtime operation.
#[cfg(feature = "testing")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_hold(step: u8) -> bool {
    // SAFETY: by the contract.
    unsafe {
        match step {
            0 => hook(),
            1 => unhook(),
            2 => return reach_oldest(),
            _ => panic!("hold step {step}"),
        }
    }
    true
}
