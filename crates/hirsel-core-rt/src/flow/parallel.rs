//! The S8 flow engine uses the reference's packed rings and position rules.
use super::*;
use crate::tasks::{self, FlowWaiter, FlowWork};
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(any(test, feature = "testing"))]
use crate::schedule_script::{FlowStep, PausePoint, ScheduleScript, StepBoundary};

type Graph = Reference<Plain, Ring>;

// Yield the dispatcher to other jobs even if a producer keeps this stage ready.
const STAGE_BATCH: usize = 64;
// Give the downstream worker a group of entries without waiting for the full turn.
const HANDOFF_BATCH: usize = 8;

pub(super) struct Parallel {
    shared: Arc<Shared>,
    _charges: Vec<crate::block::Block>,
}

struct Shared {
    state: Mutex<State>,
    #[cfg(any(test, feature = "testing"))]
    script: Option<(Arc<ScheduleScript>, u64)>,
}

struct State {
    graph: Graph,
    /// The stage that produces each port, and the stage that reads it.
    writers: Vec<Option<usize>>,
    readers: Vec<Option<usize>>,
    /// Exactly one queued or executing job per stage. A prepared item
    /// awaiting credit is idle and leaves its head in place.
    busy: Vec<bool>,
    enabled: usize,
    stopping: bool,
    waiter: Option<(FlowWaiter, WaitFor)>,
}

#[derive(Clone, Copy)]
enum WaitFor {
    Credit { port: usize, key: u64 },
    Sink(usize),
    Drain,
    Stop,
}

#[derive(Default)]
struct Handoffs {
    reader: Option<usize>,
    sink_changed: bool,
}

// SAFETY: the sealed native graph has no owner charges or external objects.
// Stage callbacks own plain state and a private, bound-only Context. A
// worker exclusively owns the callback during its call. A ring loan pins
// its occupied head until that same stage takes it, and the producer writes
// only vacant payload slots. All metadata and ring credit use the flow lock.
unsafe impl Send for State {}

impl Parallel {
    pub(super) fn new(mut graph: Graph) -> Self {
        graph.sealed = true;
        let charges = std::mem::take(&mut graph.charges);
        let mut writers = vec![None; graph.ports.len()];
        let mut readers = vec![None; graph.ports.len()];
        for (index, stage) in graph.stages.iter().enumerate() {
            writers[stage.output] = Some(index);
            for &port in &graph.merges[stage.input] {
                readers[port] = Some(index);
            }
        }
        let busy = vec![false; graph.stages.len()];
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                graph,
                writers,
                readers,
                busy,
                enabled: 0,
                stopping: false,
                waiter: None,
            }),
            #[cfg(any(test, feature = "testing"))]
            script: tasks::flow_script().filter(|(script, _)| script.has_pause_hook()),
        });
        {
            let mut state = shared.state.lock().unwrap();
            for index in 0..state.busy.len() {
                shared.schedule(&mut state, Some(index));
            }
        }
        Self {
            shared,
            _charges: charges,
        }
    }

    pub(super) fn push(
        &self,
        port: Endpoint,
        key: u64,
        value: Plain,
    ) -> Result<(), PushError<Plain>> {
        let mut state = self.shared.state.lock().unwrap();
        let Ok(port) = state.graph.index(port) else {
            return Err(PushError::Foreign(value));
        };
        loop {
            break match state.graph.ports[port].push_status(key) {
                Push::Ready => {
                    let ring = &mut state.graph.ports[port];
                    let first = ring.slots.len() == 0 && ring.head_mark.is_none();
                    ring.push(key, value);
                    if first {
                        self.shared.wrote(&mut state, port);
                    }
                    Ok(())
                }
                Push::Ended => Err(PushError::Ended(value)),
                Push::Order => Err(PushError::Order(value)),
                Push::Closed => Err(PushError::Closed(value)),
                Push::Full => {
                    let Some(again) = self.shared.wait(state, WaitFor::Credit { port, key }) else {
                        return Err(PushError::Stalled(value));
                    };
                    state = again;
                    continue;
                }
            };
        }
    }

    pub(super) fn advance(&self, port: Endpoint, key: u64) -> Result<(), Foreign> {
        let mut state = self.shared.state.lock().unwrap();
        let port = state.graph.index(port)?;
        let ring = &mut state.graph.ports[port];
        if key > ring.bound && ring.terminal.is_none() {
            ring.advance(key);
            self.shared.wrote(&mut state, port);
        }
        Ok(())
    }

    pub(super) fn end(&self, port: Endpoint) -> Result<(), Foreign> {
        let mut state = self.shared.state.lock().unwrap();
        let port = state.graph.index(port)?;
        self.shared.end(&mut state, port);
        Ok(())
    }

    pub(super) fn next(
        &self,
        sink: Endpoint,
        write: impl FnOnce(&Plain),
    ) -> Result<Pull<()>, Foreign> {
        let mut state = self.shared.state.lock().unwrap();
        let sink = state.graph.index(sink)?;
        loop {
            match state.next_head(sink) {
                Head::End => return Ok(Pull::End),
                Head::Stalled => {
                    let Some(again) = self.shared.wait(state, WaitFor::Sink(sink)) else {
                        return Ok(Pull::Stalled);
                    };
                    state = again;
                }
                Head::Entry { index, key, entry } => {
                    let port = state.graph.merges[sink][index];
                    if let Some(item) = state.graph.ports[port].take(entry) {
                        write(&item);
                        self.shared.freed(&mut state, port);
                        return Ok(Pull::Item(key, index, ()));
                    }
                    return Ok(Pull::Dropped(index, key));
                }
            }
        }
    }

    pub(super) fn finish(self) -> Result<(), Failure> {
        let mut state = self.shared.state.lock().unwrap();
        for index in 0..state.graph.writers.len() {
            let port = state.graph.writers[index];
            self.shared.end(&mut state, port);
        }
        loop {
            for index in 0..state.graph.sinks.len() {
                let sink = state.graph.sinks[index];
                while let Head::Entry { index, entry, .. } = state.head(sink) {
                    let port = state.graph.merges[sink][index];
                    if state.graph.ports[port].take(entry).is_some() {
                        self.shared.freed(&mut state, port);
                    }
                }
            }
            let failure = state.graph.stages.iter().find_map(|stage| stage.failure);
            let Some(again) = self.shared.wait(state, WaitFor::Drain) else {
                return failure.map_or(Ok(()), Err);
            };
            state = again;
        }
    }
}

