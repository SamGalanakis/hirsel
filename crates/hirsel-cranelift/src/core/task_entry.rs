//! Native task-entry glue. The runtime owns a fixed C entry and an input buffer;
//! ordinary CoreIR callable signatures retain their concrete value convention.
use super::*;
use cranelift_codegen::ir::{MemFlagsData, Value, condcodes::IntCC};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Glues {
    pub entries: BTreeMap<(TypeId, TypeId), cranelift_module::FuncId>,
}
impl Glues {
    pub fn declare(object: &mut ObjectModule, input: &c::Module) -> Result<Self> {
        let pairs = input
            .functions
            .iter()
            .filter_map(|f| f.body.as_ref())
            .flat_map(|b| b.calls.iter())
            .filter_map(|c| match c.callee {
                Callee::Runtime(RtOp::Spawn { x, r } | RtOp::SpawnUnit { x, r }) => Some((x, r)),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let mut out = Self {
            entries: BTreeMap::new(),
        };
        for (x, r) in pairs {
            let f = object
                .declare_function(
                    &format!("hirsel.task.entry.{}.{}", x.0, r.0),
                    Linkage::Local,
                    &abi::Signature::c(object, &[Ty::Ptr; 3], &[Ty::Scalar(Scalar::U8)]).clif,
                )
                .map_err(error)?;
            out.entries.insert((x, r), f);
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
        for (&(x, r), &f) in &self.entries {
            let context = &mut scratch.codegen;
            object.clear_context(context);
            context.func.signature =
                abi::Signature::c(object, &[Ty::Ptr; 3], &[Ty::Scalar(Scalar::U8)]).clif;
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
                        place: c::PlaceTy::Declared(x),
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
            let abi = abi::Signature::new(&sig, object, tables.call_layouts);
            let signature = b.import_signature(abi.clif.clone());
            let mut args = Vec::with_capacity(4);
            args.push(ps[0]);
            if abi.dest {
                args.push(ps[1]);
            }
            let environment = b.ins().iconst(types::I64, 0);
            args.push(environment);
            match xc {
                Carried::Address => {
                    if let Some(record) = abi.param_records[1] {
                        abi::load_record(&mut b, record, data, &mut args);
                    } else {
                        args.push(data);
                    }
                }
                Carried::Value(t) => {
                    let v = b.ins().load(ty(t), MemFlagsData::trusted(), data, 0);
                    args.push(v)
                }
                Carried::Nothing => {}
            }
            args.push(b.ins().iconst(types::I64, 0));
            let call = b.ins().call_indirect(signature, code, &args);
            let physical = b.inst_results(call).to_vec();
            let mut values = [Value::from_u32(0); 3];
            let mut n = 0;
            abi.unpack(&mut b, &physical, None, |v| {
                values[n] = v;
                n += 1
            });
            let status = values[n - 1];
            if matches!(rc, Carried::Value(_)) || abi.record.is_some() {
                let success = b.create_block();
                let done = b.create_block();
                let ok = b.ins().icmp_imm(IntCC::Equal, status, 0);
                b.ins().brif(ok, success, &[], done, &[]);
                b.switch_to_block(success);
                if let Some(record) = abi.record {
                    abi::store_record(&mut b, record, ps[1], &values[..n - 1]);
                } else {
                    b.ins().store(MemFlagsData::trusted(), values[0], ps[1], 0);
                }
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
