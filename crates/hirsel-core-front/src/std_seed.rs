//! The standard library seed, `core/std`: its library files and its contract tests
//! (`core/std/tests`), checked against the canonical core library `core/core`.
//!
//! `std_seed_parses_and_resolves` is the acceptance gate: any parse or name-resolution
//! refusal in a std or contract-test file fails it. `std_seed_check_report` runs the
//! front-end slices over every library body and contract test and prints the outcome per
//! operation to the test log. It reports the checker's progress, so it asserts nothing,
//! writes nothing to the tree, and is run by hand:
//! `cargo test -p hirsel-core-front std_seed_check_report -- --ignored --nocapture`.

use super::{line_col, package, run_expect};
use crate::*;
use hirsel_core_ids::PackageId;
use hirsel_core_resolve::{CorpusManifest, ModuleSource, PackageManifest};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The `.hirsel` files directly in `dir`, each one module named `prefix` then its stem.
fn sources(dir: &Path, prefix: &[&str], signatures: bool) -> Vec<ModuleSource> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("source directory reads")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "hirsel"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|file| {
            let stem = file
                .file_stem()
                .and_then(|s| s.to_str())
                .expect("UTF-8 name");
            let mut modules: Vec<String> = prefix.iter().map(|s| s.to_string()).collect();
            modules.push(stem.to_string());
            ModuleSource {
                file,
                modules,
                signatures,
            }
        })
        .collect()
}

/// core (declarations) and std (library code, depends on core), laid out as the canonical
/// image reads `core/std`: one package, the contract tests its modules `tests.<module>`.
fn manifest() -> CorpusManifest {
    let root = repo();
    let core = package("core");
    let mut std_sources = sources(&root.join("core/std"), &[], false);
    std_sources.extend(sources(&root.join("core/std/tests"), &["tests"], false));
    CorpusManifest {
        core: core.clone(),
        packages: vec![
            PackageManifest {
                package: core.clone(),
                sources: sources(&root.join("core/core"), &[], true),
                dependencies: vec![],
            },
            PackageManifest {
                package: package("std"),
                sources: std_sources,
                dependencies: vec![core],
            },
        ],
    }
}

fn is_seed(p: &PackageId) -> bool {
    p.name == "std"
}

/// The report covers std's bodies and core's (core-form-883: class C operations).
fn is_reported(p: &PackageId) -> bool {
    is_seed(p) || p.name == "core"
}

/// A row's module: std's as written, core's prefixed, since std extends core's modules.
fn label(p: &PackageId, modules: &[String]) -> String {
    let m = modules.join(".");
    if is_seed(p) { m } else { format!("core.{m}") }
}

fn is_test(s: &hirsel_core_resolve::CorpusSource) -> bool {
    s.module.modules.first().is_some_and(|m| m == "tests")
}

#[test]
fn std_seed_parses_and_resolves() {
    let corpus = manifest().load().expect("std seed reads");
    let parsed = corpus.parse();
    let index = corpus.index(&parsed);
    let report = corpus.analyze(&parsed, &index);
    let mut refusals = vec![];
    for f in report.files.iter().filter(|f| is_seed(&f.module.package)) {
        for d in &f.diagnostics {
            refusals.push(format!(
                "{}:{}:{} {} {}",
                f.file.display(),
                d.line,
                d.column,
                d.refusal.rule,
                d.refusal.message
            ));
        }
        if f.resource_refusal {
            refusals.push(format!("{}: resource refusal", f.file.display()));
        }
    }
    assert!(refusals.is_empty(), "{}", refusals.join("\n"));
}

#[test]
#[ignore = "prints the checker's outcome per std body; run by hand with --nocapture"]
fn std_seed_check_report() {
    print!("{}", std_seed_check());
}

/// A std or core body whose emission stopped without a refusal or a gap would be counted
/// accepted while its program silently computes something else (`list.sum` returned 0).
#[test]
fn std_bodies_never_halt_silently() {
    let halted: Vec<String> = std_seed_check()
        .lines()
        .filter(|l| l.contains(crate::body::SILENT_HALT))
        .map(str::to_string)
        .collect();
    assert!(halted.is_empty(), "{}", halted.join("\n"));
}

