use super::*;
use cranelift_codegen::ir::{
    BlockArg, MemFlagsData as MemFlags, StackSlotData, StackSlotKind, Value,
};
#[derive(Clone, Copy)]
pub(super) struct Address {
    pub base: Value,
    pub offset: i32,
}
impl From<Value> for Address {
    fn from(base: Value) -> Self {
        Self { base, offset: 0 }
    }
}
enum AddressValue {
    Direct(Address),
    // Address IDs are evaluated snapshots, unlike immutable CoreIR values.
    // A repeatedly assigned ID retains its latest snapshot across control flow.
    Snapshot(ir::StackSlot),
}

pub(super) struct PendingRaise {
    pub kind: u8,
    pub target: Option<BlockId>,
    pub anchor: Option<Anchor>,
    pub srcloc: ir::SourceLoc,
    pub block: ir::Block,
}

pub(super) struct Function<'a, 'b, 'c> {
    pub b: FunctionBuilder<'a>,
    pub object: &'b mut ObjectModule,
    pub body: &'c Body,
    pub input: &'c c::Module,
    pub instance: &'c InstanceKey,
    pub layouts: &'c [hirsel_core_layout::Recipe],
    pub values: Vec<Value>,
    /// The upper 16 bytes of each 32-byte vector value.
    pub high: Vec<Value>,
    addresses: Vec<AddressValue>,
    pub blocks: Vec<ir::Block>,
    pub slots: Vec<ir::StackSlot>,
    pub functions: &'c [cranelift_module::FuncId],
    pub statics: &'c [cranelift_module::DataId],
    pub descriptors: &'c [Option<cranelift_module::DataId>],
    pub signatures: &'c [abi::Signature],
    pub glues: &'c drop::Glues,
    pub task_entries: &'c task_entry::Glues,
    pub context: Value,
    pub caller_site: Value,
    pub anchor: Option<Anchor>,
    pub srcloc: ir::SourceLoc,
    pub site_symbols:
        &'c std::cell::RefCell<std::collections::HashMap<ItemId, cranelift_module::DataId>>,
    pub site_constants: &'c std::cell::RefCell<sites::Constants>,
    pub debug: bool,
    pub signature: &'c abi::Signature,
    pub result_storage: Option<Value>,
    record_dest: Option<Value>,
    /// By value: the constant an `Inst::Const` defines.
    pub constants: Vec<Option<u64>>,
    pub raise_blocks: Vec<PendingRaise>,
    pub raise_lookup: std::collections::HashMap<(u8, Option<BlockId>, Option<Anchor>), ir::Block>,
    pub site_reference: Option<ir::GlobalValue>,
    pub propagation: Option<ir::Block>,
    pub cause_slot: Option<ir::StackSlot>,
    pub calls: calls::Calls<'c>,
    pub testing: bool,
    pub empty_bound: bool,
    pub observe_charge: bool,
    receive_loops: c::receive_loops::ReceiveLoops,
}

