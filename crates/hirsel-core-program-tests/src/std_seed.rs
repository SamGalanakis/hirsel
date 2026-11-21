//! The standard library seed, `core/std`: its library files and its contract tests
//! (`core/std/tests`), checked against the canonical core library `core/core`.
//!
//! `std_seed_parses_and_resolves` is the acceptance gate: any parse or name-resolution
//! refusal in a std or contract-test file fails it. `std_bodies_never_halt_silently` runs
//! the front-end slices over every library body and contract test and fails on a body
//! whose emission stopped without a refusal or a gap.

use crate::cases::{line_col, run_expect};
use crate::{Checked, package};
use hirsel_core_front::*;
use hirsel_core_ids::PackageId;
use hirsel_core_ir::Identity;
use hirsel_core_resolve::{Corpus, CorpusManifest, ModuleSource, PackageManifest, Refusal};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The `.hirsel` files directly in `dir`, each one module named `prefix` then its stem.
fn sources(dir: &Path, prefix: &[&str], signatures: bool) -> Vec<ModuleSource> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("source directory reads")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "hirsel"))
        .filter(|p| {
            prefix != ["tests"]
                || !std::fs::read_to_string(p)
                    .expect("std contract reads")
                    .lines()
                    .next()
                    .is_some_and(|line| line.starts_with("// test-of:"))
        })
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
                foreign: false,
                captured: None,
                file,
                modules,
                signatures,
            }
        })
        .collect()
}

#[test]
fn marked_contracts_run_only_in_the_native_walker() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    let marked = dir.join("private.hirsel");
    let public = dir.join("public.hirsel");
    let later = dir.join("later.hirsel");
    std::fs::write(&marked, "// test-of: std.pool\nfn private() {}\n").unwrap();
    std::fs::write(&public, "fn public() {}\n").unwrap();
    std::fs::write(
        &later,
        "// ordinary contract\n// test-of: std.pool\nfn later() {}\n",
    )
    .unwrap();
    let selected = sources(dir, &["tests"], false);
    assert_eq!(
        selected.iter().map(|s| &s.file).collect::<Vec<_>>(),
        [&later, &public]
    );
    // The marker is a tests-loader convention, never a library-source exclusion.
    assert_eq!(sources(dir, &[], false).len(), 3);
}

