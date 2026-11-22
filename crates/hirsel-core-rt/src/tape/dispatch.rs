//! Link boundary between the common provider/scheduler and an open tape.
use super::{Cell, Fault, cell_of};
use crate::provider::{Provider, Request, Step};
use std::ptr;

type Deadlock = ([u8; 16], Fault);

/// Installed by the tape constructor. The scheduler carries the same cell and
/// generation as the provider, so it needs neither a global mode nor a lookup.
pub(super) trait Hooks: Sync {
    fn seal_deadlock(&self, group: Group) -> Result<(), Deadlock>;
    fn check(&self, group: Group) -> Result<(), Fault>;
    fn cancel_wait(&self, group: Group, request: *mut Request);
    fn child(&self, group: Group) -> Group;
    fn event(&self, group: Group, index: &dyn Fn() -> u64, ended: bool);
    fn turn(&self, group: Group) -> Turn;
    fn choose(&self, group: Group, task: u64);
    fn replaying(&self, group: Group) -> bool;
    fn fail(&self, group: Group, ready: Option<usize>);
    fn wake(&self, group: Group, request: *mut Request);
    fn cancel(&self, group: Group, source: Option<Group>) -> bool;
    fn consume_input(&self, group: Group);
    fn source(&self, cell: &Cell, handle: u64) -> Option<(Provider, u64)>;
    fn net_rules(&self, cell: &Cell, handle: u64) -> u64;
    fn exec_rules(
        &self,
        cell: &Cell,
        key: u64,
    ) -> Option<std::sync::Arc<hirsel_core_net::exec::ExecRules>>;
    fn narrow_exec(
        &self,
        cell: &'static Cell,
        parent: u64,
        inner: Option<u64>,
        rules: hirsel_core_net::exec::ExecRules,
    ) -> u64;
    fn narrow_net(&self, cell: &'static Cell, parent: u64, rules: u64) -> (u64, Group);
    unsafe fn issue(&self, cell: &Cell, request: *mut Request, token: u64) -> Step;
}

#[derive(Clone, Copy)]
pub(crate) struct Group {
    pub(super) cell: &'static Cell,
    pub(super) generation: u32,
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

    pub fn drives_replay(self, provider: Provider) -> bool {
        provider.state == self.cell.provider.state && self.replaying()
    }

    pub fn seal_deadlock(self) -> Result<(), Deadlock> {
        self.cell.hooks.seal_deadlock(self)
    }
    pub fn check(self) -> Result<(), Fault> {
        self.cell.hooks.check(self)
    }
    pub fn cancel_wait(self, request: *mut Request) {
        self.cell.hooks.cancel_wait(self, request);
    }
    pub fn child(self) -> Self {
        self.cell.hooks.child(self)
    }
    pub fn event(self, index: impl Fn() -> u64, ended: bool) {
        self.cell.hooks.event(self, &index, ended);
    }
    pub fn turn(self) -> Turn {
        self.cell.hooks.turn(self)
    }
    pub fn choose(self, task: u64) {
        self.cell.hooks.choose(self, task);
    }
    pub fn replaying(self) -> bool {
        self.cell.hooks.replaying(self)
    }
    pub fn fail(self) {
        self.cell.hooks.fail(self, None);
    }
    pub fn fail_choice(self, ready: usize) {
        self.cell.hooks.fail(self, Some(ready));
    }
    pub fn wake(self, request: *mut Request) {
        self.cell.hooks.wake(self, request);
    }
    pub fn cancel(self, source: Option<Self>) -> bool {
        self.cell.hooks.cancel(self, source)
    }
    pub fn consume_input(self) {
        self.cell.hooks.consume_input(self);
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
    Deadlock,
    Wake(u64),
    Cancel(u64),
    Done(u64),
}

const ISSUE: unsafe extern "C" fn(*mut u8, *mut Request, u64) -> Step = issue;

pub(crate) fn owns(provider: &Provider) -> bool {
    ptr::fn_addr_eq(provider.issue, ISSUE)
}

/// The source of a substituted capability, present only during recording.
pub(crate) fn source(provider: &Provider, handle: u64) -> Option<(Provider, u64)> {
    let cell = cell_of(provider.state);
    cell.hooks.source(cell, handle)
}

/// The source rules in Record or the inert snapshot (or its intersection)
/// in Replay. A CLI Net grant stays live through the run's cleanup.
pub(crate) fn net_rules(provider: &Provider, handle: u64) -> u64 {
    let cell = cell_of(provider.state);
    cell.hooks.net_rules(cell, handle)
}

pub(crate) fn exec_rules(
    provider: &Provider,
    handle: u64,
) -> Option<std::sync::Arc<hirsel_core_net::exec::ExecRules>> {
    let cell = cell_of(provider.state);
    cell.hooks.exec_rules(cell, handle)
}

pub(crate) fn narrow_exec(
    provider: &Provider,
    parent: u64,
    inner: Option<u64>,
    rules: hirsel_core_net::exec::ExecRules,
) -> u64 {
    let cell = cell_of(provider.state);
    cell.hooks.narrow_exec(cell, parent, inner, rules)
}

/// Successful local narrowing assigns the child ordinal in commit order.
pub(crate) fn narrow_net(provider: &Provider, parent: u64, rules: u64) -> (u64, Group) {
    let cell = cell_of(provider.state);
    cell.hooks.narrow_net(cell, parent, rules)
}

/// Also identifies tape providers without retaining their issue implementation.
#[inline(never)]
pub(super) unsafe extern "C" fn issue(state: *mut u8, request: *mut Request, token: u64) -> Step {
    let cell = cell_of(state);
    // SAFETY: the provider driver pins the request for the duration of the call.
    unsafe { cell.hooks.issue(cell, request, token) }
}
