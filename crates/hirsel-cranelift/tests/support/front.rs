//! Shared input fixture for the native tests and backend measurement example.
use hirsel_core_front::{Emitter, ModuleInput, Program};
use hirsel_core_ids::PackageId;
use hirsel_core_ir::{FuncId, Module};
use hirsel_core_resolve::{ModuleSource, NameIndex, PackageManifest};
use hirsel_core_syntax::{ParseMode, Symbols, parse};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
pub fn lower(root: &Path, source: &str) -> (Module, HashMap<String, FuncId>) {
    let package = |name: &str| PackageId {
        name: name.into(),
        version: "1".into(),
    };
    let core = PackageManifest::directory(&root.join("../core"), package("core"), true).unwrap();
    let app = PackageManifest {
        package: package("app"),
        sources: vec![ModuleSource {
            file: PathBuf::from("input.hirsel"),
            modules: vec!["main".into()],
            signatures: false,
        }],
        dependencies: vec![package("core")],
    };
    let mut symbols = Symbols::default();
    core.intern(&mut symbols);
    app.intern(&mut symbols);
    let mut syntax = core
        .read(&mut symbols)
        .unwrap()
        .into_iter()
        .map(|m| {
            assert!(m.parsed.diagnostics.is_empty());
            m.parsed.syntax
        })
        .collect::<Vec<_>>();
    let parsed = parse(source, ParseMode::Source, &mut symbols);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    syntax.push(parsed.syntax);
    let mut index = NameIndex::new(Arc::new(symbols));
    let modules = index
        .add_manifests(&[core, app])
        .into_iter()
        .zip(syntax)
        .map(|(id, syntax)| ModuleInput { id, syntax })
        .collect::<Vec<_>>();
    for m in &modules {
        for (i, item) in m.syntax.items().enumerate() {
            index.index_item(m.id, i as u32, item);
        }
    }
    let core = index
        .package(
            index.symbols.find("core").unwrap(),
            index.symbols.find("1").unwrap(),
        )
        .unwrap();
    index.configure_core(core);
    let program = Program::declare(&index, &modules);
    assert!(program.refusals.is_empty(), "{:?}", program.refusals);
    let mut emitter = Emitter::new();
    let mut functions = HashMap::new();
    let mut checked = vec![false; program.fns.len()];
    let mut refusals = Vec::new();
    for (i, f) in program.fns.iter().enumerate() {
        if f.path.0.package != package("app") || f.body.is_none() {
            continue;
        }
        checked[i] = true;
        let check = program.check_lower(i, &mut emitter);
        assert!(check.gaps.is_empty(), "{:?}", check.gaps);
        for refusal in &check.refusals {
            let site = &refusal.primary;
            let input = modules.iter().find(|m| m.id == site.module).unwrap();
            let start = input.syntax.items[site.item as usize].file_offset + site.span.start;
            let before = &input.syntax.text()[..start as usize];
            let line = before.matches('\n').count() + 1;
            let column = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
            refusals.push(format!("{} at {line}:{column}", refusal.rule));
        }
        if let Some(function) = check.function {
            functions.insert(f.path.0.name.clone(), function);
        }
    }
    let mut expected = source
        .lines()
        .filter_map(|l| {
            l.trim()
                .strip_prefix("// expect: refuse ")
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    expected.sort();
    refusals.sort();
    assert_eq!(refusals, expected, "source refusal rules and sites");
    // Core library operations can have ordinary source bodies. Assemble those
    // requested by the fixture before specializing their generic call rows.
    loop {
        let pending = program
            .fns
            .iter()
            .enumerate()
            .filter(|(i, f)| {
                !checked[*i]
                    && f.body.is_some()
                    && emitter
                        .module
                        .instances
                        .iter()
                        .any(|k| k.definition == f.path)
            })
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        if pending.is_empty() {
            break;
        }
        for i in pending {
            checked[i] = true;
            let check = program.check_lower(i, &mut emitter);
            assert!(
                check.refusals.is_empty() && check.gaps.is_empty(),
                "core body {}: {check:?}",
                program.fns[i].path
            );
        }
    }
    emitter
        .materialize_instances()
        .expect("materialize the front-end fixture call closure");
    (emitter.module, functions)
}
