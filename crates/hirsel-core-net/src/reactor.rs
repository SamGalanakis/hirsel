//! Readiness interests, adapted from Go runtime/netpoll_epoll.go.
//! Slot generations identify sockets across poll and dispatch.
//!
//! Two kernel interfaces carry them. epoll keeps one persistent
//! edge-triggered interest per descriptor. io_uring arms a one-shot poll per
//! direction only when a waiter parks there ([`Sockets::arm`]): a poll
//! completes through task work on the thread that armed it, so a persistent
//! poll on a socket being read interrupts its reader once per arriving
//! segment, while a parked waiter is about to sleep anyway. One interface is
//! chosen once per process ([`reactor`]); both turn a kernel turn into the
//! same `(tag, flags)` events, so dispatch is shared.
use std::{
    collections::{HashMap, VecDeque},
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{Arc, Mutex, MutexGuard, OnceLock, TryLockError},
};
mod sendzc;
pub use sendzc::{TxSlot, TxState};

const POLL_BATCH: i32 = 128;
/// The wake eventfd's tag.
const WAKE: u64 = u64::MAX;
/// io_uring: the completion of a poll removal, which carries no readiness.
const REMOVED: u64 = u64::MAX - 1;
const INTEREST: u32 = (libc::EPOLLIN | libc::EPOLLOUT | libc::EPOLLRDHUP) as u32;
/// io_uring: a direction's one-shot poll carries its tag with this bit set
/// for the write direction. Tags never use it: a slot index stays below it
/// ([`Sockets::insert_record`]) and WAKE and REMOVED are matched first.
const WRITE_POLL: u64 = 1 << 31;

/// The kernel interface behind the Net transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reactor {
    Epoll,
    IoUring,
}
impl Reactor {
    pub fn name(self) -> &'static str {
        match self {
            Reactor::Epoll => "epoll",
            Reactor::IoUring => "io_uring",
        }
    }
}

/// `auto`, and an unset variable, use io_uring when this kernel allows it and
/// its release is at least 6.13, and epoll otherwise; `epoll` forces epoll and
/// `io_uring` forces io_uring.
pub const REACTOR_ENV: &str = "HIRSEL_NET_REACTOR";
static CHOSEN: OnceLock<Result<Reactor, String>> = OnceLock::new();

/// When an io_uring ring sends from runtime slots zero-copy (`IORING_OP_SEND_ZC`):
/// `auto`, and an unset variable, for every TCP peer but a loopback one;
/// `always` for loopback too; `never` for none. On loopback the kernel copies
/// every zero-copy send at delivery (`ZC_COPIED`), so it only adds the
/// notification and page pinning: TLS PUT measured 17% slower there.
pub const SEND_ZC_ENV: &str = "HIRSEL_NET_SEND_ZC";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SendZc {
    Auto,
    Always,
    Never,
}
static SEND_ZC: OnceLock<SendZc> = OnceLock::new();

fn send_zc() -> SendZc {
    *SEND_ZC.get().unwrap_or(&SendZc::Auto)
}
/// The ring and wake descriptor the probe built, for the first poller.
static PROBED: Mutex<Option<Uring>> = Mutex::new(None);

/// Whether `auto` may take io_uring on a kernel of this release
/// (`uname -r`). Through 6.12, a request reached from the exit fallback
/// (`io_fallback_req_func`) puts its task through `io_put_task_remote`, which
/// oopsed on 6.8 when a process was killed with multishot polls armed. 6.13
/// removed requests' task pointers and that function; 6.12.y still has them.
fn release_allows_uring(release: &str) -> bool {
    let mut parts = release
        .split(|c: char| !c.is_ascii_digit())
        .map(str::parse::<u32>);
    match (parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor))) => (major, minor) >= (6, 13),
        _ => false,
    }
}

