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
//! A forward may-analysis then tracks, for each place some `drop` may reach,
//! whether it may be dropped. A `drop` through one place drops it; through several,
//! it may drop each. Any write overlapping the place, and its slot's `live` or
//! `dead`, begin it anew. A move leaves its source's state alone: the producer clears
//! the source's drop flag, which is what the next paragraph reads.
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

use super::*;

type Steps = Vec<(u8, u32, u32)>;

/// A root and its static steps; `cut` when a run-time offset stands for some part
/// of the place before it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Pl {
    root: usize,
    steps: Steps,
    cut: bool,
}

/// No loop of `pointer` and block parameters grows a place's steps past this.
const DEEP: usize = 16;

fn step(p: &mut Pl, s: &Step) -> bool {
    match *s {
        Step::Deref => return false,
        _ if p.cut => {}
        _ if p.steps.len() >= DEEP => p.cut = true,
        Step::Field(f) => p.steps.push((0, f, 0)),
        Step::CaseField { case, field } => p.steps.push((1, case, field)),
        Step::Seq(w) => p.steps.push((2, w as u32, 0)),
        Step::Index(_) | Step::Element(_) | Step::Byte(_) => p.cut = true,
    }
    true
}

fn overlap(a: &Pl, b: &Pl) -> bool {
    a.root == b.root && (a.steps.starts_with(&b.steps) || b.steps.starts_with(&a.steps))
}

/// What each address names, and the places an address's `deref` steps read.
struct Names {
    addrs: Vec<Vec<Pl>>,
    derefs: Vec<Vec<Pl>>,
}

fn names(b: &Body, entry: &[ValueId]) -> Names {
    let slots = b.slots.len();
    let mut vals: Vec<Vec<Pl>> = vec![vec![]; b.values.len()];
    for (k, v) in entry.iter().enumerate() {
        vals[v.index()].push(Pl {
            root: slots + k,
            steps: vec![],
            cut: false,
        });
    }
    let mut addrs: Vec<Vec<Pl>> = vec![vec![]; b.addresses.len()];
    let mut derefs: Vec<Vec<Pl>> = vec![vec![]; b.addresses.len()];
    let pointers: Vec<(usize, usize)> = b
        .insts
        .iter()
        .filter_map(|i| match *i {
            Inst::Pointer { result, address } => Some((result.index(), address.index())),
            _ => None,
        })
        .collect();
    let edges: Vec<(usize, usize)> = b
        .edges
        .iter()
        .flat_map(|e| {
            let params = b.blocks[e.target.index()]
                .params
                .get(&b.pools.values)
                .unwrap_or_default();
            let args = e.args.get(&b.pools.values).unwrap_or_default();
            params
                .iter()
                .zip(args)
                .filter(|(p, _)| b.values[p.index()] == Ty::Ptr)
                .map(|(p, a)| (a.index(), p.index()))
                .collect::<Vec<_>>()
        })
        .collect();
    let union = |to: &mut Vec<Pl>, from: &[Pl]| {
        let mut grew = false;
        for p in from {
            if !to.contains(p) {
                to.push(p.clone());
                grew = true;
            }
        }
        grew
    };
    loop {
        let mut grew = false;
        for (i, info) in b.addresses.iter().enumerate() {
            let base = match info.base {
                AddressBase::Slot(s) => vec![Pl {
                    root: s.index(),
                    steps: vec![],
                    cut: false,
                }],
                AddressBase::Address(x) => addrs[x.index()].clone(),
                AddressBase::Value(v) => vals[v.index()].clone(),
            };
            let steps = info.steps.get(&b.pools.steps).unwrap_or_default();
            let mut out = vec![];
            let mut read = vec![];
            for mut p in base {
                let mut whole = true;
                for s in steps {
                    if !step(&mut p, s) {
                        read.push(p.clone());
                        whole = false;
                        break;
                    }
                }
                if whole {
                    out.push(p);
                }
            }
            grew |= union(&mut addrs[i], &out);
            union(&mut derefs[i], &read);
        }
        for &(v, a) in &pointers {
            let from = addrs[a].clone();
            grew |= union(&mut vals[v], &from);
        }
        for &(a, p) in &edges {
            let from = vals[a].clone();
            grew |= union(&mut vals[p], &from);
        }
        if !grew {
            return Names { addrs, derefs };
        }
    }
}

const ZT: u8 = 1;
const ZF: u8 = 2;
const DT: u8 = 4;
const DF: u8 = 8;
const T: u8 = ZT | DT;
const F: u8 = ZF | DF;
const D: u8 = DT | DF;
const Z: u8 = ZT | ZF;

