//! Slot extents and frame homes (crate docs, "Values, slots and addresses"). A
//! slot's storage is in use from a `StorageLive` to the `StorageDead` that ends it;
//! a use after the end is ill formed (`use_after_end`, which wf runs).
//!
//! [`share_slots`] gives slots of one type whose needed ranges never meet one
//! home, so a frame is bounded by what is needed at once rather than by how many
//! slots a body names. The driver runs it on each body just before an engine
//! sees the module; stored bodies and object keys are the unmerged IR.
//!
//! A use of a slot is an instruction or terminator naming an address based on it,
//! or a pointer value made from such an address in this body (through `pointer`,
//! value-based addresses and block parameters). A slot a pointer into may leave
//! the body through is never shared.
use crate::*;
use std::collections::{HashMap, HashSet};

mod budget;
pub(crate) use budget::use_after_end_with_budget;
mod operands;
pub use operands::{operands, terminator_uses};
pub(crate) use operands::{visit_operands, visit_terminator_uses};
mod needed;
pub(crate) use needed::slot_conflicts;
use needed::{anchored_views, needed_ranges};

/// The blocks block `bi`'s terminator continues at.
fn targets(b: &Body, bi: usize) -> Vec<usize> {
    let edge = |e: &EdgeId| b.edges[e.index()].target.index();
    match &b.terminators[bi] {
        Terminator::Jump(e) => vec![edge(e)],
        Terminator::Branch { yes, no, .. } => vec![edge(yes), edge(no)],
        Terminator::Switch {
            arms, otherwise, ..
        } => arms
            .get(&b.pools.arms)
            .unwrap_or_default()
            .iter()
            .map(|a| edge(&a.edge))
            .chain([edge(otherwise)])
            .collect(),
        Terminator::Raise { cleanup, .. } => cleanup.iter().map(|c| c.index()).collect(),
        Terminator::Return(_) | Terminator::Propagate | Terminator::Unreachable => vec![],
    }
}

/// A point of a body: block, and the instruction index in it (the block's length
/// is its terminator).
pub(crate) type Point = (u32, u32);

/// Each slot's begin, end and use points.
pub(crate) struct Slots {
    pub lives: Vec<Vec<Point>>,
    pub deads: Vec<Vec<Point>>,
    pub uses: Vec<Vec<Point>>,
}

/// The slots each value and address may point into, through `pointer`,
/// value-based addresses and block parameters (names as in [`operands`]).
fn provenance(b: &Body) -> Vec<Vec<u32>> {
    let n = b.values.len();
    let mut from: Vec<Vec<u32>> = vec![vec![]; n + b.addresses.len()];
    // Each name's slots flow on to the names derived from it.
    let mut next: Vec<Vec<usize>> = vec![vec![]; from.len()];
    let mut work = vec![];
    for (i, info) in b.addresses.iter().enumerate() {
        match info.base {
            AddressBase::Slot(s) => {
                from[n + i].push(s.0);
                work.push(n + i);
            }
            AddressBase::Address(x) => next[n + x.index()].push(n + i),
            AddressBase::Value(x) => next[x.index()].push(n + i),
        }
    }
    if work.is_empty() {
        return from;
    }
    for inst in &b.insts {
        if let Inst::Pointer { result, address } = *inst {
            next[n + address.index()].push(result.index());
        }
    }
    for e in &b.edges {
        let params = b.blocks[e.target.index()]
            .params
            .get(&b.pools.values)
            .unwrap_or_default();
        let args = e.args.get(&b.pools.values).unwrap_or_default();
        for (p, a) in params.iter().zip(args) {
            next[a.index()].push(p.index());
        }
    }
    while let Some(src) = work.pop() {
        for &to in &next[src] {
            if to == src {
                continue;
            }
            let mut grew = false;
            for i in 0..from[src].len() {
                let s = from[src][i];
                if !from[to].contains(&s) {
                    from[to].push(s);
                    grew = true;
                }
            }
            if grew {
                work.push(to);
            }
        }
    }
    from
}

/// What a pass that moves slots needs to know of them: each slot's use points
/// (`Slots::uses`), the slots each name may point into ([`provenance`]), and
/// the slots whose address leaves what provenance follows, as in
/// [`scoped_slots`] (a `mut` loan to a call with results counts as leaving).
pub(crate) struct SlotFacts {
    pub uses: Vec<Vec<Point>>,
    pub from: Vec<Vec<u32>>,
    pub escapes: Vec<bool>,
}

