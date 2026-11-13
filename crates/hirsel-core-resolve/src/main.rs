//! Acceptance and repeatable stage measurements over an explicit source manifest.
hirsel_guard::install!();
use hirsel_core_resolve::*;
use std::hint::black_box;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let path=args.first().ok_or("usage: hirsel-core-resolve MANIFEST.json [--json FILE] [--phase parse|resolve] [--iterations N] [--strip-comments] [--clean]")?;
    let option = |key: &str| args.windows(2).find(|a| a[0] == key).map(|a| a[1].as_str());
    let manifest: CorpusManifest = serde_json::from_slice(&std::fs::read(path)?)?;
    let mut corpus = manifest.load()?;
    if args.iter().any(|a| a == "--strip-comments") {
        corpus = corpus.without_comments();
    }
    if args.iter().any(|a| a == "--clean") {
        corpus = corpus.cleaned();
    }
    let parsed = corpus.parse();
    let index = corpus.index(&parsed);
    let iterations: usize = option("--iterations").unwrap_or("1").parse()?;
    match option("--phase") {
        Some("parse") => {
            for _ in 0..iterations {
                let p = corpus.parse();
                black_box(corpus.index(&p));
                black_box(p);
            }
        }
        Some("resolve") => {
            for _ in 0..iterations {
                for (source, p) in corpus.sources.iter().zip(&parsed.files) {
                    for (i, item) in p.syntax.items().enumerate() {
                        black_box(resolve(
                            &index,
                            index.module(&source.module).unwrap(),
                            i as u32,
                            item,
                        ));
                    }
                }
            }
        }
        None => {}
        Some(_) => return Err("phase must be parse or resolve".into()),
    }
    let report = corpus.analyze(&parsed, &index);
    if let Some(path) = option("--json") {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    for file in &report.files {
        for d in &file.diagnostics {
            eprintln!(
                "{}:{}:{} {} {}",
                d.file.display(),
                d.line,
                d.column,
                d.refusal.rule,
                d.refusal.message
            );
        }
    }
    println!(
        "files={} parsed={} resolved={} items={} names={} reads={} diagnostics={} bytes={} code_lines={} iterations={}",
        report.files.len(),
        report.files.iter().filter(|f| f.parsed).count(),
        report.files.iter().filter(|f| f.resolved).count(),
        report.files.iter().map(|f| f.items).sum::<usize>(),
        report.files.iter().map(|f| f.names).sum::<usize>(),
        report.files.iter().map(|f| f.reads).sum::<usize>(),
        report
            .files
            .iter()
            .map(|f| f.diagnostics.len())
            .sum::<usize>(),
        report.bytes,
        report.code_lines,
        iterations
    );
    Ok(())
}