impl Drop for Parallel {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap();
        state.stopping = true;
        while let Some(again) = self.shared.wait(state, WaitFor::Stop) {
            state = again;
        }
    }
}

impl State {
    fn head(&self, merge: usize) -> Head {
        merge_head(&self.graph.ports, &self.graph.merges[merge])
    }

    fn next_head(&mut self, sink: usize) -> Head {
        loop {
            let head = self.head(sink);
            let Head::Entry {
                index,
                entry: entry @ (Entry::Mark | Entry::Terminal(Terminal::End)),
                ..
            } = head
            else {
                return head;
            };
            let port = self.graph.merges[sink][index];
            self.graph.ports[port].take(entry);
        }
    }

    fn step_ready(&self, stage: usize, head: Head) -> bool {
        let s = &self.graph.stages[stage];
        match head {
            Head::Stalled => false,
            Head::Entry {
                key,
                entry: Entry::Item,
                ..
            } => {
                !matches!(s.prepared, Some(Ok(Some(_))))
                    || self.graph.ports[s.output].push_status(key) != Push::Full
            }
            _ => true,
        }
    }

    fn ready_step(&self, stage: usize) -> Option<Head> {
        let s = &self.graph.stages[stage];
        if self.stopping || !s.live {
            return None;
        }
        let head = self.head(s.input);
        self.step_ready(stage, head).then_some(head)
    }

    fn enable(&mut self, stage: Option<usize>) -> Option<usize> {
        let stage = stage?;
        if self.busy[stage] || self.ready_step(stage).is_none() {
            return None;
        }
        self.busy[stage] = true;
        self.enabled += 1;
        Some(stage)
    }

    fn notify(&mut self, wrote_sink: bool) {
        let Some((_, reason)) = self.waiter else {
            return;
        };
        if self.enabled == 0
            || match reason {
                WaitFor::Credit { port, key } => {
                    self.graph.ports[port].push_status(key) != Push::Full
                }
                // Consuming a mark or End terminal frees no credit and
                // needs no owner turn. Wake for the next visible answer.
                WaitFor::Sink(sink) => wrote_sink && self.next_head(sink) != Head::Stalled,
                WaitFor::Drain => wrote_sink,
                WaitFor::Stop => false,
            }
        {
            let (waiter, _) = self.waiter.take().unwrap();
            waiter.wake();
        }
    }
}

impl Shared {
    fn schedule(self: &Arc<Self>, state: &mut State, stage: Option<usize>) {
        if let Some(stage) = state.enable(stage) {
            tasks::enqueue_flow(self.clone(), stage);
        }
    }

    fn publish(self: &Arc<Self>, state: &mut State, handoffs: &mut Handoffs) {
        if let Some(reader) = handoffs.reader.take() {
            tasks::enqueue_flow(self.clone(), reader);
        }
        state.notify(std::mem::take(&mut handoffs.sink_changed));
    }

    fn wrote(self: &Arc<Self>, state: &mut State, port: usize) {
        self.schedule(state, state.readers[port]);
    }

    fn freed(self: &Arc<Self>, state: &mut State, port: usize) {
        self.schedule(state, state.writers[port]);
    }

    fn end(self: &Arc<Self>, state: &mut State, port: usize) {
        if state.graph.ports[port].terminal.is_none() {
            state.graph.ports[port].end();
            self.wrote(state, port);
        }
    }

