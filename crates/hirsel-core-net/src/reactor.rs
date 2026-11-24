//! Readiness interests, adapted from Go runtime/netpoll_epoll.go.
//! Slot generations identify sockets across poll and dispatch.
//!
//! Two kernel interfaces carry them, both persistently: epoll keeps one
//! edge-triggered interest per descriptor, and io_uring one multishot poll
//! per descriptor for every direction. A poller is owned: only its owner (a
//! unit pool's worker slot, or a tied run's driver) submits to it and waits
//! in it; other threads post to its mailbox and wake it ([`slot_pollers`]).
//! One interface is chosen once per process ([`reactor`]); both turn a
//! kernel turn into the same `(tag, flags)` events, so dispatch is shared.
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{Arc, Mutex, MutexGuard, OnceLock, TryLockError},
};
mod sendzc;
mod uring;
pub use sendzc::{TxSlot, TxState};
use uring::{Uring, msg_ring_wake};

const POLL_BATCH: i32 = 128;
/// A wake's tag: the eventfd's on epoll, a MSG_RING completion's on io_uring.
const WAKE: u64 = u64::MAX;
/// io_uring: the completion of a poll removal, which carries no readiness.
const REMOVED: u64 = u64::MAX - 1;
const INTEREST: u32 = (libc::EPOLLIN | libc::EPOLLOUT | libc::EPOLLRDHUP) as u32;
/// io_uring: a multishot poll carries its tag with this bit flipped at each
/// re-arm on adoption ([`Uring::adopt`]), so the ends of the poll it replaced
/// are told from its own. Tags never use it: a slot index stays below it
/// ([`Sockets::insert_record`]) and WAKE and REMOVED are matched first.
const ARM_PARITY: u64 = 1 << 31;
/// `IORING_REGISTER_SEND_MSG_RING`: a MSG_RING issued without a source ring.
const REGISTER_SEND_MSG_RING: libc::c_long = 31;

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
    /// The owner's turn: held while a thread submits to or waits in this
    /// poller, which makes it the poller's owner for that turn.
    events: Mutex<Vec<libc::epoll_event>>,
    operations: &'static PollOperations,
}

enum Backend {
    /// The epoll fd and its wake eventfd, watched edge-triggered and never
    /// read: each write is a fresh edge.
    Epoll {
        epoll: OwnedFd,
        wake: OwnedFd,
    },
    Uring(Box<Uring>),
}

struct PollOperations {
    poll: for<'a> fn(&'a Poller, Turn<'a>, Option<u64>) -> io::Result<Option<PollEvents<'a>>>,
    wake: fn(&Poller),
}

