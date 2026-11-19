//! Source corpus assertions over per-definition products and reached units.
use crate::{Checked, repo};
use hirsel_core_front::*;
use hirsel_core_ids::{DefPath, PackageId};
use hirsel_core_ir::{FuncId, Identity, Module, Param, Ty, eval, wf};
use hirsel_core_resolve::{
    CorpusManifest, ModulePath, ModuleSource, NameIndex, PackageManifest, Site,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/front")
}

fn core_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/core")
}

fn package(name: &str) -> PackageId {
    PackageId {
        name: name.into(),
        version: "1".into(),
    }
}

pub(crate) fn program_inputs(source: &str) -> (NameIndex, Vec<ModuleInput>) {
    inputs_with_core(source, &core_dir())
}

pub(crate) fn inputs_with_core(source: &str, core_dir: &Path) -> (NameIndex, Vec<ModuleInput>) {
    inputs_in(source, core_dir, "main")
}

fn inputs_in(source: &str, core_dir: &Path, module: &str) -> (NameIndex, Vec<ModuleInput>) {
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("input.hirsel");
    std::fs::write(&path, source).unwrap();
    let corpus = crate::source_manifest(source, path, core_dir, module)
        .load()
        .unwrap();
    let parsed = corpus.parse();
    let index = corpus.index(&parsed);
    let modules = corpus
        .sources
        .iter()
        .zip(parsed.files)
        .enumerate()
        .map(|(i, (s, p))| ModuleInput {
            source: hirsel_core_resolve::SourceId(i as u32),
            id: index.module(&s.module).unwrap(),
            syntax: p.syntax,
        })
        .collect();
    (index, modules)
}

pub(super) fn line_col(modules: &[ModuleInput], site: &Site) -> String {
    let m = modules
        .iter()
        .find(|m| m.id == site.module)
        .expect("site module");
    let start = match m.syntax.items.get(site.item as usize) {
        Some(item) => item.file_offset + site.span.start,
        None => site.span.start,
    };
    let text = m.syntax.text();
    let before = &text[..start as usize];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap().chars().count() + 1;
    format!("{line}:{col}")
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Plain => "plain",
        Kind::Own => "own",
        Kind::Region => "region",
        Kind::Resource => "resource",
        Kind::Counted => "counted",
        Kind::Scoped => "scoped",
        Kind::Generic => "generic",
    }
}

fn holds_names(h: u8) -> String {
    let names = [
        (H_RESOURCE, "resource"),
        (H_CAPABILITY, "capability"),
        (H_SCOPED, "scoped"),
        (H_REGION, "region"),
        (H_OPAQUE, "opaque"),
    ];
    let v: Vec<_> = names
        .iter()
        .filter(|(b, _)| h & b != 0)
        .map(|(_, n)| *n)
        .collect();
    if v.is_empty() {
        "-".into()
    } else {
        v.join(",")
    }
}

fn concrete_rows_reaching_rigid(module: &Module) -> Vec<(String, String)> {
    use hirsel_core_layout::{Body as ShapeBody, FieldTy};
    fn refs(ty: &FieldTy, out: &mut Vec<u32>) {
        match ty {
            FieldTy::Inline(r)
            | FieldTy::Box(r)
            | FieldTy::Rc(r)
            | FieldTy::Arc(r)
            | FieldTy::RegionRef(r)
            | FieldTy::Link(r) => out.push(r.0),
            FieldTy::Seq(e)
            | FieldTy::Array(e, _)
            | FieldTy::View(e)
            | FieldTy::Locator(e)
            | FieldTy::FixedSpan(e)
            | FieldTy::SeqSpan(e)
            | FieldTy::IndexedSpan(e) => refs(e, out),
            FieldTy::Map(k, v) => {
                refs(k, out);
                refs(v, out);
            }
            FieldTy::Scalar(_)
            | FieldTy::Fn
            | FieldTy::OwnFn
            | FieldTy::Borrow
            | FieldTy::Handle(_)
            | FieldTy::Text
            | FieldTy::FrozenBytes => {}
        }
    }
    let mut found = vec![];
    for shape in module.types.iter().filter(|s| !s.name.contains('$')) {
        let mut out = vec![];
        let fields: Vec<_> = match &shape.body {
            ShapeBody::Record(fs) => fs.iter().collect(),
            ShapeBody::Variant(cs) => cs.iter().flat_map(|c| &c.fields).collect(),
        };
        for f in fields {
            refs(&f.ty, &mut out);
        }
        for r in out {
            let name = &module.types[r as usize].name;
            if name.contains('$') {
                found.push((shape.name.clone(), name.clone()));
            }
        }
    }
    found
}

