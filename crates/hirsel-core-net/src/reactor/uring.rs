//! The io_uring backend of a [`Poller`](super::Poller): one ring per owner,
//! its mailbox, and the multishot polls of its watched descriptors.
use super::{
    Backend, INTEREST, POLL_BATCH, PollEvents, Poller, REGISTER_SEND_MSG_RING, Turn, WAKE, sendzc,
};
use std::{
    collections::VecDeque,
    io,
    os::fd::{AsRawFd, BorrowedFd, OwnedFd},
    sync::{Arc, Mutex},
};
mod completion;
mod watch;
use completion::Completion;
use watch::{REMOVE, Watches};

pub(super) const COMPLETION_READY: u64 = u64::MAX - 2;

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
    pub(super) watched: Mutex<Watches>,
    /// Transmit slots and their zero-copy sends. Lock before `sq`.
    pub(super) tx: Mutex<sendzc::Table>,
    /// The kernel runs SEND_ZC.
    pub(super) zc: bool,
    pub(super) completion: Completion,
    #[cfg(test)]
    pub(super) refuse_submit: std::sync::atomic::AtomicBool,
}

struct Submission {
    entry: io_uring::squeue::Entry,
    /// The kernel resolves an fd number when it consumes the entry. Keep
    /// the descriptor open through a short or failed enter, even after close.
    _fd: Option<Arc<OwnedFd>>,
}

#[derive(Default)]
struct Submissions {
    entries: VecDeque<Submission>,
    /// Prefix already visible in the SQ. The rest waits for space.
    published: usize,
}

impl Submissions {
    fn consumed(&mut self, remaining: usize) {
        self.entries.drain(..self.published - remaining);
        self.published = remaining;
    }
}

fn push_entry(sq: &Mutex<Submissions>, entry: io_uring::squeue::Entry, fd: Option<Arc<OwnedFd>>) {
    let mut pending = sq.lock().unwrap_or_else(|error| error.into_inner());
    pending.entries.push_back(Submission { entry, _fd: fd });
}

fn queue_due(ring: &io_uring::IoUring, mailbox: &Mutex<Submissions>) -> bool {
    // SAFETY: the owner is the only completion reader and SQ writer.
    let cq = unsafe { ring.completion_shared() };
    if !cq.is_empty() {
        return true;
    }
    drop(cq);
    // SAFETY: no submissions or completions are read by another thread.
    let sq = unsafe { ring.submission_shared() };
    sq.taskrun()
        || sq.cq_overflow()
        || !mailbox
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entries
            .is_empty()
}

/// Publish one SQ batch and preserve the unconsumed prefix across short or failed enters.
fn enter_queue(
    ring: &io_uring::IoUring,
    mailbox: &Mutex<Submissions>,
    timeout_ns: Option<u64>,
    refuse_submit: bool,
) -> io::Result<usize> {
    let mut pending = mailbox.lock().unwrap_or_else(|error| error.into_inner());
    // SAFETY: the owner holds the turn and is the only SQ writer.
    let mut sq = unsafe { ring.submission_shared() };
    while let Some(submission) = pending.entries.get(pending.published) {
        // SAFETY: the mailbox pins each fd. Completion callers keep operand storage live
        // until their result, and zero-copy sends keep it through the notification.
        if unsafe { sq.push(&submission.entry) }.is_err() {
            break;
        }
        pending.published += 1;
    }
    sq.sync();
    drop(pending);
    if timeout_ns == Some(0) && sq.is_empty() && !sq.taskrun() && !sq.cq_overflow() {
        return Ok(0);
    }
    drop(sq);
    let submitter = ring.submitter();
    let result = match timeout_ns {
        Some(0) if refuse_submit => Err(io::Error::from_raw_os_error(libc::EBUSY)),
        Some(0) => submitter.submit(),
        None => submitter.submit_and_wait(1),
        Some(ns) => {
            let ts = io_uring::types::Timespec::new()
                .sec(ns / 1_000_000_000)
                .nsec((ns % 1_000_000_000) as u32);
            submitter.submit_with_args(1, &io_uring::types::SubmitArgs::new().timespec(&ts))
        }
    };
    let mut pending = mailbox.lock().unwrap_or_else(|error| error.into_inner());
    // SAFETY: only the owner reads how much of this submission batch the kernel consumed.
    let mut sq = unsafe { ring.submission_shared() };
    sq.sync();
    pending.consumed(sq.len());
    result
}

