//! Native memory ABI, FORM v1. Every symbol and failure convention lives here.
//!
//! `alloc` and `limit_enter` return null plus Capacity on refusal; on success
//! `capacity` is zero. Allocation inside a finalizer must branch to the emitter's
//! Capacity failure path on null. `free`, limit exit and mechanical release never
//! fail. `alloc like` follows the original payer except within finalizers.
//!
//! Contexts are task-owned. Allocation pointers, sizes, alignment and remaining
//! logical charges are trusted emitter operands. None may be stale. Provider
//! operations serialize Arc operations and cross-task refunds in event order.

use crate::{
    Capacity, Context,
    block::{allocate, free, funding},
};

/// Open an empty explicit region. Opening admits nothing.
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_region_open(chunk_bytes: usize) -> *mut crate::region::Region {
    Box::into_raw(Box::new(crate::region::Region::new(chunk_bytes)))
}

/// # Safety
/// Context and region are live, task-local, and exclusively used during this call.
/// Size, alignment and charge describe the emitter's stable allocation. A null
/// result is Capacity; under a grant the emitter routes it to failure cleanup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_region_alloc(
    context: *const Context,
    region: *mut crate::region::Region,
    bytes: usize,
    align: usize,
    charge: u64,
    overflow: bool,
) -> Allocation {
    // SAFETY: emitter supplies live task-local region and context.
    match unsafe { (&mut *region).allocate(&*context, bytes, align, (!overflow).then_some(charge)) }
    {
        Ok(p) => Allocation {
            pointer: p.as_ptr(),
            capacity: Capacity {
                requested: 0,
                available: 0,
            },
        },
        Err(capacity) => Allocation {
            pointer: ptr::null_mut(),
            capacity,
        },
    }
}

/// Lower a live region allocation's charge, without moving published bytes.
/// # Safety
/// The sole destination writer owns a live allocation and proves its unused tail;
/// retained is its new charge, no larger than its current charge.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_region_lower_charge(pointer: NonNull<u8>, retained: u64) {
    // SAFETY: the caller holds the live allocation's sole writer.
    unsafe {
        crate::region::Region::lower_charge(pointer, retained);
    }
}

/// # Safety
/// Caller transfers sole ownership of an open region; its views have ended.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_region_close(region: *mut crate::region::Region) {
    // SAFETY: every stable locator into this region has ended.
    unsafe {
        drop(Box::from_raw(region));
    }
}
use crate::causes::{self, Cause, Cell};
use crate::counted::{self, Recipe, Worklist};

