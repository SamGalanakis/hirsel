//! MEM-30 step accounts (budgets.md, tasks.md CONC-10/15).
//!
//! `bound steps n` opens an account funded with n from the current one. A
//! same-unit spawn makes the child a participant of the spawner's innermost
//! account by identity: no fuel moves, and every participant's charged step
//! takes from the one balance. One task runs at a time here, so a charge
//! debits the account directly; native lends a window per metered turn and
//! returns it at every switch, which is the same ledger.
//!
//! An account closes when its last interest goes: the entering task's lexical
//! exit, each participant's end, each nested account's close and each helper
//! grant's settlement. On close its unused credit goes once to the account
//! that funded it. A helper (`spawn_unit`) gets a private root holding
//! floor(R / 2) of the donor's available R; its residual waits in `residuals`
//! until the owner consumes or abandons the handle, then goes to the donor.
//!
//! The first refused step latches the account exhausted. Late credit still
//! accumulates for settlement at close, but cannot fund another step.

use super::*;

#[derive(Default)]
pub(super) struct StepBook {
    next: u64,
    open: HashMap<u64, Account>,
    /// A closed helper root's unused credit, by its id, until the owner settles.
    residuals: HashMap<u64, u64>,
}

struct Account {
    left: u64,
    /// The account this one returns its credit to: a nested bound's funder.
    /// A helper's root has none; its owner settles it.
    parent: Option<u64>,
    interest: u32,
    exhausted: bool,
}

impl Account {
    fn available(&self) -> u64 {
        if self.exhausted { 0 } else { self.left }
    }
}

impl<'m, H: Host<'m>> Machine<'m, H> {
    /// Open an account with the given id (a run's root) or a fresh one.
    pub(super) fn open_steps(&mut self, id: Option<u64>, left: u64, parent: Option<u64>) -> u64 {
        let book = &mut self.step_book;
        let id = id.unwrap_or_else(|| {
            book.next += 1;
            book.next
        });
        let account = Account {
            left,
            parent,
            interest: 1,
            exhausted: false,
        };
        assert!(
            book.open.insert(id, account).is_none(),
            "a step account reopened"
        );
        id
    }

    fn account(&mut self, id: u64) -> &mut Account {
        self.step_book
            .open
            .get_mut(&id)
            .expect("an open step account")
    }

    /// Drop one interest in `id`; the last closes it and returns its credit.
    pub(super) fn release_steps(&mut self, mut id: u64) {
        loop {
            let account = self.account(id);
            account.interest -= 1;
            if account.interest != 0 {
                return;
            }
            let account = self.step_book.open.remove(&id).unwrap();
            let Some(parent) = account.parent else {
                self.step_book.residuals.insert(id, account.left);
                return;
            };
            self.account(parent).left += account.left;
            id = parent;
        }
    }

    pub(super) fn take_steps(&mut self, t: &mut Task, mut count: u64) -> X<()> {
        while count != 0 {
            let Some(&StepBound { id }) = t.step_bounds.last() else {
                self.steps = self.steps.saturating_add(count);
                break;
            };
            let account = self.account(id);
            if account.available() == 0 {
                account.exhausted = true;
                pure::root_steps_out(id);
                let site = self.site(t);
                if self.observe_steps && self.step_refusal.is_none() {
                    self.step_refusal = site;
                }
                self.record(
                    t,
                    Cause {
                        kind: "OutOfTime",
                        message: [id, 0, 0],
                        site,
                    },
                );
                return Err(Stop::Chained);
            }
            let amount = count.min(account.left).min(65_536 - t.slice_steps);
            account.left -= amount;
            count -= amount;
            t.slice_steps += amount;
            self.steps = self.steps.saturating_add(amount);
            if t.slice_steps == 65_536 {
                t.slice_steps = 0;
                H::slice_end(self);
            }
        }
        Ok(())
    }

    /// The innermost account's balance; a work charge reads it around itself.
    pub(super) fn steps_left(&mut self, t: &Task) -> Option<u64> {
        let &StepBound { id } = t.step_bounds.last()?;
        Some(self.account(id).left)
    }

    /// `bound steps n`: false, touching nothing, when the current account
    /// cannot fund n.
    pub(super) fn step_enter(&mut self, t: &mut Task, n: u64) -> bool {
        let n = self.step_injection.take().unwrap_or(n);
        let parent = t.step_bounds.last().map(|b| b.id);
        if let Some(p) = parent {
            let account = self.account(p);
            if account.available() < n {
                pure::root_steps_out(p);
                if self.observe_steps && self.step_refusal.is_none() {
                    self.step_refusal = self.site(t);
                }
                return false;
            }
            account.left -= n;
            account.interest += 1;
        }
        let id = self.open_steps(None, n, parent);
        t.step_bounds.push(StepBound { id });
        true
    }

    /// The lexical exit of the innermost bound: the entering task's interest.
    pub(super) fn step_leave(&mut self, t: &mut Task) -> u64 {
        let StepBound { id } = t.step_bounds.pop().expect("an open step scope");
        self.release_steps(id);
        id
    }

    /// A same-unit child's bounds: the spawner's innermost account, shared.
    pub(super) fn share_steps(&mut self, t: &Task) -> Vec<StepBound> {
        let Some(&StepBound { id }) = t.step_bounds.last() else {
            return Vec::new();
        };
        self.account(id).interest += 1;
        vec![StepBound { id }]
    }

    /// A helper's private root: floor(R / 2) of the donor's available R, the
    /// donor kept open by the handle. Returns the child's bounds and the
    /// (donor, grant account) pair its handle settles.
    pub(super) fn grant_steps(&mut self, t: &Task) -> (Vec<StepBound>, Option<(u64, u64)>) {
        let Some(&StepBound { id: donor }) = t.step_bounds.last() else {
            return (Vec::new(), None);
        };
        let account = self.account(donor);
        let grant = account.available() / 2;
        account.left -= grant;
        account.interest += 1;
        let id = self.open_steps(None, grant, None);
        (vec![StepBound { id }], Some((donor, id)))
    }

    /// The owner consumed or abandoned a helper's handle: credit the helper's
    /// residual to its donor, once, and drop the handle's interest.
    pub(super) fn settle_steps(&mut self, (donor, grant): (u64, u64)) {
        let residual = self
            .step_book
            .residuals
            .remove(&grant)
            .expect("a helper's step grant settled before it closed");
        self.account(donor).left += residual;
        self.release_steps(donor);
    }

    /// A task ended: its interest in each account still on its stack.
    pub(super) fn end_steps(&mut self, t: &mut Task) {
        while !t.step_bounds.is_empty() {
            self.step_leave(t);
        }
    }
}
