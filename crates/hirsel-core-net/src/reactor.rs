//! Persistent edge-triggered interests, adapted from Go runtime/netpoll_epoll.go.
//! Slot generations identify sockets across poll and dispatch.
use crate::model::Model;
use std::{
    collections::BTreeSet,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{Arc, Mutex, MutexGuard, TryLockError},
};

const POLL_BATCH: i32 = 128;

pub struct Poller {
    epoll: OwnedFd,
    wake: OwnedFd,
    events: Mutex<Vec<libc::epoll_event>>,
    operations: &'static PollOperations,
}

struct PollOperations {
    poll: for<'a> fn(&'a Poller, i32) -> io::Result<Option<PollEvents<'a>>>,
    wake: fn(&Poller),
}

/// The initialized prefix of one kernel turn, held until registry dispatch.
pub struct PollEvents<'a>(MutexGuard<'a, Vec<libc::epoll_event>>);
fn checked(rc: i32) -> io::Result<i32> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}
impl Poller {
    fn new() -> io::Result<Arc<Self>> {
        // SAFETY: these calls create new, uniquely owned CLOEXEC descriptors.
        let epoll =
            unsafe { OwnedFd::from_raw_fd(checked(libc::epoll_create1(libc::EPOLL_CLOEXEC))?) };
        let wake = unsafe {
            OwnedFd::from_raw_fd(checked(libc::eventfd(
                0,
                libc::EFD_CLOEXEC | libc::EFD_NONBLOCK,
            ))?)
        };
        let poller = Arc::new(Self {
            epoll,
            wake,
            events: Mutex::new(Vec::with_capacity(POLL_BATCH as usize)),
            operations: &PollOperations {
                poll: Self::poll_epoll,
                wake: Self::wake_epoll,
            },
        });
        poller.ctl(
            libc::EPOLL_CTL_ADD,
            poller.wake.as_raw_fd(),
            libc::EPOLLIN as u32,
            u64::MAX,
        )?;
        Ok(poller)
    }
    fn ctl(&self, op: i32, fd: i32, flags: u32, token: u64) -> io::Result<()> {
        let mut event = libc::epoll_event {
            events: flags,
            u64: token,
        };
        // SAFETY: live descriptors and writable event; DEL ignores the event.
        checked(unsafe { libc::epoll_ctl(self.epoll.as_raw_fd(), op, fd, &mut event) })?;
        Ok(())
    }
    fn try_turn(&self) -> Option<MutexGuard<'_, Vec<libc::epoll_event>>> {
        match self.events.try_lock() {
            Ok(events) => Some(events),
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Poisoned(events)) => Some(events.into_inner()),
        }
    }

    /// Try the driver token. A busy token returns immediately while another
    /// worker polls or dispatches. A zero timeout is a nonblocking turn.
    pub fn poll(&self, timeout_ms: i32) -> io::Result<Option<PollEvents<'_>>> {
        (self.operations.poll)(self, timeout_ms)
    }

    // The optional operations are installed with the native poller, so a
    // scheduler that can never construct one does not link this syscall path.
    fn poll_epoll(&self, timeout_ms: i32) -> io::Result<Option<PollEvents<'_>>> {
        let Some(mut events) = self.try_turn() else {
            return Ok(None);
        };
        events.clear();
        crate::EPOLL_WAITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: epoll owns its descriptor and events has POLL_BATCH spare slots.
        let count = unsafe {
            libc::epoll_wait(
                self.epoll.as_raw_fd(),
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
        if events.iter().any(|event| event.u64 == u64::MAX) {
            self.drain();
        }
        Ok(Some(PollEvents(events)))
    }

    pub fn wake(&self) {
        (self.operations.wake)(self);
    }
    /// mc-units-untied: the epoll fd, for the shared poller.
    pub fn raw_fd(&self) -> i32 {
        self.epoll.as_raw_fd()
    }

    fn wake_epoll(&self) {
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
        let mut value = 0u64;
        // SAFETY: nonblocking eventfd, writable eight-byte counter.
        while unsafe { libc::read(self.wake.as_raw_fd(), (&mut value as *mut u64).cast(), 8) } < 0 {
            if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                break;
            }
        }
    }
}
fn tag(index: usize, generation: u32) -> u64 {
    index as u64 | (u64::from(generation) << 32)
}

