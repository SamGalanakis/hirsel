//! Combined directional registration, adapted from Go runtime/netpoll_epoll.go.
//! Slot generations replace pointer tokens; dispatch and release share one worker.
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub struct Poller {
    epoll: OwnedFd,
    wake: OwnedFd,
    polling: AtomicBool,
}

struct PollTurn<'a>(&'a AtomicBool);
impl Drop for PollTurn<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// One kernel turn, before socket identities are resolved by the registry.
struct PollEvents {
    events: [libc::epoll_event; 128],
    len: usize,
}
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
            polling: AtomicBool::new(false),
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
    fn try_turn(&self) -> Option<PollTurn<'_>> {
        self.polling
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| PollTurn(&self.polling))
    }

    /// Try the driver token. A busy token returns immediately, even if the
    /// token holder is waiting for I/O. A zero timeout is a nonblocking turn.
    fn poll(&self, timeout_ms: i32) -> io::Result<Option<PollEvents>> {
        let Some(_turn) = self.try_turn() else {
            return Ok(None);
        };
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 128];
        // SAFETY: epoll owns its descriptor and the fixed array is writable.
        let count = unsafe {
            libc::epoll_wait(
                self.epoll.as_raw_fd(),
                events.as_mut_ptr(),
                events.len() as i32,
                timeout_ms,
            )
        };
        if count < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            return Ok(None);
        }
        let len = checked(count)? as usize;
        if events[..len].iter().any(|event| event.u64 == u64::MAX) {
            self.drain();
        }
        Ok(Some(PollEvents { events, len }))
    }

    pub fn wake(&self) {
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
#[derive(Clone, Copy)]
struct Wait {
    token: u64,
    generation: u32,
}

fn tag(index: usize, generation: u32) -> u64 {
    index as u64 | (u64::from(generation) << 32)
}

struct Record {
    fd: OwnedFd,
    owners: u8,
    reset: bool,
    ended: bool,
    waiters: [Option<Wait>; 2],
    registered: bool,
}
struct Slot {
    generation: u32,
    // Registration generation is independent of the socket carrier's lifetime.
    // It lasts across slot reuse, and a new arm publishes a fresh epoll tag.
    armed_generation: u32,
    record: Option<Record>,
}
/// O(1) host storage per live descriptor. No table is sized from the fd limit.
#[derive(Default)]
pub struct Sockets {
    slots: Vec<Slot>,
    free: Vec<u32>,
    poller: Option<Arc<Poller>>,
    live: usize,
    model: Option<crate::model::Model>,
    model_keys: std::collections::BTreeSet<u64>,
}
impl Sockets {
    pub fn model(&self) -> Option<&crate::model::Model> {
        self.model.as_ref()
    }
    pub fn with_model(model: crate::model::Model) -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            poller: None,
            live: 0,
            model: Some(model),
            model_keys: Default::default(),
        }
    }
    pub fn adopt_model(&mut self, model: &crate::model::Model, key: u64) {
        self.model = Some(model.clone());
        self.model_keys.insert(key & !3);
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
                armed_generation: 0,
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
        self.record_mut(key).reset = true;
    }
    pub fn ended(&self, key: u64) -> bool {
        self.record(key).ended
    }
    pub fn mark_ended(&mut self, key: u64) {
        self.record_mut(key).ended = true;
    }
    pub fn split(&mut self, key: u64) -> (u64, u64) {
        if crate::model::Model::owns(key) {
            return self.model.as_ref().unwrap().split(key);
        }
        self.record_mut(key).owners = 6;
        (key | 1, key | 2)
    }
    fn update(
        &mut self,
        index: usize,
        generation: u32,
        waiters: [Option<Wait>; 2],
    ) -> io::Result<()> {
        let slot = &mut self.slots[index];
        let r = slot.record.as_mut().unwrap();
        let mut flags = 0;
        if waiters[0].is_some() {
            flags |= libc::EPOLLIN | libc::EPOLLRDHUP;
        }
        if waiters[1].is_some() {
            flags |= libc::EPOLLOUT;
        }
        if flags == 0 {
            if r.registered {
                self.poller
                    .as_ref()
                    .unwrap()
                    .ctl(libc::EPOLL_CTL_DEL, r.fd.as_raw_fd(), 0, 0)?;
            }
        } else {
            self.poller.as_ref().unwrap().ctl(
                if r.registered {
                    libc::EPOLL_CTL_MOD
                } else {
                    libc::EPOLL_CTL_ADD
                },
                r.fd.as_raw_fd(),
                flags as u32,
                tag(index, generation),
            )?;
        }
        r.registered = flags != 0;
        r.waiters = waiters;
        slot.armed_generation = generation;
        Ok(())
    }
    /// Returns the identity attached to the new epoll registration.
    pub fn arm(&mut self, key: u64, write: bool, token: u64) -> io::Result<u64> {
        self.poller()?;
        let index = ((key >> 2) as u32) as usize;
        let slot = &self.slots[index];
        // Only the driver's current epoll_wait batch can retain an old tag.
        // It cannot span a generation wrap. Reserve u64::MAX for eventfd.
        let generation = slot.armed_generation.wrapping_add(1) % u32::MAX;
        let wait = Wait { token, generation };
        let mut waiters = slot.record.as_ref().unwrap().waiters;
        waiters[usize::from(write)] = Some(wait);
        self.update(index, wait.generation, waiters)?;
        Ok(tag(index, wait.generation))
    }
    pub fn disarm(&mut self, key: u64, write: bool) -> io::Result<()> {
        let index = ((key >> 2) as u32) as usize;
        let slot = &self.slots[index];
        let mut waiters = slot.record.as_ref().unwrap().waiters;
        waiters[usize::from(write)] = None;
        self.update(index, slot.armed_generation, waiters)
    }
    /// Dispatch a host event only for the current registration.
    pub fn dispatch(
        &mut self,
        identity: u64,
        flags: u32,
        mut ready: impl FnMut(u64),
    ) -> io::Result<()> {
        let index = identity as u32 as usize;
        let Some(slot) = self.slots.get_mut(index) else {
            return Ok(());
        };
        let generation = (identity >> 32) as u32;
        if slot.armed_generation != generation {
            return Ok(());
        }
        let Some(r) = slot.record.as_ref() else {
            return Ok(());
        };
        let mut waiters = r.waiters;
        let failed = flags & (libc::EPOLLERR | libc::EPOLLHUP) as u32 != 0;
        for (direction, mask) in [libc::EPOLLIN | libc::EPOLLRDHUP, libc::EPOLLOUT]
            .into_iter()
            .enumerate()
        {
            if (failed || flags & mask as u32 != 0)
                && let Some(wait) = waiters[direction].take()
            {
                ready(wait.token);
            }
        }
        self.update(index, generation, waiters)
    }
    /// Poll only returned registrations. The token covers the kernel poll,
    /// while the caller has exclusive access to the socket registry.
    pub fn poll(&mut self, timeout_ms: i32, ready: impl FnMut(u64)) -> io::Result<usize> {
        let Some(poller) = self.poller.as_ref() else {
            return Ok(0);
        };
        let Some(events) = poller.poll(timeout_ms)? else {
            return Ok(0);
        };
        self.dispatch_polled(events, ready)
    }

    /// Apply a poll from this registry's driver. A re-arm between collection
    /// and dispatch invalidates the old kernel identity.
    fn dispatch_polled(
        &mut self,
        events: PollEvents,
        mut ready: impl FnMut(u64),
    ) -> io::Result<usize> {
        for event in &events.events[..events.len] {
            if event.u64 != u64::MAX {
                self.dispatch(event.u64, event.events, &mut ready)?;
            }
        }
        Ok(events.len)
    }

    pub fn release(&mut self, key: u64) {
        if crate::model::Model::owns(key) {
            let model = self.model.as_ref().unwrap();
            model.release(key);
            if !model.contains(key) {
                self.model_keys.remove(&(key & !3));
            }
            return;
        }
        let r = self.record_mut(key);
        let direction = key & 3;
        if direction == 2 {
            // SAFETY: the record owns the connected descriptor through final release.
            unsafe {
                libc::shutdown(r.fd.as_raw_fd(), libc::SHUT_WR);
            }
        }
        r.owners &= !(1 << direction);
        if r.owners == 0 {
            let fd = r.fd.as_raw_fd();
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
    }
    pub fn len(&self) -> usize {
        self.live + self.model_keys.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
impl Drop for Sockets {
    fn drop(&mut self) {
        if let Some(model) = &self.model {
            for key in std::mem::take(&mut self.model_keys) {
                model.release_all(key);
            }
        }
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
            scope.spawn(|| driver.poll(0).unwrap().is_none()).join().unwrap()
        });
        drop(held);
        assert!(busy);
        let events = driver.poll(0).unwrap().unwrap();
        let mut tokens = Vec::new();
        sockets.dispatch_polled(events, |token| tokens.push(token)).unwrap();
        assert_eq!(tokens, [17]);
    }
}
