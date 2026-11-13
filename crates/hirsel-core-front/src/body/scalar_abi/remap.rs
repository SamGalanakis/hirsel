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

fn mark_values(body: &mut Body, aliases: &[ValueId], used: &mut [bool]) {
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
}

pub(super) fn compact(
    module: &Module,
    body: &mut Body,
    aliases: &[ValueId],
    dead_values: &[bool],
    promoted: &[bool],
    prune_reads: bool,
) {
    calls(body);
    // Rewriting a call appends its new operands to the arenas. Retire old ranges
    // before a subsequent specialization scan sees their former IDs.
    let old_values = std::mem::take(&mut body.pools.values);
    body.pools.values.reserve_exact(old_values.len());
    let mut range = |r: &mut PoolRange<ValueId>| {
        *r = PoolRange::push(
            &mut body.pools.values,
            r.get(&old_values).unwrap().iter().copied(),
        );
    };
    for block in &mut body.blocks {
        range(&mut block.params);
    }
    for call in &mut body.calls {
        range(&mut call.results);
    }
    for edge in &mut body.edges {
        range(&mut edge.args);
    }
    for term in &mut body.terminators {
        if let Terminator::Return(r) = term {
            range(r);
        }
    }
    let mut used = vec![false; body.values.len()];
    let mut dead = dead_values.to_vec();
    if prune_reads {
        mark_values(body, aliases, &mut used);
        for inst in &body.insts {
            if let Inst::Read {
                result,
                mode: ReadMode::Bits,
                ..
            } = inst
                && !used[result.index()]
            {
                dead[result.index()] = true;
            }
        }
    }
    let mut failing = vec![false; body.addresses.len()];
    for inst in &body.insts {
        if let Inst::Address {
            address,
            fail: Some(_),
        } = inst
        {
            failing[address.index()] = true;
        }
    }
    for i in 0..body.addresses.len() {
        if body.addresses[i].steps.len == 0
            && let AddressBase::Address(parent) = body.addresses[i].base
            && !failing[parent.index()]
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
    // A rigid scalar's slot acquires scalar-typed addresses at each former
    // by-address use. Reuse the address established with its StorageLive, just
    // as lowering a closed scalar local does. Other projections stay distinct.
    let mut address_alias = Vec::new();
    if body.slots.iter().enumerate().any(|(i, t)| {
        !promoted[i] && matches!(t, PlaceTy::Declared(_)) && place_scalar(module, *t).is_some()
    }) {
        let mut canonical = vec![None; body.slots.len()];
        address_alias = (0..body.addresses.len())
            .map(|i| AddressId(i as u32))
            .collect::<Vec<_>>();
        for inst in &body.insts {
            if let Inst::Address {
                address,
                fail: None,
            } = inst
            {
                let info = &body.addresses[address.index()];
                if info.steps.len == 0
                    && let AddressBase::Slot(slot) = info.base
                    && matches!(body.slots[slot.index()], PlaceTy::Declared(_))
                    && place_scalar(module, body.slots[slot.index()])
                        .is_some_and(|ty| info.ty == PlaceTy::Value(ty))
                {
                    if let Some(first) = canonical[slot.index()] {
                        address_alias[address.index()] = first;
                    } else {
                        canonical[slot.index()] = Some(*address);
                    }
                }
            }
        }
        for info in &mut body.addresses {
            if let AddressBase::Address(parent) = &mut info.base {
                *parent = address_alias[parent.index()];
            }
        }
    }
    let mut live = vec![false; body.addresses.len()];
    let mut work = Vec::new();
    let mut root = |a: AddressId| {
        let a = address_alias.get(a.index()).copied().unwrap_or(a);
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
            Inst::Read {
                result,
                mode: ReadMode::Bits,
                ..
            } if dead[result.index()] => {}
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
    for (i, &a) in address_alias.iter().enumerate() {
        address_map[i] = address_map[a.index()];
    }
    let old_steps = std::mem::take(&mut body.pools.steps);
    body.pools.steps.reserve_exact(old_steps.len());
    for address in &mut body.addresses {
        address.steps = PoolRange::push(
            &mut body.pools.steps,
            address.steps.get(&old_steps).unwrap().iter().copied(),
        );
    }
    for a in &mut body.addresses {
        if let AddressBase::Address(parent) = &mut a.base {
            *parent = address_map[parent.index()]
        }
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
    used.fill(false);
    mark_values(body, aliases, &mut used);
    for inst in &body.insts {
        if let Inst::Const { result, .. } = inst
            && !used[result.index()]
        {
            dead[result.index()] = true;
        }
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
    for i in 0..value_map.len() {
        value_map[i] = value_map[alias(aliases, ValueId(i as u32)).index()];
    }
    let map = |v: &mut ValueId| *v = value_map[v.index()];
    let len = body.insts.len();
    let old = std::mem::replace(&mut body.insts, Vec::with_capacity(len));
    let anchors = std::mem::replace(&mut body.inst_anchors, Vec::with_capacity(len));
    for block in &mut body.blocks {
        let start = body.insts.len() as u32;
        for i in block.insts.start as usize..(block.insts.start + block.insts.len) as usize {
            if matches!(old[i], Inst::Address {address,..} if !live[address.index()])
                || matches!(old[i], Inst::Const {result,..} | Inst::Read {result,mode:ReadMode::Bits,..} if dead[result.index()])
            {
                continue;
            }
            let mut inst = old[i].clone();
            addresses(&mut inst, &mut |a| *a = address_map[a.index()]);
            values(&mut inst, true, &mut |v| map(v));
            body.insts.push(inst);
            body.inst_anchors.push(anchors[i]);
        }
        block.insts = PoolRange::new(start, body.insts.len() as u32 - start);
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

/// Remove call rows whose recipes have been expanded into the instruction stream.
pub(super) fn calls(body: &mut Body) {
    if body.calls.is_empty() {
        body.pools.args.clear();
        return;
    }
    let mut live = vec![false; body.calls.len()];
    for inst in &body.insts {
        if let Inst::Call { call } = inst {
            live[call.index()] = true;
        }
    }
    let mut call_map = vec![CallId(0); body.calls.len()];
    let mut kept = Vec::new();
    for (i, c) in std::mem::take(&mut body.calls).into_iter().enumerate() {
        if live[i] {
            call_map[i] = CallId(kept.len() as u32);
            kept.push(c);
        }
    }
    body.calls = kept;
    let old_args = std::mem::take(&mut body.pools.args);
    body.pools.args.reserve_exact(old_args.len());
    for call in &mut body.calls {
        call.args = PoolRange::push(
            &mut body.pools.args,
            call.args.get(&old_args).unwrap().iter().copied(),
        );
    }
    for inst in &mut body.insts {
        if let Inst::Call { call } = inst {
            *call = call_map[call.index()];
        }
    }
}