struct Record {
    fd: OwnedFd,
    owners: u8,
    reset: bool,
    ended: bool,
    waiters: [Option<u64>; 2],
    registered: bool,
    read_drained: bool,
    read_terminal: bool,
}
struct Slot {
    generation: u32,
    record: Option<Record>,
}
/// The model owns its socket keys and cleanup. Native registries hold no
/// reference to the simulation's resource implementation.
trait ModelResources: Send + Sync {
    fn model(&self) -> &Model;
    fn adopt(&mut self, model: &Model, key: u64);
    fn release(&mut self, key: u64);
    fn len(&self) -> usize;
}

struct ModeledSockets {
    model: Model,
    keys: BTreeSet<u64>,
}
impl ModeledSockets {
    fn new(model: Model) -> Self {
        Self {
            model,
            keys: BTreeSet::new(),
        }
    }
}
impl ModelResources for ModeledSockets {
    fn model(&self) -> &Model {
        &self.model
    }
    fn adopt(&mut self, model: &Model, key: u64) {
        self.model = model.clone();
        self.keys.insert(key & !3);
    }
    fn release(&mut self, key: u64) {
        self.model.release(key);
        if !self.model.contains(key) {
            self.keys.remove(&(key & !3));
        }
    }
    fn len(&self) -> usize {
        self.keys.len()
    }
}
impl Drop for ModeledSockets {
    fn drop(&mut self) {
        for key in std::mem::take(&mut self.keys) {
            self.model.release_all(key);
        }
    }
}

/// mc-units-proto: an in-process crossing endpoint attached to a socket slot.
/// The registry tells it when the program releases its write half or the
/// whole endpoint, so the peer unit observes end of stream.
pub trait Crossing: Send + Sync {
    fn shutdown_write(&self);
    fn close(&self);
    fn as_any(&self) -> &dyn std::any::Any;
}

