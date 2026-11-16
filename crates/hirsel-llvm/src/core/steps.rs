//! Exact step batching. Unchecked copies contain no failing or observable body
//! operation; a short window executes one original checked trip.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub(super) struct Loop {
    pub header: usize,
    nodes: BTreeSet<usize>,
    index: ValueId,
    limit: ValueId,
    yes: EdgeId,
    no: EdgeId,
    cost: u64,
    index_slot: AddressId,
    constant: Option<(u64, u64)>,
}
fn next(body: &Body, block: usize) -> Option<Vec<usize>> {
    let edge = |e: EdgeId| body.edges[e.index()].target.index();
    Some(match &body.terminators[block] {
        Terminator::Jump(e) => vec![edge(*e)],
        Terminator::Branch { yes, no, .. } => vec![edge(*yes), edge(*no)],
        _ => return None,
    })
}
fn local(body: &Body, mut address: AddressId) -> bool {
    loop {
        match body.addresses[address.index()].base {
            AddressBase::Slot(_) => return true,
            AddressBase::Address(parent) => address = parent,
            AddressBase::Value(_) => return false,
        }
    }
}
pub(super) fn pure(
    module: &Module,
    body: &Body,
    constants: &[Option<u64>],
    inst: &Inst,
    stores: bool,
) -> bool {
    match *inst {
        Inst::Step { .. } => true,
        Inst::Store { address, .. } => stores && local(body, address),
        Inst::Binary {
            op: BinaryOp::Shl | BinaryOp::Shr,
            left,
            right,
            ..
        } => failure::bounded_shift(body.values[left.index()], constants[right.index()]),
        Inst::Call { call } => {
            let Callee::Direct(f) = body.calls[call.index()].callee else {
                return false;
            };
            let function = &module.functions[f.index()];
            let signature = &module.signatures[function.signature.index()];
            if signature.dest.is_some()
                || signature
                    .params
                    .iter()
                    .any(|p| !matches!(p, Param::Value(Ty::Scalar(_))))
                || signature
                    .results
                    .iter()
                    .any(|t| !matches!(t, Ty::Scalar(_)))
            {
                return false;
            }
            let Some(callee) = &function.body else {
                return false;
            };
            let mut values = vec![None; callee.values.len()];
            for i in &callee.insts {
                if let Inst::Const { result, bits } = *i {
                    values[result.index()] = Some(bits);
                }
            }
            empty::loop_free(callee)
                && callee.terminators.iter().all(|t| {
                    matches!(
                        t,
                        Terminator::Jump(_) | Terminator::Branch { .. } | Terminator::Return(_)
                    )
                })
                && callee.insts.iter().all(|i| {
                    !matches!(i, Inst::Call { .. } | Inst::Step { .. })
                        && pure(module, callee, &values, i, true)
                })
        }
        Inst::Const { .. }
        | Inst::ReadStatic { .. }
        | Inst::FunctionRef { .. }
        | Inst::DescriptorRef { .. }
        | Inst::Unary { .. }
        | Inst::Binary { .. }
        | Inst::Compare { .. }
        | Inst::Convert { .. }
        | Inst::IntArith { .. }
        | Inst::ScalarUnary { .. }
        | Inst::MulAdd { .. }
        | Inst::Address { .. }
        | Inst::Read { .. }
        | Inst::StorageLive { .. }
        | Inst::StorageDead { .. }
        | Inst::Pointer { .. }
        | Inst::UpdateMask { .. } => !inst.can_fail(body),
        _ => false,
    }
}
fn path_cost(
    module: &Module,
    body: &Body,
    constants: &[Option<u64>],
    header: usize,
    block: usize,
    state: &mut BTreeMap<usize, Option<u64>>,
    known: &BTreeMap<usize, Loop>,
) -> Option<u64> {
    if block == header {
        return Some(0);
    }
    if let Some(child) = known.get(&block) {
        let (_, trips) = child.constant?;
        for &node in &child.nodes {
            state.insert(node, Some(0));
        }
        let after = body.edges[child.no.index()].target.index();
        let tail = path_cost(module, body, constants, header, after, state, known)?;
        return child.cost.checked_mul(trips)?.checked_add(tail);
    }
    if let Some(cost) = state.get(&block) {
        return *cost;
    }
    state.insert(block, None);
    let insts = body.blocks[block].insts.get(&body.insts)?;
    if !insts.iter().all(|i| pure(module, body, constants, i, true)) {
        return None;
    }
    let mut tails = next(body, block)?.into_iter();
    let first = path_cost(module, body, constants, header, tails.next()?, state, known)?;
    for successor in tails {
        if path_cost(module, body, constants, header, successor, state, known)? != first {
            return None;
        }
    }
    let cost = first.checked_add(
        insts
            .iter()
            .filter(|i| matches!(i, Inst::Step { .. }))
            .count() as u64,
    )?;
    state.insert(block, Some(cost));
    Some(cost)
}
pub(super) fn loops(module: &Module, body: &Body, constants: &[Option<u64>]) -> Vec<Loop> {
    let mut known = BTreeMap::new();
    loop {
        let before = known.len();
        for (header, row) in body.blocks.iter().enumerate() {
            if known.contains_key(&header) {
                continue;
            }
            let Some(
                [
                    Inst::Read {
                        result: index,
                        address: index_slot,
                        ..
                    },
                    Inst::Compare {
                        result: condition,
                        op: CompareOp::Lt,
                        left,
                        right: limit,
                    },
                ],
            ) = row.insts.get(&body.insts)
            else {
                continue;
            };
            if left != index
                || !matches!(
                    body.values[index.index()],
                    Ty::Scalar(Scalar::U8 | Scalar::U16 | Scalar::U32 | Scalar::U64)
                )
                || !local(body, *index_slot)
            {
                continue;
            }
            let Terminator::Branch {
                condition: tested,
                yes,
                no,
            } = body.terminators[header]
            else {
                continue;
            };
            if tested != *condition || body.edges[no.index()].args.len != 0 {
                continue;
            }
            let entry = body.edges[yes.index()].target.index();
            let mut state = BTreeMap::new();
            let Some(cost) = path_cost(module, body, constants, header, entry, &mut state, &known)
            else {
                continue;
            };
            if cost == 0 {
                continue;
            }
            let mut nodes = state.keys().copied().collect::<BTreeSet<_>>();
            nodes.insert(header);
            if nodes.contains(&body.edges[no.index()].target.index()) {
                continue;
            }
            // The one induction update is i+1, at every return to the head. No
            // source break, return, nested cycle, or nonuniform path was admitted.
            let mut updates = Vec::new();
            let mut valid = true;
            for &block in &nodes {
                for inst in body.blocks[block].insts.get(&body.insts).unwrap() {
                    if let Inst::Store { address, value } = *inst {
                        if address == *index_slot {
                            let increment = body.blocks[block].insts.get(&body.insts).unwrap().iter().any(|i|
                            matches!(*i, Inst::IntArith { result, flag: None, op: ArithOp::Add, mode: ArithMode::Wrapping, left, right }
                                if result == value && left == *index && constants[right.index()] == Some(1)));
                            if !increment {
                                valid = false;
                            }
                            updates.push(block);
                        }
                    }
                }
                if block != header {
                    for (other, _) in body.blocks.iter().enumerate() {
                        if !nodes.contains(&other)
                            && next(body, other).is_some_and(|n| n.contains(&block))
                        {
                            valid = false;
                        }
                    }
                }
            }
            if !valid || updates.len() != 1 {
                continue;
            }
            let update = updates[0];
            // Every back edge passes the induction update, including a source
            // continue. Reject a path that could repeat a trip without advancing.
            if nodes.iter().any(|&b| {
                b != header && next(body, b).is_some_and(|n| n.contains(&header)) && b != update
            }) {
                continue;
            }
            let mut initial = None;
            let mut fixed = true;
            for (b, row) in body.blocks.iter().enumerate() {
                if nodes.contains(&b) || !next(body, b).is_some_and(|n| n.contains(&header)) {
                    continue;
                }
                let stored = row
                    .insts
                    .get(&body.insts)
                    .unwrap()
                    .iter()
                    .rev()
                    .find_map(|i| {
                        if let Inst::Store { address, value } = *i {
                            if address == *index_slot {
                                return Some(constants[value.index()]);
                            }
                        }
                        None
                    })
                    .flatten();
                match (initial, stored) {
                    (_, None) => fixed = false,
                    (Some(a), Some(b)) if a != b => fixed = false,
                    (_, Some(n)) => initial = Some(n),
                }
            }
            let constant = if fixed {
                initial
                    .zip(constants[limit.index()])
                    .and_then(|(start, end)| Some((start, end.checked_sub(start)?)))
                    .filter(|(_, trips)| cost.checked_mul(*trips).is_some())
            } else {
                None
            };
            known.insert(
                header,
                Loop {
                    header,
                    nodes,
                    index: *index,
                    limit: *limit,
                    yes,
                    no,
                    cost,
                    index_slot: *index_slot,
                    constant,
                },
            );
        }
        if known.len() == before {
            break;
        }
    }
    let nested = known
        .values()
        .flat_map(|p| p.nodes.iter().copied().filter(|b| *b != p.header))
        .collect::<BTreeSet<_>>();
    known
        .into_values()
        .filter(|p| !nested.contains(&p.header))
        .collect()
}
impl FunctionEmitter<'_> {
    pub(super) fn step_counter(&mut self) -> Value {
        self.step_counter
            .clone()
            .unwrap_or_else(|| self.offset(&CONTEXT, memory_layout::CONTEXT_STEPS as u64))
    }
    pub(super) fn flush_steps(&mut self) {
        if let Some(counter) = self.step_counter.clone() {
            let value = self.load(&Type::I64, &counter, 8);
            let context = self.offset(&CONTEXT, memory_layout::CONTEXT_STEPS as u64);
            self.store(&Type::I64, &value, &context, 8);
        }
    }
    pub(super) fn reload_steps(&mut self) {
        if let Some(counter) = self.step_counter.clone() {
            let context = self.offset(&CONTEXT, memory_layout::CONTEXT_STEPS as u64);
            let value = self.load(&Type::I64, &context, 8);
            self.store(&Type::I64, &value, &counter, 8);
        }
    }
}
impl FunctionEmitter<'_> {
    pub(super) fn step_label(&self, block: usize) -> Label {
        self.step_labels
            .get(&block)
            .copied()
            .unwrap_or_else(|| Label::block(block))
    }
    fn step_word(&mut self, value: ValueId) -> Value {
        let v = self.numeric(value);
        let t = ty(self.body.values[value.index()]);
        if t == Type::I64 {
            v
        } else {
            self.zext(&t, &v, &Type::I64)
        }
    }
    pub(super) fn step_dispatch(&mut self, plan: &Loop, end: Local, number: usize) {
        let condition = match self.body.terminators[plan.header] {
            Terminator::Branch { condition, .. } => condition,
            _ => unreachable!(),
        };
        let value = self.numeric(condition);
        let exists = self.cmp(Predicate::Ne, &Type::I8, &value, &int(0));
        let n = self.counter();
        let reserve = Label::new("step_reserve", n);
        let out = self.edge_label(plan.no);
        self.cond(&exists, reserve, out);
        self.emit_edge(out, plan.no);
        self.block(reserve);
        let index = self.step_word(plan.index);
        let limit = self.step_word(plan.limit);
        let trips = self.bin(Binary::Sub, &Type::I64, &limit, &index);
        let counter = self.step_counter();
        let before = self.load(&Type::I64, &counter, 8);
        let quota = self.bin(Binary::UDiv, &Type::I64, &before, &int(plan.cost));
        let full = if let Some((start, count)) = plan.constant {
            let first = self.cmp(Predicate::Eq, &Type::I64, &index, &int(start));
            let enough = self.cmp(Predicate::Uge, &Type::I64, &quota, &int(count));
            self.bin(Binary::And, &Type::I1, &first, &enough)
        } else {
            self.cmp(Predicate::Uge, &Type::I64, &quota, &trips)
        };
        let whole = Label::new("step_whole", n);
        let partial = Label::new("step_partial", n);
        self.likely(&full, whole, partial);
        self.block(whole);
        if let Some((start, _)) = plan.constant {
            let slot = self.address(plan.index_slot);
            self.store(
                &ty(self.body.values[plan.index.index()]),
                &int(start),
                &slot,
                1,
            );
        }
        let charge = if let Some((_, count)) = plan.constant {
            int(count * plan.cost)
        } else {
            self.bin(Binary::Mul, &Type::I64, &trips, &int(plan.cost))
        };
        let after = self.bin(Binary::Sub, &Type::I64, &before, &charge);
        self.store(&Type::I64, &after, &counter, 8);
        self.br(version_label(
            number,
            self.body.blocks.len(),
            plan.header,
            true,
        ));
        self.block(partial);
        let any = self.cmp(Predicate::Ne, &Type::I64, &quota, &int(0));
        let chunk = Label::new("step_chunk", n);
        let checked = self.edge_label(plan.yes);
        self.cond(&any, chunk, checked);
        self.emit_edge(checked, plan.yes);
        self.block(chunk);
        let fits = self.cmp(Predicate::Ult, &Type::I64, &quota, &trips);
        let quota = self.select(&Type::I64, &fits, &quota, &trips);
        let stop = self.bin(Binary::Add, &Type::I64, &index, &quota);
        self.store(&Type::I64, &stop, &Value::Local(end), 8);
        let charge = self.bin(Binary::Mul, &Type::I64, &quota, &int(plan.cost));
        let after = self.bin(Binary::Sub, &Type::I64, &before, &charge);
        self.store(&Type::I64, &after, &counter, 8);
        self.br(version_label(
            number,
            self.body.blocks.len(),
            plan.header,
            false,
        ));
    }
    pub(super) fn step_versions(
        &mut self,
        plan: &Loop,
        end: Local,
        number: usize,
        location: impl Fn(Option<Anchor>) -> Option<usize>,
    ) -> Result<()> {
        for full in [true, false] {
            self.step_labels = plan
                .nodes
                .iter()
                .map(|&b| (b, version_label(number, self.body.blocks.len(), b, full)))
                .collect();
            self.unchecked_steps = true;
            let n = self.counter();
            let done = Label::new("step_chunk_done", n);
            for &block in &plan.nodes {
                self.block(self.step_label(block));
                let row = &self.body.blocks[block];
                for (offset, inst) in row.insts.get(&self.body.insts).unwrap().iter().enumerate() {
                    if !matches!(inst, Inst::Const { .. } | Inst::Step { .. }) {
                        self.debug_location =
                            location(self.body.inst_anchors[row.insts.start as usize + offset]);
                        self.inst(inst, row.insts.start as usize + offset)?;
                    }
                }
                self.anchor = row.anchor;
                self.debug_location = location(row.anchor);
                if block == plan.header {
                    let condition = if full {
                        let Terminator::Branch { condition, .. } = self.body.terminators[block]
                        else {
                            unreachable!()
                        };
                        let condition = self.numeric(condition);
                        self.cmp(Predicate::Ne, &Type::I8, &condition, &int(0))
                    } else {
                        let index = self.step_word(plan.index);
                        let limit = self.load(&Type::I64, &Value::Local(end), 8);
                        self.cmp(Predicate::Ult, &Type::I64, &index, &limit)
                    };
                    let yes = self.edge_label(plan.yes);
                    self.cond(&condition, yes, done);
                    self.emit_edge(yes, plan.yes);
                } else {
                    self.term(&self.body.terminators[block])?;
                }
            }
            self.step_labels.clear();
            self.unchecked_steps = false;
            self.block(done);
            self.step_settle_chunk();
            if full {
                let out = self.edge(plan.no);
                self.br(out);
            } else {
                self.br(Label::block(plan.header));
            }
        }
        Ok(())
    }
    fn step_settle_chunk(&mut self) {
        let counter = self.step_counter();
        let remaining = self.load(&Type::I64, &counter, 8);
        let positive = self.cmp(Predicate::Sgt, &Type::I64, &remaining, &int(0));
        let n = self.counter();
        let ready = Label::new("step_chunk_ready", n);
        let slice = Label::new("step_chunk_slice", n);
        self.likely(&positive, ready, slice);
        self.block(slice);
        let site = self.site_word();
        // A pre-charge only takes available complete trips. Zero is a taken
        // last Step, so this yields/refills and cannot raise OutOfTime.
        self.call_declared(
            "hirsel_core_step_out",
            Type::I8,
            &[ptr(CONTEXT), word(site)],
            FnAttrs::default(),
            &CallAttrs {
                cold: true,
                ..CallAttrs::default()
            },
        );
        self.br(ready);
        self.block(ready);
    }
}
fn version_label(number: usize, blocks: usize, block: usize, full: bool) -> Label {
    Label::new(
        if full { "step_full" } else { "step_fast" },
        number * blocks + block,
    )
}

