use super::super::REMOVED;
use super::{Ring, Submissions, Uring, enter_queue, push_entry, queue_due};
use io_uring::squeue;
use std::{
    collections::HashMap,
    io,
    os::fd::OwnedFd,
    sync::{Arc, Mutex, MutexGuard},
};

const FILE: u64 = 1 << 63;
/// Zero-copy sends in flight, counted as their result plus notification.
const TCP_DATA_CREDITS: usize = 4096;

struct FileCompletion {
    context: *const (),
    complete: unsafe fn(*const (), i32),
}
// SAFETY: the issuing unit keeps the reply live until its callback.
unsafe impl Send for FileCompletion {}

#[derive(Default)]
struct Credits {
    tcp_data: usize,
    files: HashMap<u16, FileCompletion>,
}

pub(in crate::reactor) struct Completion {
    pub(in crate::reactor) ring: Ring,
    pub(in crate::reactor) sparse: bool,
    sq: Mutex<Submissions>,
    credits: Mutex<Credits>,
}

impl Completion {
    pub(super) fn open(entries: u32, zc: bool) -> io::Result<Self> {
        let ring = Ring::new(Uring::ENTRIES, entries, false)?;
        ring.iowq_max_workers(2, 1)?;
        let sparse = zc && ring.buffers_sparse(super::sendzc::REGISTERED).is_ok();
        Ok(Self {
            ring,
            sparse,
            sq: Mutex::default(),
            credits: Mutex::default(),
        })
    }

    fn credits(&self) -> MutexGuard<'_, Credits> {
        self.credits
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Admit a zero-copy send: its result and its notification.
    pub(in crate::reactor) fn reserve_send(&self) -> bool {
        let mut credits = self.credits();
        if credits.tcp_data + 2 > TCP_DATA_CREDITS {
            return false;
        }
        credits.tcp_data += 2;
        true
    }

    pub(in crate::reactor) fn release_send(&self, data: usize) {
        self.credits().tcp_data -= data;
    }

    pub(in crate::reactor) fn push(&self, entry: squeue::Entry, fd: Option<Arc<OwnedFd>>) {
        push_entry(&self.sq, entry, fd);
    }

    pub(super) unsafe fn push_file(
        &self,
        unit: u16,
        entry: squeue::Entry,
        context: *const (),
        complete: unsafe fn(*const (), i32),
    ) {
        let reply = FileCompletion { context, complete };
        assert!(self.credits().files.insert(unit, reply).is_none());
        self.push(entry.user_data(FILE | u64::from(unit)), None);
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
        out: &mut [libc::epoll_event],
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
                data if data & FILE != 0 => {
                    let file = self.credits().files.remove(&(data as u16)).unwrap();
                    // SAFETY: the issuing unit pins the reply until this one callback.
                    unsafe { (file.complete)(file.context, cqe.result()) };
                }
                id => {
                    if let Some(tag) = owner.tx_complete(id, cqe.result(), cqe.flags()) {
                        out[n] = libc::epoll_event {
                            events: libc::EPOLLOUT as u32,
                            u64: tag,
                        };
                        n += 1;
                    }
                }
            }
        }
        n
    }
}
