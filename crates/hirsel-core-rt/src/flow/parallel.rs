//! The S8 flow engine uses the reference's packed rings and position rules.
use super::*;
use crate::tasks::{self, FlowWaiter, FlowWork};
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(any(test, feature = "testing"))]
use crate::schedule_script::{FlowStep, PausePoint, ScheduleScript, StepBoundary};

type Graph = Reference<Plain, Ring>;

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
    /// Exactly one queued or executing step for a stage at a time. A
    /// prepared item awaiting credit is idle and leaves its head in place.
    busy: Vec<bool>,
    enabled: usize,
    stopping: bool,
    waiter: Option<FlowWaiter>,
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
                    let Some(again) = self.shared.wait(state) else {
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
            match state.head(sink) {
                Head::End => return Ok(Pull::End),
                Head::Stalled => {
                    let Some(again) = self.shared.wait(state) else {
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
                    if entry == Entry::Terminal(Terminal::Dropped) {
                        return Ok(Pull::Dropped(index, key));
                    }
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
            let Some(again) = self.shared.wait(state) else {
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
        while let Some(again) = self.shared.wait(state) {
            state = again;
        }
    }
}

impl State {
    fn head(&self, merge: usize) -> Head {
        merge_head(&self.graph.ports, &self.graph.merges[merge])
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

    fn notify(&mut self) {
        if let Some(waiter) = self.waiter.take() {
            waiter.wake();
        }
    }
}

impl Shared {
    fn schedule(self: &Arc<Self>, state: &mut State, stage: Option<usize>) {
        let Some(stage) = stage else { return };
        let s = &state.graph.stages[stage];
        if !state.stopping
            && !state.busy[stage]
            && s.live
            && state.step_ready(stage, state.head(s.input))
        {
            state.busy[stage] = true;
            state.enabled += 1;
            tasks::enqueue_flow(self.clone(), stage);
        }
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

    fn wait<'a>(&'a self, mut state: MutexGuard<'a, State>) -> Option<MutexGuard<'a, State>> {
        if state.enabled == 0 {
            return None;
        }
        state.waiter = Some(FlowWaiter::current());
        drop(state);
        #[cfg(any(test, feature = "testing"))]
        if let Some((script, domain)) = &self.script {
            script.pause(PausePoint::FlowWait { domain: *domain });
        }
        tasks::park_flow();
        Some(self.state.lock().unwrap())
    }

    fn retire(self: &Arc<Self>, state: &mut State, stage: usize) {
        state.busy[stage] = false;
        self.schedule(state, Some(stage));
        state.enabled -= 1;
        state.notify();
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
}

impl FlowWork for Shared {
    fn run(self: Arc<Self>, stage: usize) {
        let mut state = self.state.lock().unwrap();
        if state.stopping {
            self.retire(&mut state, stage);
            return;
        }
        let s = &state.graph.stages[stage];
        let head = state.head(s.input);
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
        }
        if state.stopping || !state.step_ready(stage, head) {
            self.retire(&mut state, stage);
            return;
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
            self.wrote(&mut state, output);
            if !live && head != Head::End {
                for index in 0..state.graph.merges[input].len() {
                    let port = state.graph.merges[input][index];
                    self.freed(&mut state, port);
                }
            }
        }
        #[cfg(any(test, feature = "testing"))]
        if let Some((script, domain)) = &self.script {
            drop(state);
            script.pause(PausePoint::Flow {
                domain: *domain,
                stage,
                step,
                boundary: StepBoundary::After,
            });
            state = self.state.lock().unwrap();
        }
        self.retire(&mut state, stage);
    }
}