/// O(1) host storage per live descriptor. No table is sized from the fd limit.
#[derive(Default)]
pub struct Sockets {
    slots: Vec<Slot>,
    free: Vec<u32>,
    poller: Option<Arc<Poller>>,
    live: usize,
    waiting: usize,
    model: Option<Box<dyn ModelResources>>,
    /// mc-units-proto: crossing endpoints by slot index.
    pub crossings: std::collections::HashMap<u32, Arc<dyn Crossing>>,
    /// mc-units-proto: this registry's unit, set by its first crossing.
    pub unit: Option<u32>,
}
impl Sockets {
    pub fn model(&self) -> Option<&Model> {
        self.model.as_ref().map(|resources| resources.model())
    }
    pub fn with_model(model: Model) -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            poller: None,
            live: 0,
            waiting: 0,
            model: Some(Box::new(ModeledSockets::new(model))),
            crossings: Default::default(),
            unit: None,
        }
    }
    /// mc-units-proto: the crossing attached to a live key, if any.
    pub fn crossing(&self, key: u64) -> Option<&Arc<dyn Crossing>> {
        if self.crossings.is_empty() || Model::owns(key) {
            return None;
        }
        self.crossings.get(&((key >> 2) as u32))
    }
    pub fn attach_crossing(&mut self, key: u64, crossing: Arc<dyn Crossing>) {
        self.crossings.insert((key >> 2) as u32, crossing);
    }
    pub fn adopt_model(&mut self, model: &Model, key: u64) {
        self.model
            .get_or_insert_with(|| Box::new(ModeledSockets::new(model.clone())))
            .adopt(model, key);
    }
    /// Lazily created once per run, at its first real socket operation.
    pub fn poller(&mut self) -> io::Result<Arc<Poller>> {
        if self.poller.is_none() {
            self.poller = Some(Poller::new()?);
        }
        Ok(self.poller.as_ref().unwrap().clone())
    }
    /// Transfer a uniquely owned descriptor; harness descriptors need no poller until parked.
    pub fn insert(&mut self, fd: OwnedFd) -> Option<u64> {
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
        slot.record = Some(Record {
            fd,
            owners: 1,
            reset: false,
            ended: false,
            waiters: [None; 2],
            registered: false,
            read_drained: false,
            read_terminal: false,
        });
        self.live += 1;
        Some((u64::from(slot.generation) << 34) | (u64::from(index) << 2))
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
    pub fn split(&mut self, key: u64) -> (u64, u64) {
        if Model::owns(key) {
            return self.model().unwrap().split(key);
        }
        self.record_mut(key).owners = 6;
        (key | 1, key | 2)
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
    /// Register a socket once and publish this direction's current waiter.
    pub fn arm(&mut self, key: u64, write: bool, token: u64) -> io::Result<u64> {
        let index = ((key >> 2) as u32) as usize;
        if !self.slots[index].record.as_ref().unwrap().registered {
            self.poller()?;
            let slot = &mut self.slots[index];
            let r = slot.record.as_mut().unwrap();
            self.poller.as_ref().unwrap().ctl(
                libc::EPOLL_CTL_ADD,
                r.fd.as_raw_fd(),
                (libc::EPOLLIN | libc::EPOLLOUT | libc::EPOLLRDHUP | libc::EPOLLET) as u32,
                tag(index, slot.generation),
            )?;
            r.registered = true;
        }
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
        if slot.generation != (identity >> 32) as u32 {
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
        if timeout_ms == 0 && self.waiting == 0 {
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
            if event.u64 != u64::MAX {
                Self::dispatch_edge(slots, waiting, event.u64, event.events, &mut ready);
            }
        }
        events.0.len()
    }

    pub fn release(&mut self, key: u64) -> bool {
        if Model::owns(key) {
            self.model.as_mut().unwrap().release(key);
            return !self.model().unwrap().contains(key);
        }
        let index = (key >> 2) as u32;
        let crossing = self.crossings.get(&index).cloned();
        let r = self.record_mut(key);
        let direction = key & 3;
        if let Some(c) = &crossing
            && direction == 2
        {
            c.shutdown_write();
        }
        if direction == 2 && crossing.is_none() {
            // SAFETY: the record owns the connected descriptor through final release.
            unsafe {
                libc::shutdown(r.fd.as_raw_fd(), libc::SHUT_WR);
            }
        }
        r.owners &= !(1 << direction);
        let last = r.owners == 0;
        let fd = r.fd.as_raw_fd();
        let waiting = r.waiters.iter().flatten().count();
        if last && let Some(c) = self.crossings.remove(&index) {
            c.close();
        }
        if last {
            self.waiting -= waiting;
            let index = ((key >> 2) as u32) as usize;
            let slot = &mut self.slots[index];
            if slot.record.as_ref().unwrap().registered {
                // Unregister before dropping the sole descriptor owner.
                let _ = self
                    .poller
                    .as_ref()
                    .unwrap()
                    .ctl(libc::EPOLL_CTL_DEL, fd, 0, 0);
            }
            slot.record = None;
            self.live -= 1;
            // The two carrier direction bits leave thirty generation bits. Retire on exhaustion.
            if slot.generation < (1 << 30) - 1 {
                slot.generation += 1;
                self.free.push(index as u32);
            }
        }
        last
    }
    pub fn len(&self) -> usize {
        self.live + self.model.as_ref().map_or(0, |resources| resources.len())
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
impl Drop for Sockets {
    fn drop(&mut self) {
        drop(self.model.take());
        for slot in &mut self.slots {
            if let Some(r) = slot.record.take() {
                if r.registered {
                    let _ = self.poller.as_ref().unwrap().ctl(
                        libc::EPOLL_CTL_DEL,
                        r.fd.as_raw_fd(),
                        0,
                        0,
                    );
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
}
