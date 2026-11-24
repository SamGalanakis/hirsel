//! Owned media state is pinned independently of the one-word resource carrier.
use crate::Context;

#[repr(C)]
struct Slot<T> {
    drop: unsafe fn(*mut u8),
    value: T,
}

pub(crate) fn hold<T: Send>(value: T) -> u64 {
    unsafe fn drop_slot<T>(slot: *mut u8) {
        // SAFETY: the owned word came from hold::<T> and is consumed once.
        drop(unsafe { Box::from_raw(slot.cast::<Slot<T>>()) });
    }
    Box::into_raw(Box::new(Slot {
        drop: drop_slot::<T>,
        value,
    })) as u64
}

/// # Safety
/// The live media handle moves to this call and is never used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_media_drop(_: *mut Context, handle: u64) {
    let handle = handle as *mut u8;
    // SAFETY: the first field of the pinned repr(C) slot is its own destructor.
    let drop = unsafe { *handle.cast::<unsafe fn(*mut u8)>() };
    unsafe { drop(handle) };
}
