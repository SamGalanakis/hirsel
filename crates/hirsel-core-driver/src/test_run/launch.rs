//! A test launch as data: what the driver that built a test hands the
//! process that runs it (`docs/spec/cli.md`, "The resident compiler"). The
//! runner starts the executable in a private working directory under its own
//! environment, captures and limits it, reads its law and coverage files and
//! writes the pass cache; the driver only reads the run that comes back.
use super::laws::{LawCapture, Sites};
use super::{CoverageCapture, InheritedSlots, LawReport, RunCoverage, TestCache, TestOutcome};
use hirsel_core_ids::{ContentHash, DefPath};
use hirsel_core_ir::coverage::Fragment;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct LaunchSpec {
    program: OsString,
    args: Vec<OsString>,
    /// Set (Some) or removed (None) over the runner's own environment.
    env: Vec<(OsString, Option<OsString>)>,
    slots: InheritedSlots,
    /// The law sites the run's summary is read against, when it is observed.
    laws: Option<Sites>,
    /// The site map of a coverage build (the inner None: the build has none).
    coverage: Option<Option<Fragment>>,
    /// The pass cache's root and this run's key: a pass is stored there.
    pass: Option<(PathBuf, ContentHash)>,
    /// A run reaching foreign code reports no law rows (FFI-8).
    foreign: bool,
    timeout: Option<Duration>,
}

/// A test run as its runner collected it.
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct Ran {
    /// The raw wait status.
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
    wall_time: Duration,
    laws: LawReport,
    coverage: RunCoverage,
}

impl Ran {
    pub(super) fn outcome(self, test: &DefPath) -> TestOutcome {
        use std::os::unix::process::ExitStatusExt;
        TestOutcome {
            definition: test.clone(),
            output: Output {
                status: std::process::ExitStatus::from_raw(self.status),
                stdout: self.stdout,
                stderr: self.stderr,
            },
            timed_out: self.timed_out,
            wall_time: self.wall_time,
            corpus: None,
            laws: self.laws,
            coverage: self.coverage,
        }
    }
}

pub(super) struct Observe {
    pub laws: Option<Sites>,
    pub coverage: Option<Option<Fragment>>,
    pub pass: Option<(PathBuf, ContentHash)>,
    pub foreign: bool,
}

impl LaunchSpec {
    /// `command` as data: its program, arguments and environment changes. A
    /// working directory or descriptor setup of its own is not carried.
    pub(super) fn new(
        command: &Command,
        slots: InheritedSlots,
        observe: Observe,
        timeout: Option<Duration>,
    ) -> Self {
        Self {
            program: command.get_program().to_owned(),
            args: command.get_args().map(ToOwned::to_owned).collect(),
            env: command
                .get_envs()
                .map(|(name, value)| (name.to_owned(), value.map(ToOwned::to_owned)))
                .collect(),
            slots,
            laws: observe.laws,
            coverage: observe.coverage,
            pass: observe.pass,
            foreign: observe.foreign,
            timeout,
        }
    }

    pub(super) fn set_env(&mut self, name: &str, value: &str) {
        self.env.push((name.into(), Some(value.into())));
    }

    /// Run the launch here: in a fresh private working directory, under this
    /// process's environment with the launch's changes, within its deadline.
    pub(crate) fn run(self) -> io::Result<Ran> {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        for (name, value) in &self.env {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        self.slots.pass(&mut command)?;
        let directory = private_directory(&mut command)?;
        // A run whose law file has nowhere to go is unobserved, and so not stored.
        let observed = self.laws.is_some();
        let laws = self
            .laws
            .and_then(|sites| LawCapture::attach(&mut command, sites));
        let coverage = self
            .coverage
            .map(|map| CoverageCapture::attach(&mut command, map.map(Arc::new).as_ref()));
        let (output, timed_out, wall_time) = execute(&mut command, self.timeout)?;
        let laws = if self.foreign {
            LawReport::Foreign
        } else {
            laws.as_ref()
                .map_or_else(LawReport::default, LawCapture::report)
        };
        let coverage_report = coverage
            .as_ref()
            .map_or_else(RunCoverage::default, CoverageCapture::report);
        drop(directory);
        let passed = !timed_out && output.status.success();
        if passed
            && (!observed || matches!(laws, LawReport::Observed(_)))
            && (coverage.is_none() || matches!(coverage_report, RunCoverage::Observed(_)))
            && let Some((root, key)) = &self.pass
            && let Some(cache) = TestCache::open(root)
        {
            cache.put(*key, &output, &laws, &coverage_report);
        }
        use std::os::unix::process::ExitStatusExt;
        Ok(Ran {
            status: output.status.into_raw(),
            stdout: output.stdout,
            stderr: output.stderr,
            timed_out,
            wall_time,
            laws,
            coverage: coverage_report,
        })
    }
}

/// How a test request's launches may overlap: at most `jobs` at once, and an
/// exclusive unit runs alone.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Plan {
    pub jobs: usize,
    pub exclusive: Vec<bool>,
}

