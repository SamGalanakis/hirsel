use super::*;

#[path = "../check_library.rs"]
mod check_library;

pub(super) fn run(mut args: Vec<String>) -> Result<Outcome, Box<dyn std::error::Error>> {
    if args.len() == 1 && args[0] == check_library::WORKER_ARG {
        return conclude(Outcome::Exit(check_library::run_worker()));
    }
    let json = args
        .iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--json");
    if args.first().is_some_and(|arg| arg == "--json") {
        let end = args
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(args.len());
        args[..end].rotate_left(1);
    }
    let parsed = crate::cli::parse(args.iter().cloned());
    let action = match parsed {
        Ok(action) => action,
        Err(detail) => {
            let name = args
                .iter()
                .take_while(|arg| !arg.starts_with('-'))
                .take(
                    if args
                        .first()
                        .is_some_and(|arg| matches!(arg.as_str(), "op" | "registry"))
                    {
                        2
                    } else {
                        1
                    },
                )
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            return conclude(Outcome::Exit(operator_cli::finite::invalid(
                &name, detail, json,
            )));
        }
    };
    let invocation = match action {
        crate::cli::Action::Help(help) => {
            return conclude(Outcome::Exit(operator_cli::finite::schema_help(help, json)));
        }
        crate::cli::Action::Version => {
            crate::installation::print_version()?;
            return conclude(Outcome::Exit(0));
        }
        crate::cli::Action::Invoke(invocation) => invocation,
    };
    match invocation {
        Invocation::OpGet(request) => conclude(Outcome::Exit(operator_cli::finite::get(request))),
        Invocation::Help(request) => conclude(Outcome::Exit(operator_cli::finite::help(request))),
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
        Invocation::RegistryPublish(request) => {
            crate::registry_publish::run(request, check_registry_member)?;
            conclude(Outcome::Exit(0))
        }
        Invocation::Add(request) => conclude(Outcome::Exit(crate::package_add::run(request)?)),
        Invocation::Update(request) => {
            conclude(Outcome::Exit(crate::package_update::run(request)?))
        }
        Invocation::Audit(request) => conclude(Outcome::Exit(crate::package_audit::run(request)?)),
        Invocation::Launcher(request) => match crate::launcher::run(request)? {},
        Invocation::Serve(request) => {
            if crate::resident::serving() {
                return Err("a resident compiler serves build and run requests only".into());
            }
            let serve = crate::resident::Serve::read(request)?;
            conclude(Outcome::Exit(crate::resident::run(serve)?))
        }
        Invocation::Host(request) => match operator_cli::host(request)? {},
        Invocation::Deploy(request) => conclude(Outcome::Exit(operator_cli::deploy(request))),
        Invocation::Decide(request) => conclude(Outcome::Exit(operator_cli::decide(request))),
        Invocation::Call(request) => conclude(Outcome::Exit(crate::call::run(
            crate::call::Options::read(request)?,
        )?)),
        Invocation::Dev(request) => conclude(Outcome::Exit(launch::dev::run(request)?)),
        Invocation::Store(request) => launch::store_tool(request),
        Invocation::Sdk(mut request) => {
            use crate::cli::SdkOption;
            let emit = request
                .options
                .extract_if(.., |option| matches!(option, SdkOption::Emit(_)))
                .filter_map(|option| match option {
                    SdkOption::Emit(value) => Some(value),
                    _ => None,
                })
                .last();
            let kind = output_kind(request.command.name, emit.as_deref())?;
            if request.command.name == "sdk swift" {
                crate::sdk_command::swift::run(request)?;
                return conclude(Outcome::Exit(0));
            }
            for option in &request.options {
                if let SdkOption::Target(target) = option {
                    admit_target(target, kind)?;
                }
            }
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
            if request.positional.is_some() {
                return Err("primer takes no arguments".into());
            }
            print!("{}", crate::cli_tools::PRIMER);
            conclude(Outcome::Exit(0))
        }
        Invocation::New(request) => {
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
            let action = if request.options.contains(&FmtOption::FmtCheck) {
                FormatAction::Check
            } else {
                FormatAction::Write
            };
            let explicit_data = request.options.contains(&FmtOption::FmtData);
            let data_file = request
                .positional
                .as_deref()
                .is_some_and(|p| Path::new(p).extension().is_some_and(|ext| ext == "hcfg"));
            let mode = if explicit_data || data_file {
                if request.options.contains(&FmtOption::Signatures) {
                    return Err("data and signature modes are mutually exclusive".into());
                }
                if request
                    .positional
                    .as_deref()
                    .is_some_and(|p| Path::new(p).is_dir())
                {
                    return Err("data formatting requires a file or --stdin".into());
                }
                ParseMode::Data
            } else if request.options.contains(&FmtOption::Signatures) {
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
            crate::cli_tools::explain(
                request.positional.as_deref(),
                request.options.contains(&ExplainOption::ListRules),
                request.options.contains(&ExplainOption::Json),
            )?;
            conclude(Outcome::Exit(0))
        }
        Invocation::Lsp(_) => conclude(Outcome::Exit(crate::lsp::run()?)),
        Invocation::Compiler(request) => run_compiler(request),
    }
}

/// Run the request `args` names (the arguments after the program name) and
/// exit with its status, through a resident compiler when one serves the
/// request's store (`crate::resident`). Process setup (allocator guard,
/// signal dispositions, inherited descriptors) is the caller's.
pub fn main(mut args: Vec<String>) -> ! {
    if args
        .first()
        .is_some_and(|arg| arg == hirsel_core_ids::module_build::WORKER)
    {
        args[0] = "build".into();
        let error = run(args)
            .err()
            .expect("a one-shot worker exits in conclude");
        use hirsel_core_ids::module_build::ModuleBuild;
        let rejected = error
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::get_ref)
            .and_then(|cause| cause.downcast_ref::<crate::RejectedCheck>())
            .is_some_and(|r| {
                r.product.resources.is_empty()
                    && r.product.incomplete.is_empty()
                    && !r.product.diagnostics.is_empty()
            });
        let reply = if rejected {
            ModuleBuild::Rejected
        } else {
            ModuleBuild::ToolRefusal
        };
        println!("{}", serde_json::to_string(&reply).unwrap());
        std::process::exit(report_request_error(error.as_ref()));
    }
    if args.len() == 1 && args[0] == check_library::WORKER_ARG {
        std::process::exit(check_library::run_worker());
    }
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

fn check_registry_member(
    resolution: &crate::package_resolve::Resolution,
    id: &crate::package_content::Id,
    libraries: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let manifest = &resolution.workspace.members[id].manifest_path;
    let check = crate::cli::COMMANDS
        .iter()
        .find(|spec| spec.name == "check")
        .unwrap();
    let invocation = Request {
        command: check,
        positional: Some(manifest.parent().unwrap().to_string_lossy().into_owned()),
        options: libraries
            .map(|path| Opt::Libraries(path.to_owned()))
            .into_iter()
            .collect(),
        arguments: Vec::new(),
    };
    let app = crate::package_dir::PackageDir::from_resolution(resolution, id, true)?;
    match run_compiler_request(
        invocation,
        check.name,
        OutputKind::Executable,
        None,
        PackageInput::Captured(app),
        None,
    ) {
        Ok(Outcome::Exit(0)) => Ok(()),
        other => {
            if let Err(error) = other {
                report_request_error(error.as_ref());
            }
            Err(format!("registry publish: {} does not check", manifest.display()).into())
        }
    }
}

fn run_compiler(mut invocation: Request<Opt>) -> Result<Outcome, Box<dyn std::error::Error>> {
    let emit = invocation
        .options
        .extract_if(.., |option| matches!(option, Opt::Emit(_)))
        .filter_map(|option| match option {
            Opt::Emit(value) => Some(value),
            _ => None,
        })
        .last();
    let output_kind = output_kind(invocation.command.name, emit.as_deref())?;
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
    let (package_mode, package_store, pack_out) = packages::options(&mut invocation, command);
    let transport = package_mode.transport();
    let remote = crate::store::remote::for_request(&transport)?;
    let store = crate::package_store::PackageStore::with_transport(package_store, transport);
    if matches!(command, "lock" | "fetch" | "pack") {
        return packages::run(invocation, store, package_mode, pack_out, remote);
    }
    run_compiler_request(
        invocation,
        command,
        output_kind,
        profile,
        PackageInput::Discover(package_mode, store),
        remote,
    )
}
