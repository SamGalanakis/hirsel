use super::Poller;
use crate::udp::{
    Captured, MemberProducer, MemberReader, Options, ReceivePool, Socket, Steer, TurnBudget,
    WriterList, measure, member_queue,
};
use std::{
    collections::{HashMap, VecDeque},
    io,
    num::{NonZeroU16, NonZeroU64},
    os::fd::{AsFd, AsRawFd},
    sync::{Arc, Weak},
};

// Unit 0xffff is reserved for poller-owned registrations.
const FIRST: u64 = 0xffff_0000_0000_0000;
const LAST: u64 = super::uring::COMPLETION_READY;
const READ: u32 = libc::EPOLLIN as u32;
const WRITE: u32 = libc::EPOLLOUT as u32;
const FAILED: u32 = (libc::EPOLLERR | libc::EPOLLHUP) as u32;

struct Group {
    socket: Socket,
    poller: Weak<Poller>,
    tag: u64,
    writers: WriterList,
    ready: Box<dyn Fn(u64) + Send + Sync>,
    measure: Option<measure::Group>,
}

pub struct UdpMember {
    group: Option<Arc<Group>>,
    index: usize,
    reader: Option<MemberReader>,
}

impl UdpMember {
    fn group(&self) -> &Group {
        self.group.as_deref().unwrap()
    }

    pub fn reader(&mut self) -> &mut MemberReader {
        self.reader.as_mut().unwrap()
    }

    pub fn socket(&self) -> &Socket {
        &self.group().socket
    }

    pub fn measure_delivery(&self, poller: &Arc<Poller>, captured_at: u64) {
        let group = self.group();
        if let Some(measure) = &group.measure {
            measure.delivered(
                self.index,
                Weak::as_ptr(&group.poller) == Arc::as_ptr(poller),
                captured_at,
            );
        }
    }

    pub fn publish_writer(&self, token: NonZeroU64) {
        self.group().writers.publish(self.index, token);
    }

    pub fn cancel_writer(&self) {
        self.group().writers.cancel(self.index);
    }
}

impl Drop for UdpMember {
    fn drop(&mut self) {
        self.cancel_writer();
        let mut reader = self.reader.take().unwrap();
        reader.cancel_waiter();
        drop(reader);
        let group = self.group.take().unwrap();
        let poller = group.poller.upgrade();
        let tag = group.tag;
        drop(group);
        if let Some(poller) = poller {
            poller.retire_member(tag, self.index);
        }
    }
}

type Retire = Box<dyn FnMut(usize) + Send>;

#[derive(Default)]
pub(super) struct Deferred {
    readers: VecDeque<Box<dyn FnOnce() + Send>>,
    group_first: bool,
}

struct Intake {
    group: Arc<Group>,
    steer: Steer,
    members: NonZeroU16,
    live: usize,
    queues: Vec<Option<MemberProducer>>,
    pool: ReceivePool,
    reading: bool,
    writer_edge: bool,
    writer_cursor: u32,
    queued: bool,
    retire: Retire,
}

