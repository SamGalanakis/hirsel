use super::*;
use cranelift_codegen::ir::{
    BlockArg, MemFlagsData as MemFlags, StackSlotData, StackSlotKind, Value,
};
enum AddressValue {
    Direct(Value),
    // Address IDs are evaluated snapshots, unlike immutable CoreIR values.
    // A repeatedly assigned ID retains its latest snapshot across control flow.
    Snapshot(ir::StackSlot),
}

pub(super) struct Function<'a, 'b, 'c> {
    pub b: FunctionBuilder<'a>,
    pub object: &'b mut ObjectModule,
    pub body: &'c Body,
    pub input: &'c c::Module,
    pub layouts: &'c [hirsel_core_layout::Recipe],
    pub values: Vec<Value>,
    addresses: Vec<AddressValue>,
    pub blocks: Vec<ir::Block>,
    pub slots: Vec<ir::StackSlot>,
    pub functions: &'c [cranelift_module::FuncId],
    pub statics: &'c [cranelift_module::DataId],
    pub signatures: &'c [abi::Signature],
    pub glues: &'c drop::Glues,
    pub task_entries: &'c task_entry::Glues,
    pub context: Value,
    pub allocators: &'c [fast::Allocator],
    pub signature: &'c abi::Signature,
    pub result_storage: Option<Value>,
    pub raise_blocks: Vec<(u8, Option<BlockId>, ir::Block)>,
    pub propagation: Option<ir::Block>,
    pub cause_slot: Option<ir::StackSlot>,
    pub calls: calls::Calls<'c>,
    pub testing: bool,
    pub empty_bound: bool,
}

