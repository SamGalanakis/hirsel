//! One compiler request, from its arguments to its output and exit status:
//! the `hirsel` executable's whole behaviour after process setup. It lives in
//! the library so that a test process can run a request exactly as the
//! executable does (request_sequences.rs compares their outputs).
use crate::code_store::CodeStore;
use crate::native::recipe_build::Publication;
use crate::{
    Backend, BuildProduct, CheckRequest, CoreImage, Driver, LibraryInput, Link, Meter,
    NativeOptions,
};
use hirsel_core_resolve::CorpusManifest;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
pub(crate) fn hex(hash: hirsel_core_ids::ContentHash) -> String {
    hash.0.iter().map(|b| format!("{b:02x}")).collect()
}
/// Local loading still parses disk input under CA4's decoder. Size limits
/// follow the process allowance. Bound recursive decoder stack use explicitly.
pub(crate) fn local_admission_limits() -> crate::code_admission::AdmissionLimits {
    crate::code_admission::AdmissionLimits {
        recipe_bytes: usize::MAX,
        closure_objects: usize::MAX,
        object_bytes: u64::MAX,
        decoded_bytes: usize::MAX,
        members: usize::MAX,
        edges: usize::MAX,
        nesting: 512,
        outstanding: 0,
        timeout: None,
    }
}
fn run(args: Vec<String>) -> Result<std::convert::Infallible, Box<dyn std::error::Error>> {
    let mut invocation = match crate::cli::parse(args.into_iter())? {
        crate::cli::Action::Help(help) => {
            print!("{help}");
            std::process::exit(0);
        }
        crate::cli::Action::Version => {
            crate::installation::print_version()?;
            std::process::exit(0);
        }
        crate::cli::Action::Invoke(invocation) => invocation,
    };
    let command = invocation.command.name;
    if matches!(command, "host" | "deploy") {
        std::process::exit(crate::host::run(invocation)?);
    }
    if command == "replay"
        && invocation
            .options
            .iter()
            .any(|(option, _)| *option == Opt::HostState)
    {
        std::process::exit(crate::host::replay(invocation)?);
    }
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
    if command == "manifest" {
        crate::release_manifest::show(Path::new(
            invocation
                .positional
                .as_deref()
                .ok_or("manifest show requires a path")?,
        ))?;
        std::process::exit(0);
    }
    let mut package_mode = crate::package_lock::Mode::default();
    let mut package_store = None;
    let mut pack_out = None;
    for (option, value) in invocation.options.extract_if(.., |(option, _)| {
        matches!(
            option,
            Opt::Locked | Opt::Frozen | Opt::PackageStore | Opt::PackOut
        )
    }) {
        match option {
            Opt::Locked if package_mode != crate::package_lock::Mode::Frozen => {
                package_mode = crate::package_lock::Mode::Locked
            }
            Opt::Frozen => package_mode = crate::package_lock::Mode::Frozen,
            Opt::PackageStore => package_store = value.map(PathBuf::from),
            Opt::PackOut => pack_out = value.map(PathBuf::from),
            _ => {}
        }
    }
    let package_store = crate::package_store::PackageStore::new(package_store);
    let package_command = matches!(command, "lock" | "fetch" | "pack");
    if package_command {
        let directory = invocation.positional.as_deref().unwrap_or(".");
        let workspace = crate::package_workspace::Workspace::discover_for(
            Path::new(directory),
            command == "pack",
        )?
        .ok_or_else(|| format!("{directory} has no workspace manifest"))?;
        let selected = (command == "pack")
            .then(|| workspace.selected().cloned())
            .transpose()?;
        let mut resolved = crate::package_resolve::Resolution::resolve(
            workspace,
            package_store,
            package_mode == crate::package_lock::Mode::Frozen,
        )?;
        let lock = crate::package_lock::Lock::compare(&resolved, package_mode)?;
        if let Some(selected) = selected {
            let out = pack_out.ok_or("pack requires --out FILE")?;
            let (bytes, archive) = resolved.make_archive(&selected)?;
            let output = crate::package_lock::PackOutput::stage(&resolved, &archive, &lock, &out)?;
            lock.commit(&resolved.workspace)?;
            resolved.store.install(&archive)?;
            output.write(&bytes)?;
            println!("{selected} {}", archive.head);
            for (root, library) in &archive.entries {
                if *root != archive.head {
                    println!("{} {root}", library.content.id);
                }
            }
        } else {
            lock.commit(&resolved.workspace)?;
        }
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
        crate::package_store::atomic_write(
            &Path::new(directory).join("hirsel.lock"),
            crate::package_lock::scaffold().as_bytes(),
        )?;
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
    if command == "primer" {
        if invocation.positional.is_some() {
            return Err("primer takes no arguments".into());
        }
        print!("{}", crate::cli_tools::PRIMER);
        std::process::exit(0);
    }
    if command == "fmt" || command == "explain" {
        use crate::cli::OptionId;
        let has = |option| invocation.options.iter().any(|(id, _)| *id == option);
        if command == "fmt" {
            use hirsel_core_syntax::{FormatAction, ParseMode};
            let action = if has(OptionId::FmtCheck) {
                FormatAction::Check
            } else {
                FormatAction::Write
            };
            let mode = if has(OptionId::Signatures) {
                ParseMode::Signatures
            } else {
                ParseMode::Source
            };
            let status = match (has(OptionId::FmtStdin), invocation.positional.as_deref()) {
                (true, None) => crate::cli_tools::format_stdin(action, mode)?,
                (false, Some(path)) => crate::cli_tools::format(Path::new(path), action, mode)?,
                _ => return Err("fmt requires one of FILE, DIR, or --stdin".into()),
            };
            std::process::exit(status);
        }
        crate::cli_tools::explain(
            invocation.positional.as_deref(),
            has(OptionId::ListRules),
            has(OptionId::Json),
        )?;
        std::process::exit(0);
    }
    let inspection = (command == "inspect")
        .then(|| crate::inspect::Inspection::read(&mut invocation))
        .transpose()?;
    let inspect_std = inspection.as_ref().is_some_and(|option| option.std);
    let inspect_foreign = inspection
        .as_ref()
        .and_then(crate::inspect::Inspection::foreign_module);
    let dir = invocation
        .positional
        .map(PathBuf::from)
        .or_else(|| inspect_foreign.map(|_| PathBuf::from(".")));
    let mut libraries = None;
    let mut store = None;
    let mut manifest = None;
    let mut metrics = None;
    let mut profile_front = false;
    let mut image_path = None;
    let mut install_root = None;
    let mut core_product = None;
    let mut source_core = inspect_std;
    let mut source_std = false;
    let mut product_name = None;
    let mut entry = None;
    let test_command = command == "test";
    let recipe_command = matches!(command, "build" | "publish");
    let native_command = recipe_command || command == "run" || replay_command || test_command;
    let mut recipe_root = None;
    let mut code_store = None;
    let mut release_manifest = None;
    let mut n_place = None;
    let mut place_timeout = Duration::from_secs(10);
    let mut test_filter = None;
    let mut test_jobs = None;
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
    let mut recording_build = false;
    let mut recording_runtime = None;
    let mut record_path = None;
    let mut replay_tape = None;
    let mut test_replay = false;
    let mut test_rerun = false;
    let mut expect_verdict = None;
    let mut diagnose = false;
    let mut grants = crate::package_dir::Grants::default();
    let mut json = false;
    use crate::cli::OptionId as Opt;
    for (option, value) in invocation.options {
        let value = value.unwrap_or_default();
        match option {
            Opt::Json => json = true,
            Opt::Recipe => recipe_root = Some(crate::native::recipe_build::parse_root(&value)?),
            Opt::CodeStore => code_store = Some(PathBuf::from(value)),
            Opt::ReleaseManifest => release_manifest = Some(PathBuf::from(value)),
            Opt::NPlace => n_place = Some(value.parse::<u64>()?),
            Opt::PlaceTimeout => {
                place_timeout = Duration::from_secs(value.parse::<u64>()?);
                if place_timeout.is_zero() {
                    return Err("--place-timeout must be positive".into());
                }
            }
            Opt::Recording => recording_build = true,
            Opt::RecordingRuntime => recording_runtime = Some(PathBuf::from(value)),
            Opt::Record => record_path = Some(value),
            Opt::Tape => replay_tape = Some(value),
            Opt::Replay => test_replay = true,
            Opt::Rerun => test_rerun = true,
            Opt::ExpectVerdict => expect_verdict = Some(value),
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
            Opt::Dir => grants.directories.push(value),
            Opt::Grant => grants.grant(&value)?,
            Opt::Filter => test_filter = Some(value),
            Opt::TestJobs => {
                test_jobs = Some(
                    value
                        .parse()
                        .map_err(|_| "-j expects a positive job count")?,
                );
            }
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
            Opt::Libraries => libraries = Some(PathBuf::from(value)),
            Opt::Store => store = Some(PathBuf::from(value)),
            Opt::Inputs => manifest = Some(PathBuf::from(value)),
            Opt::Metrics => metrics = Some(PathBuf::from(value)),
            Opt::ProfileFront => profile_front = true,
            Opt::Image => image_path = Some(PathBuf::from(value)),
            Opt::Help
            | Opt::Interface
            | Opt::ForeignHeader
            | Opt::Dependents
            | Opt::Transitive
            | Opt::AffectedTests
            | Opt::Unused
            | Opt::InspectLibrary
            | Opt::InspectStd
            | Opt::IncludeCredentials
            | Opt::Lib
            | Opt::FmtCheck
            | Opt::FmtStdin
            | Opt::Signatures
            | Opt::ListRules
            | Opt::SdkOut
            | Opt::PackOut
            | Opt::PackageStore
            | Opt::Locked
            | Opt::Frozen
            | Opt::Url
            | Opt::Control
            | Opt::HostState
            | Opt::Workdir
            | Opt::Listen
            | Opt::Scratch
            | Opt::GrantAdded
            | Opt::MaxPause
            | Opt::MaxRecipeBytes
            | Opt::MaxClosure
            | Opt::MaxDecodeBytes
            | Opt::StagingBytes
            | Opt::AdmitTimeout
            | Opt::HostMemory
            | Opt::StartRetries
            | Opt::TapeRetention
            | Opt::CallTimeout => {
                unreachable!("CLI early command option")
            }
        }
    }
    if expect_verdict.is_some() && !test_replay {
        return Err("--expect-verdict requires --replay".into());
    }
    let app_directory = dir.clone().or_else(|| {
        (manifest.is_none()
            && recipe_root.is_none()
            && matches!(command, "check" | "build" | "publish" | "run" | "test"))
        .then(|| PathBuf::from("."))
    });
    let mut workspace_app = None;
    let mut foreign_symbols = None;
    if manifest.is_some() {
        package_mode.require_workspace("--inputs")?;
    } else if recipe_root.is_some() {
        package_mode.require_workspace("--recipe")?;
    } else if inspect_std {
        package_mode.require_workspace("--std")?;
    } else if matches!(
        command,
        "check" | "build" | "publish" | "run" | "test" | "inspect" | "replay"
    ) {
        if let Some(directory) = &app_directory {
            if let Some(workspace) = crate::package_workspace::Workspace::discover(directory)? {
                let selected = workspace.selected()?.clone();
                let resolved = crate::package_resolve::Resolution::resolve(
                    workspace,
                    package_store,
                    package_mode == crate::package_lock::Mode::Frozen,
                )?;
                if let Some(path) = inspect_foreign {
                    foreign_symbols = Some(crate::foreign_header::symbol_prefix(&resolved, path)?);
                }
                crate::package_lock::Lock::compare(&resolved, package_mode)?
                    .commit(&resolved.workspace)?;
                let include_tests = test_command
                    || inspection
                        .as_ref()
                        .is_some_and(|option| option.includes_tests());
                workspace_app = Some(crate::package_dir::PackageDir::from_resolution(
                    &resolved,
                    &selected,
                    include_tests,
                )?);
            } else {
                package_mode.require_workspace("the directory convention")?;
            }
        } else {
            package_mode.require_workspace("the request")?;
        }
    }
    let mut replay_plan = None;
    if replay_command {
        if !grants.is_empty() {
            return Err("replay takes no host grants".into());
        }
        let mut path = replay_tape.ok_or("replay requires --tape TAPE")?;
        if !Path::new(&path).is_absolute() {
            path = std::env::current_dir()?
                .join(&path)
                .to_str()
                .ok_or("tape path is not UTF-8")?
                .to_owned();
        }
        if !run_args.is_empty() {
            return Err("replay takes its arguments from the tape".into());
        }
        replay_plan = Some(crate::ReplayPlan { path, diagnose });
    }
    if let Some(path) = &mut record_path {
        if !Path::new(path).is_absolute() {
            *path = std::env::current_dir()?
                .join(&*path)
                .to_str()
                .ok_or("tape path is not UTF-8")?
                .to_owned();
        }
    }
    let needs_recording = recording_build || record_path.is_some() || replay_command || test_replay;
    let stored_recipe = if let Some(root) = recipe_root {
        if dir.is_some()
            || manifest.is_some()
            || entry.is_some()
            || image_path.is_some()
            || libraries.is_some()
            || source_core
            || source_std
            || core_product.is_some()
            || explicit_target
            || explicit_limit
            || n_place.is_some()
            || !grants.is_empty()
        {
            return Err("--recipe supplies source, target, limits and grants".into());
        }
        let path = code_store
            .clone()
            .ok_or("--recipe requires --code-store DIR")?;
        let publication =
            Publication::open(&CodeStore::new(path), root, &local_admission_limits())?;
        target.clone_from(&publication.recipe().launch.target);
        Some(publication)
    } else {
        None
    };
    let dir = app_directory;
    let runner = match (emulator, sysroot) {
        (None, None) => crate::Runner::Native,
        (Some(emulator), Some(sysroot)) => crate::Runner::qemu(&emulator, &sysroot, &target)?,
        _ => return Err("--emulator and --sysroot must be supplied together".into()),
    };
    if test_command && entry.is_some() {
        return Err("test selects marked functions with --filter, not --entry".into());
    }
    let executable = std::env::current_exe()?;
    let installed = crate::installation::Installed::discover(&executable)?;
    let env_recording_runtime = installed
        .as_ref()
        .and_then(|installed| installed.recording_runtime(&target));
    if let Some(installed) = &installed {
        if core_product.is_none() {
            core_product = Some(installed.cache());
        }
        if runtime.is_none() {
            runtime = if needs_recording {
                installed.recording_runtime(&target)
            } else {
                installed.runtime(&target)
            };
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
            env_recording_runtime.as_deref(),
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
            println!(
                "recording runtime: {}",
                report.recording_runtime.as_ref().map_or_else(
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
    if install_root.is_none()
        && ((command == "core-product" && (explicit_runtime || explicit_target))
            || (matches!(command, "core-product" | "std-product") && recording_runtime.is_some()))
    {
        return Err(
            "core-product --runtime/--target and --recording-runtime require --install".into(),
        );
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
    let installed_std = if installed.is_some()
        && libraries.is_none()
        && !source_std
        && !source_core
        && workspace_app.as_ref().is_none_or(|app| app.needs_std())
    {
        installed.as_ref().unwrap().product("std")?
    } else {
        None
    };
    if image_path.is_some() {
        meter.result.image_selection = Some("image used: explicit image".into());
    } else if installed_std.is_some() {
        meter.result.image_selection = Some("image used: installed std product".into());
    } else if source_std || libraries.is_some() {
        meter.result.image_selection = Some("source checked: explicit library source".into());
    } else if installed.is_some() {
        meter.result.image_selection = Some("source checked: no registered std product".into());
    } else {
        meter.result.image_selection = Some("source checked: no installed image".into());
    }
    // An uninstalled compiler can use the current checkout for source checks.
    let libraries = libraries.as_deref().or_else(|| {
        (installed.is_none() && Path::new("core/std").is_dir()).then_some(Path::new("core"))
    });
    let store = store
        .or_else(|| install_root.as_ref().map(|root| root.join("core-packages")))
        .unwrap_or_else(|| {
            dir.as_deref()
                .unwrap_or(Path::new("."))
                .join(".hirsel-core")
        });
    let code_store = CodeStore::new(code_store.unwrap_or_else(|| store.join("code-store")));
    let select_link = |cc: Option<PathBuf>| -> Result<Link, Box<dyn std::error::Error>> {
        match (cc, &installed) {
            (Some(cc), _) => Ok(Link::Driver(crate::link_driver(&cc)?)),
            (None, Some(installed)) => Ok(Link::Installed(installed.link(&target).ok_or_else(
                || format!("this installation links its host target only, not {target}; pass --cc"),
            )?)),
            (None, None) => Ok(Link::Driver(crate::link_driver(&pinned().ok_or(
                "no link driver: this compiler is not installed and LLVM_SYS_231_PREFIX is unset; pass --cc",
            )?)?)),
        }
    };
    let select_runtime = || -> Result<PathBuf, Box<dyn std::error::Error>> {
        let archive = runtime.as_deref().ok_or_else(|| {
            if needs_recording {
                format!("no recording runtime archive for {target}; pass --runtime with a recording archive")
            } else {
                "build/run/test requires --runtime <core runtime archive>".into()
            }
        })?;
        if explicit_runtime {
            let bytes = std::fs::read(archive)?;
            crate::installation::check_runtime(archive, &bytes, needs_recording)?;
        }
        Ok(archive.to_owned())
    };
    if let Some(publication) = stored_recipe {
        use hirsel_core_ir::release_recipe::Backend as RecipeBackend;
        let root = publication.root();
        let recipe = publication.recipe();
        let mut options = NativeOptions::development(
            target.clone(),
            select_runtime()?,
            select_link(cc)?,
            store.join("native"),
            output_path.unwrap_or_else(|| PathBuf::from("program")),
        );
        options.backend = match recipe.launch.backend {
            RecipeBackend::Cranelift => Backend::Cranelift,
            RecipeBackend::Llvm => Backend::Llvm,
        };
        if backend.is_some_and(|backend| backend != options.backend) {
            return Err("--backend differs from the recipe".into());
        }
        options.optimize =
            release || (matches!(options.backend, Backend::Llvm) && backend.is_none());
        options.debug_lines = debug_lines.unwrap_or(options.debug_lines);
        options.logical_charge_report = logical_charge_report;
        options.recording = needs_recording;
        let recipe = release_manifest.as_ref().map(|_| recipe.clone());
        let prebuilt = installed_std.as_ref().map(|image| {
            image
                .store_root()
                .join(crate::native::std_release::PREBUILT)
        });
        let product =
            publication.assemble_with(&options, Default::default(), None, prebuilt.as_deref())?;
        if let (Some(path), Some(recipe)) = (release_manifest, recipe) {
            crate::release_manifest::write(
                &path,
                &product.executable,
                root,
                &recipe,
                place_timeout,
            )?;
        }
        if json {
            println!(
                "{}",
                serde_json::json!({ "accepted": true, "native": product })
            );
        } else {
            println!("built {}", product.executable.display());
        }
        std::process::exit(0);
    }
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
                    recording_runtime
                        .as_deref()
                        .map(|runtime| (runtime, target.as_str())),
                    cc.clone().or_else(pinned).as_deref(),
                    &mut meter,
                )?
            } else if command == "std-product" {
                let package = crate::PackageProduct::bundled_std(&store, &mut meter)?;
                if let Some(runtime) = &runtime {
                    let bytes = std::fs::read(runtime)?;
                    crate::installation::check_runtime(runtime, &bytes, false)?;
                    let recording = recording_runtime
                        .as_deref()
                        .map(|path| {
                            let bytes = std::fs::read(path)?;
                            crate::installation::check_runtime(path, &bytes, true)?;
                            std::io::Result::Ok(bytes)
                        })
                        .transpose()?;
                    crate::installation::prebuild_release(
                        &package.image,
                        &target,
                        &bytes,
                        recording.as_deref(),
                        &mut meter,
                    )?;
                }
                package
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
    if command != "publish"
        && native_command
        && (matches!(backend, Some(Backend::Llvm)) || (backend.is_none() && release))
    {
        hirsel_llvm_lazy::llvm_library()?;
    }
    let image = image_path
        .as_ref()
        .map(|path| {
            CoreImage::open(path)?
                .ok_or_else(|| std::io::Error::other("core image cache miss; rebuild the image"))
        })
        .transpose()?;
    if command == "image" && image.is_some() {
        return Err("image production takes sources, not an image".into());
    }
    let mut manifest_runs = None;
    let mut test_owner = None;
    let mut request = if let Some(manifest) = manifest {
        CheckRequest {
            inputs: serde_json::from_slice::<CorpusManifest>(&std::fs::read(&manifest)?)?,
            manifest: Some(match manifest.parent() {
                Some(dir) if !dir.as_os_str().is_empty() => dir.to_owned(),
                _ => PathBuf::from("."),
            }),
            store,
            library: image
                .clone()
                .map_or(LibraryInput::Sources, LibraryInput::Image),
            entry: None,
        }
    } else if inspect_std {
        CheckRequest::library_request(libraries, &store)?
    } else if command == "image" {
        CheckRequest::library_request(dir.as_deref().or(libraries), &store)?
    } else {
        let mut app = match workspace_app {
            Some(app) => app,
            None => crate::package_dir::PackageDir::convention(
                dir.as_deref().ok_or("missing package directory")?,
            )?,
        };
        test_owner = Some(app.package.package.clone());
        manifest_runs = app.tests.take();
        CheckRequest::from_app(
            app,
            libraries,
            &store,
            native_command || installed.is_some() || inspection.is_some() || image.is_some(),
        )?
    };
    if entry.is_some() {
        request.entry = entry;
    }
    if let Some(module) = inspect_foreign {
        let symbols =
            foreign_symbols.ok_or("inspect --foreign-header requires a foreign manifest")?;
        print!(
            "{}",
            crate::foreign_header::header(request.inputs, module, &symbols)?
        );
        std::process::exit(0);
    }
    // PROG-4: each test's run configuration, from the package's manifest
    // when it declares one, else from the command line.
    let test_runs = if test_command {
        request.entry = None;
        Some(match manifest_runs {
            Some(runs) => {
                let mut configured = grants.clone();
                configured.foreign.clear();
                if explicit_limit || !configured.is_empty() {
                    return Err("the package's manifest declares its tests' run configuration; test takes no --limit, --dir or --grant".into());
                }
                runs
            }
            None => {
                if !explicit_limit {
                    return Err("test requires an explicit --limit <bytes> for each run".into());
                }
                // A test runs in a working directory of its own; a directory
                // the command line names is the invoker's.
                for directory in &mut grants.directories {
                    *directory = std::path::absolute(&*directory)?
                        .to_str()
                        .ok_or("directory path is not UTF-8")?
                        .to_owned();
                }
                crate::package_dir::TestRuns::given(root_limit, grants.clone())
            }
        })
    } else {
        None
    };
    if test_command && let Some(path) = &mut logical_charge_report {
        *path = std::path::absolute(&*path)?;
    }
    let selected_entry = request.entry.clone();
    request.capture_sources()?;
    let sources = crate::SourceFiles::of(&request);
    if let Some(owner) = &test_owner {
        if let Some(image) = image {
            if !request.use_image(image)? {
                return Err(format!(
                    "--image {} does not hold exactly this request's packages",
                    image_path.unwrap().display()
                )
                .into());
            }
        } else if let Some(image) = installed_std {
            request.use_image(image)?;
        } else if request.manifest.is_some()
            && !source_core
            && !source_std
            && let Some(image) =
                crate::image::closure_image(&request, owner, command == "check", &mut meter)?
            && request.use_image(image)?
        {
            meter.result.image_selection = Some("image used: workspace dependencies".into());
        }
    }

    let native_store = request.store.join("native");
    let store_root = request.store.clone();
    let foreign_replay = replay_command
        && request
            .inputs
            .packages
            .iter()
            .any(|package| package.sources.iter().any(|source| source.foreign));
    if let Some(replay) = &replay_plan
        && !foreign_replay
    {
        let header = crate::tape_cli::header(Path::new(&replay.path))?;
        if !explicit_limit {
            root_limit = header.limit;
        }
    }
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
            let publishing = command == "publish";
            let link = if publishing {
                Link::Driver(PathBuf::new())
            } else {
                select_link(cc)?
            };
            let archive = if publishing {
                PathBuf::new()
            } else {
                select_runtime()?
            };
            let mut options = NativeOptions::development(
                target.clone(),
                archive,
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
            grants.apply(&mut options);
            options.logical_charge_report = logical_charge_report;
            options.recording = needs_recording;
            options.replay = replay_plan;
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
        _ if command == "image"
            || test_command
            || recipe_command
            || !grants.foreign.is_empty()
            || inspection.is_some()
            || foreign_replay =>
        {
            None
        }
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
                record_path.as_deref(),
                &mut meter,
                metrics,
            );
        }
    };
    if foreign_replay {
        let entry = selected_entry.as_ref().ok_or("replay requires an entry")?;
        let limit = prepared
            .as_ref()
            .and_then(|options| options.as_ref().ok())
            .map_or(crate::native::DEFAULT_INSTANCE_LIMIT, |options| {
                options.instance_limit
            });
        if let Some(crate::NativeStop::ForeignRecording { reason, site }) = product
            .reachable(entry, limit)?
            .recording_stop(&product, entry)?
        {
            eprintln!("{site}: recording refusal site");
            eprintln!("replay: refused: {reason}");
            std::process::exit(2);
        }
        if let Some(Ok(options)) = &mut prepared {
            let header =
                crate::tape_cli::header(Path::new(&options.replay.as_ref().unwrap().path))?;
            if !explicit_limit {
                options.root_limit = header.limit;
            }
        }
    }
    let reach = if native_command && !test_command {
        Some({
            let entry = selected_entry
                .as_ref()
                .ok_or("build/run requires an entry")?;
            match &prepared {
                Some(Ok(options))
                    if recipe_command || (command == "run" && !grants.foreign.is_empty()) =>
                {
                    product.reachable(entry, options.instance_limit)?
                }
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
    if accepted && let Some(inspection) = inspection {
        inspection.write(&product, selected_entry.as_ref(), json)?;
        std::process::exit(0);
    }
    let native_options = if native_command && (test_command || accepted) {
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
        use crate::{Source, TestCache};

        let test_runs = test_runs.expect("a test request has its run configuration");
        let own_tests = || {
            product.tests().filter(|path| {
                test_owner
                    .as_ref()
                    .is_none_or(|owner| &path.0.package == owner)
            })
        };
        test_runs.check_names(&own_tests().collect())?;
        let tests: Vec<_> = own_tests()
            .filter(|path| {
                test_filter
                    .as_ref()
                    .is_none_or(|filter: &String| path.to_string().contains(filter.as_str()))
            })
            .cloned()
            .collect();
        let cache = if !test_replay && matches!(runner, crate::Runner::Native) {
            TestCache::open(&TestCache::default_root())
        } else {
            None
        };
        let mut rows = Vec::new();
        let mut failed = false;
        let batch = crate::test_run::TestBatch {
            runs: &test_runs,
            jobs: test_jobs.map_or_else(crate::test_run::affinity_jobs, Ok)?,
            timeout: test_timeout,
            runner: &runner,
            replay: test_replay,
            expect_verdict: expect_verdict.as_deref(),
            cache: cache.as_ref(),
            rerun: test_rerun,
        };
        batch.run(&product, &tests, native_options.as_ref().unwrap(), |test, result| {
            let row = match result {
                Ok((outcome, source)) => {
                    let passed = outcome.passed();
                    let wall_ms = (source == Source::Executed)
                        .then(|| outcome.wall_time.as_secs_f64() * 1000.0);
                    let source = source.as_str();
                    failed |= !passed;
                    if !json {
                        if let Some(wall_ms) = wall_ms {
                            if passed {
                                println!("pass {source} {test} ({wall_ms:.3} ms)");
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
                                println!("fail {source} {test}: {reason} ({wall_ms:.3} ms)");
                            }
                        } else {
                            println!("pass {source} {test}");
                        }
                    }
                    serde_json::json!({"definition":test.to_string(),"passed":passed,"source":source,"status":outcome.output.status.code(),"stdout":String::from_utf8_lossy(&outcome.output.stdout),"stderr":String::from_utf8_lossy(&outcome.output.stderr),"timed_out":outcome.timed_out,"wall_ms":wall_ms})
                }
                Err(error) => {
                    failed = true;
                    let mut row = serde_json::json!({"definition":test.to_string(),"passed":false,"source":"executed","stopped":error.to_string(),"wall_ms":null});
                    if let crate::NativeStop::ForeignRecording { site, .. } = &error {
                        row["site"] = site.as_str().into();
                        if !json {
                            eprintln!("{site}: recording refusal site");
                            println!("fail executed {test}: {error}");
                        }
                    } else if !json {
                        println!("fail executed {test}: {error} (time unavailable)");
                    }
                    row
                }
            };
            rows.push(row);
        });
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
        let entry = selected_entry
            .as_ref()
            .ok_or("build/run requires an entry")?;
        let reached = reach.as_ref().unwrap();
        let grant_lines = command == "run" && !grants.foreign.is_empty();
        let assembled = if recipe_command || grant_lines {
            match product.publish_release(
                entry,
                reached,
                options,
                &code_store,
                n_place.unwrap_or(hirsel_core_ids::launch::DEFAULT_N_PLACE),
            ) {
                Ok(publication) => {
                    let root = publication.root;
                    if command == "publish" {
                        if let Some(path) = metrics {
                            meter.write_metrics(path)?;
                        }
                        if json {
                            println!("{}", serde_json::json!({ "recipe": hex(root) }));
                        } else {
                            println!("{}", hex(root));
                        }
                        std::process::exit(0);
                    }
                    let recipe = (release_manifest.is_some() || grant_lines)
                        .then(|| publication.recipe.clone());
                    let built =
                        publication.assemble_with(options, reached.counters, Some(&product), None);
                    if let (Ok(built), Some(path), Some(recipe)) =
                        (&built, &release_manifest, &recipe)
                    {
                        crate::release_manifest::write(
                            path,
                            &built.executable,
                            root,
                            recipe,
                            place_timeout,
                        )?;
                    }
                    if let (true, Ok(_), Some(recipe)) = (grant_lines, &built, &recipe) {
                        for line in crate::native::recipe_build::foreign_grant_lines(recipe, root) {
                            eprintln!("{line}");
                        }
                    }
                    built
                }
                Err(error) => Err(error),
            }
        } else {
            product.build_reachable(entry, reached, options)
        };
        match assembled {
            Ok(native) => Some(native),
            Err(crate::NativeStop::Output { product, error }) => {
                stopped = Some(crate::native::output_stop(&error));
                undelivered = Some(*product);
                None
            }
            Err(e) => {
                if let crate::NativeStop::ForeignRecording { site, .. } = &e {
                    eprintln!("{site}: recording refusal site");
                }
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
        && let Some(value) = crate::request_record::Value::of(crate::request_record::Answer {
            product: &product,
            diagnostics: &output.diagnostics,
            incomplete: &output.incomplete,
            resources: &output.resources,
            reach: reach.as_ref(),
            native: output.native.as_ref().or(undelivered.as_ref()),
        })
    {
        crate::request_record::publish(&store_root, key, &value)?;
        meter.finish("request record/publish");
    }
    finish(
        &output,
        command,
        json,
        &sources,
        &runner,
        run_args,
        record_path.as_deref(),
        &mut meter,
        metrics,
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
    record_path: Option<&str>,
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
        &output,
        command,
        json,
        sources,
        runner,
        run_args,
        record_path,
        meter,
        metrics,
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
    record_path: Option<&str>,
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
            if let Some(hint) = &d.hint {
                eprintln!("  {hint}");
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
            let mut cmd = runner.command(&path);
            cmd.args(run_args);
            if command == "run"
                && let Some(record) = record_path
            {
                cmd.env("HIRSEL_RECORD", record);
            }
            Some(
                cmd.status()
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
