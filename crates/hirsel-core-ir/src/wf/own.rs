//! Ownership of slot contents: no place is dropped twice (R1) or used after its
//! drop (R2) on any path, and no `view` place is dropped (R3).
//!
//! A place is a root (one of the body's slots, or an entry parameter's place) and
//! its static steps, as [`place_of`] cuts them. Every address and pointer value
//! names the places it may point at: a `pointer` the places of its address, a
//! block parameter the union of what its incoming edges carry (a fixpoint). A
//! pointer from anywhere else (an `alloc`, a load, a call's result) names nothing
//! this pass checks.
//!
//! A pointer block parameter whose edges carry more than one place is a join: it
//! gets a root of its own, the place it points at on the path taken. Each edge sets
//! that root's state from the place it carries, a drop through the join drops it
//! (and may drop each place it stands for), and a drop of one of those may drop it.
//!
//! A call whose signature marks its `dest` a view anchored on parameters (OWN-20)
//! makes the destination's places borrow the anchor arguments' places, from the
//! call until the destination is overwritten. Dropping or rewriting an anchor
//! invalidates the view, including views borrowed from it.
//!
//! For each place a join may stand for, and each of the place's pairings (below),
//! the state also holds where the join may be bound to it: an edge binds the join to
//! the places it carries and unbinds it from the rest, so a drop through the join may
//! drop the place only where the join may be it. A place whose drop flag the path
//! that bound the join to it cleared is then not dropped again under that flag.
//!
//! A forward may-analysis then tracks, for each place some `drop` may reach,
//! whether it may be dropped. A `drop` through one place drops it; through several,
//! it may drop each. A covering write, and its slot's `live`, begin a place anew
//! ([`written`] requires the bytes a drop reads after a `live`); its slot's `dead`
//! leaves it dropped, so nothing but the next `live` revives it. `move` (FORM #112)
//! is a memmove of plain bytes: an operand that may own is refused, so no value
//! changes owner through one.
//!
//! A drop flag (a whole `bool` slot, as [`written`] reads one) guards the drops on
//! the `yes` edge of a branch on a read of it. For each place and each flag guarding
//! one of its drops, the state is the set of reachable (dropped, flag) pairs: a store
//! of the flag sets its half, a branch on it keeps the pairs of its edge. A place
//! may be dropped only where every one of its pairings says so, so a drop under a
//! flag cleared by the move that consumed the place is not a second drop.
//!
//! A drop or use of a place overlapping (one's steps a prefix of the other's) a
//! place that may be dropped is an error naming both.

use super::own_names::{
    Layouts, Names, Pl, check_call, check_moves, covers, local_owned_slots, names, overlap,
    ptr_args,
};
pub(super) use super::own_names::{anchors, is_flag_slot};
use super::*;
use crate::decode::Debit;

const ZT: u8 = 1;
const ZF: u8 = 2;
const DT: u8 = 4;
const DF: u8 = 8;
pub(super) const T: u8 = ZT | DT;
pub(super) const F: u8 = ZF | DF;
pub(super) const D: u8 = DT | DF;
pub(super) const Z: u8 = ZT | ZF;

pub(super) fn halves(s: u8) -> (bool, bool) {
    (s & T != 0, s & F != 0)
}
pub(super) fn dropped(s: u8, strong: bool) -> u8 {
    let (t, f) = halves(s);
    let d = (if t { DT } else { 0 }) | (if f { DF } else { 0 });
    if strong { d } else { s | d }
}
pub(super) fn begun(s: u8) -> u8 {
    let (t, f) = halves(s);
    (if t { ZT } else { 0 }) | (if f { ZF } else { 0 })
}
fn flag(s: u8, value: Option<bool>) -> u8 {
    let (z, d) = (s & Z != 0, s & D != 0);
    let (zs, ds) = match value {
        Some(true) => (ZT, DT),
        Some(false) => (ZF, DF),
        None => (Z, D),
    };
    (if z { zs } else { 0 }) | (if d { ds } else { 0 })
}

/// What an instruction does to the tracked places, by address.
pub(super) enum Act {
    Use(AddressId),
    Drop(AddressId),
    Write(AddressId),
    Invalidate(AddressId),
    /// A copy/move into `dest` from `from`: propagates an active local borrow.
    CopyToDest(AddressId),
    /// `StorageLive`: the slot's places begin anew ([`written`] requires their bytes).
    Begin(usize),
    /// `StorageDead`: the slot's places are gone until its next `StorageLive`.
    Dead(usize),
    Flag(usize, Option<bool>),
    /// A branch-used read of a drop flag is fresh right after this `Read`.
    Fresh(usize),
    /// A call has begun borrow `k`: its dest now views its anchors.
    Lend(usize),
}

