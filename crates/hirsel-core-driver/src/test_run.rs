//! PROG-4: a checked test uses the ordinary entry ABI, in a fresh process. A run
//! stop can end that process without ending another test's run.
use crate::{CheckProduct, NativeOptions, NativeStop};
use hirsel_core_ids::DefPath;
use std::io::{self, Read};
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
        if !self.test_entries.contains_key(test) {
            return Err(NativeStop::Entry(format!(
                "{test} is not a discovered @test"
            )));
        }
        let built = self.build(test, options)?;
        let (output, timed_out, wall_time) = execute(
            &mut runner.command(&std::fs::canonicalize(&built.executable)?),
            timeout,
        )?;
        Ok(TestOutcome {
            definition: test.clone(),
            output,
            timed_out,
            wall_time,
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
