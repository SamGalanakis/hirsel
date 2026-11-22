//! Generative choices for tests. The dispatcher still records ordinary commits.
use hirsel_core_net::SplitMix64;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

/// Domain of the fixture's untaped tasks. Tape groups are numbered from zero.
pub const UNTAPED: u64 = u64::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowMode {
    Eager,
    Lazy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowStep {
    Item,
    Mark,
    Terminal,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepBoundary {
    Before,
    After,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PausePoint {
    /// Called after the receiver takes its in-flight unit, before the sender retires.
    Handoff {
        sender_domain: u64,
        receiver_domain: u64,
    },
    /// A driver has captured this operation's arm and has yet to apply it.
    ReadinessTaken { token: u64 },
    /// An idle worker has made its last checks for work and is about to
    /// wait in the run's poller or park its thread.
    IdleWait,
    /// The stage has reserved the item but has yet to enter its transform.
    FlowCall { domain: u64, stage: usize },
    /// A flow operation has registered its owner before yielding the worker.
    FlowWait { domain: u64 },
    /// A mark or terminal forwarded by an empty stage has these boundaries too.
    Flow {
        domain: u64,
        stage: usize,
        step: FlowStep,
        boundary: StepBoundary,
    },
}

struct Choices {
    prefix: Vec<usize>,
    random: SplitMix64,
    decisions: Vec<(usize, usize)>,
}
impl Choices {
    fn new(seed: u64, domain: u64) -> Self {
        Self {
            prefix: Vec::new(),
            random: SplitMix64::new(seed, domain),
            decisions: Vec::new(),
        }
    }

    fn next(&mut self, alternatives: usize, draw: impl FnOnce(&mut SplitMix64) -> usize) -> usize {
        let index = self
            .prefix
            .get(self.decisions.len())
            .copied()
            .unwrap_or_else(|| draw(&mut self.random));
        assert!(index < alternatives, "script index outside the choice set");
        self.decisions.push((alternatives, index));
        index
    }
}

pub struct ScheduleScript {
    seed: u64,
    prefer_last: bool,
    schedule: Mutex<BTreeMap<u64, Choices>>,
    flow: Mutex<BTreeMap<u64, Choices>>,
    flow_mode: FlowMode,
    pause: Option<Box<dyn Fn(PausePoint) + Send + Sync>>,
}
impl ScheduleScript {
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            prefer_last: false,
            schedule: Mutex::default(),
            flow: Mutex::default(),
            flow_mode: FlowMode::Eager,
            pause: None,
        }
    }

    /// Prefer the last runnable task at each dispatch.
    pub fn last() -> Self {
        Self {
            prefer_last: true,
            ..Self::new(0)
        }
    }

    /// Task indices use the tape's live, spawn-ordered group; each scripted
    /// choice must name a runnable member. Untaped fixtures use queue order.
    /// Forced dispatches do not consume a prefix entry.
    pub fn with_schedule_prefix(mut self, domain: u64, prefix: Vec<usize>) -> Self {
        let mut choices = Choices::new(self.seed, domain);
        choices.prefix = prefix;
        self.schedule.get_mut().unwrap().insert(domain, choices);
        self
    }

    pub fn with_flow_prefix(mut self, domain: u64, prefix: Vec<usize>) -> Self {
        let mut choices = Choices::new(self.flow_seed(), domain);
        choices.prefix = prefix;
        self.flow.get_mut().unwrap().insert(domain, choices);
        self
    }

    pub fn with_flow_mode(mut self, mode: FlowMode) -> Self {
        self.flow_mode = mode;
        self
    }

    pub fn flow_mode(&self) -> FlowMode {
        self.flow_mode
    }

    pub fn with_pause_hook(mut self, hook: impl Fn(PausePoint) + Send + Sync + 'static) -> Self {
        self.pause = Some(Box::new(hook));
        self
    }

    pub(crate) fn has_pause_hook(&self) -> bool {
        self.pause.is_some()
    }

    /// Call at the chosen boundary, outside the dispatcher lock.
    pub fn pause(&self, point: PausePoint) {
        if let Some(hook) = &self.pause {
            hook(point);
        }
    }

    pub(crate) fn choose_task(&self, domain: u64, live: usize, runnable: &[usize]) -> usize {
        if runnable.len() == 1 {
            return runnable[0];
        }
        let index = self
            .schedule
            .lock()
            .unwrap()
            .entry(domain)
            .or_insert_with(|| Choices::new(self.seed, domain))
            .next(live, |rng| {
                let position = if self.prefer_last {
                    runnable.len() - 1
                } else {
                    rng.choose(runnable.len())
                };
                runnable[position]
            });
        assert!(
            runnable.contains(&index),
            "script index names a parked task"
        );
        index
    }

    pub fn schedule_decisions(&self, domain: u64) -> Vec<(usize, usize)> {
        self.schedule
            .lock()
            .unwrap()
            .get(&domain)
            .map_or_else(Vec::new, |choices| choices.decisions.clone())
    }

    /// Index into the reference's enabled steps, presented in stage order.
    pub fn choose_flow_step(&self, domain: u64, enabled: usize) -> usize {
        assert_ne!(enabled, 0);
        self.flow
            .lock()
            .unwrap()
            .entry(domain)
            .or_insert_with(|| Choices::new(self.flow_seed(), domain))
            .next(enabled, |rng| rng.choose(enabled))
    }

    pub fn flow_decisions(&self, domain: u64) -> Vec<(usize, usize)> {
        self.flow
            .lock()
            .unwrap()
            .get(&domain)
            .map_or_else(Vec::new, |choices| choices.decisions.clone())
    }

    fn flow_seed(&self) -> u64 {
        self.seed ^ 0x9e3779b97f4a7c15
    }

    /// Install for a synchronous runtime entry. The dispatcher takes a clone
    /// for the lifetime of the run; the hook can be shared with worker threads.
    pub fn install<R>(self: &Arc<Self>, run: impl FnOnce() -> R) -> R {
        struct Restore(Option<Arc<ScheduleScript>>);
        impl Drop for Restore {
            fn drop(&mut self) {
                INSTALLED.with(|installed| installed.replace(self.0.take()));
            }
        }
        let _restore = Restore(INSTALLED.with(|installed| installed.replace(Some(self.clone()))));
        run()
    }
}

thread_local! {
    static INSTALLED: RefCell<Option<Arc<ScheduleScript>>> = const { RefCell::new(None) };
}

pub(crate) fn installed() -> Option<Arc<ScheduleScript>> {
    INSTALLED.with(|installed| installed.borrow().clone())
}
