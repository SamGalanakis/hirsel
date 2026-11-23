//! Exact u64 counts and an intrusive dead-node worklist. Rc and Arc counts are
//! plain words: an Arc never leaves its unit (CONC-2), and one worker runs a
//! unit at a time, so only that thread touches them. The event provider still
//! orders Arc operations for recording and replay (CONC-10).

use crate::Context;
use crate::abi_layout::{FIXED_POINT_DONE, FIXED_POINT_NO_INDEX, fixed_point};
use crate::block::{self, Descriptor, Point};
use std::ptr::NonNull;

/// Generated resumable mechanical glue. Cursor starts at zero in native node
/// bookkeeping. Clean fields in spec order, scanning nonlast holders locally.
/// Advance the cursor, enqueue at most
/// one descendant, then return false to resume after that descendant. Return
/// true only when all fields are clean and nothing was enqueued in this call.
/// Never recurse, allocate, fail, run user code, or free the current node.
pub type Recipe = unsafe extern "C" fn(*mut Context, *mut u8, *mut u64, *mut Worklist) -> bool;

/// One step of a runtime holder's cleanup through the active drain: clean what
/// the holder still owns, staging at most one descendant, and return false to
/// resume after it. Return true only once the holder has finished its own
/// completion (its state transition and refund); the holder may be gone then.
pub(crate) type Step = unsafe fn(*mut Context, *mut Holder, *mut Worklist) -> bool;

/// The frame of a runtime holder (a receiver's queue, an abandoned task's
/// result, a Latest's root) while it cleans through a drain. It lives in the
/// holder's own record, in words only its cleanup uses from then on.
#[repr(C)]
pub(crate) struct Holder {
    next: usize,
    step: Step,
}

/// Marks a staged holder in the worklist's descriptor word.
const HOLDER: *const Descriptor = std::ptr::without_provenance(8);

/// A drain's suspended blocks, newest first, and the descendant the running
/// glue has just handed over. A parent's frame is written only after its glue
/// returns, so the parent is always linked under the descendant (postorder).
/// Generated glue stages a descendant inline (abi_layout WORKLIST_*).
#[repr(C)]
pub struct Worklist {
    /// 0 when empty. With bit 0 set, the address of a suspended fixed block's
    /// A word; with bit 1 set, a holder's frame; otherwise a dead GRAPH
    /// buffer's payload.
    head: usize,
    /// The staged descendant, null when none.
    node: *mut u8,
    /// The staged fixed block's descriptor; null for a staged GRAPH buffer.
    descriptor: *const Descriptor,
    /// The running fixed block's suspension point for the staged descendant, 1-based.
    point: u64,
    /// The release's caller site word, passed to finalizers that inherit it.
    site: u64,
    /// Nonzero once a finalizer or handle cleaned by this drain has failed;
    /// recipes OR into it and continue (FAIL-10).
    failed: u8,
}
const _: () = assert!(std::mem::offset_of!(Worklist, head) == crate::abi_layout::WORKLIST_HEAD);
const _: () = assert!(std::mem::offset_of!(Worklist, node) == crate::abi_layout::WORKLIST_NODE);
const _: () =
    assert!(std::mem::offset_of!(Worklist, descriptor) == crate::abi_layout::WORKLIST_DESCRIPTOR);
const _: () = assert!(std::mem::offset_of!(Worklist, point) == crate::abi_layout::WORKLIST_POINT);
const _: () = assert!(std::mem::offset_of!(Worklist, site) == crate::abi_layout::WORKLIST_SITE);
const _: () = assert!(std::mem::offset_of!(Worklist, failed) == crate::abi_layout::WORKLIST_FAILED);

type Fixed = (NonNull<u8>, *const Descriptor, u64);

