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

fn package(name: &str) -> PackageId {
    PackageId {
        name: name.into(),
        version: "1".into(),
    }
}

/// The core fixture and one application module, indexed with the core fixture as the
/// core package (its `core` module is the prelude).
fn program_inputs(source: &str) -> (NameIndex, Vec<ModuleInput>) {
    let mut symbols = Symbols::default();
    let core = PackageManifest::directory(&root().join("core"), package("core"), true).unwrap();
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
        if index.module_package(fd.site.module) != index.module_package(main) || fd.body.is_none() {
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
            Param::Address(_) => Ty::Ptr,
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
                let _ = p.check_lower(other, &mut emitter);
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
        let module = &emitter.module;
        let signature = module.functions[target.index()].signature;
        let Param::Address(TypeId(list)) = module.signatures[signature.index()].params[0] else {
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
        let fixture = format!(
            r#"
{}
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
            text::print(module),
            target.0,
            signature.0
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
                    24,
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
                fields.extend([(44, 0, "u16"), (46, 8, "u16"), (48, 16, "u16")]);
            }
            for (i, bits) in [7, 11, 13].into_iter().enumerate() {
                let offset = 16
                    + width as usize * if sequence { 2 } else { 1 }
                    + i * if tuple { 9 } else { 8 };
                if tuple {
                    fields.push((offset, 1, "u8"));
                }
                fields.push((offset + usize::from(tuple), bits, "u64"));
            }
            for (i, (offset, bits, ty)) in fields.into_iter().enumerate() {
                let a = 7 + i;
                let v = 11 + 2 * i;
                addresses.push_str(&format!("addr a{a} = v4 byte v{v} : {ty}\n"));
                build.push_str(&format!("v{v}: u64 = const {offset}\nv{}: {ty} = const {bits}\naddress a{a}\nstore a{a}, v{}\n",v+1,v+1));
            }
            fixture
                .replace("v0: u64 = const 24", "v0: u64 = const 64")
                .replace(" b0:", &format!("{addresses} b0:"))
                .replace("store a3, v9", "")
                .replace("store a2, v9", "store a2, v3")
                .replace("store a4, v6\n  store a5, v7\n  store a6, v8", &build)
                .replace(&format!("drop a0 t{list}"), "free v4 charge v0")
        } else if source.contains("Map[u64,u64]") {
            let mut addresses = String::new();
            let mut build = String::new();
            for (i, (offset, bits)) in [
                (8, 1),
                (16, 7),
                (24, 70),
                (40, 0),
                (48, 11),
                (56, 110),
                (72, 1),
                (80, 13),
                (88, 130),
            ]
            .into_iter()
            .enumerate()
            {
                let a = 7 + i;
                let v = 11 + 2 * i;
                addresses.push_str(&format!("addr a{a} = v4 byte v{v} : u64\n"));
                build.push_str(&format!("v{v}: u64 = const {offset}\nv{}: u64 = const {bits}\naddress a{a}\nstore a{a}, v{}\n",v+1,v+1));
            }
            fixture
                .replace("v0: u64 = const 24", "v0: u64 = const 96")
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
        let fixture = text::parse(&fixture).unwrap();
        wf::module(&fixture).unwrap();
        let report = eval::run(&fixture, FuncId(entry as u32), &[], 1_000_000);
        assert_eq!(report.outcome, expected, "{source}");
        assert_eq!(report.used, 0, "{source}: list storage leaked");
    }
}

#[test]
fn emission_halts_but_typing_continues() {
    let programs = [
        "fn f(mut xs: List[u8], i: u64) { let s = \"\"; xs[i] = 1u8 }",
        "fn f(xs: List[u64]) -> u64 { let s = \"\"; xs[0] }",
        "fn f(xs: List[u64]) { let s = \"\"; for x in xs { let n = x + 1 } }",
        "fn f(take xs: List[u64]) { let s = \"\"; for take x in xs {} }",
        "fn f(xs: Map[u64,u64]) { let s = \"\"; for (k,v) in xs {} }",
        "fn f(xs: FixedSpan[u64]) -> u64 { let s = \"\"; xs[0] }",
        "fn f(xs: FixedSpan[u64]) { let s = \"\"; for x in xs {} }",
        "fn f(xs: SeqSpan[u64]) -> u64 { let s = \"\"; seq.len(xs) }",
        "fn f(xs: SeqSpan[u64]) { let s = \"\"; let x = seq.nth(xs, 0) }",
        "fn f(xs: List[u64]) { for x in xs { let s = \"\" } }",
        "fn f() { let s = \"\"; for x in 0u64..=3 {} }",
        "fn f() { for x in 0u64..=3 { let s = \"\"; continue } }",
        "fn f() -> u64 { let s = \"\"; [7u64,11u64][0] }",
        "fn f() { let s = \"\"; let a = [1u64; 3] }",
        "fn f() { let s = \"\"; let n: u8 = int.narrow(1u64) }",
        "fn f() { let s = \"\"; let n: Option[u8] = int.try_narrow(1u64) }",
        "fn f() { let s = \"\"; let n = int.wrapping_shl(1u64, 3) }",
        "fn f() { let s = \"\"; let n = int.checked_add(1u64, 3) }",
        "fn f() { let s = \"\"; let n = int.leading_zeros(1u64) }",
        "fn f() { let s = \"\"; let n = int.mul_wide(1u64, 3u64) }",
        "fn f() { let s = \"\"; let n: Result[u8, FloatError] = float.to_int(1.0f64) }",
        "fn f() { let s = \"\"; let n = float.total_order(1.0f64, 2.0f64) }",
        "fn f() { let s = \"\"; let n = float.mul_add(1.0f64, 2.0f64, 3.0f64) }",
        "fn f() { let s = \"\"; let n = float.to_bits64(1.0f64) }",
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
                .any(|g| g.what == "string literals are not built"),
            "{source}: {:?}",
            check.gaps
        );
        assert!(check.function.is_none(), "{source}: emitted past a gap");
    }
    let source = "fn f() { let s = \"\"; let n: u8 = int.widen(1u64) }";
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