#[test]
fn core_contract_directory_is_not_part_of_the_std_seed() {
    let inputs = manifest();
    let core = inputs
        .packages
        .iter()
        .find(|p| p.package.name == "core")
        .unwrap();
    assert!(!core.sources.is_empty());
    assert!(core.sources.iter().all(|s| s.modules.len() == 1));
    assert!(
        core.sources
            .iter()
            .all(|s| s.file.parent() == Some(repo().join("core/core").as_path()))
    );
    // Root-only discovery does not hide invalid real source files.
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    std::fs::create_dir(dir.join("tests")).unwrap();
    std::fs::write(dir.join("tests/poison.hirsel"), "fn poison(x: Missing) {}").unwrap();
    std::fs::write(dir.join("bad.hirsel"), "fn bad(").unwrap();
    let selected = sources(dir, &[], true);
    assert_eq!(selected.len(), 1);
    let mut symbols = hirsel_core_syntax::Symbols::default();
    let package = PackageManifest {
        package: package("core"),
        sources: selected,
        dependencies: vec![],
    };
    assert!(
        !package.read(&mut symbols).unwrap()[0]
            .parsed
            .diagnostics
            .is_empty()
    );
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

fn seed_corpus() -> Corpus {
    let mut corpus = manifest().load().expect("std seed reads");
    for source in &mut corpus.sources {
        if !is_seed(&source.module.package)
            || !is_test(source)
            || !source.text.starts_with("// test-vectors\n")
        {
            continue;
        }
        // The native walker appends the same generator's definitions to this module.
        let generator = source
            .file
            .parent()
            .unwrap()
            .join("vectors")
            .join(source.file.file_stem().unwrap())
            .join("generate.py");
        let output = Command::new("python3")
            .arg(&generator)
            .output()
            .unwrap_or_else(|error| panic!("run {}: {error}", generator.display()));
        assert!(
            output.status.success(),
            "{}: {}: {}",
            generator.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        source.text.push('\n');
        source.text.push_str(
            &String::from_utf8(output.stdout).expect("fixture generator emits UTF-8 Hirsel source"),
        );
    }
    corpus
}

#[test]
fn std_seed_parses_and_resolves() {
    let corpus = seed_corpus();
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

/// A std or core body whose emission stopped without a refusal or a gap would be counted
/// accepted while its program silently computes something else (`list.sum` returned 0).
#[test]
fn std_bodies_never_halt_silently() {
    let halted: Vec<String> = std_seed_check()
        .lines()
        .filter(|l| l.contains("emission halted without a reported reason"))
        .map(str::to_string)
        .collect();
    assert!(halted.is_empty(), "{}", halted.join("\n"));
}

/// The checker's outcome per std body and per contract test, as a markdown report.
fn std_seed_check() -> String {
    check_corpus(seed_corpus())
}

#[test]
fn hmac_rfc_4231_vectors() {
    let mut inputs = manifest();
    for package in &mut inputs.packages {
        if is_seed(&package.package) {
            package.sources.retain(|source| {
                source.modules == ["sha256"]
                    || source.modules == ["hmac"]
                    || source.modules == ["tests", "hmac"]
            });
        }
    }
    let checked = Checked::corpus(&inputs.load().unwrap());
    for case in 1..=7 {
        let name = format!("rfc_4231_case_{case}");
        let path =
            hirsel_core_ids::DefPath::from_full(&format!("std@1/tests/hmac/{name}")).unwrap();
        let root = Identity::plain(path);
        let units = checked.product.units([root.clone()], 65_536).unwrap();
        let (module, function) =
            hirsel_core_materialize::join(&units.modules().unwrap(), &root).unwrap();
        hirsel_core_ir::wf::module(&module).unwrap();
        let call = format!("{name}()");
        let error = run_expect(
            &module,
            &[(name, function)].into(),
            &call,
            &["true"],
            &|a| format!("{a:?}"),
        );
        assert!(error.is_none(), "{call}: {error:?}");
    }
}

fn check_corpus(corpus: hirsel_core_resolve::Corpus) -> String {
    let parsed = corpus.parse();
    let index = corpus.index(&parsed);
    let modules: Vec<ModuleInput> = corpus
        .sources
        .iter()
        .zip(parsed.files)
        .enumerate()
        .map(|(source_id, (s, p))| ModuleInput {
            source: hirsel_core_resolve::SourceId(source_id as u32),
            id: index.module(&s.module).unwrap(),
            syntax: p.syntax,
        })
        .collect();
    let p = Program::declare(&index, &modules);
    let checked = Checked::corpus(&corpus);
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
    let mut rows: Vec<(String, String, String)> = vec![];
    let mut emitted: HashMap<String, HashMap<String, hirsel_core_ids::DefPath>> = HashMap::new();
    for fd in p.fns.iter() {
        let m = &fd.path.0;
        if !is_reported(&m.package) || fd.body.is_none() {
            continue;
        }
        let module = label(&m.package, &m.modules);
        let c = &checked.product.definitions[&fd.path].check;
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
        } else if c.function.is_some() {
            emitted
                .entry(module.clone())
                .or_default()
                .insert(m.name.clone(), fd.path.clone());
            "accepted".into()
        } else {
            "not emitted".into()
        };
        rows.push((module, m.name.clone(), outcome));
    }
    for ((module, item), why) in by_item {
        rows.push((
            module,
            format!("declaration item {item}"),
            format!("refused: {}", why.join("; ")),
        ));
    }
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
                    let root = Identity::plain(functions[name].clone());
                    let units = checked
                        .product
                        .units([root.clone()], 65_536)
                        .map_err(|e| format!("units: {e:?}"));
                    let (module, function) = units
                        .and_then(|u| {
                            hirsel_core_materialize::join(
                                &u.modules().map_err(|e| e.to_string())?,
                                &root,
                            )
                        })
                        .map_err(|e| format!("assembly: {e}"))?;
                    hirsel_core_ir::wf::module(&module)
                        .map_err(|e| format!("not well formed: {e}"))?;
                    Ok::<_, String>(run_expect(
                        &module,
                        &[(name.to_string(), function)].into(),
                        call,
                        &words,
                        &|a| format!("{a:?}"),
                    ))
                }));
                match run {
                    Ok(Ok(None)) => "pass".to_string(),
                    Ok(Ok(Some(f))) => format!("FAIL: {f}"),
                    Ok(Err(f)) => format!("FAIL: {f}"),
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
        "Items: {} accepted, {} refused, {} gap, {} not emitted.\n",
        count("accepted"),
        count("refused"),
        count("gap"),
        count("not emitted")
    );
    let _ = writeln!(out, "| Module | Item | Outcome |\n|---|---|---|");
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
fn float_constant_tables_are_read_only_data() {
    use hirsel_core_ir::{Inst, Step};
    let corpus = seed_corpus();
    let parsed = corpus.parse();
    let index = corpus.index(&parsed);
    let modules: Vec<_> = corpus
        .sources
        .iter()
        .zip(parsed.files)
        .enumerate()
        .map(|(source_id, (s, p))| ModuleInput {
            source: hirsel_core_resolve::SourceId(source_id as u32),
            id: index.module(&s.module).unwrap(),
            syntax: p.syntax,
        })
        .collect();
    let p = Program::declare(&index, &modules);
    let checked = Checked::corpus(&corpus);
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
    let definition = hirsel_core_ids::DefPath::from_full("std@1/float/shift_for").unwrap();
    let published = &checked.product.definitions[&definition];
    assert!(published.check.refusals.is_empty() && published.incomplete.is_empty());
    let module = published.ir().unwrap();
    let body = module.functions[published.check.function.unwrap().index()]
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
    assert_eq!(module.statics.len(), 1);
    let recipes = hirsel_core_layout::compute(&module.types).unwrap();
    assert_eq!(
        module.statics[0].native(&module.types, &recipes).1.len(),
        19 * 8
    );
    println!(
        "std.float.shift_for SHIFTS: {} CoreIR instructions per lookup, no table stores/copies",
        end + 1 - start
    );
    println!("{}", hirsel_core_ir::text::print(&module));
}

