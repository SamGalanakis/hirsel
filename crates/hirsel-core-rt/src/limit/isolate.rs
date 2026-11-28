//! A private reservation record cannot acquire children or receive remote posts.
use super::{Capacity, Cell, Funding, Payer};
use crate::isolate::ISOLATE_CELL;

/// The isolate owns this prepaid record until it installs a replacement cell.
pub(crate) struct ReservedPayer(Payer);

impl ReservedPayer {
    pub(crate) fn into_cell(self) -> Payer {
        let payer = self.0;
        std::mem::forget(self);
        payer
    }

    pub(crate) fn refund(self, current: &Funding) {
        let child = self.0;
        let parent = current.payer.get();
        // The returning context pins its canonical limit. A forwarded parent
        // or a different owner still needs the usual handover at this position.
        let f = child.forest();
        if f.record(child).live.parent != Some(parent)
            || f.record(parent).live.owner != crate::tasks::current_id()
        {
            return; // Drop follows the live destination.
        }
        // This rank-zero record has exactly one charge, one pin and no queue.
        // A ranked union followed by release leaves the current parent ahead
        // by the charge and with the same rank as a merge with a rank-zero leaf.
        let links = f.record_mut(parent);
        links.rank = links.rank.max(1);
        links.refs -= 1;
        // SAFETY: the task owns both records and no other code can charge the cell.
        unsafe {
            (*child.local()).outstanding = 0;
            let r = &mut *parent.local();
            #[cfg(feature = "prof")]
            {
                parent.observe(r.available);
                crate::mem_prof::forward(parent.local().addr(), parent.local().addr());
            }
            r.available = r.available.wrapping_add(ISOLATE_CELL);
        }
        let mut funding = std::mem::ManuallyDrop::new(Funding {
            payer: Cell::new(child),
        });
        funding.recycle_scope(parent);
        std::mem::forget(self);
    }
}

impl Drop for ReservedPayer {
    fn drop(&mut self) {
        self.0.finish_reserved(ISOLATE_CELL);
    }
}

impl Payer {
    /// Hand a reservation's record over to the scope it leads to, returning the
    /// cell's charge with it. The record is its own root until this handover
    /// and its cell is its only charge, so the charge goes back to the record's
    /// balance and the merge hands it to the live limit together with the unused
    /// bytes (MEM-20), whichever task drops the cell: never to the inbox of the
    /// task that reserved it, which may have ended.
    pub(crate) fn finish_reserved(self, charge: u64) {
        let d = self.domain();
        let local = super::BOUND
            .get()
            .is_some_and(|binding| binding.domain == self.slot().domain)
            || d.owner == Some(super::thread());
        if local && d.handles.get() != 0 {
            self.finish_reserved_local(charge);
        } else {
            self.remote_finish_reserved(charge);
        }
    }

    pub(super) fn finish_reserved_local(self, charge: u64) {
        // SAFETY: the cell owns this unmerged record; no other handle charges it.
        unsafe {
            let r = &mut *self.local();
            debug_assert!((*self.links()).parent.is_none() && r.outstanding == charge);
            r.available += charge;
            r.outstanding = 0;
        }
        let funding = Funding {
            payer: Cell::new(self),
        };
        // The hand-over merges the record or leaves a prepaid queue pin for
        // the recipient. The reservation cell releases its own pin in either case.
        funding.end();
        funding.payer.get().forest().release(self);
        std::mem::forget(funding);
    }
}

impl Funding {
    /// Reserve an isolate cell without replacing the context's current scope.
    /// The returned payer is a child record whose charge is later installed in
    /// an obligation or refunded and merged back into this scope.
    ///
    /// Unlike `enter`, the context keeps its own pin on this scope, so the
    /// record's parent edge takes a pin of its own: the cell may outlive the
    /// scope (a caught failure's replacement cell lives until the task's
    /// Context drops), and the merge releases that edge when the cell finishes.
    pub(crate) fn isolate_reserve(&mut self) -> Result<ReservedPayer, Capacity> {
        let live = self.reserve_scope(ISOLATE_CELL)?;
        let domain = unsafe { self.payer.get().0.as_ref().domain };
        let forest = unsafe { &mut *domain.as_ref().forest.get() };
        forest.retain(self.payer.get());
        let payer = forest.take(domain, live);
        unsafe {
            let record = &mut *payer.local();
            record.available = 0;
            record.outstanding = ISOLATE_CELL;
        }
        Ok(ReservedPayer(payer))
    }
}