pub(crate) fn slot_facts(b: &Body, signatures: &[Signature]) -> SlotFacts {
    let from = provenance(b);
    SlotFacts {
        uses: slots_with(b, &from).uses,
        escapes: escaping(b, signatures, &from, Loans::Direct),
        from,
    }
}

/// Each slot's `StorageLive`, `StorageDead` and use points.
pub(crate) fn slots(b: &Body) -> Slots {
    slots_with(b, &provenance(b))
}

/// Each slot's `StorageLive`, `StorageDead` and use points, given the body's [`provenance`].
fn slots_with(b: &Body, from: &[Vec<u32>]) -> Slots {
    let k = b.slots.len();
    let mut out = Slots {
        lives: vec![vec![]; k],
        deads: vec![vec![]; k],
        uses: vec![vec![]; k],
    };
    let n = b.values.len();
    let mut seen: Vec<u32> = vec![];
    let mut note =
        |uses: &mut Vec<Vec<Point>>, names: &mut dyn Iterator<Item = usize>, at: Point| {
            seen.clear();
            for name in names {
                for &s in &from[name] {
                    if !seen.contains(&s) {
                        seen.push(s);
                        uses[s as usize].push(at);
                    }
                }
            }
        };
    for (bi, blk) in b.blocks.iter().enumerate() {
        let insts = blk.insts.get(&b.insts).unwrap_or_default();
        for (j, inst) in insts.iter().enumerate() {
            let at = (bi as u32, j as u32);
            match *inst {
                Inst::StorageLive { slot } => out.lives[slot.index()].push(at),
                Inst::StorageDead { slot } => out.deads[slot.index()].push(at),
                Inst::Address { address, .. } => {
                    note(&mut out.uses, &mut std::iter::once(n + address.index()), at)
                }
                _ => {
                    let (uses, _) = operands(b, inst);
                    note(&mut out.uses, &mut uses.into_iter(), at);
                }
            }
        }
        let at = (bi as u32, insts.len() as u32);
        note(&mut out.uses, &mut terminator_uses(b, bi).into_iter(), at);
    }
    out
}

/// For each block, the blocks control enters it from and how much of each ran
/// before: the whole block for a terminator edge, the instructions before the
/// failing one for a failure edge.
fn predecessors(b: &Body) -> Vec<Vec<Point>> {
    let mut preds = vec![vec![]; b.blocks.len()];
    for (bi, blk) in b.blocks.iter().enumerate() {
        let insts = blk.insts.get(&b.insts).unwrap_or_default();
        for (j, inst) in insts.iter().enumerate() {
            if let Some(f) = inst.fail(b) {
                preds[f.index()].push((bi as u32, j as u32));
            }
        }
        for t in targets(b, bi) {
            preds[t].push((bi as u32, insts.len() as u32));
        }
    }
    preds
}

/// The first use of a slot on a path where its last `StorageLive` or
/// `StorageDead` is a `StorageDead`, as (use point, slot).
pub(crate) fn use_after_end(b: &Body) -> Option<(Point, usize)> {
    use_after_end_with_budget(b, &crate::decode::Debit::unlimited())
        .expect("unlimited budget never fails")
}

/// For each slot used on a path where its last `StorageLive` or `StorageDead` is a
/// `StorageDead`, one such use, as (use point, slot); only the first when `first`.
fn uses_after_end(b: &Body, slots: &Slots, first: bool) -> Vec<(Point, usize)> {
    let mut found = vec![];
    let preds = predecessors(b);
    let insts = |bi: u32| {
        b.blocks[bi as usize]
            .insts
            .get(&b.insts)
            .unwrap_or_default()
    };
    for s in 0..b.slots.len() {
        if slots.deads[s].is_empty() {
            continue;
        }
        let slot = SlotId(s as u32);
        // Walk back from each use to the nearest begin or end on every path.
        let mut scanned: HashMap<u32, u32> = HashMap::new();
        'uses: for &(ub, ui) in &slots.uses[s] {
            let mut work = vec![(ub, ui)];
            while let Some((bi, upto)) = work.pop() {
                if scanned.get(&bi).is_some_and(|&u| u >= upto) {
                    continue;
                }
                scanned.insert(bi, upto);
                let mut open = true;
                for inst in insts(bi)[..upto as usize].iter().rev() {
                    match *inst {
                        Inst::StorageLive { slot: x } if x == slot => {
                            open = false;
                            break;
                        }
                        Inst::StorageDead { slot: x } if x == slot => {
                            found.push(((ub, ui), s));
                            if first {
                                return found;
                            }
                            break 'uses;
                        }
                        _ => {}
                    }
                }
                if open {
                    work.extend(preds[bi as usize].iter().copied());
                }
            }
        }
    }
    found
}

