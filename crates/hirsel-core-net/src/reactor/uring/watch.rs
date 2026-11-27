use super::{COMPLETION_READY, FILE_READY, INTEREST, Uring, WAKE};
use crate::ready::{Event, Ready};
use io_uring::{cqueue, opcode, squeue, types};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io,
    os::fd::{AsRawFd, OwnedFd},
    sync::{Arc, MutexGuard},
};

pub(super) const REMOVE: u64 = 1 << 62;

pub(in crate::reactor) struct Watch {
    pub(in crate::reactor) fd: Arc<OwnedFd>,
    interest: u32,
    id: u64,
}

enum Work {
    Remove(u64),
    Replace(u64, u64),
}

#[derive(Default)]
pub(in crate::reactor) struct Watches {
    live: HashMap<u64, Watch>,
    ids: HashMap<u64, u64>,
    retired: HashMap<u64, HashSet<u64>>,
    work: VecDeque<Work>,
    rechecks: VecDeque<u64>,
    wake: bool,
    last_dropped: u32,
    next: u64,
    #[cfg(test)]
    pub(in crate::reactor) retried: usize,
}

impl Watches {
    pub(in crate::reactor) fn get(&self, tag: &u64) -> Option<&Watch> {
        self.live.get(tag)
    }

    fn id(&mut self, tag: u64) -> io::Result<u64> {
        let id = self.next + 1;
        if id == REMOVE {
            return Err(io::Error::from_raw_os_error(libc::ENOSPC));
        }
        self.next = id;
        self.ids.insert(id, tag);
        Ok(id)
    }

    fn is_current(&self, tag: u64, id: u64) -> bool {
        self.live.get(&tag).is_some_and(|watch| watch.id == id)
    }

    fn forget(&mut self, id: u64) {
        if let Some(tag) = self.ids.remove(&id)
            && let Some(retired) = self.retired.get_mut(&tag)
        {
            retired.remove(&id);
            if retired.is_empty() {
                self.retired.remove(&tag);
            }
        }
    }

    fn retire(&mut self, tag: u64, id: u64) {
        self.retired.entry(tag).or_default().insert(id);
    }

    pub(super) fn removed(&mut self, id: u64, result: i32) -> io::Result<()> {
        let Some(&tag) = self.ids.get(&id) else {
            return Ok(());
        };
        if self.is_current(tag, id) {
            return Ok(());
        }
        if result == 0 || result == -libc::ENOENT {
            self.forget(id);
        } else if result == -libc::EALREADY {
            // The request's task work can post F_MORE and keep the poll live.
            self.work.push_back(Work::Remove(id));
            #[cfg(test)]
            {
                self.retried += 1;
            }
        } else {
            return Err(io::Error::from_raw_os_error(-result));
        }
        Ok(())
    }

    pub(super) fn polled(&mut self, ring: &Uring, cqe: cqueue::Entry) -> io::Result<Option<u64>> {
        let id = cqe.user_data();
        let Some(&tag) = self.ids.get(&id) else {
            return Ok(None);
        };
        if tag == FILE_READY && cqe.result() > 0 && self.is_current(tag, id) {
            super::super::sys::reset(&self.live[&tag].fd);
        }
        if !cqueue::more(cqe.flags()) {
            if self.is_current(tag, id) {
                let next = self.id(tag)?;
                let watch = self.live.get_mut(&tag).unwrap();
                watch.id = next;
                ring.arm(watch);
            }
            self.forget(id);
        }
        Ok(Some(tag))
    }

    pub(super) fn due(&self) -> bool {
        self.wake || !self.work.is_empty() || !self.rechecks.is_empty()
    }

    pub(super) fn recheck(&mut self, out: &mut [Event]) -> usize {
        let mut n = 0;
        if let Some(event) = out.first_mut()
            && std::mem::take(&mut self.wake)
        {
            *event = Event::new(WAKE, Ready::READABLE);
            n = 1;
        }
        for _ in n..out.len() {
            let Some(tag) = self.rechecks.pop_front() else {
                break;
            };
            if !matches!(tag, COMPLETION_READY | FILE_READY) && self.live.contains_key(&tag) {
                out[n] = Event::new(tag, Ready::READABLE | Ready::WRITABLE);
                n += 1;
            }
        }
        n
    }

    pub(super) fn recover_if_dropped(&mut self, dropped: u32) {
        if self.last_dropped != dropped {
            self.recover(dropped);
        }
    }

    #[cold]
    fn recover(&mut self, dropped: u32) {
        self.last_dropped = dropped;
        self.wake = true;
        self.work.clear();
        self.work
            .extend(self.retired.values().flatten().copied().map(Work::Remove));
        self.work.extend(
            self.live
                .iter()
                .map(|(&tag, watch)| Work::Replace(tag, watch.id)),
        );
        self.rechecks.clear();
        self.rechecks.extend(self.live.keys().copied());
    }

    pub(in crate::reactor) fn submit_recovery(&mut self, ring: &Uring) -> io::Result<bool> {
        let mut left = ring.plan.sq_entries;
        while let Some(work) = self.work.front() {
            if left == 0 || left == 1 && matches!(work, Work::Replace(..)) {
                break;
            }
            match self.work.pop_front().unwrap() {
                Work::Remove(id) => {
                    if self.ids.contains_key(&id) {
                        ring.push(Uring::poll_remove(id), None);
                        left -= 1;
                    }
                }
                Work::Replace(tag, old) => {
                    if self.is_current(tag, old) {
                        let id = self.id(tag)?;
                        self.retire(tag, old);
                        let watch = self.live.get_mut(&tag).unwrap();
                        watch.id = id;
                        ring.push(Uring::poll_remove(old), None);
                        ring.arm(watch);
                        left -= 2;
                    }
                }
            }
        }
        Ok(self.due())
    }
}

impl Uring {
    pub(in crate::reactor) fn watches(&self) -> MutexGuard<'_, Watches> {
        self.watched.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn arm(&self, watch: &Watch) {
        let entry = opcode::PollAdd::new(types::Fd(watch.fd.as_raw_fd()), watch.interest)
            .multi(true)
            .build()
            .user_data(watch.id);
        self.push(entry, Some(watch.fd.clone()));
    }

    fn poll_remove(id: u64) -> squeue::Entry {
        opcode::PollRemove::new(id).build().user_data(id | REMOVE)
    }

    /// Arm one watch under a new id. The kernel checks readiness as it arms.
    pub(in crate::reactor) fn add(&self, tag: u64, fd: Arc<OwnedFd>) -> io::Result<()> {
        let mut watches = self.watches();
        let id = watches.id(tag)?;
        let interest = if matches!(tag, COMPLETION_READY | FILE_READY) {
            Ready::READABLE.poll_mask()
        } else {
            INTEREST
        };
        let watch = Watch { fd, interest, id };
        self.arm(&watch);
        let previous = watches.live.insert(tag, watch);
        debug_assert!(previous.is_none());
        Ok(())
    }

    /// The pending SQEs and the kernel own any file references they still need.
    pub(in crate::reactor) fn remove(&self, tag: u64) -> bool {
        let mut watches = self.watches();
        if let Some(watch) = watches.live.remove(&tag) {
            watches.retire(tag, watch.id);
        }
        let Some(retired) = watches.retired.get(&tag) else {
            return false;
        };
        for &id in retired {
            self.push(Self::poll_remove(id), None);
        }
        true
    }

    pub(in crate::reactor) fn recover_watches(&self) {
        let dropped = self.ring.dropped();
        self.watches().recover(dropped);
    }
}
