//! Fixed runtime entries call ordinary CoreIR functions through their published
//! concrete convention. The input buffer also retains the plain code pointer.
use super::cleanup::{drop_signature, glue_emitter};
use super::*;

pub(super) fn input_offset(
    module: &Module,
    recipes: &[hirsel_core_layout::Recipe],
    x: TypeId,
) -> u64 {
    hirsel_core_layout::field_size(
        &module.types,
        recipes,
        &hirsel_core_layout::FieldTy::Inline(hirsel_core_layout::TypeRef(x.0)),
    )
    .1
    .max(8)
}
pub(super) fn task_entry_name(x: TypeId, r: TypeId) -> String {
    format!("core_task_entry_{}_{}", x.0, r.0)
}
pub(super) fn transfer_entry_name(x: TypeId, r: TypeId) -> String {
    format!("core_transfer_entry_{}_{}", x.0, r.0)
}
#[allow(clippy::too_many_arguments)]
pub(super) fn emit(
    module: &Module,
    target: Target,
    recipes: &[hirsel_core_layout::Recipe],
    call_layouts: &hirsel_core_ir::native_layout::CallLayouts,
    failures: &failure::Failures,
    facts: bool,
    sink: &mut dyn Sink,
    items: &mut Items,
) -> Result<()> {
    // A spawned task's code takes an environment first; a root a transfer
    // enters (`RtOp::Transfer`) is a function's own, with none.
    let pairs = module
        .functions
        .iter()
        .filter_map(|f| f.body.as_ref())
        .flat_map(|b| b.calls.iter())
        .filter_map(|c| match c.callee {
            Callee::Runtime(RtOp::Spawn { x, r } | RtOp::SpawnUnit { x, r }) => Some((x, r, true)),
            Callee::Runtime(RtOp::Transfer { x, r }) => Some((x, r, false)),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let input = Value::Local(Local::INPUT);
    let body = Body::default();
    let result = Value::Local(Local::RESULT);
    for (x, r, environment) in pairs {
        let xc = module.types[x.index()].carried;
        let rc = module.types[r.index()].carried;
        let fields = match rc {
            Carried::Value(t) => vec![t],
            _ => vec![],
        };
        let argument = match xc {
            Carried::Address => call_layouts.record(x),
            _ => None,
        };
        let returned = match rc {
            Carried::Address => call_layouts.record(r),
            _ => None,
        };
        let abi = abi::Return::core_record(target, &fields, returned, true);
        sink.begin_function(
            &if environment {
                task_entry_name(x, r)
            } else {
                transfer_entry_name(x, r)
            },
            &FnSig::plain(Type::I8, [Type::Ptr, Type::Ptr, Type::Ptr]),
            &[Local::CONTEXT, Local::RESULT, Local::INPUT],
            &FnAttrs::private(),
        );
        let mut e = glue_emitter(
            module,
            recipes,
            failures,
            facts,
            &body,
            abi::Return::on_target(target, &[Ty::Scalar(Scalar::U8)]),
            false,
            int(0),
            &mut *sink,
            &mut *items,
        );
        let code = e.load(&Type::Ptr, &input, 8);
        let data = e.offset(&input, input_offset(module, recipes, x));
        let mut args = vec![ptr(CONTEXT)];
        if matches!(rc, Carried::Address) && returned.is_none() {
            args.push(ptr(result.clone()));
        }
        if environment {
            args.push(ptr(Value::Null));
        }
        match xc {
            Carried::Address => {
                if let Some(record) = argument {
                    let values = e.load_record(record, &data);
                    args.extend(
                        record
                            .fields()
                            .iter()
                            .zip(values)
                            .map(|(&(_, t), v)| (abi.value_parameter(t), v)),
                    );
                } else {
                    args.push(ptr(data));
                }
            }
            Carried::Value(t) => {
                let v = e.load(&ty(t), &data, 1);
                args.push((
                    Parameter::extended(ty(t), abi::extension(e.abi.target, t)),
                    v,
                ));
            }
            Carried::Nothing => {}
        }
        args.push(word(int(0)));
        let values = e.invoke(&abi, &code, args, &CallAttrs::default());
        let status = values.last().unwrap().clone();
        if !fields.is_empty() || returned.is_some() {
            let success = e.cmp(Predicate::Eq, &Type::I8, &status, &int(0));
            let store = Label::plain("store_result");
            let done = Label::plain("done");
            e.cond(&success, store, done);
            e.block(store);
            if let Some(record) = returned {
                e.store_record(record, &result, &values);
            } else {
                e.store(&ty(fields[0]), &values[0], &result, 1);
            }
            e.br(done);
            e.block(done);
        }
        e.ret(&Type::I8, &status);
        drop(e);
        sink.end_function();
    }
    Ok(())
}
