//! The case corpus under `core/front/cases`: each file's `// expect:` lines are its
//! hand-derived oracle (see `core/front/README.md`).

use crate::*;
use hirsel_core_ids::PackageId;
use hirsel_core_ir::{FuncId, Module, Param, Ty, eval, wf};
use hirsel_core_resolve::{ModuleSource, PackageManifest};
use hirsel_core_syntax::{ParseMode, Symbols, parse};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/front")
}

/// The one core package, as every program checks against it.
fn core_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/core")
}

fn package(name: &str) -> PackageId {
    PackageId {
        name: name.into(),
        version: "1".into(),
    }
}

/// The core package and one application module (core's `core` module is the prelude).
pub(crate) fn program_inputs(source: &str) -> (NameIndex, Vec<ModuleInput>) {
    inputs_with_core(source, &core_dir())
}

fn inputs_with_core(source: &str, core_dir: &Path) -> (NameIndex, Vec<ModuleInput>) {
    let mut symbols = Symbols::default();
    let core = PackageManifest::directory(core_dir, package("core"), true).unwrap();
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
        .unwrap()
        .into_iter()
        .map(|m| {
            assert!(
                m.parsed.diagnostics.is_empty(),
                "{:?}",
                m.parsed.diagnostics
            );
            m.parsed.syntax
        })
        .collect::<Vec<_>>();
    let parsed = parse(source, ParseMode::Source, &mut symbols);
    let diagnostics = parsed.diagnostics;
    syntax.push(parsed.syntax);
    let mut index = NameIndex::new(std::sync::Arc::new(symbols));
    let ids = index.add_manifests(&[core, app]);
    let modules = ids
        .into_iter()
        .zip(syntax)
        .map(|(id, syntax)| ModuleInput { id, syntax })
        .collect::<Vec<_>>();
    for d in &diagnostics {
        index
            .diagnostics
            .push(Refusal::syntax(modules.last().unwrap().id, d));
    }
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
    (index, modules)
}

