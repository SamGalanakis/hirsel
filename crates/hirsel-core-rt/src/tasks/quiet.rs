//! Quiescence across the units of one run (FAIL-15, core.md Cells).
//!
//! `clock.idle` answers, and an empty `provide.next` is answered `None`, once
//! every task in every unit of the run waits on something inside the run: a
//! channel, a connection between its units, or a cell. A wait on anything
//! outside it (a host provider, a real socket, a sync's ready to come) keeps
//! its unit active. The run's view counts its active members: each unit's
//! scheduler, and each helper from its spawn until its end is published. A
//! scheduler with only inside waits counts itself quiet while it idles, and
//! the member that brings the count to zero decides for the whole run: it
//! wakes the run's idle waits, or, with none to wake and no unit waiting on a
//! connection, stops the run with Deadlock. A connection wait is never called
//! a deadlock. Any ready pushed to a quiet member makes it active again before
//! its scheduler wakes, so no decision is taken past an arrival.
//!
//! A replayed helper stream is a run of its own (units_lang::replay): each
//! unit replays from its own stream, where another unit's arrivals are takes.
use super::{Readies, Scheduler};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, AtomicUsize, Ordering::SeqCst, fence},
};

pub(crate) struct RunView {
    /// Members not quiet.
    active: AtomicUsize,
    /// Quiet members with a task waiting on a connection.
    conn: AtomicUsize,
    /// One decision at a time; its readies are pushed before the next.
    deciding: Mutex<()>,
}

/// A member's state, on its `Readies`.
#[derive(Default)]
pub(crate) struct Quiet(AtomicU8);
const ACTIVE: u8 = 0;
const QUIET: u8 = 1;
const QUIET_CONN: u8 = 2;

pub(super) enum Decision {
    /// Something woke or arrived: run on.
    Run,
    Park,
    Deadlock,
}

impl RunView {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            active: AtomicUsize::new(0),
            conn: AtomicUsize::new(0),
            deciding: Mutex::new(()),
        })
    }

    fn tag(self: &Arc<Self>) -> usize {
        Arc::as_ptr(self).addr()
    }

    fn leave(&self) {
        // The last active member leaves while the rest are quiet: wake them
        // all, so the last to quiet again decides (token 0 is no operation's).
        if self.active.fetch_sub(1, SeqCst) == 1 {
            super::hirsel_core_ready(0);
        }
    }
}

/// An active member of a run that is not a scheduler: a helper from its
/// spawn until its end is published.
pub(crate) struct Member(Arc<RunView>);
impl Member {
    /// A member of the running task's run.
    pub(crate) fn of_current() -> Self {
        Self::of(current().unwrap_or_else(|| {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"UnitOutsideTask"))
        }))
    }
    pub(crate) fn of(view: Arc<RunView>) -> Self {
        view.active.fetch_add(1, SeqCst);
        Self(view)
    }
    pub(crate) fn view(&self) -> Arc<RunView> {
        self.0.clone()
    }
}
impl Drop for Member {
    fn drop(&mut self) {
        self.0.leave();
    }
}

/// The running task's run view, if a task runs.
fn current() -> Option<Arc<RunView>> {
    let mut task = super::CURRENT.get();
    if task.is_null() {
        task = super::CALLBACK_TASK.get();
    }
    // SAFETY: a running task pins its run.
    unsafe { task.as_ref().and_then(|t| (&(*t.run).readies).run.clone()) }
}

/// The run a cell wait registers under, or 0 for a scheduler-less wait.
pub(crate) fn run_tag() -> usize {
    current().map_or(0, |view| view.tag())
}

impl Readies {
    /// A scheduler's waiter in `view`, whose activity its `Member` counts:
    /// the helper's, or one its root run holds.
    pub(super) fn in_run(view: Arc<RunView>) -> Arc<Self> {
        let mut readies = Self::for_unit(crate::units::current());
        Arc::get_mut(&mut readies).expect("new waiter").run = Some(view);
        readies
    }

    /// After a ready is published: a quiet member is active again.
    #[inline]
    pub(super) fn wake_member(&self) {
        if let Some(view) = &self.run {
            fence(SeqCst);
            if self.quiet.0.load(SeqCst) != ACTIVE {
                self.unquiet(view);
            }
        }
    }

    fn unquiet(&self, view: &RunView) {
        match self.quiet.0.swap(ACTIVE, SeqCst) {
            ACTIVE => {}
            was => {
                view.active.fetch_add(1, SeqCst);
                if was == QUIET_CONN {
                    view.conn.fetch_sub(1, SeqCst);
                }
            }
        }
    }
}

impl Scheduler {
    /// Every task of this unit waits inside the run (no outside wait, no
    /// timer): count the unit quiet, and if the whole run is, decide.
    pub(super) fn quiet(&self) -> Decision {
        let readies = &self.readies;
        let Some(view) = &readies.run else {
            return Decision::Park;
        };
        let conn = self.lang_waits();
        if conn {
            view.conn.fetch_add(1, SeqCst);
        }
        readies
            .quiet
            .0
            .store(if conn { QUIET_CONN } else { QUIET }, SeqCst);
        let last = view.active.fetch_sub(1, SeqCst) == 1;
        fence(SeqCst);
        if readies.has_pending() {
            return Decision::Run;
        }
        if !last {
            return Decision::Park;
        }
        let _one = view.deciding.lock().unwrap_or_else(|e| e.into_inner());
        if readies.quiet.0.load(SeqCst) == ACTIVE || view.active.load(SeqCst) != 0 {
            return Decision::Run;
        }
        if crate::provide::quiesce(crate::provide::Scope::Run(view.tag())) {
            Decision::Run
        } else if view.conn.load(SeqCst) == 0 {
            Decision::Deadlock
        } else {
            Decision::Park
        }
    }

    /// Leaving idle: this unit is active again.
    pub(super) fn active(&self) {
        if let Some(view) = &self.readies.run {
            self.readies.unquiet(view);
        }
    }

    /// A task waits on a connection: such a wait is never a deadlock. On
    /// replay, not while the unit's next frame is a take its pump makes.
    pub(super) fn lang_waits(&self) -> bool {
        // SAFETY: the run pins its unit.
        unsafe { self.lang.as_ref() }.is_some_and(|u| u.waits > 0 && !u.take_due())
    }
}