/// The driver token: one thread at a time submits, polls and dispatches.
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
fn timeout_ns(timeout_ms: i32) -> Option<u64> {
    u64::try_from(timeout_ms).ok().map(|ms| ms * 1_000_000)
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
        Ok(Self::from_uring(probed.map_or_else(Uring::open, Ok)?))
    }
    fn from_uring(ring: Uring) -> Arc<Self> {
        Arc::new(Self {
            backend: Backend::Uring(Box::new(ring)),
            events: Mutex::new(Vec::with_capacity(POLL_BATCH as usize)),
            operations: &PollOperations {
                poll: Self::poll_uring,
                wake: Self::wake_ring,
            },
        })
    }
    fn new_epoll() -> io::Result<Arc<Self>> {
        // SAFETY: creates a new, uniquely owned CLOEXEC descriptor.
        let epoll =
            unsafe { OwnedFd::from_raw_fd(checked(libc::epoll_create1(libc::EPOLL_CLOEXEC))?) };
        let poller = Arc::new(Self {
            backend: Backend::Epoll {
                epoll,
                wake: eventfd()?,
            },
            events: Mutex::new(Vec::with_capacity(POLL_BATCH as usize)),
            operations: &PollOperations {
                poll: Self::poll_epoll,
                wake: Self::wake_eventfd,
            },
        });
        poller.epoll_ctl(
            libc::EPOLL_CTL_ADD,
            poller.wake_fd(),
            (libc::EPOLLIN | libc::EPOLLET) as u32,
            WAKE,
        )?;
        Ok(poller)
    }
    /// The interface this poller waits on.
    pub fn reactor(&self) -> Reactor {
        match self.backend {
            Backend::Epoll { .. } => Reactor::Epoll,
            Backend::Uring(_) => Reactor::IoUring,
        }
    }
    fn epoll_ctl(&self, op: i32, fd: i32, flags: u32, token: u64) -> io::Result<()> {
        let Backend::Epoll { epoll, .. } = &self.backend else {
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
    /// Watch `fd` for every direction under `tag`. epoll's interest takes
    /// effect at once from any thread; an io_uring poll is posted to the
    /// mailbox and armed at the owner's next enter, so an `owner` caller
    /// leaves it there and anyone else wakes the owner for it.
    fn add(&self, fd: &Arc<OwnedFd>, tag: u64, owner: bool) -> io::Result<()> {
        match &self.backend {
            Backend::Epoll { .. } => self.epoll_ctl(
                libc::EPOLL_CTL_ADD,
                fd.as_raw_fd(),
                INTEREST | libc::EPOLLET as u32,
                tag,
            ),
            Backend::Uring(ring) => {
                ring.add(tag, fd.clone());
                if !owner {
                    self.wake();
                }
                Ok(())
            }
        }
    }
    /// Stop watching `fd`. `closing` names a listener whose only holder is
    /// about to close it: an io_uring poll holds its own file reference until
    /// the kernel retires it, so the listener is shut down first and its
    /// address is free for rebinding on return, as after an epoll close. The
    /// removal enters at once from an `owner` with the turn free, and
    /// otherwise wakes the owner, so the file closes (and a socket's FIN
    /// goes out) without waiting for the owner's next turn.
    fn remove(&self, fd: i32, tag: u64, closing: bool, owner: bool) -> io::Result<()> {
        match &self.backend {
            Backend::Epoll { .. } => self.epoll_ctl(libc::EPOLL_CTL_DEL, fd, 0, 0),
            Backend::Uring(ring) => {
                if closing {
                    // SAFETY: a live descriptor this process alone holds; a
                    // receive shutdown changes nothing the close that follows
                    // does not.
                    unsafe { libc::shutdown(fd, libc::SHUT_RD) };
                }
                if ring.remove(tag) {
                    self.submit_or_wake(owner);
                }
                Ok(())
            }
        }
    }
    /// io_uring: submit the mailbox now when the caller owns this poller and
    /// its turn is free; otherwise wake the owner to submit it.
    fn submit_or_wake(&self, owner: bool) {
        let Backend::Uring(ring) = &self.backend else {
            return;
        };
        if owner && let Some(_turn) = self.try_turn() {
            ring.enter(Some(0));
        } else {
            self.wake();
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
    /// thread polls or dispatches. A zero timeout is a nonblocking turn.
    pub fn poll(&self, timeout_ms: i32) -> io::Result<Option<PollEvents<'_>>> {
        self.wait(timeout_ms, || false)
    }

    /// Poll with `woken` checked once the turn is held. A waker that sets its
    /// flag before `wake` is seen here even when another thread's turn has
    /// already taken the wake: that turn ended before this one began.
    pub fn wait(
        &self,
        timeout_ms: i32,
        woken: impl FnOnce() -> bool,
    ) -> io::Result<Option<PollEvents<'_>>> {
        let Some(events) = self.try_turn() else {
            return Ok(None);
        };
        let timeout_ms = if woken() { 0 } else { timeout_ms };
        (self.operations.poll)(self, events, timeout_ns(timeout_ms))
    }

    /// A unit pool slot's turn on its own io_uring ring: submit its mailbox
    /// and reap into `out`, blocking up to `timeout_ns` (None: until a
    /// completion; Some(0): not at all). Returns the events written.
    pub fn wait_slot(&self, out: &mut [libc::epoll_event], timeout_ns: Option<u64>) -> usize {
        let Backend::Uring(ring) = &self.backend else {
            unreachable!("a pool waits on an epoll slot poller directly")
        };
        let _turn = self.events.lock().unwrap_or_else(|e| e.into_inner());
        ring.wait(out, timeout_ns)
    }

    /// io_uring: whether the owner has anything to take from its ring now: a
    /// posted completion, task work the kernel queued for it, or a mailbox
    /// entry to submit. Read without entering the kernel.
    pub fn due(&self) -> bool {
        let Backend::Uring(ring) = &self.backend else {
            return false;
        };
        ring.due()
    }

    /// io_uring: the calling thread becomes this ring's owner. A poll's
    /// readiness runs as task work on the thread that armed it, so every
    /// watch is re-armed from this thread at its next enter.
    pub fn adopt(&self) {
        if let Backend::Uring(ring) = &self.backend {
            ring.adopt();
        }
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
        let timeout_ms = timeout_ns.map_or(-1, |ns| {
            i32::try_from(ns.div_ceil(1_000_000)).unwrap_or(i32::MAX)
        });
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
        Ok(Some(PollEvents(events)))
    }

    fn poll_uring<'a>(
        &'a self,
        mut events: Turn<'a>,
        timeout_ns: Option<u64>,
    ) -> io::Result<Option<PollEvents<'a>>> {
        let Backend::Uring(ring) = &self.backend else {
            unreachable!()
        };
        events.clear();
        crate::EPOLL_WAITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        events.resize(POLL_BATCH as usize, libc::epoll_event { events: 0, u64: 0 });
        let len = ring.wait(&mut events, timeout_ns);
        events.truncate(len);
        Ok(Some(PollEvents(events)))
    }

    /// Wake this poller's owner from any thread: an eventfd edge on epoll, a
    /// MSG_RING completion on io_uring. Either is reported as a `WAKE` event.
    pub fn wake(&self) {
        (self.operations.wake)(self);
    }
    /// The epoll fd, for a pool slot that waits on it directly.
    pub fn raw_fd(&self) -> i32 {
        match &self.backend {
            Backend::Epoll { epoll, .. } => epoll.as_raw_fd(),
            Backend::Uring(ring) => ring.ring.as_raw_fd(),
        }
    }
    /// epoll: the wake eventfd, registered edge-triggered under tag `u64::MAX`.
    pub fn wake_fd(&self) -> i32 {
        match &self.backend {
            Backend::Epoll { wake, .. } => wake.as_raw_fd(),
            Backend::Uring(_) => unreachable!("a ring is woken by MSG_RING"),
        }
    }

    fn wake_eventfd(&self) {
        let fd = self.wake_fd();
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
    fn wake_ring(&self) {
        let _ = msg_ring_wake(self.raw_fd());
    }
}