pub(super) fn emit(
    object: &mut ObjectModule,
    mut b: FunctionBuilder<'_>,
    body: &Body,
    signature: &abi::Signature,
    tables: &super::Tables<'_>,
) -> Result<()> {
    let (input, layouts, signatures, functions, statics) = (
        tables.input,
        tables.layouts,
        tables.signatures,
        tables.functions,
        tables.statics,
    );
    let blocks: Vec<_> = body.blocks.iter().map(|_| b.create_block()).collect();
    b.append_block_params_for_function_params(blocks[0]);
    for (i, block) in body.blocks.iter().enumerate().skip(1) {
        for v in block.params.get(&body.pools.values).unwrap() {
            b.append_block_param(blocks[i], ty(body.values[v.index()]));
        }
    }
    let mut values = vec![Value::from_u32(0); body.values.len()];
    for (i, block) in body.blocks.iter().enumerate() {
        let params = b.block_params(blocks[i]);
        let params = if i == 0 {
            &params[usize::from(signature.indirect) + 1..]
        } else {
            params
        };
        for (&id, &value) in block
            .params
            .get(&body.pools.values)
            .unwrap()
            .iter()
            .zip(params)
        {
            values[id.index()] = value;
        }
    }
    let mut definitions = vec![0u32; body.addresses.len()];
    for inst in &body.insts {
        if let Inst::Address { address, .. } = inst {
            definitions[address.index()] += 1;
        }
    }
    let addresses = definitions
        .into_iter()
        .map(|n| {
            if n > 1 {
                AddressValue::Snapshot(b.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    8,
                    3,
                )))
            } else {
                AddressValue::Direct(Value::from_u32(0))
            }
        })
        .collect();
    let mut slots = Vec::new();
    for &s in &body.slots {
        let (size, align) = super::place::storage(input, layouts, s)?;
        slots.push(b.create_sized_stack_slot(
            StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                u32::try_from(size.max(1)).map_err(|_| {
                    error("native stack slot exceeds Cranelift's u32 representation")
                })?,
                align.trailing_zeros() as u8,
            ),
        ));
    }
    b.switch_to_block(blocks[0]);
    let params = b.block_params(blocks[0]);
    let context = params[usize::from(signature.indirect)];
    let result_storage = signature.indirect.then(|| params[0]);
    let mut f = Function {
        b,
        object,
        body,
        input,
        layouts,
        values,
        addresses,
        blocks,
        slots,
        functions,
        statics,
        signatures,
        context,
        allocators: tables.allocators,
        glues: tables.glues,
        task_entries: tables.task_entries,
        signature,
        result_storage,
        raise_blocks: Vec::with_capacity(4),
        propagation: None,
        cause_slot: None,
        testing: tables.trace_drops,
        empty_bound: fast::empty_bound(body),
        calls: calls::Calls::new(
            &tables.runtimes,
            signatures,
            body.edges
                .iter()
                .map(|e| e.args.len as usize)
                .max()
                .unwrap_or(0),
        ),
    };
    for i in order(body) {
        let block = &body.blocks[i];
        f.b.switch_to_block(f.blocks[i]);
        for inst in block.insts.get(&body.insts).unwrap() {
            f.instruction(inst)?;
        }
        match &body.terminators[i] {
            Terminator::Return(range) => {
                let mut results = std::mem::take(&mut f.calls.results);
                results.clear();
                results.extend(
                    range
                        .get(&body.pools.values)
                        .unwrap()
                        .iter()
                        .map(|&v| f.get(v)),
                );
                results.push(f.b.ins().iconst(types::I8, 0));
                signature.return_(&mut f.b, &results, result_storage);
                f.calls.results = results;
            }
            Terminator::Jump(e) => {
                let mut args = std::mem::take(&mut f.calls.edges[0]);
                let target = f.edge(*e, &mut args);
                f.b.ins().jump(target, &args);
                f.calls.edges[0] = args;
            }
            Terminator::Branch { condition, yes, no } => {
                let cond = f.get(*condition);
                let mut ya = std::mem::take(&mut f.calls.edges[0]);
                let mut na = std::mem::take(&mut f.calls.edges[1]);
                let yes = f.edge(*yes, &mut ya);
                let no = f.edge(*no, &mut na);
                f.b.ins().brif(cond, yes, &ya, no, &na);
                f.calls.edges = [ya, na];
            }
            Terminator::Switch {
                value,
                arms,
                otherwise,
            } => {
                let value = f.get(*value);
                let mut args = std::mem::take(&mut f.calls.edges[0]);
                for arm in arms.get(&body.pools.arms).unwrap() {
                    let test =
                        f.b.ins()
                            .icmp_imm_s(ir::condcodes::IntCC::Equal, value, arm.value as i64);
                    let next = f.b.create_block();
                    let target = f.edge(arm.edge, &mut args);
                    f.b.ins().brif(test, target, &args, next, &[]);
                    f.b.switch_to_block(next);
                }
                let target = f.edge(*otherwise, &mut args);
                f.b.ins().jump(target, &args);
                f.calls.edges[0] = args;
            }
            Terminator::Unreachable => {
                f.b.ins().trap(ir::TrapCode::unwrap_user(1));
            }
            Terminator::Raise { cause, cleanup } => {
                match cause {
                    RaiseCause::Unwrap => f.record_failure(3)?,
                    RaiseCause::Bounds => f.record_failure(1)?,
                    RaiseCause::Fail(address) => f.record_string(*address)?,
                }
                f.fail_to(*cleanup);
            }
            Terminator::Propagate => f.fail_to(None),
        }
    }
    f.emit_failure_blocks()?;
    f.b.seal_all_blocks();
    f.b.finalize(f.object.target_config());
    Ok(())
}