pub(super) struct Own<'a> {
    pub(super) b: &'a Body,
    pub(super) n: Names,
    /// What addresses reached through a join name, by the join's root.
    j: Names,
    /// Per address: whether it reaches its places through a join.
    routed: Vec<bool>,
    pub(super) slots: usize,
    /// The first join root: the slots, then the entry values.
    pub(super) roots: usize,
    /// Whether this function's signature has a `dest` (entry parameter 0, root `slots`).
    pub(super) has_dest: bool,
    /// Per join: its parameter's value index.
    pub(super) joins: Vec<usize>,
    /// Entry values that are `view` parameters' pointers, by entry index.
    views: Vec<bool>,
    pub(super) tracked: Vec<Pl>,
    /// Per address: the tracked places it overlaps.
    hits: Vec<Vec<usize>>,
    /// Per address: the tracked places its deref steps read through.
    deref_hits: Vec<Vec<usize>>,
    /// Per tracked place: its pairings (indices into the state).
    pub(super) pairs: Vec<Vec<usize>>,
    /// Per tracked place: the tracked join places that may be it, in part.
    shadow: Vec<Vec<usize>>,
    /// Per tracked place: (join place, pairing, state index) for each join in its
    /// shadow and each of its pairings, the state saying where the join may be bound
    /// to it (`D`) or not (`Z`), in that pairing's flag halves.
    pub(super) binds: Vec<Vec<(usize, usize, usize)>>,
    /// Per borrow (a call with a dest view): its dest's tracked places.
    borrows: Vec<Vec<usize>>,
    /// Per borrow: whether its anchors include a local slot, and whether its dest includes `dest`.
    borrow_from_local: Vec<bool>,
    borrow_to_dest: Vec<bool>,
    /// Per local slot: borrows anchored on it (invalidated on `StorageLive` / `StorageDead`).
    anchor_slots: Vec<Vec<usize>>,
    /// Per address: the borrows a write through it rewrites an anchor of.
    rewrites: Vec<Vec<usize>>,
    /// Per call: its borrow.
    lends: std::collections::HashMap<usize, usize>,
    /// Per tracked place: the borrows anchored on it, and those whose dest it is.
    pub(super) lent: Vec<Vec<usize>>,
    held: Vec<Vec<usize>>,
    /// The state index of borrow 0's bit: whether it may be in force.
    pub(super) base: usize,
    /// The state index of flag-read 0's stale bit (`0` = fresh, `1` = may be stale).
    pub(super) stale_base: usize,
    /// The state index of the `dest` local-borrow escape bit.
    pub(super) dest_escaped: usize,
    /// Per flag slot: the pairings it guards.
    pub(super) guards: std::collections::HashMap<usize, Vec<usize>>,
    /// Branch-used read results of whole unescaped flag slots.
    pub(super) flag_of: std::collections::HashMap<usize, usize>,
    /// Branch-used flag read value index -> index in `0..flag_reads`.
    pub(super) read_idx: std::collections::HashMap<usize, usize>,
    /// Per flag slot: indices in `0..flag_reads` that read it.
    reads_of_flag: std::collections::HashMap<usize, Vec<usize>>,
    consts: std::collections::HashMap<usize, u64>,
}

