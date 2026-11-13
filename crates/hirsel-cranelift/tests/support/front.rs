//! Shared input fixture for the native tests and backend measurement example.
use hirsel_core_front::{Emitter, ModuleInput, Program};
use hirsel_core_ids::PackageId;
use hirsel_core_ir::{FuncId, Module};
use hirsel_core_resolve::{Environment, ModuleId, NameIndex, PackageManifest};
use hirsel_core_syntax::{ParseMode, parse};
use std::{collections::HashMap, path::Path};
pub fn lower(root: &Path, source: &str) -> (Module, HashMap<String, FuncId>) {
    let core = PackageId {
        name: "core".into(),
        version: "1".into(),
    };
    let app = PackageId {
        name: "app".into(),
        version: "1".into(),
    };
    let mut modules = Vec::new();
    for m in PackageManifest::directory(&root.join("core"), core.clone(), true)
        .unwrap()
        .read()
        .unwrap()
    {
        assert!(m.parsed.diagnostics.is_empty());
        modules.push(ModuleInput {
            id: m.module,
            syntax: m.parsed.syntax,
        });
    }
    let id = ModuleId {
        package: app.clone(),
        modules: vec!["main".into()],
    };
    let parsed = parse(source, ParseMode::Source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    modules.push(ModuleInput {
        id,
        syntax: parsed.syntax,
    });
    let mut index = NameIndex::new(Environment::default());
    for m in &modules {
        index.add_module(&m.id);
        for (i, item) in m.syntax.items.iter().enumerate() {
            index.index_item(&m.id, i as u32, item);
        }
    }
    index.configure_core(core);
    let program = Program::declare(&index, &modules);
    assert!(program.refusals.is_empty(), "{:?}", program.refusals);
    let mut emitter = Emitter::new();
    let mut functions = HashMap::new();
    for (i, f) in program.fns.iter().enumerate() {
        if f.path.0.package != app || f.body.is_none() {
            continue;
        }
        let checked = program.check_lower(i, &mut emitter);
        assert!(checked.refusals.is_empty(), "{:?}", checked.refusals);
        assert!(checked.gaps.is_empty(), "{:?}", checked.gaps);
        functions.insert(f.path.0.name.clone(), checked.function.unwrap());
    }
    (emitter.module, functions)
}