impl Function<'_, '_, '_> {
    pub fn get(&self, v: ValueId) -> Value {
        self.values[v.index()]
    }
    pub fn set(&mut self, v: ValueId, value: Value) {
        let value = if self.body.values[v.index()].is_float() {
            self.canonical(value)
        } else {
            value
        };
        self.values[v.index()] = value;
    }
    pub fn addr(&mut self, a: AddressId) -> Value {
        match self.addresses[a.index()] {
            AddressValue::Direct(value) => value,
            AddressValue::Snapshot(slot) => {
                self.b.ins().stack_load(types::I64, types::I64, slot, 0)
            }
        }
    }
    fn edge(&self, e: EdgeId, args: &mut Vec<BlockArg>) -> ir::Block {
        let edge = &self.body.edges[e.index()];
        args.clear();
        args.extend(
            edge.args
                .get(&self.body.pools.values)
                .unwrap()
                .iter()
                .map(|&v| BlockArg::from(self.get(v))),
        );
        self.blocks[edge.target.index()]
    }
    fn instruction(&mut self, inst: &Inst) -> Result<()> {
        match *inst {
            Inst::Const { result, bits } => {
                let t = ty(self.body.values[result.index()]);
                let v = match t {
                    types::F32 => self
                        .b
                        .ins()
                        .f32const(ir::immediates::Ieee32::with_bits(bits as u32)),
                    types::F64 => self
                        .b
                        .ins()
                        .f64const(ir::immediates::Ieee64::with_bits(bits)),
                    _ => self.b.ins().iconst(t, bits as i64),
                };
                self.set(result, v);
            }
            Inst::Pointer { result, address } => {
                let pointer = self.addr(address);
                self.set(result, pointer);
            }
            Inst::ReadStatic { result, data } => {
                let global = self
                    .object
                    .declare_data_in_func(self.statics[data.index()], self.b.func);
                let v = self.b.ins().symbol_value(types::I64, global);
                self.set(result, v);
            }
            Inst::FunctionRef { result, function } => {
                let func = self.calls.reference(
                    self.object,
                    &mut self.b,
                    self.functions[function.index()],
                );
                let v = self.b.ins().func_addr(types::I64, func);
                self.set(result, v);
            }
            Inst::Unary {
                result,
                op,
                operand,
                fail,
            } => {
                let v = self.get(operand);
                let t = self.body.values[operand.index()];
                let v = match op {
                    UnaryOp::Neg if t.is_float() => self.b.ins().fneg(v),
                    UnaryOp::Neg => {
                        let zero = self.b.ins().iconst(ty(t), 0);
                        let (value, overflow) = self.b.ins().ssub_overflow(zero, v);
                        self.guard(overflow, 0, fail)?;
                        value
                    }
                    UnaryOp::Not => self.b.ins().icmp_imm_s(ir::condcodes::IntCC::Equal, v, 0),
                    UnaryOp::BitNot => self.b.ins().bnot(v),
                };
                self.set(result, v);
            }
            Inst::Binary {
                result,
                op,
                left,
                right,
                fail,
            } => {
                let t = self.body.values[left.index()];
                let l = self.get(left);
                let r = self.get(right);
                let v = if op.can_fail(t) {
                    self.checked_binary(op, t, l, r, fail)?
                } else {
                    self.binary(op, t, l, r)?
                };
                self.set(result, v);
            }
            Inst::Compare {
                result,
                op,
                left,
                right,
            } => {
                let t = self.body.values[left.index()];
                let l = self.get(left);
                let r = self.get(right);
                let v = self.compare(op, t, l, r);
                self.set(result, v);
            }
            Inst::Convert {
                result,
                flag,
                operand,
                mode,
            } => {
                let from = self.body.values[operand.index()];
                let to = self.body.values[result.index()];
                let v = self.get(operand);
                let (v, f) = self.convert(mode, from, to, v);
                self.set(result, v);
                if let Some(flag) = flag {
                    self.set(flag, f.unwrap());
                }
            }
            Inst::IntArith {
                result,
                flag,
                op,
                mode,
                left,
                right,
            } => {
                let t = self.body.values[left.index()];
                let l = self.get(left);
                let r = self.get(right);
                let (v, f) = self.arith(op, mode, t, l, r);
                self.set(result, v);
                if let Some(flag) = flag {
                    self.set(flag, f);
                }
            }
            Inst::MulAdd {
                result,
                left,
                right,
                addend,
            } => {
                let l = self.get(left);
                let r = self.get(right);
                let a = self.get(addend);
                let v = self.b.ins().fma(l, r, a);
                self.set(result, v);
            }
            Inst::ScalarUnary {
                result,
                op,
                operand,
            } => {
                let v = self.get(operand);
                let v = self.scalar_unary(op, v)?;
                self.set(result, v);
            }
            Inst::StorageLive { .. } | Inst::StorageDead { .. } => {}
            Inst::Address { address, fail } => {
                let v = self.address(address, fail)?;
                match self.addresses[address.index()] {
                    AddressValue::Direct(_) => {
                        self.addresses[address.index()] = AddressValue::Direct(v)
                    }
                    AddressValue::Snapshot(slot) => {
                        self.b.ins().stack_store(types::I64, v, slot, 0);
                    }
                }
            }
            Inst::Read {
                result, address, ..
            } => {
                let a = self.addr(address);
                let v =
                    self.b
                        .ins()
                        .load(ty(self.body.values[result.index()]), MemFlags::new(), a, 0);
                self.set(result, v);
            }
            Inst::Store { address, value } => {
                let a = self.addr(address);
                let v = self.get(value);
                self.b.ins().store(MemFlags::new(), v, a, 0);
            }
            Inst::UpdateMask {
                result,
                mask,
                field,
                initialized,
            } => {
                let v = self.get(mask);
                let bit = 1u64 << field;
                let v = if initialized {
                    self.b.ins().bor_imm_s(v, bit as i64)
                } else {
                    self.b.ins().band_imm_s(v, !bit as i64)
                };
                self.set(result, v);
            }
            Inst::Call { call } => {
                let call = &self.body.calls[call.index()];
                if let Callee::Runtime(op) = call.callee {
                    return self.runtime_call(op, call);
                }
                let signature = &self.signatures[call.signature.index()];
                let storage = signature
                    .indirect
                    .then(|| self.calls.storage(&mut self.b, signature.size));
                let mut args = std::mem::take(&mut self.calls.args);
                args.clear();
                if let Some(storage) = storage {
                    args.push(storage);
                }
                args.push(self.context);
                for arg in call.args.get(&self.body.pools.args).unwrap() {
                    args.push(match *arg {
                        Arg::Value(v) => self.get(v),
                        Arg::Address(a) => self.addr(a),
                    });
                }
                let inst = match call.callee {
                    Callee::Direct(f) => {
                        let f = self.calls.reference(
                            self.object,
                            &mut self.b,
                            self.functions[f.index()],
                        );
                        self.b.ins().call(f, &args)
                    }
                    Callee::Indirect(v) => {
                        let v = self.get(v);
                        let sig = self.calls.indirect(
                            &mut self.b,
                            call.signature.index(),
                            &signature.clif,
                        );
                        self.b.ins().call_indirect(sig, v, &args)
                    }
                    Callee::Runtime(_) => unreachable!(),
                };
                self.calls.args = args;
                let mut returned = [Value::from_u32(0); 2];
                let len = self.b.inst_results(inst).len();
                returned[..len].copy_from_slice(self.b.inst_results(inst));
                let mut results = std::mem::take(&mut self.calls.results);
                results.clear();
                signature.unpack(&mut self.b, &returned[..len], storage, |value| {
                    results.push(value)
                });
                let status = results.pop().unwrap();
                let infallible =
                    match call.callee {
                        Callee::Direct(f) => self.input.functions[f.index()]
                            .body
                            .as_ref()
                            .is_some_and(|body| {
                                body.insts.is_empty()
                                    && body
                                        .terminators
                                        .iter()
                                        .all(|t| matches!(t, Terminator::Return(_)))
                            }),
                        _ => false,
                    };
                if !infallible {
                    self.call_failure(status, call.fail)?;
                }
                for (&v, &result) in call
                    .results
                    .get(&self.body.pools.values)
                    .unwrap()
                    .iter()
                    .zip(&results)
                {
                    self.set(v, result);
                }
                self.calls.results = results;
            }
            _ => self.memory(inst)?,
        }
        Ok(())
    }
}

