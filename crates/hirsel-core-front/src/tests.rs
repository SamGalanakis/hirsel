//! The case corpus under `core/front/cases`: each file's `// expect:` lines are its
//! hand-derived oracle (see `core/front/README.md`).

use crate::*;
use hirsel_core_ids::PackageId;
use hirsel_core_resolve::{Environment, ModuleId, PackageManifest};
use hirsel_core_syntax::{ParseMode, parse};
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
    let p = Program::declare(&index, &modules);
    let main = ModuleId {
        package: package("app"),
        modules: vec!["main".into()],
    };
    let mut want_refusals = vec![];
    let mut want_gaps = vec![];
    let mut failures = vec![];
    let mut expects = 0;
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
                    "holds" => holds_names(d.summary.holds),
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
            _ => failures.push(format!("unreadable expectation `{e}`")),
        }
    }
    if expects == 0 {
        failures.push("no expectations".into());
    }
    let mut got_refusals: Vec<String> = p
        .refusals
        .iter()
        .map(|r| format!("{} at {}", r.rule, line_col(&modules, &r.primary)))
        .collect();
    got_refusals.sort();
    want_refusals.sort();
    if got_refusals != want_refusals {
        let detail: Vec<_> = p
            .refusals
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
    let mut got_gaps: Vec<String> = p.gaps.iter().map(|g| g.what.to_string()).collect();
    got_gaps.sort();
    want_gaps.sort();
    if got_gaps != want_gaps {
        failures.push(format!("gaps: want {want_gaps:?}, got {got_gaps:?}"));
    }
    failures
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
