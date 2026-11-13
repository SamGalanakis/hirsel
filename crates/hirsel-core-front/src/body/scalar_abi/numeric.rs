//! Expand the already-typed numeric recipe at its source call position.
use super::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Site {
    pub call: CallId,
    pub next_block: u32,
}

// Instructions share one arena; each block owns a chain of arena indices.
struct Tape {
    insts: Vec<Inst>,
    anchors: Vec<Option<Anchor>>,
    next: Vec<Option<usize>>,
    first: Vec<Option<usize>>,
    last: Vec<Option<usize>>,
    skipped: Vec<bool>,
    lives: Vec<Option<usize>>,
    addresses: Vec<Option<usize>>,
}
impl Tape {
    fn new(body: &Body) -> Self {
        Self {
            insts: Vec::new(),
            anchors: Vec::new(),
            next: Vec::new(),
            first: vec![None; body.blocks.len()],
            last: vec![None; body.blocks.len()],
            skipped: Vec::new(),
            lives: vec![None; body.slots.len()],
            addresses: vec![None; body.addresses.len()],
        }
    }
    fn emit(&mut self, block: BlockId, inst: Inst, anchor: Option<Anchor>) {
        let i = self.insts.len();
        self.first
            .resize(self.first.len().max(block.index() + 1), None);
        self.last
            .resize(self.last.len().max(block.index() + 1), None);
        if let Some(last) = self.last[block.index()] {
            self.next[last] = Some(i);
        } else {
            self.first[block.index()] = Some(i);
        }
        self.last[block.index()] = Some(i);
        match inst {
            Inst::StorageLive { slot } => self.lives[slot.index()] = Some(i),
            Inst::Address { address, .. } => {
                self.addresses
                    .resize(self.addresses.len().max(address.index() + 1), None);
                self.addresses[address.index()] = Some(i);
            }
            _ => {}
        }
        self.insts.push(inst);
        self.anchors.push(anchor);
        self.next.push(None);
        self.skipped.push(false);
    }
    fn defer(&mut self, index: Option<usize>) -> Option<(Inst, Option<Anchor>)> {
        index.map(|i| {
            self.skipped[i] = true;
            (self.insts[i].clone(), self.anchors[i])
        })
    }
}

fn fail_operand(inst: &mut Inst) -> Option<&mut Option<BlockId>> {
    match inst {
        Inst::Unary { fail, .. }
        | Inst::Binary { fail, .. }
        | Inst::Address { fail, .. }
        | Inst::Alloc { fail, .. }
        | Inst::Drop { fail, .. } => Some(fail),
        _ => None,
    }
}