/// Merges slots of one declared type whose needed ranges never meet. A slot is
/// needed at a point when some path reaches the point from an access of it and
/// goes on to another (its `StorageLive`, `StorageDead` and uses are accesses;
/// failure edges count, at block grain). A slot a pointer may leave the body
/// through keeps its own home. A home that takes another slot keeps its
/// `StorageLive`s and drops its `StorageDead`s, so no end of one slot ends a
/// slot sharing its home. First fit in slot order: the same body always gives
/// the same result.
pub fn share_slots(b: &mut Body, signatures: &[Signature]) {
    let k = b.slots.len();
    let mut types = HashSet::new();
    if !b.slots.iter().any(|t| !types.insert(*t)) {
        return;
    }
    let from = provenance(b);
    let mut escapes = escaping(b, signatures, &from, Loans::Escape);
    for address in b.frame_obligations() {
        if let AddressBase::Slot(slot) = b.addresses[address.index()].base {
            escapes[slot.index()] = true;
        }
    }
    // Candidates: slots that keep no address past the body and have a partner of
    // their type. Only they are tracked; every other slot keeps its home.
    let mut count: HashMap<PlaceTy, u32> = HashMap::new();
    for s in (0..k).filter(|&s| !escapes[s]) {
        *count.entry(b.slots[s]).or_default() += 1;
    }
    let candidates: Vec<usize> = (0..k)
        .filter(|&s| !escapes[s] && count[&b.slots[s]] > 1)
        .collect();
    if candidates.is_empty() {
        return;
    }
    let n = candidates.len();
    let ranges = needed_ranges(b, &from, &candidates);
    let mut map: Vec<u32> = (0..k as u32).collect();
    let mut shared = vec![false; k];
    // Each home's needed ranges, disjoint and sorted by (block, from); indexed by
    // the home's candidate number.
    let mut occupied: Vec<Vec<(u32, u32, u32)>> = vec![vec![]; n];
    let mut classes: HashMap<PlaceTy, Vec<u32>> = HashMap::new();
    for (c, &s) in candidates.iter().enumerate() {
        if ranges[c].is_empty() {
            continue;
        }
        let class = classes.entry(b.slots[s]).or_default();
        let free = |h: u32| {
            let taken = &occupied[h as usize];
            ranges[c].iter().all(|&(rb, from, to)| {
                let i = taken.partition_point(|&(tb, tf, _)| (tb, tf) <= (rb, to));
                i == 0 || taken[i - 1].0 != rb || taken[i - 1].2 < from
            })
        };
        let into = class.iter().copied().find(|&h| free(h)).unwrap_or_else(|| {
            class.push(c as u32);
            c as u32
        });
        let home = candidates[into as usize];
        if home != s {
            map[s] = home as u32;
            shared[home] = true;
        }
        let taken = &mut occupied[into as usize];
        for &(rb, from, to) in &ranges[c] {
            let i = taken.partition_point(|&(tb, tf, _)| (tb, tf) < (rb, from));
            taken.insert(i, (rb, from, to));
        }
    }
    if !shared.iter().any(|&x| x) {
        return;
    }
    // Renumber the slots that remain, in order.
    let mut index = vec![u32::MAX; k];
    let mut kept = vec![];
    for s in 0..k {
        if map[s] == s as u32 {
            index[s] = kept.len() as u32;
            kept.push(b.slots[s]);
        }
    }
    let home = |s: SlotId| map[s.index()] as usize;
    for info in &mut b.addresses {
        if let AddressBase::Slot(s) = &mut info.base {
            *s = SlotId(index[home(*s)]);
        }
    }
    let mut insts = Vec::with_capacity(b.insts.len());
    let mut anchors = Vec::with_capacity(b.insts.len());
    for bi in 0..b.blocks.len() {
        let r = b.blocks[bi].insts;
        let at = insts.len() as u32;
        for j in r.start as usize..(r.start + r.len) as usize {
            let mut inst = b.insts[j].clone();
            match &mut inst {
                Inst::StorageDead { slot } if shared[home(*slot)] => continue,
                Inst::StorageLive { slot } | Inst::StorageDead { slot } => {
                    *slot = SlotId(index[home(*slot)]);
                }
                _ => {}
            }
            insts.push(inst);
            anchors.push(b.inst_anchors[j]);
        }
        b.blocks[bi].insts = PoolRange::new(at, insts.len() as u32 - at);
    }
    b.insts = insts;
    b.inst_anchors = anchors;
    b.slots = kept;
}