/// Admit main's task record and primary cell (288) once, before running its body.
/// Child providers pre-admit the spawn as a whole and attach that storage; they
/// must not charge child startup again through this root-start entry point.
/// # Safety
/// Context is exclusively owned and has not started a task yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_task_start(context: *mut Context) -> Allocation {
    // SAFETY: the caller starts this fresh task context exactly once.
    match causes::task_start(unsafe { &mut *context }) {
        Ok(()) => Allocation {
            pointer: context.cast(),
            capacity: Capacity {
                requested: 0,
                available: 0,
            },
        },
        Err(capacity) => Allocation {
            pointer: ptr::null_mut(),
            capacity,
        },
    }
}
/// Admit storage + cause cell + grant before the emitter installs prepared fields.
/// # Safety
/// Context is live; caller owns successful resource and must finalize or wholly
/// destructure it, then resource_free once. Native layout is compiler-produced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_resource_alloc(
    context: *const Context,
    bytes: usize,
    align: usize,
    storage: u64,
    grant: u64,
) -> Allocation {
    // SAFETY: emitter owns the resource returned from this constructor.
    match unsafe { causes::resource_new(&*context, storage, grant, bytes, align) } {
        Ok(pointer) => Allocation {
            pointer: pointer.as_ptr(),
            capacity: Capacity {
                requested: 0,
                available: 0,
            },
        },
        Err(capacity) => Allocation {
            pointer: ptr::null_mut(),
            capacity,
        },
    }
}
/// Enter the resource's prepaid grant. Infallible and allocation-free.
/// # Safety
/// Resource and exclusive task context are live; its finalizer has not run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_finalizer_begin(context: *mut Context, resource: NonNull<u8>) {
    // SAFETY: caller begins exactly this live resource's finalizer.
    unsafe {
        causes::finalizer_begin(&mut *context, resource);
    }
}
/// Settle the grant and restore the previous current limit and finalizer.
/// # Safety
/// Matches the active finalizer_begin, after its inner scope cleanup, even when
/// it failed. The emitter next cleans fields in spec order, then resource_free.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_finalizer_end(context: *mut Context, resource: NonNull<u8>) {
    // SAFETY: the current resource's body has ended and its scopes are cleaned.
    unsafe {
        causes::finalizer_end(&mut *context, resource);
    }
}
/// Refund all remaining container storage and any unused cell/grant.
/// # Safety
/// Sole owner transfers an inactive resource with cleaned or moved fields.
/// Pass only field storage, excluding the 32-byte cell and grant. Whole
/// destructure calls this directly, without a finalizer (W5).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_resource_free(resource: NonNull<u8>, storage: u64) {
    // SAFETY: all container ownership is transferred exactly once.
    unsafe {
        causes::resource_free(resource, storage);
    }
}
/// Record one cause in the current prepaid task/resource cell. Admits nothing.
/// # Safety
/// Transfers the cause's owned message. The obligation has an unused cell;
/// the emitter immediately abandons this body and propagates after cleanup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_raise(context: *mut Context, cause: Cause) {
    // SAFETY: compiler failure paths use their obligation cell exactly once.
    unsafe {
        causes::record(&mut *context, cause);
    }
}
/// Deliver the current chain by ownership transfer, without admission/allocation.
/// # Safety
/// Context is live and exclusively used by its task after cleanup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_causes_take(context: *mut Context) -> *mut Cell {
    // SAFETY: the caller owns the completed task's chain during this transfer.
    std::mem::take(unsafe { &mut (*context).causes }).into_raw()
}
/// # Safety
/// Transfer the entire exclusively owned chain, or null, exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_causes_drop(causes: *mut Cell) {
    // SAFETY: consumes all transferred cells and their owned messages.
    unsafe {
        drop(causes::Causes::from_raw(causes));
    }
}
/// # Safety
/// Cell is a live borrowed cell of an unmodified Causes owner.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_cause_next(cell: *const Cell) -> *const Cell {
    // SAFETY: chain owner keeps this borrowed cell live.
    unsafe { causes::next(cell) }
}
/// # Safety
/// Cell is a live borrowed cell of an unmodified Causes owner. Returned data is
/// borrowed and immutable; neither the message nor cell ownership transfers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_cause_value(cell: *const Cell) -> *const Cause {
    // SAFETY: a delivered cell contains a cause, pinned by its chain owner.
    unsafe { causes::value(cell) }
}
use std::ptr::{self, NonNull};

