use super::*;
use crate::ready::Ready;

impl Sockets {
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
    pub fn dispatch(&mut self, identity: u64, flags: Ready, ready: impl FnMut(u64)) {
        Self::dispatch_edge(&mut self.slots, &mut self.waiting, identity, flags, ready);
    }

    fn dispatch_edge(
        slots: &mut [Slot],
        waiting: &mut usize,
        identity: u64,
        flags: Ready,
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
        let failed = flags.is_failed();
        let terminal = failed || flags.is_read_closed();
        if terminal || flags.is_readable() {
            r.read_drained = false;
            r.read_terminal |= terminal;
            if let Some(token) = r.waiters[0].take() {
                *waiting -= 1;
                ready(token);
            }
        }
        if (failed || flags.is_writable())
            && let Some(token) = r.waiters[1].take()
        {
            *waiting -= 1;
            ready(token);
        }
    }
    /// Poll only returned registrations while holding the driver token
    /// and exclusive access to the socket registry.
    pub fn poll(&mut self, timeout_ms: i32, ready: impl FnMut(u64)) -> io::Result<usize> {
        if self.shared.is_some() {
            return Ok(0);
        }
        let Some(poller) = self.poller.as_ref() else {
            return Ok(0);
        };
        if timeout_ms == 0 && self.waiting == 0 && !poller.has_udp_groups() {
            return Ok(0);
        }
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
            if event.token() != WAKE {
                Self::dispatch_edge(slots, waiting, event.token(), event.ready(), &mut ready);
            }
        }
        events.0.len()
    }
}
