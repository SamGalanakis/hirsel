use super::*;

/// Visit value operands, and optionally definitions. Pool ranges are remapped once
/// separately, so calls, edges and returns share the same substitution.
pub(super) fn values(inst: &mut Inst, definitions: bool, f: &mut impl FnMut(&mut ValueId)) {
    match inst {
        Inst::Const { result, .. }
        | Inst::ReadStatic { result, .. }
        | Inst::FunctionRef { result, .. }
        | Inst::Read { result, .. }
        | Inst::Pointer { result, .. }
        | Inst::RegionOpen { result } => {
            if definitions {
                f(result)
            }
        }
        Inst::Unary {
            result, operand, ..
        }
        | Inst::ScalarUnary {
            result, operand, ..
        } => {
            if definitions {
                f(result)
            }
            f(operand)
        }
        Inst::Binary {
            result,
            left,
            right,
            ..
        }
        | Inst::Compare {
            result,
            left,
            right,
            ..
        } => {
            if definitions {
                f(result)
            }
            f(left);
            f(right)
        }
        Inst::IntArith {
            result,
            flag,
            left,
            right,
            ..
        } => {
            if definitions {
                f(result);
                if let Some(flag) = flag {
                    f(flag)
                }
            }
            f(left);
            f(right)
        }
        Inst::Convert {
            result,
            flag,
            operand,
            ..
        } => {
            if definitions {
                f(result);
                if let Some(flag) = flag {
                    f(flag)
                }
            }
            f(operand)
        }
        Inst::MulAdd {
            result,
            left,
            right,
            addend,
        } => {
            if definitions {
                f(result)
            }
            f(left);
            f(right);
            f(addend)
        }
        Inst::Store { value, .. } => f(value),
        Inst::UpdateMask { result, mask, .. } => {
            if definitions {
                f(result)
            }
            f(mask)
        }
        Inst::Alloc {
            result,
            available,
            region_entry,
            charge,
            overflow,
            size,
            place,
            ..
        } => {
            if definitions {
                f(result);
                f(available);
                if let Some(v) = region_entry {
                    f(v)
                }
            }
            f(charge);
            if let Some(v) = overflow {
                f(v)
            }
            f(size);
            match place {
                AllocPlace::Like(v) | AllocPlace::Region(v) => f(v),
                _ => {}
            }
        }
        Inst::Free { block, charge } => {
            f(block);
            f(charge)
        }
        Inst::Copy { bytes, .. } => f(bytes),
        Inst::RegionClose { region } => f(region),
        Inst::Trim {
            block,
            region,
            entry,
            previous,
            charge,
        } => {
            f(block);
            f(region);
            f(entry);
            f(previous);
            f(charge)
        }
        Inst::LimitEnter {
            admitted,
            available,
            bytes,
        } => {
            if definitions {
                f(admitted);
                f(available)
            }
            f(bytes)
        }
        Inst::Share { node, .. } | Inst::Release { node, .. } => f(node),
        Inst::TryUnwrap { result, node, .. } => {
            if definitions {
                f(result)
            }
            f(node)
        }
        Inst::Case { result, .. } => {
            if definitions {
                f(result)
            }
        }
        Inst::Address { .. }
        | Inst::StorageLive { .. }
        | Inst::StorageDead { .. }
        | Inst::Call { .. }
        | Inst::SetCase { .. }
        | Inst::LimitExit
        | Inst::Mark { .. }
        | Inst::Drop { .. } => {}
    }
}
pub(super) fn addresses(inst: &mut Inst, f: &mut impl FnMut(&mut AddressId)) {
    match inst {
        Inst::Address { address, .. }
        | Inst::Read { address, .. }
        | Inst::Store { address, .. }
        | Inst::Pointer { address, .. }
        | Inst::Case { address, .. }
        | Inst::SetCase { address, .. }
        | Inst::Mark { address, .. }
        | Inst::Drop { address, .. } => f(address),
        Inst::Copy { to, from, .. } => {
            f(to);
            f(from)
        }
        _ => {}
    }
}
pub(super) fn alias(aliases: &[ValueId], mut v: ValueId) -> ValueId {
    while aliases[v.index()] != v {
        v = aliases[v.index()]
    }
    v
}

