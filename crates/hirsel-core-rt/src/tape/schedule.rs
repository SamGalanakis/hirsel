//! Semantic event coordinates, independent of task pointers and host time.
//! Like rr's Frame ticks, gaps locate inputs; Hirsel counts CONC-10 events
//! instead of instructions so both native engines use the same coordinates.
use super::*;

#[derive(Clone, Copy)]
pub(crate) struct Group {
    cell: &'static Cell,
    generation: u32,
    pub task: u64,
}
impl Group {
    pub fn root(cell: &'static Cell, generation: u32) -> Self {
        Self {
            cell,
            generation,
            task: 0,
        }
    }
    pub fn same(self, other: Self) -> bool {
        ptr::eq(self.cell, other.cell) && self.generation == other.generation
    }
    fn with<R>(self, f: impl FnOnce(&mut Live) -> R) -> Option<R> {
        let mut state = self.cell.state.lock().unwrap();
        if state.generation != self.generation {
            return None;
        }
        state.live.as_mut().map(f)
    }
    pub fn check(self) -> Result<(), Fault> {
        self.with(|live| live.fault.map_or(Ok(()), Err))
            .unwrap_or(Err(Fault::Ended))
    }
    pub fn cancel_wait(self, request: *mut Request) {
        self.with(|live| {
            live.waiting.remove(&(request as usize));
        });
    }
    pub fn child(self) -> Self {
        let task = self
            .with(|live| {
                let task = live.schedule.next_task;
                live.schedule.next_task += 1;
                task
            })
            .unwrap_or(0);
        Self { task, ..self }
    }
    pub fn event(self, index: impl FnOnce() -> u64, ended: bool) {
        self.with(|live| {
            live.commit(self.task, index);
            if ended {
                live.schedule.previous = None;
            }
        });
    }
    /// Only the active replay dispatcher asks for a turn. WAIT and GRANT
    /// are consumed at their own call sites, after the named fiber reaches them.
    pub fn turn(self) -> Turn {
        self.with(Live::turn).unwrap_or(Turn::Fifo)
    }
    pub fn choose(self, task: u64) {
        self.with(|live| {
            let rep = live.replayer.as_mut().unwrap();
            rep.next().unwrap();
            live.schedule.last = live.schedule.events;
            live.schedule.selected = Some(task);
        });
    }
    pub fn fifo(self) -> bool {
        self.with(|live| live.schedule.chooser == format::CHOOSER_FIFO)
            .unwrap_or(false)
    }
    pub fn replaying(self) -> bool {
        self.with(|live| live.mode == Mode::Replay).unwrap_or(false)
    }
    pub fn failed(self) -> bool {
        self.with(|live| live.fault.is_some()).unwrap_or(true)
    }
    pub fn fail(self) {
        self.with(Live::schedule_fault);
    }
    pub fn wake(self, request: *mut Request) {
        self.with(|live| {
            if live.mode == Mode::Record && live.waiting.remove(&(request as usize)).is_some() {
                live.write_input(Frame::Wake {
                    gap: 0,
                    task: self.task,
                });
            }
        });
    }
    /// An external request is an input. Replay obtains it from CANCEL instead.
    pub fn cancel(self, source: Option<Self>) -> bool {
        if source.is_some_and(|s| s.same(self)) {
            return true;
        }
        self.with(|live| {
            if live.mode == Mode::Replay {
                return false;
            }
            live.write_input(Frame::Cancel {
                gap: 0,
                task: self.task,
            });
            true
        })
        .unwrap_or(true)
    }
    pub fn consume_input(self) {
        self.with(|live| {
            live.replayer.as_mut().unwrap().next().unwrap();
            live.schedule.last = live.schedule.events;
        });
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Turn {
    Fifo,
    Event(Option<u64>),
    Choose(u64),
    Wait(u64),
    Grant,
    Finish,
    Wake(u64),
    Cancel(u64),
    Done(u64),
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
            self.fault = Some(refuse(Fault::Diverged(Divergence {
                at,
                kind: Mismatch::Schedule,
            })));
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
                Frame::End { gap, .. } => (gap, Turn::Finish),
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
        if let Err(e) = self.recorder.as_mut().unwrap().frame(&frame) {
            self.fault = Some(refuse(Fault::Io(e)));
        }
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
