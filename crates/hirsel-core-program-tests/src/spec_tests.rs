//! Check the source and output in the spec without maintaining a second fixture corpus.
use crate::{Checked, library_manifest, package, repo, std_manifest};
use hirsel_core_driver::{SourceSite, line_column};
use hirsel_core_eval as eval;
use hirsel_core_ids::DefPath;
use hirsel_core_ir::Identity;
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
    /// A `hirsel unbuilt` fence claims what the built checker does not do yet.
    unbuilt: bool,
    uses_std: bool,
    uses_store: bool,
    entry: bool,
    rules: BTreeSet<String>,
    refused_any: bool,
    run: Option<Run>,
}

struct Run {
    stdout: String,
    outcome: ExpectedOutcome,
}

enum ExpectedOutcome {
    Success,
    Failure {
        cause: String,
        site: (u32, u32),
    },
    /// PROG-3: `main` returned `Error(e)`; `stderr` is the run's line for `e`.
    Error {
        stderr: String,
    },
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
        Some(outcome) if outcome.starts_with("error, stderr `") => {
            let stderr = outcome
                .strip_prefix("error, stderr `")
                .and_then(|rest| rest.strip_suffix("`."))
                .unwrap_or_else(|| panic!("{location}: expected an error stderr line"));
            ExpectedOutcome::Error {
                stderr: stderr.to_owned(),
            }
        }
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

/// Whether `source` names a member of the library `name` (`name.` not preceded by a
/// word character).
fn names_library(source: &str, name: &str) -> bool {
    let needle = format!("{name}.");
    source.match_indices(&needle).any(|(at, _)| {
        !source[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

fn read_fences(
    path: &Path,
    std_by_default: bool,
    plain_refusals: bool,
    failures: &mut Vec<String>,
) -> Vec<Fence> {
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
            "hirsel" | "hirsel run" | "hirsel unbuilt" => ParseMode::Source,
            "hirsel core" => ParseMode::Signatures,
            "" | "ebnf" | "json" | "sh" | "text" | "toml" => continue,
            _ => panic!("{location}: unknown fence info string {info:?}"),
        };
        let signatures = mode == ParseMode::Signatures;
        let unbuilt = info == "hirsel unbuilt";
        let run = (info == "hirsel run").then(|| expected_run(&mut lines, &location));
        let mut symbols = Symbols::default();
        let parsed = parse(&source, mode, &mut symbols);
        // An unbuilt fence may use syntax the parser does not take yet.
        if !unbuilt {
            if parsed.syntax.text() != source {
                failures.push(format!("{location}: parser lost source bytes"));
            }
            if let Some(canonical) = format(&parsed) {
                let reparsed = parse(&canonical, mode, &mut Symbols::default());
                if format(&reparsed).as_deref() != Some(canonical.as_str()) {
                    failures.push(format!("{location}: formatting failed to round trip"));
                }
            }
        }
        let mut refused_any = false;
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
            .filter_map(|comment| {
                let rule = comment
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
                if rule.split_once('-').is_some_and(|(family, number)| {
                    !family.is_empty()
                        && family.bytes().all(|c| c.is_ascii_uppercase())
                        && !number.is_empty()
                        && number.bytes().all(|c| c.is_ascii_digit())
                }) {
                    Some(rule.to_owned())
                } else if plain_refusals {
                    refused_any = true;
                    None
                } else {
                    panic!("{location}: expected a rule ID after // refused:");
                }
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
                entry && rules.is_empty() && !refused_any,
                "{location}: run fence needs a public main and no refusal annotations"
            );
        }
        fences.push(Fence {
            path: path.to_owned(),
            line: at + 2,
            uses_std: std_by_default || names_library(&source, "std"),
            uses_store: names_library(&source, "store"),
            source,
            signatures,
            unbuilt,
            entry,
            rules,
            refused_any,
            run,
        });
    }
    fences
}

fn diagnostic_location(fence: Option<&Fence>, site: &SourceSite) -> String {
    if let Some(fence) = fence {
        let (line, col) = line_column(fence.source.as_bytes(), site.span.start as usize);
        format!("{}:{}:{col}", fence.path.display(), fence.line + line - 1)
    } else {
        format!("{} at byte {}", site.module, site.span.start)
    }
}

/// Checks `fences` as one corpus. Each failure names the fence it belongs to, if any.
fn check_fences<'a>(fences: &[&'a Fence]) -> Vec<(Option<&'a Fence>, String)> {
    let mut failures = Vec::new();
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
            if fence.uses_std || fence.uses_store {
                dependencies.push(package("std"));
            }
            if fence.uses_store {
                dependencies.push(package("store"));
            }
            packages.push(PackageManifest {
                namespace: None,
                package: name.clone(),
                sources: vec![source],
                dependencies,
                toolchain: false,
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
    if packages
        .iter()
        .any(|p| p.dependencies.contains(&package("store")))
    {
        packages.push(library_manifest("store"));
    }
    packages.push(library);
    let checked = Checked::check(CorpusManifest { core, packages }, files, entry.clone());
    let product = &checked.product;
    let mut seen = BTreeSet::new();
    let mut seen_any = BTreeSet::new();
    for diagnostic in product.machine_diagnostics().unwrap() {
        let fence = origins.get(&diagnostic.primary.module).copied();
        if fence.is_some_and(|f| f.rules.contains(diagnostic.rule)) {
            seen.insert((diagnostic.primary.module, diagnostic.rule));
        } else if fence.is_some_and(|f| f.refused_any) {
            seen_any.insert(diagnostic.primary.module);
        } else {
            failures.push((
                fence,
                format!(
                    "{}: unexpected {} ({})",
                    diagnostic_location(fence, &diagnostic.primary),
                    diagnostic.rule,
                    diagnostic.message
                ),
            ));
        }
    }
    for (module, fence) in &origins {
        for rule in &fence.rules {
            if !seen.contains(&(module.clone(), rule.as_str())) {
                failures.push((Some(fence), format!("{}: missing {rule}", fence.location())));
            }
        }
        if fence.refused_any && !seen_any.contains(module) {
            failures.push((
                Some(fence),
                format!("{}: expected refusal", fence.location()),
            ));
        }
    }
    for gap in product.machine_incomplete().unwrap() {
        let fence = origins.get(&gap.primary.module).copied();
        if fence.is_none_or(|f| f.rules.is_empty() && !f.refused_any) {
            failures.push((
                fence,
                format!(
                    "{}: incomplete check ({})",
                    diagnostic_location(fence, &gap.primary),
                    gap.message
                ),
            ));
        }
    }
    for resource in product.machine_resources().unwrap() {
        let fence = origins.get(&resource.primary.module).copied();
        failures.push((
            fence,
            format!(
                "{}: resource refusal ({})",
                diagnostic_location(fence, &resource.primary),
                resource.message
            ),
        ));
    }
    if let Some(run) = fences.first().and_then(|fence| fence.run.as_ref()) {
        let location = fences[0].location();
        if !product.accepted() {
            failures.push((
                Some(fences[0]),
                format!("{location}: run skipped after check failure"),
            ));
        } else if let Err(error) = evaluate(product, entry.unwrap(), run) {
            failures.push((Some(fences[0]), format!("{location}: {error}")));
        }
    }
    failures
}

fn evaluate(
    product: &hirsel_core_driver::CheckProduct,
    entry: DefPath,
    run: &Run,
) -> Result<(), String> {
    const ROOT_LIMIT: u64 = 1_048_576;
    const INSTRUCTIONS: u64 = 1_000_000_000;
    let root = Identity::plain(entry.clone());
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
    let show = product.entry_error_text(&entry, &module, result.index())?;
    let mut text = None;
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
            false,
            &sites,
            None,
            show.as_ref().map(|show| (show, &mut text)),
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
        (ExpectedOutcome::Error { stderr }, eval::Outcome::Returned(_)) => {
            let actual = text.as_ref().map(|text| format!("error: {text}"));
            if actual.as_deref() != Some(stderr.as_str()) {
                errors.push(format!("error line was {actual:?}, expected {stderr:?}"));
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

/// Checks fragments together and each entry alone, as one request selects one entry.
fn check_all<'a>(fences: &[&'a Fence]) -> Vec<(Option<&'a Fence>, String)> {
    let (entries, fragments): (Vec<_>, Vec<_>) = fences.iter().partition(|fence| fence.entry);
    let mut failures = check_fences(&fragments);
    for entry in entries {
        failures.extend(check_fences(&[entry]));
    }
    failures
}

#[test]
fn every_live_spec_fence_checks_its_claim() {
    let spec = repo().join("docs/spec");
    let mut paths = fs::read_dir(&spec)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "md"))
        .collect::<Vec<_>>();
    paths.sort();
    let primer = repo().join("docs/PRIMER.md");
    let cookbook = repo().join("docs/COOKBOOK.md");
    let unicode = spec.join("unicode.md");
    paths.extend([primer.clone(), cookbook.clone(), repo().join("README.md")]);
    let mut failures = Vec::new();
    let fences = paths
        .iter()
        // The primer and the cookbook start from `hirsel new` (which declares std),
        // and Unicode documents std extensions under the core names `char` and `string`.
        .flat_map(|path| {
            read_fences(
                path,
                path == &primer || path == &cookbook || path == &unicode,
                path == &primer,
                &mut failures,
            )
        })
        .collect::<Vec<_>>();
    let (unbuilt, live): (Vec<_>, Vec<_>) = fences.iter().partition(|fence| fence.unbuilt);
    let refused = live
        .iter()
        .filter(|fence| !fence.rules.is_empty() || fence.refused_any)
        .count();
    let runs = live.iter().filter(|fence| fence.run.is_some()).count();
    let positive = live.len() - refused - runs;
    println!(
        "spec fences: {refused} refused, {positive} positive, {runs} run, {} unbuilt",
        unbuilt.len()
    );
    assert!(
        refused > 0 && positive > 0 && runs > 0,
        "spec corpus is incomplete"
    );
    failures.extend(check_all(&live).into_iter().map(|(_, failure)| failure));
    // A `hirsel unbuilt` fence that already checks as its comments say (refused for
    // each rule it cites, or accepted) shows a built feature: its mark is stale.
    let unbuilt_failures = check_all(&unbuilt);
    if unbuilt_failures.iter().all(|(fence, _)| fence.is_some()) {
        for fence in &unbuilt {
            if !unbuilt_failures
                .iter()
                .any(|(failed, _)| failed.is_some_and(|f| std::ptr::eq(f, *fence)))
            {
                let claim = match (fence.rules.is_empty(), fence.refused_any) {
                    (true, false) => "accepted".to_owned(),
                    (true, true) => "refused".to_owned(),
                    (false, _) => format!(
                        "refused for {}",
                        Vec::from_iter(fence.rules.iter().cloned()).join(", ")
                    ),
                };
                failures.push(format!(
                    "{}: the `hirsel unbuilt` fence is {claim} as it says; remove the mark",
                    fence.location()
                ));
            }
        }
    } else {
        failures.extend(
            unbuilt_failures
                .into_iter()
                .filter(|(fence, _)| fence.is_none())
                .map(|(_, failure)| format!("unbuilt fences: {failure}")),
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
