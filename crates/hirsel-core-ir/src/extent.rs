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

/// What an instruction defines and uses, as names: value `v` is `v`, address `a` is
/// `values + a`. An `address` instruction defines its address and uses its base and
/// step operands.
pub(crate) fn operands(b: &Body, i: &Inst) -> (Vec<usize>, Vec<usize>) {
    let n = b.values.len();
    let v = |v: ValueId| v.index();
    let a = |a: AddressId| n + a.index();
    let mut uses = vec![];
    let mut defs = vec![];
    match *i {
        Inst::Const { result, .. }
        | Inst::LayoutConst { result, .. }
        | Inst::ReadStatic { result, .. }
        | Inst::FunctionRef { result, .. }
        | Inst::DescriptorRef { result, .. }
        | Inst::RegionOpen { result } => defs.push(v(result)),
        Inst::Simd {
            result,
            first,
            second,
            ..
        } => {
            uses.extend([first].into_iter().chain(second).map(v));
            defs.push(v(result));
        }
        Inst::PutLanes { to, vector, count } => uses.extend([a(to), v(vector), v(count)]),
        Inst::Unary {
            result, operand, ..
        }
        | Inst::CompactWidth {
            result,
            word: operand,
            ..
        }
        | Inst::ScalarUnary {
            result, operand, ..
        } => {
            uses.push(v(operand));
            defs.push(v(result));
        }
        Inst::Binary {
            result,
            left,
            right,
            ..
        }
        | Inst::Compare {
            result,
            left,
            right,
            ..
        } => {
            uses.extend([v(left), v(right)]);
            defs.push(v(result));
        }
        Inst::Convert {
            result,
            flag,
            operand,
            ..
        } => {
            uses.push(v(operand));
            defs.extend([result].into_iter().chain(flag).map(v));
        }
        Inst::IntArith {
            result,
            flag,
            left,
            right,
            ..
        } => {
            uses.extend([v(left), v(right)]);
            defs.extend([result].into_iter().chain(flag).map(v));
        }
        Inst::MulAdd {
            result,
            left,
            right,
            addend,
        } => {
            uses.extend([v(left), v(right), v(addend)]);
            defs.push(v(result));
        }
        Inst::Address { address, .. } => {
            if let Some(info) = b.addresses.get(address.index()) {
                match info.base {
                    AddressBase::Slot(_) => {}
                    AddressBase::Value(x) => uses.push(v(x)),
                    AddressBase::Address(x) => uses.push(a(x)),
                }
                for s in info.steps.get(&b.pools.steps).unwrap_or_default() {
                    if let Step::Index(x) | Step::Element(x) | Step::Byte(x) = *s {
                        uses.push(v(x));
                    }
                }
            }
            defs.push(a(address));
        }
        Inst::Read {
            result, address, ..
        }
        | Inst::Pointer { result, address }
        | Inst::Case { result, address } => {
            uses.push(a(address));
            defs.push(v(result));
        }
        Inst::Store { address, value } => uses.extend([a(address), v(value)]),
        Inst::StorageLive { .. }
        | Inst::StorageDead { .. }
        | Inst::BuiltinUse { .. }
        | Inst::LimitExit
        | Inst::Step { .. } => {}
        Inst::UpdateMask { result, mask, .. } => {
            uses.push(v(mask));
            defs.push(v(result));
        }
        Inst::Call { call } => {
            if let Some(site) = b.calls.get(call.index()) {
                if let Callee::Indirect(x) = site.callee {
                    uses.push(v(x));
                }
                for arg in site.args.get(&b.pools.args).unwrap_or_default() {
                    uses.push(match *arg {
                        Arg::Value(x) => v(x),
                        Arg::Address(x) => a(x),
                    });
                }
                defs.extend(
                    site.results
                        .get(&b.pools.values)
                        .unwrap_or_default()
                        .iter()
                        .map(|&x| v(x)),
                );
            }
        }
        Inst::Alloc {
            result,
            available,
            region_entry,
            place,
            charge,
            overflow,
            size,
            ..
        } => {
            if let AllocPlace::Like(x)
            | AllocPlace::Region(x)
            | AllocPlace::Frame(x)
            | AllocPlace::LentFrame(x) = place
            {
                uses.push(v(x));
            }
            uses.extend([charge, size].into_iter().chain(overflow).map(v));
            defs.extend([result, available].into_iter().chain(region_entry).map(v));
        }
        Inst::Free { block, charge } => uses.extend([v(block), v(charge)]),
        Inst::FrameRefund { charge } => uses.push(v(charge)),
        Inst::PromoteFrame { carrier } => uses.push(a(carrier)),
        Inst::Copy { to, from, bytes } => uses.extend([a(to), a(from), v(bytes)]),
        Inst::Grow {
            result,
            available,
            carrier,
            charge,
            overflow,
            size,
            capacity,
            moved,
            refund,
            ..
        } => {
            uses.push(a(carrier));
            uses.extend(
                [charge, size, capacity, moved, refund]
                    .into_iter()
                    .chain(overflow)
                    .map(v),
            );
            defs.extend([v(result), v(available)]);
        }
        Inst::SetCase { address, .. } | Inst::Drop { address, .. } => uses.push(a(address)),
        Inst::RegionClose { region } => uses.push(v(region)),
        Inst::Trim {
            block,
            region,
            entry,
            previous,
            charge,
        } => uses.extend([block, region, entry, previous, charge].map(v)),
        Inst::LimitEnter {
            admitted,
            available,
            bytes,
        } => {
            uses.push(v(bytes));
            defs.extend([v(admitted), v(available)]);
        }
        Inst::Share { node, .. } | Inst::Release { node, .. } => uses.push(v(node)),
        Inst::TryUnwrap { result, node, .. } => {
            uses.push(v(node));
            defs.push(v(result));
        }
        Inst::Mark { address, count, .. } | Inst::Unmark { address, count, .. } => {
            uses.extend([a(address), v(count)])
        }
    }
    (uses, defs)
}