fn halves(s: u8) -> (bool, bool) {
    (s & T != 0, s & F != 0)
}
fn dropped(s: u8, strong: bool) -> u8 {
    let (t, f) = halves(s);
    let d = (if t { DT } else { 0 }) | (if f { DF } else { 0 });
    if strong { d } else { s | d }
}
fn begun(s: u8) -> u8 {
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
enum Act {
    Use(AddressId),
    Drop(AddressId),
    Write(AddressId),
    Begin(usize),
    Flag(usize, Option<bool>),
}

struct Own<'a> {
    b: &'a Body,
    n: Names,
    slots: usize,
    /// Entry values that are `view` parameters' pointers, by entry index.
    views: Vec<bool>,
    tracked: Vec<Pl>,
    /// Per address: the tracked places it overlaps.
    hits: Vec<Vec<usize>>,
    /// Per address: the tracked places its deref steps read through.
    deref_hits: Vec<Vec<usize>>,
    /// Per tracked place: its pairings (indices into the state).
    pairs: Vec<Vec<usize>>,
    /// Per flag slot: the pairings it guards.
    guards: std::collections::HashMap<usize, Vec<usize>>,
    /// Read results of whole flag slots.
    flag_of: std::collections::HashMap<usize, usize>,
    consts: std::collections::HashMap<usize, u64>,
}

