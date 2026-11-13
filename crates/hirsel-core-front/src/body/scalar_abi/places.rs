use super::*;

#[derive(Clone, Copy)]
struct Pred {
    block: usize,
    edge: EdgeId,
    next: Option<usize>,
}
struct Returns {
    heads: Vec<Option<usize>>,
    preds: Vec<Pred>,
    end: Vec<Option<ValueId>>,
    entry: Vec<Option<ValueId>>,
    phis: Vec<Option<ValueId>>,
    incoming: HashMap<EdgeId, ValueId>,
    trivial: Vec<(ValueId, ValueId)>,
    ty: Ty,
}
impl Returns {
    fn new(body: &Body, end: Vec<Option<ValueId>>, ty: Ty) -> Self {
        let mut out = Self {
            heads: vec![None; body.blocks.len()],
            preds: vec![],
            end,
            entry: vec![None; body.blocks.len()],
            phis: vec![None; body.blocks.len()],
            incoming: HashMap::default(),
            trivial: vec![],
            ty,
        };
        for (bi, term) in body.terminators.iter().enumerate() {
            let mut edge = |e: EdgeId| {
                let target = body.edges[e.index()].target.index();
                let next = out.heads[target];
                out.heads[target] = Some(out.preds.len());
                out.preds.push(Pred {
                    block: bi,
                    edge: e,
                    next,
                });
            };
            match term {
                Terminator::Jump(e) => edge(*e),
                Terminator::Branch { yes, no, .. } => {
                    edge(*yes);
                    edge(*no)
                }
                Terminator::Switch {
                    arms, otherwise, ..
                } => {
                    for a in arms.get(&body.pools.arms).unwrap() {
                        edge(a.edge)
                    }
                    edge(*otherwise)
                }
                _ => {}
            }
        }
        out
    }
    fn read_end(&mut self, body: &mut Body, block: usize) -> ValueId {
        if let Some(v) = self.end[block] {
            v
        } else {
            self.read_entry(body, block)
        }
    }
    fn read_entry(&mut self, body: &mut Body, block: usize) -> ValueId {
        if let Some(v) = self.entry[block] {
            return v;
        }
        let head = self.heads[block].expect("a result is written before return");
        let p = self.preds[head];
        if p.next.is_none() {
            let v = self.read_end(body, p.block);
            self.entry[block] = Some(v);
            return v;
        }
        let phi = ValueId(body.values.len() as u32);
        body.values.push(self.ty);
        self.entry[block] = Some(phi);
        let mut common = None;
        let mut different = false;
        let mut next = Some(head);
        while let Some(i) = next {
            let p = self.preds[i];
            next = p.next;
            let value = self.read_end(body, p.block);
            self.incoming.insert(p.edge, value);
            if value != phi {
                if let Some(old) = common {
                    different |= old != value
                } else {
                    common = Some(value)
                }
            }
        }
        if !different && let Some(value) = common {
            self.entry[block] = Some(value);
            self.trivial.push((phi, value));
            value
        } else {
            self.phis[block] = Some(phi);
            phi
        }
    }
    fn finish(&self, body: &mut Body) {
        let mut scratch = Vec::new();
        for (bi, phi) in self.phis.iter().enumerate() {
            if let Some(phi) = phi {
                scratch.clear();
                scratch.extend_from_slice(body.blocks[bi].params.get(&body.pools.values).unwrap());
                scratch.push(*phi);
                body.blocks[bi].params =
                    PoolRange::push(&mut body.pools.values, scratch.iter().copied());
                let mut next = self.heads[bi];
                while let Some(i) = next {
                    let p = self.preds[i];
                    next = p.next;
                    scratch.clear();
                    scratch.extend_from_slice(
                        body.edges[p.edge.index()]
                            .args
                            .get(&body.pools.values)
                            .unwrap(),
                    );
                    scratch.push(self.incoming[&p.edge]);
                    body.edges[p.edge.index()].args =
                        PoolRange::push(&mut body.pools.values, scratch.iter().copied());
                }
            }
        }
    }
}

