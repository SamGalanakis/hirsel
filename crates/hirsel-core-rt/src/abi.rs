//! Native memory ABI, FORM v1. Every symbol and failure convention lives here.
//!
//! `alloc` and `limit_enter` return null plus Capacity on refusal; on success
//! `capacity` is zero. Which limit refused does not matter: the emitter raises the
//! cause `Capacity(c)` where the IR's producer says so (MEM-21), with `c` in the
//! cause's first two words. `free`, limit exit and mechanical release never
//! fail. All allocation admits from the current limit.
//!
//! `region_alloc`, `resource_alloc` and `task_start` use the same Allocation
//! convention. Success pointers from `limit_enter` and `task_start` equal the
//! context; other allocation success pointers address payload storage. Context
//! and region creation return owned handles and admit no language storage.
//! `try_unwrap` returns its uniqueness test. `causes_take` transfers a complete
//! chain, null for empty; cause accessors return borrowed pointers to the exact
//! core Cause layout, published in abi_layout.rs. rt.cause_next/value take only
//! the cell, return next/core-view pointers, and have no context or failure status.
//! Other entries return unit, except dest_id (u64), compare_bytes (i8) and native_alloc/region_refill
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
//! `hirsel_core_grow_cold(ctx, carrier, bytes, align, stride, charge, capacity,
//! moved, refund, overflow, kind) -> Growth { pointer, available }` consumes an
//! owned sequence backing. Success installs the carrier, refunds the original
//! charge and returns the new pointer/zero room; Capacity returns null/room,
//! unchanged carrier and backing. Native host refusal stops HostMemory102 without cleanup. No
//! new event or refund drain is introduced. Emitters complete a small backing
//! whose charge fits inline (allocate/copy/free) and call this entry for every
//! other growth; it reuses the backing in place from
//! abi_layout::GROW_CALL_MIN_PAYLOAD_BYTES of old native payload upward. kind
//! is PLAIN or GRAPH.
//!
//! Contexts are task-owned. Allocation pointers, sizes, alignment and remaining
//! logical charges are trusted emitter operands. None may be stale. Provider
//! operations serialize Arc operations and cross-task refunds in event order.

//! Inline admission compares the charge with the current limit before debiting.
//! The physical allocator records the payer once per native page. Fast callers
//! pass the payer directly after debit. Free settles the original payer before
//! returning the cleaned slot to its allocating task. Only GRAPH and resource
//! blocks reserve implicit cleanup storage by the payload.
//!
//! Region fast allocation compares its payer with current_limit, admits once,
//! adds charge and bumps a matching size/alignment pool if its chunk has room.
//! On payer change call region_alloc before admitting. On pool change or chunk
//! exhaustion after admission call region_refill. Builder captures region.entry plus its original charge; trim
//! takes (region, entry, previous_charge, retained_charge). No object headers.
//!
//! Rc uses checked u64 increment and decrement/test at count offset zero; only
//! the zero result frees a terminal edge directly or calls release_fixed_last
//! for a fixed payload. Arc uses relaxed increment and release decrement plus an
//! acquire fence before terminal free or release_fixed_last. Provider event ordering
//! still surrounds every Arc operation. Arc initialization stores an aligned
//! atomic u64 value 1 before publication; Rc initialization stores ordinary u64 1.
//! The immutable u64 event word is at context+CONTEXT_EVENTS (8). Zero means
//! no scheduler; otherwise it is the pinned Task pointer used by the
//! context-first hooks. A scheduled Arc operation calls its completion hook
//! to fold posted credits (MEM-20) and record its event. An unwrap may poll.
//! The root sets the word before execution;
//! each child inherits recording and installs its own pointer before publication.
//! It cannot change until that task's run ends. Hooks never read task TLS.
//! These hooks return unit, admit/allocate nothing and never mutate a count;
//! release publishes before last-release glue.
//! A statically empty bound can emit the
//! checked reservation and refund without materializing a payer table identity.
//!
//! Scheduler binds native execution on resume without delivering credits.
//! Admissions use the current limit. Foreign refunds and hand-overs post credits
//! to prepaid slots, folded by the recipient at its next event. A received
//! finalizer has custody of the grant until its hand-over.

