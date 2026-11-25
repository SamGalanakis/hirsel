use super::*;

// A join can carry both states. Only FrameActive can refine that union,
// and a replacement or refund requires an obligation on every incoming path.
const EMPTY: u8 = 1;
const LIVE: u8 = 2;

pub(super) fn obligations(m: &Module, b: &Body) -> R {
    let frames = b.frame_obligations();
    if frames.is_empty() {
        return Ok(());
    }
    let mut indices = vec![None; b.slots.len()];
    let mut addresses = vec![0; b.addresses.len()];
    for (i, storage) in frames.into_iter().enumerate() {
        let info = &b.addresses[storage.index()];
        let AddressBase::Slot(slot) = info.base else {
            return Err("frame obligation must name a local slot".into());
        };
        need(
            info.steps.len == 0 && indices[slot.index()].replace(i).is_none(),
            || "frame obligation must name a unique whole slot".into(),
        )?;
        addresses[storage.index()] = i;
    }

    // Frame obligations belong to the limit of the activation. A byte-bound
    // body uses ordinary allocations, whose records track changes of payer.
    need(
        !b.insts
            .iter()
            .any(|i| matches!(i, Inst::LimitEnter { .. } | Inst::LimitExit)),
        || "frame obligations require one lexical byte limit".into(),
    )?;

    let mut start = vec![None; b.blocks.len()];
    start[0] = Some(vec![EMPTY; indices.iter().flatten().count()]);
    let mut work = vec![0];
    let mut fallible_drops = std::collections::HashMap::new();
    while let Some(bi) = work.pop() {
        let mut now = start[bi].clone().unwrap();
        let mut send = |target: usize, state: &[u8]| {
            if let Some(old) = &mut start[target] {
                let mut changed = false;
                for (here, incoming) in old.iter_mut().zip(state) {
                    changed |= *here | incoming != *here;
                    *here |= incoming;
                }
                if changed {
                    work.push(target);
                }
            } else {
                start[target] = Some(state.to_vec());
                work.push(target);
            }
        };
        let insts = b.blocks[bi].insts.get(&b.insts).unwrap();
        let mut branch = None;
        for (at, inst) in insts.iter().enumerate() {
            if let Some(fail) = inst.fail(b) {
                send(fail.index(), &now);
            } else if now.iter().any(|state| state & LIVE != 0) {
                let can_fail = match *inst {
                    Inst::Drop { ty, .. } => *fallible_drops
                        .entry(ty)
                        .or_insert_with(|| drop_can_fail(&m.types, ty)),
                    _ => inst.can_fail(b),
                };
                need(!can_fail, || {
                    format!("b{bi}: implicit failure loses a live frame obligation")
                })?;
            }
            match *inst {
                Inst::StorageLive { slot } | Inst::StorageDead { slot } => {
                    if let Some(i) = indices[slot.index()] {
                        need(now[i] == EMPTY, || {
                            format!("b{bi}: storage changes with a live frame obligation")
                        })?;
                    }
                }
                Inst::FrameRefund { storage } => {
                    let state = &mut now[addresses[storage.index()]];
                    need(*state == LIVE, || {
                        format!("b{bi}: refund requires a live frame obligation")
                    })?;
                    *state = EMPTY;
                }
                Inst::FrameAdmit {
                    storage,
                    admitted,
                    replace,
                    ..
                } => {
                    let i = addresses[storage.index()];
                    let required = if replace { LIVE } else { EMPTY };
                    need(now[i] == required, || {
                        format!(
                            "b{bi}: frame admission requires a {} obligation",
                            if replace { "live" } else { "vacant" }
                        )
                    })?;
                    branch = Some((i, admitted, LIVE, required));
                }
                Inst::FrameActive { storage, result } => {
                    branch = Some((addresses[storage.index()], result, LIVE, EMPTY));
                }
                _ => {}
            }
            if let Some((_, result, _, _)) = branch {
                need(
                    at + 1 == insts.len()
                        && matches!(
                            b.terminators[bi], Terminator::Branch { condition, .. } if condition == result
                        ),
                    || {
                        format!(
                            "b{bi}: frame admission or activity test must immediately branch on its result"
                        )
                    },
                )?;
            }
        }
        if let Some((i, _, on, off)) = branch {
            let Terminator::Branch { yes, no, .. } = b.terminators[bi] else {
                unreachable!()
            };
            let possibilities = if matches!(insts.last(), Some(Inst::FrameAdmit { .. })) {
                on | off
            } else {
                now[i]
            };
            for (edge, state) in [(yes, on), (no, off)] {
                if possibilities & state != 0 {
                    now[i] = state;
                    send(b.edges[edge.index()].target.index(), &now);
                }
            }
        } else {
            let targets = successors_of_terminator(b, bi);
            if targets.is_empty() && !matches!(b.terminators[bi], Terminator::Unreachable) {
                need(now.iter().all(|&state| state == EMPTY), || {
                    format!("b{bi}: exit loses a live frame obligation")
                })?;
            }
            for target in targets {
                send(target, &now);
            }
        }
    }
    Ok(())
}

fn drop_can_fail(rows: &[Row], ty: TypeId) -> bool {
    use hirsel_core_layout::{Body, FieldTy, Handle};

    fn field(ty: &FieldTy, work: &mut Vec<TypeId>) -> bool {
        match ty {
            FieldTy::Inline(row) | FieldTy::Box(row) => {
                work.push(TypeId(row.0));
                false
            }
            FieldTy::Seq(item) | FieldTy::Columns(item) | FieldTy::Array(item, _) => {
                field(item, work)
            }
            FieldTy::Map(key, value) => field(key, work) || field(value, work),
            FieldTy::Handle(Handle::Task | Handle::Receiver) => true,
            _ => false,
        }
    }

    let mut seen = std::collections::HashSet::new();
    let mut work = vec![ty];
    while let Some(ty) = work.pop() {
        if !seen.insert(ty) {
            continue;
        }
        let Some(row) = rows.get(ty.index()) else {
            return true;
        };
        let mut calls = |f: &hirsel_core_layout::Field| field(&f.ty, &mut work);
        if row.finalizer.is_some()
            || match &row.body {
                Body::Record(fields) => fields.iter().any(calls),
                Body::Variant(cases) => cases.iter().any(|case| case.fields.iter().any(&mut calls)),
            }
        {
            return true;
        }
    }
    false
}

pub(super) fn inst(c: &mut Ctx<'_>, inst: &Inst) -> R {
    match *inst {
        Inst::FrameAdmit {
            storage,
            admitted,
            available,
            charge,
            overflow,
            ..
        } => {
            c.place(storage)?;
            c.def(admitted)?;
            c.is(admitted, Ty::Scalar(Scalar::Bool))?;
            c.def(available)?;
            c.is(available, Ty::Scalar(Scalar::U64))?;
            c.is(charge, Ty::Scalar(Scalar::U64))?;
            if let Some(flag) = overflow {
                c.is(flag, Ty::Scalar(Scalar::Bool))?;
            }
            Ok(())
        }
        Inst::FrameActive { storage, result } => {
            c.place(storage)?;
            c.def(result)?;
            c.is(result, Ty::Scalar(Scalar::Bool))
        }
        Inst::FrameRefund { storage } => c.place(storage).map(|_| ()),
        _ => unreachable!(),
    }
}
