//! An empty bound has no allocation, call or runtime observation of its payer.
//! Keep its Capacity decision, but no table identity or reservation is needed.
use super::*;

#[derive(Default)]
pub(super) struct EmptyBounds {
    pub enters: BTreeSet<ValueId>,
    pub exits: BTreeSet<usize>,
}
fn successors(body: &Body, block: usize) -> Option<Vec<usize>> {
    let edge = |e: EdgeId| body.edges[e.index()].target.index();
    Some(match &body.terminators[block] {
        Terminator::Jump(e) => vec![edge(*e)],
        Terminator::Branch { yes, no, .. } => vec![edge(*yes), edge(*no)],
        Terminator::Switch {
            arms, otherwise, ..
        } => arms
            .get(&body.pools.arms)
            .unwrap()
            .iter()
            .map(|arm| edge(arm.edge))
            .chain([edge(*otherwise)])
            .collect(),
        _ => return None,
    })
}
fn pure(body: &Body, constants: &[Option<u64>], inst: &Inst) -> bool {
    match inst {
        Inst::Binary {
            op: BinaryOp::Shl | BinaryOp::Shr,
            left,
            right,
            ..
        } => failure::bounded_shift(body.values[left.index()], constants[right.index()]),
        Inst::Const { .. }
        | Inst::ReadStatic { .. }
        | Inst::FunctionRef { .. }
        | Inst::Unary { .. }
        | Inst::Binary { .. }
        | Inst::Compare { .. }
        | Inst::Convert { .. }
        | Inst::IntArith { .. }
        | Inst::ScalarUnary { .. }
        | Inst::MulAdd { .. }
        | Inst::Address { .. }
        | Inst::Read { .. }
        | Inst::Store { .. }
        | Inst::StorageLive { .. }
        | Inst::StorageDead { .. }
        | Inst::UpdateMask { .. }
        | Inst::Pointer { .. } => !inst.can_fail(body),
        _ => false,
    }
}
impl EmptyBounds {
    pub fn new(body: &Body, constants: &[Option<u64>]) -> Self {
        let mut result = Self::default();
        let mut predecessors = vec![vec![]; body.blocks.len()];
        for b in 0..body.blocks.len() {
            for next in successors(body, b).unwrap_or_default() {
                predecessors[next].push(b);
            }
        }
        for (block, b) in body.blocks.iter().enumerate() {
            let instructions = b.insts.get(&body.insts).unwrap();
            for (position, inst) in instructions.iter().enumerate() {
                let Inst::LimitEnter { admitted, .. } = inst else {
                    continue;
                };
                if !instructions[position + 1..]
                    .iter()
                    .all(|i| pure(body, constants, i))
                {
                    continue;
                }
                let Terminator::Branch { condition, yes, no } = body.terminators[block] else {
                    continue;
                };
                if condition != *admitted
                    || body.edges[yes.index()].target == body.edges[no.index()].target
                {
                    continue;
                }
                let entry = body.edges[yes.index()].target.index();
                let mut pending = vec![entry];
                let mut visited = BTreeSet::new();
                let mut exits = BTreeSet::new();
                let mut valid = true;
                while let Some(next) = pending.pop() {
                    if next == block {
                        valid = false;
                        break;
                    }
                    if !visited.insert(next) {
                        continue;
                    }
                    let info = &body.blocks[next];
                    let mut ended = false;
                    for (offset, inst) in info.insts.get(&body.insts).unwrap().iter().enumerate() {
                        if matches!(inst, Inst::LimitExit) {
                            exits.insert(info.insts.start as usize + offset);
                            ended = true;
                            break;
                        }
                        if !pure(body, constants, inst) {
                            valid = false;
                            break;
                        }
                    }
                    if !valid {
                        break;
                    }
                    if !ended {
                        let Some(edges) = successors(body, next) else {
                            valid = false;
                            break;
                        };
                        pending.extend(edges);
                    }
                }
                if !valid || exits.is_empty() {
                    continue;
                }
                if visited.iter().any(|b| {
                    predecessors[*b]
                        .iter()
                        .any(|p| !visited.contains(p) && !(*b == entry && *p == block))
                }) {
                    continue;
                }
                result.enters.insert(*admitted);
                result.exits.extend(exits);
            }
        }
        result
    }
}