/// The checker's outcome per std body and per contract test, as a markdown report.
fn std_seed_check() -> String {
    let corpus = manifest().load().expect("std seed reads");
    let parsed = corpus.parse();
    let index = corpus.index(&parsed);
    let modules: Vec<ModuleInput> = corpus
        .sources
        .iter()
        .zip(parsed.files)
        .map(|(s, p)| ModuleInput {
            id: index.module(&s.module).unwrap(),
            syntax: p.syntax,
        })
        .collect();
    let p = Program::declare(&index, &modules);
    let describe = |r: &Refusal| {
        format!(
            "{} at {}: {}",
            r.rule,
            line_col(&modules, &r.primary),
            r.message.replace('|', "\\|")
        )
    };
    // Declaration and signature refusals, by the item they are in.
    let mut by_item: BTreeMap<(String, u32), Vec<String>> = BTreeMap::new();
    for r in p
        .refusals
        .iter()
        .filter(|r| is_reported(&index.module_path(r.primary.module).package))
    {
        let m = index.module_path(r.primary.module);
        by_item
            .entry((label(&m.package, &m.modules), r.primary.item))
            .or_default()
            .push(describe(r));
    }
    let mut emitter = Emitter::new();
    let mut rows: Vec<(String, String, String)> = vec![];
    let mut emitted: HashMap<String, HashMap<String, hirsel_core_ir::FuncId>> = HashMap::new();
    for (f, fd) in p.fns.iter().enumerate() {
        let m = &fd.path.0;
        if !is_reported(&m.package) || fd.body.is_none() {
            continue;
        }
        let module = label(&m.package, &m.modules);
        let c = p.check_lower(f, &mut emitter);
        let mut why: Vec<String> = by_item
            .remove(&(module.clone(), fd.at.item))
            .unwrap_or_default();
        why.extend(c.refusals.iter().map(describe));
        let outcome = if !why.is_empty() {
            format!("refused: {}", why.join("; "))
        } else if !c.gaps.is_empty() {
            let mut g: Vec<String> = c
                .gaps
                .iter()
                .map(|g| format!("{} at {}", g.what, line_col(&modules, &g.site)))
                .collect();
            g.dedup();
            format!("gap: {}", g.join("; "))
        } else if let Some(id) = c.function {
            emitted
                .entry(module.clone())
                .or_default()
                .insert(m.name.clone(), id);
            "accepted".into()
        } else {
            "not emitted".into()
        };
        rows.push((module, m.name.clone(), outcome));
    }
    emitter
        .materialize_instances()
        .expect("materialize seed instances");
    // Contract tests: `// expect: run f() = R` before a test function, run when emitted.
    let mut runs = vec![];
    for s in corpus.sources.iter().filter(|s| is_test(s)) {
        let module = s.module.modules.join(".");
        let none = HashMap::new();
        let functions = emitted.get(&module).unwrap_or(&none);
        for line in s.text.lines() {
            let Some(e) = line.trim().strip_prefix("// expect: run ") else {
                continue;
            };
            let Some((call, want)) = e.split_once(" = ") else {
                runs.push((module.clone(), e.to_string(), "unreadable".to_string()));
                continue;
            };
            let name = call.split('(').next().unwrap_or("");
            let outcome = if call.contains("(dir)") {
                // The CoreIR evaluator has no host to give a directory.
                "not run: needs a host directory".to_string()
            } else if !functions.contains_key(name) {
                "not run: not emitted".to_string()
            } else {
                let words: Vec<&str> = want.split_whitespace().collect();
                // The evaluator stops (panics) at a function it has no body for, such as a
                // core operation that is only a signature.
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_expect(&emitter.module, functions, call, &words)
                }));
                match run {
                    Ok(None) => "pass".to_string(),
                    Ok(Some(f)) => format!("FAIL: {f}"),
                    Err(_) => "not run: reaches a function with no body".to_string(),
                }
            };
            runs.push((module.clone(), e.to_string(), outcome));
        }
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Front-end check of the std seed\n\nParse and resolve refusals are a test failure \
         (`std_seed_parses_and_resolves`); this table records what the checker does with each \
         body.\n"
    );
    let count = |k: &str| rows.iter().filter(|r| r.2.starts_with(k)).count();
    let _ = writeln!(
        out,
        "Bodies: {} accepted, {} refused, {} gap, {} not emitted.\n",
        count("accepted"),
        count("refused"),
        count("gap"),
        count("not emitted")
    );
    let _ = writeln!(out, "| Module | Function | Outcome |\n|---|---|---|");
    for (m, f, o) in &rows {
        let _ = writeln!(out, "| {m} | {f} | {o} |");
    }
    let _ = writeln!(
        out,
        "\n## Contract tests run\n\n| Module | Run | Outcome |\n|---|---|---|"
    );
    for (m, r, o) in &runs {
        let _ = writeln!(out, "| {m} | `{r}` | {} |", o.replace('|', "\\|"));
    }
    out
}