//! Optional charge-observation archive: `hirsel_core_observe_charge() -> ()` samples
//! after an emitted debit; `hirsel_core_peak_charge() -> u64` reads the completed
//! run's root peak reservation (bound/task/grant reservations included, MEM-19).
//! memory_report(len:*mut usize)->*const u8 borrows the last UTF-8 JSON report
//! until a run next starts or completes on this worker; len is its byte count.
//! memory_checkpoint(name:*const u8,len:usize)->() records a native UTF-8 phase
//! label at an operation boundary, with no ABI region mutation in progress.
//! None has failure status, admission, refund delivery or an event. All are
//! absent from shipping archives; observed emission must link the observed archive.
//!
// Offset and header constants are shared verbatim with both native emitters.
pub use crate::abi_layout::*;

#[cfg(feature = "charge-observation")]
pub use crate::peak::{
    hirsel_core_memory_checkpoint, hirsel_core_memory_report, hirsel_core_observe_charge,
    hirsel_core_peak_charge,
};

use crate::{
    Capacity, Context,
    block::{allocate, free},
};

/// Open an empty explicit region. Opening admits nothing.
/// # Safety
/// Context is live and task-local through the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_region_open(
    context: *const Context,
) -> *mut crate::region::Region {
    // SAFETY: the emitter passes the current live context.
    let region = crate::region::Region::open_guest(unsafe { &*context });
    #[cfg(any(test, feature = "charge-observation"))]
    {
        crate::peak::allocated(region.cast(), size_of::<crate::region::Region>());
        // SAFETY: the opaque ABI handle remains pinned until region_close.
        unsafe { crate::peak::region_open(region) };
    }
    #[cfg(feature = "prof")]
    {
        crate::mem_prof::alloc(
            region.cast(),
            size_of::<crate::region::Region>(),
            crate::mem_prof::Backing::Region,
        );
        crate::mem_prof::region_open(region);
    }
    region
}

/// # Safety
/// Context and region are live, task-local, and exclusively used during this call.
/// Size, alignment and charge describe the emitter's stable allocation. A null
/// result is Capacity.
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

/// Allocate through a native activation's region slot. A null slot denotes an
/// empty region. Materialize its backing after the first successful admission.
///
/// # Safety
/// The caller owns the slot and a live task context. The slot is initialized
/// to null on entry to the scope and otherwise holds its exclusively owned Region.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_region_slot_alloc(
    context: *const Context,
    slot: *mut *mut crate::region::Region,
    bytes: usize,
    align: usize,
    charge: u64,
    overflow: bool,
) -> Allocation {
    // SAFETY: the checked scope owns this initialized slot across every call.
    let region = unsafe { *slot };
    if !region.is_null() {
        // SAFETY: this scope has its live, exclusive backing.
        return unsafe {
            hirsel_core_region_alloc(context, region, bytes, align, charge, overflow)
        };
    }
    // SAFETY: the emitter passes the task-exclusive context.
    let payer = unsafe { (*context).payer() };
    if let Err(capacity) = payer.admit((!overflow).then_some(charge)) {
        return Allocation {
            pointer: ptr::null_mut(),
            capacity,
        };
    }
    // SAFETY: publish the backing only after admission. The slot's owner closes it.
    let block = unsafe {
        let region = hirsel_core_region_open(context);
        *slot = region;
        (*region).allocate_admitted(payer, bytes, align, charge)
    };
    Allocation {
        pointer: block.as_ptr(),
        capacity: Capacity {
            requested: 0,
            available: 0,
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
        #[cfg(any(test, feature = "charge-observation"))]
        crate::peak::region_close(region);
        #[cfg(feature = "prof")]
        crate::mem_prof::region_close(region);
        #[cfg(feature = "prof")]
        crate::mem_prof::free(region.cast(), size_of::<crate::region::Region>());
        crate::region::Region::close_guest(region);
        #[cfg(any(test, feature = "charge-observation"))]
        crate::peak::deallocated(region.cast());
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
/// Record a built-in cause with no owned message in the current prepaid cell.
/// # Safety
/// The caller owns the task context and its unused obligation cell. The kind
/// is a built-in kind other than Fail, and cleanup follows the same contract
/// as for `hirsel_core_raise`.
#[cold]
#[inline(never)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_raise_builtin(context: *mut Context, kind: u8, site: u64) {
    // SAFETY: the caller supplies the live context and unused obligation.
    unsafe { hirsel_core_raise(context, Cause::new(kind, site)) };
}
/// Record `Capacity(c)` for a refused admission (MEM-21) in the current prepaid
/// cell; the refusal itself admitted nothing.
/// # Safety
/// As for `hirsel_core_raise_builtin`.
#[cold]
#[inline(never)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_raise_capacity(
    context: *mut Context,
    site: u64,
    requested: u64,
    available: u64,
) {
    #[cfg(feature = "prof")]
    crate::mem_prof::capacity_raise(requested, available);
    let capacity = Capacity {
        requested,
        available,
    };
    // SAFETY: the caller supplies the live context and unused obligation.
    unsafe { causes::record_capacity(&mut *context, capacity, site) };
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

unsafe fn map_hash_range<'a>(data: *const u8, len: u64) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        // SAFETY: the calling Map hash op guarantees a readable range.
        unsafe { std::slice::from_raw_parts(data, len as usize) }
    }
}

