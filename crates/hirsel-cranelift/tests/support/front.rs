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
    let core = PackageManifest::directory(&root.join("core"), package("core"), true).unwrap();
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
    for (i, f) in program.fns.iter().enumerate() {
        if f.path.0.package != package("app") || f.body.is_none() {
            continue;
        }
        let checked = program.check_lower(i, &mut emitter);
        assert!(checked.refusals.is_empty(), "{:?}", checked.refusals);
        assert!(checked.gaps.is_empty(), "{:?}", checked.gaps);
        functions.insert(f.path.0.name.clone(), checked.function.unwrap());
    }
    (emitter.module, functions)
}
