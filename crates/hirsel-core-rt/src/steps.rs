//! MEM-30 step accounts and per-task slice counters.
//!
//! `bound steps n` opens an account funded with n from the current one. A
//! same-unit spawn makes the child a participant of the spawner's innermost
//! account by identity (tasks.md CONC-10/15): no fuel moves. CONC-10 runs one
//! task of a unit at a time, so one fiber spends an account at a time. A
//! running fiber holds a prepaid window, `Context::steps`, of at most its slice
//! remainder and the account's available credit; it returns the unspent part at
//! every switch to idle, lexical bound switch and end, and draws again at its
//! next charged step. No participant can spend credit another holds, and none
//! overshoots by a step.
//!
//! An account closes when its last interest goes: the entering task's lexical
//! exit, each participant's end, each nested account's close and each helper
//! grant's settlement. On close its unused credit goes once to its funder. A
//! helper (`spawn_unit`) gets a private root of floor(R / 2) of the donor's
//! available R; the donor's unused-credit refund waits for the owner to consume
//! or abandon the handle (`settle`).
//!
//! The first refused step latches the account exhausted. Late credit still
//! accumulates for settlement at close, but cannot fund another step.
use crate::{Context, causes};
use hirsel_core_ids::work_steps::Work;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

mod account;
pub(crate) use account::{Account, Bound, Bounds};
use account::{Balance, promote, release};

pub const UNBOUNDED: i64 = 1 << 62;
/// EMB-3: in contained/embed mode, tasks without an explicit `bound steps`
/// initialize their countdown to 65,536 so pure loops observe generation end
/// without emitting any scheduling event or ending a slice.
pub const EMBED_QUANTUM: i64 = 65_536;
pub const SLICE: u64 = crate::abi_layout::STEP_SLICE as u64;
/// The first id no task has taken. Ids are nonzero and never reused in a
/// process: a caught `OutOfTime` names its account by id (`is_active`).
static NEXT: AtomicU64 = AtomicU64::new(1);
/// The ids one task takes from [`NEXT`] at a time.
const ID_RANGE: u64 = 64;

/// A task's unspent account ids: `next..end`. A new task holds none and takes
/// its first range at its first `bound steps`, so opening an account writes no
/// line another worker reads.
#[derive(Default)]
pub(crate) struct Ids {
    next: u64,
    end: u64,
}

impl Ids {
    fn take(&mut self) -> u64 {
        if self.next == self.end {
            self.next = NEXT
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
                    id.checked_add(ID_RANGE)
                })
                .unwrap_or_else(|_| {
                    crate::run_stop::raise(crate::run_stop::Stop::from_static(b"StepIds"))
                });
            self.end = self.next + ID_RANGE;
        }
        let id = self.next;
        self.next += 1;
        id
    }
}

#[inline(always)]
pub(crate) fn initial_unbounded_steps() -> i64 {
    if crate::run_stop::is_contained() {
        EMBED_QUANTUM
    } else {
        UNBOUNDED
    }
}

/// A native fixture's view of the steps its context spends and its first
/// refusal site.
#[cfg(feature = "testing")]
pub(crate) struct Observation {
    pub(crate) inject: Option<u64>,
    /// `Context::steps` when the current window was drawn.
    window: Option<i64>,
    steps: u64,
    refusal: Option<u64>,
}

#[cfg(feature = "testing")]
impl Observation {
    pub(crate) fn new(inject: Option<u64>) -> Self {
        Self {
            inject,
            window: None,
            steps: 0,
            refusal: None,
        }
    }

    pub(crate) fn report(&self, context: &Context) -> (u64, Option<u64>) {
        (self.spent(context.steps), self.refusal)
    }

    /// A step that outran its window and was paid by the next draw counts.
    fn spent(&self, steps: i64) -> u64 {
        let open = self.window.map_or(0, |start| (start - steps).max(0) as u64);
        self.steps.saturating_add(open)
    }
}