pub(crate) fn run_case(path: &Path) -> Vec<String> {
    let source = std::fs::read_to_string(path).expect("case reads");
    let (index, modules) = program_inputs(&source);
    let mut p = Program::declare(&index, &modules);
    let main = index
        .module(&ModulePath {
            package: package("app"),
            modules: vec!["main".into()],
        })
        .unwrap();
    p.entry = index
        .symbols
        .find("main")
        .and_then(|s| index.definition(main, s))
        .map(|d| index.definition_path(d));
    let checked = Checked::source(&source, p.entry.clone());
    let product = &checked.product;
    let relevant = |site: &Site| {
        let path = product.index.module_path(site.module);
        path.package.name == "app"
            || (path.package.name == "core"
                && matches!(path.modules.as_slice(), [m] if matches!(m.as_str(), "map" | "string" | "list")))
    };
    let refusals: Vec<_> = product
        .diagnostics
        .iter()
        .filter(|r| relevant(&r.primary))
        .cloned()
        .collect();
    let gaps: Vec<_> = product
        .incomplete
        .iter()
        .filter(|g| relevant(&g.primary))
        .cloned()
        .collect();
    let resources: Vec<_> = product
        .resources
        .iter()
        .filter(|r| relevant(&r.primary))
        .cloned()
        .collect();
    let silent: Vec<_> = gaps
        .iter()
        .filter(|g| g.message == "emission halted without a reported reason")
        .collect();
    assert!(
        silent.is_empty(),
        "{}: accepted, not emitted, no gap: {silent:?}",
        path.display()
    );
    let demanded: Vec<_> = source
        .lines()
        .filter_map(|l| l.trim().strip_prefix("// expect: run "))
        .filter_map(|l| l.split_once('(').map(|(name, _)| name))
        .collect();
    let all_emitted = refusals.is_empty() && gaps.is_empty() && resources.is_empty();
    let roots: Vec<_> = product
        .definitions
        .values()
        .filter(|d| {
            d.definition.package_is("app", "1")
                && d.check.function.is_some()
                && d.check.refusals.is_empty()
                && d.incomplete.is_empty()
                && d.check
                    .template
                    .as_ref()
                    .is_none_or(|t| t.parameters.is_empty())
                && (all_emitted || demanded.contains(&d.definition.0.name.as_str()))
        })
        .map(|d| Identity::plain(d.definition.clone()))
        .collect();
    let units = product
        .units(roots.clone(), 65_536)
        .unwrap_or_else(|e| panic!("{}: units: {e:?}", path.display()));
    let unit_modules = units.modules().unwrap();
    let joined = match roots.first() {
        Some(entry) => {
            hirsel_core_materialize::join(&unit_modules, entry)
                .unwrap_or_else(|e| panic!("{}: join: {e}", path.display()))
                .0
        }
        None => Module::default(),
    };
    let module = &joined;
    let functions: HashMap<_, _> = module
        .functions
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let key = &module.instances[f.instance.index()];
            (key.definition.package_is("app", "1") && key.arguments.is_empty() && f.body.is_some())
                .then(|| (key.definition.0.name.clone(), FuncId(i as u32)))
        })
        .collect();
    let rigid = concrete_rows_reaching_rigid(module);
    assert!(
        rigid.is_empty(),
        "{}: concrete rows reach a rigid parameter row: {rigid:?}",
        path.display()
    );

    if path
        .file_stem()
        .is_some_and(|name| name == "map-operations")
    {
        for function in &module.functions {
            let key = &module.instances[function.instance.index()];
            if key.definition.0.modules == ["map"]
                && let Some(body) = &function.body
            {
                eprintln!(
                    "Map instance {} {:?}: {} instructions, {} terminators",
                    key.definition,
                    module.argument_names(key),
                    body.insts.len(),
                    body.terminators.len()
                );
            }
        }
    }
    if path
        .file_stem()
        .is_some_and(|x| x == "generic-allocation-layout")
    {
        use hirsel_core_ir::Inst;
        let mut checked = 0;
        for unit in &unit_modules {
            let module = unit.module;
            let f = &module.functions[unit.function.index()];
            let key = &module.instances[f.instance.index()];
            let expected = match (
                key.definition.0.name.as_str(),
                module.argument_names(key).as_slice(),
            ) {
                ("hold", [t]) if *t == "u32" => (44, 16, 8),
                ("hold", [t]) if *t == "()" => (40, 8, 8),
                ("hold", [t]) if *t == "app@1/main/Big" => (104, 72, 8),
                ("hold_only", [t]) if *t == "u32" => (36, 4, 4),
                _ => continue,
            };
            let Some(b) = &f.body else { continue };
            let constant = |value| {
                b.insts
                    .iter()
                    .find_map(|i| match i {
                        Inst::Const { result, bits } if *result == value => Some(*bits),
                        _ => None,
                    })
                    .unwrap()
            };
            let actual = b
                .insts
                .iter()
                .find_map(|i| match i {
                    Inst::Alloc {
                        kind: hirsel_core_ir::AllocKind::Resource { .. },
                        charge,
                        size,
                        align,
                        ..
                    } => Some((constant(*charge), constant(*size), *align)),
                    _ => None,
                })
                .unwrap();
            assert_eq!(actual, expected, "{key:?}");
            checked += 1;
        }
        assert_eq!(checked, 4);
    }
    if path.file_stem().is_some_and(|x| x == "ordered-cost") {
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
            assert_eq!(
                a.insts
                    .iter()
                    .map(std::mem::discriminant)
                    .collect::<Vec<_>>(),
                b.insts
                    .iter()
                    .map(std::mem::discriminant)
                    .collect::<Vec<_>>(),
                "{generic}"
            );
            assert_eq!(a.slots.len(), b.slots.len(), "{generic}");
        }
    }
    let mut want_refusals = vec![];
    // PROG-7: every refusal with causal sites, or with compared facts, states them.
    let mut want_causes = vec![];
    let mut want_compared = vec![];
    let mut want_gaps = vec![];
    let mut want_resources = vec![];
    let mut failures = vec![];
    let mut expects = 0;
    if all_emitted && let Err(e) = wf::module(module) {
        failures.push(format!("emitted CoreIR is not well formed: {e:?}"));
    } else if all_emitted {
        failures.extend(split_facts(module));
    }
    // Bug: two slots share a home while one is still needed (an address taken and
    // kept, a value read after another slot's write). Law: the module engines see,
    // its slots shared (hirsel_core_ir::extent::share_slots), is well formed and
    // gives every run below its expected value.
    let mut shared = module.clone();
    let Module {
        functions: bodies,
        signatures,
        ..
    } = &mut shared;
    for body in bodies.iter_mut().filter_map(|f| f.body.as_mut()) {
        hirsel_core_ir::extent::share_slots(body, signatures);
    }
    if all_emitted && let Err(e) = wf::module(&shared) {
        failures.push(format!(
            "CoreIR with shared slots is not well formed: {e:?}"
        ));
    }
    for line in source.lines() {
        let Some(e) = line.trim().strip_prefix("// expect: ") else {
            continue;
        };
        expects += 1;
        let words: Vec<&str> = e.split_whitespace().collect();
        match words.as_slice() {
            ["accept"] => {}
            ["cached"] => {
                let warm = crate::Driver::default()
                    .check(
                        crate::CheckRequest {
                            manifest: None,
                            inputs: checked.corpus.manifest.clone(),
                            store: checked._files.path().join("store"),
                            library: crate::LibraryInput::Sources,
                            entry: p.entry.clone(),
                        },
                        &mut crate::Meter::new(false),
                    )
                    .unwrap();
                let sites = |xs: &[hirsel_core_driver::SourceSite]| {
                    xs.iter()
                        .map(|s| (s.module.clone(), s.span))
                        .collect::<Vec<_>>()
                };
                let diagnostic_rows = |p: &hirsel_core_driver::CheckProduct| {
                    p.machine_diagnostics()
                        .unwrap()
                        .into_iter()
                        .map(|d| {
                            (
                                d.rule.to_owned(),
                                d.primary.module,
                                d.primary.span,
                                sites(&d.causal),
                                d.declared.map(str::to_owned),
                                d.required.map(str::to_owned),
                                d.message.to_owned(),
                            )
                        })
                        .collect::<Vec<_>>()
                };
                let gap_rows = |p: &hirsel_core_driver::CheckProduct| {
                    p.machine_incomplete()
                        .unwrap()
                        .into_iter()
                        .map(|g| {
                            (
                                g.primary.module,
                                g.primary.span,
                                sites(&g.causal),
                                g.message.to_owned(),
                            )
                        })
                        .collect::<Vec<_>>()
                };
                if diagnostic_rows(product) != diagnostic_rows(&warm)
                    || gap_rows(product) != gap_rows(&warm)
                {
                    failures.push(
                        "cached diagnostic provenance differs from the source request".into(),
                    );
                }
            }
            ["refuse", rule, "at", at] => want_refusals.push(format!("{rule} at {at}")),
            ["cause", rule, "at", at, "from", from @ ..] => {
                want_causes.push(format!("{rule} at {at} from {}", from.join(" ")))
            }
            ["compared", ..] => match e.split(" | ").collect::<Vec<_>>().as_slice() {
                [head, declared, required] => want_compared.push(format!(
                    "{} | {declared} | {required}",
                    head.trim_start_matches("compared ")
                )),
                _ => failures.push(format!("unreadable expectation `{e}`")),
            },
            ["message", rule, "at", at, "|", text @ ..] => {
                let text = text.join(" ");
                if !refusals.iter().any(|r| {
                    r.rule == *rule
                        && line_col(&modules, &r.primary) == *at
                        && r.message.contains(&text)
                }) {
                    failures.push(format!("missing message {rule} at {at}: {text}"));
                }
            }
            ["gap-cause", "at", at, "from", from @ ..] => {
                let wanted = from.join(" ");
                if !gaps.iter().any(|g| {
                    line_col(&modules, &g.primary) == *at
                        && g.causal
                            .iter()
                            .map(|s| line_col(&modules, s))
                            .collect::<Vec<_>>()
                            .join(" ")
                            == wanted
                }) {
                    failures.push(format!("missing gap cause at {at} from {wanted}"));
                }
            }
            ["gap-site", "at", at] => {
                if !gaps.iter().any(|g| line_col(&modules, &g.primary) == *at) {
                    failures.push(format!(
                        "missing gap at {at}; got {:?}",
                        gaps.iter()
                            .map(|g| (line_col(&modules, &g.primary), &g.message))
                            .collect::<Vec<_>>()
                    ));
                }
            }
            ["gap", what @ ..] => want_gaps.push(what.join(" ")),
            ["resource", "at", at] => want_resources.push(at.to_string()),
            ["kind", ty, k] | ["holds", ty, k] | ["size", ty, k] => {
                let Some(d) = index
                    .symbols
                    .find(ty)
                    .and_then(|n| index.definition(main, n))
                    .and_then(|d| p.decl(&index.definition_path(d)))
                else {
                    failures.push(format!("no type {ty}"));
                    continue;
                };
                let got = match words[0] {
                    "kind" => d
                        .summary
                        .kind_fixed
                        .map_or("-".into(), |k| kind_name(k).to_string()),
                    "holds" => holds_names(d.holds_rigid()),
                    _ => d
                        .summary
                        .recipe
                        .as_ref()
                        .and_then(|r| r.logical_size)
                        .map_or("-".into(), |n| n.to_string()),
                };
                if got != *k {
                    failures.push(format!("{} {ty}: want {k}, got {got}", words[0]));
                }
            }
            ["field-root", name, field, want] => {
                let decl = p
                    .decls
                    .iter()
                    .find(|d| d.path.0.name == *name)
                    .expect("named type");
                let ty = decl
                    .all_fields()
                    .find(|f| f.name == *field)
                    .expect("named field")
                    .ty;
                let got = match p.types.kind(ty) {
                    TyKind::Anchored(_, r) => match p.types.kind(*r) {
                        TyKind::Region(Root::Enclosing) => "enclosing",
                        TyKind::Region(Root::Generic(i)) => {
                            index.symbols.text(decl.generics[*i as usize].name)
                        }
                        _ => "other",
                    },
                    _ => "none",
                };
                if got != *want {
                    failures.push(format!("field root: want {want}, got {got}"));
                }
            }
            ["outlives", function, source, target] => {
                let fd = p
                    .fns
                    .iter()
                    .find(|fd| fd.path.0.name == *function)
                    .expect("named function");
                let mut root = |name: &str| {
                    let r = if let Some(i) = fd
                        .generics
                        .iter()
                        .position(|g| g.region && index.symbols.text(g.name) == name)
                    {
                        Root::Generic(i as u32)
                    } else {
                        Root::Parameter(
                            fd.params
                                .iter()
                                .position(|p| index.symbols.text(p.name) == name)
                                .expect("named region") as u32,
                        )
                    };
                    p.types.intern(TyKind::Region(r))
                };
                let (a, b) = (root(source), root(target));
                if !fd.outlives(a, b) {
                    failures.push(format!("{source} does not outlive {target}"));
                }
            }
            ["run", call, "=", want @ ..] => {
                // `evaluator-only:cranelift,llvm` (last): the named native walkers do
                // not hold this run yet and require it to disagree; the evaluator does.
                let want = match want {
                    [w @ .., m] if m.starts_with("evaluator-only:") => {
                        let engines = &m["evaluator-only:".len()..];
                        if engines
                            .split(',')
                            .any(|e| !["cranelift", "llvm"].contains(&e))
                        {
                            failures.push(format!("run {call}: unknown engine in `{m}`"));
                        }
                        w
                    }
                    w => w,
                };
                // SITES.md: a site resolves through its item's place in the source
                // table, `line:column` (columns count scalars) in the case file.
                let site = |a: hirsel_core_ir::Anchor| {
                    let path = &module.items[a.item.index()];
                    let Some(fd) = p.fns.iter().find(|fd| &fd.path == path) else {
                        return format!("{path:?}");
                    };
                    if modules[fd.at.module as usize].id != main {
                        return "core".to_string();
                    }
                    let syntax = &modules[fd.at.module as usize].syntax;
                    let at = (syntax.items[fd.at.item as usize].file_offset + a.start) as usize;
                    let before = &syntax.source[..at];
                    let line = before.matches('\n').count() + 1;
                    let column = before.rsplit('\n').next().unwrap().chars().count() + 1;
                    format!("{line}:{column}")
                };
                failures.extend(run_expect(module, &functions, call, want, &site));
                failures.extend(
                    run_expect(&shared, &functions, call, want, &site)
                        .map(|f| format!("with shared slots: {f}")),
                );
            }
            _ => failures.push(format!("unreadable expectation `{e}`")),
        }
    }
    if expects == 0 {
        failures.push("no expectations".into());
    }
    let mut got_refusals: Vec<String> = refusals
        .iter()
        .map(|r| format!("{} at {}", r.rule, line_col(&modules, &r.primary)))
        .collect();
    got_refusals.sort();
    want_refusals.sort();
    if got_refusals != want_refusals {
        let detail: Vec<_> = refusals
            .iter()
            .map(|r| {
                format!(
                    "{} at {}: {}",
                    r.rule,
                    line_col(&modules, &r.primary),
                    r.message
                )
            })
            .collect();
        failures.push(format!("refusals: want {want_refusals:?}, got {detail:?}"));
    }
    let mut got_causes: Vec<String> = refusals
        .iter()
        .filter(|r| !r.causal.is_empty())
        .map(|r| {
            // A cause in the case's own file is its line:column; one in a library module
            // is that module, whose lines the case does not own.
            let from: Vec<String> = r
                .causal
                .iter()
                .map(|c| {
                    if c.module == main {
                        line_col(&modules, c)
                    } else {
                        let m = index.module_path(c.module);
                        format!("{}/{}", m.package.name, m.modules.join("/"))
                    }
                })
                .collect();
            format!(
                "{} at {} from {}",
                r.rule,
                line_col(&modules, &r.primary),
                from.join(" ")
            )
        })
        .collect();
    got_causes.sort();
    want_causes.sort();
    if got_causes != want_causes {
        failures.push(format!("causes: want {want_causes:?}, got {got_causes:?}"));
    }
    let mut got_compared: Vec<String> = refusals
        .iter()
        .filter(|r| r.declared.is_some() || r.required.is_some())
        .map(|r| {
            format!(
                "{} at {} | {} | {}",
                r.rule,
                line_col(&modules, &r.primary),
                r.declared.as_deref().unwrap_or("-"),
                r.required.as_deref().unwrap_or("-")
            )
        })
        .collect();
    got_compared.sort();
    want_compared.sort();
    if got_compared != want_compared {
        failures.push(format!(
            "compared: want {want_compared:?}, got {got_compared:?}"
        ));
    }
    let mut got_resources: Vec<String> = resources
        .iter()
        .map(|r| line_col(&modules, &r.primary))
        .collect();
    got_resources.sort();
    want_resources.sort();
    if got_resources != want_resources {
        let detail: Vec<_> = resources.iter().map(|r| &r.message).collect();
        failures.push(format!(
            "resources: want {want_resources:?}, got {got_resources:?} {detail:?}"
        ));
    }
    let mut got_gaps: Vec<String> = gaps.iter().map(|g| g.message.to_string()).collect();
    // A gap is a kind of unbuilt construct: each kind is listed once per case.
    got_gaps.sort();
    got_gaps.dedup();
    want_gaps.sort();
    if got_gaps != want_gaps {
        failures.push(format!("gaps: want {want_gaps:?}, got {got_gaps:?}"));
    }
    failures
}

