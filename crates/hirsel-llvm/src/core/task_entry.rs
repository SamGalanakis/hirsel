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
pub(super) fn emit(
    module: &Module,
    target: Target,
    recipes: &[hirsel_core_layout::Recipe],
    failures: &failure::Failures,
    facts: bool,
    sink: &mut dyn Sink,
    items: &mut Items,
) -> Result<()> {
    let pairs = module
        .functions
        .iter()
        .filter_map(|f| f.body.as_ref())
        .flat_map(|b| b.calls.iter())
        .filter_map(|c| match c.callee {
            Callee::Runtime(RtOp::Spawn { x, r }) => Some((x, r)),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let inputs = pairs.iter().map(|(x, _)| *x).collect::<BTreeSet<_>>();
    let input = Value::Local(Local::INPUT);
    for x in inputs {
        let offset = input_offset(module, recipes, x);
        sink.begin_function(
            &format!("core_task_input_drop_{}", x.0),
            &FnSig::plain(Type::I8, [Type::Ptr, Type::Ptr]),
            &[Local::CONTEXT, Local::INPUT],
            &FnAttrs::private(),
        );
        let data = Local::Named("x");
        let status = Local::Named("status");
        sink.instruction(
            Some(data),
            Operation::Gep {
                ty: &Type::I8,
                base: &input,
                indices: &[(Type::I64, int(offset))],
                inbounds: false,
            },
            None,
        );
        sink.instruction(
            Some(status),
            Operation::Call(
                &drop_signature(),
                &Value::global(format!("core_drop_{}", x.0)),
                &[CONTEXT, data.into(), int(0)],
                &CallAttrs::default(),
            ),
            None,
        );
        sink.instruction(
            None,
            Operation::Return(Some((&Type::I8, &status.into()))),
            None,
        );
        sink.end_function();
    }
    let body = Body::default();
    let result = Value::Local(Local::RESULT);
    for (x, r) in pairs {
        let xc = module.types[x.index()].carried;
        let rc = module.types[r.index()].carried;
        let fields = match rc {
            Carried::Value(t) => vec![t],
            _ => vec![],
        };
        let abi = abi::Return::internal_on_target(target, &fields);
        sink.begin_function(
            &format!("core_task_entry_{}_{}", x.0, r.0),
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
        if matches!(rc, Carried::Address) {
            args.push(ptr(result.clone()));
        }
        args.push(ptr(Value::Null));
        match xc {
            Carried::Address => args.push(ptr(data)),
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
        let values = e.invoke(&abi, &code, args);
        let status = values.last().unwrap().clone();
        if let Carried::Value(t) = rc {
            let success = e.cmp(Predicate::Eq, &Type::I8, &status, &int(0));
            let store = Label::plain("store_result");
            let done = Label::plain("done");
            e.cond(&success, store, done);
            e.block(store);
            e.store(&ty(t), &values[0], &result, 1);
            e.br(done);
            e.block(done);
        }
        e.ret(&Type::I8, &status);
        drop(e);
        sink.end_function();
    }
    Ok(())
}
