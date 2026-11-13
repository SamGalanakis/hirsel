//! Cancellation is observed at operation entry and before blocking, never by a
//! pure-code poll. The scheduler sets requests only in its event order.
use crate::{
    Context,
    causes::{self, Cause},
};
/// # Safety
/// Context is exclusively borrowed by its running task; site is emitter-bound.
pub unsafe fn check_cancel(context: *mut Context, site: u64) -> u8 {
    // SAFETY: exclusive task context, and a pending failure consumes its primary once.
    unsafe {
        if (*context).finalizing == 0 && (*context).cancel_requested {
            (*context).cancel_requested = false;
            causes::record(
                &mut *context,
                Cause {
                    kind: causes::CANCELLED,
                    site,
                    message: std::ptr::null_mut(),
                    drop_message: None,
                },
            );
            1
        } else {
            0
        }
    }
}
/// # Safety
/// Context is the currently running task and its primary cell is available.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_check_cancel(context: *mut Context, site: u64) -> u8 {
    // SAFETY: forwarded emitter context and source site.
    unsafe { check_cancel(context, site) }
}