/// Slots a pointer into may outlive the body's names: a pointer made from an
/// address based on the slot that is stored, computed with, passed to a call
/// other than as its destination, branched on or returned.
/// How [`escaping`] treats a slot's address passed to a call.
#[derive(Clone, Copy, PartialEq)]
enum Loans {
    /// Every address passed to a call escapes.
    Escape,
    /// A loan to a direct call lasts the call (crate docs, "Loans").
    Direct,
    /// As `Direct`, and so does the destination loan of a channel receive:
    /// the engine writes the item into it during the call and keeps no
    /// pointer to it after, and its result is a flag, not a place.
    Scoped,
}

fn escaping(b: &Body, signatures: &[Signature], from: &[Vec<u32>], loans: Loans) -> Vec<bool> {
    let n = b.values.len();
    let mut out = vec![false; b.slots.len()];
    let mut mark = |name: usize| {
        for &s in &from[name] {
            out[s as usize] = true;
        }
    };
    for inst in &b.insts {
        match *inst {
            Inst::Address { .. } | Inst::Pointer { .. } => {}
            Inst::Call { call } => {
                let site = &b.calls[call.index()];
                if let Callee::Indirect(x) | Callee::PackExport(x) = site.callee {
                    mark(x.index());
                }
                let sig = signatures.get(site.signature.index());
                let dest = sig.is_some_and(|g| g.dest.is_some());
                let received = loans == Loans::Scoped
                    && matches!(
                        site.callee,
                        Callee::Runtime(
                            RtOp::Receive { .. }
                                | RtOp::ReceiveUntil { .. }
                                | RtOp::TryReceive { .. }
                        )
                    );
                let lent =
                    received || loans != Loans::Escape && matches!(site.callee, Callee::Direct(_));
                // A `mut` loan to a call with results may leave a result
                // anchored to the place (crate docs, "Loans", Ruling 7).
                let anchored =
                    !received && sig.is_some_and(|g| g.dest.is_some() || !g.results.is_empty());
                for (i, arg) in site
                    .args
                    .get(&b.pools.args)
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                {
                    if dest && i == 0 && (lent || loans == Loans::Escape) {
                        continue;
                    }
                    if lent && matches!(arg, Arg::Address(_)) {
                        let param = sig.and_then(|g| g.params.get(i - usize::from(dest)));
                        let held = anchored
                            && matches!(
                                param,
                                Some(Param::Address {
                                    loan: Loan::Mut,
                                    ..
                                })
                            );
                        if !held {
                            continue;
                        }
                    }
                    mark(match *arg {
                        Arg::Value(x) => x.index(),
                        Arg::Address(x) => n + x.index(),
                    });
                }
            }
            _ => {
                // An address operand reads or writes the slot's bytes; a value
                // operand carrying a pointer into it carries the pointer on.
                for name in operands(b, inst).0 {
                    if name < n {
                        mark(name);
                    }
                }
            }
        }
    }
    for (bi, t) in b.terminators.iter().enumerate() {
        match t {
            Terminator::Branch { condition, .. } => mark(condition.index()),
            Terminator::Switch { value, .. } => mark(value.index()),
            Terminator::Return(r) => {
                for v in r.get(&b.pools.values).unwrap_or_default() {
                    mark(v.index());
                }
            }
            Terminator::Raise { .. } => {
                for name in terminator_uses(b, bi) {
                    mark(name);
                }
            }
            Terminator::Jump(_) | Terminator::Propagate | Terminator::Unreachable => {}
        }
    }
    out
}