fn line_col(modules: &[ModuleInput], site: &Site) -> String {
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
    let col = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
    format!("{line}:{col}")
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Plain => "plain",
        Kind::Own => "own",
        Kind::Stable => "stable",
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
        (H_STABLE, "stable"),
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

/// Run one case; returns its failures.
/// Materialization is substitution alone (FORM #56): a concrete row (no `$i` in its
/// name) never holds a rigid parameter's placeholder row in any field. Walks every
/// FieldTy variant on its own, independent of the substitution's field mapper.
fn concrete_rows_reaching_rigid(module: &Module) -> Vec<(String, String)> {
    use hirsel_core_layout::{Body as ShapeBody, FieldTy};
    fn refs(ty: &FieldTy, out: &mut Vec<u32>) {
        match ty {
            FieldTy::Inline(r)
            | FieldTy::Box(r)
            | FieldTy::Rc(r)
            | FieldTy::Arc(r)
            | FieldTy::Stable(r)
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
            | FieldTy::Text => {}
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

fn run_case(path: &Path) -> Vec<String> {
    let source = std::fs::read_to_string(path).expect("case reads");
    let (index, modules) = program_inputs(&source);
    let mut p = Program::declare(&index, &modules);
    let main = modules.last().unwrap().id;
    p.entry = index
        .symbols
        .find("main")
        .and_then(|s| index.definition(main, s))
        .map(|d| index.definition_path(d));
    // Every body of the application module, checked once, in order.
    let mut emitter = Emitter::new();
    let mut refusals = p.refusals.clone();
    let mut gaps = p.gaps.clone();
    let mut functions = HashMap::new();
    for (f, fd) in p.fns.iter().enumerate() {
        if fd.body.is_none()
            || (index.module_package(fd.site.module) != index.module_package(main)
                && !(fd.path.package_is("core", "1") && fd.path.0.modules == ["map"]))
        {
            continue;
        }
        let c = p.check_lower(f, &mut emitter);
        refusals.extend(c.refusals);
        gaps.extend(c.gaps);
        if let Some(id) = c.function {
            functions.insert(fd.path.0.name.clone(), id);
        }
    }
    emitter
        .materialize_instances()
        .expect("materialize checked generic bodies");
    let module = &emitter.module;
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
                    key.types,
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
        use hirsel_core_ir::{AllocKind, Inst};
        let mut checked = 0;
        for f in &module.functions {
            let key = &module.instances[f.instance.index()];
            let expected = match (key.definition.0.name.as_str(), key.types.as_slice()) {
                ("hold", [t]) if t == "u32" => (44, 16, 8),
                ("hold", [t]) if t == "()" => (40, 8, 8),
                ("hold", [t]) if t == "app@1/main/Big" => (104, 72, 8),
                ("hold_only", [t]) if t == "u32" => (36, 4, 4),
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
                        kind: AllocKind::Resource { .. },
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
            module
                .functions
                .iter()
                .find(|f| {
                    let key = &module.instances[f.instance.index()];
                    key.definition.0.name == name
                        && if generic {
                            key.types == ["u64"]
                        } else {
                            key.types.is_empty()
                        }
                })
                .unwrap()
        };
        for (generic, closed) in [("less", "closed_less"), ("min", "closed_scalar")] {
            let a = find(generic, true);
            let b = find(closed, false);
            assert_eq!(
                module.signatures[a.signature.index()],
                module.signatures[b.signature.index()]
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
    let all_emitted = refusals.is_empty() && gaps.is_empty();
    let mut want_refusals = vec![];
    let mut want_gaps = vec![];
    let mut failures = vec![];
    let mut expects = 0;
    if all_emitted && let Err(e) = wf::module(module) {
        failures.push(format!("emitted CoreIR is not well formed: {e:?}"));
    }
    for line in source.lines() {
        let Some(e) = line.trim().strip_prefix("// expect: ") else {
            continue;
        };
        expects += 1;
        let words: Vec<&str> = e.split_whitespace().collect();
        match words.as_slice() {
            ["accept"] => {}
            ["refuse", rule, "at", at] => want_refusals.push(format!("{rule} at {at}")),
            ["gap", what @ ..] => want_gaps.push(what.join(" ")),
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
                failures.extend(run_expect(module, &functions, call, want));
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
    let mut got_gaps: Vec<String> = gaps.iter().map(|g| g.what.to_string()).collect();
    // A gap is a kind of unbuilt construct: each kind is listed once per case.
    got_gaps.sort();
    got_gaps.dedup();
    want_gaps.sort();
    if got_gaps != want_gaps {
        failures.push(format!("gaps: want {want_gaps:?}, got {got_gaps:?}"));
    }
    failures
}

/// `f(a,b)` run against `= r1,r2` or `= fail C1,C2` (the cause chain, primary first),
/// optionally followed by `trace t1,t2`: the first u64 field of each resource whose
/// cleanup started, in order. Arguments and results are read at the emitted
/// signature's types, so a case writes values, not bits. A run that returns has
/// refunded every admission (MEM-23).
fn run_expect(
    module: &Module,
    functions: &HashMap<String, FuncId>,
    call: &str,
    want: &[&str],
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
    let (want, trace) = match want.iter().position(|w| *w == "trace") {
        Some(i) => (&want[..i], want.get(i + 1).copied()),
        None => (want, None),
    };
    let report = eval::run(module, f, &args, u64::MAX);
    if let Some(trace) = trace {
        let t: Vec<u64> = trace.split(',').map(|x| x.parse().unwrap()).collect();
        if report.trace != t {
            return Some(format!(
                "run {call}: want trace {t:?}, got {:?}",
                report.trace
            ));
        }
    }
    let got = report.outcome;
    if matches!(got, eval::Outcome::Returned(_)) && report.used != 0 {
        return Some(format!("run {call}: {} bytes still admitted", report.used));
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
                    .map(|(&t, r)| eval::scalar(t, r))
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
fn scalar_key_carriage() {
    let failures = run_case(&root().join("cases/body/scalar-key-carriage.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn scalar_task_entry_conventions() {
    let failures = run_case(&root().join("cases/body/scalar-task-entry.hirsel"));
    assert!(failures.is_empty(), "{failures:?}");
}

/// OWN-4 plain bounds need no destructor state even in the stored rigid body.
/// This checks publication, before instance cleanup could hide the extra carriers.
#[test]
fn plain_generic_body_needs_no_cleanup_carriers() {
    let (index, modules) = program_inputs("fn identity[T:plain](take x:T)->T {x}");
    let p = Program::declare(&index, &modules);
    let f = p
        .fns
        .iter()
        .position(|f| f.path.package_is("app", "1"))
        .unwrap();
    let mut emitter = Emitter::new();
    let checked = p.check_lower(f, &mut emitter);
    assert!(
        checked.refusals.is_empty() && checked.gaps.is_empty(),
        "{checked:?}"
    );
    let body = emitter.module.functions[checked.function.unwrap().index()]
        .body
        .as_ref()
        .unwrap();
    assert!(
        !body
            .insts
            .iter()
            .any(|i| matches!(i, hirsel_core_ir::Inst::Drop { .. })),
        "plain values have no cleanup"
    );
    assert!(
        !body
            .slots
            .contains(&hirsel_core_ir::PlaceTy::Value(Ty::Scalar(
                hirsel_core_layout::Scalar::Bool
            ))),
        "plain values need no drop flag"
    );
}

#[test]
fn scalar_instances_use_closed_signatures() {
    let (index, modules) = program_inputs(
        "fn id[T](take x:T)->T { x }\n\
         fn borrow[T](x:View[T])->View[T] in x { x }\n\
         fn edit[T](mut x:T) {}\n\
         type W { x:u64 }\n\
         fn run()->u64 { let mut x=7u64;edit(mut x);let a=id(x);let _=id(W(x:8));let _=id((9u64,));let _=id(()); borrow(view a) }",
    );
    let p = Program::declare(&index, &modules);
    let mut emitter = Emitter::new();
    for (f, fd) in p.fns.iter().enumerate() {
        if fd.body.is_none() || !fd.path.package_is("app", "1") {
            continue;
        }
        let check = p.check_lower(f, &mut emitter);
        assert!(
            check.refusals.is_empty() && check.gaps.is_empty(),
            "{check:?}"
        );
    }
    emitter.materialize_instances().unwrap();
    let module = &emitter.module;
    wf::module(module).unwrap();
    for f in &module.functions {
        let key = &module.instances[f.instance.index()];
        if !key.definition.package_is("app", "1") || key.types.is_empty() {
            continue;
        }
        let sig = &module.signatures[f.signature.index()];
        match key.definition.0.name.as_str() {
            "edit" => assert!(matches!(sig.params.as_slice(), [Param::Address { .. }])),
            "id" | "borrow" => {
                let row = module
                    .types
                    .iter()
                    .find(|t| t.name == key.types[0])
                    .unwrap();
                match row.carried {
                    hirsel_core_ir::Carried::Value(t) => {
                        assert_eq!(sig.params, [Param::Value(t)], "{key:?}");
                        assert_eq!(sig.results, [t], "{key:?}");
                        assert_eq!(sig.dest, None, "{key:?}");
                    }
                    hirsel_core_ir::Carried::Nothing => {
                        assert!(
                            sig.params.is_empty() && sig.results.is_empty() && sig.dest.is_none(),
                            "{key:?}"
                        );
                    }
                    hirsel_core_ir::Carried::Address => {
                        assert!(matches!(sig.params.as_slice(), [Param::Address { .. }]))
                    }
                }
            }
            _ => {}
        }
    }
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

/// Every source type carried as a provider/identity word has the same payload
/// width as its direct scalar ABI, including arguments and returned values.
#[test]
fn callable_provider_and_identity_words() {
    let names = [
        "Args", "Dir", "Net", "Clock", "Random", "Stdout", "Stderr", "Stdin", "Env", "Process",
        "Tasks", "Unique", "Site",
    ];
    let source = names.iter().enumerate().map(|(i, name)| format!(
        "fn id{i}(take x:{name})->{name}{{x}}\nfn f{i}(x:{name})->{name}{{let f:fn(take {name})->{name}=id{i};f(x)}}\n"
    )).collect::<String>();
    let (index, inputs) = inputs_with_core(&source, &root().join("../core"));
    let p = Program::declare(&index, &inputs);
    assert!(
        p.refusals.is_empty() && p.gaps.is_empty(),
        "{:?} {:?}",
        p.refusals,
        p.gaps
    );
    let mut emitter = Emitter::new();
    let mut targets = vec![];
    for (f, fd) in p.fns.iter().enumerate() {
        if !fd.path.package_is("app", "1") {
            continue;
        }
        let check = p.check_lower(f, &mut emitter);
        assert!(
            check.refusals.is_empty() && check.gaps.is_empty(),
            "{}: {check:?}",
            fd.path
        );
        if fd.path.0.name.starts_with('f') {
            targets.push(check.function.unwrap());
        }
    }
    wf::module(&emitter.module).unwrap();
    for target in targets {
        let report = eval::run(&emitter.module, target, &[0x1234_5678_90ab_cdef], 0);
        assert_eq!(
            report.outcome,
            eval::Outcome::Returned(vec![0x1234_5678_90ab_cdef])
        );
        assert_eq!(report.used, 0);
    }
}

/// OWN-4 (e9073564): `integer` and `float` imply every bound all their admitted types
/// have, and a core body may rely on them. `T: integer` is plain and ordered, so `max`
/// copies and compares; `T: float` is not key, so `same`'s `==` is refused under
/// LANG-21 (7d90583a, Q41).
/// These bounds are written only in core, so the bodies are a module of a copy of core.
#[test]
fn integer_and_float_bounds_imply_their_types_bounds() {
    let dir = std::env::temp_dir().join(format!("hirsel-own4-core-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for e in std::fs::read_dir(core_dir()).unwrap() {
        let path = e.unwrap().path();
        if path.extension().is_some_and(|e| e == "hirsel") {
            std::fs::copy(&path, dir.join(path.file_name().unwrap())).unwrap();
        }
    }
    std::fs::write(
        dir.join("bounds.hirsel"),
        "pub fn max[T: integer](a: T, b: T) -> T { if a < b { b } else { a } }\n\
         pub fn same[T: float](a: T, b: T) -> bool { a == b }\n",
    )
    .unwrap();
    let (index, inputs) = inputs_with_core("", &dir);
    let p = Program::declare(&index, &inputs);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    let mut emitter = Emitter::new();
    let mut rules = HashMap::new();
    for (f, fd) in p.fns.iter().enumerate() {
        if fd.path.0.modules == ["bounds"] {
            let check = p.check_lower(f, &mut emitter);
            let refused: Vec<&str> = check.refusals.iter().map(|r| r.rule.as_str()).collect();
            rules.insert(fd.path.0.name.to_string(), refused.join(","));
        }
    }
    // `max` is accepted; `same` is refused once, at its `==`.
    assert_eq!(
        (rules["max"].as_str(), rules["same"].as_str()),
        ("", "LANG-21")
    );
}

#[test]
fn corpus() {
    let mut paths = vec![];
    cases(&root().join("cases"), &mut paths);
    assert!(!paths.is_empty());
    let mut report = vec![];
    for path in &paths {
        for f in run_case(path) {
            report.push(format!(
                "{}: {f}",
                path.strip_prefix(root()).unwrap_or(path).display()
            ));
        }
    }
    assert!(report.is_empty(), "\n{}", report.join("\n"));
}

/// One program of shape `shape` at size `n`, and the work and emitted bytes of checking
/// its bodies.
fn work(shape: &str, n: usize) -> (u64, u64) {
    let mut s = String::new();
    let line = |s: &mut String, l: String| {
        s.push_str(&l);
        s.push('\n');
    };
    match shape {
        // Statements of one body.
        "size" => {
            line(&mut s, "fn f(x: u64) -> u64 {\n  let mut s: u64 = 0".into());
            for i in 0..n {
                line(
                    &mut s,
                    format!("  if s > x {{ s -= x }} else {{ s += x * {i} }}"),
                );
            }
            line(&mut s, "  s\n}".into());
        }
        // Blocks nested n deep.
        "nesting" => {
            line(&mut s, "fn f(x: u32) -> u32 {\n  let mut s: u32 = 0".into());
            for d in 0..n {
                line(&mut s, format!("  while s < {d} {{ s += 1"));
            }
            for _ in 0..n {
                line(&mut s, "  }".into());
            }
            line(&mut s, "  s\n}".into());
        }
        // One call of n arguments.
        "arity" => {
            let ps: Vec<String> = (0..n).map(|i| format!("p{i}: u64")).collect();
            line(&mut s, format!("fn h({}) -> u64 {{ p0 }}", ps.join(", ")));
            let args: Vec<String> = (0..n).map(|i| format!("{i}")).collect();
            line(
                &mut s,
                format!("fn f() -> u64 {{ h({}) }}", args.join(", ")),
            );
        }
        // A left-nested operator chain whose literals wait for the expected type.
        "chain" => {
            let terms: Vec<String> = (0..n).map(|i| format!("{i}")).collect();
            line(
                &mut s,
                format!("fn f(x: u64) -> u64 {{ x + ({}) }}", terms.join(" + ")),
            );
        }
        // Generic core operations nested n deep.
        _ => {
            let open = "int.wrapping_add(".repeat(n);
            line(
                &mut s,
                format!("fn f(x: u32) -> u32 {{ {open}x{} }}", ", 1)".repeat(n)),
            );
        }
    }
    let (index, modules) = program_inputs(&s);
    let p = Program::declare(&index, &modules);
    assert!(p.refusals.is_empty(), "{shape}: {:?}", p.refusals);
    let mut e = Emitter::new();
    let mut c = Counts::default();
    for (f, fd) in p.fns.iter().enumerate() {
        if fd.path.0.package == package("app") {
            let r = p.check_lower(f, &mut e);
            assert!(r.refusals.is_empty() && r.gaps.is_empty(), "{shape}: {r:?}");
            c += r.counts;
        }
    }
    (c.nodes + c.uses + c.places + c.calls + c.types, c.bytes)
}

/// The body pass is linear (LANG-8: one forward visit, no fixpoint): doubling a shape's
/// size at most doubles its counted work and emitted bytes, give or take the body's
/// fixed part. A walk repeated per element (re-deciding synthesis down an operator
/// chain, re-reading a parameter list per argument) breaks the law.
#[test]
fn doubling() {
    for shape in ["size", "nesting", "arity", "chain", "generic"] {
        // The resolver's nesting bound is above 64.
        let (w1, b1) = work(shape, 32);
        let (w2, b2) = work(shape, 64);
        assert!(w2 * 100 <= w1 * 210, "{shape}: work {w1} then {w2}");
        assert!(b2 * 100 <= b1 * 210, "{shape}: bytes {b1} then {b2}");
    }
}

#[path = "std_seed.rs"]
mod std_seed;

/// The M1 sources are executable specifications under construction. Parsing,
/// resolution and formatter stability are gates now; checker gaps remain visible
/// until the ownership and allocation slices can check these programs end to end.
#[test]
fn m1_program_manifest() {
    use hirsel_core_resolve::CorpusManifest;
    use hirsel_core_syntax::format;
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = workspace.join("core/programs/m1/manifest.json");
    let manifest_bytes = std::fs::read(path).unwrap();
    let config: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    let require_checker = config["checker"] == "require";
    let mut manifest: CorpusManifest = serde_json::from_slice(&manifest_bytes).unwrap();
    for package in &mut manifest.packages {
        for source in &mut package.sources {
            source.file = workspace.join(&source.file);
        }
    }
    let corpus = manifest.load().unwrap();
    let parsed = corpus.parse();
    let index = corpus.index(&parsed);
    let report = corpus.analyze(&parsed, &index);
    assert!(
        !report.files.is_empty(),
        "M1 manifest must contain source bodies"
    );
    let mechanical_ok = report
        .files
        .iter()
        .all(|f| f.parsed && f.resolved && f.roundtrip && !f.resource_refusal);
    for file in &report.files {
        for diagnostic in &file.diagnostics {
            println!(
                "M1 mechanical refusal {}:{}:{} {}: {}",
                file.file.display(),
                diagnostic.line,
                diagnostic.column,
                diagnostic.refusal.rule,
                diagnostic.refusal.message
            );
        }
    }
    let mut programs = 0;
    for (source, parsed) in corpus.sources.iter().zip(&parsed.files) {
        if source.mode != ParseMode::Source {
            continue;
        }
        let once = format(parsed).expect("accepted source formats");
        let again = parse(&once, ParseMode::Source, &mut Symbols::default());
        assert!(
            again.diagnostics.is_empty(),
            "{}: {:?}",
            source.file.display(),
            again.diagnostics
        );
        assert_eq!(
            format(&again).as_deref(),
            Some(once.as_str()),
            "{}",
            source.file.display()
        );
        assert_eq!(
            source.text,
            once,
            "format {} before committing",
            source.file.display()
        );
        programs += 1;
    }
    let modules: Vec<_> = corpus
        .sources
        .iter()
        .zip(parsed.files)
        .map(|(source, parsed)| ModuleInput {
            id: index.module(&source.module).unwrap(),
            syntax: parsed.syntax,
        })
        .collect();
    let mut halted = vec![];
    for entry in modules.iter().filter(|m| {
        !matches!(
            index.module_path(m.id).package.name.as_str(),
            "core" | "std"
        )
    }) {
        let selected: Vec<_> = modules
            .iter()
            .filter(|m| {
                matches!(
                    index.module_path(m.id).package.name.as_str(),
                    "core" | "std" | "m1"
                ) || index.module_package(m.id) == index.module_package(entry.id)
            })
            .map(|m| ModuleInput {
                id: m.id,
                syntax: m.syntax.clone(),
            })
            .collect();
        let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let program = Program::declare(&index, &selected);
            for refusal in &program.refusals {
                println!(
                    "M1 declaration refusal {} {} {:?}: {}",
                    index.module_path(refusal.primary.module).package.name,
                    line_col(&selected, &refusal.primary),
                    refusal.rule,
                    refusal.message
                );
            }
            for gap in &program.gaps {
                println!(
                    "M1 declaration gap {} {}: {}",
                    index.module_path(gap.site.module).package.name,
                    line_col(&selected, &gap.site),
                    gap.what
                );
            }
            let mut emitter = Emitter::new();
            let mut accepted = 0;
            let mut bodies = 0;
            let mut refused = 0;
            let mut gaps = 0;
            for (f, definition) in program.fns.iter().enumerate() {
                if definition.body.is_none()
                    || definition.path.0.package != index.module_path(entry.id).package
                    || definition.path.0.modules != index.module_path(entry.id).modules
                {
                    continue;
                }
                bodies += 1;
                let checked = program.check_lower(f, &mut emitter);
                if checked.refusals.is_empty() && checked.gaps.is_empty() {
                    accepted += 1;
                }
                refused += checked.refusals.len();
                gaps += checked.gaps.len();
                for refusal in &checked.refusals {
                    println!(
                        "M1 body refusal {}.{} {} {:?}: {}",
                        definition.path.0.package.name,
                        definition.path.0.name,
                        line_col(&selected, &refusal.primary),
                        refusal.rule,
                        refusal.message
                    );
                }
                for gap in &checked.gaps {
                    if gap.what == crate::body::SILENT_HALT {
                        halted.push(format!(
                            "{}.{} {}",
                            definition.path.0.package.name,
                            definition.path.0.name,
                            line_col(&selected, &gap.site)
                        ));
                    }
                    println!(
                        "M1 body gap {}.{} {}: {}",
                        definition.path.0.package.name,
                        definition.path.0.name,
                        line_col(&selected, &gap.site),
                        gap.what
                    );
                }
            }
            assert!(bodies > 0, "M1 acceptance must visit program bodies");
            if require_checker {
                assert!(program.refusals.is_empty() && program.gaps.is_empty());
                assert_eq!(
                    (refused, gaps),
                    (0, 0),
                    "M1 checker is required by the manifest"
                );
            }
            println!(
                "M1 checker {}.{}: {accepted}/{bodies} bodies accepted, {refused} refusals, {gaps} gaps; declarations {} refusals, {} gaps",
                index.module_path(entry.id).package.name,
                index.module_path(entry.id).modules.join("."),
                program.refusals.len(),
                program.gaps.len()
            );
        }));
        if let Err(panic) = checked {
            if require_checker {
                std::panic::resume_unwind(panic);
            }
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic");
            println!(
                "M1 checker CRASH {}.{}: {message}; no acceptance claimed",
                index.module_path(entry.id).package.name,
                index.module_path(entry.id).modules.join(".")
            );
        }
    }
    println!(
        "M1 mechanical: {}/{} files parse/resolve; {programs} source files format idempotently; checker outcomes above are diagnostic",
        report
            .files
            .iter()
            .filter(|f| f.parsed && f.resolved)
            .count(),
        report.files.len()
    );
    assert!(mechanical_ok, "M1 parse/resolve refusals are listed above");
    // A body whose emission stopped without a reported reason is never accepted.
    assert!(halted.is_empty(), "silent halts: {halted:?}");
}

#[test]
fn sibling_main_wrapper_executes_the_sibling_body() {
    let app = package("files");
    let manifest = PackageManifest {
        package: app,
        dependencies: vec![],
        sources: ["main", "matched"]
            .into_iter()
            .map(|name| ModuleSource {
                file: name.into(),
                modules: vec![name.into()],
                signatures: false,
            })
            .collect(),
    };
    let mut symbols = Symbols::default();
    manifest.intern(&mut symbols);
    let syntax = [
        "pub fn main() -> u64 { 42 }",
        "pub fn main() -> u64 { main.main() }",
    ]
    .into_iter()
    .map(|source| {
        let p = parse(source, ParseMode::Source, &mut symbols);
        assert!(p.diagnostics.is_empty());
        p.syntax
    })
    .collect::<Vec<_>>();
    let mut index = NameIndex::new(std::sync::Arc::new(symbols));
    let ids = index.add_manifests(&[manifest]);
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
    let p = Program::declare(&index, &modules);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    assert!(p.gaps.is_empty(), "{:?}", p.gaps);
    let mut emitter = Emitter::new();
    let mut wrapper = None;
    for (f, fd) in p.fns.iter().enumerate() {
        let checked = p.check_lower(f, &mut emitter);
        assert!(checked.refusals.is_empty(), "{:?}", checked.refusals);
        assert!(checked.gaps.is_empty(), "{:?}", checked.gaps);
        if fd.path.0.modules == ["matched"] {
            wrapper = checked.function;
        }
    }
    assert_eq!(
        eval::run(&emitter.module, wrapper.unwrap(), &[], 1_000).outcome,
        eval::Outcome::Returned(vec![42])
    );
}

/// PROG-5: type widths distinguish instances; equal requests name the same callee.
#[test]
fn generic_call_keys_separate_widths_and_reuse_equal_instances() {
    let (index, modules) = program_inputs(
        "fn id[T: key](take x: T) -> T { x }\n\
         fn byte(x: u8) -> u8 { id(x) }\n\
         fn word(x: u64) -> u64 { id(x) }\n\
         fn again(x: u8) -> u8 { id(x) }",
    );
    let p = Program::declare(&index, &modules);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    let mut emitter = Emitter::new();
    let mut callers = HashMap::new();
    for (f, fd) in p.fns.iter().enumerate() {
        if fd.body.is_none() || !fd.generics.is_empty() {
            continue;
        }
        let check = p.check_lower(f, &mut emitter);
        assert!(check.refusals.is_empty(), "{:?}", check.refusals);
        assert!(check.gaps.is_empty(), "{:?}", check.gaps);
        callers.insert(fd.path.0.name.as_str(), check.function.unwrap());
    }
    let module = &emitter.module;
    let target = |name| {
        let caller = &module.functions[callers[name].index()];
        let call = &caller.body.as_ref().unwrap().calls[0];
        let hirsel_core_ir::Callee::Direct(function) = call.callee else {
            panic!("direct call")
        };
        function
    };
    assert!(wf::module(module).is_ok());
    let byte = target("byte");
    let word = target("word");
    assert_eq!(byte, target("again"));
    assert_ne!(byte, word);
    for (function, ty) in [(byte, "u8"), (word, "u64")] {
        let function = &module.functions[function.index()];
        let key = &module.instances[function.instance.index()];
        assert_eq!(key.types, [ty]);
        assert_eq!(key.dictionaries, [format!("key[{ty}]")]);
    }
}

/// The carrier is an independent hand-written FORM producer, so these cases can
/// exercise populated Lists before the allocation/growth core calls are emitted.
/// Three elements 7,11,13 have sum 31; both success and failure must refund 24 bytes.
#[test]
fn list_index_iteration_and_failure_cleanup() {
    use hirsel_core_ir::{TypeId, text};
    let vectors = [
        (
            "type Choice = Small(u8) | Large(u64)\nfn f(xs: FixedSpan[Choice]) -> u64 { let mut s:u64=0; for c in xs { s+=match c { Small(v)=>{let n:u64=int.widen(v); n}, Large(v)=>v*100u64 } }; s }",
            false,
            eval::Outcome::Returned(vec![1120]),
        ),
        (
            "fn helper(xs: SeqSpan[(u8,u64)]) -> Option[View[(u8,u64)]] in xs { seq.nth(xs,1) }\nfn f(xs: SeqSpan[(u8,u64)]) -> u64 { match helper(xs) { Some(pair) => { let (_,v)=pair; v }, None=>0 } }",
            false,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn helper(xs: IndexedSpan[(u8,u64)]) -> Option[View[(u8,u64)]] in xs { indexed.get(xs,2) }\nfn f(xs: IndexedSpan[(u8,u64)]) -> u64 { match helper(xs) { Some(pair) => { let (_,v)=pair; v }, None=>0 } }",
            false,
            eval::Outcome::Returned(vec![13]),
        ),
        (
            "fn f(w32: IndexedSpan[u64]) -> u64 { match indexed.get(w32,2) { Some(v)=>v+0u64, None=>0 } }",
            false,
            eval::Outcome::Returned(vec![13]),
        ),
        (
            "fn f(w64: IndexedSpan[u64]) -> u64 { match indexed.get(w64,3) { Some(v)=>v+0u64, None=>0 } }",
            false,
            eval::Outcome::Returned(vec![0]),
        ),
        (
            "fn f(xs: FixedSpan[Inline[(u8,u64),2]]) -> u64 { let mut s:u64=0; for row in xs { for (_, y) in row { s += y } }; s }",
            false,
            eval::Outcome::Returned(vec![341]),
        ),
        (
            "fn f(xs: FixedSpan[Inline[(u8,u64),2]]) -> u64 { let (_,y) = xs[1][1]; y }",
            false,
            eval::Outcome::Returned(vec![110]),
        ),
        (
            "fn f(xs: FixedSpan[Site]) -> u64 { if xs[0] == xs[2] { 1 } else { 0 } }",
            false,
            eval::Outcome::Returned(vec![0]),
        ),
        (
            "type Entry { tiny: u8, num: u64 }\nfn f(xs: FixedSpan[Entry]) -> u64 { xs[1].num }",
            false,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(xs: FixedSpan[()]) -> u64 { xs[3]; 0 }",
            false,
            eval::Outcome::Failed(vec!["Bounds".into()]),
        ),
        (
            "fn f(xs: FixedSpan[()]) -> u64 { let mut s: u64 = 0; for _ in xs { s += 1 }; s }",
            false,
            eval::Outcome::Returned(vec![3]),
        ),
        (
            "fn f(xs: SeqSpan[u64]) -> u64 { let mut s: u64 = 0; for x in xs { s += x }; s }",
            false,
            eval::Outcome::Returned(vec![31]),
        ),
        (
            "fn f(xs: IndexedSpan[u64]) -> u64 { let mut s: u64 = 0; for x in xs { s += x }; s }",
            false,
            eval::Outcome::Returned(vec![31]),
        ),
        (
            "fn f(xs: SeqSpan[u64]) -> u64 { seq.len(xs) }",
            false,
            eval::Outcome::Returned(vec![3]),
        ),
        (
            "fn f(xs: SeqSpan[u64]) -> u64 { match seq.nth(xs, 1) { Some(x) => x + 0u64, None => 0 } }",
            false,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(xs: SeqSpan[u64]) -> u64 { match seq.nth(xs, 3) { Some(x) => x + 0u64, None => 0 } }",
            false,
            eval::Outcome::Returned(vec![0]),
        ),
        (
            "fn f(xs: IndexedSpan[u64]) -> u64 { match indexed.get(xs, 2) { Some(x) => x + 0u64, None => 0 } }",
            false,
            eval::Outcome::Returned(vec![13]),
        ),
        (
            "fn f(w32: FixedSpan[u64]) -> u64 { w32[2] }",
            false,
            eval::Outcome::Returned(vec![13]),
        ),
        (
            "fn f(w64: FixedSpan[u64]) -> u64 { w64[2] }",
            false,
            eval::Outcome::Returned(vec![13]),
        ),
        (
            "fn f(xs: FixedSpan[u64]) -> u64 { span.len(xs) }",
            false,
            eval::Outcome::Returned(vec![3]),
        ),
        (
            "fn f(xs: FixedSpan[u64]) -> u64 { match span.get(xs, 1) { Some(x) => x, None => 0 } }",
            false,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(xs: FixedSpan[u64]) -> u64 { match span.get(xs, 3) { Some(x) => x, None => 0 } }",
            false,
            eval::Outcome::Returned(vec![0]),
        ),
        (
            "fn f(xs: FixedSpan[u64]) -> u64 { xs[1] }",
            false,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(xs: FixedSpan[u64]) -> u64 { xs[3] }",
            false,
            eval::Outcome::Failed(vec!["Bounds".into()]),
        ),
        (
            "fn f(xs: FixedSpan[u64]) -> u64 { let mut s: u64 = 0; for x in xs { s += x }; s }",
            false,
            eval::Outcome::Returned(vec![31]),
        ),
        (
            "fn f(xs: FixedSpan[(u8,u64)]) -> u64 { let mut s: u64 = 0; for (x,y) in xs { s += y }; s }",
            false,
            eval::Outcome::Returned(vec![31]),
        ),
        (
            "fn f(xs: FixedSpan[(u8,u64)]) -> u64 { let (x,y) = xs[1]; y }",
            false,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(mut xs:Map[u64,u64])->u64 { let old=result.unwrap(map.set(mut xs,7,99)); match map.get(xs,7) {Some(v)=>old*1000+v,None=>0} }",
            false,
            eval::Outcome::Returned(vec![70099]),
        ),
        (
            "fn f(mut xs:Map[u64,u64])->u64 { let returned:u64=match map.set(mut xs,2,99) {Ok(_)=>0,Error(v)=>v};returned*10+map.len(xs) }",
            false,
            eval::Outcome::Returned(vec![992]),
        ),
        (
            "fn f(mut xs:Map[u64,u64])->u64 { let old=option.unwrap(map.remove(mut xs,7)); old*100+map.len(xs) }",
            false,
            eval::Outcome::Returned(vec![7001]),
        ),
        (
            "fn f(mut xs:Map[u64,u64])->u64 { let _=map.remove(mut xs,11); map.len(xs) }",
            false,
            eval::Outcome::Returned(vec![2]),
        ),
        (
            "fn f(mut xs:Map[u64,u64])->u64 { let _=map.remove(mut xs,13); match map.get(xs,13) {Some(_)=>99,None=>map.len(xs)} }",
            false,
            eval::Outcome::Returned(vec![1]),
        ),
        (
            "fn f(xs:Map[u64,u64])->u64 { match map.get(xs,7) { Some(v)=>v, None=>0 } }",
            false,
            eval::Outcome::Returned(vec![70]),
        ),
        (
            "fn f(xs:Map[u64,u64])->u64 { match map.get(xs,13) { Some(v)=>v, None=>0 } }",
            false,
            eval::Outcome::Returned(vec![130]),
        ),
        (
            "fn f(xs:Map[u64,u64])->u64 { match map.get(xs,11) { Some(v)=>v, None=>99 } }",
            false,
            eval::Outcome::Returned(vec![99]),
        ),
        (
            "fn f(xs:Map[u64,u64])->u64 { match map.get(xs,2) { Some(v)=>v, None=>99 } }",
            false,
            eval::Outcome::Returned(vec![99]),
        ),
        (
            "fn lookup[K:key,V](m:Map[K,V],k:K)->Option[View[V]] in m {map.get(m,k)}\nfn f(xs:Map[u64,u64])->u64 {match lookup(xs,7) {Some(v)=>v,None=>0}}",
            false,
            eval::Outcome::Returned(vec![70]),
        ),
        (
            "fn f(xs: Map[u64,u64]) -> u64 { map.len(xs) }",
            false,
            eval::Outcome::Returned(vec![2]),
        ),
        (
            "fn count[K:key,V](m:Map[K,V])->u64 { map.len(m) }\nfn f(xs:Map[u64,u64])->u64 { count(xs) }",
            false,
            eval::Outcome::Returned(vec![2]),
        ),
        (
            "fn count[K:key,V](m:Map[K,V])->u64 { let mut n:u64=0; for (k,v) in m { n+=1 }; n }\nfn f(xs:Map[u64,u64])->u64 { count(xs) }",
            false,
            eval::Outcome::Returned(vec![2]),
        ),
        (
            "fn f(xs: Map[u64,u64]) -> u64 { let mut s: u64 = 0; for (k,v) in xs { s += k + v }; s }",
            false,
            eval::Outcome::Returned(vec![220]),
        ),
        (
            "fn f(xs: Map[u64,u64]) -> u64 { let mut s: u64 = 0; for (k,v) in xs { s = s * 100 + k }; s }",
            false,
            eval::Outcome::Returned(vec![713]),
        ),
        (
            "fn add(x: u64) -> u64 { x + 1 }\nfn f(xs: Map[u64,u64]) -> u64 { let mut s: u64 = 0; for (k,v) in xs { s += add(v) }; s }",
            false,
            eval::Outcome::Returned(vec![202]),
        ),
        (
            "fn f(xs: List[u64]) -> u64 { let v = view xs; v[1] }",
            false,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(xs: List[u64]) -> u64 { xs[3] }",
            false,
            eval::Outcome::Failed(vec!["Bounds".into()]),
        ),
        (
            "fn f(mut xs: List[u64]) -> u64 { xs[1] += 5; xs[1] }",
            false,
            eval::Outcome::Returned(vec![16]),
        ),
        (
            "fn f(xs: List[u64]) -> u64 { let mut s: u64 = 0; for x in xs { s += x }; s }",
            false,
            eval::Outcome::Returned(vec![31]),
        ),
        (
            "fn f(xs: List[u64]) -> u64 { let mut s: u64 = 0; for x in xs { if x == 11 { continue }; s += x }; s }",
            false,
            eval::Outcome::Returned(vec![20]),
        ),
        (
            "fn f(xs: List[u64]) -> u64 { let mut s: u64 = 0; for x in xs { s += x; break }; s }",
            false,
            eval::Outcome::Returned(vec![7]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { let mut s: u64 = 0; for take x in xs { s += x }; s }",
            true,
            eval::Outcome::Returned(vec![31]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { let mut s: u64 = 0; for take x in xs { s += x; break }; s }",
            true,
            eval::Outcome::Returned(vec![7]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { let mut s: u64 = 0; for take x in xs { if x == 11 { continue }; s += x }; s }",
            true,
            eval::Outcome::Returned(vec![20]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { for take x in xs { return x }; 0 }",
            true,
            eval::Outcome::Returned(vec![7]),
        ),
        (
            "fn f(take xs: List[List[u64]]) -> u64 { let mut s: u64 = 0; for take row in xs { s += row[0]; break }; s }",
            true,
            eval::Outcome::Returned(vec![7]),
        ),
        (
            "fn f(take xs: List[List[u64]]) -> u64 { let mut s: u64 = 0; for take row in xs { s += row[0] }; s }",
            true,
            eval::Outcome::Returned(vec![31]),
        ),
        (
            "fn f(take xs: List[List[u64]]) -> u64 { for take row in xs { return row[0] }; 0 }",
            true,
            eval::Outcome::Returned(vec![7]),
        ),
        (
            "fn f(take xs: List[List[u64]]) -> u64 { for take row in xs { let n: u8 = int.narrow(256u64) }; 0 }",
            true,
            eval::Outcome::Failed(vec!["Overflow".into()]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { let a = [take xs]; let mut s: u64 = 0; for row in a { s += row[1] }; s }",
            true,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { let a = [take xs]; let row = a[0]; row[1] }",
            true,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { let n: u8 = int.narrow(256u64); 0 }",
            true,
            eval::Outcome::Failed(vec!["Overflow".into()]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { let a = [take xs, { let z = 1u64 / 0u64; loop {} }]; 0 }",
            true,
            eval::Outcome::Failed(vec!["Division".into()]),
        ),
    ];
    for (source, takes, expected) in vectors {
        let (index, inputs) = program_inputs(source);
        let p = Program::declare(&index, &inputs);
        assert!(p.refusals.is_empty(), "{source}: {:?}", p.refusals);
        let f = p.fns.iter().position(|f| f.path.0.name == "f").unwrap();
        let mut emitter = Emitter::new();
        for (other, fd) in p.fns.iter().enumerate() {
            if other != f && fd.body.is_some() {
                let checked = p.check_lower(other, &mut emitter);
                assert!(
                    checked.refusals.is_empty() && checked.gaps.is_empty(),
                    "{}: {checked:?}",
                    fd.path
                );
            }
        }
        let check = p.check_lower(f, &mut emitter);
        assert!(
            check.refusals.is_empty() && check.gaps.is_empty(),
            "{source}: {:?} {:?}",
            check.refusals,
            check.gaps
        );
        let target = check.function.unwrap();
        emitter.materialize_instances().unwrap();
        if source == "fn f(xs:Map[u64,u64])->u64 { match map.get(xs,7) { Some(v)=>v, None=>0 } }" {
            for function in &emitter.module.functions {
                let key = &emitter.module.instances[function.instance.index()];
                if key.definition.0.modules == ["map"]
                    && let Some(body) = &function.body
                {
                    eprintln!(
                        "Map instance {} {:?}: {} instructions, {} terminators",
                        key.definition,
                        key.types,
                        body.insts.len(),
                        body.terminators.len()
                    );
                }
            }
        }
        let module = &emitter.module;
        let signature = module.functions[target.index()].signature;
        let Param::Address {
            ty: TypeId(list), ..
        } = module.signatures[signature.index()].params[0]
        else {
            panic!("List by address")
        };
        let (g, instance, entry) = (
            module.signatures.len(),
            module.instances.len(),
            module.functions.len(),
        );
        let drop = if takes {
            String::new()
        } else {
            format!("drop a0 t{list}\n")
        };
        // The entry alone: the vectors below edit its text, never the core bodies.
        let fixture = format!(
            r#"
sig g{g} = () -> (u64)
inst n{instance} = app@1/fixture/entry [] []
fn f{entry} = n{instance} g{g} {{
 slot s0 = t{list}
 addr a0 = s0 : t{list}
 addr a1 = a0 byte v1 : ptr
 addr a2 = a0 byte v2 : u64
 addr a3 = a0 byte v3 : u64
 addr a4 = v4 : u64
 addr a5 = v4 byte v2 : u64
 addr a6 = v4 byte v3 : u64
 b0:
  live s0
  v0: u64 = const 24
  v1: u64 = const 0
  v2: u64 = const 8
  v3: u64 = const 16
  v4: ptr, v5: u64 = alloc own v0 align 8 charge v0
  v6: u64 = const 7
  v7: u64 = const 11
  v8: u64 = const 13
  v9: u64 = const 3
  address a0
  address a1
  address a2
  address a3
  address a4
  address a5
  address a6
  store a1, v4
  store a2, v9
  store a3, v9
  store a4, v6
  store a5, v7
  store a6, v8
  v10: u64 = call f{} g{} (&a0) fail b1
  {drop}
  return v10
 b1:
  {drop}
  propagate
}}
"#,
            target.0, signature.0
        );
        let fixture = if source.contains("FixedSpan[")
            || source.contains("SeqSpan[")
            || source.contains("IndexedSpan[")
        {
            // Independent W16 blob: header, count=3, then tightly packed elements.
            let sequence = !source.contains("FixedSpan[");
            let tuple = source.contains("Span[(u8,u64)]") || source.contains("FixedSpan[Entry]");
            let width = if source.contains("w32") {
                4
            } else if source.contains("w64") {
                8
            } else {
                2
            };
            let mut addresses = String::new();
            let mut build = String::new();
            let mut fields = vec![
                (8, width, "u64"),
                (
                    16,
                    3,
                    if width == 2 {
                        "u16"
                    } else if width == 4 {
                        "u32"
                    } else {
                        "u64"
                    },
                ),
            ];
            if sequence {
                fields.push((
                    16 + width as usize,
                    if tuple { 27 } else { 24 },
                    if width == 2 {
                        "u16"
                    } else if width == 4 {
                        "u32"
                    } else {
                        "u64"
                    },
                ));
            }
            if source.contains("IndexedSpan[") {
                let stride = if tuple { 9 } else { 8 };
                let table = 16 + 2 * width as usize + 3 * stride;
                let word = if width == 2 {
                    "u16"
                } else if width == 4 {
                    "u32"
                } else {
                    "u64"
                };
                for i in 0..3usize {
                    fields.push((table + i * width as usize, (i * stride) as u64, word));
                }
            }
            let variant = source.contains("FixedSpan[Choice]");
            let array = source.contains("FixedSpan[Inline[");
            for (i, bits) in [7, 11, 13].into_iter().enumerate() {
                let first = 16 + width as usize * if sequence { 2 } else { 1 };
                if variant {
                    let offset = first + i * 16;
                    fields.push((offset, u64::from(i == 1), "u64"));
                    fields.push((offset + 8, bits, if i == 1 { "u64" } else { "u8" }));
                } else if array {
                    for (j, bits) in [bits, bits * 10].into_iter().enumerate() {
                        let offset = first + i * 18 + j * 9;
                        fields.push((offset, 1, "u8"));
                        fields.push((offset + 1, bits, "u64"));
                    }
                } else {
                    let offset = first + i * if tuple { 9 } else { 8 };
                    if tuple {
                        fields.push((offset, 1, "u8"));
                    }
                    fields.push((offset + usize::from(tuple), bits, "u64"));
                }
            }
            for (i, (offset, bits, ty)) in fields.into_iter().enumerate() {
                let a = 7 + i;
                let v = 11 + 2 * i;
                addresses.push_str(&format!("addr a{a} = v4 byte v{v} : {ty}\n"));
                build.push_str(&format!("v{v}: u64 = const {offset}\nv{}: {ty} = const {bits}\naddress a{a}\nstore a{a}, v{}\n",v+1,v+1));
            }
            fixture
                .replace("v0: u64 = const 24", "v0: u64 = const 128")
                .replace(" b0:", &format!("{addresses} b0:"))
                .replace("store a3, v9", "")
                .replace("store a2, v9", "store a2, v3")
                .replace("store a4, v6\n  store a5, v7\n  store a6, v8", &build)
                .replace(&format!("drop a0 t{list}"), "free v4 charge v0")
        } else if source.contains("Map[u64,u64]") {
            let mut addresses = String::new();
            let mut build = String::new();
            for (i, (offset, bits)) in [
                (0, 0x49ec259204a234c2u64),
                (8, 1),
                (16, 7),
                (24, 70),
                (32, 0xe1e0cd537ed145b8),
                (40, 0),
                (48, 11),
                (56, 110),
                (64, 0xaddb2134e9f8658d),
                (72, 1),
                (80, 13),
                (88, 130),
                (96, 2),  // live count differs from the three occupied slots
                (104, 8), // cached index length, independent of capacity=3
                (112, 2),
                (120, 0),
                (128, 1),
                (136, 0),
                (144, 0),
                (152, 3),
                (160, 0),
                (168, 0),
            ]
            .into_iter()
            .enumerate()
            {
                let a = 7 + i;
                let v = 12 + 2 * i;
                addresses.push_str(&format!("addr a{a} = v4 byte v{v} : u64\n"));
                build.push_str(&format!("v{v}: u64 = const {offset}\nv{}: u64 = const {bits}\naddress a{a}\nstore a{a}, v{}\n",v+1,v+1));
            }
            fixture
                .replace(
                    "v0: u64 = const 24",
                    "v0: u64 = const 96\n  v11: u64 = const 176",
                )
                .replace(
                    "alloc own v0 align 8 charge v0",
                    "alloc own v11 align 8 charge v0",
                )
                .replace(" b0:", &format!("{addresses} b0:"))
                .replace("store a4, v6\n  store a5, v7\n  store a6, v8", &build)
                .replace(&format!("drop a0 t{list}"), "free v4 charge v0")
        } else if source.contains("List[List[u64]]") {
            let mut addresses = String::new();
            let mut build = String::new();
            for i in 0..3 {
                let a = 7 + 4 * i;
                let v = 11 + 7 * i;
                let offset = i * 24;
                addresses.push_str(&format!("addr a{a} = v4 byte v{v} : ptr\naddr a{} = v4 byte v{} : u64\naddr a{} = v4 byte v{} : u64\naddr a{} = v{} : u64\n",a+1,v+1,a+2,v+2,a+3,v+3));
                build.push_str(&format!("v{v}: u64 = const {offset}\nv{}: u64 = const {}\nv{}: u64 = const {}\nv{}: ptr, v{}: u64 = alloc own v2 align 8 charge v2\nv{}: u64 = const 1\nv{}: u64 = const {}\naddress a{a}\naddress a{}\naddress a{}\naddress a{}\nstore a{a}, v{}\nstore a{}, v{}\nstore a{}, v{}\nstore a{}, v{}\n",v+1,offset+8,v+2,offset+16,v+3,v+4,v+5,v+6,[7,11,13][i],a+1,a+2,a+3,v+3,a+1,v+5,a+2,v+5,a+3,v+6));
            }
            fixture
                .replace("v0: u64 = const 24", "v0: u64 = const 72")
                .replace(" b0:", &format!("{addresses} b0:"))
                .replace("store a4, v6\n  store a5, v7\n  store a6, v8", &build)
        } else {
            fixture
        };
        let fixture = text::parse(&format!("{}{fixture}", text::print(module))).unwrap();
        wf::module(&fixture).unwrap();
        let report = eval::run(&fixture, FuncId(entry as u32), &[], 1_000_000);
        assert_eq!(report.outcome, expected, "{source}");
        assert_eq!(report.used, 0, "{source}: list storage leaked");
    }
}

#[test]
fn emission_halts_but_typing_continues() {
    let programs = [
        "fn f(mut xs: List[u8], i: u64) { let s = int; xs[i] = 1u8 }",
        "fn f(xs: List[u64]) -> u64 { let s = int; xs[0] }",
        "fn f(xs: List[u64]) { let s = int; for x in xs { let n = x + 1 } }",
        "fn f(take xs: List[u64]) { let s = int; for take x in xs {} }",
        "fn f(xs: Map[u64,u64]) { let s = int; for (k,v) in xs {} }",
        "fn f(xs: FixedSpan[u64]) -> u64 { let s = int; xs[0] }",
        "fn f(xs: FixedSpan[u64]) { let s = int; for x in xs {} }",
        "fn f(xs: SeqSpan[u64]) -> u64 { let s = int; seq.len(xs) }",
        "fn f(xs: SeqSpan[u64]) { let s = int; let x = seq.nth(xs, 0) }",
        "fn f(xs: List[u64]) { for x in xs { let s = int } }",
        "fn f() { let s = int; for x in 0u64..=3 {} }",
        "fn f() { for x in 0u64..=3 { let s = int; continue } }",
        "fn f() -> u64 { let s = int; [7u64,11u64][0] }",
        "fn f() { let s = int; let a = [1u64; 3] }",
        "fn f() { let s = int; let n: u8 = int.narrow(1u64) }",
        "fn f() { let s = int; let n: Option[u8] = int.try_narrow(1u64) }",
        "fn f() { let s = int; let n = int.wrapping_shl(1u64, 3) }",
        "fn f() { let s = int; let n = int.checked_add(1u64, 3) }",
        "fn f() { let s = int; let n = int.leading_zeros(1u64) }",
        "fn f() { let s = int; let n = int.mul_wide(1u64, 3u64) }",
        "fn f() { let s = int; let n: Result[u8, FloatError] = float.to_int(1.0f64) }",
        "fn f() { let s = int; let n = float.total_order(1.0f64, 2.0f64) }",
        "fn f() { let s = int; let n = float.mul_add(1.0f64, 2.0f64, 3.0f64) }",
        "fn f() { let s = int; let n = float.to_bits64(1.0f64) }",
    ];
    for source in programs {
        let (index, inputs) = program_inputs(source);
        let p = Program::declare(&index, &inputs);
        assert!(p.refusals.is_empty(), "{source}: {:?}", p.refusals);
        let f = p.fns.iter().position(|f| f.path.0.name == "f").unwrap();
        let check = p.check_lower(f, &mut Emitter::new());
        assert!(check.refusals.is_empty(), "{source}: {:?}", check.refusals);
        assert!(
            check
                .gaps
                .iter()
                .any(|g| g.what == "function, constant and module values are not built"),
            "{source}: {:?}",
            check.gaps
        );
        assert!(check.function.is_none(), "{source}: emitted past a gap");
    }
    let source = "fn f() { let s = int; let n: u8 = int.widen(1u64) }";
    let (index, inputs) = program_inputs(source);
    let p = Program::declare(&index, &inputs);
    let f = p.fns.iter().position(|f| f.path.0.name == "f").unwrap();
    let check = p.check_lower(f, &mut Emitter::new());
    assert!(
        check.refusals.iter().any(|r| r.rule == "SEM-1"),
        "typing stopped at the gap: {check:?}"
    );
    assert!(check.function.is_none());
}

#[path = "core_ops_tests.rs"]
mod core_ops_tests;
#[path = "tests_maps.rs"]
mod maps;

#[test]
fn callable_payload_metadata_is_built_once_per_type() {
    let source = format!(
        "fn f(a: fn(u64) -> u64, b: fn(u8) -> u8) {{ {} }}",
        "let _ = a(1); let _ = b(2);".repeat(256)
    );
    let (index, inputs) = program_inputs(&source);
    let p = Program::declare(&index, &inputs);
    let f = p.fns.iter().position(|f| f.path.0.name == "f").unwrap();
    let mut emitter = Emitter::new();
    let check = p.check_lower(f, &mut emitter);
    assert!(
        check.refusals.is_empty() && check.gaps.is_empty(),
        "{check:?}"
    );
    let body = emitter.module.functions[check.function.unwrap().index()]
        .body
        .as_ref()
        .unwrap();
    assert_eq!(body.calls.len(), 512);
    assert_eq!(emitter.callable_signature_builds, 2);
    wf::module(&emitter.module).unwrap();
}

#[test]
fn list_set_transfers_the_indexed_element() {
    let (index, modules) =
        program_inputs("fn set_one(mut xs: List[u64]) -> u64 { list.set(mut xs, 1, 9) }");
    let p = Program::declare(&index, &modules);
    let f = p
        .fns
        .iter()
        .position(|f| f.path.0.name == "set_one")
        .unwrap();
    let mut emitter = Emitter::new();
    let check = p.check_lower(f, &mut emitter);
    assert!(
        check.refusals.is_empty() && check.gaps.is_empty(),
        "{check:?}"
    );
    let target = check.function.unwrap();
    let sig = emitter.module.functions[target.index()].signature;
    let Param::Address { ty: list, .. } = emitter.module.signatures[sig.index()].params[0] else {
        panic!("mut List ABI")
    };
    let s = emitter.module.signatures.len();
    let i = emitter.module.instances.len();
    let f = emitter.module.functions.len();
    // Independent two-element fixture [5,7], avoiding an unbuilt push/fill helper.
    // update index 1 with 9: first*10000 + returned_old*100 + second = 50709.
    let source = format!(
        "{}\nsig g{s} = () -> (u64)\ninst n{i} = fixture@1/list/probe [] []\nfn f{f} = n{i} g{s} {{
  slot s0 = t{list}
  addr a0 = s0 : t{list}
  addr a1 = v3 : u64
  addr a2 = v3 byte v1 : u64
  addr a3 = s0 byte v0 : ptr
  addr a4 = s0 byte v1 : u64
  addr a5 = s0 byte v2 : u64
b0:
  live s0
  v0: u64 = const 0
  v1: u64 = const 8
  v2: u64 = const 16
  v3: ptr, v4: u64 = alloc own v2 align 8 charge v2
  v5: u64 = const 2
  v6: u64 = const 5
  v7: u64 = const 7
  v8: u64 = const 100
  v9: u64 = const 10000
  address a0
  address a1
  address a2
  address a3
  address a4
  address a5
  store a1, v6
  store a2, v7
  store a3, v3
  store a4, v5
  store a5, v5
  v10: u64 = call f{target} g{sig} (&a0)
  v11: u64 = read a1
  v12: u64 = read a2
  v13: u64 = mul v11, v9
  v14: u64 = mul v10, v8
  v15: u64 = add v13, v14
  v16: u64 = add v15, v12
  drop a0 t{list}
  dead s0
  return v16
}}",
        hirsel_core_ir::text::print(&emitter.module),
        list = list.0,
        target = target.0,
        sig = sig.0
    );
    let module = hirsel_core_ir::text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    let report = eval::run(&module, FuncId(f as u32), &[], u64::MAX);
    assert_eq!(report.outcome, eval::Outcome::Returned(vec![50709]));
    assert_eq!(report.used, 0);
    for case in ["list-set-bounds.hirsel", "list-set-static-gap.hirsel"] {
        let failures = run_case(&root().join("cases/body").join(case));
        assert!(failures.is_empty(), "{case}: {failures:?}");
    }
}

#[test]
fn causes_iteration_reads_complete_cells_and_preserves_order() {
    use hirsel_core_ir::text;
    let vectors = [
        (
            "let mut s: u64 = 0; for c in cs { let k = match c.kind { Fail(m) => if m == \"data\" { 14u64 } else { 90u64 }, Overflow => 1, _ => 90 }; s = s * 100 + k; if c.site == target { s += 1 } }; s",
            eval::Outcome::Returned(vec![1501]),
        ),
        (
            "let mut s: u64 = 0; for c in cs { match c.kind { Fail(_) => continue, _ => {} }; s += 1 }; s",
            eval::Outcome::Returned(vec![1]),
        ),
        (
            "let mut s: u64 = 0; for c in cs { s += 1; break }; s",
            eval::Outcome::Returned(vec![1]),
        ),
        (
            "for c in cs { return match c.kind { Fail(m) => if m == \"data\" { 4u64 } else { 0u64 }, _ => 0 } }; 99",
            eval::Outcome::Returned(vec![4]),
        ),
        (
            "for c in cs { let n: u8 = int.narrow(256u64) }; 0",
            eval::Outcome::Failed(vec!["Overflow".into()]),
        ),
    ];
    for (body, expected) in vectors {
        let source = format!("fn f(cs: Causes, target: Site) -> u64 {{ {body} }}");
        let (index, inputs) = program_inputs(&source);
        let p = Program::declare(&index, &inputs);
        assert!(p.refusals.is_empty(), "{:?}", p.refusals);
        let f = p.fns.iter().position(|f| f.path.0.name == "f").unwrap();
        let mut emitter = Emitter::new();
        let check = p.check_lower(f, &mut emitter);
        assert!(
            check.refusals.is_empty() && check.gaps.is_empty(),
            "{source}: {check:?}"
        );
        let target = check.function.unwrap();
        let m = &emitter.module;
        let sig = m.functions[target.index()].signature;
        let Param::Address { ty: cs, .. } = m.signatures[sig.index()].params[0] else {
            panic!("Causes carrier")
        };
        let (g, instance, entry) = (m.signatures.len(), m.instances.len(), m.functions.len());
        // Two complete cells: Fail(message length 4, site 17), then Overflow(site 23).
        // They are ordinary owned fixture blocks; f only borrows the chain. A cell is
        // core's 32-byte Cause, the Fail String carrier at 0 and the site at 24, then
        // the next cell at 32; Overflow is the capacity niche 2^63 + 0 (FORM #75).
        let mut fixture = format!(
            "{}\nsig g{g} = () -> (u64)\ninst n{instance} = app@1/fixture/causes [] []\nfn f{entry} = n{instance} g{g} {{\nslot s0 = t{}\naddr a0 = s0 : t{}\naddr a1 = a0 : ptr\n",
            text::print(m),
            cs.0,
            cs.0
        );
        let words = [
            ("v1", 8, "u64", 4),
            ("v1", 16, "u64", 8),
            ("v1", 24, "u64", 17),
            ("v3", 0, "u64", 0),
            ("v3", 8, "u64", 0),
            ("v3", 16, "u64", 1u64 << 63),
            ("v3", 24, "u64", 23),
            ("v3", 32, "ptr", 0),
        ];
        let k = words.len();
        for (i, (base, _, ty, _)) in words.iter().enumerate() {
            fixture += &format!("addr a{} = {base} byte v{} : {ty}\n", i + 2, 10 + 2 * i);
        }
        let (data_at, next_at, bytes_at) = (k + 2, k + 3, k + 4);
        let (data_off, next_off, result) = (10 + 2 * k, 11 + 2 * k, 12 + 2 * k);
        fixture += &format!(
            "addr a{data_at} = v1 byte v{data_off} : ptr\naddr a{next_at} = v1 byte v{next_off} : ptr\naddr a{bytes_at} = v6 : u32\nb0:\nlive s0\nv0: u64 = const 40\nv1: ptr, v2: u64 = alloc own v0 align 8 charge v0\nv3: ptr, v4: u64 = alloc own v0 align 8 charge v0\nv5: u64 = const 8\nv6: ptr, v7: u64 = alloc own v5 align 1 charge v5\nv8: u64 = const 17\nv9: u32 = const 1635017060\naddress a{bytes_at}\nstore a{bytes_at}, v9\n"
        );
        for (i, (_, offset, ty, bits)) in words.iter().enumerate() {
            let bits = if *ty == "ptr" {
                format!("0x{bits:x}")
            } else {
                bits.to_string()
            };
            fixture += &format!(
                "v{}: u64 = const {offset}\nv{}: {ty} = const {bits}\naddress a{}\nstore a{}, v{}\n",
                10 + 2 * i,
                11 + 2 * i,
                i + 2,
                i + 2,
                11 + 2 * i
            );
        }
        fixture += &format!(
            "v{data_off}: u64 = const 0\nv{next_off}: u64 = const 32\naddress a{data_at}\naddress a{next_at}\nstore a{data_at}, v6\nstore a{next_at}, v3\naddress a0\naddress a1\nstore a1, v1\nv{result}: u64 = call f{} g{} (&a0, v8) fail b1\nfree v6 charge v5\nfree v3 charge v0\nfree v1 charge v0\nreturn v{result}\nb1:\nfree v6 charge v5\nfree v3 charge v0\nfree v1 charge v0\npropagate\n}}",
            target.0, sig.0
        );
        let module = text::parse(&fixture).unwrap();
        wf::module(&module).unwrap_or_else(|e| panic!("{e}\n{}", text::print(&module)));
        let report = eval::run(&module, FuncId(entry as u32), &[], 10000);
        assert_eq!(report.outcome, expected, "{source}");
        assert_eq!(report.used, 0, "{source}: fixture storage leaked");
    }
}

mod run_time_abi {
    include!("../../hirsel-core-rt/src/abi_layout.rs");
}

/// The run time and both engines write and read `Cause` cells by the frozen constants
/// in `abi_layout.rs` (FORM #55, #75); core reads them through its own recipe. A layout
/// rule that moves `CauseKind`'s tag or a field (a new niche, a reordering) must fail
/// here rather than in a delivered chain.
#[test]
fn core_cause_layout_is_the_run_time_abi() {
    use hirsel_core_layout::{Body as ShapeBody, FieldTy, Layout, case_word, compute};
    use run_time_abi as abi;
    let (index, inputs) =
        program_inputs("fn f(cs: Causes) -> u64 { let mut s: u64 = 0; for c in cs { s += 1 }; s }");
    let p = Program::declare(&index, &inputs);
    let f = p.fns.iter().position(|f| f.path.0.name == "f").unwrap();
    let mut emitter = Emitter::new();
    let check = p.check_lower(f, &mut emitter);
    assert!(
        check.refusals.is_empty() && check.gaps.is_empty(),
        "{check:?}"
    );
    let m = &emitter.module;
    let recipes = compute(&m.types).unwrap();
    let row = |name: &str| {
        m.types
            .iter()
            .position(|t| t.name == name)
            .unwrap_or_else(|| panic!("no {name} row"))
    };
    let fixed = |i: usize| match &recipes[i].layout {
        Layout::Fixed(l) => l.clone(),
        other => panic!("{other:?}"),
    };
    let cause = fixed(row("core@1/core/Cause"));
    let ShapeBody::Record(fields) = &m.types[row("core@1/core/Cause")].body else {
        panic!("Cause is a record")
    };
    assert_eq!(cause.size, abi::CAUSE_BYTES as u64);
    let at = |name: &str| cause.offsets[0][fields.iter().position(|f| f.name == name).unwrap()];
    assert_eq!(at("site"), abi::CAUSE_SITE as u64);
    let FieldTy::Inline(kind) = fields.iter().find(|f| f.name == "kind").unwrap().ty else {
        panic!("CauseKind is inline")
    };
    let kind_at = at("kind");
    let kind = fixed(kind.index());
    let tag = kind.tag.unwrap();
    assert_eq!(kind_at + tag.offset, abi::CAUSE_KIND as u64);
    assert_eq!(
        kind_at + kind.offsets[abi::CAUSE_FAIL as usize][0],
        abi::CAUSE_MESSAGE as u64
    );
    for k in 0..7u8 {
        let word = case_word(&tag, u32::from(k));
        if k == abi::CAUSE_FAIL {
            assert_eq!(word, None);
        } else {
            assert_eq!(
                (tag.width, word),
                (8, Some(abi::cause_kind_word(k))),
                "kind {k}"
            );
        }
    }
}

/// Independent packed bytes: (tag, nested sequence) lengths 1,0,2 give
/// 101+200+302=603. Skipping the middle record must still advance its cursor.
#[test]
fn packed_iteration_uses_one_cursor_for_variable_records() {
    use hirsel_core_ir::{Static, text};
    for width in [2usize, 4, 8] {
        for indexed in [false, true] {
            for (body, expected) in [
                (
                    "let wide: u64 = int.widen(tag); s += wide * 100u64 + seq.len(ys)",
                    603,
                ),
                (
                    "if tag == 2u8 { continue }; let wide: u64 = int.widen(tag); s += wide * 100u64 + seq.len(ys)",
                    403,
                ),
                (
                    "if tag == 2u8 { break }; let wide: u64 = int.widen(tag); s += wide * 100u64 + seq.len(ys)",
                    101,
                ),
            ] {
                let span = if indexed { "IndexedSpan" } else { "SeqSpan" };
                let source = format!(
                    "@compact(u16) stable type Row {{ tag: u8, ys: SeqSpan[u64] }}\nfn f(xs: {span}[Row]) -> u64 {{ let mut s: u64 = 0; for row in xs {{ let tag = row.tag; let ys = row.ys; {body} }}; s }}"
                );
                let (index, inputs) = program_inputs(&source);
                let p = Program::declare(&index, &inputs);
                assert!(p.refusals.is_empty(), "{:?}", p.refusals);
                let f = p.fns.iter().position(|f| f.path.0.name == "f").unwrap();
                let mut emitter = Emitter::new();
                let check = p.check_lower(f, &mut emitter);
                assert!(
                    check.refusals.is_empty() && check.gaps.is_empty(),
                    "{:?} {:?}",
                    check.refusals,
                    check.gaps
                );
                let target = check.function.unwrap();
                let sig = emitter.module.functions[target.index()].signature;
                let Param::Address { ty, .. } = emitter.module.signatures[sig.index()].params[0]
                else {
                    panic!("span locator")
                };
                let mut elements = vec![];
                let mut offsets = vec![];
                for (tag, count) in [(1u8, 1u64), (2, 0), (3, 2)] {
                    offsets.push(elements.len() as u64);
                    elements.push(tag);
                    elements.extend_from_slice(&count.to_le_bytes()[..width]);
                    elements.extend_from_slice(&(count * 8).to_le_bytes()[..width]);
                    for _ in 0..count {
                        elements.extend_from_slice(&7u64.to_le_bytes());
                    }
                }
                let mut blob = vec![0; 8];
                blob.extend_from_slice(&(width as u64).to_le_bytes());
                blob.extend_from_slice(&3u64.to_le_bytes()[..width]);
                blob.extend_from_slice(&(elements.len() as u64).to_le_bytes()[..width]);
                blob.extend(elements);
                if indexed {
                    for off in offsets {
                        blob.extend_from_slice(&off.to_le_bytes()[..width]);
                    }
                }
                let data = emitter.module.statics.len();
                emitter.module.statics.push(Static {
                    align: 8,
                    bytes: blob,
                });
                let g = emitter.module.signatures.len();
                let inst = emitter.module.instances.len();
                let entry = emitter.module.functions.len();
                let fixture = format!(
                    r#"{}
sig g{g} = () -> (u64)
inst n{inst} = app@1/fixture/entry [] []
fn f{entry} = n{inst} g{g} {{
 slot s0 = t{ty}
 addr a0 = s0 : t{ty}
 addr a1 = a0 byte v1 : ptr
 addr a2 = a0 byte v2 : u64
 b0:
  live s0
  v0: ptr = static d{data}
  v1: u64 = const 0
  v2: u64 = const 8
  v3: u64 = const 16
  address a0
  address a1
  address a2
  store a1, v0
  store a2, v3
  v4: u64 = call f{} g{} (&a0)
  return v4
}}
"#,
                    text::print(&emitter.module),
                    target.0,
                    sig.0,
                    ty = ty.0
                );
                let module = text::parse(&fixture).unwrap();
                wf::module(&module).unwrap();
                let report = eval::run(&module, FuncId(entry as u32), &[], u64::MAX);
                assert_eq!(
                    report.outcome,
                    eval::Outcome::Returned(vec![expected]),
                    "{width} {indexed} {source}"
                );
                assert_eq!(report.used, 0);
            }
        }
    }
}

#[test]
fn aggregate_constants_publish_values_and_use_read_only_tables() {
    let failures = run_case(&root().join("cases/body/constant-aggregates.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
    let source = include_str!("../../../core/front/cases/body/constant-aggregates.hirsel");
    let (index, inputs) = program_inputs(source);
    let p = Program::declare(&index, &inputs);
    assert!(
        p.gaps.is_empty() && p.refusals.is_empty(),
        "{:?} {:?}",
        p.gaps,
        p.refusals
    );
    let hidden = p
        .constants
        .iter()
        .find(|c| c.path.0.name == "HIDDEN")
        .unwrap();
    assert!(
        p.constant_field(
            p.modules[hidden.at.module as usize].id,
            hidden.value.unwrap(),
            "raw"
        )
        .is_ok()
    );
    let foreign = p
        .modules
        .iter()
        .find(|m| m.id != p.modules[hidden.at.module as usize].id)
        .unwrap()
        .id;
    assert_eq!(
        p.constant_field(foreign, hidden.value.unwrap(), "raw"),
        Err("PROG-2")
    );
    let count = p.constant_parts.len();
    let value = p
        .constants
        .iter()
        .find(|c| c.path.0.name == "LOOKUP")
        .unwrap()
        .value
        .unwrap();
    let crate::ConstData::Fields { len, .. } = value.data else {
        panic!("checked array")
    };
    assert_eq!(len, 8);
    let f = p
        .fns
        .iter()
        .position(|f| f.path.0.name == "lookup")
        .unwrap();
    let mut emitter = Emitter::new();
    let check = p.check_lower(f, &mut emitter);
    assert!(
        check.refusals.is_empty() && check.gaps.is_empty(),
        "{:?} {:?}",
        check.refusals,
        check.gaps
    );
    let body = emitter.module.functions[check.function.unwrap().index()]
        .body
        .as_ref()
        .unwrap();
    assert!(
        body.slots.is_empty(),
        "lookup must not rebuild the table on the stack"
    );
    let instructions = &body.insts;
    let instruction_count = instructions.len();
    assert_eq!(
        instructions
            .iter()
            .filter(|i| matches!(i, hirsel_core_ir::Inst::ReadStatic { .. }))
            .count(),
        1
    );
    assert!(!instructions.iter().any(|i| matches!(
        i,
        hirsel_core_ir::Inst::Store { .. } | hirsel_core_ir::Inst::Copy { .. }
    )));
    assert_eq!(emitter.module.statics.len(), 1);
    assert_eq!(
        emitter.module.statics[0].bytes,
        (0u16..8)
            .flat_map(|i| (i * 3).to_le_bytes())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        p.constant_parts.len(),
        count,
        "body uses only published constant values"
    );
    let _ = p.check_lower(f, &mut emitter);
    assert_eq!(
        emitter.module.statics.len(),
        1,
        "reuse the same read-only table"
    );
    eprintln!(
        "constant table lookup: {} CoreIR instructions",
        instruction_count
    );
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
        assert_eq!(
            c.value.unwrap().data,
            crate::ConstData::Bits(bits),
            "{name}"
        );
    }
    let failures = run_case(&root().join("cases/body/constant-literals.hirsel"));
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn invalid_grants_and_constants_keep_their_own_rules_and_sites() {
    for (source, rule, token) in [
        (
            "resource type R grant bytes 1.5 { n: u64 } drop(r) {}",
            "LANG-5",
            "1.5",
        ),
        (
            "resource type R grant bytes 18446744073709551616 { n: u64 } drop(r) {}",
            "LANG-3",
            "18446744073709551616",
        ),
        ("const BAD: u8 = 256", "LANG-20", "256"),
        ("const BAD: String = \"hello\"", "LANG-20", "\"hello\""),
    ] {
        let (index, inputs) = program_inputs(source);
        let p = Program::declare(&index, &inputs);
        let refusals = &p.refusals;
        assert_eq!(refusals.len(), 1, "{source}: {refusals:?}");
        assert_eq!(refusals[0].rule, rule, "{source}: {refusals:?}");
        let span = refusals[0].primary.span;
        assert_eq!(&source[span.start as usize..span.end as usize], token);
    }
}

#[test]
fn aggregate_constant_errors_are_checked_before_use() {
    for source in [
        "const BAD: Inline[u8,2] = [1,256]",
        "const BAD: Inline[u8,3] = [1,2]",
        "const BAD: Inline[u8,0] = [200u8 + 100u8;0]",
        "type Point { x:u8 }\nfn id(x:u8)->u8{x}\nconst BAD: Point = Point(x:id(7))",
        "const A: (u8,u8) = (0,B.0)\nconst B: (u8,u8) = (A.0,0)",
    ] {
        let (index, inputs) = program_inputs(source);
        let p = Program::declare(&index, &inputs);
        assert!(p.gaps.is_empty(), "{source}: {:?}", p.gaps);
        assert_eq!(p.refusals.len(), 1, "{source}: {:?}", p.refusals);
        assert_eq!(p.refusals[0].rule, "LANG-20", "{source}: {:?}", p.refusals);
    }
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
fn core_bodies_check_waits_while_bodiless_rows_use_the_written_effect() {
    let dir = std::env::temp_dir().join(format!("hirsel-waits-core-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for file in std::fs::read_dir(core_dir()).unwrap() {
        let file = file.unwrap();
        if file.path().is_file() {
            std::fs::copy(file.path(), dir.join(file.file_name())).unwrap();
        }
    }
    std::fs::write(
        dir.join("effects.hirsel"),
        r#"
pub fn waiting(rx:Receiver[u64]) waits { let _=channel.receive(rx) }
pub fn hidden(rx:Receiver[u64]) { let _=channel.receive(rx) }
pub fn extra()->u64 waits { 1u64 }
pub fn written()->u64 waits
"#,
    )
    .unwrap();
    let (index, inputs) = inputs_with_core("", &dir);
    std::fs::remove_dir_all(&dir).unwrap();
    let p = Program::declare(&index, &inputs);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    let mut e = Emitter::new();
    for (f, fd) in p
        .fns
        .iter()
        .enumerate()
        .filter(|(_, fd)| fd.path.0.modules == ["effects"])
    {
        if fd.body.is_none() {
            assert_eq!(fd.path.0.name, "written");
            continue;
        }
        let c = p.check_lower(f, &mut e);
        assert!(c.gaps.is_empty(), "{}: {:?}", fd.path, c.gaps);
        let rules = c
            .refusals
            .iter()
            .map(|r| r.rule.as_str())
            .collect::<Vec<_>>();
        let want = if fd.path.0.name == "waiting" {
            vec![]
        } else {
            vec!["LANG-8"]
        };
        assert_eq!(rules, want, "{}", fd.path);
    }
}

/// FORM #64: a published row says how every signature carries its values, so a generic
/// instance's signature can read it instead of the row's fields. The bug it catches:
/// `u64`, a one-field record and a one-element tuple all publish `{0: u64}`-like
/// bodies, and a classifier reading the body passes the record or tuple by value.
/// Expected: the closed rule as core.md states it (scalars, plain `fn`, capability and
/// identity words by value; unit nothing; declared and structural types by address),
/// and each row agrees with the closed signature `s` takes the same type with.
#[test]
fn rows_publish_the_closed_signature_carriage() {
    use hirsel_core_ir::{Carried, Scalar};
    let (index, modules) = program_inputs(
        "type W { x: u64 }\n\
         fn id[T](take x: T) -> T { x }\n\
         fn s(a: u64, w: W, t: (u64,), c: char, f: f64, b: bool, u: Unique) -> u64 { a }\n\
         fn g(a: u64, w: W, t: (u64,), u: (), c: char, f: f64, b: bool, q: Unique) -> u64 {\n\
           let _ = id(w); let _ = id(t); let _ = id(u); let _ = id(c); let _ = id(f);\n\
           let _ = id(b); let _ = id(q); id(a)\n\
         }",
    );
    let p = Program::declare(&index, &modules);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    let mut emitter = Emitter::new();
    let mut s = None;
    for (f, fd) in p.fns.iter().enumerate() {
        if fd.body.is_none() || !fd.generics.is_empty() || !fd.path.package_is("app", "1") {
            continue;
        }
        let check = p.check_lower(f, &mut emitter);
        assert!(
            check.refusals.is_empty() && check.gaps.is_empty(),
            "{check:?}"
        );
        if fd.path.0.name == "s" {
            s = check.function;
        }
    }
    let module = &emitter.module;
    wf::module(module).unwrap();
    let row = |name: &str| {
        module
            .types
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "no row {name}: {:?}",
                    module.types.iter().map(|t| &t.name).collect::<Vec<_>>()
                )
            })
            .carried
    };
    let value = |s| Carried::Value(Ty::Scalar(s));
    let expected = [
        ("u64", value(Scalar::U64)),
        ("app@1/main/W", Carried::Address),
        ("(u64)", Carried::Address),
        ("char", value(Scalar::Char)),
        ("f64", value(Scalar::F64)),
        ("bool", value(Scalar::Bool)),
        ("core@1/core/Unique", value(Scalar::U64)),
        ("()", Carried::Nothing),
    ];
    for (name, carried) in expected {
        assert_eq!(row(name), carried, "{name}");
    }
    let sig = &module.signatures[module.functions[s.unwrap().index()].signature.index()];
    let from_rows: Vec<Param> = expected
        .iter()
        .filter_map(|&(name, carried)| match carried {
            Carried::Value(t) => Some(Param::Value(t)),
            Carried::Address => Some(Param::Address {
                ty: hirsel_core_ir::TypeId(
                    module.types.iter().position(|t| t.name == name).unwrap() as u32,
                ),
                loan: hirsel_core_ir::Loan::View,
            }),
            Carried::Nothing => None,
        })
        .collect();
    assert_eq!(sig.params, from_rows);
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
    let (index, inputs) = program_inputs("fn f(b:Bytes,at:u64)->u64 { list.word_le(b,at) }");
    let p = Program::declare(&index, &inputs);
    let f = p
        .fns
        .iter()
        .position(|fd| fd.path.package_is("app", "1"))
        .unwrap();
    let mut emitter = Emitter::new();
    let check = p.check_lower(f, &mut emitter);
    assert!(
        check.refusals.is_empty() && check.gaps.is_empty(),
        "{check:?}"
    );
    let body = emitter.module.functions[check.function.unwrap().index()]
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
fn mut_scoped_parameters_keep_the_callers_anchors() {
    let mut failures = vec![];
    for case in [
        "mut-scoped-anchors.hirsel",
        "mut-scoped-anchors-refused.hirsel",
        "mut-scoped-forward-refused.hirsel",
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
    ] {
        failures.extend(
            run_case(&root().join("cases/body").join(format!("{name}.hirsel")))
                .into_iter()
                .map(|f| format!("{name}: {f}")),
        );
    }
    assert!(failures.is_empty(), "{failures:?}");
}
