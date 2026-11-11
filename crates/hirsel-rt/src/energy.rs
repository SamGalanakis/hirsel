//! A store's energy scopes on the task's meter (storage.md STORE-11 "Energy").
//! Compiled charges subtract from `TaskCtx.energy`; these entries open and
//! close a budget around one applied body, and an exhausted charge's failure
//! grants the cleanup reserve (`hirsel_core_contract::energy`).
use crate::TaskCtx;

/// `EnergyUsed { units: u64, exhausted: bool }` as the layout lays it out.
#[repr(C)]
pub struct UsedOut {
    units: u64,
    exhausted: u8,
}

/// `energy.enter(budget) -> Energy`: opens a scope of `budget` inside the
/// current allowance and writes its token.
/// # Safety
/// Context bound; out writable. A nominal result is given a
/// caller destination (layout `may_hold_blocks`); these words need none.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_energy_enter(
    ctx: *mut TaskCtx,
    _destination: *mut u8,
    out: *mut u64,
    budget: u64,
) -> u8 {
    // SAFETY: the running task owns its context for this entry.
    let ctx = unsafe { &mut *ctx };
    let token = ctx.energy_scopes.enter(&mut ctx.energy, budget);
    unsafe { out.write(token) };
    0
}

/// `energy.leave(take saved) -> EnergyUsed`: closes the innermost scope.
/// Only std@1's store machinery names these operations, and it brackets each
/// applied body with one enter and one leave, so any other token is a defect.
/// # Safety
/// Context bound; the Energy word transfers; out writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_energy_leave(
    ctx: *mut TaskCtx,
    _destination: *mut u8,
    out: *mut UsedOut,
    saved: *mut u64,
) -> u8 {
    // SAFETY: the running task owns its context for this entry.
    let ctx = unsafe { &mut *ctx };
    let Ok(used) = ctx
        .energy_scopes
        .leave(&mut ctx.energy, unsafe { saved.read() })
    else {
        crate::fault("energy.leave of a scope that is not the innermost open one")
    };
    unsafe {
        out.write(UsedOut {
            units: used.units,
            exhausted: u8::from(used.exhausted),
        })
    };
    0
}

/// An exhausted charge's failure begins: the charge has already stored its
/// wrapped allowance, which this replaces before any cleanup charges.
pub(crate) fn exhausted(ctx: &mut TaskCtx) {
    ctx.energy_scopes.exhaust(&mut ctx.energy);
}
