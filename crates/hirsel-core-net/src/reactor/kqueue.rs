//! The Darwin readiness provider: [`crate::ready`]'s events are kevents, and
//! the kqueue backend of a [`Poller`]. A descriptor's directions are two
//! filters (`EVFILT_READ`, `EVFILT_WRITE`) under one token, so a turn may
//! report a token twice; dispatch handles each report on its own. A wake is
//! an `EVFILT_USER` event, which needs no descriptor, and a child's exit is
//! `EVFILT_PROC` in the same queue.
use super::{Backend, POLL_BATCH, PollEvents, PollOperations, Poller, Turn, WAKE, checked};
use crate::ready::{Event, Trigger};
use std::{
    io,
    marker::PhantomData,
    mem::ManuallyDrop,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    sync::{Arc, Mutex, OnceLock},
};

pub(crate) type RawEvent = libc::kevent;
/// A wake is a user event: the queue that holds it and its identifier.
pub(crate) type WakerId = (RawFd, usize);

/// A selector's level-triggered user event.
pub(crate) struct WakeSource {
    kq: RawFd,
    ident: usize,
    token: u64,
}

/// The child a selector watches for exit: its process id, which stays the
/// child's until the parent reaps it.
#[derive(Clone, Copy, Debug)]
pub struct Child<'a>(libc::pid_t, PhantomData<&'a ()>);

impl Child<'_> {
    /// A child of this process that it has not reaped.
    pub fn new(pid: libc::pid_t) -> Self {
        Self(pid, PhantomData)
    }
}

pub(crate) fn waker_id(source: &WakeSource) -> WakerId {
    (source.kq, source.ident)
}

// Hirsel's own condition bits. A kevent carries a filter and flags, so each
// report is mapped onto these in `ready`; a user event carries them in its
// fflags, which is how a synthesized `Event` is encoded.
pub(crate) const READABLE: u32 = 1;
pub(crate) const WRITABLE: u32 = 2;
pub(crate) const READ_CLOSED: u32 = 4;
pub(crate) const HANG_UP: u32 = 8;
pub(crate) const ERROR: u32 = 16;
pub(crate) const CHANGED: u32 = 32;
/// A socket's persistent interest: every direction and the peer's shutdown.
pub(super) const INTEREST: u32 = READABLE | WRITABLE | READ_CLOSED;
/// The poller's own wake event.
const POLLER_WAKE: usize = 0;

const fn kevent(ident: usize, filter: i16, flags: u16, fflags: u32, token: u64) -> RawEvent {
    libc::kevent {
        ident,
        filter,
        flags,
        fflags,
        data: 0,
        udata: std::ptr::without_provenance_mut(token as usize),
    }
}

/// A synthesized report: a user event whose fflags are the conditions.
pub(crate) const fn raw_event(token: u64, ready: u32) -> RawEvent {
    kevent(0, libc::EVFILT_USER, 0, ready, token)
}
#[inline(always)]
pub(crate) fn token(event: &RawEvent) -> u64 {
    event.udata.addr() as u64
}
#[inline(always)]
pub(crate) fn ready(event: &RawEvent) -> u32 {
    let failed = if event.flags & libc::EV_ERROR != 0 {
        ERROR
    } else {
        0
    };
    let eof = event.flags & libc::EV_EOF != 0;
    // At EOF a socket's pending error is in fflags.
    let error = if eof && event.fflags != 0 { ERROR } else { 0 };
    failed
        | match event.filter {
            libc::EVFILT_READ => READABLE | if eof { READ_CLOSED | error } else { 0 },
            // Write EOF is this side's shutdown or a dead peer: the writer
            // retries and learns which. It does not end the reading side.
            libc::EVFILT_WRITE => WRITABLE | error,
            libc::EVFILT_PROC | libc::EVFILT_TIMER => READABLE,
            libc::EVFILT_VNODE => CHANGED,
            libc::EVFILT_USER => event.fflags & libc::NOTE_FFLAGSMASK,
            _ => 0,
        }
}