/// Start observing this context before a native fixture enters its bound.
///
/// # Safety
/// The fixture owns an exclusive live context.
#[cfg(feature = "testing")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_step_observe(
    context: *mut Context,
    inject: bool,
    limit: u64,
) {
    unsafe { (*context).step_observation = Some(Observation::new(inject.then_some(limit))) };
}

/// Read the steps and first refusal of a native fixture's context.
///
/// # Safety
/// The fixture owns the live context and a writable refusal-site output.
#[cfg(feature = "testing")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_step_report(
    context: *const Context,
    refusal: *mut u64,
) -> u64 {
    let context = unsafe { &*context };
    let (steps, first) = context.step_observation.as_ref().unwrap().report(context);
    unsafe { refusal.write(first.unwrap_or(0)) };
    steps
}

fn window_left(context: &Context) -> u64 {
    context.steps.max(0) as u64
}

/// Change the window, keeping a fixture's observation of what it spent.
#[inline(always)]
fn rewindow(context: &mut Context, change: impl FnOnce(&mut Context)) {
    #[cfg(feature = "testing")]
    if let Some(obs) = &mut context.step_observation {
        obs.steps = obs.spent(context.steps);
        obs.window = None;
    }
    change(context);
    #[cfg(feature = "testing")]
    if let Some(obs) = &mut context.step_observation {
        obs.window = (!context.step_bounds.is_empty()).then_some(context.steps);
    }
}

/// Return the unspent window to the innermost account, keeping slice phase.
fn flush(context: &mut Context) {
    if context.steps < 0 {
        return;
    }
    rewindow(context, |context| {
        let window = window_left(context);
        if let Some(current) = context.step_bounds.last_mut() {
            current.with_balance(|b| b.left += window);
            context.step_slice_tail += window;
            context.steps -= window as i64;
        }
    });
}

/// Lend the running fiber the rest of its slice the innermost account funds.
fn draw(context: &mut Context) -> u64 {
    let current = context.step_bounds.last_mut().expect("a step account");
    let window = current.with_balance(|b| {
        let window = b.available().min(context.step_slice_tail);
        b.left -= window;
        window
    });
    if window != 0 {
        rewindow(context, |context| {
            context.step_slice_tail -= window;
            context.steps += window as i64;
        });
    }
    window
}

/// The fiber is leaving the CPU: it holds no credit while it does not run.
pub(crate) fn suspend(context: &mut Context) {
    if matches!(context.step_bounds.last(), Some(Bound::Shared(_))) {
        flush(context);
    }
}

pub(crate) fn remaining(context: &Context) -> Option<u64> {
    context
        .step_bounds
        .last()
        .map(|current| current.available() + window_left(context))
}

/// A same-unit child participates in the spawner's innermost account.
pub(crate) fn share(child: &mut Context, parent: &mut Context) {
    if let Some(account) = promote(&mut parent.step_bounds) {
        account.participate();
        child.step_bounds.push(Bound::Shared(account));
        child.step_slice_tail = SLICE;
        child.steps = 0;
    }
}

/// A flow stage's private copy of the wiring task's remaining steps (MEM-30).
pub(crate) fn copy(stage: &mut Context, parent: &Context) {
    if let Some(left) = remaining(parent) {
        stage.step_slice_tail = SLICE;
        open_inline(stage, left);
    }
}

/// A helper's grant: floor(R / 2) of the spawner's available R, and the donor
/// account the handle keeps open until it settles.
pub(crate) fn grant(context: &mut Context) -> Option<(Arc<Account>, u64)> {
    flush(context);
    let donor = promote(&mut context.step_bounds)?;
    let grant = {
        let mut s = donor.state();
        let grant = s.balance.available() / 2;
        s.balance.left -= grant;
        s.interest += 1;
        grant
    };
    draw(context);
    Some((donor, grant))
}

