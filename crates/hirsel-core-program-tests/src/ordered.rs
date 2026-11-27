use hirsel_core_front::Program;
use hirsel_core_ir::{Callee, FuncId, Inst, RtOp};
#[test]
fn scalar_order_instances_are_direct_predicates() {
    let source = r#"
fn less[T:ordered](a:View[T],b:View[T])->bool { a < b }
fn equal[T:key](a:T,b:T)->bool { a == b }
fn equal_same()->bool { equal(7u64,7u64) }
fn equal_different()->bool { equal(7u64,9u64) }
fn closed(a:View[u64],b:View[u64])->bool { a < b }
fn closed_text(a:View[String],b:View[String])->bool { a < b }
fn closed_min(a:u64,b:u64)->u64 { if b<a {b} else {a} }
fn min[T:ordered+plain](a:T,b:T)->T { if b<a {b} else {a} }
fn min_scalar()->u64 { min(7u64,9u64) }
fn scalar()->bool { let a=7u64; let b=9u64; less(view a,view b) }
fn text()->bool { let a="a"; let b="b"; less(view a,view b) }
"#;
    let (index, inputs) = crate::cases::program_inputs(source);
    let p = Program::declare(&index, &inputs);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    let (module, _functions) =
        crate::SourceProgram::from_inputs(&index, &inputs, |d| d.package_is("app", "1"));
    hirsel_core_ir::wf::module(&module).unwrap();
    for f in &module.functions {
        let key = &module.instances[f.instance.index()];
        if key.definition.0.modules == ["order"]
            || key.definition.0.name == "closed"
            || key.definition.0.name == "closed_text"
            || matches!(
                key.definition.0.name.as_str(),
                "less" | "equal" | "min" | "borrowed_min" | "closed_min" | "closed_borrowed"
            )
        {
            if let Some(body) = &f.body {
                let mut key_calls = 0;
                let mut step_charges = 0;
                let mut other_calls = 0;
                for inst in &body.insts {
                    let Inst::Call { call } = inst else {
                        continue;
                    };
                    match body.calls[call.index()].callee {
                        Callee::Runtime(RtOp::WorkSteps) => step_charges += 1,
                        Callee::Direct(f) => {
                            let callee =
                                &module.instances[module.functions[f.index()].instance.index()];
                            if callee.definition.0.package.name == "core"
                                && matches!(
                                    callee.definition.0.modules.as_slice(),
                                    [module] if module == "map" || module == "order"
                                )
                                && callee.definition.0.name.starts_with('$')
                            {
                                key_calls += 1;
                            } else {
                                other_calls += 1;
                            }
                        }
                        _ => other_calls += 1,
                    }
                }
                let comparisons = body
                    .insts
                    .iter()
                    .filter(|i| matches!(i, Inst::Compare { .. }))
                    .count();
                eprintln!(
                    "ORDER COST {:?}: {} insts, {comparisons} compares, {key_calls} key calls, {step_charges} step charges, {other_calls} other calls",
                    key,
                    body.insts.len()
                );
                if module.argument_names(key) == ["u64"]
                    && matches!(key.definition.0.name.as_str(), "less" | "equal")
                {
                    assert_eq!(key_calls, 0);
                    assert_eq!(other_calls, 0);
                    assert_eq!(step_charges, 1);
                    assert_eq!(comparisons, 1);
                }
            }
        }
    }
    for (name, expected) in [("equal_same", 1), ("equal_different", 0)] {
        let f = module
            .functions
            .iter()
            .position(|f| module.instances[f.instance.index()].definition.0.name == name)
            .unwrap();
        assert_eq!(
            hirsel_core_eval::run(&module, FuncId(f as u32), &[], 0).outcome,
            hirsel_core_eval::Outcome::Returned(vec![expected]),
        );
    }
    eprintln!("{}", hirsel_core_ir::text::print(&module));
}