pub(super) fn emit(
    object: &mut ObjectModule,
    mut b: FunctionBuilder<'_>,
    function: &c::Function,
    signature: &abi::Signature,
    tables: &super::Tables<'_>,
) -> Result<()> {
    let body = function.body.as_ref().unwrap();
    let (input, layouts, signatures, functions, statics) = (
        tables.input,
        tables.layouts,
        tables.signatures,
        tables.functions,
        tables.statics,
    );
    let blocks: Vec<_> = body.blocks.iter().map(|_| b.create_block()).collect();
    for (i, cold) in cold_blocks(body).into_iter().enumerate() {
        if cold {
            b.set_cold_block(blocks[i]);
        }
    }
    b.append_block_params_for_function_params(blocks[0]);
    for (i, block) in body.blocks.iter().enumerate().skip(1) {
        for v in block.params.get(&body.pools.values).unwrap() {
            let t = body.values[v.index()];
            b.append_block_param(blocks[i], ty(t));
            if simd::two_halves(t) {
                b.append_block_param(blocks[i], ty(t));
            }
        }
    }
    let missing = Value::from_u32(0);
    let mut values = vec![missing; body.values.len()];
    let mut high = if function.versioned {
        vec![missing; body.values.len()]
    } else {
        Vec::new()
    };
    for (i, block) in body.blocks.iter().enumerate().skip(1) {
        let mut physical = b.block_params(blocks[i]).iter().copied();
        for &id in block.params.get(&body.pools.values).unwrap() {
            values[id.index()] = physical.next().unwrap();
            if simd::two_halves(body.values[id.index()]) {
                high[id.index()] = physical.next().unwrap();
            }
        }
    }
    let fills = super::loops::byte_fills(body);
    let mut definitions = vec![0u32; body.addresses.len()];
    let mut defined_in = vec![usize::MAX; body.addresses.len()];
    for (block, row) in body.blocks.iter().enumerate() {
        for inst in row.insts.get(&body.insts).unwrap() {
            if let Inst::Address { address, .. } = *inst {
                definitions[address.index()] += 1;
                defined_in[address.index()] = block;
            }
        }
    }
    for (block, row) in body.blocks.iter().enumerate() {
        let mut used = |address: AddressId| {
            // Entry definitions dominate every use. A later evaluated address
            // used outside its block can be a loop's last snapshot; it is not
            // necessarily a dominating SSA definition, even with one emitter.
            let definition = defined_in[address.index()];
            if definition != 0 && definition != block {
                definitions[address.index()] = 2;
            }
        };
        for inst in row.insts.get(&body.insts).unwrap() {
            match *inst {
                Inst::Address { address, .. } => {
                    if let AddressBase::Address(parent) = body.addresses[address.index()].base {
                        used(parent);
                    }
                }
                Inst::Read { address, .. }
                | Inst::Store { address, .. }
                | Inst::PutLanes { to: address, .. }
                | Inst::Pointer { address, .. }
                | Inst::Case { address, .. }
                | Inst::SetCase { address, .. }
                | Inst::Drop { address, .. }
                | Inst::Grow {
                    carrier: address, ..
                } => used(address),
                Inst::Copy { to, from, .. } => {
                    used(to);
                    used(from);
                }
                Inst::Call { call } => {
                    for arg in body.calls[call.index()].args.get(&body.pools.args).unwrap() {
                        if let Arg::Address(address) = *arg {
                            used(address);
                        }
                    }
                }
                _ => {}
            }
        }
        if let Terminator::Raise {
            cause: RaiseCause::Fail(address),
            ..
        } = body.terminators[block]
        {
            used(address);
        }
    }
    // A fused loop publishes its last evaluated address, which may be read
    // after a guarded exit even though the body does not dominate that read.
    for fill in fills.iter().flatten() {
        definitions[fill.store.index()] = 2;
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
                AddressValue::Direct(Value::from_u32(0).into())
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
    let params = b.block_params(blocks[0]).to_vec();
    let context = params[usize::from(signature.indirect)];
    let result_storage = signature.indirect.then(|| params[0]);
    let caller_site = *params.last().unwrap();
    let record_dest = signature
        .record
        .map(|record| abi::record_place(&mut b, record));
    let mut physical = params[usize::from(signature.indirect) + 1..]
        .iter()
        .copied();
    let mut logical = body.blocks[0]
        .params
        .get(&body.pools.values)
        .unwrap()
        .iter();
    if signature.dest || signature.record.is_some() {
        values[logical.next().unwrap().index()] =
            record_dest.unwrap_or_else(|| physical.next().unwrap());
    }
    for (&id, record) in logical.zip(&signature.param_records) {
        values[id.index()] = if let Some(record) = *record {
            let mut incoming = [Value::from_u32(0); 2];
            for value in &mut incoming[..record.fields().len()] {
                *value = physical.next().unwrap();
            }
            let place = abi::record_place(&mut b, record);
            abi::store_record(&mut b, record, place, &incoming);
            place
        } else {
            let low = physical.next().unwrap();
            if simd::two_halves(body.values[id.index()]) {
                high[id.index()] = physical.next().unwrap();
            }
            low
        };
    }
    let mut f = Function {
        b,
        object,
        body,
        input,
        instance: &input.instances[function.instance.index()],
        layouts,
        values,
        high,
        addresses,
        blocks,
        slots,
        functions,
        statics,
        descriptors: tables.descriptors,
        signatures,
        context,
        caller_site,
        anchor: None,
        srcloc: ir::SourceLoc::default(),
        site_symbols: &tables.site_symbols,
        site_constants: &tables.site_constants,
        debug: tables.debug,
        glues: tables.glues,
        task_entries: tables.task_entries,
        signature,
        result_storage,
        record_dest,
        constants: c::failure::constants(body),
        raise_blocks: Vec::with_capacity(4),
        raise_lookup: Default::default(),
        site_reference: None,
        propagation: None,
        cause_slot: None,
        testing: tables.trace_drops,
        empty_bound: !tables.observe_charge && fast::empty_bound(body),
        observe_charge: tables.observe_charge,
        receive_loops: c::receive_loops::ReceiveLoops::new(body),
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
        if i != 0 {
            f.b.switch_to_block(f.blocks[i]);
        }
        if f.receive_loops.exit(i) {
            f.c_call("hirsel_core_receive_loop_end", &[], &[], &[])?;
        }
        let fill = fills.get(i).copied().flatten();
        let original_index =
            if let Some(fill) = fill {
                let store = block.insts.get(&body.insts).unwrap().iter().position(|inst|
                matches!(inst, Inst::Store { address, .. } if *address == fill.store)).unwrap();
                let index = block.insts.start as usize + store;
                f.at_anchor(
                    body.inst_anchors.get(index).copied().flatten(),
                    index as u32 + 1,
                );
                let index = f.get(fill.index);
                let bound = f.get(fill.bound);
                let base = f.get(fill.base);
                let to = f.b.ins().iadd(base, index);
                let bytes = f.b.ins().isub(bound, index);
                let byte = f.get(fill.byte);
                let byte = f.resize(byte, types::I32, false);
                f.c_call(
                    "memset",
                    &[Ty::Ptr, Ty::Scalar(Scalar::U32), Ty::Scalar(Scalar::U64)],
                    &[Ty::Ptr],
                    &[to, byte, bytes],
                )?;
                // This block is entered only for index < bound. Preserve its
                // evaluated address snapshot and scalar definitions as the last
                // iteration; its existing backedge then carries bound to the test.
                let last = f.b.ins().iadd_imm_s(bound, -1);
                f.set(fill.index, last);
                Some(index)
            } else {
                None
            };
        for (offset, inst) in block.insts.get(&body.insts).unwrap().iter().enumerate() {
            let index = block.insts.start as usize + offset;
            f.at_anchor(
                body.inst_anchors.get(index).copied().flatten(),
                index as u32 + 1,
            );
            if !matches!((fill, inst), (Some(fill), Inst::Store { address, .. }) if fill.store == *address)
            {
                f.instruction(inst)?;
            }
        }
        if let (Some(fill), Some(index)) = (fill, original_index) {
            f.set(fill.index, index);
        }
        f.at_anchor(block.anchor, body.insts.len() as u32 + i as u32 + 1);
        match &body.terminators[i] {
            Terminator::Return(range) => {
                let mut results = std::mem::take(&mut f.calls.results);
                results.clear();
                results.extend(
                    range
                        .get(&body.pools.values)
                        .unwrap()
                        .iter()
                        .flat_map(|&v| f.parts(v)),
                );
                if let Some(record) = signature.record {
                    abi::load_record(&mut f.b, record, f.record_dest.unwrap(), &mut results);
                }
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
                    RaiseCause::Capacity {
                        requested,
                        available,
                    } => {
                        let (requested, available) = (f.get(*requested), f.get(*available));
                        f.record_capacity(requested, available)?
                    }
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
    fn parts(&self, v: ValueId) -> impl Iterator<Item = Value> {
        let high = simd::two_halves(self.body.values[v.index()]).then(|| self.high[v.index()]);
        std::iter::once(self.get(v)).chain(high)
    }
    pub fn set(&mut self, v: ValueId, value: Value) {
        self.values[v.index()] = value;
    }
    pub fn addr(&mut self, a: AddressId) -> Value {
        let address = self.addr_parts(a);
        self.materialize(address)
    }
    pub fn addr_parts(&mut self, a: AddressId) -> Address {
        match self.addresses[a.index()] {
            AddressValue::Direct(value) => value,
            AddressValue::Snapshot(slot) => self
                .b
                .ins()
                .stack_load(types::I64, types::I64, slot, 0)
                .into(),
        }
    }
    pub fn materialize(&mut self, address: Address) -> Value {
        if address.offset == 0 {
            address.base
        } else {
            self.b
                .ins()
                .iadd_imm_s(address.base, i64::from(address.offset))
        }
    }
    pub fn offset_address(&mut self, address: Address, offset: u64) -> Address {
        let offset = i64::from(address.offset).wrapping_add(offset as i64);
        if let Ok(offset) = i32::try_from(offset) {
            Address {
                base: address.base,
                offset,
            }
        } else {
            self.b.ins().iadd_imm_s(address.base, offset).into()
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
                .flat_map(|&v| self.parts(v).map(BlockArg::from)),
        );
        self.blocks[edge.target.index()]
    }
    fn instruction(&mut self, inst: &Inst) -> Result<()> {
        match *inst {
            Inst::Simd {
                result,
                op,
                first,
                second,
                fail,
            } => {
                self.simd(result, op, first, second, fail)?;
            }
            Inst::PutLanes { to, vector, count } => self.put_lanes(to, vector, count)?,
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
            Inst::LayoutConst { .. } => unreachable!("engine-private operands are resolved"),
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
            Inst::DescriptorRef {
                result, function, ..
            } => {
                let data = self.descriptors[function.index()]
                    .expect("every described function of the unit has a descriptor");
                let global = self.object.declare_data_in_func(data, self.b.func);
                let v = self.b.ins().symbol_value(types::I64, global);
                self.set(result, v);
            }
            Inst::CompactWidth { result, word, .. } => {
                let v = self.get(word);
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
                    let safe = c::failure::constant_safe(op, t, self.constants[right.index()]);
                    self.checked_binary(op, t, l, r, safe, fail)?
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
            // FORM v1c: a mark is the evaluator's alone; no code.
            Inst::StorageLive { .. }
            | Inst::StorageDead { .. }
            | Inst::BuiltinUse { .. }
            | Inst::Mark { .. }
            | Inst::Unmark { .. } => {}
            Inst::Address { address, fail } => {
                let v = self.address(address, fail)?;
                match self.addresses[address.index()] {
                    AddressValue::Direct(_) => {
                        self.addresses[address.index()] = AddressValue::Direct(v)
                    }
                    AddressValue::Snapshot(slot) => {
                        let v = self.materialize(v);
                        self.b.ins().stack_store(types::I64, v, slot, 0);
                    }
                }
            }
            Inst::Read {
                result, address, ..
            } => {
                let a = self.addr_parts(address);
                let result_ty = self.body.values[result.index()];
                if matches!(result_ty, Ty::Vector(_)) {
                    self.load_vector(result, a);
                    return Ok(());
                }
                let v = self
                    .b
                    .ins()
                    .load(ty(result_ty), MemFlags::new(), a.base, a.offset);
                self.set(result, v);
            }
            Inst::Store { address, value } => {
                let a = self.addr_parts(address);
                if matches!(self.body.values[value.index()], Ty::Vector(_)) {
                    self.store_vector(value, a);
                    return Ok(());
                }
                let v = self.get(value);
                self.b.ins().store(MemFlags::new(), v, a.base, a.offset);
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
                let fused_receive = self.receive_loops.call(call);
                let call = &self.body.calls[call.index()];
                if let Callee::Runtime(op) = call.callee {
                    return self.runtime_call(op, call, fused_receive);
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
                let actual = call.args.get(&self.body.pools.args).unwrap();
                let dest = if signature.dest || signature.record.is_some() {
                    let Arg::Address(dest) = actual[0] else {
                        unreachable!()
                    };
                    Some(self.addr(dest))
                } else {
                    None
                };
                if signature.dest {
                    args.push(dest.unwrap());
                }
                for (&arg, record) in actual[usize::from(dest.is_some())..]
                    .iter()
                    .zip(&signature.param_records)
                {
                    match arg {
                        Arg::Value(v) => args.extend(self.parts(v)),
                        Arg::Address(a) => {
                            let address = self.addr(a);
                            if let Some(record) = *record {
                                abi::load_record(&mut self.b, record, address, &mut args);
                            } else {
                                args.push(address);
                            }
                        }
                    }
                }
                let site = match call.callee {
                    Callee::Direct(f) if !self.input.passes_caller_site(f) => self.context,
                    _ => self.site()?,
                };
                args.push(site);
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
                let mut returned = [Value::from_u32(0); 3];
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
                if let Some(record) = signature.record {
                    abi::store_record(&mut self.b, record, dest.unwrap(), &results);
                }
                let mut physical = results.iter().copied();
                for &v in call.results.get(&self.body.pools.values).unwrap() {
                    self.set(v, physical.next().unwrap());
                    if simd::two_halves(self.body.values[v.index()]) {
                        self.high[v.index()] = physical.next().unwrap();
                    }
                }
                self.calls.results = results;
            }
            _ => self.memory(inst)?,
        }
        Ok(())
    }
}

/// Blocks entered only through edges the front marked cold (core-ir Edge::cold)
/// or from blocks that are cold themselves; Cranelift lays them out last.
fn cold_blocks(body: &Body) -> Vec<bool> {
    let n = body.blocks.len();
    // Each block's predecessors, with whether that entry is a cold edge.
    let mut incoming: Vec<Vec<(usize, bool)>> = vec![Vec::new(); n];
    for b in 0..n {
        let mut edges = Vec::new();
        match &body.terminators[b] {
            Terminator::Jump(e) => edges.push(*e),
            Terminator::Branch { yes, no, .. } => edges.extend([*yes, *no]),
            Terminator::Switch {
                arms, otherwise, ..
            } => {
                edges.push(*otherwise);
                edges.extend(
                    arms.get(&body.pools.arms)
                        .unwrap()
                        .iter()
                        .map(|arm| arm.edge),
                );
            }
            Terminator::Raise {
                cleanup: Some(block),
                ..
            } => incoming[block.index()].push((b, false)),
            _ => {}
        }
        for e in edges {
            let e = &body.edges[e.index()];
            incoming[e.target.index()].push((b, e.cold));
        }
        for inst in body.blocks[b].insts.get(&body.insts).unwrap() {
            if let Some(block) = inst.fail(body) {
                incoming[block.index()].push((b, false));
            }
        }
    }
    let mut cold = vec![false; n];
    loop {
        let mut changed = false;
        for b in 1..n {
            if !cold[b]
                && !incoming[b].is_empty()
                && incoming[b].iter().all(|&(from, edge)| edge || cold[from])
            {
                cold[b] = true;
                changed = true;
            }
        }
        if !changed {
            return cold;
        }
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