    fn wait<'a>(
        &'a self,
        mut state: MutexGuard<'a, State>,
        reason: WaitFor,
    ) -> Option<MutexGuard<'a, State>> {
        if state.enabled == 0 {
            return None;
        }
        // The failed operation and waiter registration share the flow lock
        // with publication, including when a wake precedes the actual park.
        state.waiter = Some((FlowWaiter::current(), reason));
        drop(state);
        #[cfg(any(test, feature = "testing"))]
        if let Some((script, domain)) = &self.script {
            script.pause(PausePoint::FlowWait { domain: *domain });
        }
        tasks::park_flow();
        Some(self.state.lock().unwrap())
    }

    #[cfg(any(test, feature = "testing"))]
    fn step_kind(state: &State, stage: usize, head: Head) -> FlowStep {
        match head {
            Head::Entry {
                entry: Entry::Mark, ..
            } => FlowStep::Mark,
            Head::Entry {
                key,
                entry: Entry::Item,
                ..
            } => {
                let s = &state.graph.stages[stage];
                if matches!(s.prepared, Some(Ok(Some(_))))
                    && state.graph.ports[s.output].push_status(key) == Push::Closed
                {
                    FlowStep::Closed
                } else {
                    FlowStep::Item
                }
            }
            _ => FlowStep::Terminal,
        }
    }
    fn run_step<'a>(
        self: &'a Arc<Self>,
        mut state: MutexGuard<'a, State>,
        stage: usize,
        head: Head,
        handoffs: &mut Handoffs,
    ) -> MutexGuard<'a, State> {
        let s = &state.graph.stages[stage];
        if let Head::Entry {
            index,
            key,
            entry: Entry::Item,
        } = head
            && s.prepared.is_none()
        {
            let port = state.graph.merges[s.input][index];
            let input = state.graph.ports[port]
                .slots
                .value(|value| Plain::span(value.at, value.bytes));
            let mut body = state.graph.stages[stage].body.take().unwrap();
            drop(state);
            #[cfg(any(test, feature = "testing"))]
            if let Some((script, domain)) = &self.script {
                script.pause(PausePoint::FlowCall {
                    domain: *domain,
                    stage,
                });
            }
            let value = tasks::call_flow(|| body(key, &input));
            state = self.state.lock().unwrap();
            let s = &mut state.graph.stages[stage];
            s.body = Some(body);
            s.prepared = Some(value);
            if state.stopping || !state.step_ready(stage, head) {
                return state;
            }
        }
        #[cfg(any(test, feature = "testing"))]
        let step = Self::step_kind(&state, stage, head);
        if let Some(port) = state.graph.take_step(stage, head) {
            self.freed(&mut state, port);
        }
        #[cfg(any(test, feature = "testing"))]
        if let Some((script, domain)) = &self.script {
            drop(state);
            script.pause(PausePoint::Flow {
                domain: *domain,
                stage,
                step,
                boundary: StepBoundary::Before,
            });
            state = self.state.lock().unwrap();
        }
        if !state.stopping {
            state.graph.publish_step(stage, head);
            let s = &state.graph.stages[stage];
            let (output, input, live) = (s.output, s.input, s.live);
            let reader = state.readers[output];
            handoffs.sink_changed |= reader.is_none();
            if handoffs.reader.is_none() {
                // Count the reader at the transfer even if its dispatch
                // waits for the end of this group of entries.
                handoffs.reader = state.enable(reader);
            }
            if !live && head != Head::End {
                for index in 0..state.graph.merges[input].len() {
                    let port = state.graph.merges[input][index];
                    self.freed(&mut state, port);
                }
            }
        }
        #[cfg(any(test, feature = "testing"))]
        if let Some((script, domain)) = &self.script {
            self.publish(&mut state, handoffs);
            drop(state);
            script.pause(PausePoint::Flow {
                domain: *domain,
                stage,
                step,
                boundary: StepBoundary::After,
            });
            state = self.state.lock().unwrap();
        }
        state
    }
}

impl FlowWork for Shared {
    fn run(self: Arc<Self>, stage: usize) {
        let mut state = self.state.lock().unwrap();
        let mut handoffs = Handoffs::default();
        'turn: for _ in 0..STAGE_BATCH / HANDOFF_BATCH {
            for _ in 0..HANDOFF_BATCH {
                let Some(head) = state.ready_step(stage) else {
                    break 'turn;
                };
                // The successor inherits this job's enabled unit under
                // the flow lock before the completed step releases it.
                state = self.run_step(state, stage, head, &mut handoffs);
            }
            self.publish(&mut state, &mut handoffs);
        }
        self.publish(&mut state, &mut handoffs);
        state.busy[stage] = false;
        self.schedule(&mut state, Some(stage));
        state.enabled -= 1;
        state.notify(false);
    }
}
