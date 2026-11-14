//! Expand the already-typed numeric recipe at its source call position.
use super::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Site {
    pub call: CallId,
    pub next_block: u32,
    pub op: numeric_glue::NumericOp,
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
    fn new(body: &Body, capacity: usize) -> Self {
        Self {
            insts: Vec::with_capacity(capacity),
            anchors: Vec::with_capacity(capacity),
            next: Vec::with_capacity(capacity),
            first: vec![None; body.blocks.len()],
            last: vec![None; body.blocks.len()],
            skipped: Vec::with_capacity(capacity),
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
        | Inst::Grow { fail, .. }
        | Inst::Drop { fail, .. } => Some(fail),
        _ => None,
    }
}

fn value(body: &mut Body, ty: Ty) -> ValueId {
    let v = ValueId(body.values.len() as u32);
    body.values.push(ty);
    v
}
fn block(body: &mut Body, anchor: Option<Anchor>) -> BlockId {
    let b = BlockId(body.blocks.len() as u32);
    body.blocks.push(Block {
        params: PoolRange::EMPTY,
        insts: PoolRange::EMPTY,
        anchor,
    });
    body.terminators.push(Terminator::Unreachable);
    b
}
fn edge(body: &mut Body, target: BlockId) -> EdgeId {
    let e = EdgeId(body.edges.len() as u32);
    body.edges.push(Edge {
        target,
        args: PoolRange::EMPTY,
    });
    e
}

pub(super) fn inline(module: &Module, body: &mut Body, sites: &[Site]) {
    use numeric_glue::NumericOp;
    if sites.is_empty() {
        return;
    }
    let original_blocks = body.blocks.clone();
    let original_terms = body.terminators.clone();
    let old = std::mem::take(&mut body.insts);
    let anchors = std::mem::take(&mut body.inst_anchors);
    let mut selected = vec![None; body.calls.len()];
    for site in sites {
        selected[site.call.index()] = Some(site.op);
    }
    let mut inserted = vec![None; body.calls.len()];
    let mut tape = Tape::new(body, old.len() + sites.len() * 12);
    for (bi, source_block) in original_blocks.iter().enumerate() {
        let mut current = BlockId(bi as u32);
        for i in source_block.insts.start as usize
            ..(source_block.insts.start + source_block.insts.len) as usize
        {
            let Inst::Call { call } = old[i] else {
                tape.emit(current, old[i].clone(), anchors[i]);
                continue;
            };
            let Some(op) = selected[call.index()] else {
                tape.emit(current, old[i].clone(), anchors[i]);
                continue;
            };
            let site = &body.calls[call.index()];
            let fail = site.fail;
            let signature = &module.signatures[site.signature.index()];
            let args = site.args.get(&body.pools.args).unwrap();
            let dest = signature.dest.map(|_| match args[0] {
                Arg::Address(a) => a,
                _ => unreachable!(),
            });
            let mut operands = [ValueId(0); 2];
            for (i, arg) in args[usize::from(dest.is_some())..].iter().enumerate() {
                let Arg::Value(v) = arg else {
                    unreachable!("closed numeric operands are scalars")
                };
                operands[i] = *v;
            }
            let (target, payload_steps) = if let Some(row) = signature.dest {
                numeric_glue::payload(module, row)
            } else {
                let Ty::Scalar(s) = signature.results[0] else {
                    unreachable!()
                };
                (s, Vec::new())
            };
            let result = if dest.is_none() {
                site.results.get(&body.pools.values).unwrap()[0]
            } else {
                value(body, Ty::Scalar(target))
            };
            let flag = (op.optional() || op == NumericOp::Narrow)
                .then(|| value(body, Ty::Scalar(Scalar::Bool)));
            let success = match op {
                NumericOp::Arithmetic(mode, op) => {
                    tape.emit(
                        current,
                        Inst::IntArith {
                            result,
                            flag,
                            mode,
                            op,
                            left: operands[0],
                            right: operands[1],
                        },
                        anchors[i],
                    );
                    flag.map(|flag| {
                        let result = value(body, Ty::Scalar(Scalar::Bool));
                        tape.emit(
                            current,
                            Inst::Unary {
                                result,
                                op: UnaryOp::Not,
                                operand: flag,
                                fail: None,
                            },
                            anchors[i],
                        );
                        result
                    })
                }
                NumericOp::Narrow | NumericOp::TryNarrow => {
                    tape.emit(
                        current,
                        Inst::Convert {
                            result,
                            flag,
                            operand: operands[0],
                            mode: ConvertMode::TryNarrow,
                        },
                        anchors[i],
                    );
                    flag
                }
            };
            if let Some(dest) = dest
                && let AddressBase::Slot(slot) = body.addresses[dest.index()].base
            {
                let deferred = [
                    tape.defer(tape.lives[slot.index()]),
                    tape.defer(tape.addresses[dest.index()]),
                ];
                for (inst, anchor) in deferred.into_iter().flatten() {
                    tape.emit(current, inst, anchor);
                }
            }
            let start = body.blocks.len() as u32;
            if let Some(success) = success {
                let yes = block(body, anchors[i]);
                let no = block(body, anchors[i]);
                let continuation = if dest.is_some() {
                    block(body, anchors[i])
                } else {
                    yes
                };
                let yes_edge = edge(body, yes);
                let no_edge = edge(body, no);
                body.terminators[current.index()] = Terminator::Branch {
                    condition: success,
                    yes: yes_edge,
                    no: no_edge,
                };
                if let Some(dest) = dest {
                    tape.emit(
                        yes,
                        Inst::SetCase {
                            address: dest,
                            case: 1,
                        },
                        anchors[i],
                    );
                    let address = AddressId(body.addresses.len() as u32);
                    let steps = PoolRange::push(&mut body.pools.steps, payload_steps);
                    body.addresses.push(AddressInfo {
                        base: AddressBase::Address(dest),
                        steps,
                        ty: PlaceTy::Value(Ty::Scalar(target)),
                    });
                    tape.emit(
                        yes,
                        Inst::Address {
                            address,
                            fail: None,
                        },
                        anchors[i],
                    );
                    tape.emit(
                        yes,
                        Inst::Store {
                            address,
                            value: result,
                        },
                        anchors[i],
                    );
                    let done = edge(body, continuation);
                    body.terminators[yes.index()] = Terminator::Jump(done);
                    tape.emit(
                        no,
                        Inst::SetCase {
                            address: dest,
                            case: 0,
                        },
                        anchors[i],
                    );
                    let done = edge(body, continuation);
                    body.terminators[no.index()] = Terminator::Jump(done);
                } else {
                    let max = value(body, Ty::Scalar(Scalar::U8));
                    let one = value(body, Ty::Scalar(Scalar::U8));
                    let unused = value(body, Ty::Scalar(Scalar::U8));
                    tape.emit(
                        no,
                        Inst::Const {
                            result: max,
                            bits: 255,
                        },
                        anchors[i],
                    );
                    tape.emit(
                        no,
                        Inst::Const {
                            result: one,
                            bits: 1,
                        },
                        anchors[i],
                    );
                    tape.emit(
                        no,
                        Inst::Binary {
                            result: unused,
                            op: BinaryOp::Add,
                            left: max,
                            right: one,
                            fail,
                        },
                        anchors[i],
                    );
                    body.terminators[no.index()] = Terminator::Propagate;
                }
                current = continuation;
            }
            inserted[call.index()] = Some((start, body.blocks.len() as u32));
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
