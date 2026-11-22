//! A counted store loop with no per-element effects can emit one bulk fill.
use crate::{
    AddressBase, AddressId, ArithMode, ArithOp, Body, CompareOp, Inst, PlaceTy, Step, Terminator,
    ValueId, failure, ir::for_each_successor,
};
use hirsel_core_layout::{Scalar, Ty};

#[derive(Clone, Copy)]
pub struct Fill {
    pub index: ValueId,
    pub bound: ValueId,
    pub base: ValueId,
    /// A runtime byte for u8, or None for a scalar with zero bits.
    pub byte: Option<ValueId>,
    pub store: AddressId,
    pub stride: u64,
    pub offset: ValueId,
    pub next: ValueId,
}

/// The effect-free fill body at each block, when there is one.
pub fn fills(body: &Body) -> Vec<Option<Fill>> {
    if !body.blocks.iter().any(|b| {
        matches!(b.insts.get(&body.insts), Some([_, _, Inst::Store { value, .. }, _]) if matches!(body.values[value.index()], Ty::Scalar(_)))
    }) {
        return Vec::new();
    }
    let constants = failure::constants(body);
    let mut predecessors = vec![0usize; body.blocks.len()];
    for block in 0..body.blocks.len() {
        for_each_successor(body, block, |next| predecessors[next.index()] += 1);
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
            let Ty::Scalar(scalar) = body.values[byte.index()] else {
                return None;
            };
            let stride = constants[stride.index()]?;
            if stride != scalar.size() {
                return None;
            }
            let byte = if scalar == Scalar::U8 {
                Some(*byte)
            } else {
                (constants[byte.index()] == Some(0)).then_some(())?;
                None
            };
            if block == 0
                || predecessors[block] != 1
                || b.params.len != 0
                || address != store
                || index != incremented
                || constants[one.index()] != Some(1)
                || body.values[index.index()] != Ty::Scalar(Scalar::U64)
            {
                return None;
            }
            let a = &body.addresses[address.index()];
            let AddressBase::Value(base) = a.base else {
                return None;
            };
            if a.ty != PlaceTy::Value(Ty::Scalar(scalar))
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
                || bound == offset
                || bound == next
                || body.edges[yes.index()].target.index() != block
                || body.edges[no.index()].target.index() == block
            {
                return None;
            }
            Some(Fill {
                index: *index,
                bound: *bound,
                base,
                byte,
                store: *store,
                stride,
                offset: *offset,
                next: *next,
            })
        })
        .collect()
}
