use super::*;

// A join can carry both states. Only FrameActive can refine that union,
// and a replacement or refund requires an obligation on every incoming path.
const EMPTY: u8 = 1;
const LIVE: u8 = 2;

pub(super) fn obligations(
    m: &Module,
    b: &Body,
    budget: &crate::decode::Debit<'_>,
) -> Result<(), Error> {
    let scratch = budget.scratch();
    scratch.check()?;
    let frames = b.frame_obligations();
    if frames.is_empty() {
        return Ok(());
    }
    let mut indices = Vec::new();
    scratch.reserve(&mut indices, b.slots.len())?;
    indices.resize(b.slots.len(), None);
    let mut addresses = Vec::new();
    scratch.reserve(&mut addresses, b.addresses.len())?;
    addresses.resize(b.addresses.len(), 0);
    let count = frames.len();
    for (i, storage) in frames.into_iter().enumerate() {
        let info = &b.addresses[storage.index()];
        let AddressBase::Slot(slot) = info.base else {
            return Err(Error::Invalid(
                "frame obligation must name a local slot".into(),
            ));
        };
        need(
            info.steps.len == 0 && indices[slot.index()].replace(i).is_none(),
            || "frame obligation must name a unique whole slot".into(),
        )
        .map_err(Error::Invalid)?;
        addresses[storage.index()] = i;
    }

    // Frame obligations belong to the limit of the activation. A byte-bound
    // body uses ordinary allocations, whose records track changes of payer.
    need(
        !b.insts
            .iter()
            .any(|i| matches!(i, Inst::LimitEnter { .. } | Inst::LimitExit)),
        || "frame obligations require one lexical byte limit".into(),
    )
    .map_err(Error::Invalid)?;

    let mut start: Vec<Option<Vec<u8>>> = Vec::new();
    scratch.reserve(&mut start, b.blocks.len())?;
    start.resize_with(b.blocks.len(), || None);
    let mut init = Vec::new();
    scratch.reserve(&mut init, count)?;
    init.resize(count, EMPTY);
    start[0] = Some(init);
    let mut now = Vec::new();
    scratch.reserve(&mut now, count)?;
    now.resize(count, EMPTY);
    let mut queued = Vec::new();
    scratch.reserve(&mut queued, b.blocks.len())?;
    queued.resize(b.blocks.len(), false);
    let mut work = Vec::new();
    scratch.reserve(&mut work, b.blocks.len())?;
    work.push(0);
    queued[0] = true;
    let mut fallible_drops = std::collections::HashMap::new();
    while let Some(bi) = work.pop() {
        scratch.check()?;
        queued[bi] = false;
        now.copy_from_slice(start[bi].as_ref().unwrap());
        let mut send = |target: usize, state: &[u8]| -> Result<(), Error> {
            scratch.check()?;
            if let Some(old) = &mut start[target] {
                let mut changed = false;
                for (here, incoming) in old.iter_mut().zip(state) {
                    changed |= *here | incoming != *here;
                    *here |= incoming;
                }
                if changed && !queued[target] {
                    queued[target] = true;
                    work.push(target);
                }
            } else {
                let mut next = Vec::new();
                scratch.reserve(&mut next, state.len())?;
                next.extend_from_slice(state);
                start[target] = Some(next);
                queued[target] = true;
                work.push(target);
            }
            Ok(())
        };
        let insts = b.blocks[bi].insts.get(&b.insts).unwrap();
        let mut branch = None;
        for (at, inst) in insts.iter().enumerate() {
            scratch.check()?;
            if let Some(fail) = inst.fail(b) {
                send(fail.index(), &now)?;
            } else if now.iter().any(|state| state & LIVE != 0) {
                let can_fail = match *inst {
                    Inst::Drop { ty, .. } => {
                        if let Some(&v) = fallible_drops.get(&ty) {
                            v
                        } else {
                            scratch.allocation::<(TypeId, bool)>(2)?;
                            let v = drop_can_fail(&m.types, ty);
                            fallible_drops.insert(ty, v);
                            v
                        }
                    }
                    _ => inst.can_fail(b),
                };
                need(!can_fail, || {
                    format!("b{bi}: implicit failure loses a live frame obligation")
                })
                .map_err(Error::Invalid)?;
            }
            match *inst {
                Inst::StorageLive { slot } | Inst::StorageDead { slot } => {
                    if let Some(i) = indices[slot.index()] {
                        need(now[i] == EMPTY, || {
                            format!("b{bi}: storage changes with a live frame obligation")
                        })
                        .map_err(Error::Invalid)?;
                    }
                }
                Inst::FrameRefund { storage } => {
                    let state = &mut now[addresses[storage.index()]];
                    need(*state == LIVE, || {
                        format!("b{bi}: refund requires a live frame obligation")
                    })
                    .map_err(Error::Invalid)?;
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
                    })
                    .map_err(Error::Invalid)?;
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
                )
                .map_err(Error::Invalid)?;
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
                    send(b.edges[edge.index()].target.index(), &now)?;
                }
            }
        } else {
            let targets = successors_of_terminator(b, bi);
            if targets.is_empty() && !matches!(b.terminators[bi], Terminator::Unreachable) {
                need(now.iter().all(|&state| state == EMPTY), || {
                    format!("b{bi}: exit loses a live frame obligation")
                })
                .map_err(Error::Invalid)?;
            }
            for target in targets {
                send(target, &now)?;
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
