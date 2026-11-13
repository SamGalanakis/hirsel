//! Source fixture lowering shared by native agreement and developer measurements.
use hirsel_core_front::{Emitter, ModuleInput, Program};
use hirsel_core_ids::PackageId;
use hirsel_core_ir::{FuncId, Module};
use hirsel_core_resolve::{ModuleSource, NameIndex, PackageManifest};
use hirsel_core_syntax::{ParseMode, Symbols, parse};
use std::collections::{HashMap, HashSet};
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
    let mut checked = HashSet::new();
    let mut refusals = Vec::new();
    for (f, fd) in program.fns.iter().enumerate() {
        if index.module_package(fd.site.module) != index.module_package(main) || fd.body.is_none() {
            continue;
        }
        checked.insert(fd.path.clone());
        let check = program.check_lower(f, &mut emitter);
        if !check.gaps.is_empty() {
            return Err(format!("{check:?}"));
        }
        for refusal in &check.refusals {
            let site = &refusal.primary;
            let input = modules.iter().find(|m| m.id == site.module).unwrap();
            let start = input.syntax.items[site.item as usize].file_offset + site.span.start;
            let before = &input.syntax.text()[..start as usize];
            let line = before.matches('\n').count() + 1;
            let col = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
            refusals.push(format!("{} at {line}:{col}", refusal.rule));
        }
        if let Some(id) = check.function {
            names.insert(fd.path.0.name.clone(), id);
        }
    }
    let mut expected = source
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("// expect: refuse ")
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    expected.sort();
    refusals.sort();
    if refusals != expected {
        return Err(format!("refusals: {refusals:?}; expected: {expected:?}"));
    }
    // The fixture owns one combined module. Pull in the source/template bodies
    // named by its external rows, as the driver's instance collector does for
    // separately published definitions, before closing generic instances.
    loop {
        let needed = emitter
            .module
            .functions
            .iter()
            .filter(|f| f.body.is_none())
            .filter_map(|f| {
                let key = &emitter.module.instances[f.instance.index()];
                program.fns.iter().position(|fd| {
                    fd.path == key.definition && fd.body.is_some() && !checked.contains(&fd.path)
                })
            })
            .collect::<std::collections::BTreeSet<_>>();
        if needed.is_empty() {
            break;
        }
        for f in needed {
            checked.insert(program.fns[f].path.clone());
            let check = program.check_lower(f, &mut emitter);
            if !check.refusals.is_empty() || !check.gaps.is_empty() {
                return Err(format!("{}: {check:?}", program.fns[f].path));
            }
        }
    }
    emitter.materialize_instances()?;
    Ok((emitter.module, names))
}
