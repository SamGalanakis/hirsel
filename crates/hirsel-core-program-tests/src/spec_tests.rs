//! Check the source and output in the spec without maintaining a second fixture corpus.
use crate::{Checked, package, repo, std_manifest};
use hirsel_core_driver::{SourceSite, line_column};
use hirsel_core_ids::DefPath;
use hirsel_core_ir::{Identity, eval};
use hirsel_core_layout::{Layout, case_of, compute};
use hirsel_core_resolve::{CorpusManifest, ModuleSource, PackageManifest};
use hirsel_core_syntax::{NodeKind, ParseMode, Symbols, TokenKind, format, parse};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

struct Fence {
    path: PathBuf,
    line: usize,
    source: String,
    signatures: bool,
    entry: bool,
    rules: BTreeSet<String>,
    run: Option<Run>,
}

struct Run {
    stdout: String,
    outcome: ExpectedOutcome,
}

enum ExpectedOutcome {
    Success,
    Failure { cause: String, site: (u32, u32) },
}

impl Fence {
    fn location(&self) -> String {
        format!("{}:{}", self.path.display(), self.line)
    }
}

type Lines<'a> = std::iter::Enumerate<std::str::SplitInclusive<'a, char>>;

fn body(lines: &mut Lines<'_>, location: &str) -> String {
    let mut source = String::new();
    for (_, line) in lines {
        if line.trim() == "```" {
            return source;
        }
        source.push_str(line);
    }
    panic!("{location}: unclosed fence");
}

fn expected_run(lines: &mut Lines<'_>, location: &str) -> Run {
    let (_, output) = lines
        .find(|(_, line)| line.trim_start().starts_with("```") || line.starts_with("## "))
        .unwrap_or_else(|| panic!("{location}: missing stdout fence"));
    assert_eq!(
        output.trim(),
        "```text",
        "{location}: expected stdout fence"
    );
    let stdout = body(lines, location);
    let (_, outcome) = lines
        .find(|(_, line)| !line.trim().is_empty())
        .unwrap_or_else(|| panic!("{location}: missing outcome"));
    let outcome = match outcome.trim().strip_prefix("Outcome: ") {
        Some(outcome) if outcome.starts_with("success.") => ExpectedOutcome::Success,
        Some(outcome) if outcome.starts_with("failure, primary cause `") => {
            let failure = outcome.strip_prefix("failure, primary cause `").unwrap();
            let (cause, site) = failure
                .split_once("` at ")
                .unwrap_or_else(|| panic!("{location}: expected a failure cause and site"));
            let (line, column) = site
                .trim_end_matches('.')
                .split(';')
                .next()
                .unwrap()
                .split_once(':')
                .unwrap_or_else(|| panic!("{location}: expected a failure site"));
            ExpectedOutcome::Failure {
                cause: cause.split('(').next().unwrap().to_owned(),
                site: (line.parse().unwrap(), column.parse().unwrap()),
            }
        }
        _ => panic!("{location}: unrecognized outcome {outcome:?}"),
    };
    Run { stdout, outcome }
}

fn read_fences(path: &Path, failures: &mut Vec<String>, unbuilt: &mut usize) -> Vec<Fence> {
    let markdown = fs::read_to_string(path).unwrap();
    let mut lines = markdown.split_inclusive('\n').enumerate();
    let mut fences = Vec::new();
    while let Some((at, line)) = lines.next() {
        let Some(info) = line.trim().strip_prefix("```") else {
            continue;
        };
        let location = format!("{}:{}", path.display(), at + 1);
        let source = body(&mut lines, &location);
        let mode = match info {
            "hirsel" | "hirsel run" => ParseMode::Source,
            "hirsel core" => ParseMode::Signatures,
            "hirsel unbuilt" => {
                *unbuilt += 1;
                continue;
            }
            "" | "ebnf" | "sh" | "text" => continue,
            _ => panic!("{location}: unknown fence info string {info:?}"),
        };
        let signatures = mode == ParseMode::Signatures;
        let run = (info == "hirsel run").then(|| expected_run(&mut lines, &location));
        let mut symbols = Symbols::default();
        let parsed = parse(&source, mode, &mut symbols);
        if parsed.syntax.text() != source {
            failures.push(format!("{location}: parser lost source bytes"));
        }
        if let Some(canonical) = format(&parsed) {
            let reparsed = parse(&canonical, mode, &mut Symbols::default());
            if format(&reparsed).as_deref() != Some(canonical.as_str()) {
                failures.push(format!("{location}: formatting failed to round trip"));
            }
        }
        let rules = parsed
            .syntax
            .tree
            .tokens
            .iter()
            .filter(|token| token.kind == TokenKind::LineComment)
            .filter_map(|token| {
                source[token.span.start as usize..token.span.end as usize]
                    .strip_prefix("// refused:")
            })
            .map(|comment| {
                let rule = comment
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
                assert!(
                    rule.split_once('-').is_some_and(|(family, number)| {
                        !family.is_empty()
                            && family.bytes().all(|c| c.is_ascii_uppercase())
                            && !number.is_empty()
                            && number.bytes().all(|c| c.is_ascii_digit())
                    }),
                    "{location}: expected a rule ID after // refused:"
                );
                rule.to_owned()
            })
            .collect::<BTreeSet<_>>();
        let entry = !signatures
            && parsed.syntax.items().any(|item| {
                let root = item.tree.node(item.root);
                root.kind == NodeKind::Function
                    && root
                        .token
                        .is_some_and(|t| symbols.text(item.symbol(t)) == "main")
                    && item
                        .tree
                        .children(item.root)
                        .iter()
                        .any(|&child| item.tree.node(child).kind == NodeKind::Public)
            });
        if run.is_some() {
            assert!(
                entry && rules.is_empty(),
                "{location}: run fence needs a public main and no refusal annotations"
            );
        }
        fences.push(Fence {
            path: path.to_owned(),
            line: at + 2,
            source,
            signatures,
            entry,
            rules,
            run,
        });
    }
    fences
}