fn kernel_release() -> String {
    // SAFETY: uname fills the zeroed struct; release is NUL-terminated.
    unsafe {
        let mut name: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut name) != 0 {
            return String::new();
        }
        std::ffi::CStr::from_ptr(name.release.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}

/// Whether sends to `fd`'s peer go zero-copy under [`SEND_ZC_ENV`]: TCP
/// only, and under `auto` not to a loopback address.
fn zero_copy_peer(fd: i32) -> bool {
    let mode = send_zc();
    if mode == SendZc::Never {
        return false;
    }
    // SAFETY: a live socket; the storage holds any address family.
    let peer = unsafe {
        let mut storage: libc::sockaddr_storage = std::mem::zeroed();
        let mut len = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        if libc::getpeername(fd, (&raw mut storage).cast(), &mut len) != 0 {
            return false;
        }
        match i32::from(storage.ss_family) {
            libc::AF_INET => {
                let v4 = &*(&raw const storage).cast::<libc::sockaddr_in>();
                std::net::SocketAddr::from((
                    std::net::Ipv4Addr::from(u32::from_be(v4.sin_addr.s_addr)),
                    0,
                ))
            }
            libc::AF_INET6 => {
                let v6 = &*(&raw const storage).cast::<libc::sockaddr_in6>();
                std::net::SocketAddr::from((std::net::Ipv6Addr::from(v6.sin6_addr.s6_addr), 0))
            }
            _ => return false,
        }
    };
    mode == SendZc::Always || !is_loopback(&peer)
}

fn is_loopback(peer: &std::net::SocketAddr) -> bool {
    match peer.ip() {
        std::net::IpAddr::V4(ip) => ip.is_loopback(),
        std::net::IpAddr::V6(ip) => {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// The process's reactor, chosen at its first use and fixed after.
pub fn reactor() -> io::Result<Reactor> {
    CHOSEN
        .get_or_init(|| {
            let send_zc = match std::env::var_os(SEND_ZC_ENV) {
                None => SendZc::Auto,
                Some(v) if v == "auto" => SendZc::Auto,
                Some(v) if v == "always" => SendZc::Always,
                Some(v) if v == "never" => SendZc::Never,
                Some(v) => {
                    return Err(format!(
                        "{SEND_ZC_ENV}={} is not a choice; use auto, always or never",
                        v.to_string_lossy()
                    ));
                }
            };
            let _ = SEND_ZC.set(send_zc);
            let forced = match std::env::var_os(REACTOR_ENV) {
                Some(v) if v == "epoll" => return Ok(Reactor::Epoll),
                Some(v) if v == "io_uring" => true,
                Some(v) if v != "auto" => {
                    return Err(format!(
                        "{REACTOR_ENV}={} is not a reactor; use auto, epoll or io_uring",
                        v.to_string_lossy()
                    ));
                }
                _ => false,
            };
            if !forced && !release_allows_uring(&kernel_release()) {
                return Ok(Reactor::Epoll);
            }
            match Uring::open() {
                Ok(probed) => {
                    *PROBED.lock().unwrap_or_else(|e| e.into_inner()) = Some(probed);
                    Ok(Reactor::IoUring)
                }
                Err(e) if forced => Err(format!("{REACTOR_ENV}=io_uring: {e}")),
                Err(_) => Ok(Reactor::Epoll),
            }
        })
        .clone()
        .map_err(|message| {
            eprintln!("{message}");
            io::Error::new(io::ErrorKind::InvalidInput, message)
        })
}

pub struct Poller {
    backend: Backend,
    wake: Arc<OwnedFd>,
    events: Mutex<Vec<libc::epoll_event>>,
    operations: &'static PollOperations,
}

enum Backend {
    Epoll(OwnedFd),
    Uring(Box<Uring>),
}

struct PollOperations {
    poll: for<'a> fn(&'a Poller, Turn<'a>, i32) -> io::Result<Option<PollEvents<'a>>>,
    wake: fn(&Poller),
}

/// The driver token: one worker at a time polls and dispatches.
type Turn<'a> = MutexGuard<'a, Vec<libc::epoll_event>>;

/// The initialized prefix of one kernel turn, held until registry dispatch.
pub struct PollEvents<'a>(Turn<'a>);
fn checked(rc: i32) -> io::Result<i32> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
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
impl Poller {
    fn new() -> io::Result<Arc<Self>> {
        match reactor()? {
            Reactor::Epoll => Self::new_epoll(),
            Reactor::IoUring => Self::new_uring(),
        }
    }
    fn new_uring() -> io::Result<Arc<Self>> {
        let probed = PROBED.lock().unwrap_or_else(|e| e.into_inner()).take();
        let ring = match probed {
            Some(probed) => probed,
            None => Uring::open()?,
        };
        let wake = ring.wake.clone();
        Ok(Arc::new(Self {
            backend: Backend::Uring(Box::new(ring)),
            wake,
            events: Mutex::new(Vec::with_capacity(POLL_BATCH as usize)),
            operations: &PollOperations {
                poll: Self::poll_uring,
                wake: Self::wake_eventfd,
            },
        }))
    }
    fn new_epoll() -> io::Result<Arc<Self>> {
        // SAFETY: creates a new, uniquely owned CLOEXEC descriptor.
        let epoll =
            unsafe { OwnedFd::from_raw_fd(checked(libc::epoll_create1(libc::EPOLL_CLOEXEC))?) };
        let poller = Arc::new(Self {
            backend: Backend::Epoll(epoll),
            wake: Arc::new(eventfd()?),
            events: Mutex::new(Vec::with_capacity(POLL_BATCH as usize)),
            operations: &PollOperations {
                poll: Self::poll_epoll,
                wake: Self::wake_eventfd,
            },
        });
        poller.epoll_ctl(
            libc::EPOLL_CTL_ADD,
            poller.wake.as_raw_fd(),
            libc::EPOLLIN as u32,
            WAKE,
        )?;
        Ok(poller)
    }
    /// The interface this poller waits on.
    pub fn reactor(&self) -> Reactor {
        match self.backend {
            Backend::Epoll(_) => Reactor::Epoll,
            Backend::Uring(_) => Reactor::IoUring,
        }
    }
    fn epoll_ctl(&self, op: i32, fd: i32, flags: u32, token: u64) -> io::Result<()> {
        let Backend::Epoll(epoll) = &self.backend else {
            unreachable!()
        };
        let mut event = libc::epoll_event {
            events: flags,
            u64: token,
        };
        // SAFETY: live descriptors and writable event; DEL ignores the event.
        checked(unsafe { libc::epoll_ctl(epoll.as_raw_fd(), op, fd, &mut event) })?;
        Ok(())
    }
    /// Whether a registered descriptor reports every edge without a waiter
    /// parked, so a drained reader may wait without trying again.
    fn persistent(&self) -> bool {
        matches!(self.backend, Backend::Epoll(_))
    }
    /// A waiter for this direction of `fd` has just parked under `tag`. Its
    /// syscall has already returned EAGAIN, and the readiness it waits for
    /// is reported however soon it arrives.
    fn park(&self, tag: u64, write: bool) {
        match &self.backend {
            Backend::Epoll(_) => {}
            Backend::Uring(ring) => ring.park(tag, write),
        }
    }
    /// Watch `fd` for every direction under `tag`: epoll edge-triggered now,
    /// io_uring once a waiter parks.
    fn add(&self, fd: &Arc<OwnedFd>, tag: u64) -> io::Result<()> {
        match &self.backend {
            Backend::Epoll(_) => self.epoll_ctl(
                libc::EPOLL_CTL_ADD,
                fd.as_raw_fd(),
                INTEREST | libc::EPOLLET as u32,
                tag,
            ),
            Backend::Uring(ring) => {
                ring.add(tag, fd.clone());
                Ok(())
            }
        }
    }
    /// Stop watching `fd`. `closing` names a listener whose only holder is
    /// about to close it: an io_uring poll holds its own file reference until
    /// the kernel retires it, so the listener is shut down first and its
    /// address is free for rebinding on return, as after an epoll close.
    fn remove(&self, fd: i32, tag: u64, closing: bool) -> io::Result<()> {
        match &self.backend {
            Backend::Epoll(_) => self.epoll_ctl(libc::EPOLL_CTL_DEL, fd, 0, 0),
            Backend::Uring(ring) => {
                if closing {
                    // SAFETY: a live descriptor this process alone holds; a
                    // receive shutdown changes nothing the close that follows
                    // does not.
                    unsafe { libc::shutdown(fd, libc::SHUT_RD) };
                }
                ring.remove(tag);
                Ok(())
            }
        }
    }
    fn try_turn(&self) -> Option<Turn<'_>> {
        match self.events.try_lock() {
            Ok(events) => Some(events),
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Poisoned(events)) => Some(events.into_inner()),
        }
    }

    /// Try the driver token. A busy token returns immediately while another
    /// worker polls or dispatches. A zero timeout is a nonblocking turn.
    pub fn poll(&self, timeout_ms: i32) -> io::Result<Option<PollEvents<'_>>> {
        self.wait(timeout_ms, || false)
    }

    /// Poll with `woken` checked once the turn is held. A waker that sets its
    /// flag before `wake` is seen here even when another worker's turn has
    /// already drained the wake: that turn ended before this one began.
    pub fn wait(
        &self,
        timeout_ms: i32,
        woken: impl FnOnce() -> bool,
    ) -> io::Result<Option<PollEvents<'_>>> {
        let Some(events) = self.try_turn() else {
            return Ok(None);
        };
        let timeout_ms = if woken() { 0 } else { timeout_ms };
        (self.operations.poll)(self, events, timeout_ms)
    }

    /// mc-units-untied: the shared poller's turn on an io_uring ring, for the
    /// pool, which holds its own poll turn. Returns the events written.
    pub fn wait_shared(&self, out: &mut [libc::epoll_event], timeout_ms: i32) -> usize {
        let Backend::Uring(ring) = &self.backend else {
            unreachable!("the pool waits on an epoll poller directly")
        };
        let _turn = self.events.lock().unwrap_or_else(|e| e.into_inner());
        let n = ring.wait(out, timeout_ms);
        if out[..n].iter().any(|event| event.u64 == WAKE) {
            self.drain();
        }
        n
    }

    // The optional operations are installed with the native poller, so a
    // scheduler that can never construct one does not link this syscall path.
    fn poll_epoll<'a>(
        &'a self,
        mut events: Turn<'a>,
        timeout_ms: i32,
    ) -> io::Result<Option<PollEvents<'a>>> {
        let Backend::Epoll(epoll) = &self.backend else {
            unreachable!()
        };
        events.clear();
        crate::EPOLL_WAITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: epoll owns its descriptor and events has POLL_BATCH spare slots.
        let count = unsafe {
            libc::epoll_wait(
                epoll.as_raw_fd(),
                events.as_mut_ptr(),
                POLL_BATCH,
                timeout_ms,
            )
        };
        if count < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            return Ok(None);
        }
        let len = checked(count)? as usize;
        // SAFETY: epoll_wait initialized exactly this prefix of the buffer.
        unsafe { events.set_len(len) };
        if events.iter().any(|event| event.u64 == WAKE) {
            self.drain();
        }
        Ok(Some(PollEvents(events)))
    }

    fn poll_uring<'a>(
        &'a self,
        mut events: Turn<'a>,
        timeout_ms: i32,
    ) -> io::Result<Option<PollEvents<'a>>> {
        let Backend::Uring(ring) = &self.backend else {
            unreachable!()
        };
        events.clear();
        crate::EPOLL_WAITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        events.resize(POLL_BATCH as usize, libc::epoll_event { events: 0, u64: 0 });
        let len = ring.wait(&mut events, timeout_ms);
        events.truncate(len);
        if events.iter().any(|event| event.u64 == WAKE) {
            self.drain();
        }
        Ok(Some(PollEvents(events)))
    }

    pub fn wake(&self) {
        (self.operations.wake)(self);
    }
    /// mc-units-untied: the epoll fd (or io_uring ring fd), for the shared poller.
    pub fn raw_fd(&self) -> i32 {
        match &self.backend {
            Backend::Epoll(epoll) => epoll.as_raw_fd(),
            Backend::Uring(ring) => ring.ring.as_raw_fd(),
        }
    }
    /// mc-units-light: the wake eventfd, registered under tag `u64::MAX`.
    pub fn wake_fd(&self) -> i32 {
        self.wake.as_raw_fd()
    }

    fn wake_eventfd(&self) {
        signal(self.wake.as_raw_fd());
    }
    fn drain(&self) {
        drain(self.wake.as_raw_fd());
    }
}
fn signal(fd: i32) {
    let one = 1u64;
    loop {
        // SAFETY: nonblocking eventfd, readable eight-byte counter.
        if unsafe { libc::write(fd, (&raw const one).cast(), 8) } >= 0
            || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
        {
            return;
        }
    }
}
fn drain(fd: i32) {
    let mut value = 0u64;
    // SAFETY: nonblocking eventfd, writable eight-byte counter.
    while unsafe { libc::read(fd, (&mut value as *mut u64).cast(), 8) } < 0 {
        if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            break;
        }
    }
}

