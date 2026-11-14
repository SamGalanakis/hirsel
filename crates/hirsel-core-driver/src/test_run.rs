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
        let (output, timed_out) = execute(
            &mut runner.command(&std::fs::canonicalize(&built.executable)?),
            timeout,
        )?;
        Ok(TestOutcome {
            definition: test.clone(),
            output,
            timed_out,
        })
    }
}

fn execute(command: &mut Command, timeout: Option<Duration>) -> io::Result<(Output, bool)> {
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
        let (output, timed_out) = execute(
            Command::new("sh").args(["-c", "printf before; exec sleep 60"]),
            Some(Duration::from_millis(50)),
        )
        .unwrap();
        assert!(timed_out);
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"before");
        let (output, timed_out) = execute(
            Command::new("sh").args(["-c", "printf after"]),
            Some(Duration::from_secs(5)),
        )
        .unwrap();
        assert!(!timed_out);
        assert!(output.status.success());
        assert_eq!(output.stdout, b"after");
    }
}
