//! Native memory ABI, FORM v1. Every symbol and failure convention lives here.
//!
//! `alloc` and `limit_enter` return null plus Capacity on refusal; on success
//! `capacity` is zero. Null from the active finalizer grant takes the emitter's
//! Capacity failure path; an inner bound returns ordinary Capacity. `free`, limit
//! exit and mechanical release never
//! fail. `alloc like` follows the original payer except within finalizers.
//!
//! `region_alloc`, `resource_alloc` and `task_start` use the same Allocation
//! convention. Success pointers from `limit_enter` and `task_start` equal the
//! context; other allocation success pointers address payload storage. Context
//! and region creation return owned handles and admit no language storage.
//! `try_unwrap` returns its uniqueness test. `causes_take` transfers a complete
//! chain, null for empty; cause accessors return borrowed pointers. allocation_status
//! returns 1 only for a null result under the current finalizer grant, else 0. Other
//! entries return unit, except dest_id (u64) and native_alloc/region_refill
//! (nonnull native base/payload). Count overflow and physical host exhaustion are run
//! stops. A drop recipe's boolean means completion, with false requesting resume.
//!
//! These C signatures are complete. The FORM function-row u8 failure status is
//! handled by the emitter. FORM decision 39 fixes successful admission's
//! available field at zero; no source-level Capacity exists on success.
//!
//! FORM v1b: `rt.dest_id` maps to `hirsel_core_dest_id() -> u64`, with no
//! parameters, context or failure status. `trim` maps to region_lower_charge;
//! allocation's byte size is already a dynamic parameter. The producer writes
//! the 16-byte blob header and data, checks Child identity/offset and width/fit,
//! and emits writer/copy/index operations over these entries and byte accesses.
//!
//! Contexts are task-owned. Allocation pointers, sizes, alignment and remaining
//! logical charges are trusted emitter operands. None may be stale. Provider
//! operations serialize Arc operations and cross-task refunds in event order.

//! Inline contract: compare charge with current_limit.available, subtract on
//! success and add it to outstanding. A plain block's only native word is its
//! payer pointer at payload-8. Native allocation kind is 0 plain, 1 graph or 2
//! resource. Payload offset is align_up(prefix_bytes, max(alignment,8)); native
//! size is offset+max(bytes,1). The payer word tags its low five bits: kind in
//! bits 0..1; bits 2..4 encode log2(max(alignment,8))-3. Index 7 stores the
//! alignment in one extra word before the prefix. Generic free takes pointer
//! and charge; it decodes this word and calls the native allocator's free.
//!
//! Region fast allocation compares its payer with current_limit, admits once,
//! adds charge, aligns/bumps cursor and checks end. On payer change call the full
//! region_alloc before admitting; on chunk exhaustion after admission call
//! region_refill. Builder captures region.entry plus its original charge; trim
//! takes (region, entry, previous_charge, retained_charge). No object headers.
//!
//! Rc uses checked u64 increment and decrement/test at count offset zero; only
//! the zero result calls release_last. Arc uses relaxed increment and release
//! decrement plus an acquire fence before release_last. Provider event ordering
//! still surrounds every Arc operation. Arc initialization stores an aligned
//! atomic u64 value 1 before publication; Rc initialization stores ordinary u64 1.
//! Inline Arc calls arc_initialized/shared/released/unwrapped after each operation.
//! These hooks return unit, admit/allocate nothing and never mutate a count;
//! release publishes before last-release glue.
//! A statically empty bound can emit the
//! checked reservation and refund without materializing a payer table identity.
//!
//! Scheduler binds native execution on resume and unbinds before transferring
//! it. Bind drains prepaid per-slot remote refunds; local accounting has no
//! atomics or lock. Foreign-payer growth (MEM-26) and a received resource's grant
//! use abi_tasks::payer_lease over the original payer's complete table and drain
//! prior refunds before admission. The one-worker scheduler owns that lease
//! throughout its turn. Multiple workers must quiesce every task sharing the
//! domain across growth; a received grant keeps the lease through finalizer_end,
//! including suspension. No local admission/refund/enter/exit acquires a lock.

