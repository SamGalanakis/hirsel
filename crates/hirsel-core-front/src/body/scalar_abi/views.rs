use super::*;

/// Rewrite scalar-sized copies before promoting places. Byte-run copies remain
/// byte-run copies; their byte count need not be one element's size.
pub(super) fn scalar_copies(module: &Module, recipes: &[Recipe], body: &mut Body) -> bool {
    let mut removed_drop = false;
    let constants: HashMap<_, _> = body
        .insts
        .iter()
        .filter_map(|i| {
            if let Inst::Const { result, bits } = i {
                Some((*result, *bits))
            } else {
                None
            }
        })
        .collect();
    let old = std::mem::take(&mut body.insts);
    let anchors = std::mem::take(&mut body.inst_anchors);
    let mut c = Code {
        body,
        insts: Vec::with_capacity(old.len()),
        anchors: Vec::with_capacity(old.len()),
        anchor: None,
    };
    for bi in 0..c.body.blocks.len() {
        let range = c.body.blocks[bi].insts;
        let start = c.insts.len() as u32;
        for i in range.start as usize..(range.start + range.len) as usize {
            c.anchor = anchors[i];
            match old[i] {
                Inst::Copy { bytes, .. } if constants.get(&bytes) == Some(&0) => {}
                Inst::Copy { to, from, bytes }
                    if let Some(t) = place_scalar(module, c.body.addresses[to.index()].ty)
                        && place_scalar(module, c.body.addresses[from.index()].ty) == Some(t)
                        && constants.get(&bytes) == Some(&width(t)) =>
                {
                    let value = c.load(from, t);
                    c.store(to, value);
                }
                Inst::Drop { ty, .. } if recipes[ty.index()].drop.is_none() => removed_drop = true,
                _ => c.emit(old[i].clone()),
            }
        }
        c.body.blocks[bi].insts = PoolRange::new(start, c.insts.len() as u32 - start);
    }
    c.body.insts = c.insts;
    c.body.inst_anchors = c.anchors;
    removed_drop
}

struct Groups {
    parents: Vec<usize>,
    ranks: Vec<u8>,
}
impl Groups {
    fn root(&self, mut a: usize) -> usize {
        while self.parents[a] != a {
            a = self.parents[a]
        }
        a
    }
    fn union(&mut self, a: usize, b: usize) {
        let mut a = self.root(a);
        let mut b = self.root(b);
        if a == b {
            return;
        }
        if self.ranks[a] < self.ranks[b] {
            std::mem::swap(&mut a, &mut b)
        }
        self.parents[b] = a;
        if self.ranks[a] == self.ranks[b] {
            self.ranks[a] += 1
        }
    }
}

