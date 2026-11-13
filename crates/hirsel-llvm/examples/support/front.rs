//! Source fixture lowering shared by native agreement and developer measurements.
use hirsel_core_front::{Emitter, ModuleInput, Program};
use hirsel_core_ids::PackageId;
use hirsel_core_ir::{FuncId, Module};
use hirsel_core_resolve::{ModuleSource, NameIndex, PackageManifest};
use hirsel_core_syntax::{ParseMode, Symbols, parse};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn lower(
    source: &str,
    core: &Path,
    entry: &str,
) -> Result<(Module, HashMap<String, FuncId>), String> {
    let package = |name: &str| PackageId {
        name: name.into(),
        version: "1".into(),
    };
    let mut symbols = Symbols::default();
    let core =
        PackageManifest::directory(core, package("core"), true).map_err(|e| e.to_string())?;
    let app = PackageManifest {
        package: package("app"),
        sources: vec![ModuleSource {
            file: PathBuf::from("input.hirsel"),
            modules: vec!["main".into()],
            signatures: false,
        }],
        dependencies: vec![package("core")],
    };
    core.intern(&mut symbols);
    app.intern(&mut symbols);
    let mut syntax = core
        .read(&mut symbols)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|m| m.parsed.syntax)
        .collect::<Vec<_>>();
    let parsed = parse(source, ParseMode::Source, &mut symbols);
    if !parsed.diagnostics.is_empty() {
        return Err(format!("{:?}", parsed.diagnostics));
    }
    syntax.push(parsed.syntax);
    let mut index = NameIndex::new(std::sync::Arc::new(symbols));
    let ids = index.add_manifests(&[core, app]);
    let modules = ids
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
    let main = modules.last().unwrap().id;
    let mut program = Program::declare(&index, &modules);
    program.entry = index
        .symbols
        .find(entry)
        .and_then(|s| index.definition(main, s))
        .map(|d| index.definition_path(d));
    if !program.refusals.is_empty() || !program.gaps.is_empty() {
        return Err(format!("{:?} {:?}", program.refusals, program.gaps));
    }
    let mut emitter = Emitter::new();
    let mut names = HashMap::new();
    for (f, fd) in program.fns.iter().enumerate() {
        if index.module_package(fd.site.module) != index.module_package(main) || fd.body.is_none() {
            continue;
        }
        let check = program.check_lower(f, &mut emitter);
        if !check.refusals.is_empty() || !check.gaps.is_empty() {
            return Err(format!("{check:?}"));
        }
        if let Some(id) = check.function {
            names.insert(fd.path.0.name.clone(), id);
        }
    }
    Ok((emitter.module, names))
}
