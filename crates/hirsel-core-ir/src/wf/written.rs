//! Every reachable read of a slot's contents follows, on every path to it from
//! the entry, a write that overlaps the place read, budgeted under [`Debit`].
use super::*;
use crate::decode::Debit;

/// A place in one of the body's slots, as far as its address names it statically:
/// the slot and its field, case-field and carrier-word steps, cut at the first
/// step whose offset is a run-time value (`index`, `element`, `byte`), which then
/// stands for some part of the place before it.
type Place = (usize, Vec<(u8, u32, u32)>);

#[derive(Default)]
struct Touch {
    reads: Vec<Place>,
    writes: Vec<Place>,
    resets: Vec<usize>,
}

fn place_of(b: &Body, a: AddressId, reads: &mut Vec<Place>) -> Option<Place> {
    let info = b.addresses.get(a.index())?;
    let mut at = match info.base {
        AddressBase::Slot(s) => Some((s.index(), vec![])),
        AddressBase::Value(_) => None,
        AddressBase::Address(x) => place_of(b, x, reads),
    };
    let mut cut = false;
    for s in info.steps.get(&b.pools.steps).unwrap_or_default() {
        let Some(p) = at.as_mut() else { break };
        match *s {
            Step::Deref => {
                reads.push(p.clone());
                at = None;
            }
            _ if cut => {}
            Step::Field(f) => p.1.push((0, f, 0)),
            Step::CaseField { case, field } => p.1.push((1, case, field)),
            Step::Seq(w) => p.1.push((2, w as u32, 0)),
            Step::Index(_) | Step::Element(_) | Step::Byte(_) => cut = true,
        }
    }
    at
}

fn no_bytes(m: &Module, ty: PlaceTy) -> bool {
    fn field(m: &Module, f: &hirsel_core_layout::FieldTy) -> bool {
        use hirsel_core_layout::FieldTy as F;
        match f {
            F::Inline(r) => record(m, r.index()),
            F::Array(x, n) => *n == 0 || field(m, x),
            _ => false,
        }
    }
    fn record(m: &Module, t: usize) -> bool {
        m.types.get(t).is_some_and(|t| match &t.body {
            hirsel_core_layout::Body::Record(f) => f.iter().all(|f| field(m, &f.ty)),
            hirsel_core_layout::Body::Variant(_) => false,
        })
    }
    match ty {
        PlaceTy::Declared(t) => record(m, t.index()),
        PlaceTy::Value(_) => false,
    }
}

fn touch(m: &Module, b: &Body, i: &Inst) -> Touch {
    let mut t = Touch::default();
    let read = |t: &mut Touch, a: AddressId| {
        let mut r = vec![];
        let p = place_of(b, a, &mut r);
        t.reads.extend(r);
        p
    };
    let bytes = |a: AddressId, p: Option<Place>| {
        p.filter(|_| {
            !b.addresses
                .get(a.index())
                .is_some_and(|i| no_bytes(m, i.ty))
        })
    };
    match *i {
        Inst::Address { address, .. } => {
            read(&mut t, address);
        }
        Inst::Read { address, .. } | Inst::Case { address, .. } | Inst::Drop { address, .. } => {
            let p = read(&mut t, address);
            t.reads.extend(bytes(address, p));
        }
        Inst::Store { address, .. }
        | Inst::SetCase { address, .. }
        | Inst::PutLanes { to: address, .. } => {
            let p = read(&mut t, address);
            t.writes.extend(p);
        }
        Inst::Copy { to, from, .. } | Inst::Move { to, from, .. } => {
            let p = read(&mut t, from);
            t.reads.extend(bytes(from, p));
            let p = read(&mut t, to);
            t.writes.extend(p);
        }
        Inst::Grow { carrier, .. } => {
            let p = read(&mut t, carrier);
            t.reads.extend(p.clone());
            t.writes.extend(p);
        }
        Inst::Pointer { address, .. } => {
            if let Some((s, _)) = read(&mut t, address) {
                t.writes.push((s, vec![]));
            }
        }
        Inst::StorageLive { slot } | Inst::StorageDead { slot } => t.resets.push(slot.index()),
        Inst::Call { call } => {
            let Some(site) = b.calls.get(call.index()) else {
                return t;
            };
            let Some(sig) = m.signatures.get(site.signature.index()) else {
                return t;
            };
            let args = site.args.get(&b.pools.args).unwrap_or_default();
            let dest = usize::from(sig.dest.is_some());
            for (k, arg) in args.iter().enumerate() {
                let Arg::Address(a) = *arg else { continue };
                let p = read(&mut t, a);
                if k < dest {
                    t.writes.extend(p);
                    continue;
                }
                match sig.params.get(k - dest) {
                    Some(Param::Address {
                        loan: Loan::Mut, ..
                    }) => t.writes.extend(p),
                    _ => t.reads.extend(bytes(a, p)),
                }
            }
        }
        _ => {}
    }
    t
}

