//! One compiler request, from its arguments to its output and exit status:
//! the `hirsel` executable's whole behaviour after process setup. It lives in
//! the library so that a test process can run a request exactly as the
//! executable does (request_sequences.rs compares their outputs).
use crate::cli::{CompilerOption as Opt, ExplainOption, FmtOption, Invocation, NewOption, Request};
use crate::code_store::CodeStore;
use crate::native::recipe_build::Publication;
use crate::product::ProductLibraries;
use crate::{
    Backend, BuildProduct, CheckRequest, CoreImage, Driver, LibraryInput, Link, Meter,
    NativeOptions, operator_cli,
};
use hirsel_core_resolve::CorpusManifest;
use serde::Serialize;
use std::path::{Path, PathBuf};
mod flags;
mod lean;
mod release;
use flags::CompilerFlags;
pub(crate) use release::local_admission_limits;
#[derive(Serialize)]
struct Output<'a> {
    accepted: bool,
    root: String,
    files: usize,
    checked_definitions: usize,
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

fn run(args: Vec<String>) -> Result<Outcome, Box<dyn std::error::Error>> {
    let invocation = match crate::cli::parse(args.into_iter())? {
        crate::cli::Action::Help(help) => {
            print!("{help}");
            return conclude(Outcome::Exit(0));
        }
        crate::cli::Action::Version => {
            crate::installation::print_version()?;
            return conclude(Outcome::Exit(0));
        }
        crate::cli::Action::Invoke(invocation) => invocation,
    };
    match invocation {
        Invocation::Attach(request) => {
            conclude(crate::prof_command::attach(request).map(|()| Outcome::Exit(0))?)
        }
        Invocation::ProfileExport(request) => {
            conclude(crate::prof_command::export(request).map(|()| Outcome::Exit(0))?)
        }
        Invocation::ProfileQuery(request) => {
            conclude(crate::prof_command::query(request).map(|()| Outcome::Exit(0))?)
        }
        Invocation::Coverage(request) => conclude(crate::coverage::cli::run(request)?),
        Invocation::Lean(request) => conclude(lean::run(request).map(|()| Outcome::Exit(0))?),
        Invocation::Add(request) => conclude(Outcome::Exit(crate::package_add::run(request)?)),
        Invocation::Launcher(request) => match crate::launcher::run(request)? {},
        Invocation::Serve(request) => {
            if crate::resident::serving() {
                return Err("a resident compiler serves build and run requests only".into());
            }
            let serve = crate::resident::Serve::read(request)?;
            conclude(Outcome::Exit(crate::resident::run(serve)?))
        }
        Invocation::Host(request) => match operator_cli::host(request)? {},
        Invocation::Register(request) => conclude(Outcome::Exit(operator_cli::register(request)?)),
        Invocation::Deploy(request) => conclude(Outcome::Exit(operator_cli::deploy(request)?)),
        Invocation::Decide(request) => conclude(Outcome::Exit(operator_cli::decide(request)?)),
        Invocation::Call(request) => conclude(Outcome::Exit(crate::call::run(
            crate::call::Options::read(request)?,
        )?)),
        Invocation::Dev(request) => conclude(Outcome::Exit(launch::dev::run(request)?)),
        Invocation::Store(request) => launch::store_tool(request),
        Invocation::Sdk(request) => {
            crate::sdk_command::run(crate::sdk_command::Options::read(request)?)?;
            conclude(Outcome::Exit(0))
        }
        Invocation::Tape(request) => {
            crate::tape_cli::show(request)?;
            conclude(Outcome::Exit(0))
        }
        Invocation::Simple(request) if request.command.name == "manifest" => {
            crate::release_manifest::show(Path::new(
                request
                    .positional
                    .as_deref()
                    .ok_or("manifest show requires a path")?,
            ))?;
            conclude(Outcome::Exit(0))
        }
        Invocation::Simple(request) => {
            crate::store::remote::begin()?;
            if request.positional.is_some() {
                return Err("primer takes no arguments".into());
            }
            print!("{}", crate::cli_tools::PRIMER);
            conclude(Outcome::Exit(0))
        }
        Invocation::New(request) => {
            crate::store::remote::begin()?;
            let directory = request
                .positional
                .as_deref()
                .ok_or("new requires a package directory")?;
            let library = request.options.contains(&NewOption::Lib);
            crate::cli::scaffold(Path::new(directory), library)?;
            crate::fs::atomic_write(
                &Path::new(directory).join("hirsel.lock"),
                crate::package_lock::scaffold().as_bytes(),
            )?;
            crate::cli_tools::print_scaffold_summary(directory, library);
            conclude(Outcome::Exit(0))
        }
        Invocation::Fmt(request) => {
            use hirsel_core_syntax::{FormatAction, ParseMode};
            crate::store::remote::begin()?;
            let action = if request.options.contains(&FmtOption::FmtCheck) {
                FormatAction::Check
            } else {
                FormatAction::Write
            };
            let mode = if request.options.contains(&FmtOption::Signatures) {
                ParseMode::Signatures
            } else {
                ParseMode::Source
            };
            let stdin = request.options.contains(&FmtOption::FmtStdin);
            let status = match (stdin, request.positional.as_deref()) {
                (true, None) => crate::cli_tools::format_stdin(action, mode)?,
                (false, Some(path)) => crate::cli_tools::format(Path::new(path), action, mode)?,
                _ => return Err("fmt requires one of FILE, DIR, or --stdin".into()),
            };
            conclude(Outcome::Exit(status))
        }
        Invocation::Explain(request) => {
            crate::store::remote::begin()?;
            crate::cli_tools::explain(
                request.positional.as_deref(),
                request.options.contains(&ExplainOption::ListRules),
                request.options.contains(&ExplainOption::Json),
            )?;
            conclude(Outcome::Exit(0))
        }
        Invocation::Compiler(request) => run_compiler(request),
    }
}

