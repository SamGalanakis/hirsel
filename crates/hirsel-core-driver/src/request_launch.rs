use super::{Meter, Output, PathBuf};
use std::fs::File;
use std::io::{self, Write};
use std::os::{
    fd::{AsFd, FromRawFd},
    unix::{fs::MetadataExt, process::ExitStatusExt},
};

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

/// The program a `run` or `replay` request built: its argument vector (the
/// runner's, then the executable's, then the request's), the variables it runs
/// with besides the caller's environment, and the scratch directory that holds
/// it until it has run.
pub struct Launch {
    pub argv: Vec<std::ffi::OsString>,
    pub env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    pub(crate) mode: crate::tape_cli::outputs::Mode,
    pub(crate) keep: Option<crate::store::Scratch>,
}
impl Launch {
    /// Run the program and return the request's exit status: the program's,
    /// or 102 when a signal ended it.
    pub fn execute(
        argv: &[std::ffi::OsString],
        env: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> Result<i32, Box<dyn std::error::Error>> {
        Self::execute_mode(argv, env, &crate::tape_cli::outputs::Mode::Run)
    }

    pub(crate) fn execute_mode(
        argv: &[std::ffi::OsString],
        env: &[(std::ffi::OsString, std::ffi::OsString)],
        mode: &crate::tape_cli::outputs::Mode,
    ) -> Result<i32, Box<dyn std::error::Error>> {
        let mut command = std::process::Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .envs(env.iter().map(|(name, value)| (name, value)));
        let status = match mode {
            crate::tape_cli::outputs::Mode::Run => command.status(),
            crate::tape_cli::outputs::Mode::Replay(dir) => {
                crate::tape_cli::outputs::execute(&mut command, dir.as_deref())
            }
        }
        .map_err(|e| format!("run execution {:?}: {e}", argv[0]))?;
        if !status.success() {
            eprintln!("hirsel: main {}", crate::test_run::outcome_text(status));
        }
        Ok(match mode {
            crate::tape_cli::outputs::Mode::Run => i32::from(!status.success()),
            crate::tape_cli::outputs::Mode::Replay(_) => status.code().unwrap_or(1),
        })
    }
}

#[path = "dev.rs"]
pub(crate) mod dev;
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
/// `hirsel store rehearse` and `hirsel store backup|restore`: the commands that run a tool on a
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
        if matches!(invocation.command.name, "store rehearse" | "tape check") || releases {
            dry_run::run(invocation)?
        } else {
            store_cli::run(invocation)?
        },
    ))
}

pub(super) fn module_worker() -> bool {
    std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new(hirsel_core_ids::module_build::WORKER))
}

pub(super) fn conclude(outcome: Outcome) -> Result<Outcome, Box<dyn std::error::Error>> {
    conclude_with_remote(outcome, None)
}

pub(super) fn conclude_with_remote(
    outcome: Outcome,
    remote: Option<&crate::store::remote::Remote>,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    let finish_uploads = || {
        if let Some(remote) = remote {
            remote.finish_uploads();
        }
    };
    if crate::resident::serving() {
        // A test session retains its remote until its client finishes stepping it.
        if !matches!(outcome, Outcome::Test(_)) {
            crate::resident::answer(&outcome);
            finish_uploads();
        }
        return Ok(outcome);
    }
    match outcome {
        Outcome::Exit(code) => {
            finish_uploads();
            std::process::exit(code)
        }
        Outcome::Test(session) => {
            let code = session.conclude();
            finish_uploads();
            std::process::exit(code)
        }
        Outcome::Launch(launch) => {
            finish_uploads();
            let code = Launch::execute_mode(&launch.argv, &launch.env, &launch.mode)?;
            drop(launch);
            std::process::exit(code)
        }
    }
}

