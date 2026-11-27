use hirsel_core_ir::{Body, Callee, Inst, Module, Param, RtOp};
use std::{collections::HashMap, mem::discriminant};

pub(super) fn check(unit_modules: &[hirsel_core_materialize::Unit<'_>]) {
    let find = |name: &str, generic: bool| {
        unit_modules
            .iter()
            .find_map(|unit| {
                let module = unit.module;
                let f = &module.functions[unit.function.index()];
                let key = &module.instances[f.instance.index()];
                (key.definition.0.name == name
                    && if generic {
                        module.argument_names(key) == ["u64"]
                    } else {
                        key.arguments.is_empty()
                    })
                .then_some((module, f))
            })
            .unwrap()
    };
    for (generic, closed) in [("less", "closed_less"), ("min", "closed_scalar")] {
        let (am, a) = find(generic, true);
        let (bm, b) = find(closed, false);
        let mut types = HashMap::new();
        let mut canonical = |module: &Module, signature: &hirsel_core_ir::Signature| {
            let mut signature = signature.clone();
            let mut ty = |t: &mut hirsel_core_ir::TypeId| {
                let next = hirsel_core_ir::TypeId(types.len() as u32);
                *t = *types.entry(module.closure(*t).hash).or_insert(next);
            };
            for parameter in &mut signature.params {
                if let Param::Address {
                    place: hirsel_core_ir::PlaceTy::Declared(t),
                    ..
                } = parameter
                {
                    ty(t);
                }
            }
            if let Some(t) = &mut signature.dest {
                ty(t);
            }
            signature
        };
        assert_eq!(
            canonical(am, &am.signatures[a.signature.index()]),
            canonical(bm, &bm.signatures[b.signature.index()])
        );
        let a = a.body.as_ref().unwrap();
        let b = b.body.as_ref().unwrap();
        let generic_ops = without_work_charge(a);
        let closed_ops = b
            .insts
            .iter()
            .map(std::mem::discriminant)
            .collect::<Vec<_>>();
        assert_eq!(generic_ops, closed_ops, "{generic}");
        assert_eq!(a.slots.len(), b.slots.len(), "{generic}");
    }
}

// The generic comparison pays the MEM-30 fixed part before both paths
// perform the same scalar comparison. Only its charge operands and call
// are excluded from the scalar-kernel comparison.
fn without_work_charge(body: &Body) -> Vec<std::mem::Discriminant<Inst>> {
    let mut instructions = body.insts.iter().map(discriminant).collect::<Vec<_>>();
    let metering = body.insts.iter().enumerate().filter_map(|(at, inst)| {
        if let Inst::Call { call } = *inst
            && body.calls[call.index()].callee == Callee::Runtime(RtOp::WorkSteps)
        {
            Some(at)
        } else {
            None
        }
    });
    for at in metering.rev() {
        let start = at.checked_sub(5).expect("work charge operands");
        assert!(matches!(
            &body.insts[start..at],
            [
                Inst::Const { .. },
                Inst::Const { .. } | Inst::LayoutConst { .. },
                Inst::IntArith {
                    op: hirsel_core_ir::ArithOp::Add,
                    ..
                },
                Inst::Const { .. },
                Inst::Const { .. },
            ]
        ));
        instructions.drain(start..=at);
    }
    instructions
}