/// The names block `bi`'s terminator uses: its condition or scrutinee, its edges'
/// arguments, its results, a `raise fail` cause's address.
pub(crate) fn terminator_uses(b: &Body, bi: usize) -> Vec<usize> {
    let vals = |r: PoolRange<ValueId>| -> Vec<usize> {
        r.get(&b.pools.values)
            .unwrap_or_default()
            .iter()
            .map(|v| v.index())
            .collect()
    };
    let edge_args = |e: &EdgeId| vals(b.edges[e.index()].args);
    match &b.terminators[bi] {
        Terminator::Jump(e) => edge_args(e),
        Terminator::Branch { condition, yes, no } => [condition.index()]
            .into_iter()
            .chain(edge_args(yes))
            .chain(edge_args(no))
            .collect(),
        Terminator::Switch {
            value,
            arms,
            otherwise,
        } => [value.index()]
            .into_iter()
            .chain(
                arms.get(&b.pools.arms)
                    .unwrap_or_default()
                    .iter()
                    .flat_map(|arm| edge_args(&arm.edge)),
            )
            .chain(edge_args(otherwise))
            .collect(),
        Terminator::Raise { cause, .. } => match cause {
            RaiseCause::Fail(a) => vec![b.values.len() + a.index()],
            RaiseCause::Capacity {
                requested,
                available,
            } => vec![requested.index(), available.index()],
            _ => vec![],
        },
        Terminator::Return(r) => vals(*r),
        Terminator::Propagate | Terminator::Unreachable => vec![],
    }
}

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

#[cfg(any(test, feature = "wf"))]
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

#[cfg(any(test, feature = "wf"))]
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

#[cfg(any(test, feature = "wf"))]
/// The first use of a slot on a path where its last `StorageLive` or
/// `StorageDead` is a `StorageDead`, as (use point, slot).
pub(crate) fn use_after_end(b: &Body) -> Option<(Point, usize)> {
    if !b
        .insts
        .iter()
        .any(|i| matches!(i, Inst::StorageDead { .. }))
    {
        return None;
    }
    uses_after_end(b, &slots(b), true).into_iter().next()
}

