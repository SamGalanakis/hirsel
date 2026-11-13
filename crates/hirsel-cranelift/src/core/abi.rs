//! System V scalar-struct returns. Like rustc_codegen_cranelift's ABI lowering,
//! physical register carriers follow aggregate classification, not source arity.
use super::*;
use cranelift_codegen::ir::{ArgumentPurpose, MemFlagsData as MemFlags, Value};
use cranelift_codegen::isa::CallConv;

pub(super) struct Signature {
    pub clif: ir::Signature,
    pub offsets: Vec<u32>,
    pub results: Vec<Ty>,
    pub size: u32,
    pub indirect: bool,
    pub aggregate: bool,
}
impl Signature {
    pub fn new(s: &c::Signature) -> Self {
        let mut native = s.clone();
        native.results.push(Ty::Scalar(Scalar::U8));
        Self::build(&native, true)
    }
    pub fn c(params: &[Ty], results: &[Ty]) -> Self {
        Self::build(
            &c::Signature {
                params: params.iter().copied().map(Param::Value).collect(),
                results: results.to_vec(),
                dest: None,
                never: false,
            },
            false,
        )
    }
    fn build(s: &c::Signature, context: bool) -> Self {
        let mut clif = ir::Signature::new(CallConv::SystemV);
        let mut offsets = Vec::new();
        let mut size: u32 = 0;
        let mut align = 1;
        for &r in &s.results {
            let n = ty(r).bytes();
            align = align.max(n);
            size = size.next_multiple_of(n);
            offsets.push(size);
            size += n;
        }
        size = size.next_multiple_of(align);
        let indirect = size > 16;
        let aggregate = s.results.len() > 1;
        if indirect {
            clif.params
                .push(AbiParam::special(types::I64, ArgumentPurpose::StructReturn));
        }
        // FORM #38 uses context plus a trailing u8 result status for every
        // function row. Raw runtime C signatures omit both additions.
        if context {
            clif.params.push(AbiParam::new(types::I64));
        }
        if s.dest.is_some() {
            clif.params.push(AbiParam::new(types::I64));
        }
        clif.params.extend(s.params.iter().map(|p| {
            AbiParam::new(match *p {
                Param::Value(t) => ty(t),
                Param::Address(_) => types::I64,
            })
        }));
        if !indirect {
            if !aggregate {
                clif.returns
                    .extend(s.results.iter().map(|&r| AbiParam::new(ty(r))));
            } else {
                for chunk in 0..size.div_ceil(8) {
                    let mut integer = false;
                    let mut f64_ = false;
                    for (&r, &off) in s.results.iter().zip(&offsets) {
                        if off / 8 == chunk {
                            integer |= !r.is_float();
                            f64_ |= ty(r) == types::F64;
                        }
                    }
                    let carrier = if integer {
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
            results: s.results.clone(),
            size,
            indirect,
            aggregate,
        }
    }
    pub fn unpack(
        &self,
        b: &mut FunctionBuilder<'_>,
        values: &[Value],
        storage: Option<Value>,
        mut push: impl FnMut(Value),
    ) {
        if !self.aggregate {
            for &value in values {
                push(value);
            }
            return;
        }
        for (&t, &offset) in self.results.iter().zip(&self.offsets) {
            let target = ty(t);
            let value = if self.indirect {
                b.ins()
                    .load(target, MemFlags::trusted(), storage.unwrap(), offset as i32)
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
        if !self.aggregate {
            b.ins().return_(values);
            return;
        }
        if self.indirect {
            let addr = storage.unwrap();
            for (&value, &offset) in values.iter().zip(&self.offsets) {
                b.ins()
                    .store(MemFlags::trusted(), value, addr, offset as i32);
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