/// # Safety
/// Node is fresh aligned storage for a FORM count word and payload at offset 8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_counted_init(node: NonNull<u8>, atomic: bool) {
    // SAFETY: emitter supplies a fresh counted allocation.
    unsafe {
        counted::initialize(node, atomic);
    }
}
/// # Safety
/// A live holder pins node; Rc is task-local, Arc is provider-event-ordered.
/// Count overflow is a FAIL-15 run stop, never Capacity.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_share(node: NonNull<u8>, atomic: bool) {
    // SAFETY: the caller holds the live node under the count/event discipline.
    unsafe {
        counted::share(node, atomic);
    }
}
/// # Safety
/// A live holder pins node, under the same count/event discipline as share.
/// True transfers exclusive payload custody for copy plus free; false preserves h.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_try_unwrap(node: NonNull<u8>, atomic: bool) -> bool {
    // SAFETY: the caller owns a live holder in its provider event.
    unsafe { counted::try_unwrap(node, atomic) }
}
/// # Safety
/// Transfers one holder. Context is live; charge/recipe describe node. Generated
/// glue enqueues descendants and neither recurses nor allocates nor runs user code.
/// Arc release and its last refund execute within the caller's provider event.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_release(
    context: *mut Context,
    node: NonNull<u8>,
    atomic: bool,
    charge: u64,
    recipe: Recipe,
) {
    // SAFETY: the emitter supplies the node's mechanical recipe and one holder.
    unsafe {
        counted::release(context, node, atomic, charge, recipe);
    }
}
/// # Safety
/// Called only by drop glue with its live worklist, transferring one descendant
/// holder, its exact charge and its mechanical recipe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_release_enqueue(
    worklist: *mut Worklist,
    node: NonNull<u8>,
    atomic: bool,
    charge: u64,
    recipe: Recipe,
) {
    // SAFETY: drop glue holds the current worklist exclusively.
    unsafe {
        counted::enqueue(&mut *worklist, node, atomic, charge, recipe);
    }
}
/// # Safety
/// As for release_enqueue, transferring a uniquely owned raw block without a
/// count word. Its recipe receives payload offset 0. No resources are reachable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_release_own_enqueue(
    worklist: *mut Worklist,
    node: NonNull<u8>,
    charge: u64,
    recipe: Recipe,
) {
    // SAFETY: the generated recipe transfers an exclusively owned descendant.
    unsafe {
        crate::block::push_dead(&mut *worklist, node, charge, recipe, 0);
    }
}

#[repr(C)]
pub struct Allocation {
    pub pointer: *mut u8,
    pub capacity: Capacity,
}

/// Create a context. The provider owns task startup admission (256 + 32).
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_context_new(bytes: u64) -> *mut Context {
    Box::into_raw(Box::new(Context::new(bytes)))
}
/// # Safety
/// `context` is a uniquely owned context from context_new, with no active calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_context_free(context: *mut Context) {
    // SAFETY: the caller transfers the uniquely owned context back to Rust.
    unsafe {
        drop(Box::from_raw(context));
    }
}
/// # Safety
/// Context is live and exclusively used by its task. `like` is null or a live
/// own/counted/resource allocation. Size/alignment are the emitter's layout.
/// Overflow denotes an exact logical charge greater than u64::MAX. The caller
/// owns a successful pointer and must free it once after cleaning its payload.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_alloc(
    context: *const Context,
    like: *mut u8,
    bytes: usize,
    align: usize,
    charge: u64,
    overflow: bool,
) -> Allocation {
    // SAFETY: the emitter supplies a live context and optional live block.
    let (context, like) = unsafe { (&*context, NonNull::new(like).map(|p| funding(p))) };
    match allocate(
        context.payer(like),
        (!overflow).then_some(charge),
        bytes,
        align,
    ) {
        Ok(p) => Allocation {
            pointer: p.as_ptr(),
            capacity: Capacity {
                requested: 0,
                available: 0,
            },
        },
        Err(capacity) => Allocation {
            pointer: ptr::null_mut(),
            capacity,
        },
    }
}
/// # Safety
/// `pointer` is uniquely owned with cleaned payload; charge is its remaining charge.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_free(pointer: NonNull<u8>, charge: u64) {
    // SAFETY: ownership and the exact charge are transferred by the caller.
    unsafe {
        free(pointer, charge);
    }
}
/// # Safety
/// Context is live and exclusively used by its task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_limit_enter(context: *mut Context, bytes: u64) -> Allocation {
    // SAFETY: caller owns the task context during this call.
    match unsafe { &mut *context }.limit_enter(bytes) {
        Ok(()) => Allocation {
            pointer: context.cast(),
            capacity: Capacity {
                requested: 0,
                available: 0,
            },
        },
        Err(capacity) => Allocation {
            pointer: ptr::null_mut(),
            capacity,
        },
    }
}
/// # Safety
/// Context has an entered bound scope, whose emitter cleanup has completed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_limit_exit(context: *mut Context) {
    // SAFETY: the caller owns the task context and ends its innermost bound.
    unsafe { &mut *context }.limit_exit();
}
