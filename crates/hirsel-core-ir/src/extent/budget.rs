use super::*;
use crate::decode::{Debit, DecodeResult};

/// Budgeted [`use_after_end`]: charges scratch allocations and checks deadline under `budget`.
pub(crate) fn use_after_end_with_budget(
    b: &Body,
    budget: &Debit<'_>,
) -> DecodeResult<Option<(Point, usize)>> {
    let scratch = budget.scratch();
    scratch.check()?;
    if !b
        .insts
        .iter()
        .any(|i| matches!(i, Inst::StorageDead { .. }))
    {
        return Ok(None);
    }
    let from = provenance_budgeted(b, &scratch)?;
    let slots = slots_with_budget(b, &from, &scratch)?;
    Ok(uses_after_end_budgeted(b, &slots, true, &scratch)?
        .into_iter()
        .next())
}

fn provenance_budgeted(b: &Body, scratch: &Debit<'_>) -> DecodeResult<Vec<Vec<u32>>> {
    let n = b.values.len();
    let total = n + b.addresses.len();
    let mut from: Vec<Vec<u32>> = Vec::new();
    scratch.reserve(&mut from, total)?;
    from.resize_with(total, Vec::new);
    let mut next: Vec<Vec<usize>> = Vec::new();
    scratch.reserve(&mut next, total)?;
    next.resize_with(total, Vec::new);
    let mut work = Vec::new();
    scratch.reserve(&mut work, b.addresses.len())?;
    for (i, info) in b.addresses.iter().enumerate() {
        scratch.check()?;
        match info.base {
            AddressBase::Slot(s) => {
                scratch.reserve(&mut from[n + i], 1)?;
                from[n + i].push(s.0);
                work.push(n + i);
            }
            AddressBase::Address(x) => {
                scratch.reserve(&mut next[n + x.index()], 1)?;
                next[n + x.index()].push(n + i);
            }
            AddressBase::Value(x) => {
                scratch.reserve(&mut next[x.index()], 1)?;
                next[x.index()].push(n + i);
            }
        }
    }
    if work.is_empty() {
        return Ok(from);
    }
    for inst in &b.insts {
        if let Inst::Pointer { result, address } = *inst {
            scratch.reserve(&mut next[n + address.index()], 1)?;
            next[n + address.index()].push(result.index());
        }
    }
    for e in &b.edges {
        scratch.check()?;
        let params = b.blocks[e.target.index()]
            .params
            .get(&b.pools.values)
            .unwrap_or_default();
        let args = e.args.get(&b.pools.values).unwrap_or_default();
        for (p, a) in params.iter().zip(args) {
            scratch.reserve(&mut next[a.index()], 1)?;
            next[a.index()].push(p.index());
        }
    }
    while let Some(src) = work.pop() {
        scratch.check()?;
        for &to in &next[src] {
            if to == src {
                continue;
            }
            let mut grew = false;
            for i in 0..from[src].len() {
                let s = from[src][i];
                if !from[to].contains(&s) {
                    scratch.reserve(&mut from[to], 1)?;
                    from[to].push(s);
                    grew = true;
                }
            }
            if grew {
                scratch.reserve(&mut work, 1)?;
                work.push(to);
            }
        }
    }
    Ok(from)
}

