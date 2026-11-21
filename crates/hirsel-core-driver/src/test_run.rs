//! PROG-4: a checked test uses the ordinary entry ABI, in a fresh process. A run
//! stop can end that process without ending another test's run.
use crate::{CheckProduct, NativeOptions, NativeStop};
use hirsel_core_ids::{ContentHash, DefPath};
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::num::NonZeroUsize;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Execution inputs are separate from the target-independent checking products
/// and target-specific object/link products. Cross execution is always explicit.
#[derive(Clone, Debug, Default)]
pub enum Runner {
    #[default]
    Native,
    Qemu {
        emulator: std::path::PathBuf,
        sysroot: std::path::PathBuf,
    },
}
impl Runner {
    /// Validate the externally selected execution tools before compiling a run.
    pub fn qemu(
        emulator: &std::path::Path,
        sysroot: &std::path::Path,
        target: &str,
    ) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let emulator = std::fs::canonicalize(emulator).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("execution emulator {}: {e}", emulator.display()),
            )
        })?;
        if !emulator.is_file() || emulator.metadata()?.permissions().mode() & 0o111 == 0 {
            return Err(io::Error::other(format!(
                "execution emulator {} is not an executable file",
                emulator.display()
            )));
        }
        let sysroot = std::fs::canonicalize(sysroot).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("runtime sysroot {}: {e}", sysroot.display()),
            )
        })?;
        let interpreter = match hirsel_core_layout::Target::from_triple(target) {
            Some(hirsel_core_layout::Target::Aarch64Linux) => "lib/ld-linux-aarch64.so.1",
            Some(hirsel_core_layout::Target::X86_64Linux) => "lib64/ld-linux-x86-64.so.2",
            None => {
                return Err(io::Error::other(format!(
                    "unsupported execution target {target}"
                )));
            }
        };
        if !sysroot.is_dir() || !sysroot.join(interpreter).is_file() {
            return Err(io::Error::other(format!(
                "runtime sysroot {} has no {interpreter}",
                sysroot.display()
            )));
        }
        Ok(Self::Qemu { emulator, sysroot })
    }

    pub fn command(&self, executable: &std::path::Path) -> Command {
        match self {
            Self::Native => Command::new(executable),
            Self::Qemu { emulator, sysroot } => {
                let mut command = Command::new(emulator);
                command.arg("-L").arg(sysroot).arg(executable);
                command
            }
        }
    }
}

#[derive(Debug)]
pub struct TestOutcome {
    pub definition: DefPath,
    pub output: Output,
    pub timed_out: bool,
    /// Time spent launching, running and collecting the child or replay pair.
    pub wall_time: Duration,
}
impl TestOutcome {
    /// PROG-3 maps every Error, failure, stop and not-started outcome to nonzero.
    pub fn passed(&self) -> bool {
        !self.timed_out && self.output.status.success()
    }
}

pub(crate) fn affinity_jobs() -> io::Result<NonZeroUsize> {
    let mut mask = vec![0usize; 16];
    loop {
        // SAFETY: The kernel can write up to the allocated byte length to mask.
        let status = unsafe {
            libc::sched_getaffinity(
                0,
                std::mem::size_of_val(mask.as_slice()),
                mask.as_mut_ptr().cast(),
            )
        };
        if status == 0 {
            let count = mask.iter().map(|word| word.count_ones() as usize).sum();
            return NonZeroUsize::new(count)
                .ok_or_else(|| io::Error::other("empty CPU affinity mask"));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINVAL) {
            return Err(error);
        }
        // A fixed cpu_set_t can be too small for the kernel's CPU mask.
        mask.resize(mask.len() * 2, 0);
    }
}

type TestResult = Result<TestOutcome, NativeStop>;

pub(crate) struct TestBatch<'a> {
    pub jobs: NonZeroUsize,
    pub timeout: Option<Duration>,
    pub runner: &'a Runner,
    pub replay: bool,
    pub expect_verdict: Option<&'a str>,
}