pub(super) fn merged(
    module: &Module,
    body: &Body,
    constants: &[Option<u64>],
    instructions: PoolRange<Inst>,
) -> BTreeMap<usize, Vec<usize>> {
    let mut result = BTreeMap::new();
    let insts = instructions.get(&body.insts).unwrap();
    let mut start = 0;
    while start < insts.len() {
        let Inst::Step { fail } = insts[start] else {
            start += 1;
            continue;
        };
        let mut sites = vec![instructions.start as usize + start];
        let mut end = start + 1;
        while end < insts.len() {
            match insts[end] {
                Inst::Step { fail: next } if next == fail => {
                    sites.push(instructions.start as usize + end)
                }
                Inst::Step { .. } => break,
                ref inst if pure(module, body, constants, inst, false) => {}
                _ => break,
            }
            end += 1;
        }
        if sites.len() > 1 {
            result.insert(sites[0], sites);
        }
        start = end;
    }
    result
}
impl FunctionEmitter<'_> {
    pub(super) fn step_group(&mut self, sites: &[usize]) {
        let counter = self.step_counter();
        let before = self.load(&Type::I64, &counter, 8);
        let enough = self.cmp(
            Predicate::Sge,
            &Type::I64,
            &before,
            &int(sites.len() as u64),
        );
        let n = self.counter();
        let fast = Label::new("step_group_fast", n);
        let checked = Label::new("step_group_checked", n);
        let ready = Label::new("step_group_ready", n);
        self.likely(&enough, fast, checked);
        self.block(fast);
        let after = self.bin(Binary::Sub, &Type::I64, &before, &int(sites.len() as u64));
        self.store(&Type::I64, &after, &counter, 8);
        self.anchor = self.body.inst_anchors[*sites.last().unwrap()];
        self.step_settle_chunk();
        self.br(ready);
        self.block(checked);
        for &site in sites {
            let Inst::Step { fail } = self.body.insts[site] else {
                unreachable!()
            };
            self.anchor = self.body.inst_anchors[site];
            self.step(fail);
        }
        self.br(ready);
        self.block(ready);
    }
}
