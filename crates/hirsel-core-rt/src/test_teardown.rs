//! Test-only store `Db` teardown hook (`DESIGN.md` / store-trust-teardown-3021).
//! Active only in `@test` binaries that register `store@1/store/verify_teardown`.

use crate::{Context, limit::Funding};
use std::{
    cell::Cell,
    sync::{
        Mutex,
        atomic::{AtomicPtr, Ordering},
    },
};

type TeardownFn = unsafe extern "C" fn(*mut Context, *const u8, u64) -> u8;

static TEARDOWN_FN: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());
static OPTED_OUT: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// Register `store@1/store/verify_teardown` at test-process entry.
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_test_db_teardown_register(f: *const ()) {
    TEARDOWN_FN.store(f.cast_mut(), Ordering::Release);
}

/// Mark a `StoreId` (or `(0, 0)` for the whole test process) as opted out of
/// automatic teardown verification.
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_test_db_opt_out(hi: u64, lo: u64) {
    let mut list = OPTED_OUT.lock().unwrap();
    if !list.contains(&(hi, lo)) {
        list.push((hi, lo));
    }
}

/// Query whether `(hi, lo)` (or the whole test process via `(0, 0)`) opted out.
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_test_db_is_opted_out(hi: u64, lo: u64) -> u8 {
    let list = OPTED_OUT.lock().unwrap();
    u8::from(list.contains(&(0, 0)) || list.contains(&(hi, lo)))
}

/// Run the registered `store@1/store/verify_teardown(db)` on a live `Db` during
/// `@test` teardown (`close` or `Db` drop). Runs under an isolated root funding
/// scope and step budget so tight `bound bytes` / step-bound laws are neither
/// starved nor perturbed.
///
/// # Safety
/// `context` is the running task's exclusive context and `db` points to a live
/// `store@1/store/Db` value whose fields have not been dropped yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_db_teardown(
    context: *mut Context,
    db: *const u8,
    site: u64,
) -> u8 {
    let raw = TEARDOWN_FN.load(Ordering::Acquire);
    if raw.is_null() {
        return 0;
    }
    // SAFETY: caller passes the task's exclusive context.
    let ctx = unsafe { &mut *context };
    if !ctx.causes.is_empty() || ctx.cancel_requested.load(Ordering::Relaxed) {
        return 0;
    }
    if ACTIVE.with(|active| active.replace(true)) {
        return 0;
    }
    struct ActiveGuard;
    impl Drop for ActiveGuard {
        fn drop(&mut self) {
            ACTIVE.with(|active| active.set(false));
        }
    }
    let _active_guard = ActiveGuard;

    // Isolate memory and step budgets from the test's inner `bound bytes` /
    // step bounds while preserving the task's primary cause cell.
    let budget = ctx.root_limit.max(128 * 1024 * 1024);
    let teardown_funding = Funding::root(budget);
    let previous_funding = std::mem::replace(&mut ctx.current, teardown_funding);
    ctx.current.bind();
    let previous_steps = ctx.steps;
    let previous_step_bounds = std::mem::take(&mut ctx.step_bounds);
    ctx.steps = i64::MAX / 2;
    ctx.cancel_shield += 1;

    // SAFETY: `TEARDOWN_FN` was registered from `store@1/store/verify_teardown`
    // whose ABI is `fn(*mut Context, *const Db, u64) -> u8`.
    let teardown: TeardownFn = unsafe { std::mem::transmute(raw) };
    let status = unsafe { teardown(context, db, site) };

    ctx.cancel_shield -= 1;
    ctx.step_bounds = previous_step_bounds;
    ctx.steps = previous_steps;
    let used_funding = std::mem::replace(&mut ctx.current, previous_funding);
    ctx.current.bind();
    drop(used_funding);
    status
}