/// Slots whose storage an engine may end at `StorageDead` and begin, unwritten,
/// at `StorageLive` (LLVM lifetime markers), so that slots never in use at once
/// share frame bytes whatever their types. A slot qualifies when its address
/// leaves the body only as a loan whose extent is the call: to a direct call
/// (crate docs, "Loans"), but not as a `mut` loan to one with results, which
/// may stay anchored to the place; or as a channel receive's destination. And
/// every use must follow a `StorageLive` on every path from the entry, failure
/// edges included.
///
/// A qualifying slot also ends where it stops being needed
/// ([`ScopedSlots::ends_at_entry`], [`ScopedSlots::ends_after`]): at a point
/// from which no path reaches a use of it before its next `StorageLive` or
/// `StorageDead`, failure edges included. Every access of the slot is one of
/// its uses, and no use follows a `StorageDead` without a new `StorageLive`
/// (wf), so nothing reaches its bytes while it is ended. The exception is a
/// slot lent `view` to a call with a pointer result or a destination: the
/// result may be anchored to the place (OWN-20) and its uses are not the
/// slot's, so that slot ends only at its `StorageDead`s.
///
/// Last, no `StorageLive` of a slot may be reached while it is live. A slot
/// that ends where it stops being needed meets this always, since a
/// `StorageLive` is no use. It matters because wf counts a `mut` loan, a
/// `pointer` and any overlapping store as a write of the whole place: a slot
/// begun again while live would let a read after the begin reach bytes from
/// before it, where an engine that begins it unwritten has none.
///
/// The result also holds the slots that may be live at each block's entry, so
/// an engine ends at a return only the scoped slots that may be live there.
pub fn scoped_slots(b: &Body, signatures: &[Signature]) -> ScopedSlots {
    let k = b.slots.len();
    let words = k.div_ceil(64);
    if !b
        .insts
        .iter()
        .any(|i| matches!(i, Inst::StorageLive { .. }))
    {
        return ScopedSlots::none(k);
    }
    let from = provenance(b);
    let mut out: Vec<bool> = escaping(b, signatures, &from, Loans::Scoped)
        .into_iter()
        .map(|e| !e)
        .collect();
    let slots = slots_with(b, &from);
    let register = register_slots(b);
    for s in 0..k {
        if slots.lives[s].is_empty() || register[s] {
            out[s] = false;
        }
    }
    let bit = |s: usize| (s / 64, 1u64 << (s % 64));
    // The slots each point uses.
    let mut used: HashMap<Point, Vec<usize>> = HashMap::new();
    for s in (0..k).filter(|&s| out[s]) {
        for &p in &slots.uses[s] {
            used.entry(p).or_default().push(s);
        }
    }
    // A result anchored to a `view` argument (OWN-20) may point into the lent
    // place, and provenance does not follow call results: a slot lent `view`
    // to a call whose results include a pointer, or that has a destination,
    // ends only at its `StorageDead`s. A `mut` loan to such a call already
    // escapes (Ruling 7), and a `take` place is consumed.
    let anchored = anchored_views(b, signatures, &from);
    let early: Vec<bool> = out.iter().zip(&anchored).map(|(&o, &a)| o && !a).collect();
    let mut ends = storage_ends(b, &early, &used, words);
    // Live on every path into each block (`must`, a meet) and on some path
    // (`may`, a join).
    let (must, may) = live_at_entry(b, words, &ends);
    // The ends were placed against what may be live without them; keep only
    // those at blocks a slot may reach live.
    for (e, m) in ends.entry.iter_mut().zip(&may) {
        *e &= m;
    }
    for (bi, blk) in b.blocks.iter().enumerate() {
        let mut sure = must[bi * words..(bi + 1) * words].to_vec();
        let mut maybe = may[bi * words..(bi + 1) * words].to_vec();
        for w in 0..words {
            sure[w] &= !ends.entry[bi * words + w];
            maybe[w] &= !ends.entry[bi * words + w];
        }
        let insts = blk.insts.get(&b.insts).unwrap_or_default();
        for j in 0..=insts.len() {
            for &s in used.get(&(bi as u32, j as u32)).into_iter().flatten() {
                let (w, m) = bit(s);
                if sure[w] & m == 0 {
                    out[s] = false;
                }
            }
            match insts.get(j) {
                Some(&Inst::StorageLive { slot }) => {
                    let (w, m) = bit(slot.index());
                    if maybe[w] & m != 0 {
                        out[slot.index()] = false;
                    }
                    sure[w] |= m;
                    maybe[w] |= m;
                }
                Some(&Inst::StorageDead { slot }) => {
                    let (w, m) = bit(slot.index());
                    sure[w] &= !m;
                    maybe[w] &= !m;
                }
                _ => {}
            }
            if j < insts.len() {
                for s in ends.after(blk.insts.start as usize + j) {
                    let (w, m) = bit(s);
                    sure[w] &= !m;
                    maybe[w] &= !m;
                }
            }
        }
    }
    ScopedSlots {
        slots: out,
        words,
        may,
        ends,
    }
}