/// An io_uring ring: one multishot poll on the wake eventfd, and a one-shot
/// poll per direction of a watched descriptor while a waiter is parked there.
/// Setup flags are 0: the driver token migrates between workers, which rules
/// out `SINGLE_ISSUER` and with it `DEFER_TASKRUN`, and `COOP_TASKRUN` would
/// leave a poll's completion unposted until its arming thread next enters the
/// kernel, up to a scheduler tick later while that thread computes.
struct Uring {
    ring: io_uring::IoUring,
    wake: Arc<OwnedFd>,
    /// Serializes every submission-queue access; completions are read only
    /// under the driver turn.
    sq: Mutex<Submissions>,
    /// Lock before `sq`.
    watched: Mutex<HashMap<u64, Watch>>,
    /// The kernel ended the wake eventfd's multishot poll, which waits for
    /// completion-queue room to be re-armed. Touched only under the driver turn.
    wake_ended: Mutex<bool>,
    /// Transmit slots and their zero-copy sends. Lock before `sq`.
    tx: Mutex<sendzc::Table>,
    /// The kernel runs SEND_ZC.
    zc: bool,
    /// A sparse registered-buffer table exists for transmit slots.
    sparse: bool,
    #[cfg(test)]
    refuse_submit: std::sync::atomic::AtomicBool,
}

struct Watch {
    fd: Arc<OwnedFd>,
    /// Set when a waiter arms the direction and cleared when the driver reaps it.
    polling: [bool; 2],
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
    /// Prefix already visible in the shared SQ. The rest waits for space.
    published: usize,
}

impl Submissions {
    fn consumed(&mut self, remaining: usize) {
        self.entries.drain(..self.published - remaining);
        self.published = remaining;
    }
}
impl Uring {
    const ENTRIES: u32 = 64;
    /// The kernel clamps this to its own maximum (65536 on Linux 6.8). Tests
    /// use a small queue so a few thousand sockets overflow it.
    #[cfg(not(test))]
    const COMPLETIONS: u32 = 65536;
    #[cfg(test)]
    const COMPLETIONS: u32 = 1024;
    const READ: u32 = (libc::EPOLLIN | libc::EPOLLRDHUP) as u32;
    const WRITE: u32 = libc::EPOLLOUT as u32;