impl Uring {
    const ENTRIES: u32 = 64;
    /// NODROP retains completions past this capacity. The owner enters
    /// with GETEVENTS to flush them once reaping has made room.
    const READINESS_ENTRIES: u32 = 4096;
    const COMPLETION_ENTRIES: u32 = 65_536;

    /// A ring whose kernel runs multishot polls, bounded waits and blind
    /// MSG_RING wakes. Any refusal (an old kernel, a seccomp filter,
    /// `io_uring_disabled`) is an error, and the caller uses epoll.
    pub(super) fn open() -> io::Result<Uring> {
        Self::with_cq_size(Self::READINESS_ENTRIES)
    }

    pub(super) fn with_cq_size(completions: u32) -> io::Result<Uring> {
        Self::with_cq_sizes(completions, Self::COMPLETION_ENTRIES)
    }

    pub(super) fn with_cq_sizes(readiness: u32, completion: u32) -> io::Result<Uring> {
        use io_uring::{opcode, register::Probe};
        let unsupported = || io::Error::from(io::ErrorKind::Unsupported);
        let ring = io_uring::IoUring::builder()
            .setup_cqsize(readiness)
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
        let zc = probe.is_supported(opcode::SendZc::CODE)
            && probe.is_supported(opcode::AsyncCancel::CODE);
        let completion = Completion::open(completion, zc)?;
        // SAFETY: the completion ring owns this fd and pins it through the clone.
        let completion_fd = unsafe { BorrowedFd::borrow_raw(completion.ring.as_raw_fd()) };
        let completion_fd = Arc::new(completion_fd.try_clone_to_owned()?);
        let uring = Uring {
            ring,
            sq: Mutex::default(),
            watched: Mutex::default(),
            tx: Mutex::default(),
            zc,
            completion,
            #[cfg(test)]
            refuse_submit: std::sync::atomic::AtomicBool::new(false),
        };
        // Every wake is a blind MSG_RING: it must post its completion.
        msg_ring_wake(uring.ring.as_raw_fd())?;
        // SAFETY: no poller exists yet; this is the only completion reader.
        let woken = unsafe { uring.ring.completion_shared() }
            .next()
            .is_some_and(|cqe| cqe.user_data() == WAKE);
        if !woken {
            return Err(unsupported());
        }
        uring.add(COMPLETION_READY, completion_fd)?;
        Ok(uring)
    }