/// # Safety
/// Context belongs to this task and a non-empty input is readable for its length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_map_hash_bytes(
    context: *const Context,
    data: *const u8,
    len: u64,
) -> u64 {
    let mut state = unsafe { (*context).map_hash_key.state() };
    state.bytes(unsafe { map_hash_range(data, len) });
    state.finish()
}

/// # Safety
/// Context belongs to this task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_map_hash_word(context: *const Context, word: u64) -> u64 {
    let mut state = unsafe { (*context).map_hash_key.state() };
    state.word(word);
    state.finish()
}

/// # Safety
/// Context belongs to this task and a non-empty input is readable for its length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_map_strong_bytes(
    context: *const Context,
    data: *const u8,
    len: u64,
) -> u64 {
    let mut state = unsafe { (*context).map_strong_key.state() };
    state.bytes(unsafe { map_hash_range(data, len) });
    state.finish()
}

/// # Safety
/// Context belongs to this task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_map_strong_word(context: *const Context, word: u64) -> u64 {
    let mut state = unsafe { (*context).map_strong_key.state() };
    state.word(word);
    state.finish()
}

use hirsel_core_layout::map_hash::State as MapHashState;

/// # Safety
/// Context belongs to this task and state points to writable, aligned scratch.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_map_strong_start(
    context: *const Context,
    state: *mut MapHashState,
) {
    unsafe { state.write((*context).map_strong_key.state()) }
}

/// # Safety
/// Context belongs to this task and state points to writable, aligned scratch.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_map_hash_start(
    context: *const Context,
    state: *mut MapHashState,
) {
    unsafe { state.write((*context).map_hash_key.state()) }
}

/// # Safety
/// State points to initialized, exclusive, aligned scratch.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_map_hash_add_word(state: *mut MapHashState, word: u64) {
    unsafe { (*state).word(word) }
}

/// # Safety
/// State points to initialized, exclusive, aligned scratch. A non-empty input
/// is readable for its length and disjoint from the state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_map_hash_add_bytes(
    state: *mut MapHashState,
    data: *const u8,
    len: u64,
) {
    unsafe { (*state).bytes(map_hash_range(data, len)) }
}