impl TestBatch<'_> {
    pub(crate) fn run(
        self,
        product: &CheckProduct,
        tests: &[DefPath],
        options: &NativeOptions,
        mut report: impl FnMut(&DefPath, TestResult),
    ) {
        let output = |test: &DefPath| {
            options
                .output
                .join(crate::request::hex(ContentHash::of_bytes(
                    test.to_string().as_bytes(),
                )))
        };
        let mut test_options = options.clone();
        if self.jobs.get() == 1
            || tests.len() <= 1
            || self.replay
            || options.recording
            || options.replay.is_some()
            || options.logical_charge_report.is_some()
            || !matches!(self.runner, Runner::Native)
            || std::env::var_os("HIRSEL_CONTROL_FD").is_some()
        {
            for test in tests {
                test_options.output = output(test);
                let result = if self.replay {
                    product.run_test_with_replay(
                        test,
                        &test_options,
                        self.timeout,
                        self.runner,
                        self.expect_verdict,
                    )
                } else {
                    product.run_test_with_runner(test, &test_options, self.timeout, self.runner)
                };
                report(test, result);
            }
            return;
        }

        let mut next_row = 0;
        let mut rows = BTreeMap::new();
        let mut complete = |index, result| {
            rows.insert(index, result);
            while let Some(result) = rows.remove(&next_row) {
                report(&tests[next_row], result);
                next_row += 1;
            }
        };

        std::thread::scope(|scope| {
            let (completed, results) = std::sync::mpsc::channel();
            let unwrap = |outcome: std::thread::Result<TestResult>| {
                outcome.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            };
            let mut running = 0;
            for (index, test) in tests.iter().enumerate() {
                let exclusive = product.test_needs_exclusive_run(test, options);
                let slots = if exclusive { 0 } else { self.jobs.get() - 1 };
                while running > slots {
                    let (index, outcome) =
                        results.recv().expect("running child reports its result");
                    complete(index, unwrap(outcome));
                    running -= 1;
                }

                test_options.output = output(test);
                if exclusive {
                    complete(
                        index,
                        product.run_test_with_runner(
                            test,
                            &test_options,
                            self.timeout,
                            self.runner,
                        ),
                    );
                    continue;
                }
                let mut command = match product.test_command(test, &test_options, self.runner) {
                    Ok(command) => command,
                    Err(error) => {
                        complete(index, Err(error));
                        continue;
                    }
                };
                let completed = completed.clone();
                let timeout = self.timeout;
                let waiter = std::thread::Builder::new().spawn_scoped(scope, move || {
                    // Report panics too so the caller can unwind while the
                    // scope joins the remaining children.
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        test_outcome(test, &mut command, timeout)
                    }));
                    let _ = completed.send((index, outcome));
                });
                match waiter {
                    Ok(_) => running += 1,
                    Err(error) => complete(index, Err(error.into())),
                }
            }
            drop(completed);
            for (index, outcome) in results {
                complete(index, unwrap(outcome));
            }
        });
    }
}

impl CheckProduct {
    /// Tests declared in this request's source modules. Published dependencies
    /// retain their reference restrictions but are not rediscovered as local tests.
    pub fn tests(&self) -> impl Iterator<Item = &DefPath> {
        self.test_entries.keys()
    }

    /// The caller supplies this test's root limit and grants explicitly. Nothing
    /// here chooses a default, shares a run, or stops the caller's remaining tests.
    pub fn run_test(
        &self,
        test: &DefPath,
        options: &NativeOptions,
    ) -> Result<TestOutcome, NativeStop> {
        self.run_test_with_timeout(test, options, None)
    }

    /// An optional execution-only deadline. Checking, compiling and linking are
    /// outside this bound; a timed-out test fails without ending later runs.
    pub fn run_test_with_timeout(
        &self,
        test: &DefPath,
        options: &NativeOptions,
        timeout: Option<Duration>,
    ) -> Result<TestOutcome, NativeStop> {
        self.run_test_with_runner(test, options, timeout, &Runner::Native)
    }

    /// Run a checked test with explicit native or emulator execution inputs.
    pub fn run_test_with_runner(
        &self,
        test: &DefPath,
        options: &NativeOptions,
        timeout: Option<Duration>,
        runner: &Runner,
    ) -> Result<TestOutcome, NativeStop> {
        test_outcome(
            test,
            &mut self.test_command(test, options, runner)?,
            timeout,
        )
    }

    fn test_command(
        &self,
        test: &DefPath,
        options: &NativeOptions,
        runner: &Runner,
    ) -> Result<Command, NativeStop> {
        if !self.test_entries.contains_key(test) {
            return Err(NativeStop::Entry(format!(
                "{test} is not a discovered @test"
            )));
        }
        let built = self.build(test, options)?;
        Ok(runner.command(&std::fs::canonicalize(&built.executable)?))
    }

