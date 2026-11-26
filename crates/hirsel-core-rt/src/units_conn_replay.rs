//! A replaying unit's arrivals (docs/recording.md, "Replay of a run with
//! helpers"): every take comes from the unit's own `TAKE` frames. What is
//! published to it otherwise is parked and never taken unless a `TAKE`
//! names it; a recorded item is delivered to its own connection at its
//! `TAKE`, and a replayed send delivers nothing.
use super::super::{Body, UnitCtx};
use super::{ConnCore, Outgoing, Sent};
use crate::units_lang::CrossSend;

impl UnitCtx {
    /// From now on what is published to the unit is parked.
    pub(crate) fn park(&mut self) {
        self.hook();
        self.parking = true;
    }

    /// What was just published to the port reaches the unit, intaken.
    fn admit_published(&mut self) {
        for a in self.port.drain() {
            let a = self.intake_one(a);
            self.reached.push_back(a);
        }
    }

    /// The first parked notice `body` matches reaches the unit; false when
    /// none was published to it.
    pub(crate) fn admit_parked(&mut self, matches: impl Fn(&Body) -> bool) -> bool {
        self.hold_published();
        let Some(held) = self.held.as_mut() else {
            return false;
        };
        let Some(i) = held.iter().position(|a| matches(&a.body)) else {
            return false;
        };
        let a = held.remove(i).unwrap();
        self.reached.push_back(a);
        true
    }

    /// The end of the helper whose context is `helper` reaches the unit.
    pub(crate) fn admit_end(&mut self, helper: usize) -> bool {
        self.admit_parked(|b| matches!(b, Body::HelperEnd(e) if e.0.as_ptr().addr() == helper))
    }

    /// A recorded item of `bytes` reaches the unit on its own connection `c`,
    /// copied from `source`; false when the connection cannot admit it.
    /// # Safety
    /// `source` holds an item laid out as `c`'s items are.
    pub(crate) unsafe fn replay_item(
        &mut self,
        c: &ConnCore,
        source: CrossSend,
        bytes: u64,
    ) -> bool {
        self.hold_published();
        let item = Outgoing::Cross(source);
        let sent = match c.admit_fast(bytes) {
            Ok(w) => {
                Self::publish_item(c, w, item, bytes);
                Sent::Accepted
            }
            Err(_) => self.issue_slow(c, item, bytes, false),
        };
        if !matches!(sent, Sent::Accepted) {
            return false;
        }
        self.admit_published();
        true
    }
}