fn split_facts(module: &Module) -> Option<String> {
    use hirsel_core_ir::roles::Facts;
    let core = |f: usize| {
        module.instances[module.functions[f].instance.index()]
            .definition
            .to_string()
            .starts_with("core@")
    };
    let part = |library: bool| {
        let mut m = module.clone();
        for (f, function) in m.functions.iter_mut().enumerate() {
            if core(f) != library {
                function.body = None;
            }
        }
        m
    };
    let (library, application) = (part(true), part(false));
    let whole = Facts::new(module);
    let published = Facts::new(&library);
    let reading = Facts::program(&[&application], &published.record(&library))
        .pop()
        .unwrap();
    let linked = Facts::program(&[&library, &application], &[]);
    // A kept module's facts come back whole from its consumed rows, and never
    // from its bodies' rows alone, which leave out its declarations.
    if Facts::restore(module, &whole.consumed(module)).as_ref() != Some(&whole) {
        return Some("facts restored from consumed rows differ".into());
    }
    if module.functions.iter().any(|f| f.body.is_none())
        && Facts::restore(module, &whole.record(module)).is_some()
    {
        return Some("facts restored from body rows alone".into());
    }
    let mut reordered = whole.consumed(module);
    reordered.reverse();
    if reordered.len() > 1 && Facts::restore(module, &reordered).is_some() {
        return Some("facts restored from rows in another function order".into());
    }
    for (f, function) in module.functions.iter().enumerate() {
        let sig = &module.signatures[function.signature.index()];
        let split = if core(f) { &published } else { &reading };
        for p in 0..sig.params.len() as u32 {
            let g = FuncId(f as u32);
            let want = (whole.sealed(g, p), whole.place_blind(g, p));
            for (how, facts) in [
                ("recorded", split),
                ("linked", &linked[usize::from(!core(f))]),
            ] {
                let got = (facts.sealed(g, p), facts.place_blind(g, p));
                if got != want {
                    let name = &module.instances[function.instance.index()].definition;
                    return Some(format!(
                        "{how} facts of {name} parameter {p}: (sealed, blind) {got:?}, whole module {want:?}"
                    ));
                }
            }
        }
    }
    None
}

