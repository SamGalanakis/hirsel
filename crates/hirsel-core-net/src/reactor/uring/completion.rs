use super::super::{REMOVED, ReactorPlan};
use super::{Ring, Submissions, Uring, enter_queue, push_entry, queue_due};
use io_uring::squeue;
use std::{
    io,
    os::fd::OwnedFd,
    sync::{Arc, Mutex},
};

/// Zero-copy sends in flight, counted as their result plus notification.
const TCP_DATA_CREDITS: usize = 4096;

pub(in crate::reactor) struct Completion {
    pub(in crate::reactor) ring: Ring,
    pub(in crate::reactor) sparse: bool,
    sq: Mutex<Submissions>,
    credits: std::sync::atomic::AtomicUsize,
}

impl Completion {
    pub(super) fn open(plan: ReactorPlan, zc: bool) -> io::Result<Self> {
        let ring = Ring::new(plan.sq_entries, plan.completion_cq, false)?;
        ring.iowq_max_workers(plan.iowq_bounded, plan.iowq_unbounded)?;
        let sparse = zc && ring.buffers_sparse(super::sendzc::REGISTERED).is_ok();
        Ok(Self {
            ring,
            sparse,
            sq: Mutex::default(),
            credits: Default::default(),
        })
    }

    pub(in crate::reactor) fn reserve_send(&self) -> bool {
        self.credits
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |used| (used + 2 <= TCP_DATA_CREDITS).then_some(used + 2),
            )
            .is_ok()
    }

    pub(in crate::reactor) fn release_send(&self, data: usize) {
        self.credits
            .fetch_sub(data, std::sync::atomic::Ordering::Relaxed);
    }

    pub(in crate::reactor) fn push(&self, entry: squeue::Entry, fd: Option<Arc<OwnedFd>>) {
        push_entry(&self.sq, entry, fd);
    }

    /// Posted or overflowed completions, or entries to submit.
    pub(super) fn due(&self) -> bool {
        queue_due(&self.ring, &self.sq)
    }

    /// Submit without waiting; an overflowed CQ is flushed into the room
    /// reaping made (NODROP).
    pub(super) fn enter(&self, refuse_submit: bool) {
        let _ = enter_queue(&self.ring, &self.sq, Some(0), refuse_submit);
    }

    /// Every completion on this ring is the sole record of its operation, so
    /// one the kernel dropped (no memory for an overflow entry) fails the
    /// poller instead of leaving its waiter parked.
    pub(super) fn check_dropped(&self) -> io::Result<()> {
        if self.ring.dropped() != 0 {
            return Err(io::Error::from_raw_os_error(libc::ENOMEM));
        }
        Ok(())
    }

    pub(super) fn reap(
        &self,
        owner: &Uring,
        out: &mut [crate::ready::Event],
        left: &mut usize,
    ) -> usize {
        let mut n = 0;
        while *left != 0 && n < out.len() {
            // SAFETY: only the owner of this poller reads the completion queue.
            let Some(cqe) = (unsafe { self.ring.next() }) else {
                break;
            };
            *left -= 1;
            match cqe.user_data() {
                REMOVED => {}
                id => {
                    if let Some(tag) = owner.tx_complete(id, cqe.result(), cqe.flags()) {
                        out[n] = crate::ready::Event::new(tag, crate::ready::Ready::WRITABLE);
                        n += 1;
                    }
                }
            }
        }
        n
    }
}