/// Where each slot of `candidates` stops being needed: no path from the point
/// reaches a use of it (`used`) before a `StorageLive` or `StorageDead` of it.
/// An end is placed only where the slot may be live, right after the
/// instruction past which it stops being needed, or at the entry of a block
/// that does not need it.
fn storage_ends(
    b: &Body,
    candidates: &[bool],
    used: &HashMap<Point, Vec<usize>>,
    words: usize,
) -> Ends {
    let nb = b.blocks.len();
    let mut ends = Ends {
        entry: vec![0; nb * words],
        after: Vec::new(),
    };
    if !candidates.iter().any(|&c| c) {
        return ends;
    }
    let bit = |s: usize| (s / 64, 1u64 << (s % 64));
    let mut mask = vec![0u64; words];
    for s in (0..candidates.len()).filter(|&s| candidates[s]) {
        let (w, m) = bit(s);
        mask[w] |= m;
    }
    let add_used = |state: &mut [u64], at: Point| {
        for &s in used.get(&at).into_iter().flatten() {
            let (w, m) = bit(s);
            state[w] |= m;
        }
    };
    // Needed before each of block `bi`'s points, given what each block needs
    // at its entry: `at[j]` before instruction `j`, `at[len]` before the
    // terminator. A failure edge leaves before its instruction.
    let walk = |bi: usize, need: &[u64], at: &mut Vec<Vec<u64>>| {
        let insts = b.blocks[bi].insts.get(&b.insts).unwrap_or_default();
        let mut state = vec![0u64; words];
        for t in targets(b, bi) {
            for w in 0..words {
                state[w] |= need[t * words + w];
            }
        }
        add_used(&mut state, (bi as u32, insts.len() as u32));
        at.resize(insts.len() + 1, Vec::new());
        at[insts.len()] = state.clone();
        for (j, inst) in insts.iter().enumerate().rev() {
            if let Inst::StorageLive { slot } | Inst::StorageDead { slot } = *inst {
                let (w, m) = bit(slot.index());
                state[w] &= !m;
            }
            add_used(&mut state, (bi as u32, j as u32));
            if let Some(f) = inst.fail(b) {
                for w in 0..words {
                    state[w] |= need[f.index() * words + w];
                }
            }
            for w in 0..words {
                state[w] &= mask[w];
            }
            at[j] = state.clone();
        }
    };
    let mut need = vec![0u64; nb * words];
    let mut at = Vec::new();
    let mut changed = true;
    while changed {
        changed = false;
        for bi in (0..nb).rev() {
            walk(bi, &need, &mut at);
            if need[bi * words..(bi + 1) * words] != at[0][..] {
                need[bi * words..(bi + 1) * words].copy_from_slice(&at[0]);
                changed = true;
            }
        }
    }
    // Walk forward keeping what may be live within what is needed.
    let (_, may) = live_at_entry(b, words, &ends);
    for bi in 0..nb {
        walk(bi, &need, &mut at);
        let mut maybe: Vec<u64> = (0..words).map(|w| may[bi * words + w] & mask[w]).collect();
        for w in 0..words {
            ends.entry[bi * words + w] = maybe[w] & !at[0][w];
            maybe[w] &= at[0][w];
        }
        let blk = &b.blocks[bi];
        for (j, inst) in blk
            .insts
            .get(&b.insts)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            match *inst {
                Inst::StorageLive { slot } if candidates[slot.index()] => {
                    let (w, m) = bit(slot.index());
                    maybe[w] |= m;
                }
                Inst::StorageDead { slot } => {
                    let (w, m) = bit(slot.index());
                    maybe[w] &= !m;
                }
                _ => {}
            }
            for w in 0..words {
                let mut gone = maybe[w] & !at[j + 1][w];
                maybe[w] &= at[j + 1][w];
                while gone != 0 {
                    let s = w * 64 + gone.trailing_zeros() as usize;
                    gone &= gone - 1;
                    ends.after.push((blk.insts.start + j as u32, s as u32));
                }
            }
        }
    }
    ends.after.sort_unstable();
    // An end lets a later begin take the bytes; where no `StorageLive` can
    // follow, the slot may as well last to the return, which ends it anyway.
    let begins = |i: &Inst| matches!(i, Inst::StorageLive { slot } if candidates[slot.index()]);
    let mut ahead = vec![false; nb];
    let mut changed = true;
    while changed {
        changed = false;
        for bi in (0..nb).rev() {
            let insts = b.blocks[bi].insts.get(&b.insts).unwrap_or_default();
            let reach = targets(b, bi).into_iter().any(|t| ahead[t])
                || insts
                    .iter()
                    .any(|i| begins(i) || i.fail(b).is_some_and(|f| ahead[f.index()]));
            if reach && !ahead[bi] {
                ahead[bi] = true;
                changed = true;
            }
        }
    }
    for bi in (0..nb).filter(|&bi| !ahead[bi]) {
        ends.entry[bi * words..(bi + 1) * words].fill(0);
    }
    let mut kept = Vec::with_capacity(ends.after.len());
    for bi in 0..nb {
        let blk = &b.blocks[bi];
        let insts = blk.insts.get(&b.insts).unwrap_or_default();
        let mut follows = targets(b, bi).into_iter().any(|t| ahead[t]);
        let first = blk.insts.start as usize;
        let mut here: Vec<(u32, u32)> = vec![];
        for j in (0..insts.len()).rev() {
            let at = (first + j) as u32;
            if follows {
                let i = ends.after.partition_point(|&(x, _)| x < at);
                here.extend(ends.after[i..].iter().take_while(|&&(x, _)| x == at));
            }
            follows |= begins(&insts[j]) || insts[j].fail(b).is_some_and(|f| ahead[f.index()]);
        }
        kept.extend(here);
    }
    kept.sort_unstable();
    ends.after = kept;
    ends
}

