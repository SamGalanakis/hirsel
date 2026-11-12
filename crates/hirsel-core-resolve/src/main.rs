//! Directory parse/resolve entry and amortized stage measurement workload.
hirsel_guard::install!();
use hirsel_core_ids::PackageId;
use hirsel_core_resolve::*;
use hirsel_core_syntax::{ParseMode, TokenKind, lex, parse};
use std::{hint::black_box, path::Path};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let root=args.first().ok_or("usage: hirsel-core-resolve DIRECTORY [--signatures] [--library DIRECTORY] [--prelude JSON] [--phase parse|resolve] [--iterations N]")?;
    let signatures = args.iter().any(|a| a == "--signatures");
    let option = |key: &str| args.windows(2).find(|a| a[0] == key).map(|a| a[1].as_str());
    let phase = option("--phase").unwrap_or("both");
    let iterations: usize = option("--iterations").unwrap_or("1").parse()?;
    let manifest = PackageManifest::directory(
        Path::new(root),
        PackageId {
            name: if signatures { "core" } else { "example" }.into(),
            version: "1".into(),
        },
        signatures,
    )?;
    let sources = manifest
        .sources
        .iter()
        .map(|s| std::fs::read_to_string(&s.file))
        .collect::<Result<Vec<_>, _>>()?;
    let lines: usize = sources.iter().map(|s| code_lines(s)).sum();
    let bytes: usize = sources.iter().map(String::len).sum();
    let mut modules = manifest.read()?;
    let mut environment = Environment::default();
    if let Some(file) = option("--prelude") {
        let mappings: std::collections::BTreeMap<String, String> =
            serde_json::from_slice(&std::fs::read(file)?)?;
        for (name, path) in mappings {
            environment.prelude.insert(
                name,
                hirsel_core_ids::DefPath::from_full(&path)
                    .ok_or("prelude paths must be package@version/module/name")?,
            );
        }
    }
    let library = if let Some(root) = option("--library") {
        PackageManifest::directory(
            Path::new(root),
            PackageId {
                name: "core".into(),
                version: "1".into(),
            },
            true,
        )?
        .read()?
    } else {
        vec![]
    };
    for m in &library {
        if let Some(head) = m.module.modules.first() {
            environment
                .dependencies
                .entry(manifest.package.clone())
                .or_default()
                .insert(
                    head.clone(),
                    ModuleId {
                        package: m.module.package.clone(),
                        modules: vec![head.clone()],
                    },
                );
        }
    }
    let mut index = NameIndex::new(environment);
    for module in library.iter().chain(&modules) {
        for (i, item) in module.parsed.syntax.items.iter().enumerate() {
            index.index_item(&module.module, i as u32, item);
        }
    }
    if phase == "parse" {
        for _ in 0..iterations {
            let mut index = NameIndex::default();
            for (i, s) in sources.iter().enumerate() {
                let p = parse(
                    black_box(s),
                    if signatures {
                        ParseMode::Signatures
                    } else {
                        ParseMode::Source
                    },
                );
                for (j, item) in p.syntax.items.iter().enumerate() {
                    index.index_item(&modules[i].module, j as u32, item);
                }
                black_box(p);
            }
            black_box(index);
        }
    } else if phase == "resolve" {
        for _ in 0..iterations {
            for m in &modules {
                for (i, item) in m.parsed.syntax.items.iter().enumerate() {
                    black_box(resolve(black_box(&index), &m.module, i as u32, item));
                }
            }
        }
    } else if phase != "both" {
        return Err("unknown phase".into());
    }
    let mut parsed = 0;
    let mut resolved = 0;
    let mut items = 0;
    let mut names = 0;
    let mut reads = 0;
    let mut diagnostics = 0;
    for m in &mut modules {
        let syntax_ok = m.parsed.diagnostics.is_empty() && m.parsed.resource_refusal.is_none();
        parsed += usize::from(syntax_ok);
        let mut errors = m.parsed.diagnostics.len();
        for d in &m.parsed.diagnostics {
            eprintln!(
                "{}",
                serde_json::to_string(&Refusal::syntax(m.module.clone(), d))?
            );
        }
        if let Some(resource) = &m.parsed.resource_refusal {
            eprintln!("resource refusal: {resource:?}");
            errors += 1;
        }
        for (i, item) in m.parsed.syntax.items.iter().enumerate() {
            let r = resolve(&index, &m.module, i as u32, item);
            items += 1;
            names += r.names.len();
            reads += r.reads.len();
            errors += r.diagnostics.len();
            for d in r.diagnostics {
                eprintln!("{}", serde_json::to_string(&d)?);
            }
        }
        diagnostics += errors;
        resolved += usize::from(errors == 0);
    }
    println!(
        "files={} parsed={} resolved={} items={} names={} reads={} diagnostics={} bytes={} code_lines={} phase={} iterations={}",
        modules.len(),
        parsed,
        resolved,
        items,
        names,
        reads,
        diagnostics,
        bytes,
        lines,
        phase,
        iterations
    );
    Ok(())
}
fn code_lines(text: &str) -> usize {
    let mut line = 0;
    let mut cursor = 0;
    let mut last = None;
    let mut count = 0;
    for t in lex(text).tokens {
        let start = t.span.start as usize;
        let end = t.span.end as usize;
        line += text[cursor..start].bytes().filter(|b| *b == b'\n').count();
        let newlines = text[start..end].bytes().filter(|b| *b == b'\n').count();
        if !matches!(t.kind, TokenKind::Trivia(_) | TokenKind::Eof) {
            for at in line..=line + newlines {
                if last != Some(at) {
                    count += 1;
                    last = Some(at);
                }
            }
        }
        line += newlines;
        cursor = end;
    }
    count
}
