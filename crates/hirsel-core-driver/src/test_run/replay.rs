//! A recorded test run and its replay (PROG-4, `--replay`): the recording
//! build's launch, then, once it passed untruncated, the replay build's.
use super::launch::{LaunchSpec, Observe};
use super::{Runner, TestOutcome, laws::Sites};
use crate::{CheckProduct, NativeOptions, NativeStop};
use hirsel_core_ids::DefPath;
use std::time::Duration;

/// A recording launch's tape and the options its replay builds from.
pub(super) struct Recording {
    _directory: tempfile::TempDir,
    tape: std::path::PathBuf,
    options: NativeOptions,
}

impl CheckProduct {
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
        let context = super::session::Context {
            runner,
            timeout,
            replay: true,
            expect_verdict,
            ..Default::default()
        };
        super::session::Unit::new(test.clone(), None, Ok(options.clone()))
            .run_here(self, &context)
            .map(|(outcome, _)| outcome)
    }

    /// The recording build's launch, with `HIRSEL_RECORD` naming its tape.
    pub(super) fn replay_record(
        &self,
        test: &DefPath,
        options: &NativeOptions,
        runner: &Runner,
        laws: bool,
        timeout: Option<Duration>,
    ) -> Result<(LaunchSpec, Recording), NativeStop> {
        use std::os::unix::fs::PermissionsExt;
        if !self.test_entries.contains_key(test) {
            return Err(NativeStop::Entry(format!(
                "{test} is not a discovered @test"
            )));
        }
        let directory = tempfile::tempdir()?;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        let tape = directory.path().join("test.tape");
        let mut record_options = self.granted(test, options);
        record_options.recording = true;
        record_options.replay = None;
        let slots = super::InheritedSlots::take(&mut record_options);
        let built = self.build(test, &record_options)?;
        let mut cmd = runner.test_command(&std::fs::canonicalize(&built.executable)?);
        for grant in &record_options.exec_grants {
            grant.approve(&mut cmd);
        }
        cmd.env("HIRSEL_RECORD", &tape);
        let observe = Observe {
            laws: laws
                .then(|| Sites::for_test(self, test, options.instance_limit).ok())
                .flatten(),
            // The profile is the recorded run's; the replay writes none.
            coverage: record_options
                .coverage
                .is_some()
                .then(|| built.coverage.as_deref().cloned()),
            pass: None,
            foreign: false,
        };
        let spec = LaunchSpec::new(&cmd, slots, observe, timeout);
        Ok((
            spec,
            Recording {
                _directory: directory,
                tape,
                options: record_options,
            },
        ))
    }

    /// After the recording run `record`: its own outcome when it did not
    /// pass, else the replay build's launch, which reads `recording`'s tape.
    pub(super) fn replay_after_record(
        &self,
        test: &DefPath,
        recording: &Recording,
        record: TestOutcome,
        runner: &Runner,
        timeout: Option<Duration>,
    ) -> Result<Result<TestOutcome, (LaunchSpec, TestOutcome)>, NativeStop> {
        if !record.passed() {
            return Ok(Ok(record));
        }
        let record_stderr = String::from_utf8_lossy(&record.output.stderr);
        if record_stderr.contains("record: truncated at ") {
            return Err(NativeStop::Entry(record_stderr.trim().to_owned()));
        }
        let mut replay_options = recording.options.clone();
        crate::package_dir::Grants::default().apply(&mut replay_options);
        // A recorded test reaches only contained code (FFI-8), whose
        // Foreign[I] grant is a marker the tape does not carry (FFI-14).
        replay_options
            .foreign_grants
            .clone_from(&recording.options.foreign_grants);
        replay_options.output = replay_options.output.with_extension("replay");
        replay_options.replay = Some(crate::ReplayPlan {
            path: recording
                .tape
                .to_str()
                .ok_or_else(|| NativeStop::Entry("tape path is not UTF-8".into()))?
                .to_owned(),
            diagnose: false,
        });
        let replay_built = self.build(test, &replay_options)?;
        let replay = runner.test_command(&std::fs::canonicalize(&replay_built.executable)?);
        let observe = Observe {
            laws: None,
            coverage: None,
            pass: None,
            foreign: false,
        };
        // The caller keeps `recording`, and so the tape, until the replay ran.
        let spec = LaunchSpec::new(&replay, Default::default(), observe, timeout);
        Ok(Err((spec, record)))
    }
}

/// The replay's verdict: `Sealed` is the replay's own outcome; any other
/// expected verdict is the recording's outcome when the replay reported it.
pub(super) fn verdict(
    record: TestOutcome,
    replay: TestOutcome,
    expect_verdict: Option<&str>,
) -> Result<TestOutcome, NativeStop> {
    let wall_time = record.wall_time + replay.wall_time;
    if replay.timed_out || expect_verdict.unwrap_or("Sealed") == "Sealed" {
        return Ok(TestOutcome {
            wall_time,
            laws: record.laws,
            coverage: record.coverage,
            ..replay
        });
    }
    let expected = expect_verdict.unwrap();
    let replay_stderr = String::from_utf8_lossy(&replay.output.stderr);
    let prefix = format!("replay: {expected}");
    if replay.output.status.code() == Some(1)
        && replay_stderr.lines().any(|line| line.starts_with(&prefix))
    {
        return Ok(TestOutcome {
            wall_time,
            ..record
        });
    }
    Err(NativeStop::Entry(format!(
        "expected replay verdict `{expected}`, got status {:?}: {}",
        replay.output.status.code(),
        replay_stderr.trim()
    )))
}