pub(super) fn check(m: &Module, b: &Body, budget: &Debit<'_>) -> Result<(), Error> {
    let scratch = budget.scratch();
    scratch.check()?;
    let blocks = b.blocks.len();
    let insts = |bi: usize| b.blocks[bi].insts.get(&b.insts).unwrap_or_default();
    let mut touches: Vec<Vec<Touch>> = Vec::new();
    scratch.reserve(&mut touches, blocks)?;
    for bi in 0..blocks {
        scratch.check()?;
        let row_insts = insts(bi);
        let mut row = Vec::new();
        scratch.reserve(&mut row, row_insts.len())?;
        for i in row_insts {
            let t = touch(m, b, i);
            scratch.allocation::<Place>(t.reads.len() + t.writes.len())?;
            scratch.allocation::<usize>(t.resets.len())?;
            row.push(t);
        }
        touches.push(row);
    }
    let mut ids: std::collections::HashMap<Place, usize> = Default::default();
    let mut all: Vec<Place> = Vec::new();
    for t in touches.iter().flatten() {
        for p in t.reads.iter().chain(&t.writes) {
            if !ids.contains_key(p) {
                scratch.reserve(&mut all, 1)?;
                scratch.allocation::<(Place, usize)>(2)?;
                let id = all.len();
                all.push(p.clone());
                ids.insert(p.clone(), id);
            }
        }
    }
    let mut by_slot: Vec<Vec<usize>> = Vec::new();
    scratch.reserve(&mut by_slot, b.slots.len())?;
    by_slot.resize_with(b.slots.len(), Vec::new);
    for (k, p) in all.iter().enumerate() {
        if let Some(v) = by_slot.get_mut(p.0) {
            scratch.reserve(v, 1)?;
            v.push(k);
        }
    }
    let words = all.len().div_ceil(64).max(1);
    type Set = Vec<u64>;
    let has = |s: &Set, k: usize| s[k / 64] >> (k % 64) & 1 == 1;
    let apply = |now: &mut Set, t: &Touch| {
        for &s in &t.resets {
            for &k in by_slot.get(s).map_or(&[][..], |v| v) {
                now[k / 64] &= !(1 << (k % 64));
            }
        }
        for p in &t.writes {
            let k = ids[p];
            now[k / 64] |= 1 << (k % 64);
        }
    };
    let covered = |now: &Set, p: &Place| {
        by_slot.get(p.0).is_some_and(|ks| {
            ks.iter()
                .any(|&k| has(now, k) && (all[k].1.starts_with(&p.1) || p.1.starts_with(&all[k].1)))
        })
    };
    let is_flag = |a: AddressId| {
        b.addresses
            .get(a.index())
            .is_some_and(|i| i.ty == PlaceTy::Value(Ty::Scalar(hirsel_core_layout::Scalar::Bool)))
    };
    let whole = |a: AddressId| {
        place_of(b, a, &mut vec![])
            .filter(|p| p.1.is_empty() && is_flag(a))
            .map(|p| p.0)
    };
    let mut flag_of: std::collections::HashMap<usize, usize> = Default::default();
    let mut consts: std::collections::HashMap<usize, u64> = Default::default();
    for i in b.insts.iter() {
        match *i {
            Inst::Read {
                result, address, ..
            } => {
                if let Some(f) = whole(address) {
                    scratch.allocation::<(usize, usize)>(2)?;
                    flag_of.insert(result.index(), f);
                }
            }
            Inst::Const { result, bits } => {
                scratch.allocation::<(usize, u64)>(2)?;
                consts.insert(result.index(), bits);
            }
            _ => {}
        }
    }
    let mut guards: Vec<Option<Set>> = Vec::new();
    scratch.reserve(&mut guards, b.slots.len())?;
    guards.resize_with(b.slots.len(), || None);
    let solve = |guards: &Vec<Option<Set>>| -> Result<Vec<Option<Set>>, Error> {
        let pass = scratch.scratch();
        let mut start: Vec<Option<Set>> = Vec::new();
        pass.reserve(&mut start, blocks)?;
        start.resize_with(blocks, || None);
        let mut now = Vec::new();
        pass.reserve(&mut now, words)?;
        now.resize(words, 0u64);
        let mut taken = Vec::new();
        pass.reserve(&mut taken, words)?;
        taken.resize(words, 0u64);
        let mut queued = Vec::new();
        pass.reserve(&mut queued, blocks)?;
        queued.resize(blocks, false);
        let mut work = Vec::new();
        pass.reserve(&mut work, blocks)?;
        if blocks > 0 {
            let mut init = Vec::new();
            pass.reserve(&mut init, words)?;
            init.resize(words, 0u64);
            start[0] = Some(init);
            work.push(0usize);
            queued[0] = true;
        }
        while let Some(bi) = work.pop() {
            pass.check()?;
            queued[bi] = false;
            now.copy_from_slice(start[bi].as_ref().unwrap());
            let mut send = |t: usize, carried: &[u64]| -> Result<(), Error> {
                pass.check()?;
                if t == 0 {
                    return Ok(());
                }
                let changed = if let Some(old) = &mut start[t] {
                    let mut changed = false;
                    for (x, y) in old.iter_mut().zip(carried) {
                        let next = *x & *y;
                        changed |= *x != next;
                        *x = next;
                    }
                    changed
                } else {
                    let mut state = Vec::new();
                    pass.reserve(&mut state, words)?;
                    state.extend_from_slice(carried);
                    start[t] = Some(state);
                    true
                };
                if changed && !queued[t] {
                    queued[t] = true;
                    work.push(t);
                }
                Ok(())
            };
            for (k, i) in insts(bi).iter().enumerate() {
                if let Some(f) = i.fail(b) {
                    send(f.index(), &now)?;
                }
                apply(&mut now, &touches[bi][k]);
            }
            let edge = |e: &EdgeId| b.edges[e.index()].target.index();
            if let Terminator::Branch { condition, yes, no } = &b.terminators[bi] {
                taken.copy_from_slice(&now);
                if let Some(&f) = flag_of.get(&condition.index()) {
                    match &guards[f] {
                        None => taken.iter_mut().for_each(|w| *w = !0),
                        Some(g) => taken.iter_mut().zip(g).for_each(|(w, g)| *w |= g),
                    }
                }
                send(edge(yes), &taken)?;
                send(edge(no), &now)?;
            } else {
                for t in successors_of_terminator(b, bi) {
                    send(t, &now)?;
                }
            }
        }
        Ok(start)
    };
    let branches_on_flags = b.terminators.iter().any(|t| {
        matches!(t, Terminator::Branch { condition, .. } if flag_of.contains_key(&condition.index()))
    });
    let start = loop {
        scratch.check()?;
        let start = solve(&guards)?;
        if !branches_on_flags {
            break start;
        }
        let step = scratch.scratch();
        let mut next: Vec<Option<Set>> = Vec::new();
        step.reserve(&mut next, b.slots.len())?;
        next.resize_with(b.slots.len(), || None);
        let mut zero = Vec::new();
        step.reserve(&mut zero, words)?;
        zero.resize(words, 0u64);
        let mut meet = |f: usize, s: &[u64]| -> Result<(), Error> {
            if let Some(o) = &mut next[f] {
                for (x, y) in o.iter_mut().zip(s) {
                    *x &= *y;
                }
            } else {
                let mut copy = Vec::new();
                step.reserve(&mut copy, words)?;
                copy.extend_from_slice(s);
                next[f] = Some(copy);
            }
            Ok(())
        };
        let mut now = Vec::new();
        step.reserve(&mut now, words)?;
        now.resize(words, 0u64);
        for bi in 0..blocks {
            step.check()?;
            let Some(st) = start[bi].as_ref() else {
                continue;
            };
            now.copy_from_slice(st);
            for (k, i) in insts(bi).iter().enumerate() {
                match *i {
                    Inst::Store { address, value } => {
                        if let Some(f) = whole(address) {
                            match consts.get(&value.index()) {
                                Some(0) => {}
                                Some(_) => meet(f, &now)?,
                                None => meet(f, &zero)?,
                            }
                        }
                    }
                    _ => {
                        for p in &touches[bi][k].writes {
                            if p.1.is_empty()
                                && guards.get(p.0).is_some()
                                && own::is_flag_slot(b, p.0)
                            {
                                meet(p.0, &zero)?;
                            }
                        }
                    }
                }
                apply(&mut now, &touches[bi][k]);
            }
        }
        let mut normalized: Vec<Option<Set>> = Vec::new();
        step.reserve(&mut normalized, b.slots.len())?;
        for g in next {
            let set = match g {
                Some(s) => s,
                None => {
                    let mut z = Vec::new();
                    step.reserve(&mut z, words)?;
                    z.resize(words, 0u64);
                    z
                }
            };
            normalized.push(Some(set));
        }
        if normalized == guards {
            break start;
        }
        guards = normalized;
    };
    let show = |p: &Place| {
        let mut s = format!("s{}", p.0);
        for &(k, x, y) in &p.1 {
            match k {
                0 => s += &format!(".{x}"),
                1 => s += &format!(".case{x}.{y}"),
                _ => s += &format!(".seq{x}"),
            }
        }
        s
    };
    let mut now = Vec::new();
    scratch.reserve(&mut now, words)?;
    now.resize(words, 0u64);
    for bi in 0..blocks {
        scratch.check()?;
        let Some(st) = start[bi].as_ref() else {
            continue;
        };
        now.copy_from_slice(st);
        let check_read = |now: &Set, p: &Place| -> Result<(), Error> {
            need(covered(now, p), || {
                format!(
                    "b{bi}: {} is read on a path that does not write it",
                    show(p)
                )
            })
            .map_err(Error::Invalid)
        };
        for t in &touches[bi] {
            for p in &t.reads {
                check_read(&now, p)?;
            }
            apply(&mut now, t);
        }
        if let Terminator::Raise {
            cause: RaiseCause::Fail(a),
            ..
        } = b.terminators[bi]
        {
            let mut r = vec![];
            let p = place_of(b, a, &mut r);
            for p in r.iter().chain(&p) {
                check_read(&now, p)?;
            }
        }
    }
    Ok(())
}