impl Intake {
    fn dispatch(&mut self, budget: &mut TurnBudget, writer_visits: &mut usize) {
        while self.reading && budget.has_datagrams() {
            if self.pool.front().is_none() {
                match self.pool.read(&self.group.socket, None, budget) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => break,
                    Err(_) => {
                        self.reading = false;
                        break;
                    }
                }
            }
            if !Self::deliver(
                &self.group,
                self.steer,
                self.members,
                &mut self.queues,
                self.pool.front().unwrap(),
                budget,
            ) {
                break;
            }
            self.pool.advance();
        }
        let writers = &self.group.writers;
        let mut batch = if self.writer_cursor != 0 {
            writers.resume(self.writer_cursor)
        } else if std::mem::take(&mut self.writer_edge) {
            writers.take()
        } else {
            return;
        };
        while *writer_visits != 0 {
            let Some(token) = batch.next() else { break };
            *writer_visits -= 1;
            if let Some(token) = token {
                (self.group.ready)(token.get());
            }
        }
        self.writer_cursor = batch.cursor();
    }

    fn deliver(
        group: &Group,
        steer: Steer,
        members: NonZeroU16,
        queues: &mut [Option<MemberProducer>],
        packet: Captured<'_>,
        budget: &mut TurnBudget,
    ) -> bool {
        match packet {
            Captured::Datagram {
                peer,
                ecn,
                length,
                payload,
                captured_at,
            } => {
                let index = steer.select(members, peer, payload);
                if !budget.dispatch(length) {
                    return false;
                }
                let discarded = match &mut queues[usize::from(index)] {
                    Some(queue) => {
                        if queue.push_at(peer, ecn, length, payload, captured_at) {
                            if let Some(token) = queue.notify() {
                                (group.ready)(token.get());
                            }
                            None
                        } else {
                            group.measure.as_ref().map(|m| &m.full)
                        }
                    }
                    None => group.measure.as_ref().map(|m| &m.dropped),
                };
                if let Some(counter) = discarded {
                    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            Captured::Discarded => {
                assert!(budget.dispatch(0));
                if let Some(measure) = &group.measure {
                    measure
                        .capture_discard
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        true
    }

    fn due(&self) -> bool {
        self.reading || self.writer_edge || self.writer_cursor != 0
    }
}

pub(super) struct Groups {
    next: u64,
    live: HashMap<u64, Intake>,
    runnable: VecDeque<u64>,
    retired: VecDeque<(u64, usize)>,
}

impl Default for Groups {
    fn default() -> Self {
        Self {
            next: FIRST,
            live: HashMap::new(),
            runnable: VecDeque::new(),
            retired: VecDeque::new(),
        }
    }
}

impl Groups {
    pub(super) fn due(&self) -> bool {
        !self.runnable.is_empty() || !self.retired.is_empty()
    }

    fn enqueue(queue: &mut VecDeque<u64>, tag: u64, intake: &mut Intake) {
        if !intake.queued {
            intake.queued = true;
            queue.push_back(tag);
        }
    }

    pub(super) fn turn(
        &mut self,
        poller: &Poller,
        events: &mut [libc::epoll_event],
        budget: &mut TurnBudget,
    ) -> usize {
        while let Some((tag, index)) = self.retired.pop_front() {
            let intake = self.live.get_mut(&tag).unwrap();
            intake.queues[index] = None;
            (intake.retire)(index);
            intake.live -= 1;
            if intake.live == 0 {
                let fd = intake.group.socket.as_fd().as_raw_fd();
                let _ = poller.remove(fd, tag, false, true);
                self.live.remove(&tag);
            }
        }
        let mut visible = 0;
        for i in 0..events.len() {
            let event = events[i];
            let tag = event.u64;
            if !(FIRST..LAST).contains(&tag) {
                events[visible] = event;
                visible += 1;
            } else if let Some(intake) = self.live.get_mut(&tag) {
                intake.reading |= event.events & (READ | FAILED) != 0;
                intake.writer_edge |= event.events & (WRITE | FAILED) != 0;
                Self::enqueue(&mut self.runnable, tag, intake);
            }
        }
        let mut writer_visits = 64;
        let mut revisit_first = None;
        for i in 0..self.runnable.len() {
            if !budget.has_datagrams() && writer_visits == 0 {
                break;
            }
            let tag = self.runnable.pop_front().unwrap();
            let Some(intake) = self.live.get_mut(&tag) else {
                continue;
            };
            intake.queued = false;
            intake.dispatch(budget, &mut writer_visits);
            if intake.due() {
                if i == 0 {
                    revisit_first = Some(tag);
                } else {
                    Self::enqueue(&mut self.runnable, tag, intake);
                }
            }
        }
        if let Some(tag) = revisit_first {
            Self::enqueue(&mut self.runnable, tag, self.live.get_mut(&tag).unwrap());
        }
        visible
    }
}

impl Poller {
    pub(super) fn has_udp_groups(&self) -> bool {
        self.udp_groups
            .get()
            .is_some_and(|groups| !groups.lock().unwrap().live.is_empty())
    }

    pub fn udp_due(&self) -> bool {
        !self.udp_deferred.lock().unwrap().readers.is_empty()
            || self
                .udp_groups
                .get()
                .is_some_and(|groups| groups.lock().unwrap().due())
    }

    pub fn dispatch_udp_slot(&self, events: &mut [libc::epoll_event]) -> usize {
        let reader = {
            let mut deferred = self.udp_deferred.lock().unwrap();
            if deferred.readers.is_empty() {
                None
            } else if self.udp_groups.get().is_some() && deferred.group_first {
                deferred.group_first = false;
                None
            } else {
                deferred.group_first = true;
                deferred.readers.pop_front()
            }
        };
        let mut budget = self.udp_turn_budget();
        *budget = TurnBudget::default();
        let count = self.udp_groups.get().map_or(events.len(), |groups| {
            let mut paused = TurnBudget::paused();
            let budget = if reader.is_some() {
                &mut paused
            } else {
                &mut *budget
            };
            groups.lock().unwrap().turn(self, events, budget)
        });
        drop(budget);
        if let Some(wake) = reader {
            wake();
        }
        count
    }

    pub fn defer_udp_reader(&self, wake: impl FnOnce() + Send + 'static) {
        self.udp_deferred
            .lock()
            .unwrap()
            .readers
            .push_back(Box::new(wake));
        self.wake();
    }

    pub fn udp_turn_budget(&self) -> std::sync::MutexGuard<'_, TurnBudget> {
        self.udp_budget.lock().unwrap()
    }

    pub fn add_udp_group(
        self: &Arc<Self>,
        socket: Socket,
        options: Options,
        members: NonZeroU16,
        steer: Steer,
        ready: impl Fn(u64) + Send + Sync + 'static,
        retire: impl FnMut(usize) + Send + 'static,
    ) -> io::Result<Vec<UdpMember>> {
        let pool = ReceivePool::new(options)?;
        let mut groups = self
            .udp_groups
            .get_or_init(Default::default)
            .lock()
            .unwrap();
        let tag = groups.next;
        if tag == LAST {
            return Err(io::Error::from_raw_os_error(libc::ENFILE));
        }
        let group = Arc::new(Group {
            socket,
            poller: Arc::downgrade(self),
            tag,
            writers: WriterList::new(members.get()),
            ready: Box::new(ready),
            measure: measure::Group::new(usize::from(members.get())),
        });
        let count = usize::from(members.get());
        let mut queues = Vec::new();
        let mut readers = Vec::new();
        let mut handles = Vec::new();
        queues
            .try_reserve_exact(count)
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOMEM))?;
        readers
            .try_reserve_exact(count)
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOMEM))?;
        handles
            .try_reserve_exact(count)
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOMEM))?;
        for _ in 0..count {
            let (producer, reader) = member_queue(options)?;
            queues.push(Some(producer));
            readers.push(reader);
        }
        self.add(&group.socket.descriptor(), tag, true)?;
        groups.next += 1;
        groups.live.insert(
            tag,
            Intake {
                group: group.clone(),
                steer,
                members,
                live: count,
                queues,
                pool,
                reading: false,
                writer_edge: false,
                writer_cursor: 0,
                queued: false,
                retire: Box::new(retire),
            },
        );
        for (index, reader) in readers.into_iter().enumerate() {
            handles.push(UdpMember {
                group: Some(group.clone()),
                index,
                reader: Some(reader),
            });
        }
        Ok(handles)
    }

    fn retire_member(&self, tag: u64, index: usize) {
        self.udp_groups
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .retired
            .push_back((tag, index));
        self.wake();
    }
}
