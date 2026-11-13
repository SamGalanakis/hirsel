use hirsel_core_driver::{CheckRequest, CoreImage, Driver, LibraryInput, Meter};
use hirsel_core_resolve::CorpusManifest;
use serde::Serialize;
use std::path::{Path, PathBuf};
hirsel_guard::install!();

#[derive(Serialize)]
struct Output {
    accepted: bool,
    root: String,
    files: usize,
    rechecked: usize,
    source_bytes: usize,
    diagnostics: Vec<hirsel_core_resolve::Refusal>,
    incomplete: Vec<hirsel_core_driver::Incomplete>,
    image: Option<String>,
}
fn hex(hash: hirsel_core_ids::ContentHash) -> String {
    hash.0.iter().map(|b| format!("{b:02x}")).collect()
}
fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .ok_or("usage: hirsel-core check <package-dir> | image --libraries <core-root>")?;
    if command != "check" && command != "image" {
        return Err("expected check or image".into());
    }
    let mut dir = None;
    let mut libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let mut store = None;
    let mut manifest = None;
    let mut metrics = None;
    let mut image_path = None;
    let mut entry = None;
    let mut json = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--entry" => {
                entry = Some(
                    hirsel_core_ids::DefPath::from_full(
                        &args.next().ok_or("missing entry definition path")?,
                    )
                    .ok_or("invalid entry path")?,
                )
            }
            "--libraries" => libraries = PathBuf::from(args.next().ok_or("missing library root")?),
            "--store" => store = Some(PathBuf::from(args.next().ok_or("missing store directory")?)),
            "--inputs" => {
                manifest = Some(PathBuf::from(args.next().ok_or("missing input manifest")?))
            }
            "--metrics" => {
                metrics = Some(PathBuf::from(args.next().ok_or("missing metrics path")?))
            }
            "--image" => {
                image_path = Some(PathBuf::from(args.next().ok_or("missing image directory")?))
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
    let image = if let Some(path) = image_path {
        Some(CoreImage::open(&path)?.ok_or("core image cache miss; rebuild the image")?)
    } else {
        None
    };
    if command == "image" && image.is_some() {
        return Err("image production takes sources, not an image".into());
    }
    let mut request = if let Some(manifest) = manifest {
        CheckRequest {
            inputs: serde_json::from_slice::<CorpusManifest>(&std::fs::read(manifest)?)?,
            store,
            library: image.map_or(LibraryInput::Sources, LibraryInput::Image),
            entry: None,
        }
    } else if command == "image" {
        CheckRequest::libraries(dir.as_deref().unwrap_or(&libraries), &store)?
    } else if let Some(image) = image {
        CheckRequest::with_image(&dir.ok_or("missing package directory")?, &store, image)?
    } else {
        CheckRequest::directory(&dir.ok_or("missing package directory")?, &libraries, &store)?
    };
    if entry.is_some() {
        request.entry = entry;
    }
    meter.finish("startup/discovery");
    let mut driver = Driver::default();
    let (product, image) = if command == "image" {
        driver.produce_image(request, &mut meter)?
    } else {
        (driver.check(request, &mut meter)?, None)
    };
    let output = Output {
        accepted: product.accepted(),
        root: hex(product.root),
        files: product.files.len(),
        rechecked: product.rechecked.len(),
        source_bytes: product.bytes,
        diagnostics: product.diagnostics.clone(),
        incomplete: product.incomplete.clone(),
        image: image.map(|i| hex(i.identity)),
    };
    if json {
        println!("{}", serde_json::to_string(&output)?);
    } else {
        for d in &output.diagnostics {
            eprintln!(
                "{} {:?} {}..{}: {}",
                d.rule, d.primary.module, d.primary.span.start, d.primary.span.end, d.message
            );
        }
        for g in &output.incomplete {
            eprintln!(
                "incomplete {:?} {}..{}: {}",
                g.primary.module, g.primary.span.start, g.primary.span.end, g.message
            );
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
