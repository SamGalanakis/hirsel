//! Semantic event coordinates, independent of task pointers and host time.
//! Like rr's Frame ticks, gaps locate inputs; Hirsel counts CONC-10 events
//! instead of instructions so both native engines use the same coordinates.
pub(crate) use super::dispatch::{Group, Turn};
use super::{dispatch::Hooks, *};

impl Group {
    fn with<R>(self, f: impl FnOnce(&mut Live) -> R) -> Option<R> {
        let mut state = self.cell.state.lock().unwrap();
        if state.generation != self.generation {
            return None;
        }
        state.live.as_mut().map(f)
    }
    pub fn fifo(self) -> bool {
        self.with(|live| live.schedule.chooser == format::CHOOSER_FIFO)
            .unwrap_or(false)
    }
}

pub(super) struct TapeHooks;

impl Hooks for TapeHooks {
    fn seal_deadlock(&self, group: Group) -> Result<(), ([u8; 16], Fault)> {
        let id = group.with(|live| live.tape_id).unwrap();
        finish(group.cell, group.generation, Seal::Deadlock)
            .map(|_| ())
            .map_err(|fault| (id, fault))
    }

    fn check(&self, group: Group) -> Result<(), Fault> {
        group
            .with(|live| live.fault.map_or(Ok(()), Err))
            .unwrap_or(Err(Fault::Ended))
    }
    fn cancel_wait(&self, group: Group, request: *mut Request) {
        group.with(|live| {
            live.waiting.remove(&(request as usize));
        });
    }
    fn child(&self, group: Group) -> Group {
        let task = group
            .with(|live| {
                let task = live.schedule.next_task;
                live.schedule.next_task += 1;
                task
            })
            .unwrap_or(0);
        Group { task, ..group }
    }
    fn event(&self, group: Group, index: &dyn Fn() -> u64, ended: bool) {
        group.with(|live| {
            live.commit(group.task, index);
            if ended {
                live.schedule.previous = None;
            }
        });
    }
    /// Only the active replay dispatcher asks for a turn. WAIT and GRANT
    /// are consumed at their own call sites, after the named fiber reaches them.
    fn turn(&self, group: Group) -> Turn {
        group.with(Live::turn).unwrap_or(Turn::Fifo)
    }
    fn choose(&self, group: Group, task: u64) {
        group.with(|live| {
            let rep = live.replayer.as_mut().unwrap();
            rep.next().unwrap();
            live.schedule.last = live.schedule.events;
            live.schedule.selected = Some(task);
        });
    }
    fn replaying(&self, group: Group) -> bool {
        group
            .with(|live| live.mode == Mode::Replay)
            .unwrap_or(false)
    }
    fn fail(&self, group: Group) {
        group.with(Live::schedule_fault);
    }
    fn wake(&self, group: Group, request: *mut Request) {
        group.with(|live| {
            if live.mode == Mode::Record && live.waiting.remove(&(request as usize)).is_some() {
                live.write_input(Frame::Wake {
                    gap: 0,
                    task: group.task,
                });
            }
        });
    }
    /// An external request is an input. Replay obtains it from CANCEL instead.
    fn cancel(&self, group: Group, source: Option<Group>) -> bool {
        if source.is_some_and(|s| s.same(group)) {
            return true;
        }
        group
            .with(|live| {
                if live.mode == Mode::Replay {
                    return false;
                }
                live.write_input(Frame::Cancel {
                    gap: 0,
                    task: group.task,
                });
                true
            })
            .unwrap_or(true)
    }
    fn consume_input(&self, group: Group) {
        group.with(|live| {
            live.replayer.as_mut().unwrap().next().unwrap();
            live.schedule.last = live.schedule.events;
        });
    }
    fn source(&self, cell: &Cell, handle: u64) -> Option<(Provider, u64)> {
        let state = cell.state.lock().unwrap();
        if state.generation != generation_of(handle) {
            return None;
        }
        let inner = state.live.as_ref()?.objects[ordinal_of(handle) - 1].inner?;
        Some((inner.provider, inner.handle))
    }

    fn net_rules(&self, cell: &Cell, handle: u64) -> u64 {
        let state = cell.state.lock().unwrap();
        let live = state.live.as_ref().unwrap();
        let ordinal = ordinal_of(handle);
        match live.objects[ordinal - 1].inner {
            Some(inner) => inner.handle,
            None => live.replay_net_rules[&ordinal],
        }
    }

    fn narrow_net(&self, cell: &'static Cell, parent: u64, rules: u64) -> (u64, Group) {
        let _commit = crate::tasks::before_commit();
        let mut state = cell.state.lock().unwrap();
        let generation = state.generation;
        let live = state.live.as_mut().unwrap();
        let task = super::schedule::task(cell);
        live.commit(task, crate::tasks::group_index);
        let inner = live.objects[ordinal_of(parent) - 1]
            .inner
            .map(|source| Inner {
                provider: source.provider,
                handle: rules,
            });
        let ordinal = live.objects.len() + 1;
        if inner.is_none() {
            live.replay_net_rules.insert(ordinal, rules);
        }
        live.objects.push(Object {
            inner,
            open_file: false,
        });
        (handle(generation, ordinal), Group::root(cell, generation))
    }

