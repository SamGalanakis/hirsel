//! Represented calls and what they keep in the root's frame record.

use super::{leaf, receive};
use crate::{Body, Callee, FuncId, Inst, RtOp, Wait};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Site {
    /// A wait made by the runtime: the returnable op the call becomes.
    Leaf(RtOp),
    Callee(FuncId),
    /// A tail call of `g` that hands the task to `g`'s own root
    /// (`RtOp::Transfer`): the caller's frames return at once, and the record
    /// they held is freed before `g` runs. Declined (stackfully, in replay) it
    /// is a plain call of `g`.
    Transfer(FuncId),
}

/// The largest wait state that may be held outside the stack in this record.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum HeldWait {
    None,
    Send,
    Net,
}

/// Which calls of a body are sites: callees that can be represented (`ok`),
/// those that reach an owned wait (`owned`), and the calls that transfer.
#[derive(Clone, Copy)]
pub(super) struct Calling<'a> {
    pub(super) ok: &'a [bool],
    pub(super) owned: &'a [bool],
    pub(super) transfers: &'a BTreeSet<usize>,
}

pub(super) fn sites_of(body: &Body, calling: Calling<'_>, receives: bool) -> BTreeMap<usize, Site> {
    let Calling {
        ok,
        owned,
        transfers,
    } = calling;
    let mut sites = BTreeMap::new();
    for (k, inst) in body.insts.iter().enumerate() {
        if let Inst::Call { call } = *inst {
            let c = &body.calls[call.index()];
            if let Some(op) = leaf(c.callee) {
                sites.insert(k, Site::Leaf(RtOp::Returnable(op)));
            } else if let Some(op) = receive(c.callee)
                && receives
            {
                sites.insert(k, Site::Leaf(op));
            } else if let Callee::Direct(g) = c.callee
                && ok[g.index()]
                && (receives || owned[g.index()])
            {
                sites.insert(
                    k,
                    if transfers.contains(&k) {
                        Site::Transfer(g)
                    } else {
                        Site::Callee(g)
                    },
                );
            }
        }
    }
    sites
}

/// The largest child area and held-wait state used by these sites.
pub(super) fn storage<'a>(
    sites: impl Iterator<Item = &'a Site>,
    callee: &dyn Fn(FuncId) -> (u64, HeldWait),
) -> (u64, HeldWait) {
    let mut child = 8;
    let mut held = HeldWait::None;
    for site in sites {
        let wait = match *site {
            Site::Leaf(RtOp::Returnable(Wait::Host(_))) => HeldWait::Net,
            Site::Leaf(RtOp::Returnable(Wait::ConnSend { .. })) => HeldWait::Send,
            Site::Callee(g) => {
                let (area, held) = callee(g);
                child = child.max(area);
                held
            }
            // A transfer target makes its own record after this one is freed.
            Site::Leaf(_) | Site::Transfer(_) => HeldWait::None,
        };
        held = held.max(wait);
    }
    (child, held)
}