/// A helper's root context starts with its private grant, the account
/// returned to read its residual once the helper's unit is done.
pub(crate) fn install_grant(context: &mut Context, grant: Option<u64>) -> Option<Arc<Account>> {
    let left = grant?;
    let account = Account::new(context.step_ids.take(), Balance::new(left), None);
    context.step_bounds.push(Bound::Shared(account.clone()));
    context.step_slice_tail = SLICE;
    context.steps = 0;
    draw(context);
    Some(account)
}

/// A helper root's unused credit once every participant has ended; credit
/// a participant still holds is not returned.
pub(crate) fn residual(root: &Account) -> u64 {
    let s = root.state();
    if s.interest == 0 { s.balance.left } else { 0 }
}

/// The owner consumed or abandoned a helper's handle: its residual goes to the
/// donor once, and the handle's interest goes.
pub(crate) fn settle(donor: Arc<Account>, residual: u64) {
    donor.state().balance.left += residual;
    release(donor);
}

/// A task ended: its interest in each account still on its stack.
pub(crate) fn end(context: &mut Context) {
    flush(context);
    while let Some(bound) = context.step_bounds.pop() {
        bound.close(0, context.step_bounds.last_mut());
    }
    context.steps = initial_unbounded_steps();
}

/// Whether two tasks hold a common account.
#[cfg(feature = "record")]
pub(crate) fn shares(a: &Context, b: &Context) -> bool {
    a.step_bounds
        .iter()
        .any(|x| b.step_bounds.iter().any(|y| x.id() == y.id()))
}

pub(crate) fn is_active(context: &Context, id: u64) -> bool {
    context.step_bounds.iter().any(|bound| bound.id() == id)
}

pub(crate) fn leave(context: &mut Context) {
    #[cfg(feature = "prof")]
    if let Some(left) = remaining(context) {
        crate::mem_prof::step_remaining(left);
        crate::mem_prof::scope_exit(1);
    }
    let window = window_left(context);
    rewindow(context, |context| {
        context.step_slice_tail += window;
        if matches!(context.step_bounds, Bounds::One(Bound::Inline { .. })) {
            // The unpublished root has neither a funder nor another participant.
            context.step_bounds = Bounds::Empty;
        } else {
            context.steps -= window as i64;
            let bound = context.step_bounds.pop().expect("an open step scope");
            bound.close(window, context.step_bounds.last_mut());
        }
    });
    if context.step_bounds.is_empty() {
        context.steps = initial_unbounded_steps();
    } else {
        draw(context);
    }
}

/// # Safety
/// The current task owns context exclusively; its native counter is flushed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_step_enter(context: *mut Context, steps: u64) -> bool {
    unsafe { hirsel_core_step_enter_at(context, steps, 0) }
}

/// # Safety
/// The current task owns context exclusively; its native counter is flushed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_step_enter_at(
    context: *mut Context,
    steps: u64,
    _site: u64,
) -> bool {
    // SAFETY: the caller provides its live exclusive task context.
    let context = unsafe { &mut *context };
    #[cfg(feature = "testing")]
    let steps = context
        .step_observation
        .as_mut()
        .and_then(|obs| obs.inject.take())
        .unwrap_or(steps);
    if !context.step_bounds.is_empty() && !fund_nested(context, steps, _site) {
        return false;
    }
    open_inline(context, steps);
    #[cfg(feature = "prof")]
    crate::mem_prof::scope_enter(1, 0, steps, false);
    true
}