// A producer's block numbers need not be topological. Reverse postorder makes
// dominating definitions available before their uses; loop parameters already
// have their Cranelift values before any instruction is lowered.
fn order(body: &Body) -> Vec<usize> {
    let mut seen = vec![false; body.blocks.len()];
    let mut pending = vec![(0usize, false)];
    let mut result = Vec::with_capacity(body.blocks.len());
    while let Some((b, exit)) = pending.pop() {
        if exit {
            result.push(b);
            continue;
        }
        if std::mem::replace(&mut seen[b], true) {
            continue;
        }
        pending.push((b, true));
        let mut target = |block: BlockId| pending.push((block.index(), false));
        let mut edge = |e: EdgeId| target(body.edges[e.index()].target);
        match &body.terminators[b] {
            Terminator::Jump(e) => edge(*e),
            Terminator::Branch { yes, no, .. } => {
                edge(*no);
                edge(*yes);
            }
            Terminator::Switch {
                arms, otherwise, ..
            } => {
                edge(*otherwise);
                for arm in arms.get(&body.pools.arms).unwrap() {
                    edge(arm.edge);
                }
            }
            Terminator::Raise {
                cleanup: Some(block),
                ..
            } => target(*block),
            _ => {}
        }
        for inst in body.blocks[b].insts.get(&body.insts).unwrap() {
            if let Some(block) = inst.fail(body) {
                target(block);
            }
        }
    }
    result.reverse();
    result
}