impl Worklist {
    /// Record a failure of something this drain cleaned (FAIL-10).
    #[cfg(test)]
    pub(crate) fn fail(&mut self) {
        self.failed = 1;
    }
    fn staged(&self) -> bool {
        !self.node.is_null()
    }
    /// Hand over an exclusively owned dead fixed block from point `point`.
    /// # Safety
    /// Glue calls this at most once per run, then returns false.
    pub(crate) unsafe fn stage_fixed(
        &mut self,
        node: NonNull<u8>,
        descriptor: *const Descriptor,
        point: u64,
    ) {
        debug_assert!(!self.staged());
        self.node = node.as_ptr();
        self.descriptor = descriptor;
        self.point = point;
    }
    /// Hand over buffer storage from point `point`; see block::dead_buffer.
    /// # Safety
    /// As for stage_fixed, plus block::dead_buffer's contract.
    pub(crate) unsafe fn stage_buffer(
        &mut self,
        buffer: NonNull<u8>,
        charge: u64,
        recipe: Recipe,
        payload: *mut u8,
        point: u64,
    ) {
        debug_assert!(!self.staged());
        // SAFETY: the caller transfers the exclusively owned buffer.
        unsafe { block::dead_buffer(buffer, charge, recipe, payload) };
        self.node = buffer.as_ptr();
        self.descriptor = std::ptr::null();
        self.point = point;
    }
    /// Hand over a runtime holder whose cleanup has begun; its next step runs
    /// once it is linked.
    /// # Safety
    /// As for stage_fixed; the holder stays live until its step returns true.
    pub(crate) unsafe fn stage_holder(&mut self, holder: *mut Holder, step: Step, point: u64) {
        debug_assert!(!self.staged());
        // SAFETY: the caller hands over the holder's frame words.
        unsafe { (*holder).step = step };
        self.node = holder.cast();
        self.descriptor = HOLDER;
        self.point = point;
    }
    /// Link a staged buffer or holder above the head; return a staged fixed
    /// block to run.
    unsafe fn take(&mut self) -> Option<Fixed> {
        let node = std::mem::replace(&mut self.node, std::ptr::null_mut());
        let buffer = NonNull::new(node)?;
        if self.descriptor.is_null() {
            // SAFETY: stage_buffer wrote this frame; the buffer is ours.
            unsafe { (*block::dead(buffer)).next = self.head };
            self.head = node as usize;
            None
        } else if self.descriptor == HOLDER {
            // SAFETY: stage_holder handed over this frame.
            unsafe { (*node.cast::<Holder>()).next = self.head };
            self.head = node as usize | 2;
            None
        } else {
            Some((buffer, self.descriptor, 0))
        }
    }
}

/// # Safety
/// The emitter initializes a newly allocated PLAIN or GRAPH node, count at 0 and payload at 8.
pub unsafe fn initialize(node: NonNull<u8>, arc: bool) {
    // SAFETY: node has fresh aligned storage for one count word.
    unsafe {
        if arc {
            crate::tasks::before_commit();
        }
        node.cast::<u64>().as_ptr().write(1);
        if arc {
            crate::tasks::event(crate::tasks::ARC_NEW, 0, 1);
        }
    }
}

/// # Safety
/// The caller holds a live holder. Rc operations are task-local; Arc operations
/// occur in the provider's event order. Payload mutation remains forbidden.
#[inline]
pub unsafe fn share(node: NonNull<u8>, arc: bool) {
    // SAFETY: a live holder keeps the initialized count word alive, and only
    // the thread running its unit touches it (CONC-2: an Arc is not cross).
    unsafe {
        if arc {
            crate::tasks::before_commit();
        }
        let count = &mut *node.cast::<u64>().as_ptr();
        // A wrap stops the run before any holder reads the wrapped word.
        *count = count
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"CountOverflow"));
        if arc {
            crate::tasks::event_committed(crate::tasks::ARC_SHARE, 0, *count);
        }
    }
}

/// # Safety
/// The caller holds a live holder and meets share's task/event discipline.
/// A true result transfers exclusive payload custody; the caller moves it out
/// and frees the node. A false result leaves the original holder unchanged.
#[inline]
pub unsafe fn try_unwrap(node: NonNull<u8>, arc: bool) -> bool {
    // SAFETY: the caller's holder pins the initialized count word.
    unsafe {
        if arc {
            crate::tasks::before_commit();
        }
        let unique = node.cast::<u64>().as_ptr().read() == 1;
        if arc {
            crate::tasks::event_committed(crate::tasks::ARC_UNWRAP, 0, u64::from(unique));
        }
        unique
    }
}

