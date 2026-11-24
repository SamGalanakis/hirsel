//! In-flight sync cells tracked on the running pooled unit, so unit teardown
//! detaches any that have not completed.
use super::{cur, untied};

/// Record an in-flight sync cell on the running pooled unit so unit teardown
/// detaches it if the unit ends before completion. Returns `true` if tracked.
pub(crate) fn track_sync(cell: *mut crate::os_sync::SyncCell) -> bool {
    if !untied() {
        return false;
    }
    let u = cur();
    if u.is_null() {
        return false;
    }
    // SAFETY: only the running unit's worker touches its `sync_cells`.
    unsafe { (*u).sync_cells.borrow_mut().push(cell) };
    true
}

/// Remove a completed sync cell from the running pooled unit's tracking list.
pub(crate) fn untrack_sync(cell: *mut crate::os_sync::SyncCell) {
    if !untied() {
        return;
    }
    let u = cur();
    if u.is_null() {
        return;
    }
    // SAFETY: only the running unit's worker touches its `sync_cells`.
    let mut cells = unsafe { (*u).sync_cells.borrow_mut() };
    if let Some(i) = cells.iter().position(|&c| c == cell) {
        cells.swap_remove(i);
    }
}