impl Own<'_> {
    fn show(&self, p: &Pl) -> String {
        let mut s = if p.root < self.slots {
            format!("s{}", p.root)
        } else if p.root < self.roots {
            format!("parameter {}", p.root - self.slots)
        } else {
            format!("*v{}", self.joins[p.root - self.roots])
        };
        for &(k, x, y) in &p.steps {
            match k {
                0 => s += &format!(".{x}"),
                1 => s += &format!(".case{x}.{y}"),
                2 => s += &format!(".seq{x}"),
                5 => s += &format!(":t{x}"),
                _ => s += &format!("[{x}..{y}]"),
            }
        }
        if p.cut {
            s += "[..]";
        }
        s
    }
    fn whole_flag(&self, a: AddressId) -> Option<usize> {
        match self.n.addrs[a.index()].as_slice() {
            [p] if p.root < self.slots && p.steps.is_empty() && !p.cut => {
                is_flag_slot(self.b, p.root).then_some(p.root)
            }
            _ => None,
        }
    }
    pub(super) fn acts(&self, m: &Module, i: &Inst, out: &mut Vec<Act>) {
        out.clear();
        match *i {
            Inst::Address { address, .. } => {
                if !self.deref_hits[address.index()].is_empty() {
                    out.push(Act::Use(address));
                }
            }
            Inst::Read {
                result, address, ..
            } => {
                out.push(Act::Use(address));
                if let Some(&ri) = self.read_idx.get(&result.index()) {
                    out.push(Act::Fresh(ri));
                }
            }
            Inst::Case { address, .. } => out.push(Act::Use(address)),
            Inst::Drop { address, .. } => out.push(Act::Drop(address)),
            Inst::Store { address, value } => {
                out.push(Act::Write(address));
                if let Some(f) = self.whole_flag(address) {
                    out.push(Act::Flag(
                        f,
                        self.consts.get(&value.index()).map(|&c| c != 0),
                    ));
                }
            }
            Inst::SetCase { address, .. } | Inst::PutLanes { to: address, .. } => {
                out.push(Act::Write(address))
            }
            Inst::Copy { to, from, .. } | Inst::Move { to, from, .. } => {
                out.push(Act::Use(from));
                out.push(Act::Write(to));
                if self.has_dest
                    && self.n.addrs[to.index()]
                        .iter()
                        .any(|p| p.root == self.slots)
                {
                    out.push(Act::CopyToDest(from));
                }
            }
            Inst::Grow { carrier, .. } | Inst::PromoteFrame { carrier } => {
                out.push(Act::Use(carrier));
                out.push(Act::Invalidate(carrier));
            }
            Inst::StorageLive { slot } | Inst::StorageDead { slot } => {
                out.push(if matches!(i, Inst::StorageLive { .. }) {
                    Act::Begin(slot.index())
                } else {
                    Act::Dead(slot.index())
                });
                if self.guards.contains_key(&slot.index())
                    || self.reads_of_flag.contains_key(&slot.index())
                {
                    out.push(Act::Flag(slot.index(), None));
                }
            }
            Inst::Call { call } => {
                let site = &self.b.calls[call.index()];
                let sig = &m.signatures[site.signature.index()];
                let args = site.args.get(&self.b.pools.args).unwrap_or_default();
                let dest = usize::from(sig.dest.is_some());
                for (k, arg) in args.iter().enumerate() {
                    let Arg::Address(a) = *arg else { continue };
                    match k.checked_sub(dest).map(|k| sig.params.get(k)) {
                        None
                        | Some(Some(Param::Address {
                            loan: Loan::Mut, ..
                        })) => out.push(Act::Write(a)),
                        _ => out.push(Act::Use(a)),
                    }
                }
                if let Some(&k) = self.lends.get(&call.index()) {
                    out.push(Act::Lend(k));
                }
            }
            _ => {}
        }
    }
    pub(super) fn may_drop(&self, now: &[u8], q: usize) -> bool {
        self.pairs[q].iter().all(|&p| now[p] & D != 0)
    }
    pub(super) fn dest_may_be_dropped(&self, now: &[u8]) -> bool {
        self.tracked
            .iter()
            .enumerate()
            .any(|(q, t)| t.root == self.slots && self.may_drop(now, q))
    }
    /// Apply `act`, checking it first when `check`.
    pub(super) fn apply(
        &self,
        m: &Module,
        now: &mut [u8],
        act: &Act,
        check: bool,
        seen: &mut Vec<usize>,
    ) -> R {
        match *act {
            Act::Use(a) => {
                if check {
                    for &q in self.hits[a.index()]
                        .iter()
                        .chain(&self.deref_hits[a.index()])
                    {
                        need(!self.may_drop(now, q), || {
                            format!(
                                "a{}: {} is used where it may be dropped",
                                a.0,
                                self.show(&self.tracked[q])
                            )
                        })?;
                    }
                }
            }
            Act::Drop(a) => {
                let roots = &self.n.addrs[a.index()];
                if check {
                    for r in roots {
                        need(
                            !(r.root >= self.slots && self.views[r.root - self.slots]),
                            || format!("a{}: drops a view parameter's place", a.0),
                        )?;
                    }
                    for &q in &self.hits[a.index()] {
                        need(!self.may_drop(now, q), || {
                            format!(
                                "a{}: drops {} where it may be dropped already",
                                a.0,
                                self.show(&self.tracked[q])
                            )
                        })?;
                    }
                }
                let routed = self.routed[a.index()];
                let mut own = vec![];
                if routed {
                    let joined = &self.j.addrs[a.index()];
                    for r in joined.iter().filter(|r| !r.cut) {
                        own.extend(self.tracked.iter().position(|t| t == r));
                    }
                    for &q in &own {
                        self.drop(now, q, joined.len() == 1, seen);
                    }
                }
                let strong = !routed && roots.len() == 1;
                for r in roots.iter().filter(|r| !r.cut) {
                    if let Some(q) = self.tracked.iter().position(|t| t == r) {
                        let bound = routed && self.binds[q].iter().any(|(t, _, _)| own.contains(t));
                        if bound {
                            self.drop_bound(now, q, &own, seen);
                        } else {
                            self.drop(now, q, strong, seen);
                        }
                        // A join place overlapping one dropped through the join is that drop's:
                        // `Two(..)` dropping `*v.case1.0` leaves `*v` and `*v.case1.1` undropped.
                        let held = |t: &usize| {
                            own.iter()
                                .any(|&o| overlap(&self.tracked[o], &self.tracked[*t]))
                        };
                        for &t in self.shadow[q].iter().filter(|t| !held(t)) {
                            if bound {
                                self.drop_shadow(now, q, t, &own);
                            } else {
                                self.drop(now, t, false, seen);
                            }
                        }
                    }
                }
            }
            Act::Invalidate(a) => self.invalidate(now, &self.rewrites[a.index()], seen),
            Act::Write(a) => {
                self.invalidate(now, &self.rewrites[a.index()], seen);
                // Any non-`store` write (copy/move/set_case/mut-arg/partial store) that may
                // touch a drop-flag slot invalidates its tracked value and marks its reads stale.
                for r in &self.n.addrs[a.index()] {
                    if r.root < self.slots {
                        if let Some(ps) = self.guards.get(&r.root) {
                            for &p in ps {
                                now[p] = flag(now[p], None);
                            }
                        }
                        if let Some(ris) = self.reads_of_flag.get(&r.root) {
                            for &ri in ris {
                                now[self.stale_base + ri] = 1;
                            }
                        }
                    }
                }
                let places = if self.routed[a.index()] {
                    &self.j.addrs[a.index()]
                } else {
                    &self.n.addrs[a.index()]
                };
                if let [r] = places.as_slice()
                    && !r.cut
                {
                    if self.has_dest && r.root == self.slots && r.steps.is_empty() {
                        now[self.dest_escaped] = 0;
                    }
                    for &q in &self.hits[a.index()] {
                        if covers(m, self.b, r, &self.tracked[q]) {
                            self.end(now, q);
                            self.mark(now, q, begun, true);
                            for &t in &self.shadow[q] {
                                self.mark(now, t, begun, false);
                            }
                        }
                    }
                }
            }
            Act::CopyToDest(from) => {
                for &q in &self.hits[from.index()] {
                    if self.held[q]
                        .iter()
                        .any(|&k| self.borrow_from_local[k] && now[self.base + k] != 0)
                    {
                        now[self.dest_escaped] = 1;
                        break;
                    }
                }
            }
            Act::Begin(s) | Act::Dead(s) => {
                // `dead` never revives a dropped place: only the next `live` begins it.
                let f: fn(u8) -> u8 = if matches!(act, Act::Begin(_)) {
                    begun
                } else {
                    |v| dropped(v, true)
                };
                self.invalidate(now, &self.anchor_slots[s], seen);
                for (q, t) in self.tracked.iter().enumerate() {
                    if t.root == s {
                        self.end(now, q);
                        self.mark(now, q, f, true);
                        for &t in &self.shadow[q] {
                            self.mark(now, t, f, false);
                        }
                    }
                }
            }
            Act::Lend(k) => {
                now[self.base + k] = 1;
                if self.borrow_to_dest[k] && self.borrow_from_local[k] {
                    now[self.dest_escaped] = 1;
                }
            }
            Act::Flag(f, value) => {
                for &p in self.guards.get(&f).map_or(&[][..], |v| v) {
                    now[p] = flag(now[p], value);
                }
                if let Some(ris) = self.reads_of_flag.get(&f) {
                    for &ri in ris {
                        now[self.stale_base + ri] = 1;
                    }
                }
            }
            Act::Fresh(ri) => {
                now[self.stale_base + ri] = 0;
            }
        }
        Ok(())
    }
    /// Walk active borrow edges with one budgeted work vector. A cycle can add
    /// each tracked destination at most once.
    pub(super) fn invalidate(&self, now: &mut [u8], loans: &[usize], seen: &mut Vec<usize>) {
        seen.clear();
        self.enqueue(now, loans, seen);
        let mut next = 0;
        while let Some(&q) = seen.get(next) {
            self.mark(now, q, |s| dropped(s, true), false);
            // A join that may be the view dangles with it.
            for &t in &self.shadow[q] {
                self.mark(now, t, |s| dropped(s, true), false);
            }
            self.enqueue(now, &self.lent[q], seen);
            next += 1;
        }
    }
    fn enqueue(&self, now: &[u8], loans: &[usize], seen: &mut Vec<usize>) {
        for &k in loans {
            if now[self.base + k] != 0 {
                for &d in &self.borrows[k] {
                    if !seen.contains(&d) {
                        seen.push(d);
                    }
                }
            }
        }
    }
    fn drop(&self, now: &mut [u8], q: usize, strong: bool, seen: &mut Vec<usize>) {
        self.mark(now, q, |s| dropped(s, true), strong);
        self.invalidate(now, &self.lent[q], seen);
    }
    /// A new value in place `q` ends the borrows whose dest it is: a slot reused
    /// for another value no longer views the old anchor.
    fn end(&self, now: &mut [u8], q: usize) {
        for &k in &self.held[q] {
            if self.borrows[k].len() == 1 {
                now[self.base + k] = 0;
            }
        }
    }
    /// Set place `q`'s pairings to `f` of themselves, or, when not `strong`, add
    /// that to what they were.
    fn mark(&self, now: &mut [u8], q: usize, f: impl Fn(u8) -> u8, strong: bool) {
        for &p in &self.pairs[q] {
            now[p] = if strong {
                f(now[p])
            } else {
                now[p] | f(now[p])
            };
        }
    }
}

