//! The io_uring backend of a [`Poller`](super::Poller): one ring per owner,
//! its mailbox, and the multishot polls of its watched descriptors.
use super::{Backend, INTEREST, POLL_BATCH, Poller, REGISTER_SEND_MSG_RING, REMOVED, WAKE, sendzc};
use std::{
    collections::{HashMap, VecDeque},
    io,
    os::fd::{AsRawFd, OwnedFd},
    sync::{Arc, Mutex},
};

/// Post a `WAKE` completion to the ring `fd` without a source ring
/// (`IORING_REGISTER_SEND_MSG_RING`, 6.13). The kernel posts it into the
/// target's completion queue at once and wakes a thread waiting there.
pub(super) fn msg_ring_wake(fd: i32) -> io::Result<()> {
    let entry = io_uring::opcode::MsgRingData::new(io_uring::types::Fd(fd), 0, WAKE, None).build();
    // SAFETY: the kernel reads one initialized submission entry.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_io_uring_register,
            -1,
            REGISTER_SEND_MSG_RING,
            (&raw const entry).cast::<libc::c_void>(),
            1,
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// An io_uring ring with one multishot poll per watched descriptor, set up
/// `COOP_TASKRUN | TASKRUN_FLAG`.
///
/// The ring belongs to one thread at a time, its owner: a unit pool slot's
/// worker thread, or a tied run's driver. A poll's readiness is task work on
/// the thread that armed it, which under `COOP_TASKRUN` runs at that thread's
/// next return to user space without an interrupt; so only the owner submits
/// (it arms every poll) and only the owner waits; a slot's thread never gives
/// its slot up. Other threads post entries to the mailbox and wake the owner
/// with a MSG_RING completion. The owner learns of pending task work from
/// `IORING_SQ_TASKRUN` without a syscall.
pub(super) struct Uring {
    pub(super) ring: io_uring::IoUring,
    /// The mailbox: entries for the owner to submit, in order.
    sq: Mutex<Submissions>,
    /// Lock before `tx` and `sq`.
    pub(super) watched: Mutex<HashMap<u64, Watch>>,
    /// Transmit slots and their zero-copy sends. Lock before `sq`.
    pub(super) tx: Mutex<sendzc::Table>,
    /// The kernel runs SEND_ZC.
    pub(super) zc: bool,
    /// A sparse registered-buffer table exists for transmit slots.
    pub(super) sparse: bool,
    #[cfg(test)]
    pub(super) refuse_submit: std::sync::atomic::AtomicBool,
}

pub(super) struct Watch {
    pub(super) fd: Arc<OwnedFd>,
}

struct Submission {
    entry: io_uring::squeue::Entry,
    /// The kernel resolves an fd number when it consumes the entry. Keep
    /// the descriptor open through a short or failed enter, even after close.
    _fd: Option<Arc<OwnedFd>>,
}

// The ring's reserved unit, with an index above every transmit slot's index.
const FILE: u64 = 0xffff_0000_8000_0000;
struct FileCompletion {
    context: *const (),
    complete: unsafe fn(*const (), i32),
}
// SAFETY: submit_file keeps the context live across the owner's turns.
unsafe impl Send for FileCompletion {}

#[derive(Default)]
struct Submissions {
    entries: VecDeque<Submission>,
    /// Prefix already visible in the SQ. The rest waits for space.
    published: usize,
    files: HashMap<u16, FileCompletion>,
}

impl Submissions {
    fn consumed(&mut self, remaining: usize) {
        self.entries.drain(..self.published - remaining);
        self.published = remaining;
    }
}
impl Uring {
    const ENTRIES: u32 = 64;
    /// NODROP retains completions past this capacity. The owner enters
    /// with GETEVENTS to flush them once reaping has made room.
    const COMPLETIONS: u32 = 4096;

    /// A ring whose kernel runs multishot polls, bounded waits and blind
    /// MSG_RING wakes. Any refusal (an old kernel, a seccomp filter,
    /// `io_uring_disabled`) is an error, and the caller uses epoll.
    pub(super) fn open() -> io::Result<Uring> {
        Self::with_cq_size(Self::COMPLETIONS)
    }

    pub(super) fn with_cq_size(completions: u32) -> io::Result<Uring> {
        use io_uring::{opcode, register::Probe};
        let unsupported = || io::Error::from(io::ErrorKind::Unsupported);
        let ring = io_uring::IoUring::builder()
            .setup_cqsize(completions)
            .setup_clamp()
            .setup_coop_taskrun()
            .setup_taskrun_flag()
            .build(Self::ENTRIES)?;
        if !ring.params().is_feature_ext_arg() || !ring.params().is_feature_nodrop() {
            return Err(unsupported());
        }
        let mut probe = Probe::new();
        ring.submitter().register_probe(&mut probe)?;
        if !probe.is_supported(opcode::PollAdd::CODE)
            || !probe.is_supported(opcode::PollRemove::CODE)
            || !probe.is_supported(opcode::MsgRingData::CODE)
            || !probe.is_supported(opcode::OpenAt2::CODE)
            || !probe.is_supported(opcode::Statx::CODE)
            || !probe.is_supported(opcode::Close::CODE)
        {
            return Err(unsupported());
        }
        ring.submitter().register_iowq_max_workers(&mut [2, 1])?;
        let zc = probe.is_supported(opcode::SendZc::CODE)
            && probe.is_supported(opcode::AsyncCancel::CODE);
        let sparse = zc
            && ring
                .submitter()
                .register_buffers_sparse(sendzc::REGISTERED)
                .is_ok();
        let uring = Uring {
            ring,
            sq: Mutex::default(),
            watched: Mutex::new(HashMap::new()),
            tx: Mutex::default(),
            zc,
            sparse,
            #[cfg(test)]
            refuse_submit: std::sync::atomic::AtomicBool::new(false),
        };
        // Every wake is a blind MSG_RING: it must post its completion.
        msg_ring_wake(uring.ring.as_raw_fd())?;
        // SAFETY: no poller exists yet; this is the only completion reader.
        let woken = unsafe { uring.ring.completion_shared() }
            .next()
            .is_some_and(|cqe| cqe.user_data() == WAKE);
        if woken { Ok(uring) } else { Err(unsupported()) }
    }

    pub(super) fn submit(&self) -> io::Result<usize> {
        #[cfg(test)]
        if self
            .refuse_submit
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(io::Error::from_raw_os_error(libc::EBUSY));
        }
        self.ring.submitter().submit()
    }

    /// Post an entry for the owner's next enter.
    pub(super) fn push(&self, entry: io_uring::squeue::Entry, fd: Option<Arc<OwnedFd>>) {
        let mut pending = self.sq.lock().unwrap_or_else(|e| e.into_inner());
        pending.entries.push_back(Submission { entry, _fd: fd });
    }

    /// # Safety
    /// As `Poller::submit_file`.
    pub(super) unsafe fn push_file(
        &self,
        unit: u16,
        entry: io_uring::squeue::Entry,
        context: *const (),
        complete: unsafe fn(*const (), i32),
    ) {
        let mut pending = self.sq.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            pending
                .files
                .insert(unit, FileCompletion { context, complete })
                .is_none()
        );
        pending.entries.push_back(Submission {
            entry: entry.user_data(FILE | u64::from(unit)),
            _fd: None,
        });
    }

    fn poll_add(tag: u64, watch: &Watch) -> io_uring::squeue::Entry {
        io_uring::opcode::PollAdd::new(io_uring::types::Fd(watch.fd.as_raw_fd()), INTEREST)
            .multi(true)
            .build()
            .user_data(tag)
    }

    /// Watch `fd` under `tag` with one multishot poll for every direction. A
    /// poll checks readiness as it is armed, so nothing before it is lost.
    pub(super) fn add(&self, tag: u64, fd: Arc<OwnedFd>) {
        let mut watched = self.watched.lock().unwrap_or_else(|e| e.into_inner());
        let watch = Watch { fd };
        self.push(Self::poll_add(tag, &watch), Some(watch.fd.clone()));
        watched.insert(tag, watch);
    }

    /// Stop watching `tag`, cancelling its poll. The poll holds its own file
    /// reference until it is retired. Returns whether a removal was posted.
    pub(super) fn remove(&self, tag: u64) -> bool {
        let mut watched = self.watched.lock().unwrap_or_else(|e| e.into_inner());
        let Some(watch) = watched.remove(&tag) else {
            return false;
        };
        drop(watch);
        self.push(
            io_uring::opcode::PollRemove::new(tag)
                .build()
                .user_data(REMOVED),
            None,
        );
        true
    }

    pub(super) fn due(&self) -> bool {
        // SAFETY: the owner is the only completion reader; reading the tail
        // and the flags does not consume anything.
        let mut cq = unsafe { self.ring.completion_shared() };
        cq.sync();
        if !cq.is_empty() {
            return true;
        }
        drop(cq);
        // SAFETY: only the owner writes the submission queue; this reads its flags.
        if unsafe { self.ring.submission_shared() }.taskrun() {
            return true;
        }
        !self
            .sq
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .is_empty()
    }

    /// The owner's enter: publish the mailbox into the SQ and enter once,
    /// blocking up to `timeout_ns` for a completion (None: without bound;
    /// Some(0): not at all, and only if there is something to submit or
    /// task work to run). Never holds the mailbox lock while blocked, so
    /// other threads can post meanwhile.
    pub(super) fn enter(&self, timeout_ns: Option<u64>) {
        let mut pending = self.sq.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the owner holds its turn, so it is the only SQ writer.
        let mut sq = unsafe { self.ring.submission_shared() };
        while let Some(submission) = pending.entries.get(pending.published) {
            // SAFETY: the mailbox owns descriptors and the transmit table
            // retains any bytes named by sends until completion. File callers
            // stay parked until their completion callback runs.
            if unsafe { sq.push(&submission.entry) }.is_err() {
                break;
            }
            pending.published += 1;
        }
        sq.sync();
        drop(pending);
        if timeout_ns == Some(0) && sq.is_empty() && !sq.taskrun() && !sq.cq_overflow() {
            return;
        }
        drop(sq);
        // A timeout, an interrupt or a full completion queue is a turn with
        // whatever completions are already posted.
        let _ = match timeout_ns {
            Some(0) => self.submit(),
            None => self.ring.submitter().submit_and_wait(1),
            Some(ns) => {
                let ts = io_uring::types::Timespec::new()
                    .sec(ns / 1_000_000_000)
                    .nsec((ns % 1_000_000_000) as u32);
                self.ring
                    .submitter()
                    .submit_with_args(1, &io_uring::types::SubmitArgs::new().timespec(&ts))
            }
        };
        let mut pending = self.sq.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: as above; the owner reads how much the kernel consumed.
        let mut sq = unsafe { self.ring.submission_shared() };
        sq.sync();
        pending.consumed(sq.len());
    }

    /// One owner turn into `out`. A timeout of Some(0) does not block and
    /// None blocks until a completion.
    pub(super) fn wait(&self, out: &mut [libc::epoll_event], timeout_ns: Option<u64>) -> usize {
        let limit = POLL_BATCH as usize;
        let mut remaining = limit;
        let n = self.reap(out, &mut remaining);
        self.trim();
        // Kept transmit slots are freed by a later turn, so do not sleep
        // past their trim.
        let trim_ns = sendzc::TRIM.as_nanos() as u64;
        let timeout_ns = if remaining < limit {
            Some(0)
        } else if self.tx_kept() && timeout_ns.is_none_or(|ns| ns > trim_ns) {
            Some(trim_ns)
        } else {
            timeout_ns
        };
        self.enter(timeout_ns);
        if n != 0 {
            return n;
        }
        let n = self.reap(out, &mut remaining);
        self.trim();
        n
    }

    /// Translate posted completions into `(tag, flags)` events. A poll that
    /// ends (no `F_MORE`) is re-armed through the mailbox unless a removal or
    /// an adoption replaced it: the kernel ends a multishot poll when the
    /// completion queue overflows, and cancels it when its arming thread exits.
    fn reap(&self, out: &mut [libc::epoll_event], remaining: &mut usize) -> usize {
        let watched = self.watched.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the owner holds its turn, the only completion reader.
        let mut cq = unsafe { self.ring.completion_shared() };
        cq.sync();
        let mut n = 0;
        while *remaining != 0 && n < out.len() {
            let Some(cqe) = cq.next() else { break };
            *remaining -= 1;
            let data = cqe.user_data();
            let (tag, flags) = match data {
                REMOVED => continue,
                WAKE => (WAKE, libc::EPOLLIN as u32),
                data if data & !u64::from(u16::MAX) == FILE => {
                    let file = self
                        .sq
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .files
                        .remove(&(data as u16))
                        .unwrap();
                    // SAFETY: submission pins the context until this CQE, and
                    // no further access to it follows the callback.
                    unsafe { (file.complete)(file.context, cqe.result()) };
                    continue;
                }
                data if sendzc::is_tx(data) => {
                    // A send's answer or notification readies its socket's writer.
                    match self.tx_complete(data, cqe.result(), cqe.flags()) {
                        Some(owner) => (owner, libc::EPOLLOUT as u32),
                        None => continue,
                    }
                }
                _ => {
                    let tag = data;
                    let Some(watch) = watched.get(&tag) else {
                        continue;
                    };
                    if !io_uring::cqueue::more(cqe.flags()) {
                        self.push(Self::poll_add(tag, watch), Some(watch.fd.clone()));
                    }
                    let res = cqe.result();
                    if res <= 0 {
                        continue;
                    }
                    (tag, res as u32)
                }
            };
            out[n] = libc::epoll_event {
                events: flags,
                u64: tag,
            };
            n += 1;
        }
        n
    }
}

impl Poller {
    /// Queue a one-shot file operation to this slot's io_uring. Epoll has no
    /// submission queue and returns false without taking the operation.
    ///
    /// # Safety
    /// The caller owns the running slot and has at most one file request per
    /// unit. All descriptors and memory named by `entry` and `context` must
    /// remain valid until completion, except a descriptor consumed by CLOSE.
    /// `complete` must publish the result without unwinding or polling.
    pub unsafe fn submit_file(
        &self,
        unit: u16,
        entry: io_uring::squeue::Entry,
        context: *const (),
        complete: unsafe fn(*const (), i32),
    ) -> bool {
        let Backend::Uring(ring) = &self.backend else {
            return false;
        };
        // SAFETY: the caller keeps the request live until the sole CQE.
        unsafe { ring.push_file(unit, entry, context, complete) };
        true
    }
}
