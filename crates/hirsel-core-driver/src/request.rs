//! One compiler request, from its arguments to its output and exit status:
//! the `hirsel` executable's whole behaviour after process setup. It lives in
//! the library so that a test process can run a request exactly as the
//! executable does (request_sequences.rs compares their outputs).
use crate::code_store::CodeStore;
use crate::native::recipe_build::Publication;
use crate::product::ProductLibraries;
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
fn run(args: Vec<String>) -> Result<Outcome, Box<dyn std::error::Error>> {
    let mut invocation = match crate::cli::parse(args.into_iter())? {
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
    match invocation.command.name {
        "prof attach" => {
            crate::prof_command::attach(invocation)?;
            return conclude(Outcome::Exit(0));
        }
        "prof export" => {
            crate::prof_command::export(invocation)?;
            return conclude(Outcome::Exit(0));
        }
        name if name.starts_with("prof ") && !matches!(name, "prof run" | "prof replay") => {
            crate::prof_command::query(invocation)?;
            return conclude(Outcome::Exit(0));
        }
        _ => {}
    }
    let profile = matches!(invocation.command.name, "prof run" | "prof replay")
        .then(|| crate::prof_command::Run::read(&mut invocation))
        .transpose()?;
    let command = invocation.command.name;
    if command == "launcher" {
        match crate::launcher::run(invocation)? {}
    }
    if command == "serve" {
        if crate::resident::serving() {
            return Err("a resident compiler serves build and run requests only".into());
        }
        let serve = crate::resident::Serve::read(invocation)?;
        return conclude(Outcome::Exit(crate::resident::run(serve)?));
    }
    if command == "host" {
        match crate::operator_cli::host(invocation)? {}
    }
    if command == "register" {
        return conclude(Outcome::Exit(crate::operator_cli::register(invocation)?));
    }
    if command == "deploy" {
        return conclude(Outcome::Exit(crate::operator_cli::deploy(invocation)?));
    }
    if command == "replay"
        && invocation
            .options
            .iter()
            .any(|(option, _)| *option == Opt::HostState)
    {
        return conclude(Outcome::Exit(crate::operator_cli::replay(invocation)?));
    }
    if command == "call" {
        return conclude(Outcome::Exit(crate::call::run(
            crate::call::Options::read(invocation)?,
        )?));
    }
    if command == "sdk" {
        crate::sdk_command::run(crate::sdk_command::Options::read(invocation)?)?;
        return conclude(Outcome::Exit(0));
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
        return conclude(Outcome::Exit(0));
    }
    if command == "manifest" {
        crate::release_manifest::show(Path::new(
            invocation
                .positional
                .as_deref()
                .ok_or("manifest show requires a path")?,
        ))?;
        return conclude(Outcome::Exit(0));
    }
    let mut package_mode = crate::package_lock::Mode::default();
    let mut package_store = None;
    let mut pack_out = None;
    for (option, value) in invocation.options.extract_if(.., |(option, _)| {
        matches!(option, Opt::Locked | Opt::Frozen | Opt::PackageStore)
            || (command == "pack" && *option == Opt::Output)
    }) {
        match option {
            Opt::Locked if package_mode != crate::package_lock::Mode::Frozen => {
                package_mode = crate::package_lock::Mode::Locked
            }
            Opt::Frozen => package_mode = crate::package_lock::Mode::Frozen,
            Opt::PackageStore => package_store = value.map(PathBuf::from),
            Opt::Output => pack_out = value.map(PathBuf::from),
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
        return conclude(Outcome::Exit(0));
    }
    if command == "primer" {
        if invocation.positional.is_some() {
            return Err("primer takes no arguments".into());
        }
        print!("{}", crate::cli_tools::PRIMER);
        return conclude(Outcome::Exit(0));
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
            return conclude(Outcome::Exit(status));
        }
        crate::cli_tools::explain(
            invocation.positional.as_deref(),
            has(OptionId::ListRules),
            has(OptionId::Json),
        )?;
        return conclude(Outcome::Exit(0));
    }
    let inspection = (command == "inspect")
        .then(|| crate::inspect::Inspection::read(&mut invocation))
        .transpose()?;
    let mut inspect_library = inspection.as_ref().and_then(|option| option.source_library);
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
    let mut link_kit = None;
    let mut core_product = None;
    let mut source_core = inspect_library.is_some();
    let mut source_std = false;
    let mut product_name = None;
    let mut entry = None;
    let test_command = command == "test";
    let static_laws = matches!(command, "laws check" | "laws write");
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
    let mut test_sim = false;
    let mut corpus_path = None;
    let mut test_seeds = None;
    let mut test_seed = None;
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
    let mut run_args = invocation.arguments;
    let mut recording_build = false;
    let mut additional_runtimes = crate::installation::RuntimeVariants::<PathBuf>::default();
    let mut record_path = None;
    let mut replay_tape = None;
    let mut test_replay = false;
    let mut test_laws = false;
    let mut test_write_laws = false;
    let mut law_baseline = None;
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
            Opt::RecordingRuntime => additional_runtimes.recording = Some(PathBuf::from(value)),
            Opt::ProfRuntime => additional_runtimes.prof = Some(PathBuf::from(value)),
            Opt::ProfRecordingRuntime => {
                additional_runtimes.prof_recording = Some(PathBuf::from(value))
            }
            Opt::ObservedRuntime => additional_runtimes.observed = Some(PathBuf::from(value)),
            Opt::ObservedRecordingRuntime => {
                additional_runtimes.observed_recording = Some(PathBuf::from(value))
            }
            Opt::Record => record_path = Some(value),
            Opt::Tape => replay_tape = Some(value),
            Opt::Replay => test_replay = true,
            Opt::Laws => test_laws = true,
            Opt::WriteLaws => test_write_laws = true,
            Opt::LawsBaseline => law_baseline = Some(PathBuf::from(value)),
            Opt::Rerun => test_rerun = true,
            Opt::Sim => test_sim = true,
            Opt::PropertyCorpus => corpus_path = Some(PathBuf::from(value)),
            Opt::SimSeeds => {
                test_seeds = Some(
                    value
                        .parse::<std::num::NonZeroU64>()
                        .map_err(|_| "--seeds expects a positive count")?
                        .get(),
                );
            }
            Opt::SimSeed => {
                test_seed = Some(value.parse::<u64>().map_err(|_| "--seed expects a u64")?);
            }
            Opt::ExpectVerdict => expect_verdict = Some(value),
            Opt::Diagnose => diagnose = true,
            Opt::Install => install_root = Some(PathBuf::from(value)),
            Opt::LinkKit => link_kit = Some(PathBuf::from(value)),
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
            // Routed before the request runs (`resident::submit`).
            Opt::Resident => {}
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
            | Opt::InspectCore
            | Opt::IncludeCredentials
            | Opt::Lib
            | Opt::FmtCheck
            | Opt::FmtStdin
            | Opt::Signatures
            | Opt::ListRules
            | Opt::PackageStore
            | Opt::Locked
            | Opt::Frozen
            | Opt::Url
            | Opt::Control
            | Opt::HostState
            | Opt::CgroupRoot
            | Opt::OperatorListen
            | Opt::PlatformHost
            | Opt::MaxPlaces
            | Opt::IdleMs
            | Opt::Workdir
            | Opt::Listen
            | Opt::ApproveGrant
            | Opt::RefuseGrants
            | Opt::HostMemory
            | Opt::TapeRetention
            | Opt::GroupName
            | Opt::Tenant
            | Opt::GroupStore
            | Opt::NetRule
            | Opt::Uncontained
            | Opt::Trusted
            | Opt::Pids
            | Opt::Cpu
            | Opt::RequestId
            | Opt::RequestStatus
            | Opt::MaxMemory
            | Opt::Stop
            | Opt::CallTimeout
            | Opt::ProfOut
            | Opt::ProfTasks
            | Opt::ProfMem
            | Opt::ProfFrequency
            | Opt::ProfPmu
            | Opt::ProfStackBytes
            | Opt::ProfDuration
            | Opt::ProfDebug
            | Opt::ProfBy
            | Opt::ProfEvent
            | Opt::ProfGroup
            | Opt::ProfFilter
            | Opt::ProfRows
            | Opt::ProfDepth
            | Opt::ProfMinDeltaPct
            | Opt::ProfTimeout
            | Opt::ProfFormat => {
                unreachable!("CLI early command option")
            }
        }
    }
    if law_baseline.is_some() {
        for (narrowed, option) in [
            (test_filter.is_some(), "--filter"),
            (test_seed.is_some(), "--seed"),
            (test_sim, "--sim"),
            (test_seeds.is_some(), "--seeds"),
            (corpus_path.is_some(), "--property-corpus"),
            (test_replay, "--replay"),
        ] {
            if narrowed {
                return Err(
                    format!("test: --laws-baseline compares a full run; drop {option}").into(),
                );
            }
        }
    }
    let law_baseline = law_baseline
        .as_deref()
        .map(crate::law_lock::Baseline::read)
        .transpose()?;
    test_laws |= test_write_laws || law_baseline.is_some();
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
        if let Some(module) = inspect_foreign {
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
                if let Some(path) = inspect_foreign {
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
                crate::package_lock::Lock::compare(&resolved, package_mode)?
                    .commit(&resolved.workspace)?;
                let include_tests = matches!(command, "check" | "test")
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
    if profile.is_some() && (emulator.is_some() || sysroot.is_some()) {
        return Err("profiling requires a native executable and cannot use an emulator".into());
    }
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
    let observe = profile
        .as_ref()
        .is_some_and(crate::prof_command::Run::observes);
    let charge_observation = logical_charge_report.is_some();
    let env_recording_runtime = installed
        .as_ref()
        .and_then(|installed| installed.recording_runtime(&target));
    let source_tree = libraries.is_some();
    let source_libraries = source_core || source_std || source_tree;
    if let Some(installed) = &installed {
        if core_product.is_none() && !source_tree {
            core_product = Some(installed.cache());
        }
        if runtime.is_none() {
            runtime = match (observe, charge_observation) {
                (false, true) => installed.observed_runtime(&target, needs_recording),
                (true, false) => installed.prof_runtime(&target, needs_recording),
                (false, false) if needs_recording => installed.recording_runtime(&target),
                (false, false) => installed.runtime(&target),
                (true, true) => None,
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
            for (label, archive) in [
                ("profiling runtime", report.prof_runtime),
                ("profiling recording runtime", report.prof_recording_runtime),
                ("charge observation runtime", report.observed_runtime),
                (
                    "charge observation recording runtime",
                    report.observed_recording_runtime,
                ),
            ] {
                println!(
                    "{label}: {}",
                    archive.map_or_else(|| "none".to_owned(), |p| p.display().to_string())
                );
            }
        }
        return conclude(Outcome::Exit(0));
    }
    if install_root.is_some() && (store.is_some() || image_path.is_some()) {
        return Err("--install cannot be combined with --store or --image".into());
    }
    if install_root.is_none()
        && ((command == "core-product" && (explicit_runtime || explicit_target))
            || (command == "std-product" && explicit_runtime)
            || additional_runtimes.recording.is_some()
            || additional_runtimes.prof.is_some()
            || additional_runtimes.prof_recording.is_some()
            || additional_runtimes.observed.is_some()
            || additional_runtimes.observed_recording.is_some())
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
    let test_store = crate::store::temporary_test_store(test_command && store.is_none())?;
    let store = store
        .or_else(|| install_root.as_ref().map(|root| root.join("core-packages")))
        .or_else(|| test_store.as_ref().map(|scratch| scratch.path().to_owned()))
        .unwrap_or_else(|| directory.join(".hirsel-core"));
    let code_store = CodeStore::for_request(command, code_store, &store);
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
            let bytes = std::fs::read(archive)?;
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
        let library = installed_std
            .as_ref()
            .map(|image| image.store_root().to_owned());
        let product =
            publication.assemble_with(&options, Default::default(), None, library.as_deref())?;
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
                crate::installation::publish(
                    root,
                    command == "std-product",
                    runtime.as_deref().map(|runtime| (runtime, target.as_str())),
                    crate::installation::RuntimeVariants {
                        recording: additional_runtimes
                            .recording
                            .as_deref()
                            .map(|path| (path, target.as_str())),
                        prof: additional_runtimes
                            .prof
                            .as_deref()
                            .map(|path| (path, target.as_str())),
                        prof_recording: additional_runtimes
                            .prof_recording
                            .as_deref()
                            .map(|path| (path, target.as_str())),
                        observed: additional_runtimes
                            .observed
                            .as_deref()
                            .map(|path| (path, target.as_str())),
                        observed_recording: additional_runtimes
                            .observed_recording
                            .as_deref()
                            .map(|path| (path, target.as_str())),
                    },
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
            serde_json::json!({"image":hex(image.identity),"rebuilt":rebuilt,"name":name,"index_pages":image.checked_index_pages(),"index_rows":image.decoded_index_rows()})
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
        test_owner = Some(app.package.package.clone());
        manifest_runs = app.tests.take();
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
    if let Some(module) = inspect_foreign {
        let symbols =
            foreign_symbols.ok_or("inspect --foreign-header requires a foreign manifest")?;
        print!(
            "{}",
            crate::foreign_header::header(request.inputs, module, &symbols)?
        );
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
                if explicit_limit || !configured.is_empty() {
                    return Err("this package declares [tests] in hirsel.toml. Set limit, dirs and grants there".into());
                }
                runs
            }
            None => {
                if test_command && !explicit_limit {
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
    let law_directory = dir.as_deref().unwrap_or(Path::new("."));
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
    let run_output = (command == "run" && output_path.is_none())
        .then(|| crate::Store::new(&native_store)?.scratch())
        .transpose()?;
    // Native options come before the reach: a Cranelift development build
    // walks its instance records, which its options address. An options
    // error is reported only for a request check accepts.
    let mut prepared = if native_command {
        let prepare = || -> Result<_, Box<dyn std::error::Error>> {
            let default_output = if let Some(scratch) = &run_output {
                scratch.path().join("program")
            } else if test_command {
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
                profile.as_ref(),
                run_output,
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
            if let Some(profile) = &profile {
                profile.cleanup();
            }
            return conclude(Outcome::Exit(2));
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
                    product.reachable_recipe(entry, options.instance_limit)?
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
        let law_inventory = law_files
            .as_ref()
            .map(|files| crate::law_lock::Inventory::of(&product, &tests, files, &test_runs))
            .transpose()?;
        let mut observations =
            std::collections::BTreeMap::<String, crate::test_run::LawReport>::new();
        let corpus = corpus_path
            .as_deref()
            .map(crate::property_corpus::Corpus::open)
            .transpose()?;
        let cache = if corpus.is_none()
            && !test_sim
            && !test_replay
            && matches!(runner, crate::Runner::Native)
        {
            TestCache::open(&TestCache::default_root())
        } else {
            None
        };
        let mut rows = Vec::new();
        let mut failed = false;
        let jobs = test_jobs.map_or_else(crate::test_run::affinity_jobs, Ok)?;
        for index in 0..if tests.is_empty() {
            0
        } else {
            test_seeds.unwrap_or(1)
        } {
            let seed = test_sim.then_some(test_seed.unwrap_or(index));
            let batch = crate::test_run::TestBatch {
                runs: &test_runs,
                jobs,
                timeout: test_timeout,
                runner: &runner,
                replay: test_replay,
                laws: test_laws,
                plain_runtime: installed.as_ref().and_then(|i| {
                    if charge_observation {
                        i.observed_runtime(&target, false)
                    } else {
                        i.runtime(&target)
                    }
                }),
                expect_verdict: expect_verdict.as_deref(),
                cache: cache.as_ref(),
                rerun: test_rerun || test_write_laws,
                seed,
                corpus: corpus.as_ref(),
            };
            batch.run(&product, &tests, native_options.as_ref().unwrap(), |test, result| {
            let label = seed.map_or_else(|| test.to_string(), |seed| format!("{test} (seed {seed})"));
            let mut row = match result {
                Ok((outcome, source)) => {
                    let passed = outcome.passed();
                    let wall_ms = (source == Source::Executed)
                        .then(|| outcome.wall_time.as_secs_f64() * 1000.0);
                    let source = source.as_str();
                    failed |= !passed;
                    let corpus_note = outcome.corpus.map_or_else(String::new, |state| {
                        format!(" [property corpus {state}]")
                    });
                    let strength = test_laws.then(|| outcome.laws.strength());
                    let laws_note = strength.map_or_else(String::new, |class| format!(" {class}"));
                    if !json {
                        if let Some(wall_ms) = wall_ms {
                            if passed {
                                println!("pass {source} {label}{corpus_note}{laws_note} ({wall_ms:.3} ms)");
                            } else {
                                let mut reason = if outcome.timed_out {
                                    "execution timed out".to_owned()
                                } else {
                                    crate::test_run::outcome_text(outcome.output.status)
                                };
                                if !outcome.output.stderr.is_empty() {
                                    reason.push_str(": ");
                                    reason.push_str(
                                        &String::from_utf8_lossy(&outcome.output.stderr)
                                            .lines()
                                            .collect::<Vec<_>>()
                                            .join(" | "),
                                    );
                                }
                                println!("fail {source} {label}{corpus_note}{laws_note}: {reason} ({wall_ms:.3} ms)");
                                for line in String::from_utf8_lossy(&outcome.output.stdout).lines() {
                                    println!("  stdout: {line}");
                                }
                            }
                        } else {
                            println!("pass {source} {label}{laws_note}");
                        }
                    }
                    let mut row = serde_json::json!({"definition":test.to_string(),"passed":passed,"source":source,"status":outcome.output.status.code(),"stdout":String::from_utf8_lossy(&outcome.output.stdout),"stderr":String::from_utf8_lossy(&outcome.output.stderr),"timed_out":outcome.timed_out,"wall_ms":wall_ms});
                    if let Some(state) = outcome.corpus {
                        row["property_corpus"] = state.into();
                    }
                    if let Some(strength) = strength {
                        if !json {
                            outcome.laws.print();
                        }
                        row["law_strength"] = strength.into();
                        row["laws"] = outcome.laws.json_rows();
                    }
                    if test_write_laws || law_baseline.is_some() {
                        match observations.entry(test.to_string()) {
                            std::collections::btree_map::Entry::Occupied(mut held) => held.get_mut().merge(outcome.laws),
                            std::collections::btree_map::Entry::Vacant(slot) => { slot.insert(outcome.laws); }
                        }
                    }
                    row
                }
                Err(error) => {
                    failed = true;
                    let mut row = serde_json::json!({"definition":test.to_string(),"passed":false,"source":"executed","stopped":error.to_string(),"wall_ms":null});
                    if test_laws {
                        row["law_strength"] = if matches!(error, crate::NativeStop::ForeignRecording { .. }) { "unobserved (FFI-8)" } else { "unobserved" }.into();
                        row["laws"] = serde_json::json!([]);
                    }
                    if let crate::NativeStop::ForeignRecording { site, .. } = &error {
                        row["site"] = site.as_str().into();
                        if !json {
                            eprintln!("{site}: recording refusal site");
                            println!("fail executed {label}: {error}");
                        }
                    } else if !json {
                        println!("fail executed {label}: {error} (time unavailable)");
                    }
                    row
                }
            };
            if let Some(seed) = seed {
                row["seed"] = seed.into();
            }
            rows.push(row);
        });
        }
        if let Some(current) = law_inventory {
            crate::law_lock::update(law_directory, |lock| lock.record(&current, &observations))?;
        }
        let regressions = law_baseline
            .as_ref()
            .map(|baseline| baseline.compare(&observations));
        let law_red = regressions.as_ref().is_some_and(|rows| !rows.is_empty());
        failed |= law_red;
        let passed = !tests.is_empty() && !failed;
        if json {
            let mut report = serde_json::json!({"passed":passed,"tests":rows});
            if let Some(regressions) = regressions {
                report["law_regressions"] = serde_json::json!(regressions);
            }
            println!("{report}");
        } else if let Some(regressions) = regressions {
            for red in regressions {
                println!("{red}");
            }
        }
        if tests.is_empty() && !json {
            eprintln!("no tests found");
        }
        meter.finish("test execution/output");
        if let Some(path) = metrics {
            meter.write_metrics(path)?;
        }
        drop(test_store);
        return conclude(Outcome::Exit(if tests.is_empty() && !law_red {
            2
        } else {
            i32::from(failed)
        }));
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
                        return conclude(Outcome::Exit(0));
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
        image: image.map(|i| hex(i.identity)),
        native,
        stopped: stopped.or_else(|| {
            reach
                .as_ref()
                .and_then(|r| r.instances.as_ref().err())
                .filter(|e| !matches!(e, crate::InstanceStop::Resource { .. }))
                .map(|e| format!("instances: {e}"))
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
        profile.as_ref(),
        run_output,
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
        profile,
        run_output,
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
    profile: Option<&crate::prof_command::Run>,
    mut run_output: Option<crate::store::Scratch>,
    meter: &mut Meter,
    metrics: Option<PathBuf>,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    if json {
        println!("{}", serde_json::to_string(output)?);
    } else {
        for d in &output.diagnostics {
            eprintln!("{}: {} {}", sources.render(&d.primary), d.rule, d.message);
            for cause in &d.causal {
                eprintln!("  {}: {}", cause.role.label(), sources.render(&cause.site));
            }
            for suggestion in crate::diagnostics::render_suggestions(&d.fixes, &d.available_members)
            {
                eprintln!("  {suggestion}");
            }
        }
        for g in &output.incomplete {
            eprintln!("{}: incomplete {}", sources.render(&g.primary), g.message);
            for cause in &g.causal {
                eprintln!("  {}: {}", cause.role.label(), sources.render(&cause.site));
            }
        }
        for r in &output.resources {
            eprintln!("{}: resource {}", sources.render(&r.primary), r.message);
        }
        // Behind a printed refusal an instance stop is its consequence.
        if let Some(stop) = &output.stopped
            && (output.diagnostics.is_empty() && output.incomplete.is_empty()
                || !stop.starts_with("instances: "))
        {
            eprintln!("{stop}");
        }
        if command != "run" && command != "replay" && (!output.accepted || output.stopped.is_none())
        {
            println!(
                "{}: {} files, {} checked definitions",
                if output.accepted {
                    "accepted"
                } else {
                    "not accepted"
                },
                output.files,
                output.checked_definitions
            );
        }
    }
    let mut launch = None;
    let run_exit = if command == "run" || command == "replay" {
        if let Some(native) = &output.native {
            let path = std::fs::canonicalize(&native.executable)?;
            if let Some(profile) = profile {
                let status = profile.capture(&path, &run_args, record_path)?;
                if !status.success() {
                    eprintln!("hirsel: main {}", crate::test_run::outcome_text(status));
                }
                Some(status.code().unwrap_or(102))
            } else {
                let program = runner.command(&path);
                let mut argv = vec![program.get_program().to_owned()];
                argv.extend(program.get_args().map(ToOwned::to_owned));
                argv.extend(run_args.into_iter().map(Into::into));
                let env = (command == "run")
                    .then_some(record_path)
                    .flatten()
                    .map(|record| ("HIRSEL_RECORD".into(), record.into()))
                    .into_iter()
                    .collect();
                launch = Some(Launch {
                    argv,
                    env,
                    keep: run_output.take(),
                });
                None
            }
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
    if let Some(profile) = profile {
        profile.cleanup();
    }
    drop(run_output);
    conclude(launch.map_or(Outcome::Exit(exit), Outcome::Launch))
}

/// What a request leaves for its caller: an exit status, or the program it
/// built for `run` or `replay`, which the caller runs and whose status is the
/// request's. A resident compiler's client runs the program itself, so it has
/// the client's terminal, signals and process group.
pub enum Outcome {
    Exit(i32),
    Launch(Launch),
}
/// The program a `run` or `replay` request built: its argument vector (the
/// runner's, then the executable's, then the request's), the variables it runs
/// with besides the caller's environment, and the scratch directory that holds
/// it until it has run.
pub struct Launch {
    pub argv: Vec<std::ffi::OsString>,
    pub env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    pub(crate) keep: Option<crate::store::Scratch>,
}
impl Launch {
    /// Run the program and return the request's exit status: the program's,
    /// or 102 when a signal ended it.
    pub fn execute(
        argv: &[std::ffi::OsString],
        env: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> Result<i32, Box<dyn std::error::Error>> {
        let status = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .envs(env.iter().map(|(name, value)| (name, value)))
            .status()
            .map_err(|e| format!("run execution {:?}: {e}", argv[0]))?;
        if !status.success() {
            eprintln!("hirsel: main {}", crate::test_run::outcome_text(status));
        }
        Ok(status.code().unwrap_or(102))
    }
}

/// End a request with `outcome`. A one-shot process exits here, running the
/// program first for a launch: the in-memory graphs are the OS's to reclaim,
/// with no teardown. A resident compiler answers its client here, before the
/// request's frames unwind, and the request returns its outcome to the server.
fn conclude(outcome: Outcome) -> Result<Outcome, Box<dyn std::error::Error>> {
    if crate::resident::serving() {
        crate::resident::answer(&outcome);
        return Ok(outcome);
    }
    match outcome {
        Outcome::Exit(code) => std::process::exit(code),
        Outcome::Launch(launch) => {
            let code = Launch::execute(&launch.argv, &launch.env)?;
            drop(launch);
            std::process::exit(code)
        }
    }
}

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
    eprintln!("hirsel: {e}", e = error);
    std::process::exit(2)
}

/// Serve one request in a resident compiler: [`main`]'s request, with its
/// error answered as main reports it.
pub(crate) fn serve(args: Vec<String>) -> Outcome {
    match run(args) {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("hirsel: {e}");
            let outcome = Outcome::Exit(2);
            crate::resident::answer(&outcome);
            outcome
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