#[test]
#[ignore = "explicit std constant-table inspection"]
fn float_constant_tables_are_read_only_data() {
    use hirsel_core_ir::{Inst, Step};
    let corpus = manifest().load().unwrap();
    let parsed = corpus.parse();
    let index = corpus.index(&parsed);
    let modules: Vec<_> = corpus
        .sources
        .iter()
        .zip(parsed.files)
        .map(|(s, p)| ModuleInput {
            id: index.module(&s.module).unwrap(),
            syntax: p.syntax,
        })
        .collect();
    let p = Program::declare(&index, &modules);
    for name in ["SHIFTS", "POW10_64", "POW10_32", "POW5"] {
        let constant = p
            .constants
            .iter()
            .find(|c| c.path.0.modules == ["float"] && c.path.0.name == name)
            .unwrap();
        assert!(
            constant.value.is_some(),
            "{name}: {:?} {:?}",
            p.refusals,
            p.gaps
        );
    }
    let f = p
        .fns
        .iter()
        .position(|f| {
            f.path.0.package.name == "std"
                && f.path.0.modules == ["float"]
                && f.path.0.name == "shift_for"
        })
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
    let start = body
        .insts
        .iter()
        .position(|i| matches!(i, Inst::ReadStatic { .. }))
        .unwrap();
    let end = body
        .insts
        .iter()
        .enumerate()
        .skip(start)
        .find_map(|(i, inst)| match inst {
            Inst::Read { address, .. }
                if body.addresses[address.index()]
                    .steps
                    .get(&body.pools.steps)
                    .unwrap()
                    .iter()
                    .any(|s| matches!(s, Step::Element(_))) =>
            {
                Some(i)
            }
            _ => None,
        })
        .unwrap();
    assert!(
        !body.insts[start..=end]
            .iter()
            .any(|i| matches!(i, Inst::Store { .. } | Inst::Copy { .. }))
    );
    assert_eq!(emitter.module.statics.len(), 1);
    assert_eq!(emitter.module.statics[0].bytes.len(), 19 * 8);
    println!(
        "std.float.shift_for SHIFTS: {} CoreIR instructions per lookup, no table stores/copies",
        end + 1 - start
    );
    println!("{}", hirsel_core_ir::text::print(&emitter.module));
}

/// Rigid numeric operands used to disappear from emission, making sum return zero.
/// Until typed numeric helpers are materialized, these real source bodies must GAP.
#[test]
fn generic_scalar_operations_are_explicit_gaps() {
    let corpus = manifest().load().unwrap();
    let parsed = corpus.parse();
    let index = corpus.index(&parsed);
    let modules: Vec<_> = corpus
        .sources
        .iter()
        .zip(parsed.files)
        .map(|(s, p)| ModuleInput {
            id: index.module(&s.module).unwrap(),
            syntax: p.syntax,
        })
        .collect();
    let p = Program::declare(&index, &modules);
    let mut emitter = Emitter::new();
    let mut checked = 0;
    for (f, fd) in p.fns.iter().enumerate() {
        if fd.path.package_is("core", "1")
            && matches!(
                (fd.path.0.modules[0].as_str(), fd.path.0.name.as_str()),
                ("int", "parse") | ("list", "sum")
            )
        {
            let check = p.check_lower(f, &mut emitter);
            assert!(check.refusals.is_empty(), "{check:?}");
            assert!(check.function.is_none(), "{check:?}");
            assert!(
                check
                    .gaps
                    .iter()
                    .any(|g| g.what == "generic scalar operations are not built"),
                "{check:?}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 2);
}
