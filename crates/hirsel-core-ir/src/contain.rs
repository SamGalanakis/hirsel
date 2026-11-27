//! Fused scope rows (PACK-12): which `bound bytes`, `bound steps` and `isolate`
//! rows of a body an engine may run as one runtime call.
//!
//! The plan changes no meaning. A fused enter performs its two enters in their
//! written order at the first row; a fused leave performs its leaves in their
//! written order at the first row. Only instructions that neither charge,
//! refund, meter, call nor fail lie between the rows of one group, so no
//! program can observe that the later rows ran early. Rows on a path that does
//! not pass a group's first row keep their own runtime call.
use crate::{Arg, Body, Callee, Inst, RtOp, Terminator, ValueId, ir::for_each_successor};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Layer {
    Bytes = 1,
    Steps = 2,
    Isolate = 3,
}

/// The second enter's size: a value defined before the first row, or a constant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operand {
    Value(ValueId),
    Const(u64),
}

/// Two enters run by the first row. `second` is the later row's instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Enter {
    pub first: Layer,
    pub second: usize,
    pub operand: Operand,
}

/// Instruction indexes are positions in `Body::insts`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Plan {
    /// A first enter row and the enter it also performs.
    pub enters: BTreeMap<usize, Enter>,
    /// A later enter row and its first row, whose results give its own.
    pub entered: BTreeMap<usize, usize>,
    /// A first leave row and every layer it leaves, itself first, four bits
    /// each from the low end ([`Layer`] as its number).
    pub leaves: BTreeMap<usize, u64>,
    /// Later leave rows: each does nothing when its group's first row ran.
    pub left: BTreeSet<usize>,
}

impl Plan {
    /// The number of rows after `first` that its fused leave covers.
    pub fn covered(layers: u64) -> u64 {
        u64::from((64 - layers.leading_zeros()).div_ceil(4)) - 1
    }
}

fn enter(body: &Body, inst: &Inst) -> Option<(Layer, ValueId, ValueId)> {
    match *inst {
        Inst::LimitEnter {
            admitted, bytes, ..
        } => Some((Layer::Bytes, admitted, bytes)),
        Inst::Call { call } => {
            let site = body.calls.get(call.index())?;
            if site.callee != Callee::Runtime(RtOp::StepEnter) {
                return None;
            }
            let &[Arg::Value(steps)] = site.args.get(&body.pools.args)? else {
                return None;
            };
            let &[admitted] = site.results.get(&body.pools.values)? else {
                return None;
            };
            Some((Layer::Steps, admitted, steps))
        }
        _ => None,
    }
}

fn leave(body: &Body, inst: &Inst) -> Option<Layer> {
    match *inst {
        Inst::LimitExit => Some(Layer::Bytes),
        Inst::Call { call } => match body.calls.get(call.index())?.callee {
            Callee::Runtime(RtOp::StepLeave) => Some(Layer::Steps),
            Callee::Runtime(RtOp::IsolateLeave) => Some(Layer::Isolate),
            _ => None,
        },
        _ => None,
    }
}

/// An instruction that reads and writes only values and frame memory: it
/// cannot fail, and no limit, step account or isolate can tell when it ran.
fn quiet(body: &Body, inst: &Inst) -> bool {
    matches!(
        inst,
        Inst::Const { .. }
            | Inst::FunctionRef { .. }
            | Inst::Address { .. }
            | Inst::Read { .. }
            | Inst::Store { .. }
            | Inst::StorageLive { .. }
            | Inst::StorageDead { .. }
            | Inst::Pointer { .. }
            | Inst::Copy { .. }
            | Inst::Case { .. }
            | Inst::SetCase { .. }
            | Inst::LayoutConst { .. }
            | Inst::BuiltinUse { .. }
    ) && !inst.can_fail(body)
        && inst.fail(body).is_none()
}

fn defined(inst: &Inst) -> Option<ValueId> {
    match *inst {
        Inst::Const { result, .. }
        | Inst::FunctionRef { result, .. }
        | Inst::Read { result, .. }
        | Inst::Pointer { result, .. }
        | Inst::Case { result, .. }
        | Inst::LayoutConst { result, .. } => Some(result),
        _ => None,
    }
}

