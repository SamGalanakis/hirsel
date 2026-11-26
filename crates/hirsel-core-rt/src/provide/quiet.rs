//! Cell waits at quiescence (FAIL-15). Each `clock.idle` and `provide.next`
//! wait registers under its run (tasks/quiet.rs); a quiescent run wakes only
//! its own. A cancelled wait forgets its registration, so every registered
//! token and every clock deadline is a wait still parked.
use super::{Cell, cell, lock, ready_all};
use std::{collections::BTreeMap, sync::Mutex};

/// Waits by token: their cell and the run they wait in (0 for a waiter with
/// no scheduler).
pub(super) type Waits = Mutex<BTreeMap<u64, (usize, usize)>>;
/// `clock.idle` waits anywhere in the process.
pub(super) static IDLE: Waits = Mutex::new(BTreeMap::new());
/// `next` waits anywhere in the process.
pub(super) static SERVERS: Waits = Mutex::new(BTreeMap::new());

/// Register the running task's wait `token` on cell `state`.
pub(super) fn register(waits: &Waits, token: u64, state: *mut u8) {
    lock(waits).insert(token, (state.addr(), crate::tasks::run_tag()));
}

/// Which waits a quiescence answers.
#[derive(Clone, Copy)]
pub(crate) enum Scope {
    /// Those of one run, by its tag.
    Run(usize),
    /// The one wait of an operation with no scheduler.
    Token(u64),
}

impl Scope {
    fn has(self, token: u64, run: usize) -> bool {
        match self {
            Scope::Run(r) => r == run,
            Scope::Token(t) => t == token,
        }
    }
}

/// The scope is quiescent (FAIL-15): wake its `clock.idle` waits, and if none
/// of them advances time, answer `None` to each `next` with no queued call.
/// Every mark is made before the first ready is pushed, since a ready lets
/// another unit of the run run on. Returns whether it readied one.
pub(super) fn wake_quiescent(scope: Scope) -> bool {
    let mut idle = lock(&IDLE);
    let woken: Vec<(u64, usize)> = idle
        .iter()
        .filter(|&(&token, &(_, run))| scope.has(token, run))
        .map(|(&token, &(cell, _))| (token, cell))
        .collect();
    let mut advances = false; // a woken idle wait answers a deadline
    for &(token, state) in &woken {
        idle.remove(&token);
        // SAFETY: a parked idle wait pins its run and so its cell.
        if let Cell::Clock(clock) = unsafe { cell(state as *mut u8) } {
            let mut clock = lock(clock);
            advances |= !clock.timers.is_empty();
            if let Some(entry) = clock.idle.iter_mut().find(|(t, _)| *t == token) {
                entry.1 = true;
            }
        }
    }
    drop(idle);
    let servers: Vec<(u64, usize)> = lock(&SERVERS)
        .iter()
        .filter(|&(&token, &(_, run))| !advances && scope.has(token, run))
        .map(|(&token, &(cell, _))| (token, cell))
        .collect();
    let served = servers.into_iter().filter(|&(token, state)| {
        // SAFETY: a parked `next` pins its run and so its port; no ready is
        // pushed yet, so nothing of the run runs to end it.
        let Cell::Port(port) = (unsafe { cell(state as *mut u8) }) else {
            return false;
        };
        let mut port = lock(port);
        if port.queued().is_some() {
            return false;
        }
        let entry = port.servers.iter_mut().find(|(t, _)| *t == token);
        entry.map(|e| e.1 = true).is_some()
    });
    let mut tokens: Vec<u64> = woken.into_iter().map(|(token, _)| token).collect();
    tokens.extend(served.map(|(token, _)| token));
    let any = !tokens.is_empty();
    ready_all(tokens);
    any
}

/// A wait on a clock or process cancelled while it waited: forget it.
pub(super) fn forget_wait(cell: &Cell, token: u64) {
    match cell {
        Cell::Clock(clock) => {
            let mut clock = lock(clock);
            clock.timers.retain(|&(_, t)| t != token);
            clock.wall_timers.retain(|&(_, t)| t != token);
            clock.idle.retain(|&(t, _)| t != token);
            drop(clock);
            lock(&IDLE).remove(&token);
        }
        Cell::Process(process) => lock(process).waiters.retain(|&t| t != token),
        Cell::Random(_) | Cell::Port(_) => {}
    }
}
