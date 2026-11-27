//! PACK-12 fused scope rows: one call that makes two enters, and one that makes
//! a run of leaves. Each performs exactly the rows it stands for, in their
//! order, through those rows' own entries.
use crate::{Context, abi, isolate, steps};

/// The number of enters made (the second is tried only when the first is
/// admitted) and the byte bound's `available` result, when its enter ran.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Entered {
    pub layers: u64,
    pub room: u64,
}

/// `order` 0 enters the byte bound and then the step bound; 1 the step bound
/// and then the byte bound. `site` is the step bound's.
/// # Safety
/// The current task owns context exclusively; its native counter is flushed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_contain_enter(
    context: *mut Context,
    order: u64,
    bytes: u64,
    steps: u64,
    site: u64,
) -> Entered {
    // SAFETY: each entry is called under the contract of its own row.
    unsafe {
        if order == 0 {
            if let Err(capacity) = abi::limit_enter(&mut *context, bytes) {
                return Entered {
                    layers: 0,
                    room: capacity.available,
                };
            }
            let layers = 1 + u64::from(steps::hirsel_core_step_enter_at(context, steps, site));
            Entered { layers, room: 0 }
        } else {
            if !steps::hirsel_core_step_enter_at(context, steps, site) {
                return Entered { layers: 0, room: 0 };
            }
            match abi::limit_enter(&mut *context, bytes) {
                Ok(()) => Entered { layers: 2, room: 0 },
                Err(capacity) => Entered {
                    layers: 1,
                    room: capacity.available,
                },
            }
        }
    }
}

/// Leave the scopes `layers` names, innermost first, four bits each from the
/// low end: 1 a byte bound, 2 a step bound, 3 an isolate.
/// # Safety
/// The current task owns context exclusively and has entered each named scope,
/// the first innermost; its native counter is flushed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_contain_leave(context: *mut Context, layers: u64) {
    // The two complete contained calls have statically known cleanup orders.
    // Specialize the same decoder so each runs straight through its three rows.
    unsafe {
        match layers {
            0x123 => leave_layers(context, 0x123),
            0x213 => leave_layers(context, 0x213),
            _ => leave_layers(context, layers),
        }
    }
}

#[inline(always)]
unsafe fn leave_layers(context: *mut Context, mut layers: u64) {
    while layers != 0 {
        // SAFETY: each entry is called under the contract of its own row.
        unsafe {
            match layers & 15 {
                1 => (*context).limit_exit(),
                2 => steps::leave(&mut *context),
                3 => isolate::hirsel_core_isolate_leave(context),
                layer => panic!("contain leave layer {layer}"),
            }
        }
        layers >>= 4;
    }
}