/// The ends [`storage_ends`] places: at each block's entry, `words` words per
/// block, and after instructions, as (instruction index in the body, slot)
/// sorted by index.
struct Ends {
    entry: Vec<u64>,
    after: Vec<(u32, u32)>,
}

impl Ends {
    fn after(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        let i = self.after.partition_point(|&(at, _)| (at as usize) < index);
        self.after[i..]
            .iter()
            .take_while(move |&&(at, _)| at as usize == index)
            .map(|&(_, s)| s as usize)
    }
}

/// Slots that hold one scalar and are reached only by whole-place reads and
/// stores through unstepped addresses of their own: no step, derived address,
/// `pointer`, call argument, copy or other use. An engine keeps such a slot in a
/// register, so it holds no frame bytes to share and needs no storage markers.
fn register_slots(b: &Body) -> Vec<bool> {
    let n = b.values.len();
    let mut out: Vec<bool> = b
        .slots
        .iter()
        .map(|t| matches!(t, PlaceTy::Value(_)))
        .collect();
    // The slot each address is the bare home of, if any.
    let home: Vec<Option<usize>> = b
        .addresses
        .iter()
        .map(|info| match info.base {
            AddressBase::Slot(s) => Some(s.index()),
            _ => None,
        })
        .collect();
    for info in &b.addresses {
        match info.base {
            AddressBase::Slot(s) if info.steps.len != 0 => out[s.index()] = false,
            AddressBase::Address(x) => {
                if let Some(s) = home[x.index()] {
                    out[s] = false;
                }
            }
            _ => {}
        }
    }
    let mut other = |name: usize| {
        if let Some(s) = name.checked_sub(n).and_then(|a| home[a]) {
            out[s] = false;
        }
    };
    for inst in &b.insts {
        match *inst {
            Inst::Read { .. } | Inst::Store { .. } | Inst::Address { .. } => {}
            _ => operands(b, inst).0.into_iter().for_each(&mut other),
        }
    }
    for bi in 0..b.terminators.len() {
        terminator_uses(b, bi).into_iter().for_each(&mut other);
    }
    out
}

/// [`scoped_slots`]'s answer.
pub struct ScopedSlots {
    /// Whether each slot is scoped.
    pub slots: Vec<bool>,
    words: usize,
    /// The slots live on some path into each block, `words` words per block;
    /// empty when the body begins no slot.
    may: Vec<u64>,
    ends: Ends,
}

