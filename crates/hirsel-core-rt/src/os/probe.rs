//! Test-only: when a test switches it on, the OS provider's `wait`, `issue` and
//! `deliver` note each call, so a test can compare how they were called.
use super::{Outcome, Request};
use std::cell::RefCell;
pub(crate) const WAIT: u8 = 0;
pub(crate) const ISSUE: u8 = 1;
pub(crate) const DELIVER: u8 = 2;
/// One callback: which, the request's operation and site, whether a task
/// was current, and its answer (a wait's kind; an outcome's status, error
/// and count written).
pub(crate) type Call = (u8, u8, u64, bool, u64);
thread_local! { pub(crate) static CALLS: RefCell<Option<Vec<Call>>> = const { RefCell::new(None) }; }
pub(crate) fn answer(outcome: &Outcome) -> u64 {
    u64::from(outcome.status)
        | u64::from(outcome.error.tag) << 8
        | u64::from(outcome.error.code) << 16
        | outcome.written << 48
}
pub(crate) fn note(callback: u8, request: *const Request, answer: u64) {
    record(callback, request, answer, crate::tasks::in_task());
}
/// A note for a `wait` a direct call form runs in place of the callback,
/// which the scheduler always calls with no task current.
pub(crate) fn note_outside(callback: u8, request: *const Request, answer: u64) {
    record(callback, request, answer, false);
}
fn record(callback: u8, request: *const Request, answer: u64, in_task: bool) {
    CALLS.with_borrow_mut(|calls| {
        if let Some(calls) = calls {
            // SAFETY: the callback's pinned request.
            let (operation, site) = unsafe { ((*request).operation, (*request).site) };
            calls.push((callback, operation, site, in_task, answer));
        }
    });
}