/// # Safety
/// State points to initialized, aligned scratch.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_map_hash_finish(state: *const MapHashState) -> u64 {
    unsafe { (*state).finish() }
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
    // SAFETY: caller's event word pins this task and sink through the callback.
    unsafe {
        if crate::tasks::arc_unobserved(context) {
            return;
        }
        crate::tasks::event_in(context, crate::tasks::ARC_NEW, 0, 1);
    }
}
/// Complete an emitted Arc increment. No status/admission/allocation.
/// # Safety
/// As for arc_initialized; node is live and its checked increment completed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_arc_shared(context: *const Context, node: *const u8) {
    count_arc_hook();
    // SAFETY: caller holds the node and the recorded event turn.
    unsafe {
        if crate::tasks::arc_unobserved(context) {
            return;
        }
        let count = node.cast::<u64>().read();
        crate::tasks::event_in(context, crate::tasks::ARC_SHARE, 0, count);
    }
}
/// Complete an emitted Arc decrement before last-release glue/refunds.
/// # Safety
/// As for arc_initialized; last exactly describes the completed decrement.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_arc_released(context: *const Context, last: bool) {
    // The outlined release has already recorded its event.
    if unsafe { (*((*context).events as *const crate::tasks::Task)).arc_runtime } {
        return;
    }
    count_arc_hook();
    // SAFETY: the event word pins this immutable task/sink until completion.
    unsafe {
        if crate::tasks::arc_unobserved(context) {
            return;
        }
        crate::tasks::event_in(context, crate::tasks::ARC_RELEASE, 0, u64::from(last));
    }
}
/// Complete an emitted Arc acquire uniqueness test.
/// # Safety
/// As for arc_initialized; unique is the completed test result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_arc_unwrapped(context: *const Context, unique: bool) {
    count_arc_hook();
    // SAFETY: the event word pins this immutable task/sink until completion.
    unsafe {
        crate::tasks::event_in(context, crate::tasks::ARC_UNWRAP, 0, u64::from(unique));
    }
}

/// # Safety
/// Node is fresh PLAIN or GRAPH storage for a FORM count word and payload at offset 8.
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
/// An inline share wrapped a count past `2^64 - 1`: the FAIL-15 run stop.
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_count_overflow() -> ! {
    crate::run_stop::raise(crate::run_stop::Stop::from_static(b"CountOverflow"))
}
/// # Safety
/// A live holder pins node, under the same count/event discipline as share.
/// True transfers exclusive payload custody for copy plus free; false preserves h.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_try_unwrap(node: NonNull<u8>, atomic: bool) -> bool {
    // SAFETY: the caller owns a live holder in its provider event.
    unsafe { counted::try_unwrap(node, atomic) }
}
/// On replay the dispatcher grants the release before the count changes.
/// The emitter still reports the completed event.
/// # Safety
/// Consumes a live Arc holder under the current task's event discipline.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_arc_decrement_for_replay(node: NonNull<u8>) -> bool {
    // SAFETY: the runtime path decides and records the release in one commit.
    unsafe { counted::decrement(node, true) }
}

#[repr(C)]
pub struct Allocation {
    pub pointer: *mut u8,
    pub capacity: Capacity,
}

impl From<Result<NonNull<u8>, Capacity>> for Allocation {
    fn from(result: Result<NonNull<u8>, Capacity>) -> Self {
        match result {
            Ok(pointer) => Self {
                pointer: pointer.as_ptr(),
                capacity: Capacity {
                    requested: 0,
                    available: 0,
                },
            },
            Err(capacity) => Self {
                pointer: ptr::null_mut(),
                capacity,
            },
        }
    }
}

mod byte_buffers;
pub use byte_buffers::*;