impl ScopedSlots {
    /// No scoped slot among `k`.
    pub fn none(k: usize) -> Self {
        ScopedSlots {
            slots: vec![false; k],
            words: k.div_ceil(64),
            may: Vec::new(),
            ends: Ends {
                entry: Vec::new(),
                after: Vec::new(),
            },
        }
    }
    /// The scoped slots whose storage ends at `block`'s entry, before its first
    /// instruction, one bit per slot.
    pub fn ends_at_entry(&self, block: usize) -> Vec<u64> {
        if self.ends.entry.is_empty() {
            return vec![0; self.words];
        }
        (0..self.words)
            .map(|w| {
                let mut x = self.ends.entry[block * self.words + w];
                for b in 0..64 {
                    if x & (1 << b) != 0 && !self.slots[w * 64 + b] {
                        x &= !(1 << b);
                    }
                }
                x
            })
            .collect()
    }
    /// The scoped slots whose storage ends right after the body's instruction
    /// `index`.
    pub fn ends_after(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        self.ends.after(index).filter(|&s| self.slots[s])
    }
    /// The slots that may be live at `block`'s entry, one bit per slot.
    pub fn may_live_at(&self, block: usize) -> Vec<u64> {
        if self.may.is_empty() {
            return vec![0; self.words];
        }
        self.may[block * self.words..(block + 1) * self.words].to_vec()
    }
}

/// Each block's slots live at its entry, as bit sets of `words` words: live on
/// every incoming path (`must`) and on some (`may`). `StorageLive` begins a
/// slot, `StorageDead` ends it, and a failure edge leaves before its failing
/// instruction.
///
/// Each block is read once into its exits: a target and the slots begun and
/// ended on the way from the block's entry to that edge, so the set leaving
/// along it is `(entry & !ended) | begun`. A worklist then revisits a block
/// only when its entry set changes.
fn live_at_entry(b: &Body, words: usize, ends: &Ends) -> (Vec<u64>, Vec<u64>) {
    let nb = b.blocks.len();
    // `marks` holds (begun, ended) pairs of `words` words each; every exit
    // names the pair in force when it leaves.
    let mut marks: Vec<u64> = Vec::new();
    let mut exits: Vec<Vec<(usize, usize)>> = Vec::with_capacity(nb);
    for bi in 0..nb {
        let mut out = Vec::new();
        let mut current = marks.len() / (2 * words);
        marks.resize(marks.len() + 2 * words, 0);
        let mut fresh = true;
        let first = b.blocks[bi].insts.start as usize;
        if let Some(entry) = ends.entry.get(bi * words..(bi + 1) * words) {
            let base = current * 2 * words;
            for w in 0..words {
                marks[base + words + w] |= entry[w];
            }
        }
        for (j, inst) in b.blocks[bi]
            .insts
            .get(&b.insts)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            if let Some(f) = inst.fail(b) {
                out.push((f.index(), current));
                fresh = false;
            }
            let mark = match *inst {
                Inst::StorageLive { slot } => Some((slot.index(), true)),
                Inst::StorageDead { slot } => Some((slot.index(), false)),
                _ => None,
            };
            for (slot, begins) in mark
                .into_iter()
                .chain(ends.after(first + j).map(|s| (s, false)))
            {
                if !fresh {
                    let base = current * 2 * words;
                    marks.extend_from_within(base..base + 2 * words);
                    current += 1;
                    fresh = true;
                }
                let (w, m) = (slot / 64, 1u64 << (slot % 64));
                let base = current * 2 * words;
                if begins {
                    marks[base + w] |= m;
                } else {
                    marks[base + w] &= !m;
                    marks[base + words + w] |= m;
                }
            }
        }
        for t in targets(b, bi) {
            out.push((t, current));
        }
        exits.push(out);
    }
    let solve = |every: bool| {
        let mut at = vec![if every { !0u64 } else { 0 }; nb * words];
        at[..words].fill(0);
        let mut queued = vec![true; nb];
        let mut work: std::collections::VecDeque<usize> = (0..nb).collect();
        while let Some(bi) = work.pop_front() {
            queued[bi] = false;
            for &(to, mark) in &exits[bi] {
                let (begun, ended) =
                    marks[mark * 2 * words..(mark + 1) * 2 * words].split_at(words);
                let mut changed = false;
                for w in 0..words {
                    let state = (at[bi * words + w] & !ended[w]) | begun[w];
                    let old = at[to * words + w];
                    let next = if every { old & state } else { old | state };
                    if next != old {
                        at[to * words + w] = next;
                        changed = true;
                    }
                }
                if changed && !queued[to] {
                    queued[to] = true;
                    work.push_back(to);
                }
            }
        }
        at
    };
    (solve(true), solve(false))
}
