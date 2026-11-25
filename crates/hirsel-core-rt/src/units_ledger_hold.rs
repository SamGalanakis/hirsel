//! The ledger's hold hook, driven a step at a time (RL8): hold what is
//! published, let the oldest held arrival reach the unit, stop holding.
use super::UnitCtx;

impl UnitCtx {
    /// Test hook: when holding, what was published to the port is held.
    pub(crate) fn hold_published(&mut self) {
        if self.held.is_none() {
            return;
        }
        loop {
            let mut list = self.port.drain().peekable();
            if list.peek().is_none() {
                break;
            }
            self.intake_remaining(list);
        }
    }

    /// Test hook: the oldest held arrival, from any source, reaches the unit;
    /// false when nothing was published to it.
    pub(crate) fn reach_oldest(&mut self) -> bool {
        self.hook();
        self.hold_published();
        let Some(a) = self.held.as_mut().unwrap().pop_front() else {
            return false;
        };
        self.reached.push_back(a);
        true
    }

    /// Test hook: stop holding; every held arrival reaches the unit, in order.
    pub(crate) fn unhook(&mut self) {
        if let Some(held) = self.held.take() {
            self.reached.extend(held);
        }
    }

    /// Test hook: hooked, and an arrival the hook let reach the unit waits
    /// for a take. Unhooked units keep the port's own answer.
    pub(crate) fn has_reached(&self) -> bool {
        self.held.is_some() && !self.reached.is_empty()
    }
}
