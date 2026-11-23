//! A transfer's moved task input (`hirsel_core_transfer`): where a task's
//! body reads its job once a transfer has handed the task to a new root.

use super::{Body, CURRENT, Context, returnable_waits, zero_block};
use std::ptr::{self, NonNull};

/// The input a task's body reads, by where its job is.
pub(super) enum Moved {
    /// The spawn input (`Task::input`): no transfer has run.
    Spawned,
    /// The input a transfer moved the job into. With a resume glue, the root
    /// reads nothing of its job once it has parked, so the first park frees
    /// the block and the root resumes through that glue (`Consumed`).
    Block {
        block: ZeroBlock,
        resume: Option<Body>,
    },
    /// The root's job is freed; the body is its resume glue, whose input is
    /// this code word alone. The spawn input is never read again.
    Consumed { code: u64 },
}
/// A task input a transfer moved its job into: a zero-charge block, like the
/// frame record, freed when the task ends, makes its next transfer, or first
/// parks in a root that reads nothing of its job after that.
pub(crate) struct ZeroBlock {
    pub(super) base: NonNull<u8>,
}
impl ZeroBlock {
    /// # Safety
    /// `context` is the running task's; nothing reads the block now.
    pub(super) unsafe fn release(self, context: &Context) {
        let base = self.base;
        std::mem::forget(self);
        // SAFETY: a zero-charge block of this context's heap.
        unsafe { crate::block::free_with(base, 0, Some(context)) }
    }
}
impl Drop for ZeroBlock {
    /// A task torn down without ending its body gives the block back with no
    /// context, as its frame record does.
    fn drop(&mut self) {
        // SAFETY: a zero-charge block; nothing reads it now.
        unsafe { crate::block::free(self.base, 0) }
    }
}
/// Move the running task's job, `bytes` packed as a task input (the code word,
/// then the job), into a fresh zero-charge input, and make `body` the task's
/// body once the current one has returned through its frames (`start`). The
/// caller's frames return at once and touch nothing after; `start` frees the
/// frame record (and an earlier moved input) before it enters `body` on the
/// same stack.
/// With `dead` nonzero and a `resume` glue, the new root reads nothing of its
/// job once it has parked: `start` frees the new input at that first park and
/// resumes the root through `resume`, which reads only the code word.
/// It declines, leaving the job where it is, when the task waits stackfully:
/// it has no frame record, or it is replaying (`returnable_waits`).
/// # Safety
/// Called by a represented transfer site, with `input` holding `bytes`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_transfer(
    context: *mut Context,
    input: *const u8,
    bytes: u64,
    body: Body,
    resume: Option<Body>,
    dead: u8,
) -> u8 {
    if !returnable_waits() {
        return 0;
    }
    let task = CURRENT.get();
    let bytes = usize::try_from(bytes).unwrap_or_else(|_| crate::process::stop(b"Transfer"));
    // SAFETY: CURRENT pins the task, and only its own fiber touches it; the
    // block holds `bytes` writable bytes.
    unsafe {
        let base = zero_block(&*context, bytes.max(8));
        ptr::copy_nonoverlapping(input, base.as_ptr(), bytes);
        debug_assert!((*task).transfer.is_none());
        (*task).transfer = Some((ZeroBlock { base }, body, resume.filter(|_| dead != 0)));
    }
    1
}
