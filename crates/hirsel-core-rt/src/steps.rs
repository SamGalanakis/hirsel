//! MEM-30 deterministic, shared step scopes. Only the running task caches a window.
use crate::{Context, causes};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

pub const UNBOUNDED: i64 = 1 << 62;
pub const SLICE: u64 = 65_536;
static NEXT: AtomicU64 = AtomicU64::new(1);

pub(crate) struct Bound {
    left: Mutex<u64>,
    parent: Option<Arc<Bound>>,
    id: u64,
}
impl Drop for Bound {
    fn drop(&mut self) {
        let mut parent = self.parent.take();
        let mut left = *self.left.get_mut().unwrap();
        while let Some(record) = parent {
            *record.left.lock().unwrap() += left;
            match Arc::try_unwrap(record) {
                Ok(mut record) => {
                    left = *record.left.get_mut().unwrap();
                    parent = record.parent.take();
                }
                Err(_) => break,
            }
        }
    }
}

pub(crate) fn is_active(context: &Context, id: u64) -> bool {
    context.step_bounds.iter().any(|bound| bound.id == id)
}

pub(crate) fn settle(context: &mut Context) {
    if context.step_loaded != 0 {
        let used = context.step_loaded - context.steps.max(0) as u64;
        *context.step_bounds.last().unwrap().left.lock().unwrap() -= used;
        context.step_slice += used;
        context.step_loaded = 0;
    }
}
pub(crate) fn resume(context: &mut Context) {
    if let Some(bound) = context.step_bounds.last() {
        let left = *bound.left.lock().unwrap();
        let window = left.min(SLICE - context.step_slice);
        context.step_loaded = window;
        context.steps = window as i64;
    } else {
        context.step_loaded = 0;
        context.steps = UNBOUNDED;
    }
}

/// # Safety
/// The current task owns context exclusively; its native counter is flushed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_step_enter(context: *mut Context, steps: u64) -> bool {
    // SAFETY: the caller provides its live exclusive task context.
    let context = unsafe { &mut *context };
    settle(context);
    let parent = context.step_bounds.last().cloned();
    if let Some(parent) = &parent {
        let mut left = parent.left.lock().unwrap();
        if *left < steps {
            drop(left);
            resume(context);
            return false;
        }
        *left -= steps;
    }
    let id = NEXT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .unwrap_or_else(|_| crate::process::stop(b"StepIds"));
    context.step_bounds.push(Arc::new(Bound {
        left: Mutex::new(steps),
        parent,
        id,
    }));
    resume(context);
    true
}

/// # Safety
/// The current task is closing its innermost lexical step scope.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_step_leave(context: *mut Context) {
    // SAFETY: the caller owns this task and an entered scope.
    let context = unsafe { &mut *context };
    settle(context);
    context.step_bounds.pop().expect("an open step scope");
    resume(context);
}

/// # Safety
/// Cleanup finished under the current exhausted scope; its counter is flushed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_step_catch(context: *mut Context) -> bool {
    // SAFETY: the caller owns this task and an entered scope.
    let context = unsafe { &mut *context };
    settle(context);
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
    context.step_bounds.pop();
    resume(context);
    caught
}

/// # Safety
/// Called immediately after a source Step decremented the flushed window to <= 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_step_out(context: *mut Context, site: u64) -> u8 {
    // SAFETY: the engine supplies the exclusively running task's context.
    unsafe {
        let attempted = (*context).steps < 0;
        settle(&mut *context);
        if attempted {
            if let Some(bound) = (*context).step_bounds.last() {
                let mut cause = causes::Cause::new(causes::OUT_OF_TIME, site);
                cause.message.data = std::ptr::without_provenance_mut(bound.id as usize);
                causes::record(&mut *context, cause);
                return 1;
            }
        }
        if (*context).step_slice == SLICE {
            (*context).step_slice = 0;
            crate::tasks::checkpoint(false);
        }
        resume(&mut *context);
        0
    }
}