fn slots_with_budget(b: &Body, from: &[Vec<u32>], scratch: &Debit<'_>) -> DecodeResult<Slots> {
    let k = b.slots.len();
    let mut lives = Vec::new();
    scratch.reserve(&mut lives, k)?;
    lives.resize_with(k, Vec::new);
    let mut deads = Vec::new();
    scratch.reserve(&mut deads, k)?;
    deads.resize_with(k, Vec::new);
    let mut uses = Vec::new();
    scratch.reserve(&mut uses, k)?;
    uses.resize_with(k, Vec::new);
    let mut out = Slots { lives, deads, uses };
    let n = b.values.len();
    let mut seen: Vec<u32> = Vec::new();
    let mut note_one = |uses: &mut Vec<Vec<Point>>,
                        seen: &mut Vec<u32>,
                        name: usize,
                        at: Point|
     -> DecodeResult<()> {
        for &s in &from[name] {
            if !seen.contains(&s) {
                scratch.reserve(seen, 1)?;
                seen.push(s);
                scratch.reserve(&mut uses[s as usize], 1)?;
                uses[s as usize].push(at);
            }
        }
        Ok(())
    };
    for (bi, blk) in b.blocks.iter().enumerate() {
        scratch.check()?;
        let insts = blk.insts.get(&b.insts).unwrap_or_default();
        for (j, inst) in insts.iter().enumerate() {
            let at = (bi as u32, j as u32);
            match *inst {
                Inst::StorageLive { slot } => {
                    scratch.reserve(&mut out.lives[slot.index()], 1)?;
                    out.lives[slot.index()].push(at);
                }
                Inst::StorageDead { slot } => {
                    scratch.reserve(&mut out.deads[slot.index()], 1)?;
                    out.deads[slot.index()].push(at);
                }
                Inst::Address { address, .. } => {
                    seen.clear();
                    note_one(&mut out.uses, &mut seen, n + address.index(), at)?;
                }
                _ => {
                    seen.clear();
                    let mut err = Ok(());
                    visit_operands(
                        b,
                        inst,
                        |u| {
                            if err.is_ok() {
                                err = note_one(&mut out.uses, &mut seen, u, at);
                            }
                        },
                        |_| {},
                    );
                    err?;
                }
            }
        }
        let at = (bi as u32, insts.len() as u32);
        seen.clear();
        let mut err = Ok(());
        visit_terminator_uses(b, bi, |u| {
            if err.is_ok() {
                err = note_one(&mut out.uses, &mut seen, u, at);
            }
        });
        err?;
    }
    Ok(out)
}

fn uses_after_end_budgeted(
    b: &Body,
    slots: &Slots,
    first: bool,
    scratch: &Debit<'_>,
) -> DecodeResult<Vec<(Point, usize)>> {
    let mut found = Vec::new();
    let mut preds: Vec<Vec<Point>> = Vec::new();
    scratch.reserve(&mut preds, b.blocks.len())?;
    preds.resize_with(b.blocks.len(), Vec::new);
    for (bi, blk) in b.blocks.iter().enumerate() {
        scratch.check()?;
        let insts = blk.insts.get(&b.insts).unwrap_or_default();
        for (j, inst) in insts.iter().enumerate() {
            if let Some(f) = inst.fail(b) {
                scratch.reserve(&mut preds[f.index()], 1)?;
                preds[f.index()].push((bi as u32, j as u32));
            }
        }
        for t in targets(b, bi) {
            scratch.reserve(&mut preds[t], 1)?;
            preds[t].push((bi as u32, insts.len() as u32));
        }
    }
    let insts = |bi: u32| {
        b.blocks[bi as usize]
            .insts
            .get(&b.insts)
            .unwrap_or_default()
    };
    let mut scanned = Vec::new();
    scratch.reserve(&mut scanned, b.blocks.len())?;
    scanned.resize(b.blocks.len(), None::<u32>);
    let mut work = Vec::new();
    for s in 0..b.slots.len() {
        scratch.check()?;
        if slots.deads[s].is_empty() {
            continue;
        }
        let slot = SlotId(s as u32);
        scanned.fill(None);
        'uses: for &(ub, ui) in &slots.uses[s] {
            work.clear();
            scratch.reserve(&mut work, 1)?;
            work.push((ub, ui));
            while let Some((bi, upto)) = work.pop() {
                scratch.check()?;
                if scanned[bi as usize].is_some_and(|u| u >= upto) {
                    continue;
                }
                scanned[bi as usize] = Some(upto);
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
                                return Ok(found);
                            }
                            break 'uses;
                        }
                        _ => {}
                    }
                }
                if open {
                    scratch.reserve(&mut work, preds[bi as usize].len())?;
                    work.extend(preds[bi as usize].iter().copied());
                }
            }
        }
    }
    Ok(found)
}
