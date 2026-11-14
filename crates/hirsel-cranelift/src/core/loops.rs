//! The producer's effect-free byte store loop can use libc's bulk fill even
//! when dev code uses the single-pass register allocator.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct ByteFill {
    pub index: ValueId,
    pub bound: ValueId,
    pub base: ValueId,
    pub byte: ValueId,
    pub store: AddressId,
}

pub(super) fn byte_fills(body: &Body) -> Vec<Option<ByteFill>> {
    if !body.blocks.iter().any(|b| {
        matches!(b.insts.get(&body.insts), Some([_, _, Inst::Store { value, .. }, _]) if body.values[value.index()] == Ty::Scalar(Scalar::U8))
    }) {
        return Vec::new();
    }
    let mut constants = vec![None; body.values.len()];
    for inst in &body.insts {
        if let Inst::Const { result, bits } = *inst {
            constants[result.index()] = Some(bits);
        }
    }
    let mut predecessors = vec![0usize; body.blocks.len()];
    for edge in &body.edges {
        predecessors[edge.target.index()] += 1;
    }
    body.blocks
        .iter()
        .enumerate()
        .map(|(block, b)| {
            let [
                Inst::IntArith {
                    result: offset,
                    flag: None,
                    op: ArithOp::Mul,
                    mode: ArithMode::Wrapping,
                    left: index,
                    right: stride,
                },
                Inst::Address {
                    address,
                    fail: None,
                },
                Inst::Store {
                    address: store,
                    value: byte,
                },
                Inst::IntArith {
                    result: next,
                    flag: None,
                    op: ArithOp::Add,
                    mode: ArithMode::Wrapping,
                    left: incremented,
                    right: one,
                },
            ] = b.insts.get(&body.insts)?
            else {
                return None;
            };
            if predecessors[block] != 1
                || b.params.len != 0
                || address != store
                || index != incremented
                || constants[stride.index()] != Some(1)
                || constants[one.index()] != Some(1)
                || body.values[index.index()] != Ty::Scalar(Scalar::U64)
                || body.values[byte.index()] != Ty::Scalar(Scalar::U8)
            {
                return None;
            }
            let a = &body.addresses[address.index()];
            let AddressBase::Value(base) = a.base else {
                return None;
            };
            if a.ty != PlaceTy::Value(Ty::Scalar(Scalar::U8))
                || a.steps.get(&body.pools.steps)? != [Step::Byte(*offset)]
            {
                return None;
            }
            let Terminator::Jump(back) = body.terminators[block] else {
                return None;
            };
            let back = &body.edges[back.index()];
            if back.args.get(&body.pools.values)? != [*next] {
                return None;
            }
            let header = back.target.index();
            if body.blocks[header].params.get(&body.pools.values)? != [*index] {
                return None;
            }
            let [
                Inst::Compare {
                    result: condition,
                    op: CompareOp::Lt,
                    left,
                    right: bound,
                },
            ] = body.blocks[header].insts.get(&body.insts)?
            else {
                return None;
            };
            let Terminator::Branch {
                condition: tested,
                yes,
                no,
            } = body.terminators[header]
            else {
                return None;
            };
            if left != index
                || tested != *condition
                || body.edges[yes.index()].target.index() != block
                || body.edges[no.index()].target.index() == block
            {
                return None;
            }
            Some(ByteFill {
                index: *index,
                bound: *bound,
                base,
                byte: *byte,
                store: *store,
            })
        })
        .collect()
}
