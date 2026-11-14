use hirsel_core_driver::{
    Backend, BuildProduct, CheckRequest, CoreImage, Driver, LibraryInput, Meter, NativeOptions,
};
use hirsel_core_resolve::CorpusManifest;
use serde::Serialize;
use std::path::{Path, PathBuf};
hirsel_guard::install!(mimalloc::MiMalloc: mimalloc::MiMalloc);

#[derive(Serialize)]
struct Output<'a> {
    accepted: bool,
    root: String,
    files: usize,
    rechecked: usize,
    source_bytes: usize,
    diagnostics: Vec<hirsel_core_driver::Diagnostic<'a>>,
    incomplete: Vec<hirsel_core_driver::IncompleteDiagnostic<'a>>,
    resources: Vec<hirsel_core_driver::ResourceDiagnostic<'a>>,
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
    let invocation = match hirsel_core_driver::cli::parse(std::env::args().skip(1))? {
        hirsel_core_driver::cli::Action::Help(help) => {
            print!("{help}");
            std::process::exit(0);
        }
        hirsel_core_driver::cli::Action::Invoke(invocation) => invocation,
    };
    let command = invocation.command.name;
    if command == "new" {
        let directory = invocation
            .positional
            .as_deref()
            .ok_or("new requires a package directory")?;
        let library = invocation
            .options
            .iter()
            .any(|(id, _)| *id == hirsel_core_driver::cli::OptionId::Lib);
        hirsel_core_driver::cli::scaffold(Path::new(directory), library)?;
        println!(
            "Created {} at {directory}",
            if library {
                "library with example main"
            } else {
                "application"
            }
        );
        std::process::exit(0);
    }
    if command == "fmt" || command == "explain" {
        use hirsel_core_driver::cli::OptionId;
        let has = |option| invocation.options.iter().any(|(id, _)| *id == option);
        if command == "fmt" {
            let directory = invocation
                .positional
                .as_deref()
                .ok_or("fmt requires a package directory")?;
            let status = hirsel_core_driver::cli_tools::format(
                Path::new(directory),
                has(OptionId::FmtCheck),
                has(OptionId::Signatures),
            )?;
            std::process::exit(status);
        }
        hirsel_core_driver::cli_tools::explain(
            invocation.positional.as_deref(),
            has(OptionId::ListRules),
            has(OptionId::Json),
        )?;
        std::process::exit(0);
    }
    let dir = invocation.positional.map(PathBuf::from);
    let mut libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let mut store = None;
    let mut manifest = None;
    let mut metrics = None;
    let mut image_path = None;
    let mut install_root = None;
    let mut core_product = None;
    let mut source_core = false;
    let mut source_std = false;
    let mut product_name = None;
    let mut entry = None;
    let test_command = command == "test";
    let native_command = command == "build" || command == "run" || test_command;
    let mut test_filter = None;
    let mut test_timeout = None;
    let mut explicit_limit = false;
    let mut backend = None;
    let mut release = false;
    let mut runtime = None;
    let mut explicit_runtime = false;
    let mut explicit_target = false;
    let mut logical_charge_report = None;
    let mut output_path = None;
    let mut root_limit = 64 * 1024 * 1024;
    let mut emulator = None;
    let mut sysroot = None;
    let mut target = "x86_64-unknown-linux-gnu".to_owned();
    let mut cc = std::env::var_os("LLVM_SYS_231_PREFIX")
        .map(|p| PathBuf::from(p).join("bin/clang"))
        .unwrap_or_else(|| PathBuf::from("cc"));
    let run_args = invocation.arguments;
    let mut directories = Vec::new();
    let mut grant_mask = 0;
    let mut json = false;
    use hirsel_core_driver::cli::OptionId as Opt;
    for (option, value) in invocation.options {
        let value = value.unwrap_or_default();
        match option {
            Opt::Json => json = true,
            Opt::Install => install_root = Some(PathBuf::from(value)),
            Opt::SourceCore => source_core = true,
            Opt::SourceStd => source_std = true,
            Opt::Name => {
                product_name = Some(
                    hirsel_core_ids::DefPath::from_full(&value)
                        .ok_or("invalid --name definition path")?,
                )
            }
            Opt::CoreProduct => core_product = Some(PathBuf::from(value)),
            Opt::Dir => directories.push(value),
            Opt::Grant => {
                let kind = hirsel_core_driver::grant_kind(&value)
                    .filter(|kind| (1..=9).contains(kind)).ok_or("expected Net, Clock, Random, Stdout, Stderr, Stdin, Env, Process or Tasks; use --dir for Dir; Args is always present")?;
                grant_mask |= 1 << kind;
            }
            Opt::Filter => test_filter = Some(value),
            Opt::TestTimeout => {
                let seconds: u64 = value.parse()?;
                if seconds == 0 {
                    return Err("test timeout must be positive".into());
                }
                test_timeout = Some(std::time::Duration::from_secs(seconds));
            }
            Opt::Release => release = true,
            Opt::Backend => backend = Some(value.parse::<Backend>()?),
            Opt::Runtime => {
                explicit_runtime = true;
                runtime = Some(PathBuf::from(value));
            }
            Opt::LogicalChargeReport => {
                logical_charge_report = Some(PathBuf::from(value));
            }
            Opt::Cc => cc = PathBuf::from(value),
            Opt::Target => {
                explicit_target = true;
                target = value;
            }
            Opt::Emulator => emulator = Some(PathBuf::from(value)),
            Opt::Sysroot => sysroot = Some(PathBuf::from(value)),
            Opt::Limit => {
                explicit_limit = true;
                root_limit = value.parse()?;
            }
            Opt::Output => output_path = Some(PathBuf::from(value)),
            Opt::Entry => {
                entry =
                    Some(hirsel_core_ids::DefPath::from_full(&value).ok_or("invalid entry path")?)
            }
            Opt::Libraries => libraries = PathBuf::from(value),
            Opt::Store => store = Some(PathBuf::from(value)),
            Opt::Inputs => manifest = Some(PathBuf::from(value)),
            Opt::Metrics => metrics = Some(PathBuf::from(value)),
            Opt::Image => image_path = Some(PathBuf::from(value)),
            Opt::Help | Opt::Lib | Opt::FmtCheck | Opt::Signatures | Opt::ListRules => {
                unreachable!("CLI early command option")
            }
        }
    }
    let runner = match (emulator, sysroot) {
        (None, None) => hirsel_core_driver::Runner::Native,
        (Some(emulator), Some(sysroot)) => {
            hirsel_core_driver::Runner::qemu(&emulator, &sysroot, &target)?
        }
        _ => return Err("--emulator and --sysroot must be supplied together".into()),
    };
    if test_command && !explicit_limit {
        return Err("test requires an explicit --limit <bytes> for each run".into());
    }
    if test_command && entry.is_some() {
        return Err("test selects marked functions with --filter, not --entry".into());
    }
    let executable = std::env::current_exe()?;
    let installed = hirsel_core_driver::installation::Installed::discover(&executable)?;
    if let Some(installed) = &installed {
        if core_product.is_none() {
            core_product = Some(installed.cache());
        }
        if runtime.is_none() {
            runtime = installed.runtime(&target);
        }
    }
    if command == "env" {
        let cache = core_product.unwrap_or_else(hirsel_core_driver::PackageProduct::default_cache);
        let report = hirsel_core_driver::installation::environment(
            &executable,
            installed.as_ref(),
            &cache,
            &target,
            runtime.as_deref(),
        )?;
        if json {
            println!("{}", serde_json::to_string(&report)?);
        } else {
            println!("compiler: {}", report.executable.display());
            println!(
                "installed root: {}",
                report
                    .installed_root
                    .as_ref()
                    .map_or_else(|| "none".to_owned(), |p| p.display().to_string())
            );
            println!("package cache: {}", report.package_cache.display());
            println!("target: {}", report.target);
            println!(
                "runtime: {}",
                report.runtime.as_ref().map_or_else(
                    || "none; select --runtime".to_owned(),
                    |p| p.display().to_string()
                )
            );
        }
        std::process::exit(0);
    }
    if install_root.is_some() && (store.is_some() || image_path.is_some()) {
        return Err("--install cannot be combined with --store or --image".into());
    }
    if matches!(command, "core-product" | "std-product")
        && install_root.is_none()
        && (explicit_runtime || explicit_target)
    {
        return Err("product --runtime/--target require --install".into());
    }
    let mut meter = Meter::new(metrics.is_some());
    // Directory checks and source comparisons use checkout sources when Buck's
    // build-time package path is relative to its action directory.
    if manifest.is_none()
        && libraries == Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core")
        && Path::new("core/std").is_dir()
    {
        libraries = PathBuf::from("core");
    }
    let store = store
        .or_else(|| install_root.as_ref().map(|root| root.join("core-packages")))
        .unwrap_or_else(|| {
            dir.as_deref()
                .unwrap_or(Path::new("."))
                .join(".hirsel-core")
        });
    if command == "core-product" || command == "std-product" {
        meter.finish("startup/discovery");
        let (image, rebuilt) = if let Some(path) = image_path {
            (
                CoreImage::open(&path)?.ok_or("core image cache miss; rebuild the image")?,
                false,
            )
        } else {
            let package = if let Some(root) = &install_root {
                hirsel_core_driver::installation::publish(
                    root,
                    command == "std-product",
                    runtime.as_deref().map(|runtime| (runtime, target.as_str())),
                    &mut meter,
                )?
            } else if command == "std-product" {
                hirsel_core_driver::PackageProduct::bundled_std(&store, &mut meter)?
            } else {
                hirsel_core_driver::PackageProduct::bundled(&store, &mut meter)?
            };
            (package.image, package.rebuilt)
        };
        let name = if let Some(name) = product_name {
            let found = image.resolve_name(&name)?.is_some();
            meter.finish("package name resolution");
            let declaration = if found {
                image.demand_declaration(&name)?
            } else {
                false
            };
            meter.finish("package signature demand");
            Some(
                serde_json::json!({"path":name.to_string(),"found":found,"declaration":declaration}),
            )
        } else {
            None
        };
        println!(
            "{}",
            serde_json::json!({"image":hex(image.identity),"rebuilt":rebuilt,"name":name,"index_nodes":image.decoded_index_nodes(),"index_rows":image.decoded_index_rows()})
        );
        drop(image);
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
    } else if native_command || installed.is_some() {
        CheckRequest::program(&dir.ok_or("missing package directory")?, &libraries, &store)?
    } else {
        CheckRequest::directory(&dir.ok_or("missing package directory")?, &libraries, &store)?
    };
    if entry.is_some() {
        request.entry = entry;
    }
    if test_command {
        request.entry = None;
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
                let root =
                    core_product.unwrap_or_else(hirsel_core_driver::PackageProduct::default_cache);
                if source_std && native_command {
                    driver.check_reachable_with_core_product(request, &root, &mut meter)?
                } else if source_std {
                    driver.check_with_core_product(request, &root, &mut meter)?
                } else if native_command {
                    driver.check_reachable_with_product(request, &root, &mut meter)?
                } else {
                    driver.check_with_product(request, &root, &mut meter)?
                }
            } else {
                driver.check(request, &mut meter)?
            },
            None,
        )
    };
    let reach = if native_command && !test_command {
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
    let mut native_options = if native_command && (test_command || accepted) {
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
            headers.path().join("hirsel_core_rt_sites.h"),
            include_bytes!("../../hirsel-core-rt/include/hirsel_core_rt_sites.h"),
        )?;
        std::fs::write(
            headers.path().join("hirsel_core_rt_tasks.h"),
            include_bytes!("../../hirsel-core-rt/include/hirsel_core_rt_tasks.h"),
        )?;
        let default_output = if test_command {
            native_store.join("tests")
        } else {
            PathBuf::from("program")
        };
        let options = NativeOptions {
            backend: backend.unwrap_or(if release {
                Backend::Llvm
            } else {
                Backend::Cranelift
            }),
            optimize: release,
            target,
            runtime: runtime.ok_or("build/run/test requires --runtime <core runtime archive>")?,
            include: headers.path().to_owned(),
            cc,
            store: native_store,
            output: output_path.unwrap_or(default_output),
            root_limit,
            instance_limit: 65_536,
            directories,
            grant_mask,
            logical_charge_report,
        };
        if options.logical_charge_report.is_some() {
            eprintln!(
                "observation build: root peak reservation includes bound, task and grant reservations (MEM-19); timings are not performance samples"
            );
        }

        Some((headers, options))
    } else {
        None
    };
    if test_command {
        let options = &mut native_options.as_mut().unwrap().1;
        let output_dir = options.output.clone();
        let tests: Vec<_> = product
            .tests()
            .filter(|path| {
                test_filter
                    .as_ref()
                    .is_none_or(|filter: &String| path.to_string().contains(filter.as_str()))
            })
            .cloned()
            .collect();
        let mut rows = Vec::new();
        let mut failed = false;
        for test in &tests {
            options.output = output_dir.join(hex(hirsel_core_ids::ContentHash::of_bytes(
                test.to_string().as_bytes(),
            )));
            let row = match product.run_test_with_runner(test, options, test_timeout, &runner) {
                Ok(outcome) => {
                    let passed = outcome.passed();
                    failed |= !passed;
                    if !json {
                        if passed {
                            println!("pass {test}");
                        } else {
                            let reason = if outcome.timed_out {
                                "execution timed out".to_owned()
                            } else if outcome.output.stderr.is_empty() {
                                "error".to_owned()
                            } else {
                                String::from_utf8_lossy(&outcome.output.stderr)
                                    .lines()
                                    .collect::<Vec<_>>()
                                    .join(" | ")
                            };
                            println!("fail {test}: {reason}");
                        }
                    }
                    serde_json::json!({"definition":test.to_string(),"passed":passed,"status":outcome.output.status.code(),"stdout":String::from_utf8_lossy(&outcome.output.stdout),"stderr":String::from_utf8_lossy(&outcome.output.stderr),"timed_out":outcome.timed_out})
                }
                Err(error) => {
                    failed = true;
                    if !json {
                        println!("fail {test}: {error}");
                    }
                    serde_json::json!({"definition":test.to_string(),"passed":false,"stopped":error.to_string()})
                }
            };
            rows.push(row);
        }
        let passed = !tests.is_empty() && !failed;
        if json {
            println!("{}", serde_json::json!({"passed":passed,"tests":rows}));
        }
        if tests.is_empty() && !json {
            eprintln!("no tests found");
        }
        meter.finish("test execution/output");
        if let Some(path) = metrics {
            std::fs::write(path, serde_json::to_vec_pretty(&meter.result)?)?;
        }
        std::process::exit(if tests.is_empty() {
            2
        } else {
            i32::from(failed)
        });
    }
    let mut stopped = None;
    let native = if native_command && accepted {
        let options = &native_options.as_ref().unwrap().1;
        match product.build_reachable(
            selected_entry
                .as_ref()
                .ok_or("build/run requires an entry")?,
            reach.as_ref().unwrap(),
            options,
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
        resources: if let Some(r) = &reach {
            product.resources_for(&r.resources)?
        } else {
            product.machine_resources()?
        },
        image: image.map(|i| hex(i.identity)),
        native,
        stopped: stopped.or_else(|| {
            reach
                .as_ref()
                .and_then(|r| r.instances.as_ref().err())
                .filter(|e| !matches!(e, hirsel_core_driver::InstanceStop::Resource { .. }))
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
        for r in &output.resources {
            eprintln!(
                "resource {} {}..{}: {}",
                r.primary.module, r.primary.span.start, r.primary.span.end, r.message
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
                runner
                    .command(&path)
                    .args(run_args)
                    .status()
                    .map_err(|e| format!("run execution {runner:?}: {e}"))?
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
    } else if !output.incomplete.is_empty() || !output.resources.is_empty() {
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