impl Own<'_> {
    fn show(&self, p: &Pl) -> String {
        let mut s = if p.root < self.slots {
            format!("s{}", p.root)
        } else {
            format!("parameter {}", p.root - self.slots)
        };
        for &(k, x, y) in &p.steps {
            match k {
                0 => s += &format!(".{x}"),
                1 => s += &format!(".case{x}.{y}"),
                _ => s += &format!(".seq{x}"),
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
    fn acts(&self, m: &Module, i: &Inst, out: &mut Vec<Act>) {
        out.clear();
        match *i {
            Inst::Address { address, .. } => {
                if !self.deref_hits[address.index()].is_empty() {
                    out.push(Act::Use(address));
                }
            }
            Inst::Read { address, .. } | Inst::Case { address, .. } => out.push(Act::Use(address)),
            Inst::Drop { address, .. } => out.push(Act::Drop(address)),
            Inst::Store { address, value } => match self.whole_flag(address) {
                Some(f) => out.push(Act::Flag(
                    f,
                    self.consts.get(&value.index()).map(|&c| c != 0),
                )),
                None => out.push(Act::Write(address)),
            },
            Inst::SetCase { address, .. } | Inst::PutLanes { to: address, .. } => {
                out.push(Act::Write(address))
            }
            Inst::Copy { to, from, .. } | Inst::Move { to, from, .. } => {
                out.push(Act::Use(from));
                out.push(Act::Write(to));
            }
            Inst::Grow { carrier, .. } => out.push(Act::Use(carrier)),
            Inst::StorageLive { slot } | Inst::StorageDead { slot } => {
                out.push(Act::Begin(slot.index()));
                if self.guards.contains_key(&slot.index()) {
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
            }
            _ => {}
        }
    }
    fn may_drop(&self, now: &[u8], q: usize) -> bool {
        self.pairs[q].iter().all(|&p| now[p] & D != 0)
    }
    /// Apply `act`, checking it first when `check`.
    fn apply(&self, now: &mut [u8], act: &Act, check: bool) -> R {
        match *act {
            Act::Use(a) => {
                if check {
                    for &q in self.hits[a.index()].iter().chain(&self.deref_hits[a.index()]) {
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
                let strong = roots.len() == 1;
                for r in roots.iter().filter(|r| !r.cut) {
                    if let Some(q) = self.tracked.iter().position(|t| t == r) {
                        for &p in &self.pairs[q] {
                            now[p] = dropped(now[p], strong);
                        }
                    }
                }
            }
            Act::Write(a) => {
                if let [r] = self.n.addrs[a.index()].as_slice()
                    && !r.cut
                {
                    for &q in &self.hits[a.index()] {
                        for &p in &self.pairs[q] {
                            now[p] = begun(now[p]);
                        }
                    }
                }
            }
            Act::Begin(s) => {
                for (q, t) in self.tracked.iter().enumerate() {
                    if t.root == s {
                        for &p in &self.pairs[q] {
                            now[p] = begun(now[p]);
                        }
                    }
                }
            }
            Act::Flag(f, value) => {
                for &p in self.guards.get(&f).map_or(&[][..], |v| v) {
                    now[p] = flag(now[p], value);
                }
            }
        }
        Ok(())
    }
}

pub(super) fn check(m: &Module, sig: &Signature, b: &Body) -> R {
    let entry = b
        .blocks
        .first()
        .and_then(|k| k.params.get(&b.pools.values))
        .unwrap_or_default();
    let skip = usize::from(sig.dest.is_some());
    let mut views = vec![false; entry.len()];
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
    let n = names(b, entry);
    // The places some drop may reach, and those a view parameter's drop would.
    let mut tracked: Vec<Pl> = vec![];
    let mut any_view_drop = false;
    for i in &b.insts {
        if let Inst::Drop { address, .. } = *i {
            for r in &n.addrs[address.index()] {
                any_view_drop |= r.root >= b.slots.len() && views[r.root - b.slots.len()];
                if !r.cut && !tracked.contains(r) {
                    tracked.push(r.clone());
                }
            }
        }
    }
    if tracked.is_empty() && !any_view_drop {
        return Ok(());
    }
    let hits_of = |ps: &[Pl]| -> Vec<usize> {
        let mut v: Vec<usize> = (0..tracked.len())
            .filter(|&q| ps.iter().any(|p| overlap(p, &tracked[q])))
            .collect();
        v.dedup();
        v
    };
    let hits: Vec<Vec<usize>> = n.addrs.iter().map(|ps| hits_of(ps)).collect();
    let deref_hits: Vec<Vec<usize>> = n.derefs.iter().map(|ps| hits_of(ps)).collect();
    let mut own = Own {
        b,
        slots: b.slots.len(),
        views,
        hits,
        deref_hits,
        pairs: (0..tracked.len()).map(|q| vec![q]).collect(),
        tracked,
        n,
        guards: Default::default(),
        flag_of: Default::default(),
        consts: Default::default(),
    };
    for i in &b.insts {
        match *i {
            Inst::Read {
                result, address, ..
            } => {
                if let Some(f) = own.whole_flag(address) {
                    own.flag_of.insert(result.index(), f);
                }
            }
            Inst::Const { result, bits } => {
                own.consts.insert(result.index(), bits);
            }
            _ => {}
        }
    }
    // Pair each place with every flag a branch to one of its drops reads.
    let mut width = own.tracked.len();
    let insts = |bi: usize| b.blocks[bi].insts.get(&b.insts).unwrap_or_default();
    for bi in 0..b.blocks.len() {
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
                for &q in &own.hits[address.index()].clone() {
                    let paired = own.guards.get(&f).is_some_and(|g| {
                        g.iter().any(|p| own.pairs[q].contains(p))
                    });
                    if !paired {
                        own.pairs[q].push(width);
                        own.guards.entry(f).or_default().push(width);
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
    // Forward may-analysis: a block starts with the union of what its edges carry.
    let blocks = b.blocks.len();
    let mut start: Vec<Option<Vec<u8>>> = vec![None; blocks];
    start[0] = Some(vec![Z; width]);
    let mut work = vec![0usize];
    let mut acts = vec![];
    let send = |start: &mut Vec<Option<Vec<u8>>>, work: &mut Vec<usize>, t: usize, s: &[u8]| {
        let next = match &start[t] {
            None => s.to_vec(),
            Some(old) => old.iter().zip(s).map(|(x, y)| x | y).collect(),
        };
        if start[t].as_deref() != Some(&next[..]) {
            start[t] = Some(next);
            work.push(t);
        }
    };
    let mut flow = |start: &mut Vec<Option<Vec<u8>>>, check: bool, bi: usize| -> Result<Vec<(usize, Vec<u8>)>, String> {
        let mut now = start[bi].clone().unwrap();
        let mut out = vec![];
        for i in insts(bi) {
            own.acts(m, i, &mut acts);
            let fail = i.fail(b);
            if let (Some(f), false) = (fail, matches!(i, Inst::Drop { .. })) {
                out.push((f.index(), now.clone()));
            }
            for a in &acts {
                own.apply(&mut now, a, check)
                    .map_err(|e| format!("b{bi}: {e}"))?;
            }
            if let (Some(f), true) = (fail, matches!(i, Inst::Drop { .. })) {
                out.push((f.index(), now.clone()));
            }
        }
        let edge = |e: &EdgeId| b.edges[e.index()].target.index();
        if let Terminator::Branch { condition, yes, no } = &b.terminators[bi]
            && let Some(f) = own.flag_of.get(&condition.index())
        {
            let (mut y, mut n) = (now.clone(), now);
            for &p in own.guards.get(f).map_or(&[][..], |v| v) {
                y[p] &= T;
                n[p] &= F;
            }
            out.push((edge(yes), y));
            out.push((edge(no), n));
        } else {
            if let Terminator::Raise {
                cause: RaiseCause::Fail(a),
                ..
            } = b.terminators[bi]
            {
                own.apply(&mut now, &Act::Use(a), check)
                    .map_err(|e| format!("b{bi}: {e}"))?;
            }
            for t in successors_of_terminator(b, bi) {
                out.push((t, now.clone()));
            }
        }
        Ok(out)
    };
    while let Some(bi) = work.pop() {
        for (t, s) in flow(&mut start, false, bi)? {
            if t != 0 {
                send(&mut start, &mut work, t, &s);
            }
        }
    }
    for bi in 0..blocks {
        if start[bi].is_some() {
            flow(&mut start, true, bi)?;
        }
    }
    Ok(())
}

/// Whether slot `s` is a `bool` value place (a drop flag's shape).
pub(super) fn is_flag_slot(b: &Body, s: usize) -> bool {
    b.slots.get(s) == Some(&PlaceTy::Value(Ty::Scalar(hirsel_core_layout::Scalar::Bool)))
}
