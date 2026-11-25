//! Linux System V / AAPCS64 scalar-struct returns. Like rustc_codegen_cranelift,
//! physical register carriers follow aggregate classification, not source arity.
use super::*;
use cranelift_codegen::ir::{
    ArgumentPurpose, MemFlagsData as MemFlags, StackSlotData, StackSlotKind, Value,
};
use hirsel_core_ir::native_layout::{CallLayouts, ScalarRecord};

pub struct Signature {
    pub clif: ir::Signature,
    pub offsets: Vec<u32>,
    pub results: Vec<Ty>,
    pub size: u32,
    pub indirect: bool,
    pub aggregate: bool,
    homogeneous: bool,
    pub param_records: Vec<Option<ScalarRecord>>,
    pub record: Option<ScalarRecord>,
    pub dest: bool,
}
impl Signature {
    pub fn new(s: &c::Signature, object: &ObjectModule, layouts: &CallLayouts) -> Self {
        Self::build(s, Some(layouts), object, true)
    }
    /// A Hirsel entry with no scalar-record parameter or result rewrites.
    /// Scalars, pointers and a destination follow the same ABI as unit code.
    pub fn foreign(s: &c::Signature, object: &ObjectModule) -> Self {
        Self::build(s, None, object, true)
    }
    pub fn c(object: &ObjectModule, params: &[Ty], results: &[Ty]) -> Self {
        let mut signature = Self::build(
            &c::Signature::new(
                params.iter().copied().map(Param::Value).collect(),
                results.to_vec(),
                None,
            ),
            None,
            object,
            false,
        );
        // Narrow C arguments and a lone narrow result carry the target's
        // extension: the C side of the call relies on the bits it names.
        let abi = c_abi(object);
        for (index, &t) in params.iter().enumerate() {
            let parameter = &mut signature.clif.params[index + usize::from(signature.indirect)];
            *parameter = extended(*parameter, abi, t);
        }
        if let ([t], [result]) = (results, signature.clif.returns.as_mut_slice()) {
            *result = extended(*result, abi, *t);
        }
        signature
    }
    fn build(
        s: &c::Signature,
        layouts: Option<&CallLayouts>,
        object: &ObjectModule,
        context: bool,
    ) -> Self {
        let arm = aarch64(object);
        let record = layouts.and_then(|layouts| layouts.result(s));
        let dest = s.dest.is_some() && record.is_none();
        let mut results: Vec<Ty> = if let Some(record) = record {
            record.fields().iter().map(|&(_, t)| t).collect()
        } else {
            s.results
                .iter()
                .copied()
                .flat_map(|t| std::iter::once(t).chain(simd::two_halves(t).then_some(t)))
                .collect()
        };
        if context {
            results.push(Ty::Scalar(Scalar::U8));
        }
        let mut clif = ir::Signature::new(object.isa().default_call_conv());
        let mut offsets = Vec::new();
        let mut size: u32 = 0;
        let mut align = 1;
        for &r in &results {
            let n = ty(r).bytes();
            align = align.max(n);
            size = size.next_multiple_of(n);
            offsets.push(size);
            size += n;
        }
        size = size.next_multiple_of(align);
        // AAPCS64 homogeneous float aggregates use one vector register per
        // member, including four f64s. All other composites up to16bytes use
        // x0/x1, even a float-only eightbyte within a mixed aggregate.
        let homogeneous = arm
            && !results.is_empty()
            && results.len() <= 4
            && results[0].is_float()
            && results.iter().all(|r| *r == results[0]);
        let indirect = record.is_none() && size > 16 && !homogeneous;
        let aggregate = results.len() > 1;
        if indirect {
            clif.params
                .push(AbiParam::special(types::I64, ArgumentPurpose::StructReturn));
        }
        // FORM #38 uses context plus a trailing u8 result status for every
        // function row. Raw runtime C signatures omit both additions.
        if context {
            clif.params.push(AbiParam::new(types::I64));
        }
        if dest {
            clif.params.push(AbiParam::new(types::I64));
        }
        let param_records: Vec<_> = s
            .params
            .iter()
            .map(|&p| layouts.and_then(|layouts| layouts.parameter(p)))
            .collect();
        for (&p, scalar) in s.params.iter().zip(&param_records) {
            if let Some(record) = scalar {
                clif.params
                    .extend(record.fields().iter().map(|&(_, t)| AbiParam::new(ty(t))));
            } else {
                match p {
                    Param::Value(t) => {
                        clif.params.push(AbiParam::new(ty(t)));
                        if simd::two_halves(t) {
                            clif.params.push(AbiParam::new(ty(t)));
                        }
                    }
                    Param::Address { .. } => clif.params.push(AbiParam::new(types::I64)),
                }
            }
        }
        if context {
            // FORM#91: activation fallback, after every source parameter.
            // The process/task C callbacks keep their published signatures.
            clif.params.push(AbiParam::new(types::I64));
        }
        if !indirect {
            if !aggregate || homogeneous || record.is_some() {
                clif.returns
                    .extend(results.iter().map(|&r| AbiParam::new(ty(r))));
            } else {
                for chunk in 0..size.div_ceil(8) {
                    let mut integer = false;
                    let mut f64_ = false;
                    for (&r, &off) in results.iter().zip(&offsets) {
                        if off / 8 == chunk {
                            integer |= !r.is_float();
                            f64_ |= ty(r) == types::F64;
                        }
                    }
                    let carrier = if arm || integer {
                        types::I64
                    } else if f64_ || size - chunk * 8 > 4 {
                        types::F64
                    } else {
                        types::F32
                    };
                    clif.returns.push(AbiParam::new(carrier));
                }
            }
        }
        Self {
            clif,
            offsets,
            results,
            size,
            indirect,
            aggregate,
            homogeneous,
            param_records,
            record,
            dest,
        }
    }
    pub fn unpack(
        &self,
        b: &mut FunctionBuilder<'_>,
        values: &[Value],
        storage: Option<Value>,
        mut push: impl FnMut(Value),
    ) {
        if !self.aggregate || self.homogeneous || self.record.is_some() {
            for &value in values {
                push(value);
            }
            return;
        }
        for (&t, &offset) in self.results.iter().zip(&self.offsets) {
            let target = ty(t);
            let value = if self.indirect {
                b.ins().load(
                    target,
                    result_flags(target),
                    storage.unwrap(),
                    offset as i32,
                )
            } else {
                let carrier = values[(offset / 8) as usize];
                let native = b.func.dfg.value_type(carrier);
                if offset % 8 == 0 && native == target {
                    carrier
                } else {
                    let integer = match native.bytes() {
                        4 => types::I32,
                        _ => types::I64,
                    };
                    let bits = if native.is_float() {
                        b.ins().bitcast(integer, MemFlags::new(), carrier)
                    } else {
                        carrier
                    };
                    let bits = if offset % 8 == 0 {
                        bits
                    } else {
                        b.ins().ushr_imm_s(bits, i64::from(offset % 8 * 8))
                    };
                    let integer = match target.bytes() {
                        1 => types::I8,
                        2 => types::I16,
                        4 => types::I32,
                        _ => types::I64,
                    };
                    let bits = if b.func.dfg.value_type(bits) != integer {
                        b.ins().ireduce(integer, bits)
                    } else {
                        bits
                    };
                    if target.is_float() {
                        b.ins().bitcast(target, MemFlags::new(), bits)
                    } else {
                        bits
                    }
                }
            };
            push(value);
        }
    }
    pub fn return_(&self, b: &mut FunctionBuilder<'_>, values: &[Value], storage: Option<Value>) {
        if !self.aggregate || self.homogeneous || self.record.is_some() {
            b.ins().return_(values);
            return;
        }
        if self.indirect {
            let addr = storage.unwrap();
            for (&value, &offset) in values.iter().zip(&self.offsets) {
                let flags = result_flags(b.func.dfg.value_type(value));
                b.ins().store(flags, value, addr, offset as i32);
            }
            b.ins().return_(&[]);
            return;
        }
        let mut registers = [Value::from_u32(0); 2];
        for (chunk, result) in self.clif.returns.iter().enumerate() {
            let mut fields = values
                .iter()
                .zip(&self.offsets)
                .zip(&self.results)
                .filter(|((_, offset), _)| **offset as usize / 8 == chunk);
            let ((&value, &offset), &t) = fields.next().unwrap();
            if fields.next().is_none() && offset % 8 == 0 && ty(t) == result.value_type {
                registers[chunk] = value;
                continue;
            }
            let mut bits = None;
            for ((&value, &offset), &t) in values.iter().zip(&self.offsets).zip(&self.results) {
                if offset as usize / 8 != chunk {
                    continue;
                }
                let native = ty(t);
                let integer = if native == types::F32 {
                    types::I32
                } else {
                    types::I64
                };
                let value = if native.is_float() {
                    b.ins().bitcast(integer, MemFlags::new(), value)
                } else {
                    value
                };
                let value = if b.func.dfg.value_type(value) != types::I64 {
                    b.ins().uextend(types::I64, value)
                } else {
                    value
                };
                let value = if offset % 8 == 0 {
                    value
                } else {
                    b.ins().ishl_imm_s(value, i64::from(offset % 8 * 8))
                };
                bits = Some(if let Some(bits) = bits {
                    b.ins().bor(bits, value)
                } else {
                    value
                });
            }
            let bits = bits.unwrap();
            let target = result.value_type;
            registers[chunk] = if target == types::I64 {
                bits
            } else {
                let bits = if target == types::F32 {
                    b.ins().ireduce(types::I32, bits)
                } else {
                    bits
                };
                b.ins().bitcast(target, MemFlags::new(), bits)
            };
        }
        b.ins().return_(&registers[..self.clif.returns.len()]);
    }
}