pub(super) fn render_diagnostics(
    sources: &crate::SourceFiles,
    diagnostics: &[crate::Diagnostic<'_>],
    incomplete: &[crate::IncompleteDiagnostic<'_>],
    resources: &[crate::ResourceDiagnostic<'_>],
) {
    for d in diagnostics {
        eprintln!("{}: {} {}", sources.render(&d.primary), d.rule, d.message);
        for cause in &d.causal {
            eprintln!("  {}: {}", cause.role.label(), sources.render(&cause.site));
        }
        for suggestion in crate::diagnostics::render_suggestions(&d.fixes, &d.available_members) {
            eprintln!("  {suggestion}");
        }
    }
    for g in incomplete {
        eprintln!("{}: incomplete {}", sources.render(&g.primary), g.message);
        for cause in &g.causal {
            eprintln!("  {}: {}", cause.role.label(), sources.render(&cause.site));
        }
    }
    for r in resources {
        eprintln!("{}: resource {}", sources.render(&r.primary), r.message);
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
    compiler_worker: Option<PathBuf>,
    mut run_args: Vec<String>,
    record_path: Option<&str>,
    replay_outputs: Option<&str>,
    profile: Option<&crate::prof_command::Run>,
    streams: Option<&[File; 2]>,
    mut run_output: Option<crate::store::Scratch>,
    meter: &mut Meter,
    metrics: Option<PathBuf>,
    remote: Option<&crate::store::remote::Remote>,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    if let Some(streams) = streams {
        let result = structured_run(
            output,
            runner,
            compiler_worker,
            &mut run_args,
            record_path,
            profile,
            streams,
        );
        meter.finish("output/render");
        if let Some(path) = metrics {
            meter.write_metrics(path)?;
        }
        if let Some(profile) = profile {
            profile.cleanup();
        }
        use crate::operator_cli::finite::{Envelope, Failure};
        use serde_json::json;
        let mut env = Envelope::read(if profile.is_some() { "prof run" } else { "run" });
        env.contract = "execute";
        env.result = json!({"build":output,"guest":null});
        match result {
            Ok(Some(status)) => {
                env.result["guest"] = json!({"exit":status.code(),"signal":status.signal()});
                env.outcome = json!({"kind":if status.success() {"Exited"} else {"GuestFailed"},"effect":"committed"});
                env.exit = i32::from(!status.success());
            }
            Ok(None) => env.fail(Failure::negative(
                "BuildRefused",
                "the compiler did not produce an executable",
            )),
            Err(error) => env.fail(Failure::negative("LaunchFailed", &error.to_string())),
        }
        return conclude_with_remote(Outcome::Exit(env.render(true)), remote);
    }
    if json && command == "tape replay" && profile.is_none() {
        use crate::operator_cli::finite::{Envelope, Failure};
        use serde_json::json;
        let mut env = Envelope::read(command);
        env.contract = "execute";
        env.result = json!({"build":output,"guest":null});
        if let Some(native) = &output.native {
            let path = std::fs::canonicalize(&native.executable)?;
            let mut guest = runner.command(&path);
            guest.args(run_args).stdout(std::process::Stdio::null());
            let status = crate::tape_cli::outputs::execute(
                &mut guest,
                replay_outputs.map(std::path::Path::new),
            )?;
            env.result["guest"] = json!({"exit":status.code(),"signal":status.signal()});
            if status.success() {
                env.outcome = json!({"kind":"Sealed","effect":"none"});
            } else {
                env.fail(Failure::negative(
                    "ReplayRefused",
                    "the guest did not seal the replay",
                ));
            }
        } else {
            env.fail(Failure::negative(
                "BuildRefused",
                "the compiler did not produce a replay executable",
            ));
        }
        meter.finish("output/render");
        if let Some(path) = metrics {
            meter.write_metrics(path)?;
        }
        return conclude_with_remote(Outcome::Exit(env.render(true)), remote);
    }
    if command == "build" && module_worker() {
        use hirsel_core_ids::module_build::ModuleBuild;
        let reply = if let Some(reason) = &output.module_unsupported {
            ModuleBuild::Unsupported(reason.clone())
        } else if !output.resources.is_empty() || !output.incomplete.is_empty() {
            ModuleBuild::ToolRefusal
        } else if !output.diagnostics.is_empty() {
            ModuleBuild::Rejected
        } else if output.native.is_some() && output.stopped.is_none() {
            ModuleBuild::Built
        } else {
            ModuleBuild::ToolRefusal
        };
        render_diagnostics(
            sources,
            &output.diagnostics,
            &output.incomplete,
            &output.resources,
        );
        if let Some(stop) = &output.stopped {
            eprintln!("{stop}");
        }
        println!("{}", serde_json::to_string(&reply)?);
    } else if json {
        println!("{}", serde_json::to_string(output)?);
    } else {
        render_diagnostics(
            sources,
            &output.diagnostics,
            &output.incomplete,
            &output.resources,
        );
        // Behind a printed refusal an instance stop is its consequence.
        if let Some(stop) = &output.stopped
            && (output.diagnostics.is_empty() && output.incomplete.is_empty()
                || !stop.starts_with("instances: "))
        {
            eprintln!("{stop}");
        }
        if command != "run"
            && command != "tape replay"
            && (!output.accepted || output.stopped.is_none())
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
    let run_exit = if command == "run" || command == "tape replay" {
        if let Some(native) = &output.native {
            let path = std::fs::canonicalize(&native.executable)?;
            if let Some(worker) = compiler_worker {
                run_args.insert(0, format!("--hirsel-compiler-worker={}", worker.display()));
            }
            if let Some(profile) = profile {
                let status = profile.capture(&path, &run_args, record_path, None)?;
                if !status.success() {
                    eprintln!("hirsel: main {}", crate::test_run::outcome_text(status));
                }
                Some(if command == "run" {
                    i32::from(!status.success())
                } else {
                    status.code().unwrap_or(1)
                })
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
                let mode = if command == "tape replay" {
                    crate::tape_cli::outputs::Mode::Replay(replay_outputs.map(PathBuf::from))
                } else {
                    crate::tape_cli::outputs::Mode::Run
                };
                launch = Some(Launch {
                    argv,
                    env,
                    mode,
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
    conclude_with_remote(launch.map_or(Outcome::Exit(exit), Outcome::Launch), remote)
}

/// Serve one request in a resident compiler: [`super::main`]'s request, with its
/// error answered as main reports it.
pub(crate) fn serve(args: Vec<String>) -> Outcome {
    match super::dispatch::run(args) {
        Ok(outcome) => outcome,
        Err(e) => {
            let outcome = Outcome::Exit(report_request_error(e.as_ref()));
            crate::resident::answer(&outcome);
            outcome
        }
    }
}

pub(super) fn guest_streams() -> io::Result<[File; 2]> {
    let control = File::from(io::stdout().as_fd().try_clone_to_owned()?).metadata()?;
    let output = |fd| {
        // SAFETY: fcntl checks the inherited descriptor and returns a new descriptor.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || flags & libc::O_ACCMODE == libc::O_RDONLY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("redirect writable fd {fd} before structured execution"),
            ));
        }
        // SAFETY: fcntl checks fd again and returns a unique close-on-exec descriptor.
        let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 5) };
        if copy < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: copy is owned by this call.
        let file = unsafe { File::from_raw_fd(copy) };
        let guest = file.metadata()?;
        if (guest.dev(), guest.ino()) == (control.dev(), control.ino()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("fd {fd} shares the JSON output destination"),
            ));
        }
        Ok(file)
    };
    Ok([output(3)?, output(4)?])
}

fn structured_run(
    output: &Output<'_>,
    runner: &crate::Runner,
    compiler_worker: Option<PathBuf>,
    args: &mut Vec<String>,
    record_path: Option<&str>,
    profile: Option<&crate::prof_command::Run>,
    streams: &[File; 2],
) -> Result<Option<std::process::ExitStatus>, Box<dyn std::error::Error>> {
    let Some(native) = &output.native else {
        return Ok(None);
    };
    let launch = || -> Result<_, Box<dyn std::error::Error>> {
        let path = std::fs::canonicalize(&native.executable)?;
        if let Some(worker) = compiler_worker {
            args.insert(0, format!("--hirsel-compiler-worker={}", worker.display()));
        }
        if let Some(profile) = profile {
            return profile.capture(&path, args, record_path, Some(streams));
        }
        let [stdout, stderr] = streams;
        let mut program = runner.command(&path);
        program
            .args(args)
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?);
        if let Some(path) = record_path {
            program.env("HIRSEL_RECORD", path);
        }
        Ok(program.status()?)
    };
    launch().map(Some)
}