/// Consume one holder, returning whether the emitter must run last-release glue.
/// # Safety
/// A live holder is transferred under the Rc/Arc event discipline.
#[inline(always)]
pub unsafe fn decrement(node: NonNull<u8>, arc: bool) -> bool {
    // SAFETY: the transferred holder keeps this node's initialized count alive.
    unsafe {
        if arc {
            crate::tasks::before_commit();
        }
        let count = &mut *node.cast::<u64>().as_ptr();
        *count = count.wrapping_sub(1);
        let last = *count == 0;
        if arc {
            crate::tasks::event_committed(crate::tasks::ARC_RELEASE, 0, u64::from(last));
        }
        last
    }
}

/// Clean and free a dead fixed block and everything it owns, in postorder,
/// with constant native stack and no allocation. Returns whether a finalizer
/// or handle it cleaned failed; cleanup went on regardless (FAIL-10).
/// # Safety
/// Count is zero or the block is uniquely owned; the descriptor is exact and
/// lives as long as the compiled program; the context is live.
pub(crate) unsafe fn drain(
    context: *mut Context,
    node: NonNull<u8>,
    descriptor: *const Descriptor,
    site: u64,
) -> bool {
    let mut list = Worklist::new(site);
    // SAFETY: as this function's contract; the new list's head is empty.
    unsafe {
        clean_fixed(context, &mut list, node, descriptor, 0);
        list.failed != 0
    }
}

/// Fixed blocks this deep below a drain's root are cleaned by the native
/// stack; deeper ones go to `run`'s constant-stack worklist. The bound keeps the
/// drain's stack constant (LANG-19) at a few KiB.
const NATIVE_DEPTH: u32 = 32;

/// Clean and free one dead fixed block and everything below it, in the
/// postorder `run` uses, recursing natively for descendants above
/// NATIVE_DEPTH: a descendant cleaned here needs no frame written into its
/// parent's payload and no resume through the worklist head.
/// # Safety
/// As drain; the list's head is empty and nothing is staged.
unsafe fn clean_fixed(
    context: *mut Context,
    list: &mut Worklist,
    node: NonNull<u8>,
    descriptor: *const Descriptor,
    depth: u32,
) {
    // SAFETY: as this function's contract; glue hands over one exclusively
    // owned descendant per call and leaves the parent's fields it has not
    // cleaned untouched, so the parent resumes from its live cursor.
    unsafe {
        let d = &*descriptor;
        let payload = node.as_ptr().add(d.payload_offset as usize);
        let mut cursor = 0;
        while !(d.recipe)(context, payload, &mut cursor, list) {
            if !list.staged() {
                continue;
            }
            let p = &*descriptor
                .cast::<u8>()
                .add(fixed_point(list.point))
                .cast::<Point>();
            // The cursor `run` would resume this block with.
            if p.index == FIXED_POINT_NO_INDEX {
                cursor = p.cursor;
            }
            match list.take() {
                Some((child, child_descriptor, _)) if depth < NATIVE_DEPTH => {
                    clean_fixed(context, list, child, child_descriptor, depth + 1)
                }
                staged => {
                    run(context, list, staged);
                }
            }
            if cursor == FIXED_POINT_DONE {
                // Nothing follows this yield: the block is finished.
                break;
            }
        }
        debug_assert!(!list.staged() && list.head == 0);
        block::free_dead_in(&*context, node, d.charge);
    }
}

/// Clean the elements of dead non-terminal buffer storage held outside any
/// drain (a local's or an ordinary value's Seq or Map), then free it, with
/// constant native stack. Returns whether anything it cleaned failed (FAIL-10).
/// # Safety
/// As stage_buffer; the carrier stays put until this returns; the context is live.
pub(crate) unsafe fn drain_buffer(
    context: *mut Context,
    buffer: NonNull<u8>,
    charge: u64,
    recipe: Recipe,
    carrier: *mut u8,
    site: u64,
) -> bool {
    let mut list = Worklist::new(site);
    // SAFETY: as this function's contract.
    unsafe {
        list.stage_buffer(buffer, charge, recipe, carrier, 0);
        let current = list.take();
        run(context, &mut list, current)
    }
}

