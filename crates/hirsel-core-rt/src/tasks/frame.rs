//! The frame record that a returnable task keeps between parks.

use super::{CURRENT, Context, Operation, zero_block};
use std::ptr::NonNull;

/// A task's frame record for the returnable form (DESIGN "Eager records"):
/// one block of the task's unit heap, funded by the task's payer at zero
/// logical charge and outside the observed retained backing, like the stack
/// it stands in for, made at the root's first
/// call and at a stable address until the task ends or a transfer hands it to
/// a new root (`hirsel_core_transfer`), which gets a fresh record at its own
/// first call. Its header holds the
/// owned operation state; the area after it holds the represented frames the
/// compiler lays out.
pub(super) struct FrameRecord {
    base: NonNull<u8>,
    #[cfg(debug_assertions)]
    size: usize,
}

/// The fixed prefix of a frame record. When a wait needs held state, the
/// state follows this prefix and the represented frames follow that state.
#[repr(C)]
struct FrameHeader {
    operation: std::mem::MaybeUninit<Operation>,
}
// A waiting send fits in the room the network waits already take.
const _: () = assert!(
    size_of::<crate::units_lang::SendRec>() <= size_of::<crate::net::OwnedNet>()
        && align_of::<crate::units_lang::SendRec>() <= 8
        && align_of::<crate::net::OwnedNet>() <= 8
);
pub(crate) const FRAME_HEADER: usize = size_of::<FrameHeader>();
const _: () = assert!(FRAME_HEADER % 8 == 0 && align_of::<FrameHeader>() <= 8);
impl FrameRecord {
    /// A record of `size` area bytes. Only the root's own resume word, the
    /// area's first, starts zeroed: every other word is written before it is
    /// read (a caller zeroes its callee's word before each fresh call), so
    /// testing builds fill the rest to catch a word read before it is set.
    fn new(context: &Context, size: usize, header_bytes: usize) -> Self {
        let bytes = header_bytes
            .checked_add(size.max(8))
            .unwrap_or_else(|| crate::process::stop(b"FrameRecord"));
        let base = zero_block(context, bytes);
        // SAFETY: the block holds `bytes` writable bytes, the word in bounds.
        unsafe {
            #[cfg(any(test, debug_assertions, feature = "testing"))]
            std::ptr::write_bytes(base.as_ptr(), 0xA5, bytes);
            base.as_ptr().add(header_bytes).cast::<u64>().write(0);
        }
        Self {
            base,
            #[cfg(debug_assertions)]
            size,
        }
    }
    /// Give the block back through the task's live context.
    /// # Safety
    /// `context` is the running task's; nothing borrows the record now.
    pub(super) unsafe fn release(self, context: &Context) {
        let base = self.base;
        std::mem::forget(self);
        // SAFETY: a zero-charge block of this context's heap.
        unsafe { crate::block::free_with(base, 0, Some(context)) }
    }
    fn header(&self) -> *mut FrameHeader {
        self.base.as_ptr().cast()
    }
    pub(super) fn operation(&self) -> *mut Operation {
        // SAFETY: the header is in bounds; MaybeUninit is repr(transparent).
        unsafe { (&raw mut (*self.header()).operation).cast() }
    }
}
impl Drop for FrameRecord {
    /// A task torn down without ending its body gives the block back with no
    /// context: the heap takes it as any other foreign return.
    fn drop(&mut self) {
        // SAFETY: a zero-charge block; nothing borrows it now.
        unsafe { crate::block::free(self.base, 0) }
    }
}
/// The current task's frame record of `size` area bytes: made at the root's
/// first call, and the same one at each resume, so a represented frame reads
/// back what it stored before its task returned the stack.
/// # Safety
/// Called by a returnable root in its own task, with the same size and flags
/// each time. `net` reserves room for either kind of held state, and `send`
/// reserves room for a connection send. The root and the represented calls
/// that use this record must reserve room for every kind of held state used.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_frame_record(
    context: *mut Context,
    size: u64,
    net: u8,
    send: u8,
) -> *mut u8 {
    let task = CURRENT.get();
    if task.is_null() {
        crate::process::stop(b"FrameRecord");
    }
    let size = usize::try_from(size).unwrap_or_else(|_| crate::process::stop(b"FrameRecord"));
    let held_bytes = if net != 0 {
        size_of::<crate::net::OwnedNet>()
    } else if send != 0 {
        size_of::<crate::units_lang::SendRec>()
    } else {
        0
    };
    let header_bytes = FRAME_HEADER + held_bytes.next_multiple_of(8);
    // SAFETY: CURRENT pins the task, and only its own fiber touches the record.
    // The same flags on each entry place the area within the same allocation.
    unsafe {
        let record = (*task)
            .frame
            .get_or_insert_with(|| FrameRecord::new(&*context, size, header_bytes));
        #[cfg(debug_assertions)]
        debug_assert_eq!(record.size, size);
        record.base.as_ptr().add(header_bytes)
    }
}

#[test]
fn frame_record_reserves_only_the_wait_state_its_root_needs() {
    unsafe extern "C" fn root(context: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: the test pins both flag bytes and a u64 result through the
        // root's lifetime. The requested area holds one u64.
        unsafe {
            let [net, send] = input.cast::<[u8; 2]>().read();
            let area = hirsel_core_frame_record(context, 8, net, send);
            let word = area.cast::<u64>();
            assert_eq!(word.read(), 0);
            word.write(42);
            assert_eq!(hirsel_core_frame_record(context, 8, net, send), area);
            assert_eq!(word.read(), 42);
            let header = frame_header().unwrap().cast::<u8>();
            out.cast::<u64>().write(area.offset_from(header) as u64);
        }
        0
    }

    let prefix = size_of::<Operation>() as u64;
    let send = size_of::<crate::units_lang::SendRec>().next_multiple_of(8) as u64;
    let net = size_of::<crate::net::OwnedNet>().next_multiple_of(8) as u64;
    assert_eq!(crate::tasks_tests::run(root, [0u8, 0]).0, prefix);
    assert_eq!(crate::tasks_tests::run(root, [0u8, 1]).0, prefix + send);
    assert_eq!(crate::tasks_tests::run(root, [1u8, 0]).0, prefix + net);
}

/// The current task's frame-record header, when it runs a returnable root.
fn frame_header() -> Option<*mut FrameHeader> {
    let task = CURRENT.get();
    // SAFETY: CURRENT pins the task.
    unsafe { task.as_ref()?.frame.as_ref().map(FrameRecord::header) }
}
/// The current task's held-wait room. Only one wait can use it at a time.
/// # Safety
/// The current task runs a returnable root that reserved room for `T`, either
/// `OwnedNet` or `SendRec`. Only that task may access the state during the wait.
pub(crate) unsafe fn held_wait<T>() -> *mut T {
    match frame_header() {
        // SAFETY: the reserved, 8-aligned state follows the fixed prefix.
        Some(header) => unsafe { header.add(1).cast() },
        None => crate::process::stop(b"FrameRecord"),
    }
}