fn tag(index: usize, generation: u32) -> u64 {
    index as u64 | (u64::from(generation) << 32)
}

/// Where the calling thread is in a unit pool: its unit, the worker slot it
/// registers on, and whether it owns that slot's poller now (false on a
/// thread that lost its slot to a spare during a blocking call).
#[derive(Clone, Copy, Debug)]
pub struct Here {
    pub unit: u32,
    pub slot: usize,
    pub owner: bool,
}

/// mc-units-light: one poller per worker slot of a unit pool. A unit's
/// registry adds each descriptor to the poller of the slot it registers on
/// (its home), tagged with the owning unit, and never polls it: the slot
/// polls and hands each edge to its unit, which dispatches it as one of its
/// own events wherever it runs. A shared tag is the unit in the top 16 bits,
/// the low 16 bits of the slot generation, then the slot index.
struct SlotPollers {
    pollers: Vec<Arc<Poller>>,
    here: fn() -> Option<Here>,
}
static SHARED: Mutex<Option<Arc<SlotPollers>>> = Mutex::new(None);
/// Bits of a shared tag below the unit.
pub const SHARED_UNIT_SHIFT: u32 = 48;
/// Make the process's slot pollers, one per worker slot; the first takes the
/// probe's ring. `here` names the calling unit and slot, or None outside one
/// (such a registry keeps a private poller).
pub fn slot_pollers(workers: usize, here: fn() -> Option<Here>) -> io::Result<Vec<Arc<Poller>>> {
    share(
        (0..workers)
            .map(|_| Poller::new())
            .collect::<io::Result<_>>()?,
        here,
    )
}
/// The interleaving checker models epoll calls, so its pool's slots use
/// epoll pollers whichever reactor the process chose.
pub fn slot_epoll_pollers(
    workers: usize,
    here: fn() -> Option<Here>,
) -> io::Result<Vec<Arc<Poller>>> {
    share(
        (0..workers)
            .map(|_| Poller::new_epoll())
            .collect::<io::Result<_>>()?,
        here,
    )
}
fn share(pollers: Vec<Arc<Poller>>, here: fn() -> Option<Here>) -> io::Result<Vec<Arc<Poller>>> {
    *SHARED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(SlotPollers {
        pollers: pollers.clone(),
        here,
    }));
    Ok(pollers)
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
    /// The pool slot whose poller holds its registration (0 when private).
    home: usize,
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
    /// mc-units-light: registered in the pool's slot pollers as this unit.
    shared: Option<Shared>,
}
struct Shared {
    unit: u32,
    slots: Arc<SlotPollers>,
}
impl Sockets {
    /// Lazily created once per run, at its first real socket operation.
    pub fn poller(&mut self) -> io::Result<Arc<Poller>> {
        if self.poller.is_none() {
            let slots = SHARED.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(slots) = slots
                && let Some(here) = (slots.here)()
            {
                self.poller = Some(slots.pollers[here.slot].clone());
                self.shared = Some(Shared {
                    unit: here.unit,
                    slots,
                });
            } else {
                self.poller = Some(Poller::new()?);
            }
        }
        Ok(self.poller.as_ref().unwrap().clone())
    }
    pub fn active_poller(&self) -> Option<Arc<Poller>> {
        self.poller.clone()
    }
    /// The poller holding registrations homed at `home`, and whether the
    /// calling thread owns it now. A private poller's holder always does.
    fn home(&self, home: usize) -> (&Arc<Poller>, bool) {
        match &self.shared {
            Some(shared) => (
                &shared.slots.pollers[home],
                (shared.slots.here)().is_some_and(|here| here.owner && here.slot == home),
            ),
            None => (self.poller.as_ref().unwrap(), true),
        }
    }
    /// Unregister a removed record from its home poller.
    fn unregister(&self, record: &Record, tag: u64, closing: bool) -> io::Result<()> {
        let (poller, owner) = self.home(record.home);
        poller.remove(record.fd.as_raw_fd(), tag, closing, owner)
    }
    fn shared_unit(&self) -> Option<u32> {
        self.shared.as_ref().map(|shared| shared.unit)
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
            home: 0,
        })?;
        Some(key | direction)
    }

    fn insert_record(&mut self, record: Record) -> Option<u64> {
        let index = if let Some(index) = self.free.pop() {
            index
        } else {
            // Each slot owns a descriptor, and descriptors stay below 2^31
            // (fs.nr_open), so an io_uring poll's parity bit is free.
            let index = u32::try_from(self.slots.len())
                .ok()
                .filter(|&index| u64::from(index) < ARM_PARITY)?;
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
            let tag = Self::registration_tag(self.shared_unit(), index, generation);
            self.unregister(record, tag, false)?;
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
    /// Both interests are persistent (an epoll edge-triggered interest, an
    /// io_uring multishot poll), so a registered reader that drained its
    /// socket learns of the next byte without trying its syscall again.
    pub fn mark_read_drained(&mut self, key: u64) {
        let r = self.record_mut(key);
        if r.registered && !r.read_terminal && !r.reset {
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
        // A pool registry registers on the calling thread's slot: its own,
        // or the one it lost to a spare during a blocking call, which it
        // posts to and wakes.
        let (home, owner) = match &self.shared {
            Some(shared) => (shared.slots.here)().map_or((0, false), |h| (h.slot, h.owner)),
            None => (0, true),
        };
        let tag = Self::registration_tag(self.shared_unit(), index, self.slots[index].generation);
        let (poller, _) = self.home(home);
        let poller = poller.clone();
        let record = self.slots[index].record.as_mut().unwrap();
        poller.add(&record.fd, tag, owner)?;
        record.home = home;
        record.registered = true;
        Ok(())
    }

    /// The ring and tag that zero-copy sends on `key` use: Some only for a
    /// socket of an io_uring poller that sends zero-copy, and only on the
    /// thread that owns its home poller, the one that may submit there (a
    /// send's completions are task work on its submitter). Registers the
    /// socket, so a send's answer readies its writer.
    pub fn send_target(&mut self, key: u64) -> io::Result<Option<(Arc<Poller>, u64)>> {
        self.register(key)?;
        let (poller, owner) = self.home(self.record(key).home);
        let poller = poller.clone();
        let Backend::Uring(ring) = &poller.backend else {
            return Ok(None);
        };
        if !ring.zc || !owner {
            return Ok(None);
        }
        let r = self.record_mut(key);
        let fd = r.fd.as_raw_fd();
        if !*r.zero_copy.get_or_insert_with(|| zero_copy_peer(fd)) {
            return Ok(None);
        }
        let index = ((key >> 2) as u32) as usize;
        let tag = Self::registration_tag(self.shared_unit(), index, self.slots[index].generation);
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
    /// Only the registry's holder dispatches completions, so readiness
    /// reported before the waiter is published still finds it.
    pub fn arm(&mut self, key: u64, write: bool, token: u64) -> io::Result<u64> {
        self.register(key)?;
        let index = ((key >> 2) as u32) as usize;
        let generation = self.slots[index].generation;
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
        let tag = Self::registration_tag(self.shared_unit(), index, self.slots[index].generation);
        let record = self.remove_record(index);
        if record.registered {
            // Unregister before dropping the sole descriptor owner.
            let _ = self.unregister(&record, tag, record.own_listener);
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
        for index in 0..self.slots.len() {
            let slot = &mut self.slots[index];
            let generation = slot.generation;
            if let Some(r) = slot.record.take() {
                if r.registered {
                    let tag = Self::registration_tag(self.shared_unit(), index, generation);
                    let _ = self.unregister(&r, tag, r.own_listener);
                }
                drop(r);
            }
        }
    }
}

#[cfg(test)]
mod tests;
