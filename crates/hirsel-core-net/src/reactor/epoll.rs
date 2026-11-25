//! The Linux readiness provider: [`crate::ready`]'s events are epoll events,
//! and the epoll backend of a [`Poller`]. Each watched descriptor has one
//! edge-triggered interest; a poller's wake is an eventfd it watches
//! edge-triggered and never reads.
use super::{Backend, POLL_BATCH, PollEvents, PollOperations, Poller, Turn, WAKE, checked};
use crate::ready::{Event, Trigger};
use std::{
    io,
    mem::ManuallyDrop,
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
    sync::{Arc, Mutex, OnceLock},
};

pub(crate) type RawEvent = libc::epoll_event;
/// A wake is an eventfd: woken by an eight-byte add, reported by its set.
pub(crate) type WakerId = RawFd;
pub(crate) type WakeSource = OwnedFd;

pub(crate) fn waker_id(source: &WakeSource) -> WakerId {
    source.as_raw_fd()
}

pub(crate) const READABLE: u32 = libc::EPOLLIN as u32;
pub(crate) const WRITABLE: u32 = libc::EPOLLOUT as u32;
pub(crate) const READ_CLOSED: u32 = libc::EPOLLRDHUP as u32;
pub(crate) const HANG_UP: u32 = libc::EPOLLHUP as u32;
pub(crate) const ERROR: u32 = libc::EPOLLERR as u32;
pub(crate) const CHANGED: u32 = libc::EPOLLPRI as u32;
/// A socket's persistent interest: every direction and the peer's shutdown.
pub(super) const INTEREST: u32 = READABLE | WRITABLE | READ_CLOSED;

pub(crate) const fn raw_event(token: u64, ready: u32) -> RawEvent {
    libc::epoll_event {
        events: ready,
        u64: token,
    }
}
#[inline(always)]
pub(crate) fn token(event: &RawEvent) -> u64 {
    event.u64
}
#[inline(always)]
pub(crate) fn ready(event: &RawEvent) -> u32 {
    event.events
}

/// io_uring reports a poll's result as a poll(2) mask, whose bits are
/// epoll's on Linux.
impl crate::ready::Ready {
    pub(super) const fn from_poll_mask(mask: u32) -> Self {
        Self(mask)
    }
    pub(super) const fn poll_mask(self) -> u32 {
        self.0
    }
}

fn timeout_ms(timeout_ns: Option<u64>) -> i32 {
    timeout_ns.map_or(-1, |ns| {
        i32::try_from(ns.div_ceil(1_000_000)).unwrap_or(i32::MAX)
    })
}

#[inline(always)]
pub(crate) fn wait(
    epoll: RawFd,
    events: &mut [Event],
    timeout_ns: Option<u64>,
) -> io::Result<usize> {
    let max = i32::try_from(events.len()).unwrap_or(i32::MAX);
    // SAFETY: Event is a transparent epoll_event, and epoll writes at most
    // `max` of them.
    let count = unsafe {
        libc::epoll_wait(
            epoll,
            events.as_mut_ptr().cast(),
            max,
            timeout_ms(timeout_ns),
        )
    };
    Ok(checked(count)? as usize)
}

/// Write one to an eventfd counter, retrying an interrupted write.
pub(crate) fn wake(eventfd: RawFd) {
    let one = 1u64;
    loop {
        // SAFETY: a nonblocking eventfd takes an eight-byte counter add;
        // EAGAIN means a wake is already pending in the saturated counter.
        if unsafe { libc::write(eventfd, (&raw const one).cast(), 8) } >= 0
            || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
        {
            return;
        }
    }
}

/// Take an eventfd's pending count.
pub(crate) fn reset(eventfd: &WakeSource) {
    let mut pending = 0u64;
    // SAFETY: a nonblocking eventfd reads exactly one u64.
    unsafe { libc::read(eventfd.as_raw_fd(), (&raw mut pending).cast(), 8) };
}

fn eventfd() -> io::Result<OwnedFd> {
    // SAFETY: creates a new, uniquely owned CLOEXEC descriptor.
    Ok(unsafe {
        OwnedFd::from_raw_fd(checked(libc::eventfd(
            0,
            libc::EFD_CLOEXEC | libc::EFD_NONBLOCK,
        ))?)
    })
}

pub(crate) fn selector() -> io::Result<OwnedFd> {
    // SAFETY: creates a new, uniquely owned CLOEXEC descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(checked(libc::epoll_create1(libc::EPOLL_CLOEXEC))?) })
}

fn ctl(epoll: RawFd, op: i32, fd: RawFd, flags: u32, token: u64) -> io::Result<()> {
    let mut event = raw_event(token, flags);
    // SAFETY: live descriptors and an initialized event; DEL ignores it.
    checked(unsafe { libc::epoll_ctl(epoll, op, fd, &mut event) })?;
    Ok(())
}

