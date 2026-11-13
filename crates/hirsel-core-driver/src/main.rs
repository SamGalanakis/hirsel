use hirsel_core_driver::{CheckRequest, Driver, Meter};
use hirsel_core_resolve::CorpusManifest;
use serde::Serialize;
use std::path::{Path, PathBuf};

hirsel_guard::install!();

#[derive(Serialize)]
struct Output<'a> {
    accepted: bool,
    root: String,
    files: usize,
    rechecked: usize,
    source_bytes: usize,
    diagnostics: &'a [hirsel_core_resolve::Refusal],
    incomplete: &'a [hirsel_core_driver::Incomplete],
}
fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("check") {
        return Err("usage: hirsel-core check <package-dir> [--libraries <core-root>] [--store <dir>] [--inputs <manifest.json>] [--json] [--metrics <json>]".into());
    }
    let mut dir = None;
    let mut libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let mut store = None;
    let mut manifest = None;
    let mut metrics = None;
    let mut json = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--libraries" => libraries = PathBuf::from(args.next().ok_or("missing library root")?),
            "--store" => store = Some(PathBuf::from(args.next().ok_or("missing store directory")?)),
            "--inputs" => {
                manifest = Some(PathBuf::from(args.next().ok_or("missing input manifest")?))
            }
            "--metrics" => {
                metrics = Some(PathBuf::from(args.next().ok_or("missing metrics path")?))
            }
            _ if !arg.starts_with('-') && dir.is_none() => dir = Some(PathBuf::from(arg)),
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    let mut meter = Meter::new(metrics.is_some());
    let store = store.unwrap_or_else(|| {
        dir.as_deref()
            .unwrap_or(Path::new("."))
            .join(".hirsel-core")
    });
    let request = if let Some(manifest) = manifest {
        CheckRequest {
            inputs: serde_json::from_slice::<CorpusManifest>(&std::fs::read(manifest)?)?,
            store,
        }
    } else {
        CheckRequest::directory(&dir.ok_or("missing package directory")?, &libraries, &store)?
    };
    meter.finish("startup/discovery");
    let mut driver = Driver::default();
    let product = driver.check(request, &mut meter)?;
    let output = Output {
        accepted: product.accepted(),
        root: product.root.0.iter().map(|b| format!("{b:02x}")).collect(),
        files: product.files.len(),
        rechecked: product.rechecked.len(),
        source_bytes: product.bytes,
        diagnostics: &product.diagnostics,
        incomplete: &product.incomplete,
    };
    if json {
        println!("{}", serde_json::to_string(&output)?);
    } else {
        for d in &product.diagnostics {
            eprintln!(
                "{} {:?} {}..{}: {}",
                d.rule, d.primary.module, d.primary.span.start, d.primary.span.end, d.message
            );
        }
        for g in &product.incomplete {
            eprintln!("incomplete {:?}: {}", g.primary, g.message);
        }
        println!(
            "{}: {} files, {} checked definitions",
            if output.accepted {
                "accepted"
            } else {
                "not accepted"
            },
            output.files,
            output.rechecked
        );
    }
    let exit = if !product.incomplete.is_empty() {
        2
    } else {
        i32::from(!product.diagnostics.is_empty())
    };
    meter.finish("output/render");
    drop(product);
    meter.finish("output/drop returned Arc");
    drop(driver);
    meter.finish("output/drop resident products");
    if let Some(path) = metrics {
        std::fs::write(path, serde_json::to_vec_pretty(&meter.result)?)?;
    }
    Ok(exit)
}
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("hirsel-core: {e}");
            std::process::exit(2);
        }
    }
}