pub(super) fn run_expect(
    module: &Module,
    functions: &HashMap<String, FuncId>,
    call: &str,
    want: &[&str],
    site: &dyn Fn(hirsel_core_ir::Anchor) -> String,
) -> Option<String> {
    let (name, args) = call.strip_suffix(')')?.split_once('(')?;
    let Some(&f) = functions.get(name) else {
        return Some(format!("run {call}: `{name}` was not emitted"));
    };
    let sig = &module.signatures[module.functions[f.index()].signature.index()];
    let params: Vec<Ty> = sig
        .params
        .iter()
        .map(|p| match p {
            Param::Value(t) => *t,
            Param::Address { .. } => Ty::Ptr,
        })
        .collect();
    let args: Vec<&str> = args.split(',').filter(|a| !a.is_empty()).collect();
    if args.len() != params.len() {
        return Some(format!(
            "run {call}: {} arguments for {:?}",
            args.len(),
            params
        ));
    }
    let args: Vec<u64> = params
        .iter()
        .zip(&args)
        .map(|(&t, a)| eval::scalar(t, a))
        .collect();
    let (want, used) = match want {
        [w @ .., "used", n] => match n.parse::<u64>() {
            Ok(n) => (w, n),
            Err(_) => return Some(format!("run {call}: unreadable `used {n}`")),
        },
        w => (w, 0),
    };
    let (want, copies) = match want {
        [w @ .., "copies", count] => {
            let Some((width, count)) = count
                .split_once(',')
                .and_then(|(width, count)| Some((width.parse().ok()?, count.parse::<u64>().ok()?)))
            else {
                return Some(format!("run {call}: unreadable copy width and count"));
            };
            (w, Some((width, count)))
        }
        w => (w, None),
    };
    let (want, trace) = match want.iter().position(|w| *w == "trace") {
        Some(i) => (&want[..i], want.get(i + 1).copied()),
        None => (want, None),
    };
    // The evaluator stops (panics) on a malformed run; that is this run's failure, so
    // the corpus names the case it stopped in and checks the rest. Every stored NaN is
    // scrambled, so a reader of float bytes other than `reinterpret` misses its run
    // line (FORM #97).
    let (report, work) = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let nans = eval::Nans::Scrambled;
        match copies {
            Some((width, _)) => eval::run_counting_copies(module, f, &args, u64::MAX, nans, width),
            None => eval::run_nans(module, f, &args, u64::MAX, nans),
        }
    })) {
        Ok(report) => report,
        Err(stop) => {
            let why = stop
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| stop.downcast_ref::<&str>().copied())
                .unwrap_or("no message");
            return Some(format!("run {call}: the evaluator stopped: {why}"));
        }
    };
    if let Some((width, expected)) = copies
        && work.copies != expected
    {
        return Some(format!(
            "run {call}: want {expected} copies of {width} bytes, got {}",
            work.copies
        ));
    }
    if let Some(trace) = trace {
        let t: Vec<u64> = trace.split(',').map(|x| x.parse().unwrap()).collect();
        if report.trace != t {
            return Some(format!(
                "run {call}: want trace {t:?}, got {:?}",
                report.trace
            ));
        }
    }
    if let ["fail", causes] = want
        && causes.contains('@')
    {
        // `Kind@line:column` per cause: the site the evaluator reports (SITES.md).
        let eval::Outcome::Failed(kinds) = &report.outcome else {
            return Some(format!(
                "run {call}: want fail {causes}, got {:?}",
                report.outcome
            ));
        };
        let got: Vec<String> = kinds
            .iter()
            .zip(&report.sites)
            .map(|(k, s)| format!("{k}@{}", s.map_or("none".to_string(), site)))
            .collect();
        let got = got.join(",");
        return (got != *causes).then(|| format!("run {call}: want fail {causes}, got fail {got}"));
    }
    // Reading a float result observes it as `reinterpret` does.
    let got = match report.outcome {
        eval::Outcome::Returned(xs) => eval::Outcome::Returned(
            sig.results
                .iter()
                .zip(xs)
                .map(|(&t, x)| eval::observed(t, x))
                .collect(),
        ),
        failed => failed,
    };
    if matches!(got, eval::Outcome::Returned(_)) && report.used != used {
        return Some(format!(
            "run {call}: {} bytes still admitted, want {used}",
            report.used
        ));
    }
    let expected = match want {
        ["fail", causes] => eval::Outcome::Failed(causes.split(',').map(str::to_string).collect()),
        [rs] => {
            let rs: Vec<&str> = rs.split(',').filter(|r| !r.is_empty()).collect();
            if rs.len() != sig.results.len() {
                return Some(format!("run {call}: want {} results", sig.results.len()));
            }
            eval::Outcome::Returned(
                sig.results
                    .iter()
                    .zip(&rs)
                    .map(|(&t, r)| eval::observed(t, eval::scalar(t, r)))
                    .collect(),
            )
        }
        [] => eval::Outcome::Returned(vec![]),
        _ => return Some(format!("run {call}: unreadable result")),
    };
    (got != expected).then(|| format!("run {call}: want {expected:?}, got {got:?}"))
}

fn cases(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("cases dir")
        .map(|e| e.expect("entry").path())
        .collect();
    entries.sort();
    for e in entries {
        if e.is_dir() {
            cases(&e, out);
        } else if e.extension().is_some_and(|x| x == "hirsel") {
            out.push(e);
        }
    }
}