/// The consuming grow row's two outputs; both targets return them in registers.
#[repr(C)]
pub struct Growth {
    pub pointer: *mut u8,
    pub available: u64,
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
/// Context is live and exclusively used by its task. Size, alignment and kind
/// are the emitter's native layout.
/// Overflow denotes an exact logical charge greater than u64::MAX. The caller
/// owns a successful pointer and must free it once after cleaning its payload.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_alloc(
    context: *const Context,
    _reserved: *mut u8,
    bytes: usize,
    align: usize,
    charge: u64,
    overflow: bool,
    kind: u8,
) -> Allocation {
    // SAFETY: the emitter supplies a live context.
    let context = unsafe { &*context };
    match crate::block::allocate(
        context,
        context.payer(),
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
    // SAFETY: the running task owns this context.
    scope_allocation(context, limit_enter(unsafe { &mut *context }, bytes))
}

#[inline]
pub(crate) fn limit_enter(task: &mut Context, bytes: u64) -> Result<(), Capacity> {
    let result = if let Some(slot) = task.current.reusable_slot() {
        // No operation between selecting and consuming the slot can suspend
        // on success. A refused admission does not consume the selected slot.
        let result = task.current.enter_reused(bytes, slot);
        if result.is_ok() {
            task.bound_depth = task.bound_depth.wrapping_add(1);
        }
        result
    } else {
        task.limit_enter(bytes)
    };
    #[cfg(feature = "prof")]
    crate::mem_prof::byte_scope(task.limit_record().addr(), bytes, result.as_ref().err());
    result
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
        (*context).current.drain_in_operation();
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

/// Admit `charge` against the current limit for storage the emitter already holds
/// (a frame-backed list's bytes or log node), allocating nothing. The charge
/// pins no record (`Payer::admit_frame`); `hirsel_core_frame_refund` returns it.
/// On success the pointer is the context; on refusal it is null with the
/// Capacity, as `hirsel_core_alloc`.
/// # Safety
/// Context is live and exclusively used by its task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_frame_admit(
    context: *const Context,
    charge: u64,
    overflow: bool,
) -> Allocation {
    // SAFETY: the emitter supplies a live context.
    let task = unsafe { &*context };
    match task.payer().admit_frame((!overflow).then_some(charge)) {
        Ok(()) => Allocation {
            pointer: context.cast_mut().cast(),
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

/// Drop one initialized FrozenBytes carrier, settling a heap buffer's charge.
/// # Safety
/// The emitter owns this live carrier exclusively, in a live task context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_drop_frozen_bytes(
    context: *const Context,
    carrier: *const u8,
) {
    // SAFETY: forwarded to `causes::drop_frozen_bytes` with the same preconditions.
    unsafe { crate::causes::drop_frozen_bytes(context, carrier) };
}

/// Admit frame storage and publish its backing record for mutating callees.
/// # Safety
/// The task and the slot (payload plus allocation link) are live and exclusive.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_admit_lent_frame(
    context: *const Context,
    frame: NonNull<u8>,
    bytes: usize,
    charge: u64,
    overflow: bool,
) -> Allocation {
    // SAFETY: the caller owns the live context and the aligned frame slot.
    let task = unsafe { &*context };
    let payer = task.payer();
    match crate::block::admit_storage(payer, (!overflow).then_some(charge)) {
        Ok(()) => {
            unsafe {
                task.frame_lists.insert(
                    frame,
                    bytes,
                    payer,
                    (task.allocation_custody != 0).then_some(charge),
                )
            };
            Allocation {
                pointer: frame.as_ptr(),
                capacity: Capacity {
                    requested: 0,
                    available: 0,
                },
            }
        }
        Err(capacity) => Allocation {
            pointer: ptr::null_mut(),
            capacity,
        },
    }
}

pub use crate::frame_list::hirsel_core_promote_frame_list;

/// Refund through an original payer after emitted physical deallocation. Remote
/// payers use the event mailbox. Admits/allocates nothing, no failure status.
/// # Safety
/// Payer is pinned by these nonzero outstanding bytes, settled exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_refund(payer: *mut crate::limit::LimitRecord, bytes: u64) {
    crate::limit::Payer::from_current(payer).refund(bytes);
}

/// Return a frame charge that `hirsel_core_frame_admit` or an inline frame
/// admission took, to the current limit. No status, admission or allocation.
/// # Safety
/// Context is task-exclusive, in the body that admitted these exact bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_frame_refund(context: *const Context, bytes: u64) {
    // SAFETY: the emitter supplies a live context.
    unsafe { (*context).payer().refund_frame(bytes) }
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
            (*context).current.drain_at_bind();
        }
    }
}