/// No use of a slot, or of a pointer to it, follows its last storage end.
pub(super) fn ended(b: &Body, budget: &Debit<'_>) -> Result<(), Error> {
    match crate::extent::use_after_end_with_budget(b, budget)? {
        Some(((bi, _), s)) => Err(Error::Invalid(format!(
            "b{bi}: s{s} is used after its storage ends"
        ))),
        None => Ok(()),
    }
}

pub(super) fn check(
    m: &Module,
    sig: &Signature,
    b: &Body,
    layouts: &mut Layouts<'_, '_>,
    budget: &Debit<'_>,
) -> Result<(), Error> {
    let scratch = budget.scratch();
    scratch.check()?;
    let entry = b
        .blocks
        .first()
        .and_then(|k| k.params.get(&b.pools.values))
        .unwrap_or_default();
    let has_dest = sig.dest.is_some();
    let skip = usize::from(has_dest);
    let mut views = Vec::new();
    scratch.reserve(&mut views, entry.len())?;
    views.resize(entry.len(), false);
    for (k, p) in sig.params.iter().enumerate() {
        if let (
            Param::Address {
                loan: Loan::View, ..
            },
            Some(v),
        ) = (p, views.get_mut(k + skip))
        {
            *v = true;
        }
    }
    check_moves(b, layouts)?;
    let (n, j, joins) = names(m, b, entry, layouts, &scratch)?;
    let mut routed = Vec::new();
    scratch.reserve(&mut routed, b.addresses.len())?;
    for a in 0..b.addresses.len() {
        routed.push(!j.addrs[a].is_empty() || !j.derefs[a].is_empty());
    }
    // The places some drop may reach, and those a view parameter's drop would.
    let mut tracked: Vec<Pl> = Vec::new();
    let mut any_view_drop = false;
    for i in &b.insts {
        if let Inst::Drop { address, .. } = *i {
            for r in &n.addrs[address.index()] {
                any_view_drop |= r.root >= b.slots.len() && views[r.root - b.slots.len()];
            }
            for r in n.addrs[address.index()]
                .iter()
                .chain(&j.addrs[address.index()])
            {
                if !r.cut && !tracked.contains(r) {
                    scratch.reserve(&mut tracked, 1)?;
                    tracked.push(r.clone());
                }
            }
        }
    }
    let dropping = !tracked.is_empty();
    // A call whose signature marks its dest a view (OWN-20) makes the dest's places
    // borrow its anchor arguments' places.
    let mut borrows = Vec::new();
    for i in &b.insts {
        let Inst::Call { call } = *i else { continue };
        let site = &b.calls[call.index()];
        check_call(m, b, site, &n, &scratch)?;
        let csig = &m.signatures[site.signature.index()];
        let args = site.args.get(&b.pools.args).unwrap_or_default();
        if csig.anchors.is_empty() {
            continue;
        }
        let Some(&Arg::Address(d)) = args.first() else {
            continue;
        };
        for p in n.addrs[d.index()].iter().chain(&j.addrs[d.index()]) {
            if !tracked.contains(p) {
                scratch.reserve(&mut tracked, 1)?;
                scratch.allocation::<(u8, u32, u32)>(p.steps.len())?;
                tracked.push(p.clone());
            }
        }
        scratch.reserve(&mut borrows, 1)?;
        borrows.push((call.index(), d.index()));
    }
    // A join place stands for each place its parameter's edges carry; one that an
    // address reaches is tracked when one of those is, so a use through the join
    // after a drop of the slot itself is seen.
    let roots = b.slots.len() + entry.len();
    let expand = |tp: &Pl| -> Vec<Pl> {
        n.vals[joins[tp.root - roots]]
            .iter()
            .map(|src| {
                let mut x = src.clone();
                x.steps.extend(tp.steps.iter().copied());
                x
            })
            .collect()
    };
    for a in (0..b.addresses.len()).filter(|&a| routed[a]) {
        scratch.check()?;
        for r in j.addrs[a].iter().chain(&j.derefs[a]) {
            let r = Pl {
                cut: false,
                ..r.clone()
            };
            let under = |x: &Pl| tracked.iter().any(|t| t.root < roots && overlap(x, t));
            if !tracked.contains(&r) && expand(&r).iter().any(under) {
                scratch.reserve(&mut tracked, 1)?;
                tracked.push(r);
            }
        }
    }
    // An edge that gives one join's pointer to another reads the first join's state,
    // so the first tracks every place the second does, used there or not.
    loop {
        scratch.check()?;
        let mut grew = false;
        for e in &b.edges {
            for (a, p) in ptr_args(b, e) {
                let (Some(i), Some(k)) = (
                    joins.iter().position(|&x| x == a.index()),
                    joins.iter().position(|&x| x == p.index()),
                ) else {
                    continue;
                };
                for t in 0..tracked.len() {
                    if tracked[t].root != roots + k {
                        continue;
                    }
                    let from = Pl {
                        root: roots + i,
                        ..tracked[t].clone()
                    };
                    if !tracked.contains(&from) {
                        scratch.reserve(&mut tracked, 1)?;
                        scratch.allocation::<(u8, u32, u32)>(from.steps.len())?;
                        tracked.push(from);
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
    let has_slot_return = b.terminators.iter().any(|t| {
        matches!(t, Terminator::Return(r) if r.get(&b.pools.values).unwrap_or_default().iter().any(|v| {
            n.vals[v.index()].iter().any(|p| p.root < b.slots.len())
        }))
    });
    if !dropping && !any_view_drop && borrows.is_empty() && !has_slot_return {
        return Ok(());
    }
    let mut hits: Vec<Vec<usize>> = Vec::new();
    scratch.reserve(&mut hits, b.addresses.len())?;
    let mut deref_hits: Vec<Vec<usize>> = Vec::new();
    scratch.reserve(&mut deref_hits, b.addresses.len())?;
    for a in 0..b.addresses.len() {
        scratch.check()?;
        let ps = if routed[a] { &j.addrs[a] } else { &n.addrs[a] };
        let mut h: Vec<usize> = (0..tracked.len())
            .filter(|&q| ps.iter().any(|p| overlap(p, &tracked[q])))
            .collect();
        h.dedup();
        scratch.allocation::<usize>(h.len())?;
        hits.push(h);
        let dps = if routed[a] {
            &j.derefs[a]
        } else {
            &n.derefs[a]
        };
        let mut dh: Vec<usize> = (0..tracked.len())
            .filter(|&q| dps.iter().any(|p| overlap(p, &tracked[q])))
            .collect();
        dh.dedup();
        scratch.allocation::<usize>(dh.len())?;
        deref_hits.push(dh);
    }
    let mut shadow: Vec<Vec<usize>> = Vec::new();
    scratch.reserve(&mut shadow, tracked.len())?;
    shadow.resize_with(tracked.len(), Vec::new);
    for (t, tp) in tracked
        .iter()
        .enumerate()
        .filter(|(_, tp)| tp.root >= roots)
    {
        for x in expand(tp) {
            for (q, tq) in tracked.iter().enumerate() {
                if overlap(&x, tq) && !shadow[q].contains(&t) {
                    scratch.reserve(&mut shadow[q], 1)?;
                    shadow[q].push(t);
                }
            }
        }
    }
    let local_owned = local_owned_slots(m, sig, b, &n, &scratch)?;
    let mut lent: Vec<Vec<usize>> = Vec::new();
    scratch.reserve(&mut lent, tracked.len())?;
    lent.resize_with(tracked.len(), Vec::new);
    let mut held: Vec<Vec<usize>> = Vec::new();
    scratch.reserve(&mut held, tracked.len())?;
    held.resize_with(tracked.len(), Vec::new);
    let mut anchor_slots: Vec<Vec<usize>> = Vec::new();
    scratch.reserve(&mut anchor_slots, b.slots.len())?;
    anchor_slots.resize_with(b.slots.len(), Vec::new);
    let mut borrow_from_local = Vec::new();
    scratch.reserve(&mut borrow_from_local, borrows.len())?;
    let mut borrow_to_dest = Vec::new();
    scratch.reserve(&mut borrow_to_dest, borrows.len())?;
    let mut lends = std::collections::HashMap::new();
    let mut dests = Vec::new();
    scratch.reserve(&mut dests, borrows.len())?;
    let mut rewrites = Vec::new();
    scratch.reserve(&mut rewrites, b.addresses.len())?;
    rewrites.resize_with(b.addresses.len(), Vec::new);
    for (k, &(call, dest)) in borrows.iter().enumerate() {
        let mut to_ids = Vec::new();
        for d in n.addrs[dest].iter().chain(&j.addrs[dest]) {
            if let Some(q) = tracked.iter().position(|t| t == d)
                && !to_ids.contains(&q)
            {
                scratch.reserve(&mut to_ids, 1)?;
                to_ids.push(q);
            }
        }
        let site = &b.calls[call];
        let sig = &m.signatures[site.signature.index()];
        let args = site.args.get(&b.pools.args).unwrap_or_default();
        let from = || {
            sig.anchors
                .iter()
                .flat_map(|&k| match args[1 + k as usize] {
                    Arg::Address(a) => n.addrs[a.index()].as_slice(),
                    Arg::Value(_) => &[],
                })
        };
        let mut from_local = false;
        for f in from().filter(|f| f.root < b.slots.len()) {
            from_local |= local_owned[f.root];
            if !anchor_slots[f.root].contains(&k) {
                scratch.reserve(&mut anchor_slots[f.root], 1)?;
                anchor_slots[f.root].push(k);
            }
        }
        borrow_from_local.push(from_local);
        borrow_to_dest.push(has_dest && n.addrs[dest].iter().any(|p| p.root == b.slots.len()));
        for (a, places) in n.addrs.iter().enumerate() {
            scratch.check()?;
            if places.iter().any(|p| from().any(|f| overlap(f, p))) {
                scratch.reserve(&mut rewrites[a], 1)?;
                rewrites[a].push(k);
            }
        }
        for (q, tq) in tracked.iter().enumerate() {
            scratch.check()?;
            if tq.root < roots && from().any(|f| overlap(f, tq)) {
                scratch.reserve(&mut lent[q], 1)?;
                lent[q].push(k);
            }
        }
        for &d in &to_ids {
            scratch.reserve(&mut held[d], 1)?;
            held[d].push(k);
        }
        scratch.allocation::<(usize, usize)>(2)?;
        lends.insert(call, k);
        dests.push(to_ids);
    }
    // A flag slot whose pointer escapes into memory or an untyped call argument
    // cannot be trusted to guard drops.
    let mut escaped_flag = Vec::new();
    scratch.reserve(&mut escaped_flag, b.slots.len())?;
    escaped_flag.resize(b.slots.len(), false);
    for i in &b.insts {
        match *i {
            Inst::Store { value, .. } => {
                for p in &n.vals[value.index()] {
                    if p.root < b.slots.len() {
                        escaped_flag[p.root] = true;
                    }
                }
            }
            Inst::Call { call } => {
                let args = b.calls[call.index()]
                    .args
                    .get(&b.pools.args)
                    .unwrap_or_default();
                for arg in args {
                    if let Arg::Value(v) = *arg {
                        for p in &n.vals[v.index()] {
                            if p.root < b.slots.len() {
                                escaped_flag[p.root] = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut pairs = Vec::new();
    scratch.reserve(&mut pairs, tracked.len())?;
    for q in 0..tracked.len() {
        pairs.push(vec![q]);
    }
    let mut binds = Vec::new();
    scratch.reserve(&mut binds, shadow.len())?;
    binds.resize_with(shadow.len(), Vec::new);
    let mut own = Own {
        b,
        j,
        borrows: dests,
        borrow_from_local,
        borrow_to_dest,
        anchor_slots,
        rewrites,
        lends,
        lent,
        held,
        base: 0,
        stale_base: 0,
        dest_escaped: 0,
        routed,
        roots,
        has_dest,
        joins,
        binds,
        shadow,
        slots: b.slots.len(),
        views,
        hits,
        deref_hits,
        pairs,
        tracked,
        n,
        guards: Default::default(),
        flag_of: Default::default(),
        read_idx: Default::default(),
        reads_of_flag: Default::default(),
        consts: Default::default(),
    };
    let mut candidate_reads: std::collections::HashMap<usize, usize> = Default::default();
    for i in &b.insts {
        match *i {
            Inst::Read {
                result, address, ..
            } => {
                if let Some(f) = own.whole_flag(address)
                    && !escaped_flag[f]
                {
                    scratch.allocation::<(usize, usize)>(2)?;
                    candidate_reads.insert(result.index(), f);
                }
            }
            Inst::Const { result, bits } => {
                scratch.allocation::<(usize, u64)>(2)?;
                own.consts.insert(result.index(), bits);
            }
            _ => {}
        }
    }
    let mut flag_reads_count = 0usize;
    for t in &b.terminators {
        if let Terminator::Branch { condition, .. } = *t
            && let Some(&f) = candidate_reads.get(&condition.index())
        {
            if !own.read_idx.contains_key(&condition.index()) {
                scratch.allocation::<(usize, usize)>(2)?;
                let ri = flag_reads_count;
                flag_reads_count += 1;
                own.read_idx.insert(condition.index(), ri);
                own.flag_of.insert(condition.index(), f);
                let list = own.reads_of_flag.entry(f).or_default();
                scratch.reserve(list, 1)?;
                list.push(ri);
            }
        }
    }
    // Pair each place with every flag a branch to one of its drops reads.
    let mut width = own.tracked.len();
    let insts = |bi: usize| b.blocks[bi].insts.get(&b.insts).unwrap_or_default();
    for bi in 0..b.blocks.len() {
        scratch.check()?;
        let Terminator::Branch { condition, yes, .. } = b.terminators[bi] else {
            continue;
        };
        let Some(&f) = own.flag_of.get(&condition.index()) else {
            continue;
        };
        let mut at = b.edges[yes.index()].target.index();
        for _ in 0..4 {
            for i in insts(at) {
                let Inst::Drop { address, .. } = *i else {
                    continue;
                };
                // The places a guarded drop through a join may reach take the flag too:
                // which of them the join is, and so which it drops, follows the flag.
                let mut reach = own.hits[address.index()].clone();
                if own.routed[address.index()] {
                    for r in &own.n.addrs[address.index()] {
                        if let Some(q) = own.tracked.iter().position(|t| t == r)
                            && !reach.contains(&q)
                        {
                            reach.push(q);
                        }
                    }
                }
                scratch.allocation::<usize>(reach.len())?;
                for &q in &reach {
                    let paired = own
                        .guards
                        .get(&f)
                        .is_some_and(|g| g.iter().any(|p| own.pairs[q].contains(p)));
                    if !paired {
                        scratch.reserve(&mut own.pairs[q], 1)?;
                        own.pairs[q].push(width);
                        let g = own.guards.entry(f).or_default();
                        scratch.reserve(g, 1)?;
                        g.push(width);
                        width += 1;
                    }
                }
            }
            match b.terminators[at] {
                Terminator::Jump(e) => at = b.edges[e.index()].target.index(),
                _ => break,
            }
        }
    }
    // Pair each pairing of a place with each join that may be it: where the join
    // may be bound to it, under that pairing's flag.
    for q in 0..own.tracked.len() {
        scratch.check()?;
        for t in own.shadow[q].clone() {
            for p in own.pairs[q].clone() {
                if let Some(g) = own.guards.values_mut().find(|g| g.contains(&p)) {
                    scratch.reserve(g, 1)?;
                    g.push(width);
                }
                scratch.reserve(&mut own.binds[q], 1)?;
                own.binds[q].push((t, p, width));
                width += 1;
            }
        }
    }
    own.base = width;
    width += own.borrows.len();
    own.stale_base = width;
    width += flag_reads_count;
    own.dest_escaped = width;
    width += usize::from(has_dest);
    own.solve(m, width, &scratch)
}