/// The driver side of a test session: the launch a unit needs next, given
/// the run of its previous launch (None to start it); None once the unit is
/// done.
pub(crate) trait Steps {
    fn step(
        &mut self,
        unit: usize,
        ran: Option<Result<Ran, String>>,
    ) -> io::Result<Option<LaunchSpec>>;
}

/// Run every unit of `plan` to its end, launching here what `steps` asks for.
/// One-shot and resident requests share this scheduler.
pub(crate) fn drive(plan: &Plan, steps: &mut impl Steps) -> io::Result<()> {
    type Done = (usize, Result<Ran, String>);
    let (completed, results) = std::sync::mpsc::channel::<Done>();
    let run = |spec: LaunchSpec| spec.run().map_err(|error| error.to_string());
    let mut running = 0;
    // Step `unit` with `ran` and run its launches: alone (inline), or on a
    // thread of its own.
    let mut advance = |unit: usize,
                       mut ran: Option<Result<Ran, String>>,
                       alone: bool,
                       running: &mut usize|
     -> io::Result<()> {
        while let Some(spec) = steps.step(unit, ran.take())? {
            if alone {
                ran = Some(run(spec));
                continue;
            }
            let completed = completed.clone();
            std::thread::Builder::new().spawn(move || {
                // A panic is a stop of this run, not of the session.
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(spec)))
                    .unwrap_or_else(|_| Err("the test runner panicked".into()));
                let _ = completed.send((unit, ran));
            })?;
            *running += 1;
            break;
        }
        Ok(())
    };
    for (unit, &exclusive) in plan.exclusive.iter().enumerate() {
        let alone = exclusive || plan.jobs <= 1;
        let slots = if alone { 0 } else { plan.jobs - 1 };
        while running > slots {
            let (done, ran) = results.recv().expect("a running launch reports");
            running -= 1;
            advance(done, Some(ran), false, &mut running)?;
        }
        advance(unit, None, alone, &mut running)?;
    }
    while running > 0 {
        let (done, ran) = results.recv().expect("a running launch reports");
        running -= 1;
        advance(done, Some(ran), false, &mut running)?;
    }
    Ok(())
}

/// PROG-4: each test run starts in a working directory of its own, empty
/// when it starts; a relative directory grant resolves against it.
fn private_directory(command: &mut Command) -> io::Result<tempfile::TempDir> {
    let directory = tempfile::Builder::new().prefix("hirsel-test-").tempdir()?;
    command.current_dir(directory.path());
    Ok(directory)
}

fn execute(
    command: &mut Command,
    timeout: Option<Duration>,
) -> io::Result<(Output, bool, Duration)> {
    let start = Instant::now();
    let (output, timed_out) = capture(command, timeout)?;
    Ok((output, timed_out, start.elapsed()))
}

pub(crate) fn capture(
    command: &mut Command,
    timeout: Option<Duration>,
) -> io::Result<(Output, bool)> {
    let Some(timeout) = timeout else {
        return command.output().map(|output| (output, false));
    };
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let started = Instant::now();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    std::thread::scope(|scope| {
        let read = |stream: &mut dyn Read| {
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes)?;
            Ok::<_, io::Error>(bytes)
        };
        let out = scope.spawn(move || read(&mut stdout));
        let err = scope.spawn(move || read(&mut stderr));
        let (status, timed_out) = loop {
            if let Some(status) = child.try_wait()? {
                break (status, false);
            }
            if started.elapsed() >= timeout {
                // Only the process this test runner spawned is terminated.
                child.kill()?;
                break (child.wait()?, true);
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        Ok((
            Output {
                status,
                stdout: out
                    .join()
                    .map_err(|_| io::Error::other("test stdout reader panicked"))??,
                stderr: err
                    .join()
                    .map_err(|_| io::Error::other("test stderr reader panicked"))??,
            },
            timed_out,
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_deadline_retains_output_and_does_not_stop_the_next_run() {
        let deadline = Duration::from_millis(50);
        let (output, timed_out, wall_time) = execute(
            Command::new("sh").args(["-c", "printf before; exec sleep 60"]),
            Some(deadline),
        )
        .unwrap();
        assert!(timed_out);
        assert!(wall_time >= deadline);
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"before");
        let (output, timed_out, _) = execute(
            Command::new("sh").args(["-c", "printf after"]),
            Some(Duration::from_secs(5)),
        )
        .unwrap();
        assert!(!timed_out);
        assert!(output.status.success());
        assert_eq!(output.stdout, b"after");
    }
}