pub(super) fn compact(
    body: &mut Body,
    aliases: &[ValueId],
    dead_values: &[bool],
    promoted: &[bool],
) {
    let failing = body
        .insts
        .iter()
        .filter_map(|i| {
            if let Inst::Address {
                address,
                fail: Some(_),
            } = i
            {
                Some(*address)
            } else {
                None
            }
        })
        .collect::<std::collections::HashSet<_>>();
    for i in 0..body.addresses.len() {
        if body.addresses[i].steps.len == 0
            && let AddressBase::Address(parent) = body.addresses[i].base
            && !failing.contains(&parent)
            && !body.addresses[parent.index()]
                .steps
                .get(&body.pools.steps)
                .unwrap()
                .iter()
                .any(|s| matches!(s, Step::Element(_)))
        {
            let base = body.addresses[parent.index()].base;
            let steps = body.addresses[parent.index()].steps;
            body.addresses[i].base = base;
            body.addresses[i].steps = steps;
        }
    }
    let mut live = vec![false; body.addresses.len()];
    let mut work = Vec::new();
    let mut root = |a: AddressId| {
        if !std::mem::replace(&mut live[a.index()], true) {
            work.push(a)
        }
    };
    for inst in &mut body.insts {
        match inst {
            Inst::Address {
                address,
                fail: Some(_),
            } => root(*address),
            Inst::Address { .. } => {}
            _ => addresses(inst, &mut |a| root(*a)),
        }
    }
    for call in &body.calls {
        for &arg in call.args.get(&body.pools.args).unwrap() {
            if let Arg::Address(a) = arg {
                root(a)
            }
        }
    }
    for term in &body.terminators {
        if let Terminator::Raise {
            cause: hirsel_core_ir::RaiseCause::Fail(a),
            ..
        } = term
        {
            root(*a)
        }
    }
    while let Some(a) = work.pop() {
        if let AddressBase::Address(parent) = body.addresses[a.index()].base
            && !std::mem::replace(&mut live[parent.index()], true)
        {
            work.push(parent)
        }
    }
    let mut address_map = vec![AddressId(0); live.len()];
    let mut kept = Vec::with_capacity(live.len());
    for (i, address) in std::mem::take(&mut body.addresses).into_iter().enumerate() {
        if live[i] {
            address_map[i] = AddressId(kept.len() as u32);
            kept.push(address)
        }
    }
    body.addresses = kept;
    for a in &mut body.addresses {
        if let AddressBase::Address(parent) = &mut a.base {
            *parent = address_map[parent.index()]
        }
    }
    let old = std::mem::take(&mut body.insts);
    let anchors = std::mem::take(&mut body.inst_anchors);
    for bi in 0..body.blocks.len() {
        let range = body.blocks[bi].insts;
        let start = body.insts.len() as u32;
        for i in range.start as usize..(range.start + range.len) as usize {
            if !matches!(old[i],Inst::Address{address,..}if !live[address.index()]) {
                let mut inst = old[i].clone();
                addresses(&mut inst, &mut |a| *a = address_map[a.index()]);
                body.insts.push(inst);
                body.inst_anchors.push(anchors[i])
            }
        }
        body.blocks[bi].insts = PoolRange::new(start, body.insts.len() as u32 - start);
    }
    for a in &mut body.pools.args {
        if let Arg::Address(a) = a {
            *a = address_map[a.index()]
        }
    }
    for term in &mut body.terminators {
        if let Terminator::Raise {
            cause: hirsel_core_ir::RaiseCause::Fail(a),
            ..
        } = term
        {
            *a = address_map[a.index()]
        }
    }
    let mut used = vec![false; body.values.len()];
    let mut use_value = |v: ValueId| used[alias(aliases, v).index()] = true;
    for inst in &mut body.insts {
        values(inst, false, &mut |v| use_value(*v))
    }
    for address in &body.addresses {
        if let AddressBase::Value(v) = address.base {
            use_value(v)
        }
        for s in address.steps.get(&body.pools.steps).unwrap() {
            if let Step::Index(v) | Step::Element(v) | Step::Byte(v) = s {
                use_value(*v)
            }
        }
    }
    for call in &body.calls {
        if let Callee::Indirect(v) = call.callee {
            use_value(v)
        }
        for arg in call.args.get(&body.pools.args).unwrap() {
            if let Arg::Value(v) = arg {
                use_value(*v)
            }
        }
    }
    for edge in &body.edges {
        for &v in edge.args.get(&body.pools.values).unwrap() {
            use_value(v)
        }
    }
    for term in &body.terminators {
        match term {
            Terminator::Return(v) => {
                for &v in v.get(&body.pools.values).unwrap() {
                    use_value(v)
                }
            }
            Terminator::Branch { condition, .. } => use_value(*condition),
            Terminator::Switch { value, .. } => use_value(*value),
            _ => {}
        }
    }
    let mut dead = dead_values.to_vec();
    let old = std::mem::take(&mut body.insts);
    let anchors = std::mem::take(&mut body.inst_anchors);
    for bi in 0..body.blocks.len() {
        let range = body.blocks[bi].insts;
        let start = body.insts.len() as u32;
        for i in range.start as usize..(range.start + range.len) as usize {
            if let Inst::Const { result, .. } = old[i]
                && !used[result.index()]
            {
                dead[result.index()] = true;
                continue;
            }
            body.insts.push(old[i].clone());
            body.inst_anchors.push(anchors[i]);
        }
        body.blocks[bi].insts = PoolRange::new(start, body.insts.len() as u32 - start);
    }
    let mut value_map = vec![ValueId(0); body.values.len()];
    let mut kept = Vec::with_capacity(body.values.len());
    for (i, t) in std::mem::take(&mut body.values).into_iter().enumerate() {
        if !dead[i] && aliases[i] == ValueId(i as u32) {
            value_map[i] = ValueId(kept.len() as u32);
            kept.push(t)
        }
    }
    body.values = kept;
    let map = |v: &mut ValueId| *v = value_map[alias(aliases, *v).index()];
    for inst in &mut body.insts {
        values(inst, true, &mut |v| map(v))
    }
    for v in &mut body.pools.values {
        map(v)
    }
    for arg in &mut body.pools.args {
        if let Arg::Value(v) = arg {
            map(v)
        }
    }
    for call in &mut body.calls {
        if let Callee::Indirect(v) = &mut call.callee {
            map(v)
        }
    }
    for address in &mut body.addresses {
        if let AddressBase::Value(v) = &mut address.base {
            map(v)
        }
    }
    for step in &mut body.pools.steps {
        if let Step::Index(v) | Step::Element(v) | Step::Byte(v) = step {
            map(v)
        }
    }
    for term in &mut body.terminators {
        match term {
            Terminator::Branch { condition, .. } => map(condition),
            Terminator::Switch { value, .. } => map(value),
            _ => {}
        }
    }
    let mut slots = Vec::with_capacity(body.slots.len());
    let mut slot_map = vec![SlotId(0); body.slots.len()];
    for (i, ty) in std::mem::take(&mut body.slots).into_iter().enumerate() {
        if !promoted[i] {
            slot_map[i] = SlotId(slots.len() as u32);
            slots.push(ty)
        }
    }
    body.slots = slots;
    for address in &mut body.addresses {
        if let AddressBase::Slot(s) = &mut address.base {
            *s = slot_map[s.index()]
        }
    }
    for inst in &mut body.insts {
        if let Inst::StorageLive { slot } | Inst::StorageDead { slot } = inst {
            *slot = slot_map[slot.index()]
        }
    }
}