    fn test_needs_exclusive_run(&self, test: &DefPath, options: &NativeOptions) -> bool {
        let Some(Ok(entry)) = self.test_entries.get(test) else {
            return false;
        };
        let grants = |kind| options.grant_mask & (1 << crate::grant_kind(kind).unwrap()) != 0;
        entry
            .parameters
            .iter()
            .any(|param| match param.ty.as_str() {
                // Unbound grants can open host handles during startup, but only a
                // bound parameter can use one. Drain even unbound runs before
                // starting a test that could change another run's startup inputs.
                "core@1/core/Dir" => !options.directories.is_empty(),
                "core@1/core/Net" => grants("Net"),
                "core@1/core/Stdin" => grants("Stdin"),
                "core@1/core/Secret" => !options.secret_grants.is_empty(),
                "core@1/core/Clock"
                | "core@1/core/Random"
                | "core@1/core/Stdout"
                | "core@1/core/Stderr"
                | "core@1/core/Env"
                | "core@1/core/Process"
                | "core@1/core/Tasks" => false,
                _ => true,
            })
    }

    /// Run a checked test in the recording build with recording on, require it to
    /// pass without truncation, then replay that recording and require `Sealed`
    /// (or `expect_verdict` when given).
    pub fn run_test_with_replay(
        &self,
        test: &DefPath,
        options: &NativeOptions,
        timeout: Option<Duration>,
        runner: &Runner,
        expect_verdict: Option<&str>,
    ) -> Result<TestOutcome, NativeStop> {
        use std::os::unix::fs::PermissionsExt;
        if !self.test_entries.contains_key(test) {
            return Err(NativeStop::Entry(format!(
                "{test} is not a discovered @test"
            )));
        }
        let tape_dir = tempfile::tempdir()?;
        std::fs::set_permissions(tape_dir.path(), std::fs::Permissions::from_mode(0o700))?;
        let tape_path = tape_dir.path().join("test.tape");
        let mut record_options = options.clone();
        record_options.recording = true;
        record_options.replay = None;
        let built = self.build(test, &record_options)?;
        let mut cmd = runner.command(&std::fs::canonicalize(&built.executable)?);
        cmd.env("HIRSEL_RECORD", &tape_path);
        let (output, timed_out, wall_time) = execute(&mut cmd, timeout)?;
        if timed_out || !output.status.success() {
            return Ok(TestOutcome {
                definition: test.clone(),
                output,
                timed_out,
                wall_time,
            });
        }
        let record_stderr = String::from_utf8_lossy(&output.stderr);
        if record_stderr.contains("record: truncated at ") {
            return Err(NativeStop::Entry(record_stderr.trim().to_owned()));
        }
        let mut replay_options = options.clone();
        replay_options.recording = true;
        replay_options.grant_mask = 0;
        replay_options.directories.clear();
        replay_options.net_rules.clear();
        replay_options.secret_grants.clear();
        replay_options.output = options.output.with_extension("replay");
        replay_options.replay = Some(crate::ReplayPlan {
            path: tape_path
                .to_str()
                .ok_or_else(|| NativeStop::Entry("tape path is not UTF-8".into()))?
                .to_owned(),
            diagnose: false,
        });
        let replay_built = self.build(test, &replay_options)?;
        let (replay_output, replay_timed_out, replay_wall_time) = execute(
            &mut runner.command(&std::fs::canonicalize(&replay_built.executable)?),
            timeout,
        )?;
        let wall_time = wall_time + replay_wall_time;
        if replay_timed_out {
            return Ok(TestOutcome {
                definition: test.clone(),
                output: replay_output,
                timed_out: true,
                wall_time,
            });
        }
        let expected = expect_verdict.unwrap_or("Sealed");
        if expected == "Sealed" {
            return Ok(TestOutcome {
                definition: test.clone(),
                output: replay_output,
                timed_out: false,
                wall_time,
            });
        }
        let replay_stderr = String::from_utf8_lossy(&replay_output.stderr);
        let prefix = format!("replay: {expected}");
        if replay_output.status.code() == Some(1)
            && replay_stderr.lines().any(|line| line.starts_with(&prefix))
        {
            return Ok(TestOutcome {
                definition: test.clone(),
                output,
                timed_out: false,
                wall_time,
            });
        }
        Err(NativeStop::Entry(format!(
            "expected replay verdict `{expected}`, got status {:?}: {}",
            replay_output.status.code(),
            replay_stderr.trim()
        )))
    }
}

fn test_outcome(test: &DefPath, command: &mut Command, timeout: Option<Duration>) -> TestResult {
    let (output, timed_out, wall_time) = execute(command, timeout)?;
    Ok(TestOutcome {
        definition: test.clone(),
        output,
        timed_out,
        wall_time,
    })
}

fn execute(
    command: &mut Command,
    timeout: Option<Duration>,
) -> io::Result<(Output, bool, Duration)> {
    let start = Instant::now();
    let (output, timed_out) = capture(command, timeout)?;
    Ok((output, timed_out, start.elapsed()))
}

fn capture(command: &mut Command, timeout: Option<Duration>) -> io::Result<(Output, bool)> {
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