fn result_flags(t: ir::Type) -> MemFlags {
    // The shared caller result slot is aligned to a machine word.
    if t.is_vector() {
        MemFlags::new()
    } else {
        MemFlags::trusted()
    }
}

/// The module owns one target; every imported/generated C signature uses it.
pub(super) fn aarch64(object: &ObjectModule) -> bool {
    object.isa().name() == "aarch64"
}

/// The C ABI of the module's one target.
fn c_abi(object: &ObjectModule) -> hirsel_core_layout::CAbi {
    let triple = object.isa().triple().to_string();
    hirsel_core_layout::Target::from_triple(&triple)
        .unwrap_or_else(|| panic!("no C ABI for target {triple}"))
        .c_abi()
}
fn extended(param: AbiParam, abi: hirsel_core_layout::CAbi, t: Ty) -> AbiParam {
    match hirsel_core_layout::c_extension(abi, t) {
        hirsel_core_layout::CExtension::None => param,
        hirsel_core_layout::CExtension::Zero => param.uext(),
        hirsel_core_layout::CExtension::Sign => param.sext(),
    }
}
/// A C scalar argument or result of `t` for this module's target.
pub fn c_param(object: &ObjectModule, t: Ty) -> AbiParam {
    extended(AbiParam::new(ty(t)), c_abi(object), t)
}

pub(super) fn record_place(b: &mut FunctionBuilder<'_>, record: ScalarRecord) -> Value {
    let slot = b.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        record.size as u32,
        3,
    ));
    b.ins().stack_addr(types::I64, slot, 0)
}

pub(super) fn load_record(
    b: &mut FunctionBuilder<'_>,
    record: ScalarRecord,
    address: Value,
    values: &mut Vec<Value>,
) {
    values.extend(record.fields().iter().map(|&(offset, t)| {
        b.ins()
            .load(ty(t), MemFlags::trusted(), address, offset as i32)
    }));
}

pub(super) fn store_record(
    b: &mut FunctionBuilder<'_>,
    record: ScalarRecord,
    address: Value,
    values: &[Value],
) {
    for (&(offset, _), &value) in record.fields().iter().zip(values) {
        b.ins()
            .store(MemFlags::trusted(), value, address, offset as i32);
    }
}

pub(super) fn op_signature(op: HostOp) -> Vec<Ty> {
    hirsel_core_ir::RtOp::Host(op)
        .signature()
        .params
        .into_iter()
        .map(|p| match p {
            hirsel_core_ir::Param::Value(t) => t,
            _ => unreachable!(),
        })
        .collect()
}
