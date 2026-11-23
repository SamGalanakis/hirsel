//! Persistent edge-triggered interests, adapted from Go runtime/netpoll_epoll.go.
//! Slot generations identify sockets across poll and dispatch.
//!
//! Two kernel interfaces carry the same interests: epoll, and io_uring
//! multishot polls (edge-triggered unless `IORING_POLL_ADD_LEVEL`, as epoll
//! with `EPOLLET`). One is chosen once per process ([`reactor`]); both turn a
//! kernel turn into the same `(tag, flags)` events, so dispatch is shared.
use std::{
    collections::HashMap,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{Arc, Mutex, MutexGuard, OnceLock, TryLockError},
};

const POLL_BATCH: i32 = 128;
/// The wake eventfd's tag.
const WAKE: u64 = u64::MAX;
/// io_uring: the completion of a poll removal, which carries no readiness.
const REMOVED: u64 = u64::MAX - 1;
const INTEREST: u32 = (libc::EPOLLIN | libc::EPOLLOUT | libc::EPOLLRDHUP) as u32;

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

/// `auto` (the default) uses io_uring when this kernel allows it; `epoll`
/// forces epoll.
pub const REACTOR_ENV: &str = "HIRSEL_NET_REACTOR";
static CHOSEN: OnceLock<Result<Reactor, String>> = OnceLock::new();
/// The ring and wake descriptor the probe built, for the first poller.
static PROBED: Mutex<Option<(Uring, OwnedFd)>> = Mutex::new(None);

/// The process's reactor, chosen at its first use and fixed after.
pub fn reactor() -> io::Result<Reactor> {
    CHOSEN
        .get_or_init(|| match std::env::var_os(REACTOR_ENV) {
            Some(v) if v == "epoll" => Ok(Reactor::Epoll),
            Some(v) if v != "auto" => Err(format!(
                "{REACTOR_ENV}={} is not a reactor; use auto or epoll",
                v.to_string_lossy()
            )),
            _ => Ok(match Uring::open() {
                Ok(probed) => {
                    *PROBED.lock().unwrap_or_else(|e| e.into_inner()) = Some(probed);
                    Reactor::IoUring
                }
                Err(_) => Reactor::Epoll,
            }),
        })
        .clone()
        .map_err(|message| {
            eprintln!("{message}");
            io::Error::new(io::ErrorKind::InvalidInput, message)
        })
}

