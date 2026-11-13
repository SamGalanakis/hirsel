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
//! chain, null for empty; cause accessors return borrowed pointers to the exact
//! core Cause layout, published in abi_layout.rs. rt.cause_next/value take only
//! the cell, return next/core-view pointers, and have no context or failure status.
//! allocation_status
//! returns 1 only for a null result under the current finalizer grant, else 0. Other
//! entries return unit, except dest_id (u64), compare_bytes (i8) and native_alloc/region_refill
//! (nonnull native base/payload). Count overflow and physical host exhaustion are run
//! stops. A drop recipe's boolean means completion, with false requesting resume.
//! Shipping standalone archives also stop as HostMemory when Rust bookkeeping
//! allocation fails. Embeddable/test rlibs retain their host's global allocator.
//! Startup page-query refusal stops as PageSize; missing prior signal state as Signal.
//!
//! These C signatures are complete. The FORM function-row u8 failure status is
//! handled by the emitter. FORM decision 39 fixes successful admission's
//! available field at zero; no source-level Capacity exists on success.
//!
//! FORM v1b: `rt.dest_id` maps to `hirsel_core_dest_id() -> u64`, with no
//! parameters, context or failure status. `rt.compare_bytes` maps to
//! `hirsel_core_compare_bytes(a, a_len, b, b_len) -> i8`, likewise without
//! context or status. `trim` maps to region_lower_charge;
//! allocation's byte size is already a dynamic parameter. The producer writes
//! the 16-byte blob header and data, checks Child identity/offset and width/fit,
//! and emits writer/copy/index operations over these entries and byte accesses.
//! Exhausted destination identities stop as `DestinationIds`; physical allocator
//! refusal or an unrepresentable native layout stops as `HostMemory`. FAIL-15
//! stops write `run stop: <Name>\n` and exit 102 without cleanup or failure status.
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
//! If lowering proves the current payer stays unchanged from admission to free,
//! it may refund that record directly, skipping the native payer lookup. Otherwise
//! compare the RAW payer word to current before stripping tags or resolving
//! forwarding records. On equality and nonzero charge, refund current directly;
//! on mismatch use refund(original_payer, charge). Native tags can only cause a
//! false negative. refund_in(context, original_payer, charge) exposes the same
//! context-aware path to C. Outstanding bytes prevent a slot from being reused,
//! and current is canonical, including after scope handover or grant completion.
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
//! The immutable u64 event word is at context+CONTEXT_EVENTS (8). Zero means
//! no sink; otherwise it is the pinned Task pointer used by the context-first
//! hooks. After each inline Arc operation, test events != 0 and call
//! arc_initialized/shared/released/unwrapped(context, ...) on the cold side.
//! Without a sink no hook is called. The root sets the word before execution;
//! each child inherits recording and installs its own pointer before publication.
//! It cannot change until that task's run ends. Hooks never read task TLS.
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

//! Optional charge-observation archive: `hirsel_core_observe_charge() -> ()` samples
//! after an emitted debit; `hirsel_core_peak_charge() -> u64` reads the completed
//! run's root peak reservation (bound/task/grant reservations included, MEM-19).
//! Neither has failure status, admission, refund delivery or an event. Both are
//! absent from shipping archives; observed emission must link the observed archive.
//!
// Offset and header constants are shared verbatim with both native emitters.
pub use crate::abi_layout::*;

#[cfg(feature = "charge-observation")]
pub use crate::peak::{hirsel_core_observe_charge, hirsel_core_peak_charge};

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

/// `rt.hash_bytes`: the fixed byte hash a `Map` key's text components fold
/// (hirsel-core-ir crate docs, "Maps"): from `h = len`, fold
/// `mix(h, w) = rotl((h ^ w) × 0x9E3779B97F4A7C15, 26)` over the little-endian
/// 8-byte words, the last zero-padded, then `h ^ (h >> 32)`. The producer hashes
/// static map literals with the same function, so it is fixed, never seeded. No
/// context, admission or status; a range of length 0 is never read.
///
/// # Safety
/// A non-empty range must be readable for its length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_hash_bytes(data: *const u8, len: u64) -> u64 {
    let bytes = match len {
        0 => &[][..],
        // SAFETY: the caller passes a readable range of len bytes.
        n => unsafe { std::slice::from_raw_parts(data, n as usize) },
    };
    let mix = |h: u64, w: u64| ((h ^ w).wrapping_mul(0x9E37_79B9_7F4A_7C15)).rotate_left(26);
    let mut h = len;
    // Full words need neither padding nor a variable-sized copy. Array elements
    // have byte alignment, so this also handles unaligned input without overread.
    let (words, tail) = bytes.as_chunks::<8>();
    for &word in words {
        h = mix(h, u64::from_le_bytes(word));
    }
    if !tail.is_empty() {
        let mut word = [0u8; 8];
        word[..tail.len()].copy_from_slice(tail);
        h = mix(h, u64::from_le_bytes(word));
    }
    h ^ (h >> 32)
}

/// `rt.compare_bytes`: the bytewise order of two byte ranges (SEM-7), -1, 0 or 1,
/// a proper prefix first. No context, admission or failure status; a range of
/// length 0 is never read, so its pointer may be null or static.
///
/// # Safety
/// Each non-empty range must be readable for its length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_compare_bytes(
    a: *const u8,
    a_len: u64,
    b: *const u8,
    b_len: u64,
) -> i8 {
    let range = |p: *const u8, n: u64| match n {
        0 => &[][..],
        // SAFETY: the caller passes a readable range of n bytes.
        n => unsafe { std::slice::from_raw_parts(p, n as usize) },
    };
    range(a, a_len).cmp(range(b, b_len)) as i8
}

