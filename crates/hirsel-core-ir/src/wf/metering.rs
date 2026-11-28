//! Check the control flow that can run between successful charges. A failed
//! Step has paid nothing, so its failure edge stays in this graph.
use super::*;

mod bulk;
mod repeat;
mod scopes;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Policy {
    Admission,
    Strict,
}

pub fn module(m: &Module) -> R {
    module_with_budget(m, &Debit::unlimited()).map_err(|e| e.to_string())
}

/// Verify canonical CoreIR with the metering laws active at admission.
pub fn metering_with_budget(m: &Module, budget: &Debit<'_>) -> Result<(), Error> {
    structural(m, budget)?;
    check(m, budget, Policy::Admission)
}

/// Verify canonical CoreIR, refusing cycles whose bound cannot be proved locally.
pub fn metering_strict_with_budget(m: &Module, budget: &Debit<'_>) -> Result<(), Error> {
    structural(m, budget)?;
    check(m, budget, Policy::Strict)
}

/// Recheck a compiler-transformed body after its canonical module was verified.
pub fn specialized_module(m: &Module) -> R {
    structural(m, &Debit::unlimited()).map_err(|e| e.to_string())
}

fn check(module: &Module, budget: &Debit<'_>, policy: Policy) -> Result<(), Error> {
    let scratch = budget.scratch();
    let mut entries = Vec::new();
    scratch.reserve(&mut entries, module.functions.len())?;
    let mut nodes = 0usize;
    for function in &module.functions {
        scratch.check()?;
        entries.push(nodes);
        if let Some(body) = &function.body {
            nodes = nodes
                .checked_add(body.blocks.len())
                .ok_or(Error::Limit(DecodeLimit::Bytes))?;
        }
    }
    let external = nodes;
    let size = nodes
        .checked_add(1)
        .ok_or(Error::Limit(DecodeLimit::Bytes))?;
    let mut edges = Vec::<Vec<usize>>::new();
    scratch.reserve(&mut edges, size)?;
    edges.resize_with(size, Vec::new);
    let mut incoming = Vec::new();
    scratch.reserve(&mut incoming, size)?;
    incoming.resize(size, 0usize);
    let mut connect = |from: usize, to: usize| -> Result<(), Error> {
        scratch.reserve(&mut edges[from], 1)?;
        edges[from].push(to);
        incoming[to] += 1;
        Ok(())
    };
    for (fi, function) in module.functions.iter().enumerate() {
        let Some(body) = &function.body else { continue };
        let local = scratch.scratch();
        if policy == Policy::Strict {
            scopes::check(body, &local)?;
        }
        let facts = bulk::Facts::new(body, &local)?;
        facts.check_operations(body, policy, &local)?;
        let paid = repeat::paid_repeats(body, &facts, &local)?;
        let entry = entries[fi];
        connect(external, entry)?;
        'blocks: for (bi, block) in body.blocks.iter().enumerate() {
            scratch.check()?;
            let node = entry + bi;
            for inst in block.insts.get(&body.insts).unwrap_or_default() {
                scratch.check()?;
                if let Some(fail) = inst.fail(body) {
                    connect(node, entry + fail.index())?;
                }
                if let Inst::Call { call } = *inst
                    && facts.positive_work(body, &body.calls[call.index()])
                {
                    continue 'blocks;
                }
                match *inst {
                    Inst::Step { .. } => continue 'blocks,
                    Inst::Call { call } => match body.calls[call.index()].callee {
                        Callee::Direct(f) if module.functions[f.index()].body.is_some() => {
                            connect(node, entries[f.index()])?;
                        }
                        Callee::Direct(_) | Callee::Indirect(_) | Callee::PackExport(_) => {
                            if policy == Policy::Strict {
                                connect(node, external)?;
                            }
                        }
                        Callee::Runtime(
                            RtOp::WorkSteps
                            | RtOp::StepEnter
                            | RtOp::StepLeave
                            | RtOp::StepCatch
                            | RtOp::ZeroBytes
                            | RtOp::CompareBytes
                            | RtOp::Crc32c
                            | RtOp::Crc64Nvme
                            | RtOp::Sha1Blocks
                            | RtOp::Sha1Compress
                            | RtOp::Sha256Blocks
                            | RtOp::Sha512Blocks
                            | RtOp::Xxh3Stripes
                            | RtOp::Xxh3Short,
                        ) => {}
                        Callee::Runtime(_) if policy == Policy::Strict => connect(node, external)?,
                        Callee::Runtime(_) => {}
                    },
                    Inst::Drop { .. } | Inst::Release { .. } if policy == Policy::Strict => {
                        connect(node, external)?
                    }
                    _ => {}
                }
                // The active rule rejects a cycle only when every traversed
                // instruction is known to do no metered work. Open calls and
                // storage operations still need the strict proof.
                if policy == Policy::Admission
                    && !matches!(
                        inst,
                        Inst::Const { .. }
                            | Inst::Unary { .. }
                            | Inst::Binary { .. }
                            | Inst::IntArith { .. }
                            | Inst::Compare { .. }
                            | Inst::Convert { .. }
                            | Inst::LoopIteration
                            | Inst::StackCheck { .. }
                            | Inst::BuiltinUse { .. }
                    )
                {
                    continue 'blocks;
                }
            }
            if paid[bi] {
                continue;
            }
            let mut edge =
                |id: EdgeId| connect(node, entry + body.edges[id.index()].target.index());
            match body.terminators[bi] {
                Terminator::Jump(e) => edge(e)?,
                Terminator::Branch { yes, no, .. } => {
                    edge(yes)?;
                    edge(no)?;
                }
                Terminator::Switch {
                    arms, otherwise, ..
                } => {
                    for arm in arms.get(&body.pools.arms).unwrap_or_default() {
                        edge(arm.edge)?;
                    }
                    edge(otherwise)?;
                }
                Terminator::Raise {
                    cleanup: Some(fail),
                    ..
                } => connect(node, entry + fail.index())?,
                _ => {}
            }
        }
    }
    let mut pending = Vec::new();
    scratch.reserve(&mut pending, size)?;
    for (i, &n) in incoming.iter().enumerate() {
        scratch.check()?;
        if n == 0 {
            pending.push(i);
        }
    }
    let mut removed = 0;
    while let Some(i) = pending.pop() {
        scratch.check()?;
        removed += 1;
        for &next in &edges[i] {
            scratch.check()?;
            incoming[next] -= 1;
            if incoming[next] == 0 {
                pending.push(next);
            }
        }
    }
    if removed != size {
        return Err(Error::Invalid(
            "control-flow or call cycle without a metering charge".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{DecodeBudget, DecodeLimits};

    #[test]
    fn graph_storage_is_charged_to_the_callers_debit() {
        let module = crate::text::parse(include_str!("../../samples/control.core")).unwrap();
        let budget = DecodeBudget::new(DecodeLimits {
            bytes: 1,
            members: usize::MAX,
            slots: usize::MAX,
            depth: usize::MAX,
            deadline: None,
        });
        for policy in [Policy::Admission, Policy::Strict] {
            assert_eq!(
                check(&module, &budget.debit(), policy),
                Err(Error::Limit(DecodeLimit::Bytes))
            );
            check(&module, &Debit::unlimited(), policy).unwrap();
        }
    }
}
