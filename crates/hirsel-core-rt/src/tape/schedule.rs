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
    pub fn event(self) {
        self.with(|live| live.schedule.events += 1);
    }
    pub fn replaying(self) -> bool {
        self.with(|live| live.mode == Mode::Replay).unwrap_or(false)
    }
    pub fn failed(self) -> bool {
        self.with(|live| live.fault.is_some()).unwrap_or(true)
    }
    pub fn fail(self) {
        self.with(|live| {
            if live.fault.is_none() {
                let at = live.replayer.as_ref().map_or(1, |r| r.taken + 1);
                live.fault = Some(refuse(Fault::Diverged(Divergence {
                    at,
                    kind: Mismatch::Schedule,
                })));
            }
        });
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
    pub fn input(self) -> Option<Input> {
        self.with(|live| {
            if live.mode != Mode::Replay || live.fault.is_some() {
                return None;
            }
            let rep = live.replayer.as_mut().unwrap();
            let frame = match rep.peek() {
                Ok(frame) => frame,
                Err(f) => {
                    live.fault = Some(refuse(f));
                    return None;
                }
            };
            let (gap, input) = match frame {
                Frame::Wake { gap, task } => (*gap, Input::Wake(*task)),
                Frame::Cancel { gap, task } => (*gap, Input::Cancel(*task)),
                Frame::Done { gap, task, .. } => (*gap, Input::Done(*task)),
                _ => return None,
            };
            let Some(position) = live.schedule.last.checked_add(gap) else {
                live.fault = Some(refuse(Fault::Corrupt));
                return None;
            };
            if position > live.schedule.events {
                return None;
            }
            if position < live.schedule.events {
                live.fault = Some(refuse(rep.diverged_next(Mismatch::Schedule)));
                return None;
            }
            // DONE is consumed by the tape's resume callback.
            if !matches!(input, Input::Done(_)) {
                rep.next().unwrap();
                live.schedule.last = position;
            }
            Some(input)
        })
        .flatten()
    }
    /// Select among this group's ready tasks, in ready order.
    pub fn choose(self, count: usize, proposed: usize) -> usize {
        self.with(|live| {
            if count < 2 || live.fault.is_some() {
                return 0;
            }
            if live.schedule.chooser == 0 {
                return 0;
            }
            if live.mode == Mode::Record {
                live.write_input(Frame::Choose {
                    gap: 0,
                    index: proposed as u64,
                });
                return proposed;
            }
            let rep = live.replayer.as_mut().unwrap();
            match rep.next() {
                Ok(Frame::Choose { gap, index })
                    if live.schedule.last.checked_add(gap) == Some(live.schedule.events)
                        && index < count as u64 =>
                {
                    live.schedule.last = live.schedule.events;
                    index as usize
                }
                _ => {
                    live.fault = Some(refuse(rep.diverged(Mismatch::Schedule)));
                    0
                }
            }
        })
        .unwrap_or(0)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Input {
    Wake(u64),
    Cancel(u64),
    Done(u64),
}

pub(super) struct Schedule {
    pub events: u64,
    pub last: u64,
    next_task: u64,
    chooser: u8,
}
impl Schedule {
    pub fn new(chooser: u8) -> Self {
        Self {
            events: 0,
            last: 0,
            next_task: 1,
            chooser,
        }
    }
}
impl Live {
    /// Own frames sit at the issue; grants sit at the current coordinate.
    pub(super) fn own_position(&mut self) -> u64 {
        let position = self.schedule.events;
        self.schedule.last = position;
        position
    }
    pub(super) fn write_input(&mut self, mut frame: Frame) {
        let gap = self.schedule.events - self.schedule.last;
        match &mut frame {
            Frame::Wake { gap: g, .. }
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
