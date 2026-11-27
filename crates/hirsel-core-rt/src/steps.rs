//! MEM-30 step budgets and slice counters belong to one task.
use crate::{Context, causes};
use hirsel_core_ids::work_steps::Work;
use std::sync::atomic::{AtomicU64, Ordering};

pub const UNBOUNDED: i64 = 1 << 62;
/// EMB-3: in contained/embed mode, tasks without an explicit `bound steps`
/// initialize their countdown to 65,536 so pure loops observe generation end
/// without emitting any scheduling event or ending a slice.
pub const EMBED_QUANTUM: i64 = 65_536;
pub const SLICE: u64 = crate::abi_layout::STEP_SLICE as u64;
static NEXT: AtomicU64 = AtomicU64::new(1);

#[inline(always)]
pub(crate) fn initial_unbounded_steps() -> i64 {
    if crate::run_stop::is_contained() {
        EMBED_QUANTUM
    } else {
        UNBOUNDED
    }
}

pub(crate) struct Bound {
    id: u64,
    left: u64,
}

#[cfg(feature = "testing")]
pub(crate) struct Observation {
    pub(crate) inject: Option<u64>,
    window: Option<u64>,
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
        (self.spent(window_left(context)), self.refusal)
    }

    fn spent(&self, window_left: u64) -> u64 {
        self.steps
            .saturating_add(self.window.map_or(0, |start| start - window_left))
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

fn bound(left: u64) -> Bound {
    let id = NEXT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .unwrap_or_else(|_| crate::run_stop::raise(crate::run_stop::Stop::from_static(b"StepIds")));
    Bound { id, left }
}

// The active bound and slice hold the tails beyond the emitter's common window.
// Switching tasks leaves all three words intact.
fn window_left(context: &Context) -> u64 {
    context.steps.max(0) as u64
}

fn install_window(context: &mut Context, slice: u64) {
    #[cfg(feature = "testing")]
    {
        let left = window_left(context);
        if let Some(obs) = &mut context.step_observation {
            obs.steps = obs.spent(left);
        }
    }
    if let Some(current) = context.step_bounds.last_mut() {
        let window = current.left.min(slice);
        current.left -= window;
        context.step_slice_tail = slice - window;
        context.steps = window as i64;
    } else {
        context.step_slice_tail = slice;
        context.steps = initial_unbounded_steps();
    }
    #[cfg(feature = "testing")]
    if let Some(obs) = &mut context.step_observation {
        obs.window = (!context.step_bounds.is_empty()).then_some(context.steps as u64);
    }
}

pub(crate) fn inherit(child: &mut Context, parent: &Context) {
    install_inherited(child, remaining(parent));
}

pub(crate) fn remaining(context: &Context) -> Option<u64> {
    context
        .step_bounds
        .last()
        .map(|current| current.left + window_left(context))
}

pub(crate) fn install_inherited(child: &mut Context, remaining: Option<u64>) {
    if let Some(remaining) = remaining {
        child.step_bounds.push(bound(remaining));
        install_window(child, SLICE);
    }
}

pub(crate) fn is_active(context: &Context, id: u64) -> bool {
    context.step_bounds.iter().any(|bound| bound.id == id)
}

fn leave(context: &mut Context) {
    let window = window_left(context);
    #[cfg(feature = "prof")]
    if let Some(left) = remaining(context) {
        crate::mem_prof::step_remaining(left);
        crate::mem_prof::scope_exit(1);
    }
    let left = context.step_bounds.pop().expect("an open step scope").left + window;
    if let Some(parent) = context.step_bounds.last_mut() {
        parent.left += left;
    }
    let slice = context.step_slice_tail + window;
    install_window(context, slice);
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
    let window = window_left(context);
    let mut slice = context.step_slice_tail;
    if let Some(current) = context.step_bounds.last_mut() {
        let left = current.left + window;
        if left < steps {
            #[cfg(feature = "testing")]
            if let Some(obs) = &mut context.step_observation {
                obs.refusal.get_or_insert(_site);
            }
            #[cfg(feature = "prof")]
            {
                crate::mem_prof::step_remaining(left);
                crate::mem_prof::scope_enter(1, 0, steps, true);
            }
            return false;
        }
        current.left = left - steps;
        #[cfg(feature = "prof")]
        crate::mem_prof::step_remaining(current.left);
        slice += window;
    }
    context.step_bounds.push(bound(steps));
    install_window(context, slice);
    #[cfg(feature = "prof")]
    crate::mem_prof::scope_enter(1, 0, steps, false);
    true
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
    let id = context.step_bounds.last().expect("an open step scope").id;
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
        if crate::run_stop::is_ended() {
            crate::run_stop::leave_on_ended();
        }
        ctx.steps = initial_unbounded_steps();
        return 0;
    }
    if crate::run_stop::is_ended() {
        crate::run_stop::leave_on_ended();
    }
    if ctx.steps < 0
        && let Some(current) = ctx.step_bounds.last()
    {
        #[cfg(feature = "testing")]
        if let Some(obs) = &mut ctx.step_observation {
            obs.refusal.get_or_insert(site);
        }
        #[cfg(feature = "prof")]
        crate::mem_prof::step_refused();
        ctx.steps = 0;
        let mut cause = causes::Cause::new(causes::OUT_OF_TIME, site);
        cause.message.data = std::ptr::without_provenance_mut(current.id as usize);
        // SAFETY: the failed step owns the active obligation's primary cell.
        unsafe { causes::record(ctx, cause) };
        return 1;
    }
    let slice_end = ctx.step_slice_tail == 0;
    let slice = if slice_end {
        SLICE
    } else {
        ctx.step_slice_tail
    };
    install_window(ctx, slice);
    if slice_end {
        crate::tasks::before_commit();
        crate::tasks::event(hirsel_core_layout::event::STEP_SLICE, 0, SLICE);
        // SAFETY: the step window is installed and no borrow spans the switch.
        unsafe { crate::tasks::step_checkpoint() };
    }
    0
}