pub struct Poller {
    backend: Backend,
    wake: OwnedFd,
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
            Reactor::IoUring => {
                let probed = PROBED.lock().unwrap_or_else(|e| e.into_inner()).take();
                let (ring, wake) = match probed {
                    Some(probed) => probed,
                    None => Uring::open()?,
                };
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
        }
    }
    fn new_epoll() -> io::Result<Arc<Self>> {
        // SAFETY: creates a new, uniquely owned CLOEXEC descriptor.
        let epoll =
            unsafe { OwnedFd::from_raw_fd(checked(libc::epoll_create1(libc::EPOLL_CLOEXEC))?) };
        let poller = Arc::new(Self {
            backend: Backend::Epoll(epoll),
            wake: eventfd()?,
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
    /// Watch `fd` for every direction, edge-triggered, under `tag`.
    fn add(&self, fd: i32, tag: u64) -> io::Result<()> {
        match &self.backend {
            Backend::Epoll(_) => self.epoll_ctl(
                libc::EPOLL_CTL_ADD,
                fd,
                INTEREST | libc::EPOLLET as u32,
                tag,
            ),
            Backend::Uring(ring) => ring.add(fd, tag),
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
                ring.remove(tag)
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
        let one = 1u64;
        loop {
            // SAFETY: nonblocking eventfd, readable eight-byte counter.
            if unsafe { libc::write(self.wake.as_raw_fd(), (&one as *const u64).cast(), 8) } >= 0 {
                return;
            }
            if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                return;
            }
        }
    }
    fn drain(&self) {
        drain(self.wake.as_raw_fd());
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

/// An io_uring ring holding one multishot poll per watched descriptor.
/// Setup flags are 0: the driver token migrates between workers, which rules
/// out `SINGLE_ISSUER` and with it `DEFER_TASKRUN`.
struct Uring {
    ring: io_uring::IoUring,
    /// Serializes every submission-queue access; completions are read only
    /// under the driver turn.
    sq: Mutex<()>,
    /// Live polls by tag, for re-arming one the kernel ended. Lock before `sq`.
    armed: Mutex<HashMap<u64, i32>>,
}
impl Uring {
    const ENTRIES: u32 = 64;
    const COMPLETIONS: u32 = 1024;

    /// A ring whose kernel runs multishot polls and bounded waits, with its
    /// wake eventfd already watched. Any refusal (an old kernel, a seccomp
    /// filter, `io_uring_disabled`) is an error, and the caller uses epoll.
    fn open() -> io::Result<(Uring, OwnedFd)> {
        use io_uring::{cqueue, opcode, register::Probe};
        let unsupported = || io::Error::from(io::ErrorKind::Unsupported);
        let ring = io_uring::IoUring::builder()
            .setup_cqsize(Self::COMPLETIONS)
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
        let uring = Uring {
            ring,
            sq: Mutex::new(()),
            armed: Mutex::new(HashMap::new()),
        };
        let wake = eventfd()?;
        uring.add(wake.as_raw_fd(), WAKE)?;
        // A multishot poll must report a wake and stay armed.
        let one = 1u64;
        // SAFETY: nonblocking eventfd, readable eight-byte counter.
        if unsafe { libc::write(wake.as_raw_fd(), (&one as *const u64).cast(), 8) } != 8 {
            return Err(io::Error::last_os_error());
        }
        uring.ring.submitter().submit_and_wait(1)?;
        // SAFETY: no poller exists yet; this is the only completion reader.
        let cqe = unsafe { uring.ring.completion_shared() }.next();
        drain(wake.as_raw_fd());
        match cqe {
            Some(cqe)
                if cqe.user_data() == WAKE
                    && cqe.result() > 0
                    && cqe.result() as u32 & libc::EPOLLIN as u32 != 0
                    && cqueue::more(cqe.flags()) =>
            {
                Ok((uring, wake))
            }
            _ => Err(unsupported()),
        }
    }

    fn push(&self, entry: io_uring::squeue::Entry) -> io::Result<()> {
        let _sq = self.sq.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            // SAFETY: `sq` serializes submission-queue access.
            let pushed = unsafe { self.ring.submission_shared().push(&entry) }.is_ok();
            match self.ring.submitter().submit() {
                Ok(_) => {}
                // Entries left in the queue go in with the next enter.
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(libc::EBUSY | libc::EAGAIN | libc::EINTR)
                    ) => {}
                Err(e) => return Err(e),
            }
            if pushed {
                return Ok(());
            }
        }
    }

    /// The wake eventfd is always writable; only its counter matters.
    fn interest(tag: u64) -> u32 {
        if tag == WAKE {
            libc::EPOLLIN as u32
        } else {
            INTEREST
        }
    }

    fn arm(&self, fd: i32, tag: u64) -> io::Result<()> {
        use io_uring::{opcode, types};
        self.push(
            opcode::PollAdd::new(types::Fd(fd), Self::interest(tag))
                .multi(true)
                .build()
                .user_data(tag),
        )
    }

    fn add(&self, fd: i32, tag: u64) -> io::Result<()> {
        let mut armed = self.armed.lock().unwrap_or_else(|e| e.into_inner());
        self.arm(fd, tag)?;
        armed.insert(tag, fd);
        Ok(())
    }

    fn remove(&self, tag: u64) -> io::Result<()> {
        let mut armed = self.armed.lock().unwrap_or_else(|e| e.into_inner());
        if armed.remove(&tag).is_some() {
            self.push(
                io_uring::opcode::PollRemove::new(tag)
                    .build()
                    .user_data(REMOVED),
            )?;
        }
        Ok(())
    }

    /// One kernel turn into `out`, under the driver turn. A timeout of 0 does
    /// not block and -1 blocks until a completion.
    fn wait(&self, out: &mut [libc::epoll_event], timeout_ms: i32) -> usize {
        let n = self.reap(out);
        if n != 0 {
            return n;
        }
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
        self.reap(out)
    }

    /// Translate posted completions into `(tag, flags)` events.
    fn reap(&self, out: &mut [libc::epoll_event]) -> usize {
        // SAFETY: the caller holds the driver turn, the only completion reader.
        let mut cq = unsafe { self.ring.completion_shared() };
        cq.sync();
        let mut n = 0;
        while n < out.len() {
            let Some(cqe) = cq.next() else { break };
            let tag = cqe.user_data();
            if tag == REMOVED {
                continue;
            }
            let res = cqe.result();
            let mut flags = if res > 0 { res as u32 } else { 0 };
            if !io_uring::cqueue::more(cqe.flags()) {
                // The kernel ended this poll (a cancel when its submitting
                // thread exited, or an overflowing completion queue). Re-arm
                // a live one and report it ready, so its waiters retry.
                let armed = self.armed.lock().unwrap_or_else(|e| e.into_inner());
                let Some(&fd) = armed.get(&tag) else { continue };
                if self.arm(fd, tag).is_err() {
                    flags |= libc::EPOLLERR as u32;
                }
                flags |= (libc::EPOLLIN | libc::EPOLLOUT) as u32;
            }
            if flags != 0 {
                out[n] = libc::epoll_event {
                    events: flags,
                    u64: tag,
                };
                n += 1;
            }
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
    (u64::from(unit) << SHARED_UNIT_SHIFT) | (u64::from(generation & 0xffff) << 32) | index as u64
}

struct Record {
    fd: OwnedFd,
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
            fd,
            pipe,
            owners: 1 << direction,
            reset: false,
            ended: false,
            waiters: [None; 2],
            registered: false,
            read_drained: false,
            read_terminal: false,
            own_listener: false,
        })?;
        Some(key | direction)
    }

    fn insert_record(&mut self, record: Record) -> Option<u64> {
        let index = if let Some(index) = self.free.pop() {
            index
        } else {
            let index = u32::try_from(self.slots.len()).ok()?;
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
        let slot = &mut self.slots[index];
        let record = slot.record.as_mut().unwrap();
        self.poller.as_ref().unwrap().add(
            record.fd.as_raw_fd(),
            Self::registration_tag(self.shared, index, slot.generation),
        )?;
        record.registered = true;
        Ok(())
    }

    /// Register a socket once and publish this direction's current waiter.
    pub fn arm(&mut self, key: u64, write: bool, token: u64) -> io::Result<u64> {
        self.register(key)?;
        let index = ((key >> 2) as u32) as usize;
        let slot = &mut self.slots[index];
        let r = slot.record.as_mut().unwrap();
        if r.waiters[usize::from(write)].replace(token).is_none() {
            self.waiting += 1;
        }
        Ok(tag(index, slot.generation))
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
}
