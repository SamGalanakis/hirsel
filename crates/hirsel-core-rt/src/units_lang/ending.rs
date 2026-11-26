//! A helper's end, as its owner takes it.
use super::{Context, Ended, HelperH, LangUnit, charge, consume, helper_of, tasks, wait_end};
#[cfg(feature = "record")]
use super::{takes, unit};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Each ended helper's outcome, by its port, until its owner joins.
pub(super) static RESULTS: Mutex<Option<HashMap<usize, Ended>>> = Mutex::new(None);
pub(super) trait ResultsExt {
    fn insert(&mut self, k: usize, e: Ended);
    fn take(&mut self, k: usize) -> Option<Ended>;
}
impl ResultsExt for std::sync::MutexGuard<'_, Option<HashMap<usize, Ended>>> {
    fn insert(&mut self, k: usize, e: Ended) {
        self.get_or_insert_with(HashMap::new).insert(k, e);
    }
    fn take(&mut self, k: usize) -> Option<Ended> {
        self.as_mut().and_then(|m| m.remove(&k))
    }
}

/// A helper's end take in its owner: the survivors' charges move into the
/// owner's retained charges under their connections' keys. Run grants remain
/// charged through the owner's end. The result size, its content and failure
/// cells stay for the join, and the rest comes back now.
/// # Safety
/// The owner runs the take in a commit; the handle is live.
pub(super) unsafe fn end_take(u: &mut LangUnit, h: &mut HelperH) {
    let results = RESULTS.lock().unwrap();
    let ended = results
        .as_ref()
        .and_then(|m| m.get(&(Arc::as_ptr(&h.port) as usize)))
        .unwrap_or_else(|| crate::process::stop(b"UnitLedger"));
    let held = h.charge.as_mut().unwrap();
    // The charges taken over are the survivors the take lists: one still held
    // at the hand-over may have been released since. A refund or `RETIRED`
    // of one folds after this take.
    let taken = crate::secret::take_over(&ended.charges, |bytes| held.partition(bytes));
    #[cfg(feature = "record")]
    // SAFETY: by this function's contract.
    unsafe {
        takes::end_take(u, h.k, ended, &taken, h.files)
    };
    #[cfg(not(feature = "record"))]
    let _ = taken;
    for &(key, bytes) in &ended.survivors {
        u.retained.insert(key, held.split(bytes));
    }
    #[cfg(feature = "record")]
    if !ended.replay_survivors.is_empty() {
        use hirsel_tape_format::SurvivorKind::{Connection, GroupQueue, Shared, UdpPool};
        let mut named = Vec::new();
        for s in &ended.replay_survivors {
            let payer = held.partition(s.amount);
            if matches!(s.name.kind, Connection | Shared | UdpPool | GroupQueue) {
                named.push((s.name, payer, s.amount));
            }
        }
        if !named.is_empty() {
            crate::tape::units::replay::keep_survivors(u.tape(), named);
        }
    }
    if ended.run_grants != 0 {
        u.run_grants.push(held.split(ended.run_grants));
    }
    let causes = ended
        .causes
        .iter()
        .map(|cause| charge::CAUSE_CELL + cause.message_charge)
        .sum::<u64>();
    let keep = h.r_size + ended.result + causes;
    drop(held.split(held.bytes.saturating_sub(keep)));
}

/// `task.cancel` on a helper's handle: its `UNIT_CANCEL`, then the request.
pub(crate) unsafe fn cancel(task: *mut tasks::Task) {
    #[cfg(feature = "record")]
    // SAFETY: the owner's handle pins the helper.
    unsafe {
        let (group, k) = (tasks::current_group(), (*helper_of(task)).k);
        crate::tape::units::cancel(group, k);
        crate::tape::units::replay::cancel(group, k);
    }
    // SAFETY: forwarded.
    unsafe { request_cancel(task) }
}

/// Ask a helper to cancel: an arrival at its port, taken in its own order.
unsafe fn request_cancel(task: *mut tasks::Task) {
    let h = helper_of(task);
    // SAFETY: the owner's handle pins the helper's port.
    unsafe {
        // A replayed helper starts nothing: its own stream holds its cancel.
        #[cfg(feature = "record")]
        if unit().unstarted.contains_key(&(*h).k) {
            return;
        }
        if !(*h).ended {
            (*h).port.publish(crate::units::ledger::ArrivalRef::boxed(
                crate::units::ledger::Body::Cancel,
            ));
        }
    }
}

/// Drop a helper's handle: cancel, wait for its end (shielded), drop the result.
pub(crate) unsafe fn abandon(context: *mut Context, task: *mut tasks::Task) -> u8 {
    // SAFETY: the caller transfers the handle.
    unsafe {
        request_cancel(task);
        wait_end::<false>(context, helper_of(task), 0, true, false);
        consume(context, helper_of(task), None);
        0
    }
}
