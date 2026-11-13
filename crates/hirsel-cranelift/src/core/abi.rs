//! System V scalar-struct returns. Like rustc_codegen_cranelift's ABI lowering,
//! physical register carriers follow aggregate classification, not source arity.
use super::*;
use cranelift_codegen::ir::{
    ArgumentPurpose, MemFlagsData as MemFlags, StackSlotData, StackSlotKind, Value,
};
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
        Self::build(s, true)
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
        // The frozen v1a ABI supplies a context even for functions that need no
        // runtime operations. Infallible scalar entries do not return a status.
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
    pub fn storage(&self, b: &mut FunctionBuilder<'_>) -> Value {
        let slot = b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            self.size.max(8).next_multiple_of(8),
            3,
        ));
        let addr = b.ins().stack_addr(types::I64, slot, 0);
        let zero = b.ins().iconst(types::I64, 0);
        for off in (0..self.size.next_multiple_of(8)).step_by(8) {
            b.ins().store(MemFlags::trusted(), zero, addr, off as i32);
        }
        addr
    }
    pub fn unpack(
        &self,
        b: &mut FunctionBuilder<'_>,
        values: &[Value],
        storage: Option<Value>,
    ) -> Vec<Value> {
        if !self.aggregate {
            return values.to_vec();
        }
        let addr = storage.unwrap_or_else(|| self.storage(b));
        if !self.indirect {
            for (i, &v) in values.iter().enumerate() {
                b.ins().store(MemFlags::trusted(), v, addr, (i * 8) as i32);
            }
        }
        self.results
            .iter()
            .zip(&self.offsets)
            .map(|(&t, &off)| b.ins().load(ty(t), MemFlags::trusted(), addr, off as i32))
            .collect()
    }
    pub fn return_(&self, b: &mut FunctionBuilder<'_>, values: &[Value], storage: Option<Value>) {
        if !self.aggregate {
            b.ins().return_(values);
            return;
        }
        let addr = storage.unwrap_or_else(|| self.storage(b));
        for (&v, &off) in values.iter().zip(&self.offsets) {
            b.ins().store(MemFlags::trusted(), v, addr, off as i32);
        }
        let registers = if self.indirect {
            vec![]
        } else {
            self.clif
                .returns
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    b.ins()
                        .load(r.value_type, MemFlags::trusted(), addr, (i * 8) as i32)
                })
                .collect()
        };
        b.ins().return_(&registers);
    }
}
