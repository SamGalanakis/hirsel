//! SEM-11 has one source identity and, on x86-64, one body per psABI level.
use super::*;
#[cfg(hirsel_lazy_core)]
use hirsel_core_ids::ContentHash;
use hirsel_core_layout::isa::{Level, X86_64_LEVELS};

#[cfg(hirsel_lazy_core)]
pub(super) fn program(modules: &[&Module]) -> BTreeSet<ContentHash> {
    modules
        .iter()
        .flat_map(|module| {
            module.functions.iter().enumerate().filter_map(|(i, f)| {
                (f.versioned && f.body.is_some()).then(|| module.code_identity(FuncId(i as u32)))
            })
        })
        .collect()
}

pub(super) fn symbol(name: &str, level: Level) -> String {
    format!("{name}.isa{}", level.number)
}

pub(super) fn stub(
    sink: &mut dyn Sink,
    name: &str,
    signature: &FnSig,
    locals: &[Local],
    linkage: Linkage,
    infallible: bool,
) {
    let full_name = if infallible {
        format!("{name}.infallible")
    } else {
        name.to_owned()
    };
    let attrs = FnAttrs {
        linkage,
        inline: Inline::Never,
        ..FnAttrs::default()
    };
    sink.begin_function(&full_name, signature, locals, &attrs);
    let args: Vec<_> = locals.iter().copied().map(Value::from).collect();
    let active = Local::Named("isa.level");
    sink.instruction(
        Some(active),
        Operation::Load(
            &Type::I8,
            &Value::global("hirsel_core_isa_level"),
            &Memory {
                align: 1,
                ordering: Some(sink::Ordering::Monotonic),
                ..Memory::default()
            },
        ),
        None,
    );
    let choices: Vec<_> = X86_64_LEVELS
        .iter()
        .map(|level| {
            (
                i128::from(level.number),
                Label::new("isa", usize::from(level.number)),
            )
        })
        .collect();
    sink.instruction(
        None,
        Operation::Switch(&Type::I8, &active.into(), choices[0].1, &choices[1..]),
        None,
    );
    for (level, &(_, label)) in X86_64_LEVELS.iter().zip(&choices) {
        sink.block(label);
        let mut callee = symbol(name, *level);
        if infallible {
            callee.push_str(".infallible");
        }
        let callee = Value::global(callee);
        let result = Local::Temp(usize::from(level.number));
        let attrs = CallAttrs {
            tail: true,
            inline: Inline::Never,
            ..CallAttrs::default()
        };
        let returns = !signature.result.is_void();
        sink.instruction(
            returns.then_some(result),
            Operation::Call(signature, &callee, &args, &attrs),
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
