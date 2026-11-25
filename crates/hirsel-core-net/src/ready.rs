//! Hirsel's readiness vocabulary, shared by the Net reactor, the runtime's
//! Exec supervisor and the driver's launcher. Callers see a token and the
//! conditions that fired, never the host's event layout. The host provider
//! is chosen at compile time: epoll on Linux (`reactor/epoll.rs`), kqueue on
//! Darwin (`reactor/kqueue.rs`: one event per token and filter, user events
//! for wakes, process filters for child exit).
use crate::reactor::sys;
use std::{
    io,
    os::fd::{OwnedFd, RawFd},
    time::Duration,
};
pub use sys::Child;

/// The conditions a readiness event reports, or a watch asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ready(pub(crate) u32);

impl Ready {
    pub const EMPTY: Ready = Ready(0);
    /// Data or a connection can be taken without blocking.
    pub const READABLE: Ready = Ready(sys::READABLE);
    /// Data can be written without blocking.
    pub const WRITABLE: Ready = Ready(sys::WRITABLE);
    /// The peer shut its sending side: reading reaches the end.
    pub const READ_CLOSED: Ready = Ready(sys::READ_CLOSED);
    /// Both directions hung up.
    pub const HANG_UP: Ready = Ready(sys::HANG_UP);
    /// The descriptor has a pending error.
    pub const ERROR: Ready = Ready(sys::ERROR);
    /// A watched kernel file's contents changed (a cgroup's `cgroup.events`).
    pub const CHANGED: Ready = Ready(sys::CHANGED);

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub const fn is_readable(self) -> bool {
        self.0 & sys::READABLE != 0
    }
    pub const fn is_writable(self) -> bool {
        self.0 & sys::WRITABLE != 0
    }
    pub const fn is_read_closed(self) -> bool {
        self.0 & sys::READ_CLOSED != 0
    }
    /// An error or a hang-up: every direction's waiter learns of it.
    pub const fn is_failed(self) -> bool {
        self.0 & (sys::ERROR | sys::HANG_UP) != 0
    }
    pub const fn union(self, other: Ready) -> Ready {
        Ready(self.0 | other.0)
    }
}

impl std::ops::BitOr for Ready {
    type Output = Ready;
    fn bitor(self, other: Ready) -> Ready {
        self.union(other)
    }
}

/// One readiness report: the token its watch was registered under and the
/// conditions that fired. It has the host's event layout, so a wait writes
/// straight into a slice of them.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Event(sys::RawEvent);
// SAFETY: an event's fields are integers; kqueue's `udata` carries the token,
// never a pointer this process dereferences.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    pub const EMPTY: Event = Event::new(0, Ready::EMPTY);

    pub const fn new(token: u64, ready: Ready) -> Event {
        Event(sys::raw_event(token, ready.0))
    }
    #[inline(always)]
    pub fn token(self) -> u64 {
        sys::token(&self.0)
    }
    #[inline(always)]
    pub fn ready(self) -> Ready {
        Ready(sys::ready(&self.0))
    }
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Event")
            .field("token", &self.token())
            .field("ready", &self.ready())
            .finish()
    }
}

/// A readiness set's kernel handle (an epoll or kqueue descriptor), for a
/// thread that waits on a set another value owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle(pub(crate) RawFd);

impl Handle {
    /// Wait up to `timeout_ns` (None: until an event; Some(0): not at all)
    /// and fill a prefix of `events`. An interrupted wait is
    /// `ErrorKind::Interrupted`.
    #[inline(always)]
    pub fn wait(self, events: &mut [Event], timeout_ns: Option<u64>) -> io::Result<usize> {
        sys::wait(self.0, events, timeout_ns)
    }
}

/// Wakes a readiness set's waiter from any thread; the set reports it under
/// the token its wake was registered with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Waker(pub(crate) sys::WakerId);

impl Waker {
    #[inline(always)]
    pub fn wake(self) {
        sys::wake(self.0)
    }
}

/// How a watch reports: until the condition clears, or once until re-armed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Level,
    Once,
}

/// A readiness set owned by one waiter: the launcher's turn loop and the
/// Exec supervisor. Watches are keyed by descriptor.
pub struct Selector(OwnedFd);

impl Selector {
    pub fn new() -> io::Result<Selector> {
        sys::selector().map(Selector)
    }
    pub fn handle(&self) -> Handle {
        Handle(std::os::fd::AsRawFd::as_raw_fd(&self.0))
    }
    /// Watch `fd` for `interest` under `token`.
    pub fn add(&self, fd: RawFd, token: u64, interest: Ready, trigger: Trigger) -> io::Result<()> {
        sys::add(self.handle().0, fd, token, interest.0, trigger)
    }
    /// Replace the interest of a watched `fd`.
    pub fn modify(&self, fd: RawFd, token: u64, interest: Ready) -> io::Result<()> {
        sys::modify(self.handle().0, fd, token, interest.0)
    }
    /// Stop watching `fd`.
    pub fn remove(&self, fd: RawFd) -> io::Result<()> {
        sys::remove(self.handle().0, fd)
    }
    /// Report the exit of the child `process` (a pidfd on Linux, a process
    /// id on Darwin) once, under `token`.
    pub fn add_exit(&self, process: Child<'_>, token: u64) -> io::Result<()> {
        sys::add_exit(self.handle().0, process, token)
    }
    pub fn wait(&self, events: &mut [Event], timeout: Option<Duration>) -> io::Result<usize> {
        let timeout_ns = timeout.map(|t| u64::try_from(t.as_nanos()).unwrap_or(u64::MAX));
        self.handle().wait(events, timeout_ns)
    }
}

/// A selector's own wake source, reported under its token until reset.
pub struct Wake(sys::WakeSource);

impl Wake {
    pub fn new(selector: &Selector, token: u64) -> io::Result<Wake> {
        sys::wake_source(selector.handle().0, token).map(Wake)
    }
    pub fn wake(&self) {
        sys::wake(sys::waker_id(&self.0))
    }
    /// Take the pending wake, so the selector stops reporting it.
    pub fn reset(&self) {
        sys::reset(&self.0)
    }
}
