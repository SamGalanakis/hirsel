use super::*;
use cranelift_codegen::ir::{
    BlockArg, MemFlagsData as MemFlags, StackSlotData, StackSlotKind, Value,
};
use cranelift_frontend::Variable;

pub(super) struct Function<'a, 'b, 'c> {
    pub b: FunctionBuilder<'a>,
    pub object: &'b mut ObjectModule,
    pub body: &'c Body,
    pub input: &'c c::Module,
    pub layouts: &'c [hirsel_core_layout::Recipe],
    pub values: Vec<Variable>,
    pub addresses: Vec<Variable>,
    pub blocks: Vec<ir::Block>,
    pub slots: Vec<ir::StackSlot>,
    pub functions: &'c [cranelift_module::FuncId],
    pub statics: &'c [cranelift_module::DataId],
    pub signatures: &'c [abi::Signature],
    pub context: Value,
}

pub(super) fn emit(
    object: &mut ObjectModule,
    mut b: FunctionBuilder<'_>,
    body: &Body,
    input: &c::Module,
    layouts: &[hirsel_core_layout::Recipe],
    signature: &abi::Signature,
    signatures: &[abi::Signature],
    functions: &[cranelift_module::FuncId],
    statics: &[cranelift_module::DataId],
) -> Result<()> {
    let blocks: Vec<_> = body.blocks.iter().map(|_| b.create_block()).collect();
    b.append_block_params_for_function_params(blocks[0]);
    for (i, block) in body.blocks.iter().enumerate().skip(1) {
        for v in block.params.get(&body.pools.values).unwrap() {
            b.append_block_param(blocks[i], ty(body.values[v.index()]));
        }
    }
    let values: Vec<_> = body.values.iter().map(|&t| b.declare_var(ty(t))).collect();
    let addresses = body
        .addresses
        .iter()
        .map(|_| b.declare_var(types::I64))
        .collect();
    let mut slots = Vec::new();
    for &s in &body.slots {
        let (size, align) = super::place::storage(input, layouts, s)?;
        slots.push(b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size.max(1) as u32,
            align.trailing_zeros() as u8,
        )));
    }
    b.switch_to_block(blocks[0]);
    let params = b.block_params(blocks[0]).to_vec();
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
    };
    for (i, block) in body.blocks.iter().enumerate() {
        if i != 0 {
            f.b.switch_to_block(f.blocks[i]);
        }
        let params = f.b.block_params(f.blocks[i]).to_vec();
        let params = if i == 0 {
            &params[usize::from(signature.indirect) + 1..]
        } else {
            &params[..]
        };
        for (&v, &param) in block
            .params
            .get(&body.pools.values)
            .unwrap()
            .iter()
            .zip(params)
        {
            f.set(v, param);
        }
        for inst in block.insts.get(&body.insts).unwrap() {
            f.instruction(inst)?;
        }
        match &body.terminators[i] {
            Terminator::Return(range) => {
                let results: Vec<_> = range
                    .get(&body.pools.values)
                    .unwrap()
                    .iter()
                    .map(|&v| f.get(v))
                    .collect();
                signature.return_(&mut f.b, &results, result_storage);
            }
            Terminator::Jump(e) => {
                let (target, args) = f.edge(*e);
                f.b.ins().jump(target, &args);
            }
            Terminator::Branch { condition, yes, no } => {
                let cond = f.get(*condition);
                let (yes, ya) = f.edge(*yes);
                let (no, na) = f.edge(*no);
                f.b.ins().brif(cond, yes, &ya, no, &na);
            }
            Terminator::Switch {
                value,
                arms,
                otherwise,
            } => {
                let value = f.get(*value);
                for arm in arms.get(&body.pools.arms).unwrap() {
                    let test =
                        f.b.ins()
                            .icmp_imm_s(ir::condcodes::IntCC::Equal, value, arm.value as i64);
                    let next = f.b.create_block();
                    let (target, args) = f.edge(arm.edge);
                    f.b.ins().brif(test, target, &args, next, &[]);
                    f.b.switch_to_block(next);
                }
                let (target, args) = f.edge(*otherwise);
                f.b.ins().jump(target, &args);
            }
            Terminator::Unreachable | Terminator::Raise { .. } | Terminator::Propagate => {
                return Err(unsupported(
                    "failure propagation requires the core runtime ABI",
                ));
            }
        }
    }
    f.b.seal_all_blocks();
    f.b.finalize(f.object.target_config());
    Ok(())
}