fn diagnostic_location(fence: Option<&&Fence>, site: &SourceSite) -> String {
    if let Some(fence) = fence {
        let (line, col) = line_column(fence.source.as_bytes(), site.span.start as usize);
        format!("{}:{}:{col}", fence.path.display(), fence.line + line - 1)
    } else {
        format!("{} at byte {}", site.module, site.span.start)
    }
}

fn check_fences(fences: &[&Fence], failures: &mut Vec<String>) {
    let files = tempfile::tempdir().unwrap();
    let core = package("core");
    let mut library =
        PackageManifest::directory(&repo().join("core/core"), core.clone(), true).unwrap();
    let mut packages = Vec::new();
    let mut origins = BTreeMap::new();
    let mut entry = None;
    for (i, &fence) in fences.iter().enumerate() {
        let sample = format!("spec_{i}");
        let file = files.path().join(format!("{sample}.hirsel"));
        fs::write(&file, &fence.source).unwrap();
        let source = ModuleSource {
            foreign: false,
            captured: None,
            file,
            modules: vec![sample.clone()],
            signatures: fence.signatures,
        };
        let pkg = if fence.signatures {
            library.sources.push(source);
            core.clone()
        } else {
            let name = package(&sample);
            let mut dependencies = vec![core.clone()];
            if fence.source.contains("std.") {
                dependencies.push(package("std"));
            }
            packages.push(PackageManifest {
                namespace: None,
                package: name.clone(),
                sources: vec![source],
                dependencies,
            });
            name
        };
        let module = format!("{}@{}/{sample}", pkg.name, pkg.version);
        if fence.entry {
            assert_eq!(fences.len(), 1, "a check request selects a single entry");
            entry = DefPath::from_full(&format!("{module}/main"));
            assert!(entry.is_some());
        }
        origins.insert(module, fence);
    }
    if packages
        .iter()
        .any(|p| p.dependencies.contains(&package("std")))
    {
        packages.push(std_manifest());
    }
    packages.push(library);
    let checked = Checked::check(CorpusManifest { core, packages }, files, entry.clone());
    let product = &checked.product;
    let mut seen = BTreeSet::new();
    for diagnostic in product.machine_diagnostics().unwrap() {
        let fence = origins.get(&diagnostic.primary.module);
        if fence.is_some_and(|f| f.rules.contains(diagnostic.rule)) {
            seen.insert((diagnostic.primary.module, diagnostic.rule));
        } else {
            failures.push(format!(
                "{}: unexpected {} ({})",
                diagnostic_location(fence, &diagnostic.primary),
                diagnostic.rule,
                diagnostic.message
            ));
        }
    }
    for (module, fence) in &origins {
        for rule in &fence.rules {
            if !seen.contains(&(module.clone(), rule.as_str())) {
                failures.push(format!("{}: missing {rule}", fence.location()));
            }
        }
    }
    for gap in product.machine_incomplete().unwrap() {
        let fence = origins.get(&gap.primary.module);
        if fence.is_none_or(|f| f.rules.is_empty()) {
            failures.push(format!(
                "{}: incomplete check ({})",
                diagnostic_location(fence, &gap.primary),
                gap.message
            ));
        }
    }
    for resource in product.machine_resources().unwrap() {
        failures.push(format!(
            "{}: resource refusal ({})",
            diagnostic_location(origins.get(&resource.primary.module), &resource.primary),
            resource.message
        ));
    }
    if let Some(run) = fences.first().and_then(|fence| fence.run.as_ref()) {
        let location = fences[0].location();
        if !product.accepted() {
            failures.push(format!("{location}: run skipped after check failure"));
        } else if let Err(error) = evaluate(product, entry.unwrap(), run) {
            failures.push(format!("{location}: {error}"));
        }
    }
}