#[cfg(test)]
thread_local! { pub(crate) static ARC_HOOK_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[inline(always)]
fn count_arc_hook() {
    #[cfg(test)]
    ARC_HOOK_CALLS.update(|n| n + 1);
}
/// Complete inline atomic initialization. No status/admission/allocation/count mutation.
/// # Safety
/// Context is live and events != 0; its run's sink is immutable. Count 1 was
/// initialized before publication and the scheduler still owns the event turn.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_arc_initialized(context: *const Context) {
    count_arc_hook();
    // SAFETY: caller's event gate pins this task and sink through the callback.
    unsafe {
        crate::tasks::event_in(context, crate::tasks::ARC_NEW, 0, 1);
    }
}
/// Complete an emitted Arc increment. No status/admission/allocation.
/// # Safety
/// As for arc_initialized; node is live and its checked atomic increment completed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_arc_shared(context: *const Context, node: *const u8) {
    count_arc_hook();
    // SAFETY: caller holds the node and the recorded event turn.
    unsafe {
        let count = (&*node.cast::<std::sync::atomic::AtomicU64>())
            .load(std::sync::atomic::Ordering::Relaxed);
        crate::tasks::event_in(context, crate::tasks::ARC_SHARE, 0, count);
    }
}
/// Complete an emitted Arc decrement before last-release glue/refunds.
/// # Safety
/// As for arc_initialized; last exactly describes the completed decrement.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_arc_released(context: *const Context, last: bool) {
    count_arc_hook();
    // SAFETY: the event gate pins this immutable task/sink until completion.
    unsafe {
        crate::tasks::event_in(context, crate::tasks::ARC_RELEASE, 0, u64::from(last));
    }
}
/// Complete an emitted Arc acquire uniqueness test.
/// # Safety
/// As for arc_initialized; unique is the completed test result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_arc_unwrapped(context: *const Context, unique: bool) {
    count_arc_hook();
    // SAFETY: the event gate pins this immutable task/sink until completion.
    unsafe {
        crate::tasks::event_in(context, crate::tasks::ARC_UNWRAP, 0, u64::from(unique));
    }
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
/// its payer for growth. For a collection, the emitter selects null whenever
/// capacity is zero, even if its data points at a static literal. Size/alignment/
/// kind are the emitter's native layout.
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

/// Refund with the context-aware raw-word equality fast path. Equal nonzero
/// payer/current refunds directly; otherwise follows the original payer. No
/// status, admission or allocation. Tags may send an equal payer to the slow path.
/// # Safety
/// Context is task-exclusive; payer is retained by these exact outstanding bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_refund_in(
    context: *const Context,
    payer: *mut crate::limit::LimitRecord,
    bytes: u64,
) {
    // SAFETY: caller transfers the live block's exact remaining charge.
    unsafe {
        crate::limit::Payer::from_current(payer).settle_in(&*context, bytes);
    }
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
    if align > 16 {
        return native_alloc_aligned(bytes, align);
    }
    // SAFETY: this branch has the ordinary backend alignment contract.
    unsafe { hirsel_core_native_alloc_ordinary(bytes) }
}

/// Physical backend for a compile-time native alignment at most 16. Returns a
/// nonnull base or stops as HostMemory, with no admission, event or failure status.
/// Emitters select this entry instead of passing a known ordinary alignment.
/// # Safety
/// Positive valid native size, already logically admitted, alignment at most 16.
#[inline]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_native_alloc_ordinary(bytes: usize) -> *mut u8 {
    // SAFETY: malloc/mi_malloc provide at least 16-byte native alignment. The
    // emitter's valid nonzero size also forms a Layout at alignment 1, and the
    // backend's actual alignment satisfies its request. Alignment 1 avoids
    // rounding an already valid near-isize::MAX size up again. Logical charge
    // and native prefix operands remain the emitter's original values.
    let layout = unsafe { std::alloc::Layout::from_size_align_unchecked(bytes, 1) };
    crate::allocator::allocate(layout).as_ptr()
}
#[cold]
#[inline(never)]
fn native_alloc_aligned(bytes: usize, align: usize) -> *mut u8 {
    // SAFETY: the emitter supplies the same valid over-aligned native layout.
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
/// Recipe follows the ordinary resumable mechanical recipe contract. The
/// carrier capacity is nonzero; a static capacity-zero pointer is not a block.
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

/// Emitted free when lowering proves this block was admitted from the still
/// current record. No scope switch, handover or finalizer may intervene.
/// # Safety
/// The exclusively owned PLAIN block was admitted from context's current payer;
/// charge and native operands are exact and every payload field is clean.
#[inline(always)]
pub unsafe fn inline_own_free_current(
    context: &Context,
    pointer: NonNull<u8>,
    charge: u64,
    bytes: usize,
    align: usize,
) {
    if charge == 0 {
        // SAFETY: a zero-charge block still owns its native payer pin.
        return unsafe { inline_own_free(context, pointer, charge, bytes, align) };
    }
    // SAFETY: lowering proves the current record paid these outstanding bytes.
    unsafe {
        (*context.limit_record()).available =
            (*context.limit_record()).available.wrapping_add(charge);
        (*context.limit_record()).outstanding =
            (*context.limit_record()).outstanding.wrapping_sub(charge);
        crate::block::deallocate_plain(pointer, bytes, align);
    }
}