    fn refused_submit(&self) -> bool {
        #[cfg(test)]
        if self
            .refuse_submit
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return true;
        }
        false
    }

    /// Post a readiness entry for the owner's next enter.
    pub(super) fn push(&self, entry: io_uring::squeue::Entry, fd: Option<Arc<OwnedFd>>) {
        push_entry(&self.sq, entry, fd);
    }

    #[cfg(test)]
    pub(super) fn pending_submissions(&self) -> usize {
        self.sq
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len()
    }

    pub(super) fn due(&self) -> bool {
        queue_due(&self.ring, &self.sq) || self.watches().due() || self.completion.due()
    }

    pub(super) fn enter(&self, timeout_ns: Option<u64>) {
        let refused = self.refused_submit();
        self.completion.enter(refused);
        let timeout_ns = if self.completion.due() {
            Some(0)
        } else {
            timeout_ns
        };
        let result = enter_queue(&self.ring, &self.sq, timeout_ns, refused);
        if result.is_err_and(|error| error.raw_os_error() == Some(libc::EBADR)) {
            self.recover_watches(true);
        }
    }

    /// One owner turn into `out`. A timeout of Some(0) does not block and
    /// None blocks until a completion.
    pub(super) fn wait(
        &self,
        out: &mut [libc::epoll_event],
        timeout_ns: Option<u64>,
    ) -> io::Result<usize> {
        self.recover_watches(false);
        let limit = POLL_BATCH as usize;
        let mut remaining = limit;
        let mut completion_left = limit;
        let n = self.completion.reap(self, out, &mut completion_left);
        let n = n + self.reap(&mut out[n..], &mut remaining)?;
        let n = n + self.watches().recheck(&mut out[n..]);
        self.trim();
        // Kept transmit slots are freed by a later turn, so do not sleep
        // past their trim.
        let trim_ns = sendzc::TRIM.as_nanos() as u64;
        let recovering = self.submit_watch_recovery()?;
        let timeout_ns = if n != 0 || remaining < limit || recovering {
            Some(0)
        } else if self.tx_kept() && timeout_ns.is_none_or(|ns| ns > trim_ns) {
            Some(trim_ns)
        } else {
            timeout_ns
        };
        self.enter(timeout_ns);
        if n != 0 {
            return Ok(n);
        }
        let n = self.completion.reap(self, out, &mut completion_left);
        let n = n + self.reap(&mut out[n..], &mut remaining)?;
        self.trim();
        Ok(n + self.watches().recheck(&mut out[n..]))
    }

    /// Translate posted completions into `(tag, flags)` events. A poll that
    /// ends (no `F_MORE`) is re-armed through the mailbox unless a removal or
    /// an adoption replaced it: the kernel ends a multishot poll when the
    /// completion queue overflows, and cancels it when its arming thread exits.
    fn reap(&self, out: &mut [libc::epoll_event], remaining: &mut usize) -> io::Result<usize> {
        let mut watched = self.watches();
        // SAFETY: the owner holds its turn, the only completion reader.
        let mut cq = unsafe { self.ring.completion_shared() };
        cq.sync();
        let mut n = 0;
        while *remaining != 0 && n < out.len() {
            let Some(cqe) = cq.next() else { break };
            *remaining -= 1;
            let data = cqe.user_data();
            let (tag, flags) = match data {
                WAKE => (WAKE, libc::EPOLLIN as u32),
                data if data & REMOVE != 0 => {
                    watched.removed(data & !REMOVE, cqe.result())?;
                    continue;
                }
                _ => {
                    let res = cqe.result();
                    let Some(tag) = watched.polled(self, cqe)? else {
                        continue;
                    };
                    if res <= 0 || tag == COMPLETION_READY {
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
        Ok(n)
    }
}

impl Poller {
    pub(super) fn poll_uring<'a>(
        &'a self,
        mut events: Turn<'a>,
        timeout_ns: Option<u64>,
    ) -> io::Result<Option<PollEvents<'a>>> {
        let Backend::Uring(ring) = &self.backend else {
            unreachable!()
        };
        events.clear();
        crate::EPOLL_WAITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        events.resize(
            2 * POLL_BATCH as usize,
            libc::epoll_event { events: 0, u64: 0 },
        );
        let len = ring.wait(&mut events, timeout_ns)?;
        let len = self.dispatch_udp_slot(&mut events[..len]);
        events.truncate(len);
        Ok(Some(PollEvents(events)))
    }

    /// A unit pool slot's turn on its own io_uring ring: submit its mailbox
    /// and reap into `out`, blocking up to `timeout_ns` (None: until a
    /// completion; Some(0): not at all). Returns the events written.
    pub fn wait_slot(
        &self,
        out: &mut [libc::epoll_event],
        timeout_ns: Option<u64>,
    ) -> io::Result<usize> {
        let Backend::Uring(ring) = &self.backend else {
            unreachable!("a pool waits on an epoll slot poller directly")
        };
        let _turn = self.events.lock().unwrap_or_else(|e| e.into_inner());
        let timeout_ns = if self.udp_due() { Some(0) } else { timeout_ns };
        let len = ring.wait(out, timeout_ns)?;
        Ok(self.dispatch_udp_slot(&mut out[..len]))
    }

    /// io_uring: whether the owner has anything to take from its ring now: a
    /// posted completion, task work the kernel queued for it, or a mailbox
    /// entry to submit. Read without entering the kernel.
    pub fn due(&self) -> bool {
        let Backend::Uring(ring) = &self.backend else {
            return false;
        };
        ring.due() || self.udp_due()
    }
}

impl Poller {
    /// Queue a one-shot file operation to the slot's completion ring. It keeps its
    /// place until it can reserve a completion. Epoll leaves the operation to the caller.
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
        unsafe { ring.completion.push_file(unit, entry, context, complete) };
        true
    }
}
