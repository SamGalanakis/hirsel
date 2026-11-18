//! Definition-local checking, lowering and declaration queries.

use crate::*;
use hirsel_core_ids::PackageId;
use hirsel_core_ir::{FuncId, Param, Ty, eval, wf};
use hirsel_core_resolve::{ModuleSource, PackageManifest};
use hirsel_core_syntax::{ParseMode, Symbols, parse};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    let col = before.rsplit('\n').next().unwrap().chars().count() + 1;
    format!("{line}:{col}")
}

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
    inputs_in(source, core_dir, "main")
}

fn inputs_in(source: &str, core_dir: &Path, module: &str) -> (NameIndex, Vec<ModuleInput>) {
    let mut symbols = Symbols::default();
    let core = PackageManifest::directory(core_dir, package("core"), true).unwrap();
    let app = PackageManifest {
        package: package("app"),
        sources: vec![ModuleSource {
            file: PathBuf::from("input.hirsel"),
            modules: vec![module.into()],
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
        .map(|(id, syntax)| ModuleInput {
            id,
            source: index.add_source(),
            syntax,
        })
        .collect::<Vec<_>>();
    for d in &diagnostics {
        index.diagnostics.push(Refusal::syntax(
            modules.last().unwrap().id,
            modules.last().unwrap().source,
            d,
        ));
    }
    for m in &modules {
        for (i, item) in m.syntax.items().enumerate() {
            index.index_item(m.id, m.source, i as u32, item);
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

/// Run one case; returns its failures.
/// Materialization is substitution alone (FORM #56): a concrete row (no `$i` in its
/// name) never holds a rigid parameter's placeholder row in any field. Walks every
/// FieldTy variant on its own, independent of the substitution's field mapper.

/// Facts across modules (crate docs, "Roles"). Split as a release links it, core's
/// bodies published with their recorded facts and the case's bodies reading them,
/// and also as two modules linked together, the module derives each fact it does
/// whole. The evaluator's role check holds the whole module's facts on every run,
/// so a recorded fact that is too strong fails here or there.

/// `f(a,b)` run against `= r1,r2` or `= fail C1,C2` (the cause chain, primary first;
/// `C1@line:column` also checks each cause's site, SITES.md),
/// optionally followed by `trace t1,t2`: the first u64 field of each resource whose
/// cleanup started, in order, and by `used N`: the bytes still admitted when it returns,
/// 0 when absent. Arguments and results are read at the emitted signature's types, so a
/// case writes values, not bits. A run that returns has refunded every admission
/// (MEM-23) except what it states: a caught failure keeps its cause cell (FAIL-16).

#[test]
fn layout_names_keep_nested_views() {
    let (index, modules) = program_inputs("fn named(a:Option[u64], b:Option[View[u64]]) {}");
    let p = Program::declare(&index, &modules);
    let f = p
        .fns
        .iter()
        .position(|f| f.path.package_is("app", "1"))
        .unwrap();
    let mut emitter = Emitter::new();
    let c = p.check_lower(f, &mut emitter);
    assert!(c.refusals.is_empty() && c.gaps.is_empty(), "{c:?}");
    let row = |name: &str| {
        emitter
            .module
            .types
            .iter()
            .find(|s| s.name == name)
            .unwrap()
    };
    assert_eq!(
        row("core@1/core/Option[u64]").repr,
        hirsel_core_layout::Repr::Own
    );
    assert_eq!(
        row("core@1/core/Option[View[u64]]").repr,
        hirsel_core_layout::Repr::Scoped
    );
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

/// The growth path is cold (body/growth.rs): a call is cold exactly when it is
/// lowered in the true arm of an `if` testing the used word against the capacity
/// word of one place. Hand-derived from the source: grow and mark(2) only.
#[test]
fn growth_arm_calls_are_cold() {
    let (index, modules) = program_inputs(
        "fn grow(mut xs:List[u64])->u64 { 1 }\n\
         fn mark(x:u64)->u64 { x }\n\
         fn push(mut xs:List[u64],ys:List[u64])->u64 {\n\
           let mut n:u64=0\n\
           if list.len(xs)==list.capacity(xs) { n+=grow(mut xs) }\n\
           if list.len(xs)==list.capacity(ys) { n+=mark(1) }\n\
           let cap=list.capacity(xs)\n\
           if cap==list.len(xs) { n+=mark(2) } else { n+=mark(3) }\n\
           if list.len(xs)==list.len(xs) { n+=mark(5) }\n\
           n+mark(4)\n\
         }",
    );
    let p = Program::declare(&index, &modules);
    let mut emitter = Emitter::new();
    let f = p
        .fns
        .iter()
        .position(|f| f.path.package_is("app", "1") && f.path.0.name == "push")
        .unwrap();
    let check = p.check_lower(f, &mut emitter);
    assert!(
        check.refusals.is_empty() && check.gaps.is_empty(),
        "{check:?}"
    );
    let push = check.function;
    let names = emitter
        .module
        .functions
        .iter()
        .enumerate()
        .map(|(i, f)| {
            (
                hirsel_core_ir::FuncId(i as u32),
                emitter.module.instances[f.instance.index()]
                    .definition
                    .0
                    .name
                    .clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    let body = emitter.module.functions[push.unwrap().index()]
        .body
        .as_ref()
        .unwrap();
    let constant = |v: hirsel_core_ir::ValueId| {
        body.insts.iter().find_map(|i| match i {
            hirsel_core_ir::Inst::Const { result, bits } if *result == v => Some(*bits),
            _ => None,
        })
    };
    let mut cold = vec![];
    for c in &body.calls {
        let hirsel_core_ir::Callee::Direct(f) = c.callee else {
            continue;
        };
        let name = &names[&f];
        let label = match c.args.get(&body.pools.args).unwrap_or_default() {
            [hirsel_core_ir::Arg::Value(v)] if name == "mark" => {
                format!("mark({})", constant(*v).unwrap())
            }
            _ => name.clone(),
        };
        if c.cold {
            cold.push(label);
        }
    }
    cold.sort();
    assert_eq!(cold, ["grow", "mark(2)"]);
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
         pub fn same[T: float](a: T, b: T) -> bool { a == b }\n\
         pub fn accepts[T: codec](x: T) -> u64 { 1 }\n\
         pub fn integral[T: integer](x: T) -> u64 { accepts(x) }\n\
         pub fn fractional[T: float](x: T) -> u64 { accepts(x) }\n",
    )
    .unwrap();
    let (index, inputs) = inputs_with_core("", &dir);
    let p = Program::declare(&index, &inputs);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    let mut rules = HashMap::new();
    for (f, fd) in p.fns.iter().enumerate() {
        if fd.path.0.modules == ["bounds"] {
            let mut emitter = Emitter::new();
            let check = p.check_lower(f, &mut emitter);
            let refused: Vec<&str> = check.refusals.iter().map(|r| r.rule.as_str()).collect();
            rules.insert(fd.path.0.name.to_string(), refused.join(","));
        }
    }
    assert_eq!(rules["integral"], "");
    assert_eq!(rules["fractional"], "");
    // `max` is accepted; `same` is refused once, at its `==`.
    assert_eq!(
        (rules["max"].as_str(), rules["same"].as_str()),
        ("", "LANG-21")
    );
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
    let mut c = Counts::default();
    for (f, fd) in p.fns.iter().enumerate() {
        if fd.path.0.package == package("app") {
            let mut e = Emitter::new();
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
        .enumerate()
        .map(|(source_id, (source, parsed))| ModuleInput {
            source: hirsel_core_resolve::SourceId(source_id as u32),
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
                source: m.source,
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
                let mut emitter = Emitter::new();
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

/// LANG-11: only arguments made entirely of bare literals wait for inference.

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
    let mut callers = HashMap::new();
    for (f, fd) in p.fns.iter().enumerate() {
        if fd.body.is_none() || !fd.generics.is_empty() || !fd.path.package_is("app", "1") {
            continue;
        }
        let mut emitter = Emitter::new();
        let check = p.check_lower(f, &mut emitter);
        assert!(
            check.refusals.is_empty() && check.gaps.is_empty(),
            "{check:?}"
        );
        let module = &emitter.module;
        wf::module(module).unwrap();
        let caller = &module.functions[check.function.unwrap().index()];
        let hirsel_core_ir::Callee::Direct(function) =
            caller.body.as_ref().unwrap().calls[0].callee
        else {
            panic!("direct call")
        };
        let key = &module.instances[module.functions[function.index()].instance.index()];
        callers.insert(
            fd.path.0.name.as_str(),
            (
                module.identity(module.functions[function.index()].instance),
                module
                    .argument_names(key)
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
                key.dictionaries.clone(),
            ),
        );
    }
    let target = |name| &callers[name].0;
    let byte = target("byte");
    let word = target("word");
    assert_eq!(byte, target("again"));
    assert_ne!(byte, word);
    for (name, ty) in [("byte", "u8"), ("word", "u64")] {
        assert_eq!(callers[name].1, [ty]);
        assert_eq!(callers[name].2, [format!("key[{ty}]")]);
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
        let Param::Address {
            place: hirsel_core_ir::PlaceTy::Declared(cs),
            ..
        } = m.signatures[sig.index()].params[0]
        else {
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
        // Each cell is a boxed Cause; its message bytes a `String`'s buffer.
        let cause = m
            .types
            .iter()
            .position(|t| t.name.ends_with("Cause"))
            .unwrap();
        let (data_at, next_at, bytes_at) = (k + 2, k + 3, k + 4);
        let (data_off, next_off, result) = (10 + 2 * k, 11 + 2 * k, 12 + 2 * k);
        fixture += &format!(
            "addr a{data_at} = v1 byte v{data_off} : ptr\naddr a{next_at} = v1 byte v{next_off} : ptr\naddr a{bytes_at} = v6 : u32\nb0:\nlive s0\nv0: u64 = const 40\nv1: ptr, v2: u64 = alloc for box t{cause} v0 align 8 charge v0\nv3: ptr, v4: u64 = alloc for box t{cause} v0 align 8 charge v0\nv5: u64 = const 8\nv6: ptr, v7: u64 = alloc for seq u8 v5 align 1 charge v5\nv8: u64 = const 17\nv9: u32 = const 1635017060\naddress a{bytes_at}\nstore a{bytes_at}, v9\n"
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
                    "@compact(u16) type Row {{ tag: u8, ys: SeqSpan[u64] }}\nfn f(xs: {span}[Row]) -> u64 {{ let mut s: u64 = 0; for row in xs {{ let tag = row.tag; let ys = row.ys; {body} }}; s }}"
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
                let Param::Address {
                    place: hirsel_core_ir::PlaceTy::Declared(ty),
                    ..
                } = emitter.module.signatures[sig.index()].params[0]
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
                emitter.module.statics.push(Static::Bytes {
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
        emitter.module.statics[0]
            .native(&emitter.module.types, &emitter.recipes)
            .1,
        (0u16..8)
            .flat_map(|i| (i * 3).to_le_bytes())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        p.constant_parts.len(),
        count,
        "body uses only published constant values"
    );
    eprintln!(
        "constant table lookup: {} CoreIR instructions",
        instruction_count
    );
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
        let mut e = Emitter::new();
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

#[test]
fn scoped_environment_slots_belong_to_the_captured_activation() {
    use hirsel_core_ir::PlaceTy;
    let (index, modules) = program_inputs(
        r#"
fn checked()->u64 {
 let x:u64=7
 let f=scoped fn()->u64 {let g=scoped fn()->u64{x};g()}
 f()
}
"#,
    );
    let program = Program::declare(&index, &modules);
    let f = program
        .fns
        .iter()
        .position(|f| f.path.package_is("app", "1"))
        .unwrap();
    let mut emitter = Emitter::new();
    let check = program.check_lower(f, &mut emitter);
    assert!(
        check.refusals.is_empty() && check.gaps.is_empty(),
        "{check:?}"
    );
    let root = check.function.unwrap();
    emitter.seal();
    let recipes = hirsel_core_layout::compute(&emitter.module.types).unwrap();
    let slot_sizes = |body: &hirsel_core_ir::Body| {
        body.slots
            .iter()
            .filter_map(|ty| match ty {
                PlaceTy::Declared(id) => match &recipes[id.index()].layout {
                    hirsel_core_layout::Layout::Fixed(l) => Some(l.size),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let root_sizes = slot_sizes(
        emitter.module.functions[root.index()]
            .body
            .as_ref()
            .unwrap(),
    );
    // g's one-word environment and f's two-word environment live in checked.
    // f carries the hidden address of g's environment beside its ordinary &x.
    assert!(root_sizes.contains(&8), "{root_sizes:?}");
    assert_eq!(
        root_sizes.iter().filter(|&&n| n == 16).count(),
        2,
        "{root_sizes:?}"
    );
    for function in &emitter.module.functions {
        if function.instance == emitter.module.functions[root.index()].instance {
            continue;
        }
        if let Some(body) = &function.body {
            assert!(
                !slot_sizes(body).contains(&8),
                "nested activation owns no environment: {:?}",
                slot_sizes(body)
            );
        }
    }
    wf::module(&emitter.module).unwrap();
    assert_eq!(
        eval::run(&emitter.module, root, &[], 100_000).outcome,
        eval::Outcome::Returned(vec![7])
    );
}

/// PROG-9 identity bytes, one case per clause of the S rules, each expected value
/// derived by hand from programs.md (the first is the spec's own example, verbatim).
/// Bugs: the default mode `view` written; spaces around `->` or after `,`; a missing
/// result left out; `waits`, anchors or outlives pairs dropped, unsorted or repeated; a
/// generics list written, or region generics numbered by declared order rather than first
/// naming; an unescaped byte in a definition identity component.
#[test]
fn code_identity_bytes_follow_prog9() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "main",
            "fn double(b: Bytes) -> Result[Bytes, Capacity] { let mut out: Bytes = list.new(); list.append(mut out, b)?; list.append(mut out, b)?; Ok(out) }",
            "app@1/main/double(core@1/core/List[u8])->core@1/core/Result[core@1/core/List[u8],core@1/core/Capacity]",
        ),
        // Modes: `view` is the default and is not written; `mut` and `take` are words.
        // A missing result and a written `()` are both `->()`.
        (
            "main",
            "fn double(mut a: List[u8], take b: List[u8], c: u64) {}",
            "app@1/main/double(mut core@1/core/List[u8],take core@1/core/List[u8],u64)->()",
        ),
        ("main", "fn double() -> () {}", "app@1/main/double()->()"),
        // `waits` stays, as a word after the result.
        (
            "main",
            "fn add_one(take x: u64) -> u64 { x + 1 }\nfn double(tasks: Tasks) waits { let h = result.unwrap(task.spawn(tasks, bytes 64, 41u64, add_one)); let _ = result.unwrap(task.join(take h)) }",
            "app@1/main/double(core@1/core/Tasks)->()waits",
        ),
        // No generics list; a region generic is a root `in ^i`.
        (
            "main",
            "fn double[in s](root: Text in s, x: View[Text in s]) -> u64 { 0 }",
            "app@1/main/double(core@1/core/Text in ^0,core@1/core/View[core@1/core/Text in ^0])->u64",
        ),
        // The spec's second example, verbatim, in both declared orders: region generics
        // are numbered by first naming, parameters (Region ones too) are `#i`, and the
        // closure's pairs sort every `^` before every `#`.
        (
            "main",
            "fn edges[in b, in a](s: Region, r: Region, t: Text in a, u: Text in b) -> u64 where a: r, r: s { 0 }",
            "app@1/main/edges(core@1/core/Region,core@1/core/Region,core@1/core/Text in ^0,core@1/core/Text in ^1)->u64 where ^0:#0,^0:#1,#1:#0",
        ),
        (
            "main",
            "fn edges[in a, in b](s: Region, r: Region, t: Text in a, u: Text in b) -> u64 where a: r, r: s { 0 }",
            "app@1/main/edges(core@1/core/Region,core@1/core/Region,core@1/core/Text in ^0,core@1/core/Text in ^1)->u64 where ^0:#0,^0:#1,#1:#0",
        ),
        // A region generic named in neither the parameters nor the result takes the next
        // number: `b` is `^0`, `a` is `^1`.
        (
            "main",
            "fn double[in a, in b](x: Text in b) -> u64 where a: b { 0 }",
            "app@1/main/double(core@1/core/Text in ^0)->u64 where ^1:^0",
        ),
        // Two named in neither: declared order, `a` then `b`, whatever order the clauses
        // or their closure list them in (`a: c` by transitivity).
        (
            "main",
            "fn double[in a, in b, in c](x: Text in c) -> u64 where b: c, a: b { 0 }",
            "app@1/main/double(core@1/core/Text in ^0)->u64 where ^1:^0,^1:^2,^2:^0",
        ),
        // A root that is a parameter names it by position.
        (
            "main",
            "fn double(n: u64, r: Region, x: Text in r) -> u64 { n }",
            "app@1/main/double(u64,core@1/core/Region,core@1/core/Text in #1)->u64",
        ),
        // Outlives pairs sorted by `i` then `j`, each once.
        (
            "main",
            "fn double[in a, in b, in c](x: Text in a, y: Text in b, z: Text in c) -> u64 where c: b, a: b, a: b { 0 }",
            "app@1/main/double(core@1/core/Text in ^0,core@1/core/Text in ^1,core@1/core/Text in ^2)->u64 where ^0:^1,^2:^1",
        ),
        // A callable parameter: `own[send]`, its own modes and result, no names.
        (
            "main",
            "fn double(take k: own[send] fn(u64) -> u64, g: fn(mut u64)) -> u64 { k(1) }",
            "app@1/main/double(take own[send]fn(u64)->u64,fn(mut u64)->())->u64",
        ),
        // Every byte of a component other than a letter, digit or `_` is `%XX`, in the
        // function's identity and in every type path's.
        (
            "net-io",
            "type Doc { n: u64 }\nfn double(d: Doc) {}",
            "app@1/net%2Dio/double(app@1/net%2Dio/Doc)->()",
        ),
    ];
    let mut failures = vec![];
    for &(module, header, expected) in cases {
        let mut want = vec![1u8];
        want.extend_from_slice(expected.as_bytes());
        // The function under test is the one the expected identity names.
        let name = expected
            .split('/')
            .nth(2)
            .unwrap()
            .split('(')
            .next()
            .unwrap();
        let run = format!(
            "fn run() -> u64 {{\n    let _ = code.identity(code.describe({name}));\n    0\n}}\n"
        );
        let source = format!("{header}\n{run}pub fn main() -> Result[(), Exit] {{ Ok(()) }}\n");
        let (index, modules) = inputs_in(&source, &core_dir(), module);
        let program = Program::declare(&index, &modules);
        if !program.refusals.is_empty() || !program.gaps.is_empty() {
            failures.push(format!(
                "{header}: {:?} {:?}",
                program.refusals, program.gaps
            ));
            continue;
        }
        let target = program
            .fns
            .iter()
            .find(|fd| fd.path.package_is("app", "1") && fd.path.0.name == name)
            .unwrap();
        let direct = crate::identity::code_identity(&program.decls, &program.types, target);
        if direct.as_deref() != Some(&want[..]) {
            let got = direct.map(|b| String::from_utf8_lossy(&b[1..]).into_owned());
            failures.push(format!("{header}:\n  want {expected}\n  got  {got:?}"));
        }
        let mut found = false;
        for (f, fd) in program.fns.iter().enumerate() {
            if fd.body.is_none() || !fd.path.package_is("app", "1") {
                continue;
            }
            let mut emitter = Emitter::new();
            let check = program.check_lower(f, &mut emitter);
            if !check.refusals.is_empty() || !check.gaps.is_empty() {
                failures.push(format!("{header}: {:?} {:?}", check.refusals, check.gaps));
            }
            emitter.seal();
            found |= emitter
                .module
                .statics
                .iter()
                .any(|s| s.literal_bytes() == Some(&want));
        }
        if !found {
            failures.push(format!("{header}: code.identity does not read these bytes"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