/// Allocate physical backing after a positive charge was admitted inline.
#[unsafe(no_mangle)]
/// # Safety
/// Live task context and payer with the block's outstanding charge. Size,
/// alignment and kind are the trusted native operands.
pub unsafe extern "C" fn hirsel_core_alloc_admitted(
    context: *const Context,
    payer: *mut crate::limit::LimitRecord,
    bytes: usize,
    align: usize,
    kind: u8,
) -> *mut u8 {
    // SAFETY: caller owns the task and the positive admission pins the payer.
    unsafe {
        crate::block::allocate_admitted(
            &*context,
            crate::limit::Payer::from_current(payer),
            bytes,
            align,
            kind,
        )
        .as_ptr()
    }
}

/// # Safety
/// The live block pins its original payer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_block_payer(
    block: NonNull<u8>,
) -> *mut crate::limit::LimitRecord {
    // SAFETY: the block owns a live slot in its allocation page.
    unsafe { crate::block::funding(block).record_pointer() }
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

/// Free a wholly destructured own value or resource.
/// # Safety
/// The block is exclusively owned, all fields have moved or been cleaned, and a
/// resource has not begun finalization. Charge is its remaining logical charge.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_free_value(
    context: *const Context,
    pointer: NonNull<u8>,
    charge: u64,
) {
    // SAFETY: the emitter supplies the live block with the contract above.
    unsafe {
        let context = &*context;
        if let Some((payer, _)) = context.frame_lists.remove(pointer) {
            payer.settle_in(context, charge);
        } else {
            causes::free_value(context, pointer, charge);
        }
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
        context,
        context.payer(),
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
    _bytes: usize,
    _align: usize,
) {
    // SAFETY: the emitter transfers the exclusively owned block and native operands.
    unsafe {
        crate::block::free_in(context, pointer, charge);
    }
}

/// Enqueue owned sequence storage while its descriptor remains in its parent.
/// # Safety
/// The GRAPH buffer is exclusively owned. The borrowed payload is in the live parent
/// worklist frame and remains alive through this child's depth-first cleanup.
/// Recipe follows the ordinary resumable mechanical recipe contract. The
/// carrier capacity is nonzero; a static capacity-zero pointer is not a block.
/// Point is the running fixed parent's suspension point, or 0 under a buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_release_buffer_enqueue(
    list: *mut Worklist,
    buffer: NonNull<u8>,
    charge: u64,
    payload: *mut u8,
    recipe: Recipe,
    point: u64,
) {
    // SAFETY: the emitter transfers the buffer while its parent pins the descriptor.
    unsafe {
        (*list).stage_buffer(buffer, charge, recipe, payload, point);
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
    unsafe { (&*context).payer().injected() }
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
        crate::task_heap::release(context, pointer);
    }
}

