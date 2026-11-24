//! The io_uring reactor backend.
use super::{REMOVED, WAKE, WRITE_POLL, drain, eventfd, sendzc, signal, sys};
use std::{
    collections::{HashMap, VecDeque},
    io,
    os::fd::{AsRawFd, OwnedFd},
    sync::{Arc, Mutex},
};

/// An io_uring ring: one multishot poll on the wake eventfd, and a one-shot
/// poll per direction of a watched descriptor while a waiter is parked there.
/// Setup flags are 0: the driver token migrates between workers, which rules
/// out `SINGLE_ISSUER` and with it `DEFER_TASKRUN`, and `COOP_TASKRUN` would
/// leave a poll's completion unposted until its arming thread next enters the
/// kernel, up to a scheduler tick later while that thread computes.
pub(super) struct Uring {
    pub(super) ring: io_uring::IoUring,
    pub(super) wake: Arc<OwnedFd>,
    /// Serializes every submission-queue access; completions are read only
    /// under the driver turn.
    pub(super) sq: Mutex<Submissions>,
    /// Lock before `sq`.
    pub(super) watched: Mutex<HashMap<u64, Watch>>,
    /// The kernel ended the wake eventfd's multishot poll, which waits for
    /// completion-queue room to be re-armed. Touched only under the driver turn.
    pub(super) wake_ended: Mutex<bool>,
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
    /// Set when a waiter arms the direction and cleared when the driver reaps it.
    pub(super) polling: [bool; 2],
}

pub(super) struct Submission {
    pub(super) entry: io_uring::squeue::Entry,
    /// The kernel resolves an fd number when it consumes the entry. Keep
    /// the descriptor open through a short or failed enter, even after close.
    pub(super) _fd: Option<Arc<OwnedFd>>,
}

#[derive(Default)]
pub(super) struct Submissions {
    pub(super) entries: VecDeque<Submission>,
    /// Prefix already visible in the shared SQ. The rest waits for space.
    pub(super) published: usize,
}

impl Submissions {
    pub(super) fn consumed(&mut self, remaining: usize) {
        self.entries.drain(..self.published - remaining);
        self.published = remaining;
    }
}
impl Uring {
    const ENTRIES: u32 = 64;
    /// NODROP retains completions past this capacity. The submitter enters
    /// with GETEVENTS to flush them once reaping has made room.
    pub(super) const COMPLETIONS: u32 = 4096;
    const READ: u32 = (sys::EPOLLIN | sys::EPOLLRDHUP) as u32;
    const WRITE: u32 = sys::EPOLLOUT as u32;