fn evaluate(
    product: &hirsel_core_driver::CheckProduct,
    entry: DefPath,
    run: &Run,
) -> Result<(), String> {
    const ROOT_LIMIT: u64 = 1_048_576;
    const INSTRUCTIONS: u64 = 1_000_000_000;
    let root = Identity::plain(entry);
    let units = product
        .units([root.clone()], 65_536)
        .map_err(|e| format!("{e:?}"))?;
    let revisions = units
        .revisions()
        .ok_or("missing revisions for the reached units")?
        .into_iter()
        .map(|(identity, revision)| (identity.digest(), revision))
        .collect();
    let (module, function) =
        hirsel_core_materialize::join(&units.modules().map_err(|e| e.to_string())?, &root)?;
    hirsel_core_ir::wf::module(&module).map_err(|e| format!("invalid IR: {e}"))?;

    let signature = &module.signatures[module.functions[function.index()].signature.index()];
    let layouts = compute(&module.types).map_err(|e| format!("invalid layout: {e:?}"))?;
    let result = signature.dest.ok_or("main has no Result destination")?;
    let Layout::Fixed(layout) = &layouts[result.index()].layout else {
        return Err("main's Result has no fixed layout".into());
    };
    let tag = layout.tag.ok_or("main's Result has no variant tag")?;
    let mut destination = vec![0; layout.size as usize];
    // Each declared capability receives its model provider's first handle.
    let grants = vec![0; signature.params.len()];
    let sources = product
        .native_sources(&hirsel_core_ir::source::anchored_items(&module))
        .map_err(|e| e.to_string())?;
    let sites = sources
        .item_sites(&module.items)
        .map_err(|e| e.to_string())?;
    let evaluated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eval::run_entry(
            &module,
            function,
            &grants,
            ROOT_LIMIT,
            eval::Nans::Canonical,
            Some(&mut destination),
            INSTRUCTIONS,
            &revisions,
            None,
            false,
            &sites,
            None,
        )
        .0
    }));
    let report = evaluated.map_err(|panic| {
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or("unknown panic payload");
        format!("evaluator panicked: {message}")
    })?;
    let mut errors = Vec::new();
    if report.stdout != run.stdout.as_bytes() {
        errors.push(format!(
            "stdout was {:?}, expected {:?}",
            String::from_utf8_lossy(&report.stdout),
            run.stdout
        ));
    }
    match (&run.outcome, &report.outcome) {
        (ExpectedOutcome::Success, eval::Outcome::Returned(_)) => {
            let mut word = [0; 8];
            let width = tag.width as usize;
            let start = tag.offset as usize;
            word[..width].copy_from_slice(&destination[start..start + width]);
            let case = case_of(&tag, u64::from_le_bytes(word));
            if case != 0 {
                errors.push(format!("main returned Result case {case}, expected Ok"));
            }
        }
        (ExpectedOutcome::Failure { cause, site }, eval::Outcome::Failed(causes)) => {
            let actual_site = report.sites.first().copied().flatten().and_then(|anchor| {
                sources
                    .positions()
                    .position(&module.items[anchor.item.index()], anchor.start)
                    .map(|(_, line, column)| (line, column))
            });
            if causes.first() != Some(cause) || actual_site != Some(*site) {
                errors.push(format!(
                    "failure was {causes:?} at {actual_site:?}, expected {cause} at {}:{}",
                    site.0, site.1
                ));
            }
        }
        _ => errors.push(format!("unexpected run outcome {:?}", report.outcome)),
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[test]
fn every_live_spec_fence_checks_its_claim() {
    let mut paths = fs::read_dir(repo().join("docs/spec"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "md"))
        .collect::<Vec<_>>();
    paths.sort();
    let (mut failures, mut unbuilt) = (Vec::new(), 0);
    let fences = paths
        .iter()
        .flat_map(|path| read_fences(path, &mut failures, &mut unbuilt))
        .collect::<Vec<_>>();
    let refused = fences
        .iter()
        .filter(|fence| !fence.rules.is_empty())
        .count();
    let runs = fences.iter().filter(|fence| fence.run.is_some()).count();
    let positive = fences.len() - refused - runs;
    println!("spec fences: {refused} refused, {positive} positive, {runs} run, {unbuilt} unbuilt");
    assert!(
        refused > 0 && positive > 0 && runs > 0,
        "spec corpus is incomplete"
    );
    let (entries, fragments): (Vec<_>, Vec<_>) = fences.iter().partition(|fence| fence.entry);
    check_fences(&fragments, &mut failures);
    for entry in entries {
        check_fences(&[entry], &mut failures);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
