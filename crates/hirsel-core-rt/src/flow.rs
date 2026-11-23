//! The owner-driven CONC-14 reference. A merge sees FIFO positions, including
//! the marks between items. Taking an item is the only operation that frees a slot.
#[doc(hidden)]
pub mod bench;
mod native;
#[cfg(test)]
pub(crate) use native::fixtures;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Terminal {
    End,
    Dropped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Entry {
    Mark,
    Item,
    Terminal(Terminal),
}

pub(crate) struct Slot<T> {
    key: u64,
    following: u64,
    value: T,
}

pub(crate) trait Fifo<T> {
    fn new(cap: usize, size: usize) -> Self;
    fn len(&self) -> usize;
    fn first(&self) -> Option<u64>;
    fn value<R>(&self, read: impl FnOnce(&T) -> R) -> R;
    fn push(&mut self, key: u64, value: T);
    fn pop(&mut self) -> Slot<T>;
    fn advance(&mut self, mark: u64) -> bool;
}

impl<T> Fifo<T> for VecDeque<Slot<T>> {
    fn new(cap: usize, _: usize) -> Self {
        Self::with_capacity(cap)
    }
    fn len(&self) -> usize {
        self.len()
    }
    fn first(&self) -> Option<u64> {
        self.front().map(|slot| slot.key)
    }
    fn value<R>(&self, read: impl FnOnce(&T) -> R) -> R {
        read(&self.front().unwrap().value)
    }
    fn push(&mut self, key: u64, value: T) {
        self.push_back(Slot {
            key,
            following: key,
            value,
        });
    }
    fn pop(&mut self) -> Slot<T> {
        self.pop_front().expect("taking the merge's selected item")
    }
    fn advance(&mut self, mark: u64) -> bool {
        if let Some(last) = self.back_mut() {
            last.following = mark;
            true
        } else {
            false
        }
    }
}

struct Port<T, Q> {
    cap: usize,
    slots: Q,
    head_mark: Option<u64>,
    bound: u64,
    terminal: Option<Terminal>,
    terminal_taken: bool,
    /// The occupied slots are retained when the reader is gone. Their count
    /// is pushes minus takes for this port, so fullness is exactly i > t + cap.
    reader_gone: bool,
    payload: std::marker::PhantomData<T>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Push {
    Ready,
    Ended,
    Order,
    Closed,
    Full,
}

impl<T, Q: Fifo<T>> Port<T, Q> {
    fn new(cap: usize, size: usize) -> Self {
        Self {
            cap,
            slots: Q::new(cap, size),
            head_mark: None,
            bound: 0,
            terminal: None,
            terminal_taken: false,
            reader_gone: false,
            payload: std::marker::PhantomData,
        }
    }

    fn push_status(&self, key: u64) -> Push {
        if self.terminal.is_some() {
            Push::Ended
        } else if key < self.bound {
            Push::Order
        } else if self.slots.len() < self.cap {
            Push::Ready
        } else if self.reader_gone {
            Push::Closed
        } else {
            Push::Full
        }
    }

    fn push(&mut self, key: u64, value: T) {
        self.slots.push(key, value);
        self.bound = key;
    }

    fn advance(&mut self, key: u64) {
        if self.terminal.is_some() || key <= self.bound {
            return;
        }
        self.bound = key;
        if !self.slots.advance(key) {
            self.head_mark = Some(key);
        }
    }

    fn end(&mut self) {
        self.terminal.get_or_insert(Terminal::End);
    }

    fn drop_at(&mut self, key: u64) {
        self.bound = key;
        self.terminal = Some(Terminal::Dropped);
    }

    fn head(&self) -> Option<(u64, Entry)> {
        if let Some(key) = self.head_mark {
            Some((key, Entry::Mark))
        } else if let Some(key) = self.slots.first() {
            Some((key, Entry::Item))
        } else {
            self.terminal
                .filter(|_| !self.terminal_taken)
                .map(|terminal| (self.bound, Entry::Terminal(terminal)))
        }
    }

    fn take(&mut self, entry: Entry) -> Option<T> {
        match entry {
            Entry::Mark => self.head_mark = None,
            Entry::Terminal(_) => self.terminal_taken = true,
            Entry::Item => {
                let slot = self.slots.pop();
                self.head_mark = (slot.following != slot.key).then_some(slot.following);
                return Some(slot.value);
            }
        }
        None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Head {
    Entry {
        index: usize,
        key: u64,
        entry: Entry,
    },
    End,
    Stalled,
}

/// Merge lower bounds with (key, port index) ordering. An empty live port
/// with equal key and lower index can still produce an earlier entry.
fn merge_head<T, Q: Fifo<T>>(ports: &[Port<T, Q>], merge: &[usize]) -> Head {
    let mut least = None;
    for (index, &port) in merge.iter().enumerate() {
        let port = &ports[port];
        if port.terminal_taken {
            continue;
        }
        let head = port.head();
        let key = head.map_or(port.bound, |(key, _)| key);
        if least.is_none_or(|(other, _, _)| key < other) {
            least = Some((key, index, head.map(|(_, entry)| entry)));
        }
    }
    match least {
        None => Head::End,
        Some((key, index, Some(entry))) => Head::Entry { index, key, entry },
        Some((_, _, None)) => Head::Stalled,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Halt {
    OutOfTime,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Failure {
    pub stage: usize,
    pub key: u64,
    pub halt: Halt,
}

type Transform<T> = Box<dyn FnMut(u64, &T) -> Result<Option<T>, Halt>>;

struct Stage<T> {
    input: usize,
    output: usize,
    body: Option<Transform<T>>,
    /// Only an item awaiting its output credit needs a prepared result. The
    /// input head is still present until that step can commit.
    prepared: Option<Result<Option<T>, Halt>>,
    live: bool,
    failure: Option<Failure>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Pull<T> {
    Item(u64, usize, T),
    Dropped(usize, u64),
    End,
    Stalled,
}

#[derive(Debug, Eq, PartialEq)]
pub enum PushError<T> {
    Ended(T),
    Order(T),
    Closed(T),
    Stalled(T),
    Foreign(T),
}

#[derive(Debug, Eq, PartialEq)]
pub enum WireError {
    Started,
    Zero,
    Foreign,
    Refused(crate::Capacity),
}

#[derive(Clone, Copy)]
pub struct Endpoint(u64);

/// Endpoints are plain words and may outlive the flow that made them.
static NEXT_ENDPOINT: AtomicU64 = AtomicU64::new(1);

fn endpoint_id() -> u64 {
    NEXT_ENDPOINT
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .unwrap_or_else(|_| crate::process::stop(b"FlowIds"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Foreign;

pub struct StageCharge {
    pub output: u64,
    pub working: u64,
    pub state: u64,
    pub item_steps: u64,
}

fn port_charge(cap: u32, size: u64) -> Option<u64> {
    size.checked_add(16)?
        .checked_mul(u64::from(cap))?
        .checked_add(64)
}

/// The ordered graph. Items are plain, so preparing a stage's call can view
/// the head until the step commits its take. Each stage owns its callback and
/// state, so preparing a result reads nothing any other stage can observe.
pub(crate) struct Reference<T, Q = VecDeque<Slot<T>>> {
    endpoints: BTreeMap<u64, usize>,
    ports: Vec<Port<T, Q>>,
    merges: Vec<Vec<usize>>,
    sinks: Vec<usize>,
    writers: Vec<usize>,
    stages: Vec<Stage<T>>,
    sealed: bool,
    charges: Vec<crate::block::Block>,
}

impl<T, Q> Default for Reference<T, Q> {
    fn default() -> Self {
        Self {
            endpoints: BTreeMap::new(),
            ports: Vec::new(),
            merges: Vec::new(),
            sinks: Vec::new(),
            writers: Vec::new(),
            stages: Vec::new(),
            sealed: false,
            charges: Vec::new(),
        }
    }
}

impl<T, Q: Fifo<T>> Reference<T, Q> {
    fn endpoint(&mut self, index: usize) -> Endpoint {
        let id = endpoint_id();
        self.endpoints.insert(id, index);
        Endpoint(id)
    }

    fn index(&self, endpoint: Endpoint) -> Result<usize, Foreign> {
        self.endpoints.get(&endpoint.0).copied().ok_or(Foreign)
    }

    fn admit(&mut self, context: &crate::Context, charge: Option<u64>) -> Result<(), WireError> {
        let debit = crate::block::Block::new(context, charge, 1, 8).map_err(WireError::Refused)?;
        self.charges.push(debit);
        Ok(())
    }
    fn merge(&mut self) -> usize {
        let merge = self.merges.len();
        self.merges.push(Vec::new());
        merge
    }

    pub fn sink(&mut self) -> Result<Endpoint, WireError> {
        if self.sealed {
            return Err(WireError::Started);
        }
        let merge = self.merge();
        self.sinks.push(merge);
        Ok(self.endpoint(merge))
    }

    fn connect(
        &mut self,
        to: Endpoint,
        cap: u32,
        size: usize,
        context: &crate::Context,
        charge: Option<u64>,
    ) -> Result<usize, WireError> {
        if self.sealed {
            return Err(WireError::Started);
        }
        if cap == 0 {
            return Err(WireError::Zero);
        }
        let to = self.index(to).map_err(|Foreign| WireError::Foreign)?;
        self.admit(context, charge)?;
        let port = self.ports.len();
        self.merges[to].push(port);
        self.ports.push(Port::new(cap as usize, size));
        Ok(port)
    }

    #[cfg(test)]
    pub fn port(
        &mut self,
        context: &crate::Context,
        to: Endpoint,
        cap: u32,
        item_size: u64,
    ) -> Result<Endpoint, WireError> {
        self.port_with_size(context, to, cap, item_size, 0)
    }

    pub fn port_with_size(
        &mut self,
        context: &crate::Context,
        to: Endpoint,
        cap: u32,
        item_size: u64,
        physical: usize,
    ) -> Result<Endpoint, WireError> {
        let port = self.connect(to, cap, physical, context, port_charge(cap, item_size))?;
        self.writers.push(port);
        Ok(self.endpoint(port))
    }

    #[cfg(test)]
    pub fn stage(
        &mut self,
        context: &crate::Context,
        to: Endpoint,
        cap: u32,
        charge: StageCharge,
        body: impl FnMut(&mut crate::Context, u64, &T) -> Result<Option<T>, Halt> + 'static,
    ) -> Result<Endpoint, WireError> {
        self.stage_with_size(context, to, cap, charge, 0, || body)
    }

    pub fn stage_with_size<F>(
        &mut self,
        context: &crate::Context,
        to: Endpoint,
        cap: u32,
        charge: StageCharge,
        physical: usize,
        make: impl FnOnce() -> F,
    ) -> Result<Endpoint, WireError>
    where
        F: FnMut(&mut crate::Context, u64, &T) -> Result<Option<T>, Halt> + 'static,
    {
        let total = port_charge(cap, charge.output).and_then(|port| {
            port.checked_add(charge.working)?
                .checked_add(charge.state)?
                .checked_add(320)
        });
        let output = self.connect(to, cap, physical, context, total)?;
        let input = self.merge();
        let mut body = make();
        let mut stage = crate::tasks::private_flow_step(|| crate::Context::new(charge.working));
        stage.primary = Some(crate::causes::Cell::startup(stage.payer()));
        stage.run = context.run;
        crate::steps::inherit(&mut stage, context);
        self.stages.push(Stage {
            input,
            output,
            body: Some(Box::new(move |key, input| {
                crate::tasks::private_flow_step(|| {
                    stage.current.bind();
                    // SAFETY: the reference exclusively owns this context, and no
                    // stage code can park or enter another task.
                    unsafe {
                        if !crate::steps::hirsel_core_step_enter(&mut stage, charge.item_steps) {
                            return Err(Halt::OutOfTime);
                        }
                        let result = body(&mut stage, key, input);
                        crate::steps::hirsel_core_step_leave(&mut stage);
                        match stage.causes.iter().next() {
                            Some(cause) if cause.kind() == crate::causes::OUT_OF_TIME => {
                                Err(Halt::OutOfTime)
                            }
                            Some(_) => Err(Halt::Failed),
                            None => result,
                        }
                    }
                })
            })),
            prepared: None,
            live: true,
            failure: None,
        });
        Ok(self.endpoint(input))
    }

    fn enabled(&mut self, index: usize) -> Option<Head> {
        let stage = &mut self.stages[index];
        if !stage.live {
            return None;
        }
        let input = &self.merges[stage.input];
        let head = merge_head(&self.ports, input);
        match head {
            Head::Stalled => None,
            Head::Entry {
                index,
                key,
                entry: Entry::Item,
            } => {
                let slots = &self.ports[input[index]].slots;
                let prepared = stage.prepared.get_or_insert_with(|| {
                    slots.value(|value| (stage.body.as_mut().unwrap())(key, value))
                });
                if matches!(prepared, Ok(Some(_)))
                    && self.ports[stage.output].push_status(key) == Push::Full
                {
                    None
                } else {
                    Some(head)
                }
            }
            _ => Some(head),
        }
    }

    fn step(&mut self, stage: usize, head: Head) {
        self.take_step(stage, head);
        self.publish_step(stage, head);
    }

    fn take_step(&mut self, stage: usize, head: Head) -> Option<usize> {
        let Head::Entry { index, entry, .. } = head else {
            return None;
        };
        let port = self.merges[self.stages[stage].input][index];
        self.ports[port].take(entry);
        (entry == Entry::Item).then_some(port)
    }

    fn publish_step(&mut self, stage: usize, head: Head) {
        let s = &mut self.stages[stage];
        let input = &self.merges[s.input];
        let output = &mut self.ports[s.output];
        let (key, entry) = match head {
            Head::End => {
                output.end();
                s.live = false;
                return;
            }
            Head::Entry { key, entry, .. } => (key, entry),
            Head::Stalled => unreachable!("a stalled merge has no step"),
        };
        match entry {
            Entry::Mark => output.advance(key),
            Entry::Terminal(Terminal::End) => {}
            Entry::Terminal(Terminal::Dropped) => {
                output.drop_at(key);
                s.live = false;
            }
            Entry::Item => match s.prepared.take().unwrap() {
                Ok(None) => output.advance(key),
                Ok(Some(value)) => match output.push_status(key) {
                    Push::Ready => output.push(key, value),
                    Push::Closed => s.live = false,
                    _ => unreachable!("a completed step has output credit or Closed"),
                },
                Err(halt) => {
                    output.drop_at(key);
                    s.failure = Some(Failure { stage, key, halt });
                    s.live = false;
                }
            },
        }
        if !s.live {
            for &port in input {
                self.ports[port].reader_gone = true;
            }
        }
    }

    fn close(&mut self, drain: bool) {
        let beside = self.beside();
        loop {
            if drain {
                for &sink in &self.sinks {
                    while let Head::Entry { index, entry, .. } =
                        merge_head(&self.ports, &self.merges[sink])
                    {
                        self.ports[self.merges[sink][index]].take(entry);
                    }
                }
            }
            if !self.progress(beside) {
                break;
            }
        }
    }

    /// One step toward the closure. On a worker pool a step whose result is
    /// known commits first, and otherwise every stage whose head item still
    /// needs its call runs it on a helper unit at once.
    fn progress(&mut self, beside: bool) -> bool {
        if beside {
            if self.step_ready() {
                return true;
            }
            self.prepare_beside();
        }
        self.step_one()
    }

    /// On a worker pool the owner's operations take steps only as their
    /// answers need them, as the scripted lazy mode does, so items queue up
    /// and stage calls can run side by side. A prepared head stays the head
    /// until its step commits, and each call reads only its own stage and
    /// input port, so each stage makes the same calls in the same order.
    fn beside(&self) -> bool {
        #[cfg(any(test, feature = "testing"))]
        if crate::tasks::flow_script().is_some() {
            return false;
        }
        crate::units::running().is_some_and(|_| crate::units::pool().w > 1)
    }

    fn needs_body(&self, index: usize) -> bool {
        let stage = &self.stages[index];
        stage.live
            && stage.prepared.is_none()
            && matches!(
                merge_head(&self.ports, &self.merges[stage.input]),
                Head::Entry {
                    entry: Entry::Item,
                    ..
                }
            )
    }

    fn step_ready(&mut self) -> bool {
        let Some((stage, head)) = (0..self.stages.len()).find_map(|stage| {
            if self.needs_body(stage) {
                return None;
            }
            self.enabled(stage).map(|head| (stage, head))
        }) else {
            return false;
        };
        self.step(stage, head);
        true
    }

    fn prepare_beside(&mut self) {
        let waiting: Vec<bool> = (0..self.stages.len())
            .map(|stage| self.needs_body(stage))
            .collect();
        if waiting.iter().filter(|&&waiting| waiting).count() < 2 {
            return;
        }
        let (ports, merges) = (&self.ports, &self.merges);
        let mut calls: Vec<_> = self
            .stages
            .iter_mut()
            .zip(waiting)
            .filter_map(|(stage, waiting)| waiting.then_some(stage))
            .map(|stage| {
                let input = &merges[stage.input];
                let Head::Entry { index, key, .. } = merge_head(ports, input) else {
                    unreachable!("a waiting stage has an item head")
                };
                let slots = &ports[input[index]].slots;
                move || {
                    let body = stage.body.as_mut().unwrap();
                    stage.prepared = Some(slots.value(|value| body(key, value)));
                }
            })
            .collect();
        let mut jobs: Vec<&mut dyn FnMut()> = calls
            .iter_mut()
            .map(|call| call as &mut dyn FnMut())
            .collect();
        // SAFETY: stage code cannot park. Each call writes only its own stage
        // and reads only its own input port's head, and the flow's owner
        // stays parked until every call returns.
        if !unsafe { crate::tasks::run_beside(&mut jobs) } {
            jobs.iter_mut().for_each(|call| call());
        }
    }

    fn step_one(&mut self) -> bool {
        #[cfg(any(test, feature = "testing"))]
        if let Some((script, domain)) = crate::tasks::flow_script() {
            use crate::schedule_script::{FlowStep, PausePoint, StepBoundary};
            let enabled: Vec<_> = (0..self.stages.len())
                .filter_map(|stage| self.enabled(stage).map(|head| (stage, head)))
                .collect();
            if enabled.is_empty() {
                return false;
            }
            let (stage, head) = enabled[script.choose_flow_step(domain, enabled.len())];
            let step = match head {
                Head::Entry {
                    entry: Entry::Mark, ..
                } => FlowStep::Mark,
                Head::Entry {
                    key,
                    entry: Entry::Item,
                    ..
                } => {
                    let s = &self.stages[stage];
                    if matches!(s.prepared, Some(Ok(Some(_))))
                        && self.ports[s.output].push_status(key) == Push::Closed
                    {
                        FlowStep::Closed
                    } else {
                        FlowStep::Item
                    }
                }
                _ => FlowStep::Terminal,
            };
            let boundary = |boundary| PausePoint::Flow {
                domain,
                stage,
                step,
                boundary,
            };
            script.pause(boundary(StepBoundary::Before));
            self.step(stage, head);
            script.pause(boundary(StepBoundary::After));
            return true;
        }
        let Some((stage, head)) =
            (0..self.stages.len()).find_map(|stage| self.enabled(stage).map(|head| (stage, head)))
        else {
            return false;
        };
        self.step(stage, head);
        true
    }

    fn lazy(&self) -> bool {
        #[cfg(any(test, feature = "testing"))]
        if let Some((script, _)) = crate::tasks::flow_script() {
            return script.flow_mode() == crate::schedule_script::FlowMode::Lazy;
        }
        self.beside()
    }

    fn operate(&mut self) {
        self.sealed = true;
        if !self.lazy() {
            self.close(false);
        }
    }

    pub fn push(&mut self, port: Endpoint, key: u64, value: T) -> Result<(), PushError<T>> {
        self.operate();
        let port = match self.index(port) {
            Ok(port) => port,
            Err(Foreign) => return Err(PushError::Foreign(value)),
        };
        if self.lazy() {
            let beside = self.beside();
            while self.ports[port].push_status(key) == Push::Full && self.progress(beside) {}
        }
        let port = &mut self.ports[port];
        match port.push_status(key) {
            Push::Ready => {
                port.push(key, value);
                Ok(())
            }
            Push::Ended => Err(PushError::Ended(value)),
            Push::Order => Err(PushError::Order(value)),
            Push::Closed => Err(PushError::Closed(value)),
            Push::Full => Err(PushError::Stalled(value)),
        }
    }

    pub fn advance(&mut self, port: Endpoint, key: u64) -> Result<(), Foreign> {
        self.operate();
        let port = self.index(port)?;
        self.ports[port].advance(key);
        Ok(())
    }

    pub fn end(&mut self, port: Endpoint) -> Result<(), Foreign> {
        self.operate();
        let port = self.index(port)?;
        self.ports[port].end();
        Ok(())
    }

    pub fn next(&mut self, sink: Endpoint) -> Result<Pull<T>, Foreign> {
        self.operate();
        let sink = self.index(sink)?;
        let beside = self.lazy() && self.beside();
        loop {
            let (index, key, entry) = match merge_head(&self.ports, &self.merges[sink]) {
                Head::End => return Ok(Pull::End),
                Head::Stalled if self.lazy() && self.progress(beside) => continue,
                Head::Stalled => return Ok(Pull::Stalled),
                Head::Entry { index, key, entry } => (index, key, entry),
            };
            if let Some(value) = self.ports[self.merges[sink][index]].take(entry) {
                return Ok(Pull::Item(key, index, value));
            }
            if entry == Entry::Terminal(Terminal::Dropped) {
                return Ok(Pull::Dropped(index, key));
            }
        }
    }

    pub fn finish(mut self) -> Result<(), Failure> {
        for &writer in &self.writers {
            self.ports[writer].end();
        }
        self.close(true);
        self.stages
            .iter()
            .find_map(|s| s.failure)
            .map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests;
