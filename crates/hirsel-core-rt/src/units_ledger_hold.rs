#![cfg_attr(not(any(test, feature = "testing")), allow(dead_code))]
//! The ledger's hold hook, driven a step at a time (RL8): hold what is
//! published, let the oldest held arrival reach the unit, stop holding.
//! A native law drives it through `hirsel_core_test_hold`
//! (units_lang_hold.rs); a replaying unit parks with it
//! (units_conn_replay.rs).
use super::{Body, UnitCtx};
use std::{collections::VecDeque, sync::Arc};

impl UnitCtx {
    /// Test hook: the oldest held arrival from `src` reaches the unit.
    #[cfg(test)]
    pub(crate) fn reach(&mut self, src: &str) -> Option<String> {
        if self.held.is_none() {
            self.held = Some(VecDeque::new());
        }
        // An intake can publish a seal answer to this port: drain until empty.
        loop {
            let mut list = self.port.drain().peekable();
            if list.peek().is_none() {
                break;
            }
            self.intake_remaining(list);
        }
        let h = self.held.as_mut().unwrap();
        let i = h
            .iter()
            .position(|a| a.names.as_ref().is_some_and(|n| n.src == src))?;
        let a = h.remove(i).unwrap();
        let id = a.names.as_ref().unwrap().ident.clone();
        self.reached.push_back(a);
        Some(id)
    }

    pub(crate) fn hook(&mut self) {
        self.held.get_or_insert_with(VecDeque::new);
    }

    pub(crate) fn hooked(&self) -> bool {
        self.held.is_some()
    }

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

    /// Hooked, at the owner's own release of `conn`: its retirement notice
    /// reaches the unit now, behind what had reached it already; other held
    /// arrivals stay held.
    pub(crate) fn reach_notice(&mut self, conn: usize) {
        if self.held.is_none() {
            return;
        }
        self.hold_published();
        let held = self.held.as_mut().unwrap();
        let notice = held.iter().position(
            |a| matches!(&a.body, Body::LastRelease(_, c) if Arc::as_ptr(c).addr() == conn),
        );
        debug_assert!(
            notice.is_some(),
            "the retirement notice was not published here"
        );
        if let Some(i) = notice {
            let a = held.remove(i).unwrap();
            self.reached.push_back(a);
        }
    }
}
