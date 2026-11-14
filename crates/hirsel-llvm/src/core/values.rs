//! Keep instruction-local scalars in SSA without strengthening FORM's cleanup
//! control-flow contract. Values crossing a CoreIR block retain edge storage.
use super::*;

pub(super) fn slots(body: &Body) -> Vec<bool> {
    let mut owners = vec![None; body.values.len()];
    for (block, b) in body.blocks.iter().enumerate() {
        for (index, inst) in b.insts.get(&body.insts).unwrap().iter().enumerate() {
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
                owners[v.index()] = Some((block, index));
            }
            if let Inst::Convert { flag: Some(v), .. } | Inst::IntArith { flag: Some(v), .. } =
                *inst
            {
                owners[v.index()] = Some((block, index));
            }
        }
    }
    let mut slots: Vec<_> = owners.iter().map(Option::is_none).collect();
    // Edge transfers can execute on several continuations and retain homes.
    for edge in &body.edges {
        for v in edge.args.get(&body.pools.values).unwrap() {
            slots[v.index()] = true;
        }
    }
    for (block, b) in body.blocks.iter().enumerate() {
        for (index, inst) in b.insts.get(&body.insts).unwrap().iter().enumerate() {
            let mut read = |v: ValueId| {
                if !matches!(owners[v.index()], Some((owner, before)) if owner == block && before < index)
                {
                    slots[v.index()] = true;
                }
            };
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
                Inst::Grow {
                    charge,
                    overflow,
                    size,
                    capacity,
                    moved,
                    refund,
                    ..
                } => {
                    for v in [charge, size, capacity, moved, refund]
                        .into_iter()
                        .chain(overflow)
                    {
                        read(v);
                    }
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
                Inst::Call { call } => {
                    let call = &body.calls[call.index()];
                    if let Callee::Indirect(value) = call.callee {
                        read(value);
                    }
                    for arg in call.args.get(&body.pools.args).unwrap() {
                        if let Arg::Value(value) = *arg {
                            read(value);
                        }
                    }
                }
                Inst::Address { address, .. } => {
                    let address = &body.addresses[address.index()];
                    if let AddressBase::Value(value) = address.base {
                        read(value);
                    }
                    for step in address.steps.get(&body.pools.steps).unwrap() {
                        if let Step::Index(value) | Step::Element(value) | Step::Byte(value) = *step
                        {
                            read(value);
                        }
                    }
                }
                Inst::Const { .. }
                | Inst::ReadStatic { .. }
                | Inst::FunctionRef { .. }
                | Inst::Read { .. }
                | Inst::StorageLive { .. }
                | Inst::StorageDead { .. }
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
        let mut read = |v: ValueId| {
            if !matches!(owners[v.index()], Some((owner, _)) if owner == block) {
                slots[v.index()] = true;
            }
        };
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
    // Arguments are defined in the LLVM entry. A CoreIR back edge to b0
    // would assign its parameters again, so those retain their homes.
    if !body.edges.iter().any(|edge| edge.target == BlockId(0)) {
        for value in body.blocks[0].params.get(&body.pools.values).unwrap() {
            slots[value.index()] = false;
        }
    }
    slots
}

/// Cache only addresses that cross a CoreIR block or have multiple projections.
/// Requiring an earlier projection in the same block also covers inactive
/// cleanup operands that can precede their otherwise live projection.
pub(super) fn address_slots(body: &Body) -> Vec<bool> {
    let mut definitions = vec![None; body.addresses.len()];
    let mut slots = vec![false; body.addresses.len()];
    for (block, b) in body.blocks.iter().enumerate() {
        for (index, inst) in b.insts.get(&body.insts).unwrap().iter().enumerate() {
            if let Inst::Address { address, .. } = *inst {
                if definitions[address.index()].is_some() {
                    slots[address.index()] = true;
                } else {
                    definitions[address.index()] = Some((block, index));
                }
            }
        }
    }
    for (block, b) in body.blocks.iter().enumerate() {
        for (index, inst) in b.insts.get(&body.insts).unwrap().iter().enumerate() {
            let mut read = |address: AddressId| {
                if !matches!(definitions[address.index()], Some((owner, before)) if owner == block && before < index)
                {
                    slots[address.index()] = true;
                }
            };
            match *inst {
                Inst::Address { address, .. } => {
                    if let AddressBase::Address(base) = body.addresses[address.index()].base {
                        read(base);
                    }
                }
                Inst::Read { address, .. }
                | Inst::Store { address, .. }
                | Inst::Pointer { address, .. }
                | Inst::Case { address, .. }
                | Inst::SetCase { address, .. }
                | Inst::Drop { address, .. }
                | Inst::Mark { address, .. }
                | Inst::Unmark { address, .. } => read(address),
                Inst::Copy { to, from, .. } => {
                    read(to);
                    read(from);
                }
                Inst::Grow { carrier, .. } => read(carrier),
                Inst::Call { call } => {
                    for arg in body.calls[call.index()].args.get(&body.pools.args).unwrap() {
                        if let Arg::Address(address) = *arg {
                            read(address);
                        }
                    }
                }
                Inst::Const { .. }
                | Inst::ReadStatic { .. }
                | Inst::FunctionRef { .. }
                | Inst::Unary { .. }
                | Inst::Binary { .. }
                | Inst::Compare { .. }
                | Inst::Convert { .. }
                | Inst::IntArith { .. }
                | Inst::MulAdd { .. }
                | Inst::ScalarUnary { .. }
                | Inst::StorageLive { .. }
                | Inst::StorageDead { .. }
                | Inst::UpdateMask { .. }
                | Inst::Alloc { .. }
                | Inst::Free { .. }
                | Inst::RegionOpen { .. }
                | Inst::RegionClose { .. }
                | Inst::Trim { .. }
                | Inst::LimitEnter { .. }
                | Inst::LimitExit
                | Inst::Share { .. }
                | Inst::Release { .. }
                | Inst::TryUnwrap { .. } => {}
            }
        }
    }
    for (block, term) in body.terminators.iter().enumerate() {
        match *term {
            Terminator::Raise {
                cause: RaiseCause::Fail(address),
                ..
            } => {
                if !matches!(definitions[address.index()], Some((owner, _)) if owner == block) {
                    slots[address.index()] = true;
                }
            }
            Terminator::Raise {
                cause: RaiseCause::Bounds | RaiseCause::Unwrap,
                ..
            }
            | Terminator::Jump(_)
            | Terminator::Branch { .. }
            | Terminator::Switch { .. }
            | Terminator::Return(_)
            | Terminator::Unreachable
            | Terminator::Propagate => {}
        }
    }
    for (index, definition) in definitions.iter().enumerate() {
        if definition.is_none() {
            slots[index] = true;
        }
    }
    slots
}
