use super::super::REMOVED;
use super::{Ring, Submissions, Uring, enter_queue, push_entry, queue_due};
use io_uring::squeue;
use std::{
    collections::{HashMap, VecDeque},
    io,
    os::fd::OwnedFd,
    sync::{Arc, Mutex, MutexGuard},
};

const FILE: u64 = 1 << 63;
const TCP_DATA_CREDITS: usize = 4096;
const TCP_SEND_CREDITS: usize = 3;

struct FileCompletion {
    context: *const (),
    complete: unsafe fn(*const (), i32),
}
// SAFETY: the issuing unit keeps the reply live until its callback.
unsafe impl Send for FileCompletion {}

struct FileRequest {
    unit: u16,
    entry: squeue::Entry,
    reply: FileCompletion,
}

#[derive(Default)]
struct Credits {
    held: usize,
    tcp_data: usize,
    files: HashMap<u16, FileCompletion>,
    waiting: VecDeque<FileRequest>,
}

pub(in crate::reactor) struct Completion {
    pub(in crate::reactor) ring: Ring,
    pub(in crate::reactor) sparse: bool,
    cq_entries: usize,
    sq: Mutex<Submissions>,
    credits: Mutex<Credits>,
}

impl Completion {
    pub(super) fn open(entries: u32, zc: bool) -> io::Result<Self> {
        let ring = Ring::new(Uring::ENTRIES, entries, false)?;
        ring.iowq_max_workers(2, 1)?;
        let sparse = zc && ring.buffers_sparse(super::sendzc::REGISTERED).is_ok();
        let cq_entries = ring.cq_entries();
        Ok(Self {
            ring,
            sparse,
            cq_entries,
            sq: Mutex::default(),
            credits: Mutex::default(),
        })
    }

    fn credits(&self) -> MutexGuard<'_, Credits> {
        self.credits
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Each send can produce a result, a notification and one cancel answer.
    /// Keep one entry available to file work even if no other producer can finish.
    pub(in crate::reactor) fn reserve_send(&self) -> bool {
        let mut credits = self.credits();
        if !credits.waiting.is_empty()
            || credits.held + TCP_SEND_CREDITS >= self.cq_entries
            || credits.tcp_data + 2 > TCP_DATA_CREDITS
        {
            return false;
        }
        credits.held += TCP_SEND_CREDITS;
        credits.tcp_data += 2;
        true
    }

    pub(in crate::reactor) fn release_send(&self, data: usize, control: usize) {
        let mut credits = self.credits();
        credits.held -= data + control;
        credits.tcp_data -= data;
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
        self.credits().waiting.push_back(FileRequest {
            unit,
            entry,
            reply: FileCompletion { context, complete },
        });
    }

    pub(super) fn due(&self) -> bool {
        if queue_due(&self.ring, &self.sq) {
            return true;
        }
        let credits = self.credits();
        credits.held < self.cq_entries && !credits.waiting.is_empty()
    }

    pub(super) fn enter(&self, refuse_submit: bool) {
        {
            let mut credits = self.credits();
            let admissions = (self.cq_entries - credits.held).min(Uring::ENTRIES as usize);
            for _ in 0..admissions {
                let Some(request) = credits.waiting.pop_front() else {
                    break;
                };
                credits.held += 1;
                assert!(credits.files.insert(request.unit, request.reply).is_none());
                self.push(
                    request.entry.user_data(FILE | u64::from(request.unit)),
                    None,
                );
            }
        }
        let _ = enter_queue(&self.ring, &self.sq, Some(0), refuse_submit);
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
                REMOVED => self.release_send(0, 1),
                data if data & FILE != 0 => {
                    let file = {
                        let mut credits = self.credits();
                        let file = credits.files.remove(&(data as u16)).unwrap();
                        credits.held -= 1;
                        file
                    };
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