// Offset and header constants are shared verbatim with both native emitters.
pub use crate::abi_layout::*;

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
/// entry is its saved region entry; previous is this builder's remaining charge.
/// retained is its new charge, no larger than previous.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_region_lower_charge(
    region: *mut crate::region::Region,
    entry: usize,
    previous: u64,
    retained: u64,
) {
    // SAFETY: the caller holds the live allocation's sole writer.
    unsafe {
        (*region).lower_charge(entry, previous, retained);
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

/// `rt.dest_id`: a fresh destination identity. No admission, allocation or
/// failure status. Identities never wrap; exhausting u64 is a run stop (FAIL-15).
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_dest_id() -> u64 {
    crate::destination::next()
}

/// Publish inline Arc initialization after storing atomic u64 1. No status,
/// admission, allocation, suspension or count mutation. The scheduler owns
/// the event turn across initialization and this hook, before publication.
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_arc_initialized() {
    crate::tasks::event(crate::tasks::ARC_NEW, 0, 1);
}

/// Dispatch the provider event after an emitted Arc increment.
/// # Safety
/// Node is a live initialized Arc under the provider's operation order.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_arc_shared(node: *const u8) {
    // SAFETY: caller just completed the increment on this live Arc.
    let count = unsafe {
        (&*node.cast::<std::sync::atomic::AtomicU64>()).load(std::sync::atomic::Ordering::Relaxed)
    };
    crate::tasks::event(crate::tasks::ARC_SHARE, 0, count);
}
/// Dispatch the provider event after an emitted Arc decrement. Admits nothing
/// and has no failure status; last-release traversal has not begun yet.
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_arc_released(last: bool) {
    crate::tasks::event(crate::tasks::ARC_RELEASE, 0, u64::from(last));
}
/// Dispatch the provider event after an emitted Arc uniqueness test.
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_arc_unwrapped(unique: bool) {
    crate::tasks::event(crate::tasks::ARC_UNWRAP, 0, u64::from(unique));
}

/// # Safety
/// Node is fresh GRAPH storage for a FORM count word and payload at offset 8.
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
        if crate::block::funding(node).native_tag() & 3 == crate::block::PLAIN as usize {
            crate::block::free(node, charge);
        } else {
            crate::block::push_dead(&mut *worklist, node, charge, recipe, 0);
        }
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
    let context = Box::new(Context::new(bytes));
    context.current.native();
    Box::into_raw(context)
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
/// own/counted/resource allocation. Caller has exclusive execution custody over
/// its payer for growth. Size/alignment/kind are the emitter's native layout.
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
    kind: u8,
) -> Allocation {
    // SAFETY: the emitter supplies a live context and optional live block.
    let (context, like) = unsafe { (&*context, NonNull::new(like).map(|p| funding(p))) };
    match crate::block::allocate_like(
        context,
        like,
        (!overflow).then_some(charge),
        bytes,
        align,
        kind,
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
/// Bytes, alignment and native kind match its original physical allocation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_free(pointer: NonNull<u8>, charge: u64) {
    // SAFETY: caller transfers the cleaned native block and exact retained charge.
    unsafe {
        free(pointer, charge);
    }
}
/// # Safety
/// Context is live and exclusively used by its task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_limit_enter(context: *mut Context, bytes: u64) -> Allocation {
    // SAFETY: caller owns this context; no operation between selecting and
    // consuming the free-list head can suspend or invoke a provider.
    unsafe {
        let task = &mut *context;
        let Some(slot) = task.current.reusable_slot() else {
            return limit_enter_refill(context, bytes);
        };
        let result = task.current.enter_reused(bytes, slot);
        if result.is_ok() {
            task.bound_depth = task.bound_depth.wrapping_add(1);
        }
        scope_allocation(context, result)
    }
}
#[cold]
unsafe extern "C" fn limit_enter_refill(context: *mut Context, bytes: u64) -> Allocation {
    // SAFETY: this is the same task-exclusive entry with an empty record pool.
    let result = unsafe { &mut *context }.limit_enter(bytes);
    scope_allocation(context, result)
}
#[inline(always)]
fn scope_allocation(context: *mut Context, result: Result<(), Capacity>) -> Allocation {
    match result {
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

/// Apply the ordered remote-refund events before the payer task resumes.
/// Admits and allocates nothing; does not report a failure status.
/// # Safety
/// Task-owned live context, not concurrently running on another worker.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_refunds_drain(context: *const Context) {
    // SAFETY: the scheduler gives this task exclusive local execution custody.
    unsafe {
        (*context).current.drain();
    }
}

/// Refill after emitted admission and region charge accumulation. Host OOM is
/// a run stop; returns a nonnull pointer, never a Capacity or function status.
/// # Safety
/// Exclusive open region; bytes/alignment match the already admitted operation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_region_refill(
    region: *mut crate::region::Region,
    bytes: usize,
    align: usize,
) -> *mut u8 {
    // SAFETY: the emitter already performed the logical admission exactly once.
    unsafe { (*region).refill(bytes, align).as_ptr() }
}

