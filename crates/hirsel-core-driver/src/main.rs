use hirsel_core_driver::{
    Backend, BuildProduct, CheckRequest, CoreImage, Driver, LibraryInput, Meter, NativeOptions,
};
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
    diagnostics: Vec<hirsel_core_driver::Diagnostic<'a>>,
    incomplete: Vec<hirsel_core_driver::IncompleteDiagnostic<'a>>,
    image: Option<String>,
    native: Option<BuildProduct>,
    stopped: Option<String>,
}
fn hex(hash: hirsel_core_ids::ContentHash) -> String {
    hash.0.iter().map(|b| format!("{b:02x}")).collect()
}
fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .ok_or("usage: hirsel-core check <package-dir> | image --libraries <core-root>")?;
    if !["check", "image", "build", "run"].contains(&command.as_str()) {
        return Err("expected check, image, build or run".into());
    }
    let mut dir = None;
    let mut libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let mut store = None;
    let mut manifest = None;
    let mut metrics = None;
    let mut image_path = None;
    let mut entry = None;
    let native_command = command == "build" || command == "run";
    let mut backend = None;
    let mut release = false;
    let mut runtime = None;
    let mut output_path = None;
    let mut root_limit = 64 * 1024 * 1024;
    let mut target = "x86_64-unknown-linux-gnu".to_owned();
    let mut cc = std::env::var_os("LLVM_SYS_231_PREFIX")
        .map(|p| PathBuf::from(p).join("bin/clang"))
        .unwrap_or_else(|| PathBuf::from("cc"));
    let mut run_args = Vec::new();
    let mut json = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--release" => release = true,
            "--backend" => {
                backend = Some(args.next().ok_or("missing backend")?.parse::<Backend>()?)
            }
            "--runtime" => {
                runtime = Some(PathBuf::from(args.next().ok_or("missing runtime archive")?))
            }
            "--cc" => cc = PathBuf::from(args.next().ok_or("missing C linker driver")?),
            "--target" => target = args.next().ok_or("missing target")?,
            "--limit" => root_limit = args.next().ok_or("missing root limit")?.parse()?,
            "-o" | "--output" => {
                output_path = Some(PathBuf::from(args.next().ok_or("missing output path")?))
            }
            "--" if command == "run" => {
                run_args.extend(args);
                break;
            }
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
    let selected_entry = request.entry.clone();
    let native_store = request.store.join("native");
    meter.finish("startup/discovery");
    let mut driver = Driver::default();
    let (product, image) = if command == "image" {
        driver.produce_image(request, &mut meter)?
    } else {
        (driver.check(request, &mut meter)?, None)
    };
    let mut stopped = None;
    let native = if native_command && product.accepted() {
        let options = NativeOptions {
            backend: backend.unwrap_or(if release {
                Backend::Llvm
            } else {
                Backend::Cranelift
            }),
            optimize: release,
            target,
            runtime: runtime.ok_or("build/run requires --runtime <core runtime archive>")?,
            include: Path::new(env!("CARGO_MANIFEST_DIR")).join("../hirsel-core-rt/include"),
            cc,
            store: native_store,
            output: output_path.unwrap_or_else(|| PathBuf::from("program")),
            root_limit,
            instance_limit: 65_536,
        };
        match product.build(
            selected_entry
                .as_ref()
                .ok_or("build/run requires an entry")?,
            &options,
        ) {
            Ok(native) => Some(native),
            Err(e) => {
                stopped = Some(e.to_string());
                None
            }
        }
    } else {
        None
    };
    let output = Output {
        accepted: product.accepted(),
        root: if json {
            hex(product.root)
        } else {
            String::new()
        },
        files: product.files.len(),
        rechecked: product.rechecked.len(),
        source_bytes: product.bytes,
        diagnostics: product.machine_diagnostics()?,
        incomplete: product.machine_incomplete()?,
        image: image.map(|i| hex(i.identity)),
        native,
        stopped,
    };
    if json {
        println!("{}", serde_json::to_string(&output)?);
    } else {
        for d in &output.diagnostics {
            eprintln!(
                "{} {} {}..{}: {}",
                d.rule, d.primary.module, d.primary.span.start, d.primary.span.end, d.message
            );
        }
        for g in &output.incomplete {
            eprintln!(
                "incomplete {} {}..{}: {}",
                g.primary.module, g.primary.span.start, g.primary.span.end, g.message
            );
        }
        if let Some(stop) = &output.stopped {
            eprintln!("{stop}");
        }
        if command != "run" {
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
    }
    let run_exit = if command == "run" {
        if let Some(native) = &output.native {
            let path = std::fs::canonicalize(&native.executable)?;
            Some(
                std::process::Command::new(path)
                    .args(run_args)
                    .status()?
                    .code()
                    .unwrap_or(102),
            )
        } else {
            None
        }
    } else {
        None
    };
    let exit = if let Some(code) = run_exit {
        code
    } else if !product.incomplete.is_empty() || output.stopped.is_some() {
        2
    } else {
        i32::from(!product.diagnostics.is_empty())
    };
    drop(output);
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