pub(super) fn inline(module: &Module, body: &mut Body, sites: &[Site]) {
    if sites.is_empty() {
        return;
    }
    let original_blocks = body.blocks.clone();
    let original_terms = body.terminators.clone();
    let old = std::mem::take(&mut body.insts);
    let anchors = std::mem::take(&mut body.inst_anchors);
    let selected: HashMap<_, _> = sites.iter().map(|s| (s.call, s.next_block)).collect();
    let mut inserted = vec![None; body.calls.len()];
    let mut tape = Tape::new(body);
    let mut map = Vec::new();
    let mut returned = Vec::new();
    let mut address_map = Vec::new();
    let mut blocks = Vec::new();
    let mut edges = Vec::new();
    for (bi, block) in original_blocks.iter().enumerate() {
        let mut current = BlockId(bi as u32);
        for i in block.insts.start as usize..(block.insts.start + block.insts.len) as usize {
            let Inst::Call { call } = old[i] else {
                tape.emit(current, old[i].clone(), anchors[i]);
                continue;
            };
            if !selected.contains_key(&call) {
                tape.emit(current, old[i].clone(), anchors[i]);
                continue;
            }
            let site = body.calls[call.index()].clone();
            let Callee::Direct(function) = site.callee else {
                unreachable!("numeric recipes are direct")
            };
            let function = &module.functions[function.index()];
            let helper = function.body.as_ref().unwrap();
            let signature = &module.signatures[function.signature.index()];
            let entry = helper.blocks[0].params.get(&helper.pools.values).unwrap();
            let args = site.args.get(&body.pools.args).unwrap();
            let dest = signature.dest.map(|_| match args[0] {
                Arg::Address(a) => a,
                _ => unreachable!(),
            });
            map.clear();
            map.resize(helper.values.len(), None);
            for (&param, arg) in entry.iter().zip(args) {
                if let Arg::Value(v) = arg {
                    map[param.index()] = Some(*v);
                }
            }
            returned.clear();
            returned.extend(helper.terminators.iter().enumerate().filter_map(|(bi, t)| {
                if let Terminator::Return(v) = t {
                    Some((bi, *v))
                } else {
                    None
                }
            }));
            if dest.is_none() {
                let values = returned[0].1.get(&helper.pools.values).unwrap();
                for (&from, &to) in values
                    .iter()
                    .zip(site.results.get(&body.pools.values).unwrap())
                {
                    map[from.index()] = Some(to);
                }
            }
            for (v, t) in helper.values.iter().enumerate() {
                if map[v].is_none() && !(dest.is_some() && v == entry[0].index()) {
                    map[v] = Some(ValueId(body.values.len() as u32));
                    body.values.push(*t);
                }
            }
            let value = |v: ValueId| map[v.index()].unwrap();
            address_map.clear();
            let mut root = None;
            for (ai, address) in helper.addresses.iter().enumerate() {
                if dest.is_some()
                    && address.base == AddressBase::Value(entry[0])
                    && address.steps.len == 0
                {
                    address_map.push(dest.unwrap());
                    root = Some(AddressId(ai as u32));
                    continue;
                }
                let mut address = address.clone();
                address.base = match address.base {
                    AddressBase::Value(v) => AddressBase::Value(value(v)),
                    AddressBase::Address(a) => AddressBase::Address(address_map[a.index()]),
                    AddressBase::Slot(_) => unreachable!("numeric recipes have no local storage"),
                };
                let steps =
                    address
                        .steps
                        .get(&helper.pools.steps)
                        .unwrap()
                        .iter()
                        .map(|s| match *s {
                            Step::Index(v) => Step::Index(value(v)),
                            Step::Element(v) => Step::Element(value(v)),
                            Step::Byte(v) => Step::Byte(value(v)),
                            other => other,
                        });
                address.steps = PoolRange::push(&mut body.pools.steps, steps);
                address_map.push(AddressId(body.addresses.len() as u32));
                body.addresses.push(address);
            }
            let mut deferred = [None, None];
            if let Some(dest) = dest {
                if let AddressBase::Slot(slot) = body.addresses[dest.index()].base {
                    deferred[0] = tape.defer(tape.lives[slot.index()]);
                    deferred[1] = tape.defer(tape.addresses[dest.index()]);
                }
            }
            let start = body.blocks.len() as u32;
            blocks.clear();
            blocks.push(current);
            for block in helper.blocks.iter().skip(1) {
                blocks.push(BlockId(body.blocks.len() as u32));
                body.blocks.push(Block {
                    params: PoolRange::push(
                        &mut body.pools.values,
                        block
                            .params
                            .get(&helper.pools.values)
                            .unwrap()
                            .iter()
                            .map(|&v| value(v)),
                    ),
                    insts: PoolRange::EMPTY,
                    anchor: anchors[i],
                });
                body.terminators.push(Terminator::Unreachable);
            }
            let continuation = if dest.is_some() {
                let b = BlockId(body.blocks.len() as u32);
                body.blocks.push(Block {
                    params: PoolRange::EMPTY,
                    insts: PoolRange::EMPTY,
                    anchor: anchors[i],
                });
                body.terminators.push(Terminator::Unreachable);
                b
            } else {
                blocks[returned[0].0]
            };
            inserted[call.index()] = Some((start, body.blocks.len() as u32));
            edges.clear();
            for edge in &helper.edges {
                let values = edge
                    .args
                    .get(&helper.pools.values)
                    .unwrap()
                    .iter()
                    .map(|&v| value(v));
                let args = PoolRange::push(&mut body.pools.values, values);
                edges.push(EdgeId(body.edges.len() as u32));
                body.edges.push(Edge {
                    target: blocks[edge.target.index()],
                    args,
                });
            }
            for (hi, helper_block) in helper.blocks.iter().enumerate() {
                for inst in helper_block.insts.get(&helper.insts).unwrap() {
                    if matches!(inst,Inst::Address{address,..}if Some(*address)==root) {
                        for item in &mut deferred {
                            if let Some((inst, anchor)) = item.take() {
                                tape.emit(blocks[hi], inst, anchor);
                            }
                        }
                        continue;
                    }
                    let can_fail = inst.can_fail(helper);
                    let mut inst = inst.clone();
                    values(&mut inst, true, &mut |v| *v = value(*v));
                    addresses(&mut inst, &mut |a| *a = address_map[a.index()]);
                    if can_fail && let Some(fail) = fail_operand(&mut inst) {
                        *fail = fail.map(|b| blocks[b.index()]).or(site.fail);
                    }
                    tape.emit(blocks[hi], inst, anchors[i]);
                }
                let term = match helper.terminators[hi].clone() {
                    Terminator::Jump(e) => Terminator::Jump(edges[e.index()]),
                    Terminator::Branch { condition, yes, no } => Terminator::Branch {
                        condition: value(condition),
                        yes: edges[yes.index()],
                        no: edges[no.index()],
                    },
                    Terminator::Return(_) if dest.is_none() => continue,
                    Terminator::Return(_) => {
                        let e = EdgeId(body.edges.len() as u32);
                        body.edges.push(Edge {
                            target: continuation,
                            args: PoolRange::EMPTY,
                        });
                        Terminator::Jump(e)
                    }
                    Terminator::Propagate => {
                        if let Some(target) = site.fail {
                            let e = EdgeId(body.edges.len() as u32);
                            body.edges.push(Edge {
                                target,
                                args: PoolRange::EMPTY,
                            });
                            Terminator::Jump(e)
                        } else {
                            Terminator::Propagate
                        }
                    }
                    _ => unreachable!("numeric recipe control flow"),
                };
                body.terminators[blocks[hi].index()] = term;
            }
            current = continuation;
        }
        body.terminators[current.index()] = original_terms[bi].clone();
    }
    let mut first: Vec<Option<usize>> = vec![None; original_blocks.len() + 1];
    let mut last = first.clone();
    let mut next = vec![None; sites.len()];
    for (i, site) in sites.iter().enumerate() {
        if inserted[site.call.index()].is_some() {
            let b = site.next_block as usize;
            if let Some(previous) = last[b] {
                next[previous] = Some(i);
            } else {
                first[b] = Some(i);
            }
            last[b] = Some(i);
        }
    }
    let mut order = Vec::with_capacity(body.blocks.len());
    for (i, head) in first.into_iter().enumerate() {
        let mut at = head;
        while let Some(j) = at {
            let (start, end) = inserted[sites[j].call.index()].unwrap();
            order.extend(start..end);
            at = next[j];
        }
        if i < original_blocks.len() {
            order.push(i as u32);
        }
    }
    let mut remap = vec![BlockId(0); body.blocks.len()];
    for (new, &old) in order.iter().enumerate() {
        remap[old as usize] = BlockId(new as u32);
    }
    let old_blocks = std::mem::take(&mut body.blocks);
    let old_terms = std::mem::take(&mut body.terminators);
    for old in order {
        let mut block = old_blocks[old as usize].clone();
        let begin = body.insts.len() as u32;
        let mut at = tape.first.get(old as usize).copied().flatten();
        while let Some(i) = at {
            at = tape.next[i];
            if !tape.skipped[i] {
                body.insts.push(tape.insts[i].clone());
                body.inst_anchors.push(tape.anchors[i]);
            }
        }
        block.insts = PoolRange::new(begin, body.insts.len() as u32 - begin);
        body.blocks.push(block);
        body.terminators.push(old_terms[old as usize].clone());
    }
    for edge in &mut body.edges {
        edge.target = remap[edge.target.index()];
    }
    for inst in &mut body.insts {
        if let Some(Some(b)) = fail_operand(inst) {
            *b = remap[b.index()];
        }
    }
    for call in &mut body.calls {
        call.fail = call.fail.map(|b| remap[b.index()]);
    }
    for term in &mut body.terminators {
        if let Terminator::Raise {
            cleanup: Some(b), ..
        } = term
        {
            *b = remap[b.index()];
        }
    }
    remap::calls(body);
}