fn cell(
    module: &Module,
    body: &Body,
    pointers: &[Option<AddressId>],
    cache: &mut [Option<Option<SlotId>>],
    a: AddressId,
) -> Option<SlotId> {
    if let Some(c) = cache[a.index()] {
        return c;
    }
    cache[a.index()] = Some(None);
    let address = &body.addresses[a.index()];
    let (root, wrapped) = match address.base {
        AddressBase::Slot(s) => (
            Some(s),
            matches!(body.slots[s.index()], PlaceTy::Declared(_)),
        ),
        AddressBase::Address(a) => (
            cell(module, body, pointers, cache, a),
            matches!(body.addresses[a.index()].ty, PlaceTy::Declared(_)),
        ),
        AddressBase::Value(v) => {
            if let Some(a) = pointers[v.index()] {
                (
                    cell(module, body, pointers, cache, a),
                    matches!(body.addresses[a.index()].ty, PlaceTy::Declared(_)),
                )
            } else {
                (None, false)
            }
        }
    };
    let root = root.filter(|s| {
        place_scalar(module, body.slots[s.index()]).is_some()
            && address
                .steps
                .get(&body.pools.steps)
                .unwrap()
                .iter()
                .all(|s| matches!(s, Step::Field(0)) && wrapped)
    });
    cache[a.index()] = Some(root);
    root
}