    /// A ring whose kernel runs multishot polls and bounded waits, with its
    /// wake eventfd already watched. Any refusal (an old kernel, a seccomp
    /// filter, `io_uring_disabled`) is an error, and the caller uses epoll.
    fn open() -> io::Result<Uring> {
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
                    && cqe.result() as u32 & libc::EPOLLIN as u32 != 0
                    && cqueue::more(cqe.flags()) =>
            {
                Ok(uring)
            }
            _ => Err(unsupported()),
        }
    }

    fn submit(&self) -> io::Result<usize> {
        #[cfg(test)]
        if self
            .refuse_submit
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(io::Error::from_raw_os_error(libc::EBUSY));
        }
        self.ring.submitter().submit()
    }

    fn flush(&self, pending: &mut Submissions) {
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

    fn push(&self, entry: io_uring::squeue::Entry, fd: Option<Arc<OwnedFd>>) {
        let mut pending = self.sq.lock().unwrap_or_else(|e| e.into_inner());
        pending.entries.push_back(Submission { entry, _fd: fd });
        self.flush(&mut pending);
        if !pending.entries.is_empty() {
            signal(self.wake.as_raw_fd());
        }
    }

    /// The wake eventfd is always writable; only its counter matters.
    fn arm_wake(&self) {
        use io_uring::{opcode, types};
        self.push(
            opcode::PollAdd::new(types::Fd(self.wake.as_raw_fd()), libc::EPOLLIN as u32)
                .multi(true)
                .build()
                .user_data(WAKE),
            Some(self.wake.clone()),
        )
    }

    fn poll_data(tag: u64, write: bool) -> u64 {
        if write { tag | WRITE_POLL } else { tag }
    }

    fn add(&self, tag: u64, fd: Arc<OwnedFd>) {
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
    fn park(&self, tag: u64, write: bool) {
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
    fn remove(&self, tag: u64) {
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
    fn wait(&self, out: &mut [libc::epoll_event], timeout_ms: i32) -> usize {
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

    fn retry_submissions(&self) {
        let mut pending = self.sq.lock().unwrap_or_else(|e| e.into_inner());
        if !pending.entries.is_empty() {
            self.flush(&mut pending);
        }
    }

    /// Re-arm an ended wake poll once the completion queue is at most half
    /// full, under the driver turn. A ready eventfd posts as soon as its poll
    /// is armed; re-arming into a full queue ends that poll again at once,
    /// and the pair would cycle without end.
    fn rearm_wake(&self) {
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
    fn reap(&self, out: &mut [libc::epoll_event]) -> usize {
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
                    (WAKE, libc::EPOLLIN as u32)
                }
                data if sendzc::is_tx(data) => {
                    // A send's answer or notification readies its socket's writer.
                    match self.tx_complete(data, cqe.result(), cqe.flags()) {
                        Some(owner) => (owner, libc::EPOLLOUT as u32),
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
            out[n] = libc::epoll_event {
                events: flags,
                u64: tag,
            };
            n += 1;
        }
        n
    }
}
fn tag(index: usize, generation: u32) -> u64 {
    index as u64 | (u64::from(generation) << 32)
}

/// mc-units-light: one poller for a whole pool of units. A unit's registry
/// adds its descriptors to it, tagged with the owning unit, and never polls
/// it: the pool polls and hands each edge to its unit, which dispatches it
/// as one of its own events. A shared tag is the unit in the top 16 bits,
/// the low 16 bits of the slot generation, then the slot index.
struct SharedPoller {
    poller: Arc<Poller>,
    unit: fn() -> Option<u32>,
}
static SHARED: std::sync::OnceLock<SharedPoller> = std::sync::OnceLock::new();
/// Bits of a shared tag below the unit.
pub const SHARED_UNIT_SHIFT: u32 = 48;
/// Make the process's shared poller. `unit` names the calling unit, or None
/// outside one (such a registry keeps a private poller).
pub fn share_poller(unit: fn() -> Option<u32>) -> io::Result<Arc<Poller>> {
    share(Poller::new()?, unit)
}
/// The interleaving checker models epoll calls, so its pool shares an epoll
/// poller whichever reactor the process chose.
pub fn share_epoll_poller(unit: fn() -> Option<u32>) -> io::Result<Arc<Poller>> {
    share(Poller::new_epoll()?, unit)
}
fn share(poller: Arc<Poller>, unit: fn() -> Option<u32>) -> io::Result<Arc<Poller>> {
    let _ = SHARED.set(SharedPoller {
        poller: poller.clone(),
        unit,
    });
    Ok(poller)
}
fn shared_tag(unit: u32, index: usize, generation: u32) -> u64 {
    // Unit 0xffff is the ring's own: wake, poll removals and transmit slots.
    assert!(unit < 0xffff, "unit {unit} is reserved");
    (u64::from(unit) << SHARED_UNIT_SHIFT) | (u64::from(generation & 0xffff) << 32) | index as u64
}

struct Record {
    fd: Arc<OwnedFd>,
    pipe: bool,
    owners: u8,
    reset: bool,
    ended: bool,
    waiters: [Option<u64>; 2],
    registered: bool,
    read_drained: bool,
    read_terminal: bool,
    /// A listener this process created, and so the only holder of its socket.
    own_listener: bool,
    /// Its last send was abandoned, so it closes with a reset.
    abandoned: bool,
    /// Whether sends to its peer go zero-copy, once a send has asked.
    zero_copy: Option<bool>,
}
struct Slot {
    generation: u32,
    record: Option<Record>,
}
/// A carrier outside either registry. Kernel receive bytes stay on the socket.
/// A fresh registration sees current readiness, including edges queued during transit.
pub struct SocketTransfer(Record);

/// O(1) host storage per live descriptor. No table is sized from the fd limit.
#[derive(Default)]
pub struct Sockets {
    slots: Vec<Slot>,
    free: Vec<u32>,
    poller: Option<Arc<Poller>>,
    live: usize,
    waiting: usize,
    /// mc-units-light: registered in the pool's shared poller as this unit.
    shared: Option<u32>,
}
impl Sockets {
    /// Lazily created once per run, at its first real socket operation.
    pub fn poller(&mut self) -> io::Result<Arc<Poller>> {
        if self.poller.is_none() {
            if let Some(shared) = SHARED.get()
                && let Some(unit) = (shared.unit)()
            {
                self.shared = Some(unit);
                self.poller = Some(shared.poller.clone());
            } else {
                self.poller = Some(Poller::new()?);
            }
        }
        Ok(self.poller.as_ref().unwrap().clone())
    }
    pub fn active_poller(&self) -> Option<Arc<Poller>> {
        self.poller.clone()
    }
    /// Transfer a uniquely owned descriptor; harness descriptors need no poller until parked.
    pub fn insert(&mut self, fd: OwnedFd) -> Option<u64> {
        self.insert_fd(fd, false, 0)
    }

    /// Reserve descriptor table slots before a provider creates its resources.
    pub fn reserve(&mut self, count: usize) -> io::Result<()> {
        let new = count.saturating_sub(self.free.len());
        if self.slots.len().saturating_add(new) > u32::MAX as usize {
            return Err(io::Error::from_raw_os_error(libc::EMFILE));
        }
        self.slots
            .try_reserve(new)
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOMEM))
    }

    /// A pipe has one directional owner and closes its descriptor on release.
    pub fn insert_pipe(&mut self, fd: OwnedFd, write: bool) -> Option<u64> {
        self.insert_fd(fd, true, if write { 2 } else { 1 })
    }

    fn insert_fd(&mut self, fd: OwnedFd, pipe: bool, direction: u64) -> Option<u64> {
        let key = self.insert_record(Record {
            fd: Arc::new(fd),
            pipe,
            owners: 1 << direction,
            reset: false,
            ended: false,
            waiters: [None; 2],
            registered: false,
            read_drained: false,
            read_terminal: false,
            own_listener: false,
            abandoned: false,
            zero_copy: None,
        })?;
        Some(key | direction)
    }

    fn insert_record(&mut self, record: Record) -> Option<u64> {
        let index = if let Some(index) = self.free.pop() {
            index
        } else {
            // Each slot owns a descriptor, and descriptors stay below 2^31
            // (fs.nr_open), so an io_uring poll's direction bit is free.
            let index = u32::try_from(self.slots.len())
                .ok()
                .filter(|&index| u64::from(index) < WRITE_POLL)?;
            self.slots.push(Slot {
                generation: 1,
                record: None,
            });
            index
        };
        let slot = &mut self.slots[index as usize];
        slot.record = Some(record);
        self.live += 1;
        Some((u64::from(slot.generation) << 34) | (u64::from(index) << 2))
    }

    fn remove_record(&mut self, index: usize) -> Record {
        let slot = &mut self.slots[index];
        let record = slot.record.take().unwrap();
        self.live -= 1;
        self.waiting -= record.waiters.iter().flatten().count();
        // The shared poller sends only 16 generation bits.
        if slot.generation < u32::from(u16::MAX) {
            slot.generation += 1;
            self.free.push(index as u32);
        }
        record
    }

    /// Detach a whole socket or listener before publishing it.
    pub fn detach(&mut self, key: u64) -> io::Result<SocketTransfer> {
        assert_eq!(key & 3, 0);
        let index = ((key >> 2) as u32) as usize;
        let record = self.record(key);
        assert_eq!(record.owners, 1);
        assert_eq!(record.waiters, [None; 2]);
        if record.registered {
            let generation = self.slots[index].generation;
            self.poller.as_ref().unwrap().remove(
                record.fd.as_raw_fd(),
                Self::registration_tag(self.shared, index, generation),
                false,
            )?;
        }
        let mut record = self.remove_record(index);
        record.registered = false;
        record.read_drained = false;
        Ok(SocketTransfer(record))
    }

    /// Give a carrier a fresh identity and register it for this unit.
    pub fn attach(&mut self, carrier: SocketTransfer) -> io::Result<u64> {
        let key = self
            .insert_record(carrier.0)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EMFILE))?;
        if let Err(error) = self.register(key) {
            self.release(key);
            return Err(error);
        }
        Ok(key)
    }

    fn record(&self, key: u64) -> &Record {
        self.slots[((key >> 2) as u32) as usize]
            .record
            .as_ref()
            .unwrap()
    }
    fn record_mut(&mut self, key: u64) -> &mut Record {
        self.slots[((key >> 2) as u32) as usize]
            .record
            .as_mut()
            .unwrap()
    }
    pub fn fd(&self, key: u64) -> i32 {
        self.record(key).fd.as_raw_fd()
    }

    pub fn pipe(&self, key: u64) -> bool {
        self.record(key).pipe
    }
    pub fn reset(&self, key: u64) -> bool {
        self.record(key).reset
    }
    pub fn mark_reset(&mut self, key: u64) {
        let r = self.record_mut(key);
        r.reset = true;
        r.read_drained = false;
    }
    pub fn ended(&self, key: u64) -> bool {
        self.record(key).ended
    }
    pub fn mark_ended(&mut self, key: u64) {
        let r = self.record_mut(key);
        r.ended = true;
        r.read_drained = false;
    }
    /// Mark a listener this process created itself. An inherited one may
    /// still be listening for its launcher and is never shut down here.
    pub fn mark_own_listener(&mut self, key: u64) {
        self.record_mut(key).own_listener = true;
    }
    pub fn split(&mut self, key: u64) -> (u64, u64) {
        self.record_mut(key).owners = 6;
        (key | 1, key | 2)
    }
    /// Restore whole ownership of a known client socket without touching its fd.
    pub fn rejoin_client(&mut self, key: u64) {
        self.record_mut(key).owners = 1;
    }
    pub fn read_drained(&self, key: u64) -> bool {
        self.record(key).read_drained
    }
    /// Only a persistent interest (epoll) reports the next byte with no
    /// waiter parked; an io_uring reader always tries its syscall first.
    pub fn mark_read_drained(&mut self, key: u64) {
        let persistent = self.poller.as_ref().is_some_and(|p| p.persistent());
        let r = self.record_mut(key);
        if persistent && r.registered && !r.read_terminal && !r.reset {
            r.read_drained = true;
        }
    }
    fn registration_tag(shared: Option<u32>, index: usize, generation: u32) -> u64 {
        match shared {
            Some(unit) => shared_tag(unit, index, generation),
            None => tag(index, generation),
        }
    }

    fn register(&mut self, key: u64) -> io::Result<()> {
        let index = ((key >> 2) as u32) as usize;
        if self.slots[index].record.as_ref().unwrap().registered {
            return Ok(());
        }
        self.poller()?;
        let slot = &mut self.slots[index];
        let record = slot.record.as_mut().unwrap();
        self.poller.as_ref().unwrap().add(
            &record.fd,
            Self::registration_tag(self.shared, index, slot.generation),
        )?;
        record.registered = true;
        Ok(())
    }

    /// The ring and tag that zero-copy sends on `key` use: Some only for a
    /// socket of an io_uring poller that sends zero-copy. Registers the
    /// socket, so a send's answer readies its writer.
    pub fn send_target(&mut self, key: u64) -> io::Result<Option<(Arc<Poller>, u64)>> {
        self.register(key)?;
        let poller = self.poller.clone().unwrap();
        let Backend::Uring(ring) = &poller.backend else {
            return Ok(None);
        };
        if !ring.zc {
            return Ok(None);
        }
        let r = self.record_mut(key);
        let fd = r.fd.as_raw_fd();
        if !*r.zero_copy.get_or_insert_with(|| zero_copy_peer(fd)) {
            return Ok(None);
        }
        let index = ((key >> 2) as u32) as usize;
        let tag = Self::registration_tag(self.shared, index, self.slots[index].generation);
        Ok(Some((poller, tag)))
    }

    /// A send on `key` was abandoned (cancelled or past its deadline): until
    /// the next send starts, closing the socket resets it (`SO_LINGER{1,0}`),
    /// so bytes still queued for a peer that stopped reading are dropped at
    /// once instead of being retried after the close. The caller cancels its
    /// own zero-copy send, if any: one left in the kernel holds the socket
    /// open past its close.
    pub fn abandon_send(&mut self, key: u64) {
        self.record_mut(key).abandoned = true;
        Self::linger(self.fd(key), true);
    }

    /// A send on `key` starts: a close after it delivers what it queued.
    pub fn resume_send(&mut self, key: u64) {
        let r = self.record_mut(key);
        if std::mem::take(&mut r.abandoned) {
            Self::linger(r.fd.as_raw_fd(), false);
        }
    }

    fn linger(fd: i32, reset: bool) {
        let linger = libc::linger {
            l_onoff: i32::from(reset),
            l_linger: 0,
        };
        // SAFETY: a live socket; an initialized option value.
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_LINGER,
                (&raw const linger).cast(),
                size_of::<libc::linger>() as u32,
            )
        };
    }

    /// Register a socket once and publish this direction's current waiter.
    /// Only the registry's holder dispatches completions, so a poll armed
    /// here before the waiter is published still finds it.
    pub fn arm(&mut self, key: u64, write: bool, token: u64) -> io::Result<u64> {
        self.register(key)?;
        let index = ((key >> 2) as u32) as usize;
        let generation = self.slots[index].generation;
        self.poller.as_ref().unwrap().park(
            Self::registration_tag(self.shared, index, generation),
            write,
        );
        let r = self.record_mut(key);
        if r.waiters[usize::from(write)].replace(token).is_none() {
            self.waiting += 1;
        }
        Ok(tag(index, generation))
    }
    pub fn disarm(&mut self, key: u64, write: bool) {
        if self.record_mut(key).waiters[usize::from(write)]
            .take()
            .is_some()
        {
            self.waiting -= 1;
        }
    }
    /// An edge names a live socket. A direction's waiter still retries its
    /// syscall before committing a result, including after stale readiness.
    pub fn dispatch(&mut self, identity: u64, flags: u32, ready: impl FnMut(u64)) {
        Self::dispatch_edge(&mut self.slots, &mut self.waiting, identity, flags, ready);
    }

    fn dispatch_edge(
        slots: &mut [Slot],
        waiting: &mut usize,
        identity: u64,
        flags: u32,
        mut ready: impl FnMut(u64),
    ) {
        let index = identity as u32 as usize;
        let Some(slot) = slots.get_mut(index) else {
            return;
        };
        // A shared tag keeps 16 generation bits. A stale edge that matches
        // only readies a waiter that retries its syscall.
        if slot.generation & 0xffff != (identity >> 32) as u32 & 0xffff {
            return;
        }
        let Some(r) = slot.record.as_mut() else {
            return;
        };
        let failed = flags & (libc::EPOLLERR | libc::EPOLLHUP) as u32 != 0;
        let terminal = failed || flags & libc::EPOLLRDHUP as u32 != 0;
        if terminal || flags & libc::EPOLLIN as u32 != 0 {
            r.read_drained = false;
            r.read_terminal |= terminal;
            if let Some(token) = r.waiters[0].take() {
                *waiting -= 1;
                ready(token);
            }
        }
        if (failed || flags & libc::EPOLLOUT as u32 != 0)
            && let Some(token) = r.waiters[1].take()
        {
            *waiting -= 1;
            ready(token);
        }
    }
    /// Poll only returned registrations while holding the driver token
    /// and exclusive access to the socket registry.
    pub fn poll(&mut self, timeout_ms: i32, ready: impl FnMut(u64)) -> io::Result<usize> {
        if (timeout_ms == 0 && self.waiting == 0) || self.shared.is_some() {
            return Ok(0);
        }
        let Some(poller) = self.poller.as_ref() else {
            return Ok(0);
        };
        let Some(events) = poller.poll(timeout_ms)? else {
            return Ok(0);
        };
        Ok(Self::dispatch_batch(
            &mut self.slots,
            &mut self.waiting,
            events,
            ready,
        ))
    }

    /// Apply a poll from this registry's driver. Closing or reusing a slot
    /// invalidates any edge still in a worker's poll batch.
    pub fn dispatch_polled(&mut self, events: PollEvents<'_>, ready: impl FnMut(u64)) -> usize {
        Self::dispatch_batch(&mut self.slots, &mut self.waiting, events, ready)
    }

    fn dispatch_batch(
        slots: &mut [Slot],
        waiting: &mut usize,
        events: PollEvents<'_>,
        mut ready: impl FnMut(u64),
    ) -> usize {
        for event in events.0.iter() {
            if event.u64 != WAKE {
                Self::dispatch_edge(slots, waiting, event.u64, event.events, &mut ready);
            }
        }
        events.0.len()
    }

    pub fn release(&mut self, key: u64) -> bool {
        let r = self.record_mut(key);
        let direction = key & 3;
        if direction == 2 && !r.pipe {
            // SAFETY: the record owns the connected descriptor through final release.
            unsafe { libc::shutdown(r.fd.as_raw_fd(), libc::SHUT_WR) };
        }
        r.owners &= !(1 << direction);
        if r.owners != 0 {
            return false;
        }
        let index = ((key >> 2) as u32) as usize;
        let tag = Self::registration_tag(self.shared, index, self.slots[index].generation);
        let record = self.remove_record(index);
        if record.registered {
            // Unregister before dropping the sole descriptor owner.
            let _ = self.poller.as_ref().unwrap().remove(
                record.fd.as_raw_fd(),
                tag,
                record.own_listener,
            );
        }
        true
    }
    pub fn len(&self) -> usize {
        self.live
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
impl Drop for Sockets {
    fn drop(&mut self) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(r) = slot.record.take() {
                if r.registered {
                    let tag = Self::registration_tag(self.shared, index, slot.generation);
                    let _ =
                        self.poller
                            .as_ref()
                            .unwrap()
                            .remove(r.fd.as_raw_fd(), tag, r.own_listener);
                }
                drop(r);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, os::unix::net::UnixStream};

    #[test]
    fn a_busy_driver_turn_leaves_readiness_for_the_next_worker() {
        let mut sockets = Sockets::default();
        let (socket, mut peer) = UnixStream::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        let key = sockets.insert(socket.into()).unwrap();
        sockets.arm(key, false, 17).unwrap();
        let driver = sockets.poller().unwrap();
        let held = driver.try_turn().unwrap();
        peer.write_all(b"x").unwrap();
        let busy = std::thread::scope(|scope| {
            scope
                .spawn(|| driver.poll(0).unwrap().is_none())
                .join()
                .unwrap()
        });
        drop(held);
        assert!(busy);
        let events = driver.poll(0).unwrap().unwrap();
        assert!(driver.poll(0).unwrap().is_none());
        let mut tokens = Vec::new();
        sockets.dispatch_polled(events, |token| tokens.push(token));
        assert_eq!(tokens, [17]);

        let events = driver.poll(0).unwrap().unwrap();
        assert_eq!(sockets.dispatch_polled(events, |_| unreachable!()), 0);
    }

    /// Poll until `token` is readied or two seconds pass.
    fn readied(sockets: &mut Sockets, token: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let mut found = false;
            sockets.poll(20, |t| found |= t == token).unwrap();
            if found {
                return true;
            }
        }
        false
    }

    #[test]
    fn refused_submit_keeps_a_closed_socket_until_consumed() {
        use std::sync::atomic::Ordering::Relaxed;
        if reactor().unwrap() != Reactor::IoUring {
            return;
        }
        let (socket, _peer) = UnixStream::pair().unwrap();
        let mut sockets = Sockets::default();
        let key = sockets.insert(socket.into()).unwrap();
        sockets.record_mut(key).zero_copy = Some(true);
        let (poller, tag) = sockets.send_target(key).unwrap().unwrap();
        let Backend::Uring(ring) = &poller.backend else {
            unreachable!()
        };
        let owner = Arc::downgrade(&sockets.record(key).fd);
        let mut slot = poller.tx_slot(1).unwrap();
        slot.bytes()[0] = b'x';

        ring.refuse_submit.store(true, Relaxed);
        sockets.arm(key, false, 17).unwrap();
        slot.send(tag, 0..1).unwrap();
        slot.cancel();
        drop(slot);
        assert!(sockets.release(key));
        assert!(owner.upgrade().is_some(), "closed before SQ consumption");

        ring.refuse_submit.store(false, Relaxed);
        poller.poll(0).unwrap();
        assert!(owner.upgrade().is_none(), "retained after SQ consumption");
    }

    /// Workers migrate, so the thread that registered a socket may be gone
    /// when it becomes readable. io_uring ties a poll to its submitting task.
    #[test]
    fn a_socket_registered_by_an_exited_thread_still_reports_readiness() {
        let mut sockets = Sockets::default();
        let (socket, mut peer) = UnixStream::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        let key = sockets.insert(socket.into()).unwrap();
        let sockets = std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    sockets.arm(key, false, 5).unwrap();
                    sockets
                })
                .join()
                .unwrap()
        });
        let mut sockets = sockets;
        // The registration's first report, before any byte arrived.
        sockets.poll(0, |_| {}).unwrap();
        peer.write_all(b"x").unwrap();
        sockets.arm(key, false, 6).unwrap();
        assert!(readied(&mut sockets, 6), "{:?}", reactor());
    }

    /// Each direction of a socket waits on its own readiness. A writer that
    /// parks on a full socket while a reader's wait is already in the kernel
    /// is woken once the peer drains it, and that wakes no reader, on epoll
    /// and, where the kernel offers it, on io_uring.
    #[test]
    fn a_writer_parked_beside_a_waiting_reader_is_woken_alone() {
        use std::io::Read;
        let mut pollers = vec![Poller::new_epoll().unwrap()];
        if kernel_offers_io_uring() {
            pollers.push(Poller::new_uring().unwrap());
        }
        for poller in pollers {
            let backend = poller.reactor();
            let mut sockets = Sockets::default();
            sockets.poller = Some(poller);
            let (mut socket, mut peer) = UnixStream::pair().unwrap();
            socket.set_nonblocking(true).unwrap();
            peer.set_nonblocking(true).unwrap();
            let fd = socket.try_clone().unwrap();
            let key = sockets.insert(fd.into()).unwrap();
            sockets.arm(key, false, 1).unwrap();
            // Let any first report settle so the reader's wait is in the kernel.
            sockets.poll(0, |_| {}).unwrap();
            sockets.arm(key, false, 1).unwrap();
            // The writer parks only after its write would block.
            let chunk = [0u8; 4096];
            while socket.write(&chunk).is_ok() {}
            sockets.arm(key, true, 2).unwrap();
            let mut drain = [0u8; 65536];
            while peer.read(&mut drain).is_ok() {}
            let mut woken = Vec::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !woken.contains(&2) && std::time::Instant::now() < deadline {
                sockets.poll(20, |token| woken.push(token)).unwrap();
            }
            assert_eq!(woken, [2], "{backend:?}");
        }
    }

    /// More sockets turn ready at once than the io_uring completion queue
    /// holds. Every one must still be reported: none of the edges past the
    /// queue's capacity may be lost.
    #[test]
    fn readiness_beyond_the_completion_queue_is_all_reported() {
        const SOCKETS: usize = 3 * Uring::COMPLETIONS as usize;
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: plain resource-limit calls on this process.
        unsafe {
            assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit), 0);
            limit.rlim_cur = limit.rlim_max;
            assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
        }
        assert!(limit.rlim_cur as usize > 2 * SOCKETS + 64, "{limit:?}");
        let mut sockets = Sockets::default();
        let mut peers = Vec::with_capacity(SOCKETS);
        let mut keys = Vec::with_capacity(SOCKETS);
        for token in 0..SOCKETS as u64 {
            let (socket, peer) = UnixStream::pair().unwrap();
            socket.set_nonblocking(true).unwrap();
            let key = sockets.insert(socket.into()).unwrap();
            sockets.arm(key, false, token).unwrap();
            keys.push(key);
            peers.push(peer);
        }
        // Each registration's first report, before any byte arrived. A reader
        // woken now finds no byte and waits again, as a real waiter does. The
        // reactor must settle: no readiness is reported while nothing changes.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut woken = Vec::new();
        while sockets.poll(0, |token| woken.push(token)).unwrap() != 0 {
            for token in woken.drain(..) {
                sockets.arm(keys[token as usize], false, token).unwrap();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "readiness never settled under {:?}",
                reactor()
            );
        }
        for peer in &mut peers {
            peer.write_all(b"x").unwrap();
        }
        let mut seen = vec![false; SOCKETS];
        let mut left = SOCKETS;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while left != 0 && std::time::Instant::now() < deadline {
            sockets
                .poll(20, |token| {
                    let seen = &mut seen[token as usize];
                    left -= usize::from(!*seen);
                    *seen = true;
                })
                .unwrap();
        }
        assert_eq!(
            left,
            0,
            "{left} of {SOCKETS} unreported under {:?}",
            reactor()
        );
    }

    const SLOT: usize = 256 << 10;

    /// A loopback TCP connection whose peer has stopped reading, with small
    /// buffers, so a large send waits in the kernel. None unless this
    /// process's poller sends zero-copy.
    fn stuck_peer() -> Option<(Sockets, u64, Arc<Poller>, u64, std::net::TcpStream)> {
        let (mut sockets, key, peer) = small_pair();
        // These laws are about the slots, so their loopback peer sends
        // zero-copy as under `always`.
        sockets.record_mut(key).zero_copy = Some(true);
        let (poller, tag) = sockets.send_target(key).unwrap()?;
        Some((sockets, key, poller, tag, peer))
    }

    /// A registered loopback client whose writes back up after a few
    /// 16 KiB buffers, and its peer.
    fn small_pair() -> (Sockets, u64, std::net::TcpStream) {
        use std::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let peer =
            crate::loopback::accept_from_address(&listener, client.local_addr().unwrap()).unwrap();
        for (socket, option) in [(&client, libc::SO_SNDBUF), (&peer, libc::SO_RCVBUF)] {
            let size = 16384i32;
            // SAFETY: a live socket; an initialized option value.
            unsafe {
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    option,
                    (&raw const size).cast(),
                    4,
                )
            };
        }
        client.set_nonblocking(true).unwrap();
        let mut sockets = Sockets::default();
        let key = sockets.insert(OwnedFd::from(client)).unwrap();
        (sockets, key, peer)
    }

    /// Reap completions until `done` holds, for at most five seconds.
    fn reap_until(sockets: &mut Sockets, mut done: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "no completion");
            sockets.poll(10, |_| {}).unwrap();
        }
    }

    /// A slot whose send waits, cancelled once part of it is in the kernel's
    /// write queue: the kernel still reads that part from the slot.
    fn cancelled_send(sockets: &mut Sockets, poller: &Arc<Poller>, tag: u64) -> TxSlot {
        let mut slot = poller.tx_slot(SLOT).unwrap();
        slot.bytes().fill(b'a');
        slot.send(tag, 0..SLOT).unwrap();
        // The send takes what the window allows and then waits.
        std::thread::sleep(std::time::Duration::from_millis(20));
        sockets.poll(0, |_| {}).unwrap();
        assert_eq!(slot.state(), TxState::Sending);
        slot
    }

    fn sent(slot: &TxSlot) -> usize {
        match slot.state() {
            TxState::Sent(n) if n > 0 => n as usize,
            state => panic!("{state:?}"),
        }
    }

    /// Fill the slots the next sends would take.
    fn refill(poller: &Arc<Poller>) -> Vec<TxSlot> {
        (0..4)
            .map(|_| {
                let mut slot = poller.tx_slot(SLOT).unwrap();
                slot.bytes().fill(b'b');
                slot
            })
            .collect()
    }

    fn read_all_a(peer: &mut std::net::TcpStream, n: usize) {
        use std::io::Read;
        let mut got = vec![0; n];
        peer.read_exact(&mut got).unwrap();
        let wrong = got.iter().filter(|&&b| b != b'a').count();
        assert_eq!(
            wrong, 0,
            "{wrong} of {n} bytes the peer read are not the sent bytes"
        );
    }

    /// A send's answer does not end the kernel's reads of its slot: until
    /// the notification, the queued part is still read from it. A claim
    /// dropped after the answer must not hand the slot to the next send.
    #[test]
    fn an_answered_slot_is_not_reused_before_its_notification() {
        let Some((mut sockets, key, poller, tag, mut peer)) = stuck_peer() else {
            return;
        };
        let slot = cancelled_send(&mut sockets, &poller, tag);
        slot.cancel();
        reap_until(&mut sockets, || matches!(slot.state(), TxState::Sent(_)));
        let n = sent(&slot);
        drop(slot);
        let _next = refill(&poller);
        read_all_a(&mut peer, n);
    }

    /// A claim dropped before its cancelled send answers: the answer comes
    /// with more to follow, and the slot waits for that notification too.
    #[test]
    fn a_cancelled_send_keeps_its_slot_until_the_notification() {
        let Some((mut sockets, key, poller, tag, mut peer)) = stuck_peer() else {
            return;
        };
        let slot = cancelled_send(&mut sockets, &poller, tag);
        let (notified, _) = poller.tx_notifications();
        slot.cancel();
        drop(slot);
        // The answer, and not yet the notification, which waits for the peer.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        while std::time::Instant::now() < deadline {
            sockets.poll(10, |_| {}).unwrap();
        }
        assert_eq!(poller.tx_notifications().0, notified);
        let _next = refill(&poller);
        // Everything the cancelled send took: the window's worth, copied when
        // it arrived, then the write queue's, read from the slot only now.
        use std::io::Read;
        peer.set_read_timeout(Some(std::time::Duration::from_millis(200)))
            .unwrap();
        let mut got = Vec::new();
        let mut buf = vec![0; 1 << 16];
        while let Ok(n @ 1..) = peer.read(&mut buf) {
            got.extend_from_slice(&buf[..n]);
        }
        assert!(got.len() > 2 * 16384, "{}", got.len());
        let wrong = got.iter().filter(|&&b| b != b'a').count();
        assert_eq!(
            wrong,
            0,
            "{wrong} of {} bytes the peer read are not the sent bytes",
            got.len()
        );
    }

    /// A ring closed while the kernel still reads a slot: those bytes must
    /// never return to the allocator.
    #[test]
    fn a_ring_dropped_with_notifications_owed_keeps_their_bytes() {
        // Slots come from the heap, where a freed one is handed out again.
        // SAFETY: a process-wide malloc tuning, harmless to other tests.
        unsafe { libc::mallopt(libc::M_MMAP_THRESHOLD, 64 << 20) };
        let Some((mut sockets, key, poller, tag, mut peer)) = stuck_peer() else {
            return;
        };
        let slot = cancelled_send(&mut sockets, &poller, tag);
        slot.cancel();
        reap_until(&mut sockets, || matches!(slot.state(), TxState::Sent(_)));
        let n = sent(&slot);
        drop((slot, sockets, poller));
        let reused: Vec<Vec<u8>> = (0..8).map(|_| vec![b'b'; SLOT]).collect();
        read_all_a(&mut peer, n);
        drop(reused);
    }

    /// Slots stay kept while sends go on, and a ring that stopped sending
    /// frees them: a blocking turn wakes for the trim, which comes a full
    /// interval after the last slot taken, so idle memory returns to 0.
    #[test]
    fn a_ring_that_stopped_sending_frees_its_slots() {
        let Some((mut sockets, key, poller, tag, mut peer)) = stuck_peer() else {
            return;
        };
        let Backend::Uring(ring) = &poller.backend else {
            unreachable!()
        };
        let mut slot = poller.tx_slot(1024).unwrap();
        slot.bytes().fill(b'a');
        slot.send(tag, 0..1024).unwrap();
        drop(slot);
        let (notified, _) = poller.tx_notifications();
        reap_until(&mut sockets, || poller.tx_notifications().0 > notified);
        read_all_a(&mut peer, 1024);
        assert!(ring.tx_kept());
        let start = std::time::Instant::now();
        let long = (3 * sendzc::TRIM).as_millis() as i32;
        while ring.tx_kept() && start.elapsed() < 3 * sendzc::TRIM {
            sockets.poll(long, |_| {}).unwrap();
        }
        let took = start.elapsed();
        assert!(
            (sendzc::TRIM / 2..2 * sendzc::TRIM).contains(&took) && !ring.tx_kept(),
            "idle slots freed after {took:?}, kept: {}",
            ring.tx_kept()
        );
    }

    /// An abandoned send on a peer that stopped reading: its queued bytes
    /// are dropped at the close, the peer sees a reset rather than every
    /// committed byte, and the slot's notification follows promptly.
    #[test]
    fn an_abandoned_send_leaves_no_queued_bytes_after_close() {
        use std::io::Read;
        let Some((mut sockets, key, poller, tag, mut peer)) = stuck_peer() else {
            return;
        };
        let slot = cancelled_send(&mut sockets, &poller, tag);
        let (notified, _) = poller.tx_notifications();
        slot.cancel();
        sockets.abandon_send(key);
        drop(slot);
        assert!(sockets.release(key));
        reap_until(&mut sockets, || poller.tx_notifications().0 > notified);
        let mut total = 0;
        let mut buf = vec![0; 1 << 16];
        let end = loop {
            match peer.read(&mut buf) {
                Ok(0) => break None,
                Ok(n) => total += n,
                Err(e) => break Some(e.kind()),
            }
        };
        assert_eq!(
            end,
            Some(io::ErrorKind::ConnectionReset),
            "after {total} bytes"
        );
    }

    /// `auto` treats every loopback form as loopback, including an IPv4
    /// loopback peer of an IPv6 socket, and nothing else.
    #[test]
    fn auto_send_zc_skips_loopback_peers_only() {
        use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
        assert!(is_loopback(&SocketAddr::from((
            Ipv4Addr::new(127, 3, 2, 1),
            9
        ))));
        assert!(is_loopback(&SocketAddr::from((Ipv6Addr::LOCALHOST, 9))));
        assert!(is_loopback(&SocketAddr::from((
            Ipv4Addr::new(127, 0, 0, 1).to_ipv6_mapped(),
            9
        ))));
        assert!(!is_loopback(&SocketAddr::from((
            Ipv4Addr::new(10, 0, 0, 1),
            9
        ))));
        assert!(!is_loopback(&SocketAddr::from((
            Ipv4Addr::new(10, 0, 0, 1).to_ipv6_mapped(),
            9
        ))));
    }

    /// A send that starts after an abandoned one takes back its reset: the
    /// close then delivers every queued byte and a FIN, under both reactors.
    #[test]
    fn a_send_after_an_abandoned_one_closes_without_a_reset() {
        use std::io::Read;
        let (mut sockets, key, mut peer) = small_pair();
        sockets.abandon_send(key);
        sockets.resume_send(key);
        let bytes = vec![b'c'; 1 << 16];
        let mut queued = 0;
        while queued < bytes.len() {
            // SAFETY: a live socket and an in-bounds buffer.
            let n = unsafe {
                libc::send(
                    sockets.fd(key),
                    bytes[queued..].as_ptr().cast(),
                    bytes.len() - queued,
                    libc::MSG_NOSIGNAL,
                )
            };
            if n <= 0 {
                break;
            }
            queued += n as usize;
        }
        assert!(sockets.release(key));
        let mut received = Vec::new();
        peer.read_to_end(&mut received)
            .unwrap_or_else(|e| panic!("{e} after {} of {queued} bytes", received.len()));
        assert_eq!(received.len(), queued);
    }

    /// Releasing a listener frees its address on return, as closing it
    /// does, including when another thread registered it.
    #[test]
    fn a_released_listener_can_be_bound_again_at_once() {
        for _ in 0..50 {
            let mut sockets = Sockets::default();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let key = sockets.insert(listener.into()).unwrap();
            sockets.mark_own_listener(key);
            let mut sockets = std::thread::scope(|scope| {
                scope
                    .spawn(move || {
                        sockets.arm(key, false, 1).unwrap();
                        sockets
                    })
                    .join()
                    .unwrap()
            });
            assert!(sockets.release(key));
            std::net::TcpListener::bind(address)
                .unwrap_or_else(|e| panic!("{address} under {:?}: {e}", reactor()));
        }
    }

    /// Set by the choice law for the fresh process it runs this test in.
    const EXPECT_ENV: &str = "HIRSEL_NET_REACTOR_LAW_EXPECT";
    const REFUSE_ENV: &str = "HIRSEL_NET_REACTOR_LAW_REFUSE";

    /// Whether a ring with the features the reactor needs (NODROP, EXT_ARG)
    /// can be set up here, asked of the kernel directly.
    fn kernel_offers_io_uring() -> bool {
        const FEAT_NODROP: u32 = 1 << 1;
        const FEAT_EXT_ARG: u32 = 1 << 8;
        // struct io_uring_params: 120 bytes, `features` at byte 20.
        let mut params = [0u32; 30];
        // SAFETY: io_uring_setup writes only into the 120-byte params.
        let fd = unsafe { libc::syscall(libc::SYS_io_uring_setup, 1u32, params.as_mut_ptr()) };
        if fd < 0 {
            return false;
        }
        // SAFETY: the ring descriptor was just created and is owned here.
        drop(unsafe { OwnedFd::from_raw_fd(fd as i32) });
        let wanted = FEAT_NODROP | FEAT_EXT_ARG;
        params[5] & wanted == wanted
    }

    /// Make this thread's `io_uring_setup` fail with EPERM, as a container's
    /// seccomp profile does.
    fn refuse_io_uring_setup() {
        let statement = |code: u32, jt, jf, k| libc::sock_filter {
            code: code as u16,
            jt,
            jf,
            k,
        };
        let ret = libc::BPF_RET | libc::BPF_K;
        let filter = [
            // seccomp_data.nr
            statement(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0, 0, 0),
            statement(
                libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
                0,
                1,
                libc::SYS_io_uring_setup as u32,
            ),
            statement(ret, 0, 0, libc::SECCOMP_RET_ERRNO | libc::EPERM as u32),
            statement(ret, 0, 0, libc::SECCOMP_RET_ALLOW),
        ];
        let program = libc::sock_fprog {
            len: filter.len() as u16,
            filter: filter.as_ptr().cast_mut(),
        };
        // SAFETY: the filter outlives the call, which copies it.
        unsafe {
            assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
            assert_eq!(
                libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program),
                0,
                "{}",
                io::Error::last_os_error()
            );
        }
    }

    /// The fresh-process half of the choice law below.
    #[test]
    #[ignore = "run by the reactor choice law in a fresh process"]
    fn the_reactor_this_process_chose() {
        let Some(expected) = std::env::var_os(EXPECT_ENV) else {
            return;
        };
        if std::env::var_os(REFUSE_ENV).is_some() {
            refuse_io_uring_setup();
        }
        assert_eq!(reactor().unwrap().name(), expected);
        let mut sockets = Sockets::default();
        let (socket, mut peer) = UnixStream::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        let key = sockets.insert(socket.into()).unwrap();
        sockets.arm(key, false, 3).unwrap();
        sockets.poll(0, |_| {}).unwrap();
        peer.write_all(b"x").unwrap();
        sockets.arm(key, false, 4).unwrap();
        assert!(readied(&mut sockets, 4));
    }

    /// D5: with the variable unset, a process takes io_uring wherever the
    /// kernel offers it and is not below the release gate, and epoll, still
    /// carrying readiness, where `io_uring_setup` is refused.
    #[test]
    fn an_unset_reactor_takes_io_uring_where_offered_and_epoll_where_refused() {
        let offered = if kernel_offers_io_uring() && release_allows_uring(&kernel_release()) {
            "io_uring"
        } else {
            "epoll"
        };
        for (refused, expected) in [(false, offered), (true, "epoll")] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "reactor::tests::the_reactor_this_process_chose",
                    "--ignored",
                    "--nocapture",
                ])
                .env_remove(REACTOR_ENV)
                .env(EXPECT_ENV, expected);
            if refused {
                child.env(REFUSE_ENV, "1");
            }
            let out = child.output().unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && stdout.contains("1 passed"),
                "refused={refused} expected {expected}:\n{stdout}{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// The gate admits 6.13 and later by numeric (major, minor), whatever
    /// the distribution suffix; everything through 6.12.y, and an unreadable
    /// release, stays on epoll.
    #[test]
    fn the_release_gate_admits_6_13_and_later_only() {
        for (release, allowed) in [
            ("6.12.118", false),
            ("6.12.999-generic", false),
            ("6.13", true),
            ("6.13.0", true),
            ("6.13.0-rc1", true),
            ("6.13-rc1", true),
            ("6.8.0-149-generic", false),
            ("6.1.0-27-amd64", false),
            ("6.2", false),
            ("5.15.0-130-generic", false),
            ("5.130.0", false),
            ("7.0.0-43-generic", true),
            ("10.1", true),
            ("6", false),
            ("", false),
            ("generic", false),
        ] {
            assert_eq!(release_allows_uring(release), allowed, "{release}");
        }
    }
}