pub(crate) fn add(
    epoll: RawFd,
    fd: RawFd,
    token: u64,
    interest: u32,
    trigger: Trigger,
) -> io::Result<()> {
    let once = match trigger {
        Trigger::Level => 0,
        Trigger::Once => libc::EPOLLONESHOT as u32,
    };
    ctl(epoll, libc::EPOLL_CTL_ADD, fd, interest | once, token)
}

pub(crate) fn modify(epoll: RawFd, fd: RawFd, token: u64, interest: u32) -> io::Result<()> {
    ctl(epoll, libc::EPOLL_CTL_MOD, fd, interest, token)
}

pub(crate) fn remove(epoll: RawFd, fd: RawFd) -> io::Result<()> {
    ctl(epoll, libc::EPOLL_CTL_DEL, fd, 0, 0)
}

/// A pidfd turns readable once its process exits.
pub(crate) fn add_exit(epoll: RawFd, pidfd: BorrowedFd<'_>, token: u64) -> io::Result<()> {
    add(epoll, pidfd.as_raw_fd(), token, READABLE, Trigger::Once)
}

/// A level-triggered eventfd watched under `token`.
pub(crate) fn wake_source(epoll: RawFd, token: u64) -> io::Result<OwnedFd> {
    let fd = eventfd()?;
    add(epoll, fd.as_raw_fd(), token, READABLE, Trigger::Level)?;
    Ok(fd)
}

impl Poller {
    pub(super) fn new_epoll() -> io::Result<Arc<Self>> {
        let poller = Arc::new(Self {
            backend: Backend::Epoll {
                epoll: selector()?,
                wake: eventfd()?,
            },
            udp_groups: ManuallyDrop::new(OnceLock::new()),
            udp_budget: Mutex::default(),
            udp_deferred: ManuallyDrop::default(),
            events: Mutex::new(Vec::with_capacity(POLL_BATCH as usize)),
            operations: &PollOperations {
                poll: Self::poll_epoll,
                wake: Self::wake_eventfd,
                add: Self::add_epoll,
                remove: Self::remove_epoll,
                release: Self::release_udp,
            },
        });
        poller.epoll_ctl(
            libc::EPOLL_CTL_ADD,
            poller.wake_fd(),
            READABLE | libc::EPOLLET as u32,
            WAKE,
        )?;
        Ok(poller)
    }

    fn epoll_ctl(&self, op: i32, fd: i32, flags: u32, token: u64) -> io::Result<()> {
        let Backend::Epoll { epoll, .. } = &self.backend else {
            unreachable!()
        };
        ctl(epoll.as_raw_fd(), op, fd, flags, token)
    }

    fn add_epoll(&self, fd: &Arc<OwnedFd>, tag: u64, _owner: bool) -> io::Result<()> {
        self.epoll_ctl(
            libc::EPOLL_CTL_ADD,
            fd.as_raw_fd(),
            INTEREST | libc::EPOLLET as u32,
            tag,
        )
    }

    fn remove_epoll(&self, fd: i32, _tag: u64, _closing: bool, _owner: bool) -> io::Result<()> {
        self.epoll_ctl(libc::EPOLL_CTL_DEL, fd, 0, 0)
    }

    // The optional operations are installed with the native poller, so a
    // scheduler that can never construct one does not link this syscall path.
    fn poll_epoll<'a>(
        &'a self,
        mut events: Turn<'a>,
        timeout_ns: Option<u64>,
    ) -> io::Result<Option<PollEvents<'a>>> {
        let Backend::Epoll { epoll, .. } = &self.backend else {
            unreachable!()
        };
        events.clear();
        crate::EPOLL_WAITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: epoll owns its descriptor and events has POLL_BATCH spare
        // slots; Event is a transparent epoll_event.
        let count = unsafe {
            libc::epoll_wait(
                epoll.as_raw_fd(),
                events.as_mut_ptr().cast(),
                POLL_BATCH,
                timeout_ms(timeout_ns),
            )
        };
        if count < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            return Ok(None);
        }
        let len = checked(count)? as usize;
        // SAFETY: epoll_wait initialized exactly this prefix of the buffer.
        unsafe { events.set_len(len) };
        let visible = self.dispatch_udp_slot(&mut events);
        Ok(Some(PollEvents(events, visible)))
    }

    /// The wake eventfd, registered edge-triggered under `WAKE`.
    pub(super) fn wake_fd(&self) -> i32 {
        match &self.backend {
            Backend::Epoll { wake, .. } => wake.as_raw_fd(),
            Backend::Uring(_) => unreachable!("a ring is woken by MSG_RING"),
        }
    }

    fn wake_eventfd(&self) {
        wake(self.wake_fd());
    }
}