/// Debit the parent before opening the inner account. The outermost entry
/// does not need the shared-account locking or refusal paths.
#[inline(never)]
fn fund_nested(context: &mut Context, steps: u64, _site: u64) -> bool {
    flush(context);
    let current = context.step_bounds.last_mut().unwrap();
    let admitted = current.with_balance(|b| {
        if b.available() < steps {
            #[cfg(feature = "prof")]
            crate::mem_prof::step_remaining(b.available());
            return false;
        }
        b.left -= steps;
        #[cfg(feature = "prof")]
        crate::mem_prof::step_remaining(b.left);
        true
    });
    if !admitted {
        #[cfg(feature = "testing")]
        if let Some(obs) = &mut context.step_observation {
            obs.refusal.get_or_insert(_site);
        }
        #[cfg(feature = "prof")]
        crate::mem_prof::scope_enter(1, 0, steps, true);
        draw(context);
        return false;
    }
    true
}

/// Install the first window directly. Only a published account needs a lock.
fn open_inline(context: &mut Context, steps: u64) {
    let window = steps.min(context.step_slice_tail);
    rewindow(context, |context| {
        context.step_bounds.push(Bound::Inline {
            id: context.step_ids.take(),
            balance: Balance::new(steps - window),
        });
        context.step_slice_tail -= window;
        context.steps = window as i64;
    });
}

/// # Safety
/// The current task is closing its innermost lexical step scope.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_step_leave(context: *mut Context) {
    // SAFETY: the caller owns this task and an entered scope.
    leave(unsafe { &mut *context });
}

/// # Safety
/// Cleanup finished under the current exhausted scope; its counter is flushed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_step_catch(context: *mut Context) -> bool {
    // SAFETY: the caller owns this task and an entered scope.
    let context = unsafe { &mut *context };
    let id = context.step_bounds.last().expect("an open step scope").id();
    let caught = context.causes.iter().next().is_some()
        && context
            .causes
            .iter()
            .all(|c| c.kind() == causes::OUT_OF_TIME && c.message.data.addr() as u64 == id);
    if caught {
        let mut chain = std::mem::take(&mut context.causes);
        let obligation = if context.active_resource.is_null() {
            &mut context.primary
        } else {
            // SAFETY: an active resource is the live current finalizer's obligation.
            unsafe { &mut (*context.active_resource).cell }
        };
        if obligation.is_none() {
            let mut cell = chain.take_first().expect("a caught chain's primary cell");
            cell.cause = causes::Cause::default();
            *obligation = Some(cell);
        }
        drop(chain);
    }
    leave(context);
    caught
}

/// Charge work at its source call, including slice boundaries.
/// An unbounded task pays no additional steps.
pub(crate) unsafe fn take_many(context: *mut Context, mut count: u64, site: u64) -> bool {
    if unsafe { (*context).step_bounds.is_empty() } {
        return true;
    }
    while count != 0 {
        let n = count.min(unsafe { window_left(&*context) }.max(1));
        count -= n;
        unsafe {
            (*context).steps -= n as i64;
            if (*context).steps <= 0 && hirsel_core_step_out(context, site) != 0 {
                return false;
            }
        }
    }
    true
}

/// Charge a work-table row before carrying out its operation.
pub(crate) unsafe fn take_work(
    context: *mut Context,
    work: Work,
    units: &[u64],
    site: u64,
) -> bool {
    if unsafe { (*context).step_bounds.is_empty() } {
        return true;
    }
    let check = Work::check_ceiling();
    let before = check.then(|| remaining(unsafe { &*context })).flatten();
    let count = work.steps_sum(units);
    if !unsafe { take_many(context, count, site) } {
        return false;
    }
    if let Some(before) = before {
        let after = remaining(unsafe { &*context }).expect("work keeps its step bound");
        work.assert_ceiling(units, before - after);
    }
    true
}

/// # Safety
/// The current task owns the context and has flushed its native step counter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_work_steps(
    context: *mut Context,
    units: u64,
    shift: u64,
    weight: u64,
    site: u64,
) -> u8 {
    u8::from(!unsafe { take_work(context, Work::from_operands(shift, weight), &[units], site) })
}

