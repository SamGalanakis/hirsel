use super::{Meter, Outcome, Output, PathBuf};
use std::io::{self, Write};

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

#[path = "dev.rs"]
pub(super) mod dev;
#[path = "dry_run.rs"]
pub(super) mod dry_run;
#[path = "fleet_cli.rs"]
pub(super) mod fleet_cli;
#[path = "store_cli.rs"]
pub(super) mod store_cli;

pub(crate) fn report_request_error(error: &(dyn std::error::Error + 'static)) -> i32 {
    if let Some(damage) = error
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::get_ref)
        .and_then(|inner| inner.downcast_ref::<crate::tape_cli::ReplayDamage>())
    {
        let _ = writeln!(io::stderr().lock(), "{damage}");
        1
    } else {
        let _ = writeln!(io::stderr().lock(), "hirsel: {error}");
        2
    }
}

/// End a request with `outcome`. A one-shot process exits here, running the
/// program first for a launch: the in-memory graphs are the OS's to reclaim,
/// with no teardown. A resident compiler answers its client here, before the
/// request's frames unwind, and the request returns its outcome to the server.
/// `hirsel dry-run` and `hirsel store backup|restore`: the commands that run a tool on a
/// store directory and exit with its status.
pub(super) fn store_tool(
    invocation: crate::cli::Request<crate::cli::StoreOption>,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    let releases = matches!(invocation.command.name, "store verify" | "store repair")
        && invocation
            .options
            .iter()
            .any(|option| matches!(option, crate::cli::StoreOption::StoreRelease(_)));
    conclude(Outcome::Exit(
        if matches!(invocation.command.name, "dry-run" | "tapes check") || releases {
            dry_run::run(invocation)?
        } else {
            store_cli::run(invocation)?
        },
    ))
}

pub(super) fn conclude(outcome: Outcome) -> Result<Outcome, Box<dyn std::error::Error>> {
    if crate::resident::serving() {
        // A session answers as its client steps it (`resident::serve_session`).
        if !matches!(outcome, Outcome::Test(_)) {
            crate::resident::answer(&outcome);
        }
        crate::store::remote::finish();
        return Ok(outcome);
    }
    crate::store::remote::finish();
    match outcome {
        Outcome::Exit(code) => std::process::exit(code),
        Outcome::Test(session) => std::process::exit(session.conclude()),
        Outcome::Launch(launch) => {
            let code = Launch::execute(&launch.argv, &launch.env)?;
            drop(launch);
            std::process::exit(code)
        }
    }
}

/// Print `output`, run a `run` request's executable, and exit with the
/// request's status.
#[allow(clippy::too_many_arguments)]
pub(super) fn finish(
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

/// Serve one request in a resident compiler: [`super::main`]'s request, with its
/// error answered as main reports it.
pub(crate) fn serve(args: Vec<String>) -> Outcome {
    match super::run(args) {
        Ok(outcome) => outcome,
        Err(e) => {
            crate::store::remote::finish(); // No session outlives its request.
            let outcome = Outcome::Exit(report_request_error(e.as_ref()));
            crate::resident::answer(&outcome);
            outcome
        }
    }
}
