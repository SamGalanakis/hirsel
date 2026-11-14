//! Native task-entry glue. The runtime owns a fixed C entry and an input buffer;
//! ordinary CoreIR callable signatures retain their concrete value convention.
use super::*;
use cranelift_codegen::ir::{MemFlagsData, Value, condcodes::IntCC};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Glues {
    pub entries: BTreeMap<(TypeId, TypeId), cranelift_module::FuncId>,
    pub inputs: BTreeMap<TypeId, cranelift_module::FuncId>,
}
impl Glues {
    pub fn declare(object: &mut ObjectModule, input: &c::Module) -> Result<Self> {
        let pairs = input
            .functions
            .iter()
            .filter_map(|f| f.body.as_ref())
            .flat_map(|b| b.calls.iter())
            .filter_map(|c| match c.callee {
                Callee::Runtime(RtOp::Spawn { x, r }) => Some((x, r)),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let mut out = Self {
            entries: BTreeMap::new(),
            inputs: BTreeMap::new(),
        };
        for (x, r) in pairs {
            let f = object
                .declare_function(
                    &format!("hirsel.task.entry.{}.{}", x.0, r.0),
                    Linkage::Local,
                    &abi::Signature::c(&[Ty::Ptr; 3], &[Ty::Scalar(Scalar::U8)]).clif,
                )
                .map_err(error)?;
            out.entries.insert((x, r), f);
            if let std::collections::btree_map::Entry::Vacant(e) = out.inputs.entry(x) {
                let f = object
                    .declare_function(
                        &format!("hirsel.task.input.{}", x.0),
                        Linkage::Local,
                        &abi::Signature::c(&[Ty::Ptr; 2], &[Ty::Scalar(Scalar::U8)]).clif,
                    )
                    .map_err(error)?;
                e.insert(f);
            }
        }
        Ok(out)
    }
    pub fn emit(
        &self,
        object: &mut ObjectModule,
        tables: &Tables<'_>,
        observer: &impl profile::Observer,
        scratch: &mut Compilation,
    ) -> Result<()> {
        for (&x, &f) in &self.inputs {
            let context = &mut scratch.codegen;
            object.clear_context(context);
            context.func.signature =
                abi::Signature::c(&[Ty::Ptr; 2], &[Ty::Scalar(Scalar::U8)]).clif;
            let mut b = FunctionBuilder::new(&mut context.func, &mut scratch.frontend);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let ps = b.block_params(entry).to_vec();
            let offset = input_offset(tables.input, tables.layouts, x)?;
            let data = b.ins().iadd_imm(ps[1], offset as i64);
            let drop = object.declare_func_in_func(tables.glues.drops[x.index()].unwrap(), b.func);
            let call = b.ins().call(drop, &[ps[0], data]);
            let status = b.inst_results(call)[0];
            b.ins().return_(&[status]);
            b.seal_all_blocks();
            b.finalize(object.target_config());
            observer.define(object, f, context).map_err(error)?;
        }
        for (&(x, r), &f) in &self.entries {
            let context = &mut scratch.codegen;
            object.clear_context(context);
            context.func.signature =
                abi::Signature::c(&[Ty::Ptr; 3], &[Ty::Scalar(Scalar::U8)]).clif;
            let mut b = FunctionBuilder::new(&mut context.func, &mut scratch.frontend);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let ps = b.block_params(entry).to_vec();
            let code = b.ins().load(types::I64, MemFlagsData::trusted(), ps[2], 0);
            let data = b
                .ins()
                .iadd_imm(ps[2], input_offset(tables.input, tables.layouts, x)? as i64);
            let xc = tables.input.types[x.index()].carried;
            let rc = tables.input.types[r.index()].carried;
            let mut sig = c::Signature {
                params: match xc {
                    // The task takes its input (RtOp::Spawn).
                    Carried::Address => vec![Param::Address {
                        ty: x,
                        loan: c::Loan::Take,
                    }],
                    Carried::Value(t) => vec![Param::Value(t)],
                    Carried::Nothing => vec![],
                },
                results: match rc {
                    Carried::Value(t) => vec![t],
                    _ => vec![],
                },
                dest: matches!(rc, Carried::Address).then_some(r),
                never: false,
            };
            sig.params.insert(0, Param::Value(Ty::Ptr));
            let abi = abi::Signature::new(&sig);
            let signature = b.import_signature(abi.clif.clone());
            let mut args = Vec::with_capacity(4);
            args.push(ps[0]);
            if matches!(rc, Carried::Address) {
                args.push(ps[1]);
            }
            let environment = b.ins().iconst(types::I64, 0);
            args.push(environment);
            match xc {
                Carried::Address => args.push(data),
                Carried::Value(t) => {
                    let v = b.ins().load(ty(t), MemFlagsData::trusted(), data, 0);
                    args.push(v)
                }
                Carried::Nothing => {}
            }
            let call = b.ins().call_indirect(signature, code, &args);
            let physical = b.inst_results(call).to_vec();
            let mut values = [Value::from_u32(0); 2];
            let mut n = 0;
            abi.unpack(&mut b, &physical, None, |v| {
                values[n] = v;
                n += 1
            });
            let status = values[n - 1];
            if matches!(rc, Carried::Value(_)) {
                let success = b.create_block();
                let done = b.create_block();
                let ok = b.ins().icmp_imm(IntCC::Equal, status, 0);
                b.ins().brif(ok, success, &[], done, &[]);
                b.switch_to_block(success);
                b.ins().store(MemFlagsData::trusted(), values[0], ps[1], 0);
                b.ins().jump(done, &[]);
                b.switch_to_block(done);
            }
            b.ins().return_(&[status]);
            b.seal_all_blocks();
            b.finalize(object.target_config());
            observer.define(object, f, context).map_err(error)?;
        }
        Ok(())
    }
}
pub(super) fn input_offset(
    input: &c::Module,
    layouts: &[hirsel_core_layout::Recipe],
    x: TypeId,
) -> Result<u64> {
    let (_, align) = place::storage(input, layouts, PlaceTy::Declared(x))?;
    Ok(align.max(8))
}
