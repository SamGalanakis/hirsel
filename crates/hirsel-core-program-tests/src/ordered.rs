use hirsel_core_front::Program;
use hirsel_core_ir::{FuncId, Inst};
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
                let calls = body
                    .insts
                    .iter()
                    .filter(|i| matches!(i, Inst::Call { .. }))
                    .count();
                let comparisons = body
                    .insts
                    .iter()
                    .filter(|i| matches!(i, Inst::Compare { .. }))
                    .count();
                eprintln!(
                    "ORDER COST {:?}: {} insts, {comparisons} compares, {calls} calls",
                    key,
                    body.insts.len()
                );
                if module.argument_names(key) == ["u64"]
                    && matches!(key.definition.0.name.as_str(), "less" | "equal")
                {
                    assert_eq!(calls, 0);
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
