//! Park a unit until its host operation publishes a completion.
use super::{Unit, cur, in_unit, park_unit, pool, pool_hooks, untied, wake, worker_ix};
use std::{
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) fn uses_file_ring_pool() -> bool {
    in_unit() && pool().uring
}

pub(crate) fn uses_file_ring() -> bool {
    untied() && (pool_hooks().uses_file_ring)()
}

/// Submit on this slot and park until its CQE, with the request still pinned.
///
/// # Safety
/// A ring unit is running. Every operand named by the SQE lives through
/// completion, and the entry generates exactly one CQE.
pub(super) unsafe fn file_request_pool(entry: hirsel_core_net::FileEntry) -> std::io::Result<i32> {
    use std::sync::atomic::AtomicI32;

    struct FileReply {
        result: AtomicI32,
        reply: Reply,
    }

    unsafe fn complete(context: *const (), result: i32) {
        // SAFETY: the unit's stack retains the cell until `Reply::wait` returns.
        let file = context.cast::<FileReply>();
        unsafe {
            (*file).result.store(result, Ordering::Relaxed);
            Reply::complete(&raw const (*file).reply);
        }
    }

    let answer = FileReply {
        result: AtomicI32::new(0),
        reply: Reply::new(),
    };
    let p = pool();
    p.net_used.store(true, Ordering::Relaxed);
    // SAFETY: `cur` is the running unit. It cannot leave the slot before
    // queuing the entry or leave this frame before its one completion.
    assert!(unsafe {
        p.slots[worker_ix()].poller.submit_file(
            (*cur()).id as u16,
            entry,
            (&raw const answer).cast(),
            complete,
        )
    });
    answer.reply.wait();
    let result = answer.result.load(Ordering::Relaxed);
    if result < 0 {
        Err(std::io::Error::from_raw_os_error(-result))
    } else {
        Ok(result)
    }
}

/// Submit through the pool's file ring. Callers use this only after
/// `uses_file_ring`; keeping the pool implementation behind the hook lets
/// executables without units dead-strip the pool.
///
/// # Safety
/// Same contract as the pool file request.
pub(crate) unsafe fn file_request(entry: hirsel_core_net::FileEntry) -> std::io::Result<i32> {
    // SAFETY: the caller establishes the pool file-ring contract.
    unsafe { (pool_hooks().file_request)(entry) }
}

/// A completion a unit parks on while another thread works for it.
pub(crate) struct Reply {
    done: AtomicBool,
    unit: *const Unit,
}
// SAFETY: `unit` names a leaked record; `done` is atomic.
unsafe impl Send for Reply {}
unsafe impl Sync for Reply {}

impl Reply {
    /// For the running unit, which must be inside the pool.
    pub(crate) fn new() -> Self {
        Self {
            done: AtomicBool::new(false),
            unit: cur(),
        }
    }

    /// Publish completion and wake the waiting unit. The caller does not
    /// touch the reply afterwards: the unit can resume and free it at once.
    /// # Safety
    /// `this` is a live reply that has not completed.
    pub(crate) unsafe fn complete(this: *const Reply) {
        // SAFETY: by the contract; the record outlives the reply.
        unsafe {
            let u = &*(*this).unit;
            (*this).done.store(true, Ordering::Release);
            wake(u);
        }
    }

    /// Park the running unit until `complete`. Any other wake of the unit
    /// (an arrival) parks it again: its tasks resume only after the call.
    /// The task's thread-locals and binding are put aside across the park, as
    /// at any other park, and restored on whichever thread resumed the unit.
    pub(crate) fn wait(&self) {
        use crate::tasks::{CALLBACK_TASK, CURRENT};
        let task = CURRENT.replace(ptr::null_mut());
        let callback = CALLBACK_TASK.replace(ptr::null_mut());
        let guard = crate::fiber::guarded();
        crate::fiber::guard((0, 0));
        crate::limit::preserve_binding(|| {
            while !self.done.load(Ordering::Acquire) {
                park_unit(None);
            }
        });
        crate::fiber::guard(guard);
        CALLBACK_TASK.set(callback);
        CURRENT.set(task);
    }
}
