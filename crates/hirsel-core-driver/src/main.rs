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
    reachable: Option<ReachableOutput<'a>>,
}
#[derive(Serialize)]
struct ReachableOutput<'a> {
    bodies: usize,
    declarations: usize,
    census: std::collections::BTreeMap<&'a str, usize>,
    first_stops: &'a [hirsel_core_driver::GapStop],
}
fn hex(hash: hirsel_core_ids::ContentHash) -> String {
    hash.0.iter().map(|b| format!("{b:02x}")).collect()
}
fn run() -> Result<std::convert::Infallible, Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .ok_or("usage: hirsel-core check <package-dir> | image --libraries <core-root>")?;
    if !["check", "image", "build", "run", "core-product"].contains(&command.as_str()) {
        return Err("expected check, image, build, run or core-product".into());
    }
    let mut dir = None;
    let mut libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let mut store = None;
    let mut manifest = None;
    let mut metrics = None;
    let mut image_path = None;
    let mut core_product = None;
    let mut source_core = false;
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
    let mut directories = Vec::new();
    let mut grant_mask = 0;
    let mut json = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--source-core" => source_core = true,
            "--core-product" => {
                core_product = Some(PathBuf::from(
                    args.next().ok_or("missing core product cache")?,
                ))
            }
            "--dir" if native_command => {
                directories.push(args.next().ok_or("missing directory grant path")?)
            }
            "--grant" if native_command => {
                let kind = hirsel_core_driver::grant_kind(&args.next().ok_or("missing grant kind")?)
                    .filter(|kind| (1..=9).contains(kind)).ok_or("expected Net, Clock, Random, Stdout, Stderr, Stdin, Env, Process or Tasks; use --dir for Dir; Args is always present")?;
                grant_mask |= 1 << kind;
            }
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
    if command == "core-product" {
        meter.finish("startup/discovery");
        let package = hirsel_core_driver::PackageProduct::bundled(&store, &mut meter)?;
        println!(
            "{}",
            serde_json::json!({"image": hex(package.image.identity), "rebuilt": package.rebuilt})
        );
        drop(package);
        meter.finish("package handle teardown");
        if let Some(path) = metrics {
            std::fs::write(path, serde_json::to_vec_pretty(&meter.result)?)?;
        }
        std::process::exit(0);
    }
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
        (
            if !source_core && (native_command || core_product.is_some()) {
                driver.check_with_product(
                    request,
                    &core_product.unwrap_or_else(hirsel_core_driver::PackageProduct::default_cache),
                    &mut meter,
                )?
            } else {
                driver.check(request, &mut meter)?
            },
            None,
        )
    };
    let reach = if native_command {
        Some(
            product.reachable(
                selected_entry
                    .as_ref()
                    .ok_or("build/run requires an entry")?,
                65_536,
            )?,
        )
    } else {
        None
    };
    let accepted = reach
        .as_ref()
        .map_or_else(|| product.accepted(), |r| r.accepted());
    let mut stopped = None;
    let native = if native_command && accepted {
        let headers = tempfile::tempdir()?;
        std::fs::write(
            headers.path().join("hirsel_core_rt.h"),
            include_bytes!("../../hirsel-core-rt/include/hirsel_core_rt.h"),
        )?;
        std::fs::write(
            headers.path().join("hirsel_core_rt_host.h"),
            include_bytes!("../../hirsel-core-rt/include/hirsel_core_rt_host.h"),
        )?;
        std::fs::write(
            headers.path().join("hirsel_core_rt_tasks.h"),
            include_bytes!("../../hirsel-core-rt/include/hirsel_core_rt_tasks.h"),
        )?;
        let options = NativeOptions {
            backend: backend.unwrap_or(if release {
                Backend::Llvm
            } else {
                Backend::Cranelift
            }),
            optimize: release,
            target,
            runtime: runtime.ok_or("build/run requires --runtime <core runtime archive>")?,
            include: headers.path().to_owned(),
            cc,
            store: native_store,
            output: output_path.unwrap_or_else(|| PathBuf::from("program")),
            root_limit,
            instance_limit: 65_536,
            directories,
            grant_mask,
        };
        match product.build_reachable(
            selected_entry
                .as_ref()
                .ok_or("build/run requires an entry")?,
            reach.as_ref().unwrap(),
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
        accepted,
        root: if json {
            hex(product.root)
        } else {
            String::new()
        },
        files: product.files.len(),
        rechecked: product.rechecked.len(),
        source_bytes: product.bytes,
        diagnostics: if let Some(r) = &reach {
            product.diagnostics_for(r.diagnostics.iter().copied())?
        } else {
            product.machine_diagnostics()?
        },
        incomplete: if let Some(r) = &reach {
            product.incomplete_for(r.incomplete.iter().copied())?
        } else {
            product.machine_incomplete()?
        },
        image: image.map(|i| hex(i.identity)),
        native,
        stopped: stopped.or_else(|| {
            reach
                .as_ref()
                .and_then(|r| r.instances.as_ref().err())
                .map(|e| format!("instances: {e:?}"))
        }),
        reachable: reach.as_ref().map(|r| ReachableOutput {
            bodies: r.bodies.len(),
            declarations: r.declarations.len(),
            census: r.census(),
            first_stops: &r.first_stops,
        }),
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
    } else if !output.incomplete.is_empty() {
        2
    } else if !output.diagnostics.is_empty() {
        1
    } else {
        i32::from(output.stopped.is_some()) * 2
    };
    meter.finish("output/render");
    if let Some(path) = metrics {
        std::fs::write(path, serde_json::to_vec_pretty(&meter.result)?)?;
    }
    // Publication and output are complete. The OS reclaims these in-memory
    // graphs at exit; the reusable library Driver retains ordinary destruction.
    std::process::exit(exit)
}
fn main() {
    match run() {
        Ok(never) => match never {},
        Err(e) => {
            eprintln!("hirsel-core: {e}");
            std::process::exit(2);
        }
    }
}