#[test]
fn generic_allocation_layout() {
    let failures = run_case(&root().join("cases/body/generic-allocation-layout.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn call_region_callback_substitution() {
    for case in ["call-region-callbacks", "call-region-callback-mismatch"] {
        let failures = run_case(&root().join(format!("cases/body/{case}.hirsel")));
        assert!(failures.is_empty(), "{case}: {failures:?}");
    }
}

#[test]
fn call_region_grouped_binders() {
    let failures = run_case(&root().join("cases/body/call-region-grouped.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn anchored_map_rows() {
    for case in ["map-anchored", "mut-scoped-regional-call"] {
        let failures = run_case(&root().join(format!("cases/body/{case}.hirsel")));
        assert!(failures.is_empty(), "{case}: {failures:?}");
    }
}

#[test]
fn scalar_constants_are_not_callable() {
    let failures = run_case(&root().join("cases/body/call-constant-refused.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn scalar_key_carriage() {
    let failures = run_case(&root().join("cases/body/scalar-key-carriage.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn scalar_task_entry_conventions() {
    let failures = run_case(&root().join("cases/body/scalar-task-entry.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn anchored_instance_key() {
    let failures = run_case(&root().join("cases/body/instance-anchored.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn list_replacement() {
    let failures = run_case(&root().join("cases/body/list-set-static-gap.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn list_slicing() {
    let failures = run_case(&root().join("cases/body/list-slice.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn list_shrink() {
    for case in ["list-pop.hirsel", "list-clear.hirsel"] {
        let failures = run_case(&root().join("cases/body").join(case));
        assert!(failures.is_empty(), "{case}: {failures:?}");
    }
}

#[test]
fn list_growth() {
    for case in [
        "list-reserve.hirsel",
        "list-reserve-zero-gap.hirsel",
        "list-push.hirsel",
        "list-append.hirsel",
        "list-append-alias.hirsel",
    ] {
        let failures = run_case(&root().join("cases/body").join(case));
        assert!(failures.is_empty(), "{case}: {failures:?}");
    }
}

#[test]
fn list_lookup() {
    let failures = run_case(&root().join("cases/body/list-get.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn list_allocation() {
    let failures = run_case(&root().join("cases/body/list-with-capacity.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn inferred_callable_kind_fits_emit_their_carriers() {
    let mut failures = run_case(&root().join("cases/body/callable-generic-fit.hirsel"));
    failures.extend(run_case(
        &root().join("cases/body/callable-generic-fit-refused.hirsel"),
    ));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn generic_static_mut_calls_keep_direct_call_loans() {
    let mut failures = run_case(&root().join("cases/body/callable-static-mut.hirsel"));
    failures.extend(run_case(
        &root().join("cases/body/callable-static-mut-refused.hirsel"),
    ));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn recursive_type_instances_are_finite() {
    let mut failures = vec![];
    for case in [
        "codec-recursion-refused",
        "type-recursion-growing",
        "type-recursion-finite",
    ] {
        failures.extend(run_case(&root().join(format!("cases/body/{case}.hirsel"))));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let source =
        std::fs::read_to_string(root().join("cases/body/type-recursion-finite.hirsel")).unwrap();
    let checked = Checked::source(&source, None);
    let roots = checked
        .product
        .definitions
        .values()
        .filter(|d| {
            d.definition.package_is("app", "1")
                && d.check.function.is_some()
                && d.check
                    .template
                    .as_ref()
                    .is_none_or(|t| t.parameters.is_empty())
        })
        .map(|d| Identity::plain(d.definition.clone()));
    let units = checked.product.units(roots, 65_536).unwrap();
    let modules = units.modules().unwrap();
    let names: Vec<_> = modules
        .iter()
        .flat_map(|u| u.module.types.iter().map(|s| s.name.as_str()))
        .collect();
    for name in [
        "app@1/main/Swap[u64, bool]",
        "app@1/main/Swap[bool, u64]",
        "app@1/main/Rotate[u64, u8, bool]",
        "app@1/main/Rotate[u8, bool, u64]",
        "app@1/main/Rotate[bool, u64, u8]",
    ] {
        assert!(names.contains(&name), "missing instance {name}: {names:?}");
    }
}

#[test]
fn codec_typed_static_registration() {
    let mut failures = run_case(&root().join("cases/body/callable-codec-register.hirsel"));
    failures.extend(run_case(
        &root().join("cases/body/callable-codec-register-refused.hirsel"),
    ));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn codec_overrides_and_tombstones() {
    let mut report = vec![];
    for name in [
        "schema-accepted",
        "schema-refused",
        "via-refused",
        "via-generic-owners",
        "via-generic-owner-refused",
    ] {
        report.extend(
            run_case(&root().join(format!("cases/body/codec-{name}.hirsel")))
                .into_iter()
                .map(|f| format!("{name}: {f}")),
        );
    }
    let source =
        std::fs::read_to_string(root().join("cases/body/codec-schema-accepted.hirsel")).unwrap();
    let (index, inputs) = program_inputs(&source);
    let p = Program::declare(&index, &inputs);
    let shape = p.decls.iter().find(|d| d.path.0.name == "Shape").unwrap();
    assert_eq!((shape.wire_position(0), shape.wire_position(1)), (0, 3));
    assert_eq!(shape.codec().unwrap().removed.len(), 2);
    let first = p
        .decls
        .iter()
        .find(|d| d.path.0.name == "DeletedFirst")
        .unwrap();
    assert_eq!(first.wire_position(0), 1);
    assert!(report.is_empty(), "{}", report.join("\n"));
}

#[test]
fn codec_bounds_and_field_defaults() {
    let mut report = vec![];
    for name in [
        "bound-accepted",
        "bound-refused",
        "defaults-accepted",
        "defaults-refused",
        "defaults-expressions-refused",
    ] {
        let failures = run_case(&root().join(format!("cases/body/codec-{name}.hirsel")));
        report.extend(
            failures
                .into_iter()
                .map(|failure| format!("{name}: {failure}")),
        );
    }
    assert!(report.is_empty(), "{}", report.join("\n"));
}

fn recovered_header_cases() -> Vec<String> {
    let mut failures = vec![];
    for (name, repaired) in [("plain", false), ("plain", true), ("store", false)] {
        let files = tempfile::tempdir().unwrap();
        let path = files.path().join("main.hirsel");
        let mut source =
            std::fs::read_to_string(root().join(format!("fixtures/ice-places-358-{name}.hirsel")))
                .unwrap();
        if repaired {
            source = source
                .replace("let mut bytes:", "let mut enc:")
                .replace("mut bytes)", "mut enc)")
                .replace("decode(c, bytes)", "decode(c, enc)");
        }
        std::fs::write(&path, source).unwrap();
        let mut packages = vec![
            PackageManifest::directory(&core_dir(), package("core"), true).unwrap(),
            PackageManifest::directory(&repo().join("core/std"), package("std"), false).unwrap(),
        ];
        let (owner, module, entry) = if name == "store" {
            let manifest: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(repo().join("core/programs/store/manifest.json")).unwrap(),
            )
            .unwrap();
            let mut store: PackageManifest =
                serde_json::from_value(manifest["packages"][0].clone()).unwrap();
            for source in &mut store.sources {
                source.file = repo().join(&source.file);
            }
            store.sources.push(ModuleSource {
                file: path,
                modules: vec!["ice_places_358_store".into()],
                signatures: false,
            });
            packages.push(store);
            ("store", "ice_places_358_store", None)
        } else {
            packages.push(PackageManifest {
                package: package("app"),
                dependencies: vec![package("core"), package("std")],
                sources: vec![ModuleSource {
                    file: path,
                    modules: vec!["main".into()],
                    signatures: false,
                }],
            });
            ("app", "main", DefPath::from_full("app@1/main/main"))
        };
        let checked = Checked::check(
            CorpusManifest {
                core: package("core"),
                packages,
            },
            files,
            entry,
        );
        let product = &checked.product;
        let label = if repaired { "plain/enc" } else { name };
        eprintln!(
            "recovery corpus {label}: accepted={}, diagnostics={}",
            product.accepted(),
            product.diagnostics.len()
        );
        if product.accepted() != repaired {
            failures.push(format!(
                "{label}: expected accepted={repaired}; {:?}; {:?}",
                product.diagnostics, product.incomplete
            ));
        }
        if !repaired
            && !product.diagnostics.iter().any(|r| {
                let at = product.index.module_path(r.primary.module);
                at.package.name == owner && at.modules.iter().map(String::as_str).eq([module])
            })
        {
            failures.push(format!(
                "{label}: missing diagnostic in the refused source module"
            ));
        }
    }
    failures
}

#[test]
fn construction_in_place() {
    let failures = run_case(&root().join("cases/body/construction-in-place.hirsel"));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn flow_transform_rejects() {
    let failures = run_case(&root().join("cases/body/flow-transform-rejects.hirsel"));
    assert!(failures.is_empty(), "{}", failures.join("\n"));

    use hirsel_core_driver::{CheckRequest, Driver, LibraryInput, Meter};
    let files = tempfile::tempdir().unwrap();
    let source = "fn body(take s: u64, k: u64, x: u64) -> (u64, Option[u64]) { (s, Some(dep.api.h(x))) }\n\
        fn wire(mut f: Flow, output: Into[u64]) -> Result[Into[u64], Wire] {\n\
            flow.stage(mut f, output, 1, bytes 0, steps 100, 0, code.describe(body))\n\
        }";
    let app = files.path().join("main.hirsel");
    let dep = files.path().join("dep.hirsel");
    std::fs::write(&app, source).unwrap();
    let mut inputs = crate::source_manifest(source, app, &core_dir(), "main");
    inputs
        .packages
        .last_mut()
        .unwrap()
        .dependencies
        .push(package("dep"));
    inputs.packages.push(PackageManifest {
        package: package("dep"),
        dependencies: vec![package("core")],
        sources: vec![ModuleSource {
            file: dep.clone(),
            modules: vec!["api".into()],
            signatures: false,
        }],
    });
    let mut driver = Driver::default();
    let mut check = |helper| {
        std::fs::write(&dep, helper).unwrap();
        driver
            .check(
                CheckRequest {
                    inputs: inputs.clone(),
                    store: files.path().join("store"),
                    entry: None,
                    library: LibraryInput::Sources,
                    manifest: None,
                },
                &mut Meter::new(false),
            )
            .unwrap()
    };
    let before = check("pub fn h(x: u64) -> u64 { x + 1 }");
    assert!(before.diagnostics.is_empty(), "{:?}", before.diagnostics);
    let after = check("pub fn h(x: u64) -> u64 { let _unused: Option[Sender[u8]] = None; x + 1 }");
    let [bad] = after.diagnostics.as_slice() else {
        panic!("{:?}", after.diagnostics)
    };
    assert_eq!(bad.rule, "CONC-14");
    assert!(
        bad.message.contains("dep@1/api/h") && bad.message.contains("Sender[u8]"),
        "{bad:?}"
    );
    let module = after.index.module_path(bad.primary.module);
    assert_eq!(module.package, package("app"));
    assert!(
        after
            .rechecked
            .iter()
            .all(|path| path.package_is("dep", "1"))
    );
}

#[test]
fn corpus() {
    let mut paths = vec![];
    cases(&root().join("cases"), &mut paths);
    assert!(!paths.is_empty());
    let mut report = vec![];
    for path in &paths {
        let failures = std::panic::catch_unwind(|| run_case(path)).unwrap_or_else(|fault| {
            vec![
                fault
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| fault.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "non-string corpus fault".into()),
            ]
        });
        for f in failures {
            report.push(format!(
                "{}: {f}",
                path.strip_prefix(root()).unwrap_or(path).display()
            ));
        }
    }
    report.extend(recovered_header_cases());
    assert!(report.is_empty(), "\n{}", report.join("\n"));
}

#[test]
fn float_contract() {
    let failures = run_case(&root().join("cases/body/float-unary.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn literal_aggregate_call_inference() {
    for case in ["generic-instances", "call-undetermined"] {
        let failures = run_case(&root().join(format!("cases/body/{case}.hirsel")));
        assert!(failures.is_empty(), "{case}: {failures:?}");
    }
}

#[test]
fn list_set_transfers_the_indexed_element() {
    let checked = Checked::source(
        "fn set_one(mut xs: List[u64]) -> u64 { list.set(mut xs, 1, 9) }\nfn probe() -> u64 { let mut xs: List[u64] = result.unwrap(list.with_capacity(2)); result.unwrap(list.push(mut xs, 5u64)); result.unwrap(list.push(mut xs, 7u64)); let old = set_one(mut xs); xs[0] * 10000 + old * 100 + xs[1] }",
        None,
    );
    assert!(
        checked.product.accepted(),
        "{:?} {:?}",
        checked.product.diagnostics,
        checked.product.incomplete
    );
    let body = &checked.product.definitions[&DefPath::from_full("app@1/main/set_one").unwrap()];
    let ir = body.ir().unwrap();
    assert!(
        matches!(
            ir.signatures[ir.functions[body.check.function.unwrap().index()]
                .signature
                .index()]
            .params[0],
            Param::Address {
                place: hirsel_core_ir::PlaceTy::Declared(_),
                ..
            }
        ),
        "mut List ABI"
    );
    let identity = Identity::plain(DefPath::from_full("app@1/main/probe").unwrap());
    let units = checked.product.units([identity.clone()], 65_536).unwrap();
    let (module, function) =
        hirsel_core_materialize::join(&units.modules().unwrap(), &identity).unwrap();
    wf::module(&module).unwrap();
    let report = eval::run(&module, function, &[], u64::MAX);
    assert_eq!(report.outcome, eval::Outcome::Returned(vec![50709]));
    assert_eq!(report.used, 0);
    for case in ["list-set-bounds.hirsel", "list-set-static-gap.hirsel"] {
        let failures = run_case(&root().join("cases/body").join(case));
        assert!(failures.is_empty(), "{case}: {failures:?}");
    }
}

#[test]
fn aggregate_constants_publish_values_and_use_read_only_tables() {
    let failures = run_case(&root().join("cases/body/constant-aggregates.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn constant_literals_are_not_type_grants() {
    let source =
        "const P: f64 = 1.5e-01\nconst N: i64 = -7\nconst B: bool = true\nconst C: char = 'x'";
    let (index, inputs) = program_inputs(source);
    let p = Program::declare(&index, &inputs);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    assert!(p.gaps.is_empty(), "{:?}", p.gaps);
    for (name, bits) in [
        ("P", 0x3fc3333333333333),
        ("N", (-7i64) as u64),
        ("B", 1),
        ("C", 120),
    ] {
        let c = p.constants.iter().find(|c| c.path.0.name == name).unwrap();
        assert_eq!(c.value.unwrap().data, ConstData::Bits(bits), "{name}");
    }
    let failures = run_case(&root().join("cases/body/constant-literals.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn private_variant_span_reads_follow_the_logical_encoding() {
    let failures = run_case(&root().join("cases/body/span-private-variant.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn packed_composite_views_escape_through_caller_destinations() {
    let failures = run_case(&root().join("cases/body/packed-escaping.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn generic_order_follows_each_shape_and_signedness() {
    let failures = run_case(&root().join("cases/body/ordered-generic.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn borrowed_generic_order_selection() {
    let failures = run_case(&root().join("cases/body/ordered-cost.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn task_send_and_waits_spec_examples() {
    let mut all = vec![];
    for name in [
        "tasks-send-record",
        "tasks-send-spawn",
        "tasks-mutable-static",
        "tasks-waits-refused",
        "tasks-send-result",
        "tasks-waits-accepted",
    ] {
        all.extend(
            run_case(&root().join(format!("cases/body/{name}.hirsel")))
                .into_iter()
                .map(|f| format!("{name}: {f}")),
        );
    }
    assert!(all.is_empty(), "{all:#?}");
}

#[test]
fn generic_order_reads_typed_locator_recipes() {
    let failures = run_case(&root().join("cases/body/ordered-stable.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
    let failures = run_case(&root().join("cases/body/ordered-compact.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn refused_pattern_binders_do_not_add_implementation_gaps() {
    let failures = run_case(&root().join("cases/body/refused-pattern-use.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn loop_callbacks_preserve_places_and_modes() {
    let failures = run_case(&root().join("cases/body/loop-capture-entry.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
    let failures = run_case(&root().join("cases/body/loop-capture-refused.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn uninitialized_take_names_use_the_normal_slot_transition() {
    let failures = run_case(&root().join("cases/body/uninit-take-name.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn indexing_has_one_closed_operand_rule() {
    for case in [
        "index-types-refused",
        "index-sequence-refused",
        "assignment-expression-refused",
    ] {
        let failures = run_case(&root().join(format!("cases/body/{case}.hirsel")));
        assert!(failures.is_empty(), "{failures:#?}");
    }
}

#[test]
fn closed_comparisons_derive_collection_payloads() {
    let failures = run_case(&root().join("cases/body/compare-collections.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn word_load_and_bit_counts_follow_width_and_bounds() {
    use hirsel_core_ir::{Inst, RaiseCause, Step, Terminator};
    let failures = run_case(&root().join("cases/body/word-bit-counts.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
    let checked = Checked::source("fn f(b:Bytes,at:u64)->u64 { list.word_le(b,at) }", None);
    let published = &checked.product.definitions[&DefPath::from_full("app@1/main/f").unwrap()];
    assert!(published.check.refusals.is_empty() && published.incomplete.is_empty());
    let ir = published.ir().unwrap();
    let body = ir.functions[published.check.function.unwrap().index()]
        .body
        .as_ref()
        .unwrap();
    let at = body.blocks[0].params.get(&body.pools.values).unwrap()[1];
    assert_eq!(
        body.terminators
            .iter()
            .filter(|t| matches!(
                t,
                Terminator::Raise {
                    cause: RaiseCause::Bounds,
                    ..
                }
            ))
            .count(),
        1,
        "one explicit range check, no per-byte checks"
    );
    assert_eq!(
        body.insts
            .iter()
            .filter(|i| matches!(i,
                Inst::Read { address, .. } if body.addresses[address.index()].steps
                    .get(&body.pools.steps).unwrap().contains(&Step::Byte(at))
            ))
            .count(),
        1,
        "one full-width unaligned read"
    );
    assert!(
        !body.insts.iter().any(|i| matches!(i, Inst::Call { .. })),
        "no kernel call"
    );
}

#[test]
fn holder_payloads_are_read_where_a_view_is_expected() {
    let failures = run_case(&root().join("cases/body/holder-payload-read.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
    let failures = run_case(&root().join("cases/body/holder-payload-root-refused.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
    let failures = run_case(&root().join("cases/body/holder-spec-refused.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
    let failures = run_case(&root().join("cases/body/holder-payload-refused.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn loop_scoped_slots_preserve_their_anchor_sets() {
    let mut failures = vec![];
    for case in [
        "loop-anchor-refused.hirsel",
        "loop-anchor-lifetimes.hirsel",
        "loop-anchor-accepted.hirsel",
        "loop-declared-anchors.hirsel",
        "loop-declared-anchors-refused.hirsel",
    ] {
        failures.extend(run_case(&root().join("cases/body").join(case)));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn map_update_receiver_overlap() {
    let failures = run_case(&root().join("cases/own/map-update-receiver-overlap.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn captured_callback_cases() {
    let mut report = vec![];
    for name in [
        "callable-captured-callback",
        "callable-captured-callback-alias",
        "callable-captured-callback-inner-alias",
        "callable-captured-callback-lifetime",
        "callable-captured-callback-list-alias",
    ] {
        let failures = run_case(&root().join("cases/body").join(format!("{name}.hirsel")));
        report.extend(failures.into_iter().map(|f| format!("{name}: {f}")));
    }
    assert!(report.is_empty(), "{report:?}");
}

#[test]
fn owning_callable_scoped_views() {
    let mut failures = vec![];
    for case in [
        "callable-own-scoped.hirsel",
        "callable-own-scoped-freeze.hirsel",
        "callable-own-scoped-lifetime.hirsel",
        "callable-own-scoped-return-refused.hirsel",
        "callable-own-scoped-temporary-return-refused.hirsel",
        "callable-own-scoped-mut-gap.hirsel",
    ] {
        failures.extend(run_case(&root().join("cases/body").join(case)));
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn scoped_callback_mutation_contracts() {
    let mut failures = vec![];
    for name in [
        "callable-mut-reset",
        "callable-mut-replace-refused",
        "callable-mut-wrap-refused",
        "callable-mut-reset-refused",
        "callable-mut-part-refused",
        "callable-mut-captured-wrap-refused",
    ] {
        failures.extend(run_case(
            &root().join("cases/body").join(format!("{name}.hirsel")),
        ));
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn mut_scoped_parameters_keep_the_callers_anchors() {
    let mut failures = vec![];
    for case in [
        "mut-scoped-anchors.hirsel",
        "mut-scoped-anchors-refused.hirsel",
        "mut-scoped-forward-refused.hirsel",
        "mut-result-anchors.hirsel",
    ] {
        failures.extend(run_case(&root().join("cases/body").join(case)));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn temporary_index_views_keep_the_reached_owner() {
    let failures = run_case(&root().join("cases/body/index-temporary-owner.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn aggregate_callback_cases() {
    let mut failures = vec![];
    for name in [
        "callable-aggregate",
        "callable-aggregate-alias",
        "callable-aggregate-destructure-alias",
        "callable-aggregate-lifetime",
        "callable-aggregate-captured",
        "callable-aggregate-overwrite",
        "callable-aggregate-record-alias",
        "callable-aggregate-overwrite-alias",
        "callable-aggregate-loop-fields",
        "callable-aggregate-viewed",
        "callable-aggregate-captured-store-gap",
        "callable-aggregate-mut-argument-gap",
        "callable-aggregate-owned-projection",
        "callable-aggregate-owned-projection-alias",
    ] {
        failures.extend(
            run_case(&root().join("cases/body").join(format!("{name}.hirsel")))
                .into_iter()
                .map(|f| format!("{name}: {f}")),
        );
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn map_clone_cases() {
    let failures = run_case(&root().join("cases/body/map-clone.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn packed_writer_reservation_boundaries() {
    let failures = run_case(&root().join("cases/body/packed-writer-reservation.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn packed_scan_offsets_preserve_source_failures() {
    let failures = run_case(&root().join("cases/body/packed-scan-offsets.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn callable_scoped_result_regions() {
    let mut failures =
        run_case(&root().join("cases/body/callable-scoped-region-result-escape.hirsel"));
    failures.extend(run_case(
        &root().join("cases/body/callable-scoped-region-result.hirsel"),
    ));
    failures.extend(run_case(
        &root().join("cases/body/callable-scoped-region-result-fit-refused.hirsel"),
    ));
    failures.extend(run_case(&root().join(
        "cases/body/callable-scoped-region-result-literal-refused.hirsel",
    )));
    failures.extend(run_case(
        &root().join("cases/body/callable-scoped-region-result-alias-escape.hirsel"),
    ));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn closure_invocation_region_headers() {
    let mut failures =
        run_case(&root().join("cases/body/callable-closure-header-anchor-gap.hirsel"));
    failures.extend(run_case(
        &root().join("cases/body/callable-closure-header-region-refused.hirsel"),
    ));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn for_carriers_share_the_loop_head_bound() {
    let mut failures = run_case(&root().join("cases/body/for-carrier-bounds.hirsel"));
    failures.extend(run_case(&root().join("cases/body/for-freeze.hirsel")));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn guarded_prefix_checks_preserve_failure_order() {
    let failures = run_case(&root().join("cases/body/guarded-byte-prefix.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn dynamic_plain_callable_fits() {
    let mut failures = vec![];
    for name in [
        "callable-dynamic-plain-fit",
        "callable-dynamic-plain-mut-own-refused",
        "callable-dynamic-plain-mut-scoped-refused",
    ] {
        failures.extend(run_case(
            &root().join("cases/body").join(format!("{name}.hirsel")),
        ));
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn scoped_callable_return_contracts() {
    let mut failures = vec![];
    for name in [
        "callable-returned-environment-refused",
        "callable-returned-call-refused",
        "callable-returned-alias-refused",
        "callable-returned-descriptor-refused",
        "callable-returned-descriptors",
        "callable-outer-environment",
    ] {
        failures.extend(run_case(
            &root().join("cases/body").join(format!("{name}.hirsel")),
        ));
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn scoped_callable_environment_placement() {
    let mut failures = vec![];
    for name in [
        "callable-scoped-placement",
        "callable-written-anchor",
        "callable-written-anchor-refused",
        "callable-scoped-mode-gap",
        "callable-mut-captured-reset-gap",
        "callable-captured-replaced-refused",
    ] {
        failures.extend(run_case(
            &root().join("cases/body").join(format!("{name}.hirsel")),
        ));
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn captured_callbacks_use_current_descriptors() {
    let mut failures = run_case(&root().join("cases/body/callable-captured-current.hirsel"));
    failures.extend(run_case(
        &root().join("cases/body/callable-captured-replaced-refused.hirsel"),
    ));
    failures.extend(run_case(
        &root().join("cases/body/callable-local-nested-mode-refused.hirsel"),
    ));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn marked_scoped_parameters_use_value_and_declared_places() {
    let mut failures = vec![];
    for case in [
        "callable-region-mut",
        "callable-scoped-parameters",
        "callable-region-store-refused",
        "callable-region-local-store-refused",
        "callable-region-take-result",
    ] {
        failures.extend(run_case(
            &root().join("cases/body").join(format!("{case}.hirsel")),
        ));
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn isolate_and_code_describe_rules() {
    let mut failures = run_case(&root().join("cases/body/callable-isolate-code.hirsel"));
    failures.extend(run_case(
        &root().join("cases/body/callable-code-describe.hirsel"),
    ));
    failures.extend(run_case(
        &root().join("cases/body/callable-static-fn-m2b.hirsel"),
    ));
    failures.extend(run_case(
        &root().join("cases/body/callable-static-fn-generic.hirsel"),
    ));
    failures.extend(run_case(
        &root().join("cases/body/callable-static-fn-open-mut.hirsel"),
    ));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn isolate_lowering_rules() {
    let failures = run_case(&root().join("cases/body/callable-isolate-lowering.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn isolate_anchor_attack_regressions() {
    let failures = run_case(&root().join("cases/body/callable-attack-945.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn isolate_parameter_region_anchor_regressions() {
    let failures = run_case(&root().join("cases/body/callable-attack-949.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn isolate_transitive_attack_regressions() {
    let failures = run_case(&root().join("cases/body/callable-isolate-attack-947.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn isolate_outer_map_generic_calls() {
    let failures =
        run_case(&repo().join("crates/hirsel-core-front/src/testdata/callable-isolate-map.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn isolate_outer_generic_map_call() {
    let failures = run_case(
        &repo().join("crates/hirsel-core-front/src/testdata/callable-isolate-map-generic.hirsel"),
    );
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn written_anchor_activation_rules() {
    let failures = run_case(&root().join("cases/body/callable-written-anchor-activation.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn prog4_signature_and_reference_cases() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/front/cases/body");
    for name in [
        "test-functions",
        "test-signatures-refused",
        "test-references-refused",
    ] {
        let path = root.join(format!("{name}.hirsel"));
        assert_eq!(run_case(&path), Vec::<String>::new(), "{name}");
    }
}

#[test]
fn word_store_preserves_bytes_and_refusal_custody() {
    let failures = run_case(&root().join("cases/body/list-put-word.hirsel"));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn nested_list_mutation_uses_existing_writable_index_places() {
    for name in [
        "list-nested-mutate",
        "list-nested-view-refused",
        "list-nested-take-refused",
        "index-types-refused",
    ] {
        let failures = run_case(&root().join(format!("cases/body/{name}.hirsel")));
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}

#[test]
fn declaration_members_and_attributes_are_unique() {
    for case in [
        "declaration-attributes-refused",
        "declaration-fields-refused",
        "declaration-variants-refused",
        "declaration-members-unique",
    ] {
        let failures = run_case(&root().join(format!("cases/body/{case}.hirsel")));
        assert!(failures.is_empty(), "{case}: {failures:?}");
    }
}

#[test]
fn step_bounds_rules() {
    let mut failures = run_case(&root().join("cases/body/step-bounds.hirsel"));
    failures.extend(run_case(
        &root().join("../../crates/hirsel-core-rt/fixtures/order/step-bounds.hirsel"),
    ));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn static_bytes_preserve_budget_law() {
    let failures = run_case(&root().join("cases/rules/MEM-28-static-bytes.hirsel"));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn simd_sem_11_evaluator() {
    for name in [
        "fixtures/simd-operations.hirsel",
        "cases/body/simd-refusals.hirsel",
    ] {
        let failures = run_case(&root().join(name));
        assert!(failures.is_empty(), "{name}: {failures:#?}");
    }
    let (program, names) = crate::SourceProgram::lower(
        include_str!("../../../core/front/fixtures/simd-operations.hirsel"),
        None,
    );
    assert!(program.functions[names["inferred"].index()].versioned);
    assert!(!program.functions[names["closure_local"].index()].versioned);
    assert!(program.functions.iter().any(|f| {
        f.versioned
            && program.instances[f.instance.index()]
                .definition
                .0
                .name
                .starts_with("closure_local$closure")
    }));
    let text = hirsel_core_ir::text::print(&program);
    let parsed = hirsel_core_ir::text::parse(&text).unwrap();
    assert_eq!(hirsel_core_ir::text::print(&parsed), text);
}
