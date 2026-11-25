//! A helper's end, as its owner takes it.
#[cfg(feature = "record")]
use super::takes;
use super::{HelperH, LangUnit, RESULTS, charge};
use std::sync::Arc;

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
    #[cfg(feature = "record")]
    // SAFETY: by this function's contract.
    unsafe {
        takes::end_take(u, h.k, ended, h.files)
    };
    let held = h.charge.as_mut().unwrap();
    for &(key, bytes) in &ended.survivors {
        u.retained.insert(key, held.split(bytes));
    }
    if ended.run_grants != 0 {
        u.run_grants.push(held.split(ended.run_grants));
    }
    crate::secret::take_over(&ended.charges, |bytes| held.partition(bytes));
    let causes = ended
        .causes
        .iter()
        .map(|cause| charge::CAUSE_CELL + cause.message_charge)
        .sum::<u64>();
    let keep = h.r_size + ended.result + causes;
    drop(held.split(held.bytes.saturating_sub(keep)));
}