/// Run a holder's cleanup, begun by its caller, to the end with constant native
/// stack. `site` is the release's caller site word, for finalizers that inherit
/// it. Returns whether anything it cleaned failed (FAIL-10).
/// # Safety
/// The holder's cleanup has begun and its frame words are free; the context is live.
pub(crate) unsafe fn drain_holder(
    context: *mut Context,
    holder: *mut Holder,
    step: Step,
    site: u64,
) -> bool {
    let mut list = Worklist::new(site);
    // SAFETY: as this function's contract.
    unsafe {
        list.stage_holder(holder, step, 0);
        let current = list.take();
        run(context, &mut list, current)
    }
}

/// Clean a live value in a stack slot, using its ordinary resumable recipe.
pub(crate) unsafe fn drain_place(context: *mut Context, place: *mut u8, recipe: Recipe) {
    let mut list = Worklist::new(0);
    let mut cursor = 0;
    // SAFETY: the caller owns the slot until its exact recipe completes.
    unsafe {
        while !recipe(context, place, &mut cursor, &mut list) {
            let current = list.take();
            run(context, &mut list, current);
        }
    }
}

impl Worklist {
    fn new(site: u64) -> Self {
        Worklist {
            head: 0,
            node: std::ptr::null_mut(),
            descriptor: std::ptr::null(),
            point: 0,
            site,
            failed: 0,
        }
    }
}

#[inline(never)]
unsafe fn run(context: *mut Context, list: &mut Worklist, mut current: Option<Fixed>) -> bool {
    // SAFETY: glue keeps unfinished blocks live and hands over one exclusively
    // owned descendant per run; a frame occupies only words its glue no longer
    // reads (hirsel_core_layout::frames).
    unsafe {
        loop {
            if let Some((node, descriptor, mut cursor)) = current.take() {
                let d = &*descriptor;
                let payload = node.as_ptr().add(d.payload_offset as usize);
                if (d.recipe)(context, payload, &mut cursor, list) {
                    debug_assert!(!list.staged());
                    block::free_dead_in(&*context, node, d.charge);
                    continue;
                }
                if !list.staged() {
                    current = Some((node, descriptor, cursor));
                    continue;
                }
                let point = descriptor.cast::<u8>().add(fixed_point(list.point));
                let p = &*point.cast::<Point>();
                debug_assert_eq!(p.back as usize, fixed_point(list.point));
                debug_assert!(
                    p.index != FIXED_POINT_NO_INDEX
                        || p.cursor == cursor
                        || p.cursor == FIXED_POINT_DONE
                );
                if p.index != FIXED_POINT_NO_INDEX {
                    payload.add(p.index as usize).cast::<u64>().write(cursor);
                }
                payload.add(p.b as usize).cast::<usize>().write(list.head);
                let a = payload.add(p.a as usize);
                a.cast::<*const u8>().write(point);
                list.head = a as usize | 1;
                current = list.take();
            } else if list.head == 0 {
                return list.failed != 0;
            } else if list.head & 1 != 0 {
                let a = (list.head & !1) as *mut u8;
                let point = a.cast::<*const u8>().read();
                let p = &*point.cast::<Point>();
                let descriptor = point.sub(p.back as usize).cast::<Descriptor>();
                let payload = a.sub(p.a as usize);
                list.head = payload.add(p.b as usize).cast::<usize>().read();
                let cursor = if p.index == FIXED_POINT_NO_INDEX {
                    p.cursor
                } else {
                    payload.add(p.index as usize).cast::<u64>().read()
                };
                let node =
                    NonNull::new_unchecked(payload.sub((*descriptor).payload_offset as usize));
                if cursor == FIXED_POINT_DONE {
                    // Nothing follows this yield: the block is finished.
                    block::free_dead_in(&*context, node, (*descriptor).charge);
                } else {
                    current = Some((node, descriptor, cursor));
                }
            } else if list.head & 2 != 0 {
                let holder = (list.head & !2) as *mut Holder;
                // Read first: a finished step may have freed the holder.
                let next = (*holder).next;
                if ((*holder).step)(context, holder, list) {
                    debug_assert!(!list.staged());
                    list.head = next;
                } else {
                    current = list.take();
                }
            } else {
                let buffer = NonNull::new_unchecked(list.head as *mut u8);
                let frame = &mut *block::dead(buffer);
                if (frame.recipe)(context, frame.payload, &mut frame.cursor, list) {
                    list.head = frame.next;
                    block::free_dead_in(&*context, buffer, frame.charge);
                } else {
                    current = list.take();
                }
            }
        }
    }
}