/// # Safety
/// Called immediately after a source Step decremented the flushed window to <= 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_step_out(context: *mut Context, site: u64) -> u8 {
    // SAFETY: the engine supplies the exclusively running task's context.
    let ctx = unsafe { &mut *context };
    if ctx.step_bounds.is_empty() {
        // EMB-3: unbound tasks in the embed slot check generation end here and
        // reload 65,536 without emitting any scheduling event or ending a slice.
        if ctx.execution_ended() {
            crate::run_stop::leave_on_ended();
        }
        ctx.steps = initial_unbounded_steps();
        return 0;
    }
    if ctx.execution_ended() {
        crate::run_stop::leave_on_ended();
    }
    // The window is spent (0) or a step outran it (< 0): end a full slice,
    // then draw for any unpaid step and the rest of the slice.
    while ctx.steps <= 0 {
        if ctx.step_slice_tail == 0 {
            ctx.step_slice_tail = SLICE;
            crate::tasks::before_commit();
            crate::tasks::event(hirsel_core_layout::event::STEP_SLICE, 0, SLICE);
            // SAFETY: the window is flushed at the switch; no borrow spans it.
            unsafe { crate::tasks::step_checkpoint() };
            continue;
        }
        if draw(ctx) != 0 {
            continue;
        }
        if ctx.steps == 0 {
            // Nothing left to lend: the next step is refused here.
            return 0;
        }
        #[cfg(feature = "testing")]
        if let Some(obs) = &mut ctx.step_observation {
            obs.refusal.get_or_insert(site);
        }
        let current = ctx.step_bounds.last_mut().unwrap();
        current.with_balance(|b| b.exhausted = true);
        let id = current.id();
        #[cfg(feature = "prof")]
        crate::mem_prof::step_refused();
        ctx.steps = 0;
        let mut cause = causes::Cause::new(causes::OUT_OF_TIME, site);
        cause.message.data = std::ptr::without_provenance_mut(id as usize);
        // SAFETY: the failed step owns the active obligation's primary cell.
        unsafe { causes::record(ctx, cause) };
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_spawn_refunds_only_after_the_last_participant() {
        let mut parent = Context::new(4096);
        let mut child = Context::new(4096);
        // SAFETY: each charged step runs with exclusive access to its task.
        unsafe {
            assert!(hirsel_core_step_enter(&mut parent, 100));
            assert!(take_many(&mut parent, 7, 1));
            assert!(hirsel_core_step_enter(&mut parent, 60));
            assert!(take_many(&mut parent, 9, 2));
            let id = parent.step_bounds.last().unwrap().id();
            share(&mut child, &mut parent);
            assert!(is_active(&child, id));
            suspend(&mut parent);
            assert!(take_many(&mut child, 11, 3));
            suspend(&mut child);
            leave(&mut parent);
            assert_eq!(remaining(&parent), Some(33));
            let (donor, half) = grant(&mut child).unwrap();
            assert_eq!(half, 20);
            end(&mut child);
            assert_eq!(remaining(&parent), Some(33));
            settle(donor, half - 3);
            assert_eq!(remaining(&parent), Some(70));
        }
    }

    /// A caught `OutOfTime` names its account by id, so two accounts that are
    /// ever open in one process never share one, whichever tasks opened them and
    /// however many ranges those tasks went through.
    #[test]
    fn accounts_opened_by_concurrent_tasks_have_distinct_ids() {
        let per_task = 2 * ID_RANGE as usize + 3;
        let tasks: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(move || {
                    let mut context = Context::new(1 << 16);
                    (0..per_task)
                        .map(|_| {
                            // SAFETY: this thread owns the context.
                            unsafe {
                                assert!(hirsel_core_step_enter(&mut context, 1));
                                let id = context.step_bounds.last().unwrap().id();
                                hirsel_core_step_leave(&mut context);
                                id
                            }
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut ids: Vec<u64> = tasks
            .into_iter()
            .flat_map(|task| task.join().unwrap())
            .collect();
        assert!(ids.iter().all(|id| *id != 0));
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4 * per_task);
    }
}