/// The plan for `body`. `elided` names the byte rows the engine lowers without
/// a runtime call; they join no group. `apart` names the byte rows at which the
/// engine may not open or close a step bound (it holds steps paid ahead there,
/// whose refund belongs to the bound then current); they start no group.
pub fn plan(body: &Body, elided: &dyn Fn(usize) -> bool, apart: &dyn Fn(usize) -> bool) -> Plan {
    let mut plan = Plan::default();
    if !body.insts.iter().any(|i| leave(body, i).is_some()) {
        return plan;
    }
    let mut entries = vec![0usize; body.blocks.len()];
    for block in 0..body.blocks.len() {
        for_each_successor(body, block, |next| entries[next.index()] += 1);
    }
    let insts = |block: usize| {
        let range = body.blocks[block].insts;
        (
            range.start as usize,
            range.get(&body.insts).unwrap_or_default(),
        )
    };
    for block in 0..body.blocks.len() {
        let (start, rows) = insts(block);
        for (position, inst) in rows.iter().enumerate() {
            let head = start + position;
            let Some((first, admitted, _)) = enter(body, inst) else {
                continue;
            };
            if elided(head)
                || plan.entered.contains_key(&head)
                || (first == Layer::Bytes && apart(head))
            {
                continue;
            }
            let Terminator::Branch { condition, yes, no } = body.terminators[block] else {
                continue;
            };
            let (yes, no) = (&body.edges[yes.index()], &body.edges[no.index()]);
            let target = yes.target.index();
            if condition != admitted
                || yes.args.len != 0
                || yes.target == no.target
                || target == block
                || entries[target] != 1
            {
                continue;
            }
            let (next_start, next_rows) = insts(target);
            let between = rows[position + 1..].iter().chain(next_rows);
            let mut constants = BTreeMap::new();
            let mut local = BTreeSet::new();
            for (offset, inst) in between.enumerate() {
                if let Some((second, _, operand)) = enter(body, inst) {
                    // Past the first block's tail, offsets count the target's rows.
                    let tail = rows.len() - position - 1;
                    if offset < tail || second == first {
                        break;
                    }
                    let row = next_start + offset - tail;
                    let operand = match constants.get(&operand) {
                        Some(&bits) => Operand::Const(bits),
                        None if !local.contains(&operand) => Operand::Value(operand),
                        None => break,
                    };
                    if !elided(row) {
                        plan.enters.insert(
                            head,
                            Enter {
                                first,
                                second: row,
                                operand,
                            },
                        );
                        plan.entered.insert(row, head);
                    }
                    break;
                }
                if !quiet(body, inst) {
                    break;
                }
                if let Inst::Const { result, bits } = *inst {
                    constants.insert(result, bits);
                }
                local.extend(defined(inst));
            }
        }
    }
    // Each leave row's next leave row down the one path that follows it.
    let mut next = BTreeMap::new();
    let mut preceded = BTreeSet::new();
    for block in 0..body.blocks.len() {
        let (start, rows) = insts(block);
        for (position, inst) in rows.iter().enumerate() {
            let Some(layer) = leave(body, inst) else {
                continue;
            };
            let row = start + position;
            if elided(row) {
                continue;
            }
            let (mut at, mut from, mut seen) = (block, position + 1, BTreeSet::from([block]));
            let found = 'path: loop {
                let (start, rows) = insts(at);
                for (position, inst) in rows.iter().enumerate().skip(from) {
                    if leave(body, inst).is_some() {
                        break 'path Some(start + position).filter(|&r| !elided(r));
                    }
                    if !quiet(body, inst) {
                        break 'path None;
                    }
                }
                let Terminator::Jump(edge) = body.terminators[at] else {
                    break None;
                };
                at = body.edges[edge.index()].target.index();
                from = 0;
                if !seen.insert(at) {
                    break None;
                }
            };
            if let Some(found) = found {
                next.insert(row, (layer, found));
                preceded.insert(found);
            }
        }
    }
    let starts = next
        .keys()
        .filter(|row| !preceded.contains(row))
        .chain(next.keys().filter(|row| preceded.contains(row)))
        .copied()
        .collect::<Vec<_>>();
    for first in starts {
        if plan.left.contains(&first)
            || plan.leaves.contains_key(&first)
            || (next[&first].0 == Layer::Bytes && apart(first))
        {
            continue;
        }
        let (mut layers, mut count, mut row) = (0u64, 0u32, first);
        let mut covered = Vec::new();
        // Sixteen layers fit the word; a longer run starts a new group.
        while count < 16 {
            let layer = match next.get(&row) {
                Some(&(layer, _)) => layer,
                None => match leave(body, &body.insts[row]) {
                    Some(layer) => layer,
                    None => break,
                },
            };
            layers |= (layer as u64) << (4 * count);
            count += 1;
            if row != first {
                covered.push(row);
            }
            match next.get(&row) {
                Some(&(_, following))
                    if !plan.left.contains(&following)
                        && !plan.leaves.contains_key(&following)
                        && following != first
                        && !covered.contains(&following) =>
                {
                    row = following
                }
                _ => break,
            }
        }
        if !covered.is_empty() {
            plan.leaves.insert(first, layers);
            plan.left.extend(covered);
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The typed contained call's rows (`bound bytes { bound steps { isolate } }`)
    /// in CoreIR text: the isolate call's metered step sits between the step
    /// bound's enter and the isolate's, so only the two bounds' enters fuse; the
    /// three leaves on the returning path fuse, and the leaves reached from the
    /// catch and refusal paths are the same rows, which then call for themselves.
    #[test]
    fn a_metered_step_between_enters_ends_the_fused_enter() {
        let module = crate::text::parse(
            "core v1\n\
             sig g0 = () -> ()\n\
             sig g1 = (u64) -> (bool)\n\
             sig g2 = () -> (bool, u64)\n\
             inst n0 = app@1/main/f [] []\n\
             fn f0 = n0 g0 {\n\
             b0:\n\
             \x20 v0: u64 = const 4096\n\
             \x20 v1: bool, v2: u64 = limit_enter v0\n\
             \x20 branch v1, b1, b6\n\
             b1:\n\
             \x20 v3: u64 = const 64\n\
             \x20 v4: bool = call rt.step_enter g1 (v3)\n\
             \x20 branch v4, b2, b5\n\
             b2:\n\
             \x20 step\n\
             \x20 v5: bool, v6: u64 = call rt.isolate_enter g2 ()\n\
             \x20 branch v5, b3, b4\n\
             b3:\n\
             \x20 call rt.isolate_leave g0 ()\n\
             \x20 jump b4\n\
             b4:\n\
             \x20 call rt.step_leave g0 ()\n\
             \x20 jump b5\n\
             b5:\n\
             \x20 limit_exit\n\
             \x20 jump b6\n\
             b6:\n\
             \x20 return\n\
             }\n",
        )
        .unwrap();
        let body = module.functions[0].body.as_ref().unwrap();
        let row = |find: &dyn Fn(&Inst) -> bool| body.insts.iter().position(find).unwrap();
        let op = |wanted: RtOp| {
            row(
                &|i| matches!(*i, Inst::Call { call } if body.calls[call.index()].callee == Callee::Runtime(wanted)),
            )
        };
        let plan = plan(body, &|_| false, &|_| false);
        let head = row(&|i| matches!(i, Inst::LimitEnter { .. }));
        assert_eq!(
            plan.enters,
            BTreeMap::from([(
                head,
                Enter {
                    first: Layer::Bytes,
                    second: op(RtOp::StepEnter),
                    operand: Operand::Const(64),
                }
            )])
        );
        assert_eq!(plan.entered, BTreeMap::from([(op(RtOp::StepEnter), head)]));
        assert_eq!(
            plan.leaves,
            BTreeMap::from([(op(RtOp::IsolateLeave), 0x123)])
        );
        assert_eq!(
            plan.left,
            BTreeSet::from([op(RtOp::StepLeave), row(&|i| matches!(i, Inst::LimitExit))])
        );
        assert_eq!(Plan::covered(0x123), 2);
    }
}