impl Function<'_, '_, '_> {
    pub fn get(&mut self, v: ValueId) -> Value {
        self.b.use_var(self.values[v.index()])
    }
    pub fn set(&mut self, v: ValueId, value: Value) {
        let value = if self.body.values[v.index()].is_float() {
            self.canonical(value)
        } else {
            value
        };
        self.b.def_var(self.values[v.index()], value);
    }
    pub fn addr(&mut self, a: AddressId) -> Value {
        self.b.use_var(self.addresses[a.index()])
    }
    fn edge(&mut self, e: EdgeId) -> (ir::Block, Vec<BlockArg>) {
        let edge = &self.body.edges[e.index()];
        let args = edge
            .args
            .get(&self.body.pools.values)
            .unwrap()
            .iter()
            .map(|&v| self.get(v).into())
            .collect();
        (self.blocks[edge.target.index()], args)
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
            Inst::ReadStatic { result, data } => {
                let global = self
                    .object
                    .declare_data_in_func(self.statics[data.index()], self.b.func);
                let v = self.b.ins().symbol_value(types::I64, global);
                self.set(result, v);
            }
            Inst::FunctionRef { result, function } => {
                let func = self
                    .object
                    .declare_func_in_func(self.functions[function.index()], self.b.func);
                let v = self.b.ins().func_addr(types::I64, func);
                self.set(result, v);
            }
            Inst::Unary {
                result,
                op,
                operand,
                ..
            } => {
                let v = self.get(operand);
                let t = self.body.values[operand.index()];
                let v = match op {
                    UnaryOp::Neg if t.is_float() => self.b.ins().fneg(v),
                    UnaryOp::Neg => {
                        return Err(unsupported("checked negation needs the failure ABI"));
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
                ..
            } => {
                let t = self.body.values[left.index()];
                if op.can_fail(t) {
                    return Err(unsupported("checked arithmetic needs the failure ABI"));
                }
                let l = self.get(left);
                let r = self.get(right);
                let v = self.binary(op, t, l, r)?;
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
            Inst::Address { address, .. } => {
                let v = self.address(address)?;
                self.b.def_var(self.addresses[address.index()], v);
            }
            Inst::Read {
                result, address, ..
            } => {
                let a = self.addr(address);
                let v = self.b.ins().load(
                    ty(self.body.values[result.index()]),
                    MemFlags::trusted(),
                    a,
                    0,
                );
                self.set(result, v);
            }
            Inst::Store { address, value } => {
                let a = self.addr(address);
                let v = self.get(value);
                self.b.ins().store(MemFlags::trusted(), v, a, 0);
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
                if call.fail.is_some() {
                    return Err(unsupported(
                        "call failure successor requires the core runtime ABI",
                    ));
                }
                let signature = &self.signatures[call.signature.index()];
                let storage = signature.indirect.then(|| signature.storage(&mut self.b));
                let mut args = Vec::new();
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
                        let f = self
                            .object
                            .declare_func_in_func(self.functions[f.index()], self.b.func);
                        self.b.ins().call(f, &args)
                    }
                    Callee::Indirect(v) => {
                        let v = self.get(v);
                        let sig = self.b.import_signature(signature.clif.clone());
                        self.b.ins().call_indirect(sig, v, &args)
                    }
                    Callee::Runtime(_) => return Err(unsupported("task runtime call")),
                };
                let results = self.b.inst_results(inst).to_vec();
                let results = signature.unpack(&mut self.b, &results, storage);
                for (&v, result) in call
                    .results
                    .get(&self.body.pools.values)
                    .unwrap()
                    .iter()
                    .zip(results)
                {
                    self.set(v, result);
                }
            }
            _ => self.memory(inst)?,
        }
        Ok(())
    }
}