/// Last-release slow path after the emitted decrement-and-test consumes count 1.
/// No failure status, allocation or admission. Same recipe convention as release.
/// # Safety
/// Exclusive dead GRAPH allocation with count zero and exact charge/recipe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_release_last(
    context: *mut Context,
    node: NonNull<u8>,
    charge: u64,
    recipe: Recipe,
) {
    // SAFETY: the emitter transferred the unique dead graph node after decrement.
    unsafe {
        counted::release_last(context, node, charge, recipe);
    }
}

/// Refund through an original payer after emitted physical deallocation. Remote
/// payers use the event mailbox. Admits/allocates nothing, no failure status.
/// # Safety
/// Payer is pinned by these nonzero outstanding bytes, settled exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_refund(payer: *mut crate::limit::LimitRecord, bytes: u64) {
    crate::limit::Payer::from_current(payer).refund(bytes);
}

/// Scheduler handoff: bind the native task to this worker and apply pending
/// ordered refunds before resumption. Null unbinds before giving custody away.
/// No admission, allocation or failure status.
/// # Safety
/// A nonnull context is a live native context under exclusive scheduler custody;
/// its previous worker has unbound and all later transfers follow this protocol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_task_bind(context: *const Context) {
    if context.is_null() {
        crate::limit::Funding::unbind();
    } else {
        // SAFETY: exclusive scheduler custody permits this worker to run the task.
        unsafe {
            (*context).current.bind();
            (*context).current.drain();
        }
    }
}

/// Physical backend for emitted own allocation after logical admission. Returns
/// nonnull native base; host OOM is a run stop, no Capacity or failure status.
/// The emitter adds the payload offset and writes its one payer word.
/// # Safety
/// Nonzero valid native layout, from trusted lowering, already logically admitted.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_native_alloc(bytes: usize, align: usize) -> *mut u8 {
    // SAFETY: trusted emitter supplies a nonzero valid native layout.
    let layout = unsafe { std::alloc::Layout::from_size_align_unchecked(bytes, align) };
    crate::allocator::allocate(layout).as_ptr()
}

/// Physical backend after emitted cleanup/refund, no admission or status.
/// # Safety
/// Base is the unique live result of native_alloc with these exact operands.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_native_free(base: *mut u8) {
    // SAFETY: emitter transfers the original matching native allocation base.
    unsafe {
        crate::allocator::deallocate_raw(base);
    }
}

/// Move a String into the current prepaid cause cell and record Fail. The
/// carrier is copied into cell-owned native bookkeeping; no admission occurs.
/// # Safety
/// Context has an unused primary/resource cell. The live String buffer and its
/// capacity transfer to that cell and must not be used or dropped by the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_raise_string(
    context: *mut Context,
    site: u64,
    data: *mut u8,
    len: u64,
    capacity: u64,
) {
    // SAFETY: the emitter transfers exactly this owned String and obligation.
    unsafe {
        causes::record_string(
            &mut *context,
            site,
            causes::StringMessage {
                data,
                len,
                capacity,
            },
        );
    }
}