fn timespec(timeout_ns: u64) -> libc::timespec {
    libc::timespec {
        tv_sec: (timeout_ns / 1_000_000_000).min(i64::MAX as u64) as libc::time_t,
        tv_nsec: (timeout_ns % 1_000_000_000) as libc::c_long,
    }
}

/// Wait up to `timeout_ns` on `kq`, filling a prefix of `out`. kqueue waits
/// on `CLOCK_UPTIME_RAW`, the runtime's monotonic clock.
fn wait_raw(kq: RawFd, out: *mut RawEvent, max: usize, timeout_ns: Option<u64>) -> i32 {
    let timeout = timeout_ns.map(timespec);
    let max = i32::try_from(max).unwrap_or(i32::MAX);
    // SAFETY: `out` has room for `max` events, and the timeout lives
    // through the call.
    unsafe {
        libc::kevent(
            kq,
            std::ptr::null(),
            0,
            out,
            max,
            timeout.as_ref().map_or(std::ptr::null(), |t| t),
        )
    }
}

#[inline(always)]
pub(crate) fn wait(kq: RawFd, events: &mut [Event], timeout_ns: Option<u64>) -> io::Result<usize> {
    // Event is a transparent kevent.
    let count = wait_raw(kq, events.as_mut_ptr().cast(), events.len(), timeout_ns);
    Ok(checked(count)? as usize)
}

/// Apply `changes` with `EV_RECEIPT`, so each change reports its own result,
/// and return the first error that `ignore` does not accept.
fn apply(kq: RawFd, changes: &mut [RawEvent], ignore: &[i32]) -> io::Result<()> {
    for change in changes.iter_mut() {
        change.flags |= libc::EV_RECEIPT;
    }
    let count = changes.len() as i32;
    let pointer = changes.as_mut_ptr();
    // SAFETY: the changes are read before the receipts overwrite them in place.
    checked(unsafe { libc::kevent(kq, pointer, count, pointer, count, std::ptr::null()) })?;
    for receipt in changes.iter() {
        let errno = receipt.data as i32;
        if receipt.flags & libc::EV_ERROR != 0 && errno != 0 && !ignore.contains(&errno) {
            return Err(io::Error::from_raw_os_error(errno));
        }
    }
    Ok(())
}