/// A view of a scalar is the scalar itself. Lift pointers and their joins together,
/// then replace a read through that view by the incoming scalar. A mutable/address
/// consumer keeps the group's pointers, preserving the original storage identity.
pub(super) fn scalar_views(module: &Module, body: &mut Body, aliases: &mut Vec<ValueId>) {
    let mut groups = Groups {
        parents: (0..body.values.len()).collect(),
        ranks: vec![0; body.values.len()],
    };
    for edge in &body.edges {
        for (&arg, &param) in edge.args.get(&body.pools.values).unwrap().iter().zip(
            body.blocks[edge.target.index()]
                .params
                .get(&body.pools.values)
                .unwrap(),
        ) {
            if body.values[arg.index()] == Ty::Ptr && body.values[param.index()] == Ty::Ptr {
                groups.union(param.index(), arg.index())
            }
        }
    }
    let mut carried_pointer = vec![false; body.values.len()];
    for inst in &body.insts {
        if let Inst::Pointer { result, address } = inst
            && place_scalar(module, body.addresses[address.index()].ty).is_some()
        {
            carried_pointer[groups.root(result.index())] = true;
        }
    }
    for inst in &body.insts {
        if let Inst::Pointer { result, address } = inst
            && place_scalar(module, body.addresses[address.index()].ty).is_some()
        {
            let mut at = *address;
            loop {
                let a = &body.addresses[at.index()];
                if a.steps.len != 0 {
                    break;
                }
                match a.base {
                    AddressBase::Value(v) => {
                        if carried_pointer[groups.root(v.index())] {
                            groups.union(result.index(), v.index());
                            carried_pointer[groups.root(result.index())] = true;
                        }
                        break;
                    }
                    AddressBase::Address(parent) => at = parent,
                    AddressBase::Slot(_) => break,
                }
            }
        }
    }
    let mut types = vec![None; body.values.len()];
    for i in &body.insts {
        if let Inst::Pointer { result, address } = i
            && let Some(t) = place_scalar(module, body.addresses[address.index()].ty)
        {
            types[groups.root(result.index())] = Some(t);
        }
    }
    let constants: HashMap<_, _> = body
        .insts
        .iter()
        .filter_map(|i| {
            if let Inst::Const { result, bits } = i {
                Some((*result, *bits))
            } else {
                None
            }
        })
        .collect();
    let mut of_address = vec![None; body.addresses.len()];
    for (i, a) in body.addresses.iter().enumerate() {
        let from = match a.base {
            AddressBase::Value(v) => Some(v),
            AddressBase::Address(a) => of_address[a.index()],
            _ => None,
        };
        if let Some(v) = from
            && let Some(t) = types[groups.root(v.index())]
            && place_scalar(module, a.ty) == Some(t)
            && a.steps
                .get(&body.pools.steps)
                .unwrap()
                .iter()
                .all(|s| match s {
                    Step::Field(0) => true,
                    Step::Byte(v) | Step::Index(v) => constants.get(v) == Some(&0),
                    _ => false,
                })
        {
            of_address[i] = Some(v);
        }
    }
    let mut escaped = vec![false; body.values.len()];
    let mut escape = |v: ValueId| {
        escaped[groups.root(v.index())] = true;
    };
    // Entry pointer parameters already have their fixed convention: notably mut T.
    for &v in body.blocks[0].params.get(&body.pools.values).unwrap() {
        if body.values[v.index()] == Ty::Ptr {
            escape(v)
        }
    }
    for (i, a) in body.addresses.iter().enumerate() {
        if let AddressBase::Value(v) = a.base
            && of_address[i].is_none()
        {
            escape(v)
        }
    }
    for inst in &mut body.insts {
        values(inst, false, &mut |v| escape(*v));
        match inst {
            Inst::Store { address, .. }
            | Inst::Case { address, .. }
            | Inst::SetCase { address, .. }
            | Inst::Mark { address, .. }
            | Inst::Unmark { address, .. }
            | Inst::Drop { address, .. } => {
                if let Some(v) = of_address[address.index()] {
                    escape(v)
                }
            }
            Inst::Copy { to, from, .. } => {
                for a in [to, from] {
                    if let Some(v) = of_address[a.index()] {
                        escape(v)
                    }
                }
            }
            _ => {}
        }
    }
    for call in &body.calls {
        if let Callee::Indirect(v) = call.callee {
            escape(v)
        }
        for &a in call.args.get(&body.pools.args).unwrap() {
            match a {
                Arg::Value(v) => escape(v),
                Arg::Address(a) => {
                    if let Some(v) = of_address[a.index()] {
                        escape(v)
                    }
                }
            }
        }
    }
    let old = std::mem::take(&mut body.insts);
    let anchors = std::mem::take(&mut body.inst_anchors);
    for (i, t) in body.values.iter_mut().enumerate() {
        if let Some(s) = types[groups.root(i)]
            && !escaped[groups.root(i)]
        {
            *t = s
        }
    }
    let mut c = Code {
        body,
        insts: Vec::with_capacity(old.len()),
        anchors: Vec::with_capacity(old.len()),
        anchor: None,
    };
    for bi in 0..c.body.blocks.len() {
        let range = c.body.blocks[bi].insts;
        let start = c.insts.len() as u32;
        for i in range.start as usize..(range.start + range.len) as usize {
            c.anchor = anchors[i];
            match old[i] {
                Inst::Pointer { result, address }
                    if let Some(v) = of_address[address.index()]
                        && !escaped[groups.root(v.index())] =>
                {
                    aliases[result.index()] = v
                }
                Inst::Pointer { result, address }
                    if let Some(t) = types[groups.root(result.index())]
                        && !escaped[groups.root(result.index())] =>
                {
                    let address = c.field(address, t);
                    c.emit(Inst::Read {
                        result,
                        address,
                        mode: ReadMode::Bits,
                    });
                }
                Inst::Read {
                    result, address, ..
                } if let Some(v) = of_address[address.index()]
                    && !escaped[groups.root(v.index())] =>
                {
                    aliases[result.index()] = v
                }
                _ => c.emit(old[i].clone()),
            }
        }
        c.body.blocks[bi].insts = PoolRange::new(start, c.insts.len() as u32 - start);
    }
    c.body.insts = c.insts;
    c.body.inst_anchors = c.anchors;
}