    /// A ring whose kernel runs multishot polls and bounded waits, with its
    /// wake eventfd already watched. Any refusal (an old kernel, a seccomp
    /// filter, `io_uring_disabled`) is an error, and the caller uses epoll.
    pub(super) fn open() -> io::Result<Uring> {
        use io_uring::{cqueue, opcode, register::Probe};
        let unsupported = || io::Error::from(io::ErrorKind::Unsupported);
        let ring = io_uring::IoUring::builder()
            .setup_cqsize(Self::COMPLETIONS)
            .setup_clamp()
            .build(Self::ENTRIES)?;
        if !ring.params().is_feature_ext_arg() || !ring.params().is_feature_nodrop() {
            return Err(unsupported());
        }
        let mut probe = Probe::new();
        ring.submitter().register_probe(&mut probe)?;
        if !probe.is_supported(opcode::PollAdd::CODE)
            || !probe.is_supported(opcode::PollRemove::CODE)
        {
            return Err(unsupported());
        }
        let zc = probe.is_supported(opcode::SendZc::CODE)
            && probe.is_supported(opcode::AsyncCancel::CODE);
        let sparse = zc
            && ring
                .submitter()
                .register_buffers_sparse(sendzc::REGISTERED)
                .is_ok();
        let uring = Uring {
            ring,
            wake: Arc::new(eventfd()?),
            sq: Mutex::default(),
            watched: Mutex::new(HashMap::new()),
            wake_ended: Mutex::new(false),
            tx: Mutex::default(),
            zc,
            sparse,
            #[cfg(test)]
            refuse_submit: std::sync::atomic::AtomicBool::new(false),
        };
        uring.arm_wake();
        let wake = uring.wake.as_raw_fd();
        // A multishot poll must report a wake and stay armed.
        let one = 1u64;
        // SAFETY: nonblocking eventfd, readable eight-byte counter.
        if unsafe { libc::write(wake, (&one as *const u64).cast(), 8) } != 8 {
            return Err(io::Error::last_os_error());
        }
        uring.ring.submitter().submit_and_wait(1)?;
        // SAFETY: no poller exists yet; this is the only completion reader.
        let cqe = unsafe { uring.ring.completion_shared() }.next();
        drain(wake);
        match cqe {
            Some(cqe)
                if cqe.user_data() == WAKE
                    && cqe.result() > 0
                    && cqe.result() as u32 & sys::EPOLLIN as u32 != 0
                    && cqueue::more(cqe.flags()) =>
            {
                Ok(uring)
            }
            _ => Err(unsupported()),
        }
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

    pub(super) fn flush(&self, pending: &mut Submissions) {
        // SAFETY: the caller holds `sq`, the only submission-queue writer.
        let mut sq = unsafe { self.ring.submission_shared() };
        pending.consumed(sq.len());
        while !pending.entries.is_empty() {
            for submission in pending.entries.iter().skip(pending.published) {
                // SAFETY: the queue owns descriptors and the transmit table
                // retains any bytes named by the entry until completion.
                if unsafe { sq.push(&submission.entry) }.is_err() {
                    break;
                }
                pending.published += 1;
            }
            sq.sync();
            // Once published an entry can be consumed by any enter. Even a
            // failed enter leaves it owned by the queue. Only the shared head
            // tells us what was consumed while another thread could enter.
            let _ = self.submit();
            sq.sync();
            let progressed = sq.len() < pending.published;
            pending.consumed(sq.len());
            if !progressed || pending.published != 0 {
                break;
            }
        }
    }

    pub(super) fn push(&self, entry: io_uring::squeue::Entry, fd: Option<Arc<OwnedFd>>) {
        let mut pending = self.sq.lock().unwrap_or_else(|e| e.into_inner());
        pending.entries.push_back(Submission { entry, _fd: fd });
        self.flush(&mut pending);
        if !pending.entries.is_empty() {
            signal(self.wake.as_raw_fd());
        }
    }

    /// The wake eventfd is always writable; only its counter matters.
    pub(super) fn arm_wake(&self) {
        use io_uring::{opcode, types};
        self.push(
            opcode::PollAdd::new(types::Fd(self.wake.as_raw_fd()), sys::EPOLLIN as u32)
                .multi(true)
                .build()
                .user_data(WAKE),
            Some(self.wake.clone()),
        )
    }

    pub(super) fn poll_data(tag: u64, write: bool) -> u64 {
        if write { tag | WRITE_POLL } else { tag }
    }

    pub(super) fn add(&self, tag: u64, fd: Arc<OwnedFd>) {
        let mut watched = self.watched.lock().unwrap_or_else(|e| e.into_inner());
        watched.insert(
            tag,
            Watch {
                fd,
                polling: [false; 2],
            },
        );
    }

    /// Arm this direction's one-shot poll of the watched `tag` unless it is
    /// already in the kernel. A poll checks readiness as it is armed, so
    /// bytes that arrived after the waiter's EAGAIN complete it at once.
    pub(super) fn park(&self, tag: u64, write: bool) {
        use io_uring::{opcode, types};
        let mut watched = self.watched.lock().unwrap_or_else(|e| e.into_inner());
        let watch = watched.get_mut(&tag).unwrap();
        let polling = &mut watch.polling[usize::from(write)];
        if *polling {
            return;
        }
        let interest = if write { Self::WRITE } else { Self::READ };
        self.push(
            opcode::PollAdd::new(types::Fd(watch.fd.as_raw_fd()), interest)
                .build()
                .user_data(Self::poll_data(tag, write)),
            Some(watch.fd.clone()),
        );
        *polling = true;
    }

    /// Stop watching `tag`, cancelling its polls still in the kernel. Each
    /// holds its own file reference until it is retired.
    pub(super) fn remove(&self, tag: u64) {
        let mut watched = self.watched.lock().unwrap_or_else(|e| e.into_inner());
        let Some(watch) = watched.remove(&tag) else {
            return;
        };
        for (write, polling) in [false, true].into_iter().zip(watch.polling) {
            if polling {
                self.push(
                    io_uring::opcode::PollRemove::new(Self::poll_data(tag, write))
                        .build()
                        .user_data(REMOVED),
                    None,
                );
            }
        }
    }

    /// One kernel turn into `out`, under the driver turn. A timeout of 0 does
    /// not block and -1 blocks until a completion.
    pub(super) fn wait(&self, out: &mut [sys::epoll_event], timeout_ms: i32) -> usize {
        let n = self.reap(out);
        self.retry_submissions();
        self.rearm_wake();
        self.trim();
        if n != 0 {
            return n;
        }
        // Kept transmit slots are freed by a later turn, so do not sleep
        // past their trim.
        let trim_ms = sendzc::TRIM.as_millis() as i32;
        let timeout_ms = if self.tx_kept() && !(0..=trim_ms).contains(&timeout_ms) {
            trim_ms
        } else {
            timeout_ms
        };
        let submitter = self.ring.submitter();
        let entered = if timeout_ms == 0 {
            submitter.submit()
        } else if timeout_ms < 0 {
            submitter.submit_and_wait(1)
        } else {
            let ms = timeout_ms as u64;
            let ts = io_uring::types::Timespec::new()
                .sec(ms / 1000)
                .nsec((ms % 1000) as u32 * 1_000_000);
            submitter.submit_with_args(1, &io_uring::types::SubmitArgs::new().timespec(&ts))
        };
        // A timeout, an interrupt or a full completion queue is a turn with
        // whatever completions are already posted.
        let _ = entered;
        let n = self.reap(out);
        self.retry_submissions();
        self.rearm_wake();
        self.trim();
        n
    }

    pub(super) fn retry_submissions(&self) {
        let mut pending = self.sq.lock().unwrap_or_else(|e| e.into_inner());
        if !pending.entries.is_empty() {
            self.flush(&mut pending);
        }
    }

    /// Re-arm an ended wake poll once the completion queue is at most half
    /// full, under the driver turn. A ready eventfd posts as soon as its poll
    /// is armed; re-arming into a full queue ends that poll again at once,
    /// and the pair would cycle without end.
    pub(super) fn rearm_wake(&self) {
        let mut ended = self.wake_ended.lock().unwrap_or_else(|e| e.into_inner());
        if !*ended {
            return;
        }
        // SAFETY: the caller holds the driver turn, the only completion reader.
        let mut cq = unsafe { self.ring.completion_shared() };
        cq.sync();
        let room = cq.capacity() - cq.len() > cq.len();
        drop(cq);
        if room {
            self.arm_wake();
            *ended = false;
        }
    }

    /// Translate posted completions into `(tag, flags)` events.
    pub(super) fn reap(&self, out: &mut [sys::epoll_event]) -> usize {
        // SAFETY: the caller holds the driver turn, the only completion reader.
        let mut cq = unsafe { self.ring.completion_shared() };
        cq.sync();
        let mut n = 0;
        while n < out.len() {
            let Some(cqe) = cq.next() else { break };
            let data = cqe.user_data();
            let (tag, flags) = match data {
                REMOVED => continue,
                WAKE => {
                    if !io_uring::cqueue::more(cqe.flags()) {
                        // The kernel ended the wake poll (its arming thread
                        // exited, or the queue was full). Report a wake, so
                        // its waiter looks, and re-arm once there is room.
                        *self.wake_ended.lock().unwrap_or_else(|e| e.into_inner()) = true;
                    }
                    (WAKE, sys::EPOLLIN as u32)
                }
                data if sendzc::is_tx(data) => {
                    // A send's answer or notification readies its socket's writer.
                    match self.tx_complete(data, cqe.result(), cqe.flags()) {
                        Some(owner) => (owner, sys::EPOLLOUT as u32),
                        None => continue,
                    }
                }
                _ => {
                    let write = data & WRITE_POLL != 0;
                    let tag = data & !WRITE_POLL;
                    let mut watched = self.watched.lock().unwrap_or_else(|e| e.into_inner());
                    // A removed tag's cancelled poll reports nothing.
                    let Some(watch) = watched.get_mut(&tag) else {
                        continue;
                    };
                    watch.polling[usize::from(write)] = false;
                    // A poll that ended without readiness (cancelled because
                    // its arming thread exited, or refused) reports its
                    // direction ready: the waiter retries, sees the socket's
                    // own answer, and parks again from a live thread.
                    let res = cqe.result();
                    let flags = if res > 0 {
                        res as u32
                    } else if write {
                        Self::WRITE
                    } else {
                        Self::READ
                    };
                    (tag, flags)
                }
            };
            out[n] = sys::epoll_event {
                events: flags,
                u64: tag,
            };
            n += 1;
        }
        n
    }
}
