//! MODE-6 recorders: a task-local queue of event images behind a resource handle.
//!
//! A recorder never leaves the invocation that made it and is never sendable, so its queue
//! is the running task's alone: no lock, no task operation and no yield. The handle word is
//! the queue's address tagged with [`RECORDER`]; the handle's CloseHandle step reaches
//! [`close`] through `run_close`, which drops the events still queued.
//!
//! An event crosses as the `Some` image of an `Option[E]`, as a channel message does: an
//! append copies it into the queue and leaves the open's `None` image in the caller's slot,
//! and a take moves the oldest event into a `None` slot.

use crate::channels::{Element, Ring};
use crate::context::TaskCtx;
use crate::{STATUS_OK, fault};

/// The tag of a recorder's handle word, below every task-runtime tag.
pub(crate) const RECORDER: u64 = 1 << 60;
/// The tags a recorder word must not carry: an endpoint, a task set or a mutex.
const OTHERS: u64 = crate::channels::ENDPOINT | crate::tasks::TASK_SET | crate::sync::MUTEX;

/// Whether `handle` is a recorder's word.
pub(crate) fn is_recorder(handle: u64) -> bool {
    handle & OTHERS == 0 && handle & RECORDER != 0
}

struct Recorder {
    element: Element,
    queue: Ring,
}

/// The recorder `r` borrows.
///
/// # Safety
/// `r` points at a live recorder handle word.
unsafe fn recorder<'a>(r: *const u64) -> &'a mut Recorder {
    // SAFETY: caller contract.
    let word = unsafe { r.read() };
    if !is_recorder(word) {
        fault("a recorder entry on a handle that is not a recorder");
    }
    // SAFETY: the word holds the address `open` boxed, live until `close`.
    unsafe { &mut *((word & !RECORDER) as *mut Recorder) }
}

/// `$intrinsic.RecorderOpen[E](none, descriptor)`: a fresh empty recorder.
///
/// # Safety
/// `ctx` is live, `out` writable, `descriptor` describes `Option[E]` and `none` its `None` image.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_recorder_open(
    ctx: *mut TaskCtx,
    _destination: *mut u8,
    out: *mut u64,
    none: *const u8,
    descriptor: u32,
) -> u8 {
    // SAFETY: caller contract.
    let layout = unsafe { (*ctx).descriptor(descriptor) };
    // SAFETY: caller contract.
    let element = unsafe { Element::new(descriptor, layout, none) };
    // SAFETY: a live descriptor.
    let queue = Ring::new(element.size, unsafe { (*layout).align } as usize);
    let address = Box::into_raw(Box::new(Recorder { element, queue })) as u64;
    if address & (OTHERS | RECORDER) != 0 {
        fault("a recorder queue above the handle tags");
    }
    // SAFETY: caller contract.
    unsafe { out.write(address | RECORDER) };
    STATUS_OK
}

/// `$intrinsic.RecorderAppend[E](r, mut event)`: queue the `Some` image at `event`, leaving
/// `None` there.
///
/// # Safety
/// `r` borrows a live recorder and `event` holds a `Some` image of its element.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_recorder_append(
    _ctx: *mut TaskCtx,
    r: *const u64,
    event: *mut u8,
) -> u8 {
    // SAFETY: caller contract.
    let recorder = unsafe { recorder(r) };
    let size = recorder.element.size;
    recorder.queue.push(event, size);
    // SAFETY: the slot is `size` bytes; the queue now owns its value.
    unsafe { std::ptr::copy_nonoverlapping(recorder.element.none.as_ptr(), event, size) };
    STATUS_OK
}

/// `$intrinsic.RecorderCount[E](r)`: the number of queued events.
///
/// # Safety
/// `out` is writable and `r` borrows a live recorder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_recorder_count(
    _ctx: *mut TaskCtx,
    out: *mut u64,
    r: *const u64,
) -> u8 {
    // SAFETY: caller contract.
    let recorder = unsafe { recorder(r) };
    // SAFETY: caller contract.
    unsafe { out.write(recorder.queue.len() as u64) };
    STATUS_OK
}

/// `$intrinsic.RecorderTake[E](r, mut event)`: move the oldest event into the `None` slot
/// `event`.
///
/// # Safety
/// `r` borrows a live recorder holding an event and `event` is a writable `None` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_recorder_take(
    _ctx: *mut TaskCtx,
    r: *const u64,
    event: *mut u8,
) -> u8 {
    // SAFETY: caller contract.
    let recorder = unsafe { recorder(r) };
    let size = recorder.element.size;
    if !recorder.queue.pop(event, size) {
        fault("a recorder take with no event queued");
    }
    STATUS_OK
}

/// Close the recorder `handle`: drop every event still queued, oldest first, then free the
/// queue. A finished recorder's queue is empty.
///
/// # Safety
/// `handle` is the word of a live recorder whose last owner is dropping it.
pub(crate) unsafe fn close(ctx: &mut TaskCtx, handle: u64, anchor: u64, operation_site: u64) -> u8 {
    // SAFETY: caller contract; the box is reclaimed exactly once, here.
    let recorder = unsafe { Box::from_raw((handle & !RECORDER) as *mut Recorder) };
    if recorder.element.inert {
        return STATUS_OK;
    }
    let mut status = STATUS_OK;
    for i in 0..recorder.queue.len() {
        let mut continuation = std::mem::MaybeUninit::<crate::finalize::Cleanup>::uninit();
        // SAFETY: each queued image owns its value, which nothing else reaches.
        status |= unsafe {
            crate::finalize::hirsel_rt_v2_finalize(
                ctx,
                recorder.element.descriptor,
                recorder.queue.at(i),
                std::ptr::null(),
                continuation.as_mut_ptr(),
                anchor,
                operation_site,
            )
        };
    }
    status
}