fn run_compiler(mut invocation: Request<Opt>) -> Result<Outcome, Box<dyn std::error::Error>> {
    let profile = matches!(invocation.command.name, "prof run" | "prof replay")
        .then(|| crate::prof_command::Run::read(&mut invocation))
        .transpose()?;
    let command = launch::fleet_cli::prepare(&mut invocation)?;
    if command == "replay"
        && invocation
            .options
            .iter()
            .any(|option| matches!(option, Opt::HostState(_)))
    {
        return conclude(Outcome::Exit(operator_cli::replay(invocation)?));
    }
    crate::store::remote::begin()?;
    let mut package_mode = crate::package_lock::Mode::default();
    let mut package_store = None;
    let mut pack_out = None;
    for option in invocation.options.extract_if(.., |option| {
        matches!(option, Opt::Locked | Opt::Frozen | Opt::PackageStore(_))
            || (command == "pack" && matches!(option, Opt::Output(_)))
    }) {
        match option {
            Opt::Locked if package_mode != crate::package_lock::Mode::Frozen => {
                package_mode = crate::package_lock::Mode::Locked
            }
            Opt::Frozen => package_mode = crate::package_lock::Mode::Frozen,
            Opt::PackageStore(value) => package_store = Some(PathBuf::from(value)),
            Opt::Output(value) => pack_out = Some(PathBuf::from(value)),
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
            let out = pack_out.ok_or("pack requires --output FILE")?;
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
        return conclude(Outcome::Exit(0));
    }
    let replay_command = command == "replay";
    let inspection = (command == "inspect")
        .then(|| crate::inspect::Inspection::read(&mut invocation))
        .transpose()?;
    let mut inspect_library = inspection.as_ref().and_then(|option| option.source_library);
    let link_only = crate::test_run::link_only_requested(&invocation)?;
    let inspect_foreign = inspection
        .as_ref()
        .and_then(crate::inspect::Inspection::foreign_module);
    let dir = invocation
        .positional
        .map(PathBuf::from)
        .or_else(|| inspect_foreign.map(|_| PathBuf::from(".")));
    let test_command = command == "test";
    let static_laws = matches!(command, "laws check" | "laws write");
    let recipe_command = matches!(command, "build" | "publish");
    let native_command = recipe_command || command == "run" || replay_command || test_command;
    let mut run_args = invocation.arguments;
    // Link with --cc, the installed recipe, or the pinned LLVM clang.
    // Never use a cc found on PATH.
    let pinned =
        || std::env::var_os("LLVM_SYS_231_PREFIX").map(|p| PathBuf::from(p).join("bin/clang"));
    let CompilerFlags {
        libraries,
        store,
        manifest,
        metrics,
        profile_front,
        image_path,
        install_root,
        link_kit,
        mut core_product,
        mut source_core,
        source_std,
        product_name,
        entry,
        recipe_root,
        code_store,
        release_manifest,
        n_place,
        place_timeout,
        test_filter,
        test_exact,
        test_jobs,
        test_timeout,
        mut test_sim,
        corpus_path,
        test_seeds,
        test_seed,
        backend,
        release,
        debug_lines,
        mut runtime,
        explicit_runtime,
        explicit_target,
        mut logical_charge_report,
        output_path,
        mut root_limit,
        emulator,
        sysroot,
        mut target,
        cc,
        recording_build,
        mut additional_runtimes,
        mut record_path,
        replay_outputs,
        replay_tape,
        test_replay,
        mut test_laws,
        test_write_laws,
        law_baseline,
        test_rerun,
        test_cache,
        coverage,
        seed_base,
        expect_verdict,
        diagnose,
        mut grants,
        json,
    } = CompilerFlags::read(
        invocation.options,
        inspect_library.is_some(),
        crate::TestCache::default_root(),
    )?;
    use crate::installation::{Flavour, Observation};
    let law_baseline = law_baseline
        .as_deref()
        .map(crate::law_lock::Baseline::read)
        .transpose()?;
    test_laws |= test_write_laws || law_baseline.is_some();
    let test_coverage = coverage.on();
    if test_write_laws && manifest.is_some() {
        return Err("test: --write-laws requires a package directory".into());
    }
    if expect_verdict.is_some() && !test_replay {
        return Err("--expect-verdict requires --replay".into());
    }
    if corpus_path.is_some() && (test_replay || test_seeds.is_some()) {
        return Err("--property-corpus requires ordinary tests or a single simulation seed".into());
    }
    test_sim |= test_seed.is_some();
    if !test_sim && test_seeds.is_some() {
        return Err("--seeds requires --sim".into());
    }
    if test_seed.is_some() && test_seeds.is_some() {
        return Err("choose one of --seed and --seeds".into());
    }
    if seed_base.is_some() && test_seeds.is_none() {
        return Err("--seed-base requires --seeds".into());
    }
    let seed_base = seed_base.unwrap_or(0);
    if seed_base.checked_add(test_seeds.unwrap_or(1) - 1).is_none() {
        return Err("--seed-base and --seeds run past the last u64 seed".into());
    }
    if test_sim && test_replay {
        return Err("choose one of --sim and --replay".into());
    }
    let archive = dir.as_deref().filter(|path| {
        inspection.is_some() && path.extension().is_some_and(|ext| ext == "hpkg") && !path.is_dir()
    });
    let dependency = dir
        .as_deref()
        .filter(|_| inspection.is_some())
        .and_then(Path::to_str)
        .filter(|name| crate::package_dir::package_name(name) && !Path::new(name).exists());
    let app_directory = if let Some(archive) = archive {
        archive
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_owned)
    } else if dependency.is_some() {
        Some(PathBuf::from("."))
    } else {
        dir.clone()
    }
    .or_else(|| {
        (manifest.is_none()
            && recipe_root.is_none()
            && (static_laws || matches!(command, "check" | "build" | "publish" | "run" | "test")))
        .then(|| PathBuf::from("."))
    });
    let mut workspace_app = None;
    let mut foreign_symbols = None;
    if manifest.is_some() {
        package_mode.require_workspace("--inputs")?;
    } else if recipe_root.is_some() {
        package_mode.require_workspace("--recipe")?;
    } else if let Some(name) = inspect_library {
        package_mode.require_workspace(if name == "core" { "--core" } else { "--std" })?;
    } else if let Some(path) = archive {
        package_mode.require_workspace("an archive path")?;
        let archive = crate::package_content::PackageArchive::open(path)?;
        if let Some((module, _)) = inspect_foreign {
            foreign_symbols = Some(crate::foreign_header::symbol_prefix(
                archive.entries.values().map(|library| &library.content),
                module,
            )?);
        }
        workspace_app = Some(crate::package_dir::PackageDir::from_archive(&archive)?);
    } else if static_laws
        || matches!(
            command,
            "check" | "build" | "publish" | "run" | "test" | "inspect" | "replay"
        )
    {
        if let Some(directory) = &app_directory {
            if let Some(workspace) = crate::package_workspace::Workspace::discover(directory)? {
                let member = workspace.selected()?;
                let selected = if let Some(name) = dependency {
                    workspace.members[member]
                        .edges
                        .keys()
                        .find(|id| id.name == name)
                        .ok_or_else(|| format!("{member} has no dependency named {name}"))?
                } else {
                    member
                }
                .clone();
                let resolved = crate::package_resolve::Resolution::resolve(
                    workspace,
                    package_store,
                    package_mode == crate::package_lock::Mode::Frozen,
                )?;
                if let Some((path, _)) = inspect_foreign {
                    foreign_symbols = Some(crate::foreign_header::symbol_prefix(
                        resolved
                            .workspace
                            .members
                            .values()
                            .map(|member| &member.content)
                            .chain(resolved.libraries.values().map(|library| &library.content)),
                        path,
                    )?);
                }
                if !static_laws || package_mode.flag().is_some() {
                    crate::package_lock::Lock::compare(&resolved, package_mode)?
                        .commit(&resolved.workspace)?;
                }
                let include_tests = static_laws
                    || matches!(command, "check" | "test")
                    || inspection
                        .as_ref()
                        .is_some_and(|option| option.includes_tests());
                if dependency == Some("std") {
                    inspect_library = Some("std");
                    source_core = true;
                } else {
                    workspace_app = Some(crate::package_dir::PackageDir::from_resolution(
                        &resolved,
                        &selected,
                        include_tests,
                    )?);
                }
            } else {
                if let Some(name) = dependency {
                    return Err(format!("cannot resolve dependency {name} without a package manifest in the current directory").into());
                }
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
    let needs_recording =
        recording_build || record_path.is_some() || replay_command || test_replay || test_laws;
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
            || root_limit.is_some()
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
        target = flags::admit_target(publication.recipe().launch.target.clone())?;
        Some(publication)
    } else {
        None
    };
    let dir = app_directory;
    if profile.is_some() && (emulator.is_some() || sysroot.is_some()) {
        return Err("profiling requires a native executable and cannot use an emulator".into());
    }
    let runner = match (emulator, sysroot) {
        (None, None) => crate::Runner::Native,
        (Some(emulator), Some(sysroot)) => crate::Runner::qemu(&emulator, &sysroot, &target)?,
        _ => return Err("--emulator and --sysroot must be supplied together".into()),
    };
    if test_command && entry.is_some() {
        return Err("test selects marked functions with --filter or --exact, not --entry".into());
    }
    let executable = crate::installation::compiler_executable()?;
    let installed = crate::installation::Installed::discover(&executable)?;
    let observe = profile
        .as_ref()
        .is_some_and(crate::prof_command::Run::observes);
    let charge_observation = logical_charge_report.is_some();
    let flavour = Flavour {
        observation: if observe {
            Observation::Prof
        } else if charge_observation {
            Observation::Observed
        } else {
            Observation::None
        },
        record: needs_recording,
        embedded: false,
    };
    let env_recording_runtime = installed
        .as_ref()
        .and_then(|installed| installed.runtime(&target, Flavour::RECORDING));
    let source_tree = libraries.is_some();
    let source_libraries = source_core || source_std || source_tree;
    if let Some(installed) = &installed {
        if core_product.is_none() && !source_tree {
            core_product = Some(installed.cache());
        }
        // A combined archive can only be supplied explicitly.
        if runtime.is_none() && !(observe && charge_observation) {
            runtime = installed.runtime(&target, flavour);
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
            crate::installation::print_environment(&report);
        }
        return conclude(Outcome::Exit(0));
    }
    if install_root.is_some() && (store.is_some() || image_path.is_some()) {
        return Err("--install cannot be combined with --store or --image".into());
    }
    if install_root.is_none()
        && ((command == "core-product" && (explicit_runtime || explicit_target))
            || (command == "std-product" && explicit_runtime)
            || !additional_runtimes.is_empty())
    {
        return Err("product --runtime, core-product --target and runtime variant options require --install".into());
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
    let installed_std = if !source_libraries
        && image_path.is_none()
        && workspace_app.as_ref().is_none_or(|app| app.needs_std())
        && let Some(installed) = &installed
    {
        installed.product("std")?
    } else {
        None
    };
    if image_path.is_some() {
        meter.result.image_selection = Some("image used: explicit image".into());
    } else if installed_std.is_some() {
        meter.result.image_selection = Some("image used: installed std product".into());
    } else if source_libraries {
        meter.result.image_selection = Some("source checked: explicit library source".into());
    } else if installed.is_some() {
        meter.result.image_selection = Some("source checked: no registered std product".into());
    } else {
        meter.result.image_selection = Some("source checked: no installed image".into());
    }
    // A checkout supplies sources when requested. Default native builds
    // use the bundled libraries even when invoked from the checkout.
    let libraries = libraries.as_deref().or_else(|| {
        ((source_libraries || installed.is_none()) && Path::new("core/std").is_dir())
            .then_some(Path::new("core"))
    });
    let directory = dir.as_deref().unwrap_or(Path::new("."));
    let store = store
        .or_else(|| install_root.as_ref().map(|root| root.join("core-packages")))
        .unwrap_or_else(|| crate::store::default_path(directory));
    let code_store = CodeStore::for_request(command, code_store, &store)?;
    let select_link = |cc: Option<PathBuf>| -> Result<Link, Box<dyn std::error::Error>> {
        match (cc, &installed) {
            (Some(cc), _) => Ok(Link::Driver(crate::link_driver(&cc)?)),
            (None, Some(installed)) => Ok(Link::Installed(installed.link(&target).ok_or_else(
                || format!("this installation has no {target} runtime; install one with std-product --install --runtime --target {target} --link-kit, or pass --cc"),
            )?)),
            (None, None) => Ok(Link::Driver(crate::link_driver(&pinned().ok_or(
                "no link driver: this compiler is not installed and LLVM_SYS_231_PREFIX is unset; pass --cc",
            )?)?)),
        }
    };
    let select_runtime = || -> Result<PathBuf, Box<dyn std::error::Error>> {
        let archive = runtime.as_deref().ok_or_else(|| {
            if observe && charge_observation {
                format!("no combined profiling and charge observation runtime archive for {target}; pass --runtime with a combined archive")
            } else if charge_observation {
                let mode = if needs_recording { " with recording" } else { "" };
                format!("no charge observation runtime archive{mode} for {target}; pass --runtime with a charge observation archive{mode}")
            } else if observe {
                format!("no profiling runtime archive for {target}; pass --runtime with a profiling archive")
            } else if needs_recording {
                format!("no recording runtime archive for {target}; pass --runtime with a recording archive")
            } else {
                "build/run/test requires --runtime <core runtime archive>".into()
            }
        })?;
        if explicit_runtime {
            let bytes = crate::installation::read_runtime_archive(archive)?;
            crate::installation::check_runtime(archive, &bytes, needs_recording)?;
            if observe
                && !crate::installation::archive_defines_symbol(&bytes, b"hirsel_core_prof_runtime")
            {
                return Err(format!(
                    "this profile requires a profiling runtime archive: {}",
                    archive.display()
                )
                .into());
            }
            if charge_observation
                && !crate::installation::archive_defines_symbol(
                    &bytes,
                    b"hirsel_core_observe_charge",
                )
            {
                return Err(format!(
                    "--logical-charge-report requires a charge observation runtime archive: {}",
                    archive.display()
                )
                .into());
            }
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
        let library = installed_std
            .as_ref()
            .map(|image| image.store_root().to_owned());
        let product =
            publication.assemble_with(&options, Default::default(), library.as_deref())?;
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
        return conclude(Outcome::Exit(0));
    }
    if command == "core-product" || command == "std-product" {
        if link_kit.is_some() && (install_root.is_none() || runtime.is_none()) {
            return Err("--link-kit requires --install and --runtime".into());
        }
        meter.finish("startup/discovery");
        let (image, rebuilt) = if let Some(path) = image_path {
            (
                CoreImage::open(&path)?.ok_or("core image cache miss; rebuild the image")?,
                false,
            )
        } else {
            let package = if let Some(root) = &install_root {
                if let Some(path) = runtime {
                    additional_runtimes.insert(Flavour::PLAIN, path);
                }
                crate::installation::publish(
                    root,
                    command == "std-product",
                    additional_runtimes
                        .iter()
                        .map(|(flavour, path)| (*flavour, (path.as_path(), target.as_str())))
                        .collect(),
                    link_kit.as_deref(),
                    &mut meter,
                )?
            } else if command == "std-product" {
                crate::PackageProduct::bundled_std(&store, &mut meter)?
            } else {
                crate::PackageProduct::bundled(&store, &mut meter)?
            };
            (package.image, package.rebuilt)
        };
        // An explicit target asks for the image's prebuilt release std, which
        // an install prebuilds for its own target.
        if command == "std-product" && explicit_target && install_root.is_none() {
            crate::installation::prebuild_release(&image, &target, &mut meter)?;
        }
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
            serde_json::json!({"image":image.identity.to_string(),"rebuilt":rebuilt,"name":name,"index_pages":image.checked_index_pages(),"index_rows":image.decoded_index_rows()})
        );
        drop(image);
        meter.finish("package handle teardown");
        if let Some(path) = metrics {
            meter.write_metrics(path)?;
        }
        return conclude(Outcome::Exit(0));
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
    let (mut test_owner, mut package_root) = (None, None);
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
    } else if inspect_library.is_some() {
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
        (test_owner, package_root) = (Some(app.package.package.clone()), app.manifest.clone());
        manifest_runs = app.tests.take();
        app.program_grants(command, &mut grants, &mut root_limit)?;
        if let Some(root) = libraries {
            app.trust_library_root(root);
        }
        let program =
            native_command || installed.is_some() || inspection.is_some() || image.is_some();
        let libraries = if source_libraries || !program {
            libraries
        } else {
            None
        };
        CheckRequest::from_app(app, libraries, &store)?
    };
    if entry.is_some() {
        request.entry = entry;
    }
    if let Some((module, print)) = inspect_foreign {
        let manifested =
            foreign_symbols.ok_or("inspect --foreign-header requires a foreign manifest")?;
        let text = crate::foreign_header::header(request.inputs, module, &manifested, print)?;
        print!("{text}");
        return conclude(Outcome::Exit(0));
    }
    // PROG-4: each test's run configuration, from the package's manifest
    // when it declares one, else from the command line.
    let test_runs = if test_command || static_laws {
        request.entry = None;
        Some(match manifest_runs {
            Some(runs) => {
                let mut configured = grants.clone();
                configured.foreign.clear();
                configured.exec.clear();
                if root_limit.is_some() || !configured.is_empty() {
                    return Err("this package declares [tests] in hirsel.toml. Set limit, dirs and grants there".into());
                }
                runs
            }
            None => {
                if test_command && root_limit.is_none() {
                    return Err("test requires an explicit --limit <bytes> for each run".into());
                }
                // A test runs in a working directory of its own; a directory
                // the command line names is the invoker's.
                for directory in &mut grants.directories {
                    if hirsel_core_ids::launch::xdg_slot(directory.as_bytes()).is_some() {
                        continue;
                    }
                    *directory = std::path::absolute(&*directory)?
                        .to_str()
                        .ok_or("directory path is not UTF-8")?
                        .to_owned();
                }
                crate::package_dir::TestRuns::given(
                    root_limit.unwrap_or(crate::native::DEFAULT_ROOT_LIMIT),
                    grants.clone(),
                )
            }
        })
    } else {
        None
    };
    if command == "run" {
        run_args.splice(
            ..0,
            grants
                .exec
                .iter()
                .map(crate::exec_launch::Binding::argument),
        );
    }
    if test_command && let Some(path) = &mut logical_charge_report {
        *path = std::path::absolute(&*path)?;
    }
    let selected_entry = request.entry.clone();
    request.capture_sources()?;
    let law_files = (static_laws || test_write_laws).then(|| crate::law_lock::test_files(&request));
    let law_directory = &package_root.or_else(|| dir.clone()).unwrap_or_default();
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
            if !request.use_image(image)? {
                return Err("installed std product does not match the bundled libraries".into());
            }
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
    // The request's store keeps scratch; executables and request records live
    // in the content store, and the old flat `native` store is removed.
    let request_store = crate::Store::new(&request.store)?;
    request_store.retire(&request.store.join("native"))?;
    let foreign_replay = replay_command
        && request
            .inputs
            .packages
            .iter()
            .any(|package| package.sources.iter().any(|s| s.foreign.is_some()));
    if let Some(replay) = &replay_plan
        && !foreign_replay
    {
        let header = crate::tape_cli::header(Path::new(&replay.path))?;
        root_limit.get_or_insert(header.limit);
    }
    meter.finish("startup/discovery");
    // Concurrent requests share keyed artifacts. Their launch paths are private.
    let temporary_output = (matches!(command, "run" | "test") && output_path.is_none())
        .then(|| request_store.scratch())
        .transpose()?;
    // Native options come before the reach: a Cranelift development build
    // walks its instance records, which its options address. An options
    // error is reported only for a request check accepts.
    let mut prepared = if native_command {
        let prepare = || -> Result<_, Box<dyn std::error::Error>> {
            if release && (recipe_command || command == "run") && root_limit.is_none() {
                return Err(crate::code_admission::AdmissionError::MissingRootLimit.into());
            }
            let default_output = temporary_output.as_ref().map_or_else(
                || PathBuf::from("program"),
                |scratch| scratch.path().join("program"),
            );
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
                output_path.unwrap_or(default_output),
            );
            if release {
                options.backend = Backend::Llvm;
                options.optimize = true;
            }
            options.backend = backend.unwrap_or(options.backend);
            options.debug_lines = debug_lines.unwrap_or(options.debug_lines);
            options.root_limit = root_limit.unwrap_or(options.root_limit);
            grants.apply(&mut options);
            if !test_command {
                options.bind_exec()?;
            }
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
            || static_laws
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
            if !source_core
                && !(source_tree && source_std)
                && (native_command || core_product.is_some())
            {
                let root = core_product.unwrap_or_else(crate::PackageProduct::default_cache);
                let libraries = if source_tree {
                    ProductLibraries::SourceTree
                } else if source_std {
                    ProductLibraries::Core
                } else {
                    ProductLibraries::BundledStd
                };
                driver.check_package_product(
                    request,
                    &root,
                    &mut meter,
                    (native_command, libraries),
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
            let native = prepared.as_ref().and_then(|p| p.as_ref().ok());
            return answer_hit(
                &hit,
                command,
                json,
                &sources,
                native.map(|n| n.output.as_path()),
                &runner,
                run_args,
                record_path.as_deref(),
                replay_outputs.as_deref(),
                profile.as_ref(),
                temporary_output,
                &mut meter,
                metrics,
            );
        }
    };
    let grant_lines = command == "run" && !grants.foreign.is_empty();
    let use_recipe = recipe_command
        || grant_lines
        || matches!(&prepared, Some(Ok(options)) if options.llvm_release());
    let mut reach = None;
    if foreign_replay {
        let entry = selected_entry.as_ref().ok_or("replay requires an entry")?;
        let reached = match &prepared {
            Some(Ok(options)) if use_recipe => product.reachable_recipe(entry, options)?,
            Some(Ok(options)) => product.reachable(entry, options.instance_limit)?,
            _ => product.reachable(entry, crate::native::DEFAULT_INSTANCE_LIMIT)?,
        };
        if let Some(crate::NativeStop::ForeignRecording { reason, site }) =
            reached.recording_stop(&product, entry)?
        {
            eprintln!("{site}: recording refusal site");
            eprintln!("replay: refused: {reason}");
            if let Some(profile) = &profile {
                profile.cleanup();
            }
            return conclude(Outcome::Exit(2));
        }
        if use_recipe {
            reach = Some(reached);
        }
        if let Some(Ok(options)) = &mut prepared {
            let header =
                crate::tape_cli::header(Path::new(&options.replay.as_ref().unwrap().path))?;
            if root_limit.is_none() {
                options.root_limit = header.limit;
            }
        }
    }
    if reach.is_none() && native_command && !test_command {
        let entry = selected_entry
            .as_ref()
            .ok_or("build/run requires an entry")?;
        reach = Some(match &prepared {
            Some(Ok(options)) if use_recipe => product.reachable_recipe(entry, options)?,
            Some(Ok(options)) => product.reachable_objects(entry, options)?,
            _ => product.reachable(entry, crate::native::DEFAULT_INSTANCE_LIMIT)?,
        });
    }
    let accepted = reach
        .as_ref()
        .map_or_else(|| product.accepted(), |r| r.accepted());
    if static_laws && accepted {
        let tests = product
            .tests()
            .filter(|path| {
                test_owner
                    .as_ref()
                    .is_none_or(|owner| &path.0.package == owner)
            })
            .cloned()
            .collect::<Vec<_>>();
        let runs = test_runs.as_ref().unwrap();
        runs.check_names(&tests.iter().collect())?;
        let current =
            crate::law_lock::Inventory::of(&product, &tests, law_files.as_ref().unwrap(), runs)?;
        if command == "laws write" {
            crate::law_lock::update(law_directory, |lock| lock.refresh(current))?;
            return conclude(Outcome::Exit(0));
        }
        let differences = crate::law_lock::read(law_directory)?.differences(&current);
        for difference in &differences {
            println!("{difference}");
        }
        return conclude(Outcome::Exit(i32::from(!differences.is_empty())));
    }
    if accepted && let Some(inspection) = inspection {
        inspection.write(&product, selected_entry.as_ref(), json)?;
        return conclude(Outcome::Exit(0));
    }
    let native_options = if native_command && accepted {
        let mut options = prepared.expect("a native command prepares its options")?;
        if test_coverage {
            options.coverage = Some(crate::test_run::Session::scope(&product));
        }
        if options.logical_charge_report.is_some() {
            eprintln!(
                "observation build: root peak reservation includes bound, task and grant reservations (MEM-19); timings are not performance samples"
            );
        }
        Some(options)
    } else {
        None
    };
    if test_command && accepted {
        let runs = test_runs.expect("a test request has its run configuration");
        let tests = crate::test_run::select_tests(
            &product,
            &runs,
            test_owner.as_ref(),
            test_filter.as_deref(),
            &test_exact,
        )?;
        let options = native_options.expect("a test request has native options");
        if link_only {
            return conclude(Outcome::Exit(crate::test_run::link_only(
                &product, &options, &runs, &tests, json,
            )));
        }
        let law_inventory = law_files
            .as_ref()
            .map(|files| crate::law_lock::Inventory::of(&product, &tests, files, &runs))
            .transpose()?;
        let corpus = corpus_path
            .as_deref()
            .map(crate::property_corpus::Corpus::open)
            .transpose()?;
        let session = crate::test_run::TestSession::prepare(crate::test_run::Prepare {
            options,
            runs,
            tests,
            law_inventory,
            law_directory: law_directory.clone(),
            law_baseline,
            // The server reads the pass cache; the process that runs a test writes it.
            cache: (corpus.is_none()
                && !test_sim
                && !test_replay
                && matches!(runner, crate::Runner::Native))
            .then(|| crate::TestCache::open(&test_cache))
            .flatten(),
            corpus,
            sim: test_sim,
            replay: test_replay,
            laws: test_laws,
            write_laws: test_write_laws,
            rerun: test_rerun || test_write_laws,
            coverage: test_coverage.then_some(coverage),
            jobs: test_jobs.map_or_else(crate::test_run::affinity_jobs, Ok)?,
            timeout: test_timeout,
            seeds: test_seeds.unwrap_or(1),
            seed: test_seed,
            seed_base,
            plain_runtime: installed.as_ref().and_then(|installed| {
                let flavour = if charge_observation {
                    Flavour::OBSERVED
                } else {
                    Flavour::PLAIN
                };
                installed.runtime(&target, flavour)
            }),
            runner,
            expect_verdict,
            json,
            meter,
            metrics,
            output: temporary_output,
            product,
        });
        return conclude(Outcome::Test(Box::new(session)));
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
        let reached = reach.as_mut().unwrap();
        let assembled = if use_recipe {
            let local_code = (!recipe_command && !grant_lines)
                .then(|| CodeStore::cache(&options.cas, "code"))
                .transpose()?;
            match product.publish_release(
                entry,
                reached,
                options,
                local_code.as_ref().unwrap_or(&code_store),
                n_place.unwrap_or(hirsel_core_ids::launch::DEFAULT_N_PLACE),
            ) {
                Ok(publication) => {
                    if command == "build" {
                        crate::native::recipe_grants::warn_ungranted(&publication.ungranted);
                    }
                    let root = publication.root;
                    if command == "publish" {
                        if let Some(path) = metrics {
                            meter.write_metrics(path)?;
                        }
                        if json {
                            println!("{}", serde_json::json!({ "recipe": root.to_string() }));
                        } else {
                            println!("{root}");
                        }
                        return conclude(Outcome::Exit(0));
                    }
                    if options.llvm_release() {
                        let mut report = release::Report::new(&product, reached, key, json);
                        let counters = reached.counters;
                        let library = product.image.as_ref().map(|image| image.store_root());
                        drop(reach);
                        drop(product);
                        let output = report.assemble(
                            publication,
                            options,
                            counters,
                            library.as_deref(),
                            release_manifest.as_deref(),
                            grant_lines,
                            place_timeout,
                            &mut meter,
                        )?;
                        return finish(
                            &output,
                            command,
                            json,
                            &sources,
                            &runner,
                            run_args,
                            record_path.as_deref(),
                            replay_outputs.as_deref(),
                            profile.as_ref(),
                            temporary_output,
                            &mut meter,
                            metrics,
                        );
                    }
                    let recipe = (release_manifest.is_some() || grant_lines)
                        .then(|| publication.recipe.clone());
                    let library = product.image.as_ref().map(|image| image.store_root());
                    let built =
                        publication.assemble_with(options, reached.finish(), library.as_deref());
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
            product.build_objects(entry, reached, options)
        };
        match assembled {
            Ok(native) => Some(native),
            Err(crate::NativeStop::Output { product, error }) => {
                stopped = Some(crate::executable::output_stop(&error));
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
            product.root.to_string()
        } else {
            String::new()
        },
        files: product.files.len(),
        checked_definitions: product.checked_definitions(),
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
        image: image.map(|i| i.identity.to_string()),
        native,
        stopped: stopped.or_else(|| {
            reach
                .as_ref()
                .and_then(|r| r.instances.as_ref().err())
                .filter(|e| !matches!(e, crate::InstanceStop::Resource { .. }))
                .map(|e| match e {
                    // A PROG-6 refusal is its own line (`target.unsupported`).
                    crate::InstanceStop::Unsupported(_) => e.to_string(),
                    _ => format!("instances: {e}"),
                })
        }),
        reachable: reach.as_ref().map(|r| ReachableOutput {
            bodies: r.bodies,
            declarations: r.declarations,
            census: r.census(),
            first_stops: &r.first_stops,
        }),
    };
    if command == "build"
        && let Some(build) = output.native.as_ref().or(undelivered.as_ref())
    {
        meter.finish(
            if build.compiled == 0 && !build.linked && build.reach.std_lowered == 0 {
                "build store/hit"
            } else {
                "build store/miss"
            },
        );
    }
    // A request that stopped is never recorded, except for the output
    // write after a stored build.
    if let Some(key) = key
        && let Some(record) = &record
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
        crate::request_record::publish(record.cas(), key, &value)?;
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
        replay_outputs.as_deref(),
        profile.as_ref(),
        temporary_output,
        &mut meter,
        metrics,
    )
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
    replay_outputs: Option<&str>,
    profile: Option<&crate::prof_command::Run>,
    run_output: Option<crate::store::Scratch>,
    meter: &mut Meter,
    metrics: Option<PathBuf>,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    let rendered = hit.render();
    let mut stopped = None;
    let native = match (output_path, &hit.executable) {
        (Some(path), Some(binary)) => {
            let product = hit.build(path).expect("a stored executable has its build");
            match binary.deliver(path) {
                Ok(()) => Some(product),
                Err(error) => {
                    stopped = Some(crate::executable::output_stop(&error));
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
            rendered.root.to_string()
        } else {
            String::new()
        },
        files: rendered.files,
        checked_definitions: rendered.checked_definitions,
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
        replay_outputs,
        profile,
        run_output,
        meter,
        metrics,
    )
}
/// What a request leaves for its caller: an exit status, the program it
/// built for `run` or `replay`, which the caller runs and whose status is the
/// request's, or a `test` request's session, whose launches the caller runs.
/// A resident compiler's client runs the program and the tests itself, so
/// they have the client's terminal, signals, directory and environment.
pub enum Outcome {
    Exit(i32),
    Launch(Launch),
    Test(Box<crate::TestSession>),
}
#[path = "request_launch.rs"]
mod launch;
pub use launch::Launch;
use launch::{conclude, finish};
pub(crate) use launch::{report_request_error, serve};

/// Run the request `args` names (the arguments after the program name) and
/// exit with its status, through a resident compiler when one serves the
/// request's store (`crate::resident`). Process setup (allocator guard,
/// signal dispositions, inherited descriptors) is the caller's.
pub fn main(args: Vec<String>) -> ! {
    if let Some(code) = crate::resident::submit(&args) {
        std::process::exit(code);
    }
    let error = match run(args) {
        Ok(outcome) => match conclude(outcome) {
            Ok(_) => unreachable!("a one-shot request exits in conclude"),
            Err(e) => e,
        },
        Err(e) => e,
    };
    std::process::exit(report_request_error(error.as_ref()))
}

#[cfg(test)]
mod net_grant_tests;
