//! Keep instruction-local scalars in SSA without strengthening FORM's cleanup
//! control-flow contract. Values crossing a CoreIR block retain edge storage.
use super::*;

pub(super) fn slots(body: &Body) -> Vec<bool> {
    let mut owners = vec![None; body.values.len()];
    for (block, b) in body.blocks.iter().enumerate() {
        for inst in b.insts.get(&body.insts).unwrap() {
            let result = match *inst {
                Inst::Unary { result, .. }
                | Inst::Binary { result, .. }
                | Inst::Compare { result, .. }
                | Inst::Convert { result, .. }
                | Inst::IntArith { result, .. }
                | Inst::MulAdd { result, .. }
                | Inst::ScalarUnary { result, .. }
                | Inst::Read { result, .. }
                | Inst::ReadStatic { result, .. }
                | Inst::FunctionRef { result, .. }
                | Inst::Pointer { result, .. }
                | Inst::UpdateMask { result, .. } => Some(result),
                _ => None,
            };
            if let Some(v) = result {
                owners[v.index()] = Some(block);
            }
            if let Inst::Convert { flag: Some(v), .. } | Inst::IntArith { flag: Some(v), .. } =
                *inst
            {
                owners[v.index()] = Some(block);
            }
        }
    }
    let mut slots: Vec<_> = owners.iter().map(Option::is_none).collect();
    // Address expressions and call/edge pools can be evaluated from several
    // continuations. Keep their operands in the existing storage representation.
    for a in &body.addresses {
        if let AddressBase::Value(v) = a.base {
            slots[v.index()] = true;
        }
        for step in a.steps.get(&body.pools.steps).unwrap() {
            if let Step::Index(v) | Step::Element(v) | Step::Byte(v) = *step {
                slots[v.index()] = true;
            }
        }
    }
    for call in &body.calls {
        if let Callee::Indirect(v) = call.callee {
            slots[v.index()] = true;
        }
        for arg in call.args.get(&body.pools.args).unwrap() {
            if let Arg::Value(v) = *arg {
                slots[v.index()] = true;
            }
        }
    }
    for edge in &body.edges {
        for v in edge.args.get(&body.pools.values).unwrap() {
            slots[v.index()] = true;
        }
    }
    for (block, b) in body.blocks.iter().enumerate() {
        let mut read = |v: ValueId| {
            if owners[v.index()] != Some(block) {
                slots[v.index()] = true;
            }
        };
        for inst in b.insts.get(&body.insts).unwrap() {
            match *inst {
                Inst::Unary { operand, .. }
                | Inst::ScalarUnary { operand, .. }
                | Inst::Convert { operand, .. } => read(operand),
                Inst::Binary { left, right, .. }
                | Inst::Compare { left, right, .. }
                | Inst::IntArith { left, right, .. } => {
                    read(left);
                    read(right);
                }
                Inst::MulAdd {
                    left,
                    right,
                    addend,
                    ..
                } => {
                    read(left);
                    read(right);
                    read(addend);
                }
                Inst::Store { value, .. } => read(value),
                Inst::UpdateMask { mask, .. } => read(mask),
                Inst::Alloc {
                    charge,
                    overflow,
                    size,
                    place,
                    ..
                } => {
                    read(charge);
                    read(size);
                    if let Some(v) = overflow {
                        read(v);
                    }
                    if let AllocPlace::Region(v) | AllocPlace::Like(v) = place {
                        read(v);
                    }
                }
                Inst::Free { block, charge } => {
                    read(block);
                    read(charge);
                }
                Inst::Copy { bytes, .. } | Inst::LimitEnter { bytes, .. } => read(bytes),
                Inst::Trim {
                    block,
                    region,
                    entry,
                    previous,
                    charge,
                } => {
                    for v in [block, region, entry, previous, charge] {
                        read(v);
                    }
                }
                Inst::RegionClose { region } => read(region),
                Inst::Share { node, .. }
                | Inst::Release { node, .. }
                | Inst::TryUnwrap { node, .. } => read(node),
                Inst::Const { .. }
                | Inst::ReadStatic { .. }
                | Inst::FunctionRef { .. }
                | Inst::Address { .. }
                | Inst::Read { .. }
                | Inst::StorageLive { .. }
                | Inst::StorageDead { .. }
                | Inst::Call { .. }
                | Inst::Pointer { .. }
                | Inst::Case { .. }
                | Inst::SetCase { .. }
                | Inst::RegionOpen { .. }
                | Inst::LimitExit
                | Inst::Drop { .. }
                | Inst::Mark { .. }
                | Inst::Unmark { .. } => {}
            }
        }
        match body.terminators[block] {
            Terminator::Branch { condition, .. } => read(condition),
            Terminator::Switch { value, .. } => read(value),
            Terminator::Return(values) => {
                for v in values.get(&body.pools.values).unwrap() {
                    read(*v);
                }
            }
            _ => {}
        }
    }
    // Effect-free literal definitions are emitted in the entry, so they can be
    // shared by inactive cleanup paths without requiring a dominance analysis.
    for inst in &body.insts {
        if let Inst::Const { result, .. } = *inst {
            slots[result.index()] = false;
        }
    }
    slots
}
