//! Test input comes from the spec itself; extracted programs are never tracked.
use super::*;
use std::{collections::HashMap, path::Path};

#[test]
fn every_live_spec_fence_parses_and_resolves_or_names_its_refusal() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let core = package("core");
    let library = PackageManifest::directory(&repo.join("core/core"), core.clone(), true).unwrap();
    let mut corpus = CorpusManifest {
        core: core.clone(),
        packages: vec![library],
    }
    .load()
    .unwrap();
    let mut files = std::fs::read_dir(repo.join("docs/spec"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect::<Vec<_>>();
    files.sort();
    let mut origins = HashMap::new();
    let (mut examples, mut rules) = (0, 0);
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        let mut active = false;
        let mut code = String::new();
        let mut first_line = 0;
        for (line_no, line) in text.split_inclusive('\n').enumerate() {
            if !active {
                if line.trim() == "```hirsel" {
                    active = true;
                    first_line = line_no + 2;
                }
            } else if line.trim() == "```" {
                active = false;
                let name = package(&format!("spec_{}", origins.len()));
                let file = path.with_extension(format!("md#{first_line}"));
                let source = ModuleSource {
                    file: file.clone(),
                    modules: vec!["sample".into()],
                    signatures: false,
                };
                let expected = code.lines().find_map(|l| {
                    l.split_once("refused:")
                        .and_then(|(_, r)| r.split_whitespace().next())
                        .map(|r| {
                            r.trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
                                .to_owned()
                        })
                });
                origins.insert(file.clone(), (path.clone(), first_line, expected));
                corpus.manifest.packages.push(PackageManifest {
                    package: name.clone(),
                    sources: vec![source],
                    dependencies: vec![core.clone()],
                });
                corpus.sources.push(CorpusSource {
                    module: ModulePath {
                        package: name,
                        modules: vec!["sample".into()],
                    },
                    file,
                    text: std::mem::take(&mut code),
                    mode: ParseMode::Source,
                });
                if path.file_name().unwrap() == "examples.md" {
                    examples += 1;
                } else {
                    rules += 1;
                }
            } else {
                code.push_str(line);
            }
        }
        assert!(!active, "unclosed Hirsel fence in {}", path.display());
    }
    assert!(
        examples > 0 && rules > 0,
        "spec corpus must execute examples and rule snippets"
    );
    let parsed = corpus.parse();
    let index = corpus.index(&parsed);
    let report = corpus.analyze(&parsed, &index);
    let mut unexpected = vec![];
    for file in &report.files {
        assert!(
            file.roundtrip && !file.resource_refusal,
            "{}",
            file.file.display()
        );
        for d in &file.diagnostics {
            let Some((path, start, expected)) = origins.get(&file.file) else {
                unexpected.push(format!(
                    "{}:{} {} {}",
                    file.file.display(),
                    d.line,
                    d.refusal.rule,
                    d.refusal.message
                ));
                continue;
            };
            if expected.as_deref() != Some(d.refusal.rule.as_str()) {
                unexpected.push(format!(
                    "{}:{}:{} {} {}",
                    path.display(),
                    start + d.line - 1,
                    d.column,
                    d.refusal.rule,
                    d.refusal.message
                ));
            }
        }
    }
    for (source, p) in corpus.sources.iter().zip(&parsed.files) {
        if p.diagnostics.is_empty() {
            let canonical = hirsel_core_syntax::format(p).unwrap();
            let q = parse(&canonical, source.mode, &mut Symbols::default());
            assert!(
                q.diagnostics.is_empty(),
                "{}: {:?}",
                source.file.display(),
                q.diagnostics
            );
            assert_eq!(
                hirsel_core_syntax::format(&q).as_deref(),
                Some(canonical.as_str()),
                "{}",
                source.file.display()
            );
        }
    }
    println!(
        "live spec: {examples} examples, {rules} rule snippets, {} core files; {} intentional refusals",
        report.files.len() - origins.len(),
        report
            .files
            .iter()
            .map(|f| f.diagnostics.len())
            .sum::<usize>()
    );
    assert!(unexpected.is_empty(), "{}", unexpected.join("\n"));
}
