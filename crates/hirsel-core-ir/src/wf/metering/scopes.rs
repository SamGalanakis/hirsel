//! A charge belongs to the enclosing bound only while the callee preserves
//! that bound. Track the local depth on success, refusal and failure edges.
use super::*;

pub(super) fn check(b: &Body, budget: &Debit<'_>) -> Result<(), Error> {
    let scratch = budget.scratch();
    let mut scopes = false;
    for call in &b.calls {
        scratch.check()?;
        scopes |= matches!(
            call.callee,
            Callee::Runtime(RtOp::StepEnter | RtOp::StepCatch | RtOp::StepLeave)
        );
    }
    if !scopes {
        return Ok(());
    }
    let mut depths = Vec::new();
    scratch.reserve(&mut depths, b.blocks.len())?;
    depths.resize(b.blocks.len(), None);
    let mut work = Vec::new();
    scratch.reserve(&mut work, b.blocks.len())?;
    depths[0] = Some(0usize);
    work.push(0usize);
    let invalid = || Error::Invalid("unbalanced local step scope".into());
    while let Some(bi) = work.pop() {
        scratch.check()?;
        let mut depth = depths[bi].unwrap();
        let mut send = |to: BlockId, d: usize| -> Result<(), Error> {
            scratch.check()?;
            match depths[to.index()] {
                None => {
                    depths[to.index()] = Some(d);
                    work.push(to.index());
                }
                Some(old) if old != d => return Err(invalid()),
                Some(_) => {}
            }
            Ok(())
        };
        let block = &b.blocks[bi];
        let rows = block.insts.get(&b.insts).unwrap_or_default();
        let mut entering = None;
        for (at, row) in rows.iter().enumerate() {
            scratch.check()?;
            if let Some(t) = row.fail(b) {
                send(t, depth)?;
            } else if depth > 0 {
                if let Inst::Address { address, .. } = row {
                    for step in b.addresses[address.index()]
                        .steps
                        .get(&b.pools.steps)
                        .unwrap_or_default()
                    {
                        scratch.check()?;
                        if matches!(step, Step::Element(_)) {
                            return Err(invalid());
                        }
                    }
                } else if row.can_fail(b) {
                    return Err(invalid());
                }
            }
            if let Inst::Call { call } = row {
                let site = &b.calls[call.index()];
                match site.callee {
                    Callee::Runtime(RtOp::StepEnter) => {
                        if at + 1 != rows.len() {
                            return Err(invalid());
                        }
                        entering = site
                            .results
                            .get(&b.pools.values)
                            .and_then(|r| r.first())
                            .copied();
                    }
                    Callee::Runtime(RtOp::StepCatch | RtOp::StepLeave) => {
                        depth = depth.checked_sub(1).ok_or_else(invalid)?
                    }
                    _ => {}
                }
            }
        }
        let target = |e: EdgeId| b.edges[e.index()].target;
        match b.terminators[bi] {
            Terminator::Branch { condition, yes, no } => {
                if entering.is_some_and(|v| v != condition) {
                    return Err(invalid());
                }
                send(target(yes), depth + usize::from(entering.is_some()))?;
                send(target(no), depth)?;
            }
            _ if entering.is_some() => return Err(invalid()),
            Terminator::Jump(e) => send(target(e), depth)?,
            Terminator::Switch {
                arms, otherwise, ..
            } => {
                for a in arms.get(&b.pools.arms).unwrap_or_default() {
                    send(target(a.edge), depth)?;
                }
                send(target(otherwise), depth)?;
            }
            Terminator::Raise {
                cleanup: Some(t), ..
            } => send(t, depth)?,
            _ if depth != 0 => return Err(invalid()),
            _ => {}
        }
    }
    Ok(())
}
