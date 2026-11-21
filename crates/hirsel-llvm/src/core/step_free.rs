//! MEM-30: a function that code pointers, finalizers or the launcher enter both
//! with and without a current step bound selects its body at entry. Under a
//! bound the step window holds at most one slice; with none it starts at 2^62.
//! Direct calls already know their context and reach a body by its own symbol.
use super::*;

/// The checked body's symbol beside the selecting entry `name`.
pub(super) fn checked(name: &str) -> String {
    format!("{name}.checked")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn stub(
    sink: &mut dyn Sink,
    name: &str,
    checked: &str,
    free: &str,
    signature: &FnSig,
    locals: &[Local],
    linkage: Linkage,
) {
    let attrs = FnAttrs {
        linkage,
        ..FnAttrs::default()
    };
    sink.begin_function(name, signature, locals, &attrs);
    let args: Vec<_> = locals.iter().copied().map(Value::from).collect();
    let address = Local::Named("step.window.at");
    sink.instruction(
        Some(address),
        Operation::Gep {
            ty: &Type::I8,
            base: &CONTEXT,
            indices: &[(Type::I64, int(memory_layout::CONTEXT_STEPS as i128))],
            inbounds: true,
        },
        None,
    );
    let window = Local::Named("step.window");
    sink.instruction(
        Some(window),
        Operation::Load(&Type::I64, &address.into(), &Memory::aligned(8)),
        None,
    );
    let unbounded = Local::Named("step.unbounded");
    sink.instruction(
        Some(unbounded),
        Operation::Compare(
            Predicate::Sgt,
            &Type::I64,
            &window.into(),
            &int(i128::from(memory_layout::STEP_SLICE)),
        ),
        None,
    );
    let free_label = Label::plain("step_free");
    let checked_label = Label::plain("step_checked");
    sink.instruction(
        None,
        Operation::Conditional(&unbounded.into(), free_label, checked_label, false),
        None,
    );
    let returns = !signature.result.is_void();
    for (n, (label, callee)) in [(free_label, free), (checked_label, checked)]
        .into_iter()
        .enumerate()
    {
        sink.block(label);
        let result = Local::Temp(n);
        let attrs = CallAttrs {
            tail: true,
            ..CallAttrs::default()
        };
        sink.instruction(
            returns.then_some(result),
            Operation::Call(signature, &Value::global(callee.to_owned()), &args, &attrs),
            None,
        );
        sink.instruction(
            None,
            Operation::Return(returns.then_some((&signature.result, &result.into()))),
            None,
        );
    }
    sink.end_function();
}
