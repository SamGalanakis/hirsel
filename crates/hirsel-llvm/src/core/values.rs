//! Keep strictly dominated scalar/address uses in SSA. Correlated cleanup
//! and simultaneous block-parameter transfers retain their existing homes.
use super::*;

pub(super) fn slots(body: &Body, flow: &flow::Flow<'_>) -> Vec<bool> {
    let mut owners = vec![None; body.values.len()];
    for (block, b) in body.blocks.iter().enumerate() {
        for (index, inst) in b.insts.get(&body.insts).unwrap().iter().enumerate() {
            let result = match *inst {
                Inst::Unary { result, .. }
                | Inst::CompactWidth { result, .. }
                | Inst::Binary { result, .. }
                | Inst::Compare { result, .. }
                | Inst::Convert { result, .. }
                | Inst::IntArith { result, .. }
                | Inst::MulAdd { result, .. }
                | Inst::ScalarUnary { result, .. }
                | Inst::Simd { result, .. }
                | Inst::Read { result, .. }
                | Inst::ReadStatic { result, .. }
                | Inst::FunctionRef { result, .. }
                | Inst::DescriptorRef { result, .. }
                | Inst::Pointer { result, .. }
                | Inst::UpdateMask { result, .. }
                | Inst::Case { result, .. }
                | Inst::RegionOpen { result }
                | Inst::TryUnwrap { result, .. }
                | Inst::FrameActive { result, .. } => Some(result),
                _ => None,
            };
            if let Some(v) = result {
                owners[v.index()] = Some((block, index));
            }
            // These producers join native fast/slow paths before publishing
            // each result exactly once. Named failure edges leave before it.
            let mut define = |v: ValueId| owners[v.index()] = Some((block, index));
            match *inst {
                Inst::Convert { flag: Some(v), .. } | Inst::IntArith { flag: Some(v), .. } => {
                    define(v)
                }
                Inst::Alloc {
                    result,
                    available,
                    region_entry,
                    ..
                } => {
                    define(result);
                    define(available);
                    if let Some(v) = region_entry {
                        define(v);
                    }
                }
                Inst::Grow {
                    result, available, ..
                } => {
                    define(result);
                    define(available);
                }
                Inst::FrameAdmit {
                    admitted,
                    available,
                    ..
                }
                | Inst::LimitEnter {
                    admitted,
                    available,
                    ..
                } => {
                    define(admitted);
                    define(available);
                }
                Inst::Call { call } => {
                    for &result in body.calls[call.index()]
                        .results
                        .get(&body.pools.values)
                        .unwrap()
                    {
                        define(result);
                    }
                }
                _ => {}
            }
        }
    }
    // A parameter of a block entered by one edge is that edge's argument: the
    // edge binds it by name, with no join to build. The parameter is defined at
    // its block's entry.
    let mut incoming = vec![0usize; body.blocks.len()];
    for edge in &body.edges {
        incoming[edge.target.index()] += 1;
    }
    let mut entries = vec![None; body.values.len()];
    for (block, b) in body.blocks.iter().enumerate().skip(1) {
        // An unreachable block's one edge may come from itself.
        if incoming[block] == 1 && flow.block_dominates(0, block) {
            for v in b.params.get(&body.pools.values).unwrap() {
                entries[v.index()] = Some(block);
            }
        }
    }
    let available = |v: ValueId, at: (usize, usize)| {
        owners[v.index()].is_some_and(|definition| flow.available(definition, at))
            || entries[v.index()]
                .is_some_and(|entry| entry == at.0 || flow.block_dominates(entry, at.0))
    };
    let mut slots: Vec<_> = owners
        .iter()
        .zip(&entries)
        .map(|(owner, entry)| owner.is_none() && entry.is_none())
        .collect();
    for (block, b) in body.blocks.iter().enumerate() {
        for (index, inst) in b.insts.get(&body.insts).unwrap().iter().enumerate() {
            let mut read = |v: ValueId| {
                if !available(v, (block, index)) {
                    slots[v.index()] = true;
                }
            };
            match *inst {
                Inst::Simd { first, second, .. } => {
                    read(first);
                    if let Some(second) = second {
                        read(second);
                    }
                }
                Inst::PutLanes { vector, count, .. } => {
                    read(vector);
                    read(count);
                }
                Inst::Prefetch { data, offset } => {
                    read(data);
                    read(offset);
                }
                Inst::Unary { operand, .. }
                | Inst::CompactWidth { word: operand, .. }
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
                    if let AllocPlace::Region(v)
                    | AllocPlace::Like(v)
                    | AllocPlace::LentFrame(v)
                    | AllocPlace::ByteTail(v) = place
                    {
                        read(v);
                    }
                }
                Inst::Free { block, charge } => {
                    read(block);
                    read(charge);
                }
                Inst::FrameAdmit {
                    charge, overflow, ..
                } => {
                    read(charge);
                    if let Some(v) = overflow {
                        read(v);
                    }
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
                Inst::Copy { bytes, .. }
                | Inst::Move { bytes, .. }
                | Inst::LimitEnter { bytes, .. } => read(bytes),
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
                | Inst::LayoutConst { .. }
                | Inst::ReadStatic { .. }
                | Inst::FunctionRef { .. }
                | Inst::DescriptorRef { .. }
                | Inst::Read { .. }
                | Inst::StorageLive { .. }
                | Inst::StorageDead { .. }
                | Inst::Pointer { .. }
                | Inst::Case { .. }
                | Inst::SetCase { .. }
                | Inst::RegionOpen { .. }
                | Inst::Step { .. }
                | Inst::StackCheck { .. }
                | Inst::PromoteFrame { .. }
                | Inst::BuiltinUse { .. }
                | Inst::LoopIteration
                | Inst::LimitExit
                | Inst::Drop { .. }
                | Inst::Mark { .. }
                | Inst::Unmark { .. }
                | Inst::FrameRefund { .. }
                | Inst::FrameActive { .. } => {}
            }
        }
        let mut read = |v: ValueId| {
            if !available(v, (block, b.insts.len as usize)) {
                slots[v.index()] = true;
            }
        };
        match body.terminators[block] {
            Terminator::Jump(edge) => {
                for value in body.edges[edge.index()]
                    .args
                    .get(&body.pools.values)
                    .unwrap()
                {
                    read(*value);
                }
            }
            Terminator::Branch { condition, yes, no } => {
                read(condition);
                for edge in [yes, no] {
                    for value in body.edges[edge.index()]
                        .args
                        .get(&body.pools.values)
                        .unwrap()
                    {
                        read(*value);
                    }
                }
            }
            Terminator::Switch {
                value,
                arms,
                otherwise,
            } => {
                read(value);
                for edge in std::iter::once(otherwise)
                    .chain(arms.get(&body.pools.arms).unwrap().iter().map(|a| a.edge))
                {
                    for value in body.edges[edge.index()]
                        .args
                        .get(&body.pools.values)
                        .unwrap()
                    {
                        read(*value);
                    }
                }
            }
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

/// Cache multiply projected addresses and uses not dominated by a projection.
/// This includes inactive cleanup operands before their live projection.
pub(super) fn address_slots(body: &Body, flow: &flow::Flow<'_>) -> Vec<bool> {
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
                if !definitions[address.index()]
                    .is_some_and(|definition| flow.available(definition, (block, index)))
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
                | Inst::PutLanes { to: address, .. }
                | Inst::Pointer { address, .. }
                | Inst::Case { address, .. }
                | Inst::SetCase { address, .. }
                | Inst::Drop { address, .. }
                | Inst::Mark { address, .. }
                | Inst::Unmark { address, .. } => read(address),
                Inst::Copy { to, from, .. } | Inst::Move { to, from, .. } => {
                    read(to);
                    read(from);
                }
                Inst::Grow { carrier, .. } | Inst::PromoteFrame { carrier } => read(carrier),
                Inst::Call { call } => {
                    for arg in body.calls[call.index()].args.get(&body.pools.args).unwrap() {
                        if let Arg::Address(address) = *arg {
                            read(address);
                        }
                    }
                }
                Inst::Const { .. }
                | Inst::LayoutConst { .. }
                | Inst::ReadStatic { .. }
                | Inst::FunctionRef { .. }
                | Inst::DescriptorRef { .. }
                | Inst::Unary { .. }
                | Inst::CompactWidth { .. }
                | Inst::Binary { .. }
                | Inst::Compare { .. }
                | Inst::Convert { .. }
                | Inst::IntArith { .. }
                | Inst::MulAdd { .. }
                | Inst::ScalarUnary { .. }
                | Inst::Simd { .. }
                | Inst::StorageLive { .. }
                | Inst::StorageDead { .. }
                | Inst::UpdateMask { .. }
                | Inst::Alloc { .. }
                | Inst::Free { .. }
                | Inst::FrameAdmit { .. }
                | Inst::FrameActive { .. }
                | Inst::FrameRefund { .. }
                | Inst::RegionOpen { .. }
                | Inst::RegionClose { .. }
                | Inst::Trim { .. }
                | Inst::LimitEnter { .. }
                | Inst::Step { .. }
                | Inst::StackCheck { .. }
                | Inst::BuiltinUse { .. }
                | Inst::LoopIteration
                | Inst::LimitExit
                | Inst::Share { .. }
                | Inst::Release { .. }
                | Inst::Prefetch { .. }
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
                if !definitions[address.index()].is_some_and(|definition| {
                    flow.available(definition, (block, body.blocks[block].insts.len as usize))
                }) {
                    slots[address.index()] = true;
                }
            }
            Terminator::Raise {
                cause: RaiseCause::Bounds | RaiseCause::Unwrap | RaiseCause::Capacity { .. },
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