    unsafe fn issue(&self, cell: &Cell, request: *mut Request, token: u64) -> Step {
        // SAFETY: the driver pins the request for this call.
        unsafe { issue_in(cell, &mut *request, token) }
    }
}

pub(super) struct Schedule {
    pub events: u64,
    pub last: u64,
    next_task: u64,
    chooser: u8,
    previous: Option<u64>,
    selected: Option<u64>,
}
impl Schedule {
    pub fn new(chooser: u8) -> Self {
        Self {
            events: 0,
            last: 0,
            next_task: 1,
            chooser,
            previous: Some(0),
            selected: None,
        }
    }

    pub fn at(&self, gap: u64) -> bool {
        self.last.checked_add(gap) == Some(self.events)
    }
}
impl Live {
    pub(super) fn schedule_fault(&mut self) {
        if self.fault.is_none() {
            let at = self.replayer.as_ref().map_or(1, |r| r.taken + 1);
            let f = Fault::Diverged(Divergence {
                at,
                kind: Mismatch::Schedule,
            });
            self.fault = Some(refuse(f));
            self.abort_replay(f, 0);
        }
    }

    pub(super) fn turn(&mut self) -> Turn {
        let Some(rep) = self.replayer.as_mut().filter(|_| self.fault.is_none()) else {
            return Turn::Fifo;
        };
        let event = if self.schedule.chooser == format::CHOOSER_COMMIT {
            Turn::Event(self.schedule.selected.or(self.schedule.previous))
        } else {
            Turn::Fifo
        };
        let (gap, turn) = match rep.peek() {
            Ok(frame) => match *frame {
                Frame::Wait { gap, task } => (gap, Turn::Wait(task)),
                Frame::Grant { gap, .. } => (gap, Turn::Grant),
                Frame::End { gap, ref ending } => {
                    let turn = if ending.kind == END_DEADLOCK {
                        Turn::Deadlock
                    } else {
                        Turn::Finish
                    };
                    (gap, turn)
                }
                Frame::Wake { gap, task } => (gap, Turn::Wake(task)),
                Frame::Cancel { gap, task } => (gap, Turn::Cancel(task)),
                Frame::Done { gap, task, .. } => (gap, Turn::Done(task)),
                Frame::Choose { gap, index } if self.schedule.chooser == format::CHOOSER_COMMIT => {
                    (gap, Turn::Choose(index))
                }
                Frame::Choose { .. } => {
                    self.schedule_fault();
                    return Turn::Fifo;
                }
                _ => return event,
            },
            Err(f) => {
                self.fault = Some(refuse(f));
                self.abort_replay(f, 0);
                return Turn::Fifo;
            }
        };
        match self.schedule.last.checked_add(gap) {
            Some(position) if position > self.schedule.events => event,
            Some(position)
                if position == self.schedule.events && self.schedule.selected.is_none() =>
            {
                turn
            }
            None => {
                self.fault = Some(refuse(Fault::Corrupt));
                self.abort_replay(Fault::Corrupt, 0);
                Turn::Fifo
            }
            Some(_) => {
                self.schedule_fault();
                Turn::Fifo
            }
        }
    }

    /// An ISSUE shares exactly the same committer identity as a language event.
    /// A synchronous outcome then consumes one more position in the same turn.
    pub(super) fn commit(&mut self, task: u64, index: impl FnOnce() -> u64) {
        if self.fault.is_some() {
            return;
        }
        if self.schedule.chooser == format::CHOOSER_COMMIT {
            if self.mode == Mode::Record && self.schedule.previous != Some(task) {
                self.write_input(Frame::Choose {
                    gap: 0,
                    index: index(),
                });
            }
            self.schedule.previous = Some(task);
            self.schedule.selected = None;
        }
        self.schedule.events += 1;
    }

    pub(super) fn delivery(&mut self, task: u64) {
        self.schedule.events += 1;
        self.schedule.previous = Some(task);
    }

    pub(super) fn write_input(&mut self, mut frame: Frame) {
        if self.fault.is_some() {
            return;
        }
        let gap = self.schedule.events - self.schedule.last;
        match &mut frame {
            Frame::Wait { gap: g, .. }
            | Frame::Grant { gap: g, .. }
            | Frame::Wake { gap: g, .. }
            | Frame::Cancel { gap: g, .. }
            | Frame::Choose { gap: g, .. }
            | Frame::Done { gap: g, .. } => *g = gap,
            _ => unreachable!(),
        }
        self.schedule.last = self.schedule.events;
        let _ = self.recorder.as_mut().unwrap().frame(&frame);
    }
}

pub(super) fn task(cell: &Cell) -> u64 {
    crate::tasks::provider_group()
        .filter(|g| ptr::eq(g.cell, cell))
        .map_or(0, |g| g.task)
}
pub(super) fn task_from_live() -> u64 {
    crate::tasks::provider_group().map_or(0, |g| g.task)
}

pub(super) fn member(cell: &Cell, generation: u32) -> bool {
    match crate::tasks::provider_group() {
        Some(group) => ptr::eq(group.cell, cell) && group.generation == generation,
        None => !crate::tasks::has_provider_task(),
    }
}