#[cfg(any(test, feature = "wf"))]
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
    let escapes = escaping(b, signatures, &from, false);
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
    let slots = slots_with(b, &from);
    let nb = b.blocks.len();
    let words = n.div_ceil(64);
    let lens: Vec<u32> = b.blocks.iter().map(|blk| blk.insts.len).collect();
    // Each candidate's accesses by block: (block, first, last) index.
    let mut acc = vec![0u64; nb * words];
    let mut spans: Vec<Vec<(u32, u32, u32)>> = vec![vec![]; n];
    for (c, &s) in candidates.iter().enumerate() {
        let mut points: Vec<Point> = slots.lives[s]
            .iter()
            .chain(&slots.deads[s])
            .chain(&slots.uses[s])
            .copied()
            .collect();
        points.sort_unstable();
        for (bi, j) in points {
            acc[bi as usize * words + c / 64] |= 1 << (c % 64);
            match spans[c].last_mut() {
                Some(last) if last.0 == bi => last.2 = j,
                _ => spans[c].push((bi, j, j)),
            }
        }
    }
    let mut succ: Vec<Vec<usize>> = vec![vec![]; nb];
    for (bi, out) in succ.iter_mut().enumerate() {
        let code = b.blocks[bi].insts.get(&b.insts).unwrap_or_default();
        out.extend(code.iter().filter_map(|i| i.fail(b)).map(|f| f.index()));
        out.extend(targets(b, bi));
        out.sort_unstable();
        out.dedup();
    }
    // Accessed on some path before a block's entry, and after its exit.
    let mut before = vec![0u64; nb * words];
    let mut after = vec![0u64; nb * words];
    let mut changed = true;
    while changed {
        changed = false;
        for bi in 0..nb {
            for &t in &succ[bi] {
                for w in 0..words {
                    let into = before[bi * words + w] | acc[bi * words + w];
                    if into & !before[t * words + w] != 0 {
                        before[t * words + w] |= into;
                        changed = true;
                    }
                }
            }
        }
        for bi in (0..nb).rev() {
            for &t in &succ[bi] {
                for w in 0..words {
                    let into = after[t * words + w] | acc[t * words + w];
                    if into & !after[bi * words + w] != 0 {
                        after[bi * words + w] |= into;
                        changed = true;
                    }
                }
            }
        }
    }
    // Each candidate's needed ranges: (block, from, to), inclusive, the block's
    // length being its terminator; in block order, at most one a block.
    let mut ranges: Vec<Vec<(u32, u32, u32)>> = vec![vec![]; n];
    for bi in 0..nb {
        for w in 0..words {
            let (pre, post, here) = (
                before[bi * words + w],
                after[bi * words + w],
                acc[bi * words + w],
            );
            let mut live = (pre & post) | here;
            while live != 0 {
                let c = w * 64 + live.trailing_zeros() as usize;
                live &= live - 1;
                let bit = 1u64 << (c % 64);
                let span = (here & bit != 0).then(|| {
                    let i = spans[c].partition_point(|&(sb, _, _)| (sb as usize) < bi);
                    (spans[c][i].1, spans[c][i].2)
                });
                let lo = if pre & bit != 0 {
                    0
                } else {
                    span.map_or(0, |x| x.0)
                };
                let hi = if post & bit != 0 {
                    lens[bi]
                } else {
                    span.map_or(lens[bi], |x| x.1)
                };
                ranges[c].push((bi as u32, lo, hi));
            }
        }
    }
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
    for bi in 0..nb {
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
fn escaping(b: &Body, signatures: &[Signature], from: &[Vec<u32>], loans: bool) -> Vec<bool> {
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
                if let Callee::Indirect(x) = site.callee {
                    mark(x.index());
                }
                let sig = signatures.get(site.signature.index());
                let dest = sig.is_some_and(|g| g.dest.is_some());
                let lent = loans && matches!(site.callee, Callee::Direct(_));
                // A `mut` loan to a call with results may leave a result
                // anchored to the place (crate docs, "Loans", Ruling 7).
                let anchored = sig.is_some_and(|g| g.dest.is_some() || !g.results.is_empty());
                for (i, arg) in site
                    .args
                    .get(&b.pools.args)
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                {
                    if dest && i == 0 && (lent || !loans) {
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
/// share frame bytes whatever their types. A slot qualifies when:
/// - its address leaves the body only as a loan to a direct call, whose extent
///   is the call (crate docs, "Loans"), and not as a `mut` loan to a call with
///   results, which may stay anchored to the place;
/// - every use follows a `StorageLive` on every path from the entry, failure
///   edges included;
/// - no `StorageLive` of it is reached on any path while it is live. A
///   `StorageLive` begins the slot unwritten in every engine, but wf counts a
///   `mut` loan, a `pointer` and any overlapping store as a write of the whole
///   place, so a read after a re-begin may still reach bytes from before it.
///
/// No use follows a `StorageDead` without a new `StorageLive` (wf), so between
/// a slot's end and its next begin nothing reaches its bytes.
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
    let mut out: Vec<bool> = escaping(b, signatures, &from, true)
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
    // Live on every path into each block (`must`, a meet) and on some path
    // (`may`, a join).
    let (must, may) = live_at_entry(b, words);
    // The slots each point uses.
    let mut used: HashMap<Point, Vec<usize>> = HashMap::new();
    for s in (0..k).filter(|&s| out[s]) {
        for &p in &slots.uses[s] {
            used.entry(p).or_default().push(s);
        }
    }
    for (bi, blk) in b.blocks.iter().enumerate() {
        let mut sure = must[bi * words..(bi + 1) * words].to_vec();
        let mut maybe = may[bi * words..(bi + 1) * words].to_vec();
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
        }
    }
    ScopedSlots {
        slots: out,
        words,
        may,
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
}

impl ScopedSlots {
    /// No scoped slot among `k`.
    pub fn none(k: usize) -> Self {
        ScopedSlots {
            slots: vec![false; k],
            words: k.div_ceil(64),
            may: Vec::new(),
        }
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
fn live_at_entry(b: &Body, words: usize) -> (Vec<u64>, Vec<u64>) {
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
        for inst in b.blocks[bi].insts.get(&b.insts).unwrap_or_default() {
            if let Some(f) = inst.fail(b) {
                out.push((f.index(), current));
                fresh = false;
            }
            let (slot, begins) = match *inst {
                Inst::StorageLive { slot } => (slot.index(), true),
                Inst::StorageDead { slot } => (slot.index(), false),
                _ => continue,
            };
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