/// Trigger a user event, waking its queue's waiter.
pub(crate) fn wake((kq, ident): WakerId) {
    let trigger = kevent(ident, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER, 0);
    // SAFETY: one initialized change and no output; kevent does not block.
    unsafe { libc::kevent(kq, &trigger, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
}

/// Take a level-triggered user event's pending trigger: re-adding the event
/// after deleting it leaves it untriggered.
pub(crate) fn reset(source: &WakeSource) {
    let mut changes = [
        kevent(source.ident, libc::EVFILT_USER, libc::EV_DELETE, 0, 0),
        user_event(source.ident, 0, source.token),
    ];
    let _ = apply(source.kq, &mut changes, &[libc::ENOENT]);
}

/// A user event reported as readable under `token`; `clear` resets it as
/// it is reported (one report per trigger).
const fn user_event(ident: usize, clear: u16, token: u64) -> RawEvent {
    kevent(
        ident,
        libc::EVFILT_USER,
        libc::EV_ADD | clear,
        libc::NOTE_FFCOPY | READABLE,
        token,
    )
}

pub(crate) fn selector() -> io::Result<OwnedFd> {
    // SAFETY: creates a new, uniquely owned descriptor.
    let kq = unsafe { OwnedFd::from_raw_fd(checked(libc::kqueue())?) };
    // SAFETY: a live descriptor this function owns.
    checked(unsafe { libc::fcntl(kq.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) })?;
    Ok(kq)
}

/// The filters `interest` names, each added with `flags` or deleted.
fn filters(fd: RawFd, token: u64, interest: u32, flags: u16) -> [RawEvent; 3] {
    let ident = fd as usize;
    let filter = |wanted: bool, filter: i16, fflags: u32| {
        let flags = if wanted {
            libc::EV_ADD | flags
        } else {
            libc::EV_DELETE
        };
        kevent(ident, filter, flags, fflags, token)
    };
    [
        filter(interest & READABLE != 0, libc::EVFILT_READ, 0),
        filter(interest & WRITABLE != 0, libc::EVFILT_WRITE, 0),
        filter(
            interest & CHANGED != 0,
            libc::EVFILT_VNODE,
            libc::NOTE_WRITE | libc::NOTE_EXTEND | libc::NOTE_ATTRIB | libc::NOTE_DELETE,
        ),
    ]
}

pub(crate) fn add(
    kq: RawFd,
    fd: RawFd,
    token: u64,
    interest: u32,
    trigger: Trigger,
) -> io::Result<()> {
    let once = match trigger {
        Trigger::Level => 0,
        Trigger::Once => libc::EV_ONESHOT,
    };
    // A filter not asked for is deleted, which fails ENOENT on a new watch.
    apply(kq, &mut filters(fd, token, interest, once), &[libc::ENOENT])
}

pub(crate) fn modify(kq: RawFd, fd: RawFd, token: u64, interest: u32) -> io::Result<()> {
    apply(kq, &mut filters(fd, token, interest, 0), &[libc::ENOENT])
}

pub(crate) fn remove(kq: RawFd, fd: RawFd) -> io::Result<()> {
    apply(kq, &mut filters(fd, 0, 0, 0), &[libc::ENOENT])
}

/// Report `child`'s exit once. A child that already exited and is not yet
/// reaped fails `ESRCH`; its parent reaps it without waiting.
pub(crate) fn add_exit(kq: RawFd, child: Child<'_>, token: u64) -> io::Result<()> {
    let mut change = [kevent(
        child.0 as usize,
        libc::EVFILT_PROC,
        libc::EV_ADD | libc::EV_ONESHOT,
        libc::NOTE_EXIT | libc::NOTE_EXITSTATUS,
        token,
    )];
    apply(kq, &mut change, &[])
}

/// A level-triggered user event reported under `token`.
pub(crate) fn wake_source(kq: RawFd, token: u64) -> io::Result<WakeSource> {
    // User events have their own identifier space; the token names this one.
    let ident = token as usize;
    apply(kq, &mut [user_event(ident, 0, token)], &[])?;
    Ok(WakeSource { kq, ident, token })
}

/// Descriptor waits made by a kqueue poller; kqueue reports each direction as
/// its own filter, so the turn buffer takes both reports of a token.
impl Poller {
    pub(super) fn new_readiness() -> io::Result<Arc<Self>> {
        let kq = selector()?;
        apply(
            kq.as_raw_fd(),
            &mut [user_event(POLLER_WAKE, libc::EV_CLEAR, WAKE)],
            &[],
        )?;
        Ok(Arc::new(Self {
            backend: Backend::Kqueue { kq },
            udp_groups: ManuallyDrop::new(OnceLock::new()),
            udp_budget: Mutex::default(),
            udp_deferred: ManuallyDrop::default(),
            events: Mutex::new(Vec::with_capacity(POLL_BATCH as usize)),
            operations: &PollOperations {
                poll: Self::poll_kqueue,
                wake: Self::wake_kqueue,
                add: Self::add_kqueue,
                remove: Self::remove_kqueue,
                release: Self::release_udp,
            },
        }))
    }

    fn kq(&self) -> RawFd {
        let Backend::Kqueue { kq } = &self.backend;
        kq.as_raw_fd()
    }

    /// Both directions edge-triggered (`EV_CLEAR`), as epoll's `EPOLLET`.
    fn add_kqueue(&self, fd: &Arc<OwnedFd>, tag: u64, _owner: bool) -> io::Result<()> {
        // A pipe end refuses the filter of the direction it lacks (EPIPE), and
        // the unasked vnode filter's delete fails ENOENT on a new watch.
        apply(
            self.kq(),
            &mut filters(fd.as_raw_fd(), tag, INTEREST, libc::EV_CLEAR),
            &[libc::EPIPE, libc::ENOENT],
        )
    }

    fn remove_kqueue(&self, fd: i32, _tag: u64, _closing: bool, _owner: bool) -> io::Result<()> {
        remove(self.kq(), fd)
    }

    fn poll_kqueue<'a>(
        &'a self,
        mut events: Turn<'a>,
        timeout_ns: Option<u64>,
    ) -> io::Result<Option<PollEvents<'a>>> {
        events.clear();
        crate::EPOLL_WAITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let count = wait_raw(
            self.kq(),
            events.as_mut_ptr().cast(),
            POLL_BATCH as usize,
            timeout_ns,
        );
        if count < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            return Ok(None);
        }
        let len = checked(count)? as usize;
        // SAFETY: kevent initialized exactly this prefix of the buffer.
        unsafe { events.set_len(len) };
        let visible = self.dispatch_udp_slot(&mut events);
        Ok(Some(PollEvents(events, visible)))
    }

    /// The poller's wake, reported as one `WAKE` event per trigger.
    pub(super) fn waker_id(&self) -> WakerId {
        (self.kq(), POLLER_WAKE)
    }

    fn wake_kqueue(&self) {
        wake(self.waker_id());
    }

    /// A unit pool slot's turn on its kqueue poller: reap into `out`,
    /// blocking up to `timeout_ns` (None: until an event; Some(0): not at
    /// all). Returns the events written.
    pub fn wait_slot(&self, out: &mut [Event], timeout_ns: Option<u64>) -> io::Result<usize> {
        let _turn = self.events.lock().unwrap_or_else(|e| e.into_inner());
        let timeout_ns = if self.udp_due() { Some(0) } else { timeout_ns };
        let len = wait(self.kq(), out, timeout_ns)?;
        Ok(self.dispatch_udp_slot(&mut out[..len]))
    }

    /// Whether the owner has anything to take now without waiting: only
    /// deferred UDP work, since kqueue queues nothing in user space.
    pub fn due(&self) -> bool {
        self.udp_due()
    }

    /// A kqueue poller completes no file operations; there are none to queue.
    ///
    /// # Safety
    /// None: no `FileEntry` exists on this host.
    pub unsafe fn submit_file(
        &self,
        _unit: u16,
        entry: FileEntry,
        _context: *const (),
        _complete: unsafe fn(*const (), i32),
    ) -> bool {
        match entry {}
    }

    /// kqueue has no transmit ring: sends copy from the caller's buffer.
    pub fn tx_slot(self: &Arc<Self>, _len: usize) -> Option<TxSlot> {
        None
    }

    /// Zero-copy notifications received and copied: none without a ring.
    pub fn tx_notifications(&self) -> (u64, u64) {
        (0, 0)
    }
}

/// A completion-ring file operation. Darwin has no completion ring, so no
/// operation can be queued and the type has no values.
pub enum FileEntry {}

/// A ring transmit slot. Darwin has no transmit ring, so no slot exists.
pub enum TxSlot {}

impl TxSlot {
    pub fn len(&self) -> usize {
        match *self {}
    }
    pub fn is_empty(&self) -> bool {
        match *self {}
    }
    pub fn bytes(&mut self) -> &mut [u8] {
        match *self {}
    }
    pub fn send(&mut self, _tag: u64, _range: std::ops::Range<usize>) -> io::Result<()> {
        match *self {}
    }
    pub fn state(&self) -> super::TxState {
        match *self {}
    }
    pub fn cancel(&self) {
        match *self {}
    }
}