/// FORM allocation refusal becomes failure only under a finalizer's grant.
/// # Safety
/// Context is live and exclusively held by its task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_allocation_status(
    context: *const Context,
    pointer: *mut u8,
) -> u8 {
    // SAFETY: the emitter passes its live hidden task context.
    u8::from(
        pointer.is_null()
            && unsafe { &*context }.finalizing != 0
            && unsafe { &*context }.bound_depth == unsafe { &*context }.grant_depth,
    )
}

/// Free a wholly destructured own value or resource.
/// # Safety
/// The block is exclusively owned, all fields have moved or been cleaned, and a
/// resource has not begun finalization. Charge is its remaining logical charge.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_free_value(pointer: NonNull<u8>, charge: u64) {
    // SAFETY: the emitter supplies the live block with the contract above.
    unsafe {
        causes::free_value(pointer, charge);
    }
}

/// Rust instruction shim for the emitted plain-own sequence, not a C symbol.
/// # Safety
/// Trusted nonzero native layout operands and exact portable charge. Caller
/// owns the resulting uninitialized bytes and cleans them before inline_own_free.
#[inline(always)]
pub unsafe fn inline_own_alloc(
    context: &Context,
    charge: Option<u64>,
    bytes: usize,
    align: usize,
) -> Result<NonNull<u8>, Capacity> {
    allocate(
        context.payer(None),
        charge,
        bytes,
        align,
        crate::block::PLAIN,
    )
}
/// Rust instruction shim for emitted plain-own refund and physical deallocation.
/// # Safety
/// Exclusive cleaned block from inline_own_alloc, exact original layout/charge,
/// and a live task context. The block may have a different or handed-over payer.
#[inline(always)]
pub unsafe fn inline_own_free(
    context: &Context,
    pointer: NonNull<u8>,
    charge: u64,
    bytes: usize,
    align: usize,
) {
    // SAFETY: the emitter transfers the exclusively owned block and native operands.
    unsafe {
        crate::block::free_in(context, pointer, charge, bytes, align);
    }
}

#[cfg(test)]
mod destination_tests {
    #[test]
    fn destination_identities_are_unique_across_tasks() {
        let mut ids = std::thread::scope(|scope| {
            let tasks: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        (0..1000)
                            .map(|_| super::hirsel_core_dest_id())
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            tasks
                .into_iter()
                .flat_map(|task| task.join().unwrap())
                .collect::<Vec<_>>()
        });
        ids.sort_unstable();
        assert_eq!(ids.len(), 4000);
        assert!(ids.windows(2).all(|pair| pair[0] != pair[1]));
    }
}

/// Enqueue owned sequence storage while its descriptor remains in its parent.
/// # Safety
/// Buffer is exclusively owned. The borrowed payload is in the live parent
/// worklist frame and remains alive through this child's depth-first cleanup.
/// Recipe follows the ordinary resumable mechanical recipe contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_release_buffer_enqueue(
    list: *mut Worklist,
    buffer: NonNull<u8>,
    charge: u64,
    payload: *mut u8,
    recipe: Recipe,
) {
    // SAFETY: the emitter transfers the buffer while its parent pins the descriptor.
    unsafe {
        crate::block::push_buffer(&mut *list, buffer, charge, recipe, payload);
    }
}

/// Count one emitted admission and consult the deterministic test injector.
/// This does not debit storage; the emitted path performs the usual balance test.
/// # Safety
/// Context is the current task and its local payer is exclusively borrowed.
#[cfg(feature = "testing")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_inline_admission(context: *const Context) -> bool {
    // SAFETY: the test object runs under the same task/payer discipline as alloc.
    unsafe { (&*context).payer(None).injected() }
}
