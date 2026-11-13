//! Optional root-reservation high water. Compiled out of the shipping runtime.
//! Samples follow successful debits, never deliver refunds or create events.
//! The current scheduler runs one cooperative OS worker; the pinned observation
//! follows all its child tasks. A multi-worker scheduler must carry a run-owned
//! observation and synchronize samples before enabling this reporting archive.
use crate::{Context, limit::Funding};
use std::cell::{Cell, RefCell};

struct Observation {
    root: Funding,
    capacity: u64,
    peak: u64,
}
thread_local! {
    static ACTIVE: RefCell<Option<Observation>> = const { RefCell::new(None) };
    static LAST: Cell<u64> = const { Cell::new(0) };
}
/// Pins the original root even when union-by-rank chooses a different slot.
/// There is no charge, admission, event, or per-allocation bookkeeping record.
pub struct Run(Option<Observation>);
impl Run {
    /// Start an observation of this empty root on the current cooperative worker.
    /// The guard ends before that worker starts an unrelated run. Nested host runs
    /// restore their enclosing observation on return.
    pub fn start(context: &Context) -> Self {
        let capacity = context.available();
        let previous = ACTIVE.with(|active| {
            active.replace(Some(Observation {
                root: context.current.clone(),
                capacity,
                peak: 0,
            }))
        });
        LAST.set(0);
        Self(previous)
    }
}
impl Drop for Run {
    fn drop(&mut self) {
        ACTIVE.with(|active| {
            let observation = active.replace(self.0.take()).unwrap();
            LAST.set(observation.peak);
        });
    }
}
#[inline]
pub(crate) fn sample() {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        if let Some(observation) = active.as_mut() {
            let used = observation.capacity - observation.root.available();
            observation.peak = observation.peak.max(used);
        }
    });
}
/// Observed-emission hook after a successful inline debit. No status, admission,
/// refund delivery or event. Absent from ordinary emission and runtime archives.
#[cfg(feature = "charge-observation")]
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_observe_charge() {
    sample();
}
/// Last completed run's root peak reservation in bytes (MEM-19), including
/// bound, task and finalizer-grant reservations, excluding host/engine memory.
/// Call on the worker that ran hirsel_core_run, after it returns. No status.
/// Available only in the charge-observation archive; an unobserved link fails.
#[cfg(feature = "charge-observation")]
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_peak_charge() -> u64 {
    LAST.get()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peak_counts_reservations_and_keeps_survivor_root_after_rank_merge() {
        let mut context = Context::new(1000);
        let run = Run::start(&context);
        context.limit_enter(600).unwrap();
        context.current.admit(Some(40)).unwrap();
        let survivor = context.current.payer;
        context.limit_enter(100).unwrap();
        context.current.admit(Some(10)).unwrap();
        let nested = context.current.payer;
        context.limit_exit();
        context.limit_exit();
        assert_eq!(context.available(), 950);
        nested.refund(10);
        survivor.refund(40);
        context.current.admit(Some(80)).unwrap();
        context.current.refund(80);
        assert_eq!(context.available(), 1000);
        drop(run);
        assert_eq!(LAST.get(), 600);
    }
    #[test]
    fn refusal_does_not_raise_peak_and_consecutive_runs_reset_it() {
        for (bound, expected) in [(1024, 0), (200, 200), (0, 0)] {
            let mut context = Context::new(1000);
            let run = Run::start(&context);
            if context.limit_enter(bound).is_ok() {
                context.limit_exit();
            }
            drop(run);
            assert_eq!(LAST.get(), expected);
        }
    }
    #[test]
    fn inline_sample_and_nested_host_run_preserve_outer_peak() {
        let mut outer = Context::new(1000);
        let outer_run = Run::start(&outer);
        // SAFETY: models the emitter's task-exclusive frozen-prefix stores.
        unsafe {
            (*outer.limit_record()).available -= 80;
            (*outer.limit_record()).outstanding += 80;
        }
        sample();
        {
            let inner = Context::new(200);
            let inner_run = Run::start(&inner);
            inner.current.admit(Some(100)).unwrap();
            inner.current.refund(100);
            drop(inner_run);
            assert_eq!(LAST.get(), 100);
        }
        outer.current.refund(80);
        outer.limit_enter(50).unwrap();
        outer.limit_exit();
        drop(outer_run);
        assert_eq!(LAST.get(), 80);
    }
}