pub(super) fn promote(module: &Module, recipes: &[Recipe], body: &mut Body, dest: Option<SlotId>) {
    let prune_reads = scalar_copies(module, recipes, body);
    let mut aliases = (0..body.values.len())
        .map(|i| ValueId(i as u32))
        .collect::<Vec<_>>();
    scalar_views(module, body, &mut aliases);
    let mut pointers = vec![None; body.values.len()];
    for inst in &body.insts {
        if let Inst::Pointer { result, address } = inst {
            pointers[result.index()] = Some(*address)
        }
    }
    let mut cells = vec![None; body.addresses.len()];
    for i in 0..body.addresses.len() {
        cell(module, body, &pointers, &mut cells, AddressId(i as u32));
    }
    let cells = cells
        .into_iter()
        .map(|c| c.unwrap_or(None))
        .collect::<Vec<_>>();
    let mut escaped = vec![false; body.slots.len()];
    let mut escape_value = |v: ValueId| {
        if let Some(a) = pointers[v.index()]
            && let Some(s) = cells[a.index()]
        {
            escaped[s.index()] = true
        }
    };
    for (i, address) in body.addresses.iter().enumerate() {
        if cells[i].is_none()
            && let AddressBase::Value(v) = address.base
        {
            escape_value(v)
        }
    }
    for inst in &mut body.insts {
        values(inst, false, &mut |v| escape_value(*v));
    }
    for edge in &body.edges {
        for &v in edge.args.get(&body.pools.values).unwrap() {
            escape_value(v)
        }
    }
    for call in &body.calls {
        if let Callee::Indirect(v) = call.callee {
            escape_value(v)
        }
        for &arg in call.args.get(&body.pools.args).unwrap() {
            if let Arg::Value(v) = arg {
                escape_value(v)
            }
        }
    }
    for inst in &body.insts {
        match inst {
            Inst::Read {
                result, address, ..
            } if let Some(slot) = cells[address.index()]
                && place_scalar(module, body.slots[slot.index()])
                    != Some(body.values[result.index()]) =>
            {
                escaped[slot.index()] = true;
            }
            Inst::Store { value, address, .. }
                if let Some(slot) = cells[address.index()]
                    && place_scalar(module, body.slots[slot.index()])
                        != Some(body.values[value.index()]) =>
            {
                escaped[slot.index()] = true;
            }
            Inst::Copy { to, from, .. } => {
                for a in [to, from] {
                    if let Some(s) = cells[a.index()] {
                        escaped[s.index()] = true
                    }
                }
            }
            Inst::Drop { address, .. }
            | Inst::Mark { address, .. }
            | Inst::Case { address, .. }
            | Inst::SetCase { address, .. } => {
                if let Some(s) = cells[address.index()] {
                    escaped[s.index()] = true
                }
            }
            _ => {}
        }
    }
    for call in &body.calls {
        for &arg in call.args.get(&body.pools.args).unwrap() {
            if let Arg::Address(a) = arg
                && let Some(s) = cells[a.index()]
            {
                escaped[s.index()] = true
            }
        }
    }
    let mut counts = vec![0; body.slots.len()];
    let mut reads = vec![0; body.slots.len()];
    let mut written = vec![None; body.slots.len()];
    let mut end = vec![None; body.blocks.len()];
    for (bi, b) in body.blocks.iter().enumerate() {
        for inst in b.insts.get(&body.insts).unwrap() {
            if let Inst::Read { address, .. } = inst
                && let Some(s) = cells[address.index()]
            {
                reads[s.index()] += 1;
            }
            if let Inst::Store { address, value } = inst
                && let Some(s) = cells[address.index()]
            {
                counts[s.index()] += 1;
                written[s.index()] = Some(*value);
                if Some(s) == dest {
                    end[bi] = Some(*value)
                }
            }
        }
    }
    let promoted = body
        .slots
        .iter()
        .enumerate()
        .map(|(i, t)| {
            place_scalar(module, *t).is_some()
                && !escaped[i]
                && (reads[i] == 0
                    || (matches!(t, PlaceTy::Declared(_))
                        && (counts[i] <= 1 || dest == Some(SlotId(i as u32)))))
        })
        .collect::<Vec<_>>();
    let mut returns = dest.filter(|s| promoted[s.index()]).map(|s| {
        Returns::new(
            body,
            end,
            place_scalar(module, body.slots[s.index()]).unwrap(),
        )
    });
    let old = std::mem::take(&mut body.insts);
    let old_anchors = std::mem::take(&mut body.inst_anchors);
    let mut dead = vec![false; body.values.len()];
    let mut current = None;
    for bi in 0..body.blocks.len() {
        let range = body.blocks[bi].insts;
        let start = body.insts.len() as u32;
        current.take();
        for i in range.start as usize..(range.start + range.len) as usize {
            let keep = match old[i] {
                Inst::Read {
                    result, address, ..
                } if let Some(s) = cells[address.index()]
                    && promoted[s.index()] =>
                {
                    let value = if counts[s.index()] <= 1 {
                        written[s.index()].expect("initialized scalar place")
                    } else if let Some(v) = current {
                        v
                    } else {
                        returns.as_mut().unwrap().read_entry(body, bi)
                    };
                    aliases[result.index()] = value;
                    false
                }
                Inst::Store { address, value }
                    if let Some(s) = cells[address.index()]
                        && promoted[s.index()] =>
                {
                    if Some(s) == dest {
                        current = Some(value)
                    }
                    false
                }
                Inst::Pointer { result, address }
                    if let Some(s) = cells[address.index()]
                        && promoted[s.index()] =>
                {
                    dead[result.index()] = true;
                    false
                }
                Inst::Address { address, .. }
                    if let Some(s) = cells[address.index()]
                        && promoted[s.index()] =>
                {
                    false
                }
                Inst::StorageLive { slot } | Inst::StorageDead { slot }
                    if promoted[slot.index()] =>
                {
                    false
                }
                _ => true,
            };
            if keep {
                body.insts.push(old[i].clone());
                body.inst_anchors.push(old_anchors[i]);
            }
        }
        body.blocks[bi].insts = PoolRange::new(start, body.insts.len() as u32 - start);
    }
    aliases.extend((aliases.len()..body.values.len()).map(|i| ValueId(i as u32)));
    dead.resize(body.values.len(), false);
    if let Some(ssa) = returns {
        ssa.finish(body);
        for (a, b) in ssa.trivial {
            aliases[a.index()] = b
        }
    }
    compact(module, body, &aliases, &dead, &promoted, prune_reads);
}
