//! The case corpus under `core/front/cases`: each file's `// expect:` lines are its
//! hand-derived oracle (see `core/front/README.md`).

use crate::*;
use hirsel_core_ids::PackageId;
use hirsel_core_ir::{FuncId, Module, Param, Ty, eval, wf};
use hirsel_core_resolve::{Environment, ModuleId, PackageManifest};
use hirsel_core_syntax::{ParseMode, parse};
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
    let core = PackageManifest::directory(&root().join("core"), package("core"), true)
        .and_then(|m| m.read())
        .expect("core fixture reads");
    let mut modules = vec![];
    for m in core {
        assert!(
            m.parsed.diagnostics.is_empty(),
            "{}: {:?}",
            m.file.display(),
            m.parsed.diagnostics
        );
        modules.push(ModuleInput {
            id: m.module,
            syntax: m.parsed.syntax,
        });
    }
    let main = ModuleId {
        package: package("app"),
        modules: vec!["main".into()],
    };
    let parsed = parse(source, ParseMode::Source);
    let mut index = NameIndex::new(Environment::default());
    for d in &parsed.diagnostics {
        index.diagnostics.push(Refusal::syntax(main.clone(), d));
    }
    modules.push(ModuleInput {
        id: main,
        syntax: parsed.syntax,
    });
    for m in &modules {
        index.add_module(&m.id);
        for (i, item) in m.syntax.items.iter().enumerate() {
            index.index_item(&m.id, i as u32, item);
        }
    }
    index.configure_core(package("core"));
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
    let main = ModuleId {
        package: package("app"),
        modules: vec!["main".into()],
    };
    p.entry = Some(main.definition("main"));
    // Every body of the application module, checked once, in order.
    let mut emitter = Emitter::new();
    let mut refusals = p.refusals.clone();
    let mut gaps = p.gaps.clone();
    let mut functions = HashMap::new();
    for (f, fd) in p.fns.iter().enumerate() {
        if fd.path.0.package != main.package || fd.body.is_none() {
            continue;
        }
        let c = p.check_lower(f, &mut emitter);
        refusals.extend(c.refusals);
        gaps.extend(c.gaps);
        if let Some(id) = c.function {
            functions.insert(fd.path.0.name.clone(), id);
        }
    }
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
                let Some(d) = p.decl(&main.definition(ty)) else {
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

/// `f(a,b)` run against `= r1,r2` or `= fail Cause`: arguments and results are read at
/// the emitted signature's types, so a case writes values, not bits.
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
    let got = eval::run(module, f, &args, u64::MAX).outcome;
    let expected = match want {
        ["fail", cause] => eval::Outcome::Failed(vec![(*cause).to_string()]),
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
