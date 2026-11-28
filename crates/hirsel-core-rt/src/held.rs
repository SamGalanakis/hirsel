//! PACK-16 erased storage. The owner keeps the producer's resumable cleanup
//! until it has finished every staged child.
use super::ops::ExecutionPin;
use crate::{
    Context,
    block::Block,
    counted::{self, Holder, Worklist},
    tasks::ValueLayout,
};
use std::{
    alloc::Layout,
    mem::MaybeUninit,
    ptr::{self, NonNull},
};

#[repr(C)]
struct Held {
    frame: MaybeUninit<Holder>,
    cursor: u64,
    witness: [u8; 32],
    pool: u64,
    layout: ValueLayout,
    payload: *mut u8,
    charge: u64,
    _pin: Option<ExecutionPin>,
}

#[repr(C)]
pub struct Admission {
    handle: *mut Held,
    available: u64,
}

/// # Safety
/// Source owns an initialized value described by layout. The caller keeps
/// custody on refusal and transfers it when the returned handle is nonnull.
#[unsafe(no_mangle)]
unsafe extern "C" fn hirsel_core_held_new(
    ctx: &Context,
    source: *const u8,
    witness: *const [u8; 32],
    charge: u64,
    layout: &ValueLayout,
) -> Admission {
    let payload = Layout::from_size_align(layout.bytes, layout.align).unwrap();
    let (storage, offset) = Layout::new::<Held>().extend(payload).unwrap();
    let block = match Block::new(ctx, Some(charge), storage.size(), storage.align()) {
        Ok(block) => block,
        Err(c) => {
            return Admission {
                handle: ptr::null_mut(),
                available: c.available,
            };
        }
    };
    let base = block.into_raw();
    let handle = base.cast::<Held>().as_ptr();
    unsafe {
        let payload = base.as_ptr().add(offset);
        ptr::copy_nonoverlapping(source, payload, layout.bytes);
        handle.write(Held {
            frame: MaybeUninit::uninit(),
            cursor: 0,
            witness: *witness,
            pool: ctx
                .execution
                .as_ref()
                .map_or(0, crate::run_stop::Execution::pool),
            layout: *layout,
            payload,
            charge,
            _pin: ExecutionPin::recipe(ctx, layout.clean),
        });
    }
    Admission {
        handle,
        available: 0,
    }
}

unsafe fn free(handle: *mut Held) {
    unsafe {
        let charge = (*handle).charge;
        ptr::drop_in_place(handle);
        drop(Block::from_raw(
            NonNull::new_unchecked(handle.cast()),
            charge,
        ));
    }
}

/// # Safety
/// Handle is an exclusive owner. A matching witness guarantees the caller
/// supplied aligned output for the stored type.
#[unsafe(no_mangle)]
unsafe extern "C" fn hirsel_core_held_take(
    handle: *mut Held,
    witness: *const [u8; 32],
    output: *mut u8,
) -> bool {
    unsafe {
        let pool = crate::run_stop::current_execution().map_or(0, crate::run_stop::Execution::pool);
        if (*handle).pool != pool || (*handle).witness != *witness {
            return false;
        }
        ptr::copy_nonoverlapping((*handle).payload, output, (*handle).layout.bytes);
        free(handle);
    }
    true
}

unsafe fn step(ctx: *mut Context, frame: *mut Holder, list: *mut Worklist) -> bool {
    let handle = frame.cast::<Held>();
    unsafe {
        if let Some(clean) = (*handle).layout.clean
            && !clean(ctx, (*handle).payload, &raw mut (*handle).cursor, list)
        {
            return false;
        }
        free(handle);
    }
    true
}

/// # Safety
/// The caller transfers one live Held owner for ordinary cleanup.
#[unsafe(no_mangle)]
unsafe extern "C" fn hirsel_core_held_drop(ctx: *mut Context, handle: *mut Held) -> u8 {
    unsafe { counted::drain_holder(ctx, handle.cast(), step, 0) as u8 }
}

/// # Safety
/// The active worklist has room for one staged child at this point.
#[unsafe(no_mangle)]
unsafe extern "C" fn hirsel_core_held_release(
    _: *mut Context,
    handle: *mut Held,
    list: *mut Worklist,
    point: u64,
) -> bool {
    unsafe { (*list).stage_holder(handle.cast(), step, point) };
    true
}
