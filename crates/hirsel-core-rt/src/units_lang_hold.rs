//! Test hooks on the running task's unit: hold its arrivals, and let the
//! oldest held one reach it (the ledger's hold hook, for RL8).
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
/// # Safety
/// As `hook`, between operations of the task.
pub(crate) unsafe fn pump() {
    // SAFETY: by the contract; no operation of the unit is under way.
    unsafe { super::unit().pump_ready() }
}