/// The one release of a block that owns more than terminal storage: a Box, a
/// last Rc/Arc holder or anything below them, cleaned by the constant-stack
/// drain (LANG-19). Returns whether a finalizer or handle failed (FAIL-10).
/// No admission or extra count/event.
/// # Safety
/// The caller transfers exclusive dead fixed ownership (for a counted node,
/// after its count operation and event); the descriptor is immutable, exact and
/// lives as long as the compiled program.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_release(
    context: *mut Context,
    node: NonNull<u8>,
    descriptor: *const crate::block::Descriptor,
    site: u64,
) -> bool {
    // SAFETY: caller transfers the fixed block and its concrete descriptor.
    unsafe { counted::drain(context, node, descriptor, site) }
}
/// Free one dead box that compiled glue cleaned in place instead of handing it
/// to the drain, exactly as the drain frees the blocks it cleans.
/// # Safety
/// The block is exclusively owned, its fields are cleaned, and `charge` is its
/// exact box charge.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_free_box(
    context: *const Context,
    node: NonNull<u8>,
    charge: u64,
) {
    // SAFETY: as this function's contract.
    unsafe { crate::block::free_dead_in(&*context, node, charge) }
}
/// The one release of non-terminal `Seq` or `Map` storage outside a drain (a
/// local's, or a field of a value ordinary glue cleans): `recipe` cleans its
/// elements through the constant-stack drain, then the storage is freed and
/// refunded (LANG-19). Returns whether a finalizer or handle failed (FAIL-10).
/// # Safety
/// The caller transfers the exclusively owned GRAPH buffer of a carrier with
/// nonzero capacity; the carrier stays put until this returns; charge and
/// recipe are exact for its element type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_release_buffer(
    context: *mut Context,
    buffer: NonNull<u8>,
    charge: u64,
    carrier: *mut u8,
    recipe: Recipe,
    site: u64,
) -> bool {
    // SAFETY: the caller transfers the buffer; its carrier outlives the drain.
    unsafe { counted::drain_buffer(context, buffer, charge, recipe, carrier, site) }
}
/// The one ordinary release of a resource (LANG-19): `recipe` runs its trace
/// hook and finalizer, then cleans its fields through the constant-stack
/// drain, and the resource is freed with `storage`, its logical field storage.
/// Returns whether its finalizer or anything it cleaned failed (FAIL-10).
/// # Safety
/// The caller transfers the exclusively owned resource; `recipe` and `storage`
/// are exact for its type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_resource_release(
    context: *mut Context,
    resource: NonNull<u8>,
    recipe: Recipe,
    storage: u64,
    site: u64,
) -> bool {
    // SAFETY: as this function's contract.
    unsafe {
        let holder = crate::causes::resource_begin(resource, recipe, storage);
        counted::drain_holder(context, holder, crate::causes::resource_step, site)
    }
}
/// Hand a resource over to the running drain at `point`, as
/// hirsel_core_resource_release would clean it.
/// # Safety
/// As hirsel_core_resource_release; `list` is the running drain's and nothing
/// is staged on it yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_resource_stage(
    resource: NonNull<u8>,
    recipe: Recipe,
    storage: u64,
    list: *mut counted::Worklist,
    point: u64,
) {
    // SAFETY: as this function's contract.
    unsafe {
        let holder = crate::causes::resource_begin(resource, recipe, storage);
        (*list).stage_holder(holder, crate::causes::resource_step, point);
    }
}
/// Full-new admission, replacement, carrier install, then original-payer
/// refund, for every sequence growth an emitter does not complete inline.
/// # Safety
/// The consuming sequence carrier's data and nonzero capacity words are
/// exclusively owned. The length word is unused and may be uninitialized.
/// Layout, stride, kind, charges and moved bytes are the exact FORM operands.
#[allow(clippy::too_many_arguments)]
#[cold]
#[inline(never)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_grow_cold(
    context: *const Context,
    carrier: *mut u8,
    bytes: usize,
    align: usize,
    stride: u64,
    charge: u64,
    capacity: u64,
    moved: usize,
    refund: u64,
    overflow: bool,
    kind: u8,
) -> Growth {
    // SAFETY: the emitter provides a live context and the exclusive carrier.
    growth_result(unsafe {
        crate::growth::grow_cold(
            &*context, carrier, bytes, align, stride, charge, capacity, moved, refund, overflow,
            kind,
        )
    })
}

/// The small, plain byte case of `hirsel_core_grow_cold`.
/// # Safety
/// As for `hirsel_core_grow_cold`. The new native size equals `capacity` and
/// fits the short slab threshold, alignment is eight, the backing is below
/// the dedicated growth threshold, and charge arithmetic has not overflowed.
#[inline(never)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_grow_small_bytes(
    context: *const Context,
    carrier: *mut u8,
    capacity: u64,
    charge: u64,
    moved: usize,
    refund: u64,
) -> Growth {
    // SAFETY: the emitter provides a live context and the exclusive carrier.
    growth_result(unsafe {
        crate::growth::replace(
            &*context,
            carrier,
            capacity as usize,
            8,
            charge,
            capacity,
            moved,
            refund,
            false,
            crate::block::PLAIN,
            false,
        )
    })
}

#[inline]
fn growth_result(result: Result<NonNull<u8>, Capacity>) -> Growth {
    match result {
        Ok(pointer) => Growth {
            pointer: pointer.as_ptr(),
            available: 0,
        },
        Err(capacity) => Growth {
            pointer: ptr::null_mut(),
            available: capacity.available,
        },
    }
}
