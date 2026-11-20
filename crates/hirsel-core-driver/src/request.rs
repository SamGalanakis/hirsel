//! One compiler request, from its arguments to its output and exit status:
//! the `hirsel` executable's whole behaviour after process setup. It lives in
//! the library so that a test process can run a request exactly as the
//! executable does (request_sequences.rs compares their outputs).
use crate::{
    Backend, BuildProduct, CheckRequest, CoreImage, Driver, LibraryInput, Link, Meter,
    NativeOptions,
};
use hirsel_core_resolve::CorpusManifest;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
struct Output<'a> {
    accepted: bool,
    root: String,
    files: usize,
    rechecked: usize,
    source_bytes: usize,
    diagnostics: Vec<crate::Diagnostic<'a>>,
    incomplete: Vec<crate::IncompleteDiagnostic<'a>>,
    resources: Vec<crate::ResourceDiagnostic<'a>>,
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
    first_stops: &'a [crate::GapStop],
}
fn hex(hash: hirsel_core_ids::ContentHash) -> String {
    hash.0.iter().map(|b| format!("{b:02x}")).collect()
}
fn run(args: Vec<String>) -> Result<std::convert::Infallible, Box<dyn std::error::Error>> {
    let invocation = match crate::cli::parse(args.into_iter())? {
        crate::cli::Action::Help(help) => {
            print!("{help}");
            std::process::exit(0);
        }
        crate::cli::Action::Invoke(invocation) => invocation,
    };
    let command = invocation.command.name;
    if command == "call" {
        std::process::exit(crate::call::run(crate::call::Options::read(invocation)?)?);
    }
    if command == "sdk" {
        crate::sdk_command::run(crate::sdk_command::Options::read(invocation)?)?;
        std::process::exit(0);
    }
    if command == "tape" {
        let path = invocation
            .positional
            .as_deref()
            .ok_or("tape show requires a tape file")?;
        if !invocation
            .options
            .iter()
            .any(|(option, _)| *option == crate::cli::OptionId::IncludeCredentials)
        {
            return Err("tape show requires --include-credentials".into());
        }
        crate::tape_cli::show(Path::new(path))?;
        std::process::exit(0);
    }
    let replay_command = command == "replay";
    if command == "new" {
        let directory = invocation
            .positional
            .as_deref()
            .ok_or("new requires a package directory")?;
        let library = invocation
            .options
            .iter()
            .any(|(id, _)| *id == crate::cli::OptionId::Lib);
        crate::cli::scaffold(Path::new(directory), library)?;
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
        use crate::cli::OptionId;
        let has = |option| invocation.options.iter().any(|(id, _)| *id == option);
        if command == "fmt" {
            let directory = invocation
                .positional
                .as_deref()
                .ok_or("fmt requires a package directory")?;
            let status = crate::cli_tools::format(
                Path::new(directory),
                has(OptionId::FmtCheck),
                has(OptionId::Signatures),
            )?;
            std::process::exit(status);
        }
        crate::cli_tools::explain(
            invocation.positional.as_deref(),
            has(OptionId::ListRules),
            has(OptionId::Json),
        )?;
        std::process::exit(0);
    }
    let dir = invocation.positional.map(PathBuf::from);
    let mut libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let mut libraries_explicit = false;
    let mut store = None;
    let mut manifest = None;
    let mut metrics = None;
    let mut profile_front = false;
    let mut image_path = None;
    let mut install_root = None;
    let mut core_product = None;
    let mut source_core = false;
    let mut source_std = false;
    let mut product_name = None;
    let mut entry = None;
    let test_command = command == "test";
    let native_command = command == "build" || command == "run" || replay_command || test_command;
    let mut test_filter = None;
    let mut test_timeout = None;
    let mut explicit_limit = false;
    let mut backend = None;
    let mut release = false;
    let mut debug_lines = None;
    let mut runtime = None;
    let mut explicit_runtime = false;
    let mut explicit_target = false;
    let mut logical_charge_report = None;
    let mut output_path = None;
    let mut root_limit = 64 * 1024 * 1024;
    let mut emulator = None;
    let mut sysroot = None;
    let mut target = "x86_64-unknown-linux-gnu".to_owned();
    // How a native request links: --cc, else the recipe the installation
    // captured, else (uninstalled) the pinned LLVM's clang. Never a `cc`
    // found on PATH.
    let mut cc = None;
    let pinned =
        || std::env::var_os("LLVM_SYS_231_PREFIX").map(|p| PathBuf::from(p).join("bin/clang"));
    let run_args = invocation.arguments;
    let mut run_tape = None;
    let mut diagnose = false;
    let mut directories = Vec::new();
    let mut grant_mask = 0;
    let mut net_rules = Vec::new();
    let mut json = false;
    use crate::cli::OptionId as Opt;
    for (option, value) in invocation.options {
        let value = value.unwrap_or_default();
        match option {
            Opt::Json => json = true,
            Opt::Record => {
                run_tape = Some(crate::RunTape {
                    path: value,
                    mode: crate::RunTapeMode::Record,
                })
            }
            Opt::Tape => {
                run_tape = Some(crate::RunTape {
                    path: value,
                    mode: crate::RunTapeMode::Replay,
                })
            }
            Opt::Diagnose => diagnose = true,
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
                if value == "Net" {
                    return Err(
                        format!("Net requires --grant {}", hirsel_core_net::RULE_SYNTAX).into(),
                    );
                }
                if let Some(rules) = value.strip_prefix("Net=") {
                    for rule in rules.split(';') {
                        net_rules.push(hirsel_core_net::Rule::parse(rule)?);
                    }
                    grant_mask |= 1 << 1;
                    continue;
                }
                let kind = crate::grant_kind(&value)
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
            Opt::DebugLines => {
                debug_lines = Some(match value.as_str() {
                    "on" => true,
                    "off" => false,
                    _ => return Err("--debug-lines expects on or off".into()),
                })
            }
            Opt::Backend => backend = Some(value.parse::<Backend>()?),
            Opt::Runtime => {
                explicit_runtime = true;
                runtime = Some(PathBuf::from(value));
            }
            Opt::LogicalChargeReport => {
                logical_charge_report = Some(PathBuf::from(value));
            }
            Opt::Cc => cc = Some(PathBuf::from(value)),
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
            Opt::Libraries => {
                libraries_explicit = true;
                libraries = PathBuf::from(value)
            }
            Opt::Store => store = Some(PathBuf::from(value)),
            Opt::Inputs => manifest = Some(PathBuf::from(value)),
            Opt::Metrics => metrics = Some(PathBuf::from(value)),
            Opt::ProfileFront => profile_front = true,
            Opt::Image => image_path = Some(PathBuf::from(value)),
            Opt::Help
            | Opt::IncludeCredentials
            | Opt::Lib
            | Opt::FmtCheck
            | Opt::Signatures
            | Opt::ListRules
            | Opt::SdkOut
            | Opt::PlaceTimeout
            | Opt::NPlace
            | Opt::Url
            | Opt::CallTimeout => {
                unreachable!("CLI early command option")
            }
        }
    }
    if replay_command {
        if !directories.is_empty() || grant_mask != 0 {
            return Err("replay takes no host grants".into());
        }
        let tape = run_tape.as_mut().ok_or("replay requires --tape TAPE")?;
        if diagnose {
            tape.mode = crate::RunTapeMode::Diagnose;
        }
        let header = crate::tape_cli::header(Path::new(&tape.path))?;
        if !explicit_limit {
            root_limit = header.limit;
        }
        if !run_args.is_empty() {
            return Err("replay takes its arguments from the tape".into());
        }
    }
    if let Some(tape) = &mut run_tape {
        if !Path::new(&tape.path).is_absolute() {
            tape.path = std::env::current_dir()?
                .join(&tape.path)
                .to_str()
                .ok_or("tape path is not UTF-8")?
                .to_owned();
        }
    }
    let runner = match (emulator, sysroot) {
        (None, None) => crate::Runner::Native,
        (Some(emulator), Some(sysroot)) => crate::Runner::qemu(&emulator, &sysroot, &target)?,
        _ => return Err("--emulator and --sysroot must be supplied together".into()),
    };
    if test_command && !explicit_limit {
        return Err("test requires an explicit --limit <bytes> for each run".into());
    }
    if test_command && entry.is_some() {
        return Err("test selects marked functions with --filter, not --entry".into());
    }
    let executable = std::env::current_exe()?;
    let installed = crate::installation::Installed::discover(&executable)?;
    if let Some(installed) = &installed {
        if core_product.is_none() {
            core_product = Some(installed.cache());
        }
        if runtime.is_none() {
            runtime = installed.runtime(&target);
        }
    }
    if command == "env" {
        let cache = core_product.unwrap_or_else(crate::PackageProduct::default_cache);
        let report = crate::installation::environment(
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
    if profile_front && !hirsel_core_ids::published::FRONT_OBSERVER_AVAILABLE {
        return Err("--profile-front needs the front-observer release compiler".into());
    }
    if profile_front && metrics.is_none() {
        return Err("--profile-front requires --metrics".into());
    }
    let mut meter = Meter::new(metrics.is_some());
    if profile_front {
        meter.profile_image_index();
    }
    let installed_std = if installed.is_some() && !libraries_explicit && !source_std && !source_core
    {
        installed.as_ref().unwrap().product("std")?
    } else {
        None
    };
    if image_path.is_some() {
        meter.result.image_selection = Some("image used: explicit image".into());
    } else if installed_std.is_some() {
        meter.result.image_selection = Some("image used: installed std product".into());
    } else if source_std || libraries_explicit {
        meter.result.image_selection = Some("source checked: explicit library source".into());
    } else if installed.is_some() {
        meter.result.image_selection = Some("source checked: no registered std product".into());
    } else {
        meter.result.image_selection = Some("source checked: no installed image".into());
    }
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
                crate::installation::publish(
                    root,
                    command == "std-product",
                    runtime.as_deref().map(|runtime| (runtime, target.as_str())),
                    cc.clone().or_else(pinned).as_deref(),
                    &mut meter,
                )?
            } else if command == "std-product" {
                crate::PackageProduct::bundled_std(&store, &mut meter)?
            } else {
                crate::PackageProduct::bundled(&store, &mut meter)?
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
            serde_json::json!({"image":hex(image.identity),"rebuilt":rebuilt,"name":name,"index_pages":image.checked_index_pages(),"index_rows":image.decoded_index_rows()})
        );
        drop(image);
        meter.finish("package handle teardown");
        if let Some(path) = metrics {
            meter.write_metrics(path)?;
        }
        std::process::exit(0);
    }
    if native_command && (matches!(backend, Some(Backend::Llvm)) || (backend.is_none() && release))
    {
        hirsel_llvm_lazy::llvm_library()?;
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
            inputs: serde_json::from_slice::<CorpusManifest>(&std::fs::read(&manifest)?)?,
            manifest: Some(match manifest.parent() {
                Some(dir) if !dir.as_os_str().is_empty() => dir.to_owned(),
                _ => PathBuf::from("."),
            }),
            store,
            library: image.map_or(LibraryInput::Sources, LibraryInput::Image),
            entry: None,
        }
    } else if command == "image" {
        CheckRequest::libraries(dir.as_deref().unwrap_or(&libraries), &store)?
    } else if let Some(image) = image {
        CheckRequest::with_image(&dir.ok_or("missing package directory")?, &store, image)?
    } else if let Some(image) = installed_std {
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
    let sources = crate::SourceFiles::of(&request);
    let native_store = request.store.join("native");
    let store_root = request.store.clone();
    meter.finish("startup/discovery");
    // Native options come before the reach: a Cranelift development build
    // walks its instance records, which its options address. An options
    // error is reported only for a request check accepts.
    let mut prepared = if native_command {
        let prepare = || -> Result<_, Box<dyn std::error::Error>> {
            let default_output = if test_command {
                native_store.join("tests")
            } else {
                PathBuf::from("program")
            };
            let link = match (cc, &installed) {
                (Some(cc), _) => Link::Driver(crate::link_driver(&cc)?),
                (None, Some(installed)) => Link::Installed(installed.link(&target).ok_or_else(
                    || format!("this installation links its host target only, not {target}; pass --cc"),
                )?),
                (None, None) => Link::Driver(crate::link_driver(&pinned().ok_or(
                    "no link driver: this compiler is not installed and LLVM_SYS_231_PREFIX is unset; pass --cc",
                )?)?),
            };
            let mut options = NativeOptions::development(
                target.clone(),
                runtime.ok_or("build/run/test requires --runtime <core runtime archive>")?,
                link,
                native_store,
                output_path.unwrap_or(default_output),
            );
            if release {
                options.backend = Backend::Llvm;
                options.optimize = true;
            }
            options.backend = backend.unwrap_or(options.backend);
            options.debug_lines = debug_lines.unwrap_or(options.debug_lines);
            options.root_limit = root_limit;
            options.directories = directories;
            options.grant_mask = grant_mask;
            options.net_rules = net_rules;
            options.logical_charge_report = logical_charge_report;
            options.tape = run_tape;
            Ok(options)
        };
        Some(prepare())
    } else {
        None
    };
    // The request record: not for an image or a test run, nor for an
    // observation build, whose report is a second output; a request whose
    // options or toolchain cannot be read goes on without one and meets
    // that error where it always has.
    let record = match &mut prepared {
        _ if command == "image" || test_command => None,
        Some(Ok(options)) if options.logical_charge_report.is_none() => {
            crate::request_record::Request::new(
                command,
                (source_core, source_std, core_product.is_some()),
                Some(options),
            )
            .ok()
        }
        Some(_) => None,
        None => Some(crate::request_record::Request::new(
            command,
            (source_core, source_std, core_product.is_some()),
            None,
        )?),
    };
    let mut driver = Driver::default();
    let (checked, image) = if command == "image" {
        let (product, image) = driver.produce_image(request, &mut meter)?;
        (crate::Checked::Product(product, None), image)
    } else {
        (
            if !source_core && (native_command || core_product.is_some()) {
                let root = core_product.unwrap_or_else(crate::PackageProduct::default_cache);
                driver.check_package_product(
                    request,
                    &root,
                    &mut meter,
                    (native_command, !source_std),
                    record.as_ref(),
                )?
            } else {
                driver.check_recorded(request, &mut meter, record.as_ref())?
            },
            None,
        )
    };
    let (product, key) = match checked {
        crate::Checked::Product(product, key) => (product, key),
        crate::Checked::Hit(hit) => {
            let output = native_output(&prepared);
            return answer_hit(
                &hit,
                command,
                json,
                &sources,
                output.as_deref(),
                &runner,
                run_args,
                &mut meter,
                metrics,
            );
        }
    };
    let reach = if native_command && !test_command {
        Some({
            let entry = selected_entry
                .as_ref()
                .ok_or("build/run requires an entry")?;
            match &prepared {
                Some(Ok(options)) => product.reachable_for(entry, options)?,
                _ => product.reachable(entry, 65_536)?,
            }
        })
    } else {
        None
    };
    let accepted = reach
        .as_ref()
        .map_or_else(|| product.accepted(), |r| r.accepted());
    let mut native_options = if native_command && (test_command || accepted) {
        let options = prepared.expect("a native command prepares its options")?;
        if options.logical_charge_report.is_some() {
            eprintln!(
                "observation build: root peak reservation includes bound, task and grant reservations (MEM-19); timings are not performance samples"
            );
        }

        Some(options)
    } else {
        None
    };
    if test_command {
        let options = native_options.as_mut().unwrap();
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
            meter.write_metrics(path)?;
        }
        std::process::exit(if tests.is_empty() {
            2
        } else {
            i32::from(failed)
        });
    }
    let mut stopped = None;
    // A build whose executable is stored though writing it to the output
    // failed (D3).
    let mut undelivered = None;
    let native = if native_command && accepted {
        let options = native_options.as_ref().unwrap();
        match product.build_reachable(
            selected_entry
                .as_ref()
                .ok_or("build/run requires an entry")?,
            reach.as_ref().unwrap(),
            options,
        ) {
            Ok(native) => Some(native),
            Err(crate::NativeStop::Output { product, error }) => {
                stopped = Some(crate::native::output_stop(&error));
                undelivered = Some(*product);
                None
            }
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
                .filter(|e| !matches!(e, crate::InstanceStop::Resource { .. }))
                .map(|e| format!("instances: {e:?}"))
        }),
        reachable: reach.as_ref().map(|r| ReachableOutput {
            bodies: r.bodies,
            declarations: r.declarations,
            census: r.census(),
            first_stops: &r.first_stops,
        }),
    };
    // A request that stopped is never recorded, except for the output
    // write after a stored build.
    if let Some(key) = key
        && (output.stopped.is_none() || undelivered.is_some())
    {
        let value = crate::request_record::Value::of(crate::request_record::Answer {
            product: &product,
            diagnostics: &output.diagnostics,
            incomplete: &output.incomplete,
            resources: &output.resources,
            reach: reach.as_ref(),
            native: output.native.as_ref().or(undelivered.as_ref()),
        });
        crate::request_record::publish(&store_root, key, &value)?;
        meter.finish("request record/publish");
    }
    finish(
        &output, command, json, &sources, &runner, run_args, &mut meter, metrics,
    )
}
/// The output path of a native request's options.
fn native_output(
    prepared: &Option<Result<NativeOptions, Box<dyn std::error::Error>>>,
) -> Option<PathBuf> {
    match prepared {
        Some(Ok(options)) => Some(options.output.clone()),
        _ => None,
    }
}
/// Answer a request from its record: render the value for this request,
/// write a build's stored executable to its output, and finish as a checked
/// request does.
#[allow(clippy::too_many_arguments)]
fn answer_hit(
    hit: &crate::request_record::Hit,
    command: &str,
    json: bool,
    sources: &crate::SourceFiles,
    output_path: Option<&Path>,
    runner: &crate::Runner,
    run_args: Vec<String>,
    meter: &mut Meter,
    metrics: Option<PathBuf>,
) -> Result<std::convert::Infallible, Box<dyn std::error::Error>> {
    let rendered = hit.render();
    let mut stopped = None;
    let native = match (output_path, &hit.executable) {
        (Some(path), Some(bytes)) => {
            let product = hit.build(path).expect("a stored executable has its build");
            match crate::native::deliver(path, bytes) {
                Ok(()) => Some(product),
                Err(error) => {
                    stopped = Some(crate::native::output_stop(&error));
                    None
                }
            }
        }
        _ => None,
    };
    meter.finish("request record/deliver");
    let reachable = rendered
        .reach
        .as_ref()
        .map(
            |(bodies, declarations, census, first_stops)| ReachableOutput {
                bodies: *bodies,
                declarations: *declarations,
                census: census.clone(),
                first_stops,
            },
        );
    let output = Output {
        accepted: rendered.accepted,
        root: if json {
            hex(rendered.root)
        } else {
            String::new()
        },
        files: rendered.files,
        rechecked: 0,
        source_bytes: rendered.source_bytes,
        diagnostics: rendered.diagnostics,
        incomplete: rendered.incomplete,
        resources: rendered.resources,
        image: None,
        native,
        stopped,
        reachable,
    };
    finish(
        &output, command, json, sources, runner, run_args, meter, metrics,
    )
}
/// Print `output`, run a `run` request's executable, and exit with the
/// request's status.
#[allow(clippy::too_many_arguments)]
fn finish(
    output: &Output<'_>,
    command: &str,
    json: bool,
    sources: &crate::SourceFiles,
    runner: &crate::Runner,
    run_args: Vec<String>,
    meter: &mut Meter,
    metrics: Option<PathBuf>,
) -> Result<std::convert::Infallible, Box<dyn std::error::Error>> {
    if json {
        println!("{}", serde_json::to_string(output)?);
    } else {
        for d in &output.diagnostics {
            eprintln!("{}: {} {}", sources.render(&d.primary), d.rule, d.message);
            for site in &d.causal {
                eprintln!("  caused by {}", sources.render(site));
            }
        }
        for g in &output.incomplete {
            eprintln!("{}: incomplete {}", sources.render(&g.primary), g.message);
            for site in &g.causal {
                eprintln!("  caused by {}", sources.render(site));
            }
        }
        for r in &output.resources {
            eprintln!("{}: resource {}", sources.render(&r.primary), r.message);
        }
        if let Some(stop) = &output.stopped {
            eprintln!("{stop}");
        }
        if command != "run" && command != "replay" {
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
    let run_exit = if command == "run" || command == "replay" {
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
        meter.write_metrics(path)?;
    }
    // Publication and output are complete. The OS reclaims these in-memory
    // graphs at exit; the reusable library Driver retains ordinary destruction.
    std::process::exit(exit)
}
/// Run the request `args` names (the arguments after the program name) and
/// exit with its status. Process setup (allocator guard, signal dispositions,
/// inherited descriptors) is the caller's.
pub fn main(args: Vec<String>) -> ! {
    match run(args) {
        Ok(never) => match never {},
        Err(e) => {
            eprintln!("hirsel: {e}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod net_grant_tests {
    #[test]
    fn bare_net_grant_is_refused_before_discovery_with_the_rule_syntax() {
        let result = super::run(
            ["run", "unused", "--grant", "Net"]
                .map(str::to_owned)
                .to_vec(),
        );
        let error = result
            .err()
            .expect("a bare Net grant cannot confer authority");
        assert!(error.to_string().contains(hirsel_core_net::RULE_SYNTAX));
    }
}
