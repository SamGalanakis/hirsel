//! Privileged slot and clone laws run through checked definition products.
use hirsel_core_front::{ModuleInput, Program};
use hirsel_core_resolve::{CorpusManifest, PackageManifest};
use std::path::Path;
const SLOT_LAWS: &str = include_str!("../../../core/front/fixtures/slots-slot-laws.hirsel");
const CLONE_PRELUDE: &str = include_str!("../../../core/front/fixtures/slots-clone-prelude.hirsel");
const CLONE_LAWS: &str = include_str!("../../../core/front/fixtures/slots-clone-laws.hirsel");
fn core_inputs(prelude: &str, list: &str) -> (hirsel_core_resolve::NameIndex, Vec<ModuleInput>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let package = hirsel_core_ids::PackageId {
        name: "core".into(),
        version: "1".into(),
    };
    let mut corpus = CorpusManifest {
        core: package.clone(),
        packages: vec![PackageManifest::directory(&root.join("core/core"), package, true).unwrap()],
    }
    .load()
    .unwrap();
    for (module, text) in [("core", prelude), ("list", list)] {
        corpus
            .sources
            .iter_mut()
            .find(|s| s.module.modules == [module])
            .unwrap()
            .text
            .push_str(text);
    }
    let parsed = corpus.parse();
    let index = corpus.index(&parsed);
    let modules = corpus
        .sources
        .iter()
        .zip(parsed.files)
        .enumerate()
        .map(|(source_id, (s, p))| ModuleInput {
            source: hirsel_core_resolve::SourceId(source_id as u32),
            id: index.module(&s.module).unwrap(),
            syntax: p.syntax,
        })
        .collect();
    (index, modules)
}
#[test]
fn private_slot_contracts_and_misuse() {
    let mut laws = SLOT_LAWS.to_string();
    laws.push_str(CLONE_LAWS);
    laws.push_str(include_str!(
        "../../../core/front/fixtures/slots-needs-drop.hirsel"
    ));
    laws.push_str("\ntype TestOwn { xs: List[u64] } drop(t) { list.clear(mut t.xs) }\n// expect: run slots_own_finalizer() = 9\nfn slots_own_finalizer() -> u64 { let mut xs: List[u64] = list.new(); list.push(mut xs, 1); list.push(mut xs, 2); let x = TestOwn(xs: take xs); let _ = take x; 9 }\n");
    let (index, modules) = core_inputs(CLONE_PRELUDE, &laws);
    let p = Program::declare(&index, &modules);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    let names = laws
        .lines()
        .filter_map(|l| l.strip_prefix("fn ").or_else(|| l.strip_prefix("pub fn ")))
        .filter_map(|l| l.split_once('(').map(|(n, _)| n))
        .collect::<std::collections::BTreeSet<_>>();
    let (module, functions) = crate::SourceProgram::from_inputs(&index, &modules, |d| {
        d.0.modules == ["list"] && names.contains(d.0.name.as_str())
    });
    let mut passed = 0;
    let mut faults = 0;
    let mut pending_fault = None;
    for line in laws.lines() {
        if let Some(expect) = line.strip_prefix("// expect: run ") {
            let (call, want) = expect.split_once(" = ").unwrap();
            let (want, held) = want
                .split_once(" used ")
                .map_or((want, 0), |(v, n)| (v, n.parse().unwrap()));
            let name = call.strip_suffix("()").unwrap();
            let f = functions[name];
            eprintln!("running {name}");
            let sig = &module.signatures[module.functions[f.index()].signature.index()];
            let report = hirsel_core_ir::eval::run(&module, f, &[], u64::MAX);
            assert_eq!(
                report.outcome,
                hirsel_core_ir::eval::Outcome::Returned(vec![hirsel_core_ir::eval::scalar(
                    sig.results[0],
                    want
                )]),
                "{name}"
            );
            assert_eq!(report.used, held, "{name} leaked its admission");
            passed += 1;
        } else if let Some(fault) = line.strip_prefix("// expect: slot-fault ") {
            pending_fault = Some(fault);
        } else if let Some(fault) = pending_fault.take() {
            let name = line.strip_prefix("fn ").unwrap().split('(').next().unwrap();
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                hirsel_core_ir::eval::run(&module, functions[name], &[], u64::MAX)
            }))
            .err()
            .expect("slot misuse must fault on the evaluator");
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap();
            assert!(message.contains(fault), "{name}: {message}, want {fault}");
            faults += 1;
        }
    }
    assert_eq!((passed, faults), (11, 5));
}
