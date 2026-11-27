use super::*;

pub(super) fn run(args: Vec<String>) -> Result<Outcome, Box<dyn std::error::Error>> {
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