/// Rigid numeric operands used to disappear, making every nonempty sum return zero.
/// These existing core contracts exercise closed helpers with u8/i8/u64/i64 rows.
#[test]
fn generic_numeric_core_contracts() {
    let corpus = seed_corpus();
    let parsed = corpus.parse();
    let index = corpus.index(&parsed);
    let modules: Vec<_> = corpus
        .sources
        .iter()
        .zip(parsed.files)
        .enumerate()
        .map(|(source_id, (s, p))| ModuleInput {
            source: hirsel_core_resolve::SourceId(source_id as u32),
            id: index.module(&s.module).unwrap(),
            syntax: p.syntax,
        })
        .collect();
    let p = Program::declare(&index, &modules);
    let checked = Checked::corpus(&corpus);
    let mut functions = HashMap::new();
    for fd in p.fns.iter() {
        if fd.body.is_none()
            || !(fd.path.package_is("core", "1")
                || fd.path.0.modules == ["tests", "int"]
                || fd.path.0.modules == ["tests", "list"])
        {
            continue;
        }
        let check = &checked.product.definitions[&fd.path].check;
        assert!(
            check.refusals.is_empty() && check.gaps.is_empty(),
            "{}: {check:?}",
            fd.path
        );
        functions.insert(fd.path.0.name.clone(), fd.path.clone());
    }
    for (call, want) in [
        ("parse_u8_cases()", &["true"][..]),
        ("parse_i8_cases()", &["true"][..]),
        ("parse_u64_limits()", &["18446744073709551615"][..]),
        ("parse_i64_min()", &["-9223372036854775808"][..]),
        ("sum_empty()", &["0"][..]),
        ("sum_to_max()", &["255"][..]),
        ("sum_overflow()", &["fail", "Overflow"][..]),
        ("sum_signed()", &["-2"][..]),
    ] {
        let name = call.split('(').next().unwrap();
        let root = Identity::plain(functions[name].clone());
        let units = checked.product.units([root.clone()], 65_536).unwrap();
        let (module, function) =
            hirsel_core_materialize::join(&units.modules().unwrap(), &root).unwrap();
        hirsel_core_ir::wf::module(&module).unwrap();
        let error = run_expect(
            &module,
            &[(name.to_string(), function)].into(),
            call,
            want,
            &|a| format!("{a:?}"),
        );
        assert!(error.is_none(), "{call}: {error:?}");
    }
}

#[test]
fn seed_report_keeps_refusals_on_constant_items() {
    let mut corpus = manifest().load().unwrap();
    // Keep the real core package but replace std bodies with a constant-only module.
    corpus.sources.retain(|s| s.module.package.name == "core");
    let std = corpus
        .manifest
        .packages
        .iter_mut()
        .find(|p| p.package.name == "std")
        .unwrap();
    std.sources.truncate(1);
    let source = &std.sources[0];
    corpus.sources.push(hirsel_core_resolve::CorpusSource {
        module: hirsel_core_resolve::ModulePath {
            package: std.package.clone(),
            modules: source.modules.clone(),
        },
        file: source.file.clone(),
        text: "const BAD: u8 = 256".into(),
        mode: hirsel_core_syntax::ParseMode::Source,
    });
    let report = check_corpus(corpus);
    assert!(
        report.contains("declaration item 0 | refused: LANG-20"),
        "{report}"
    );
}
