//! A recorded test run and its replay (PROG-4, `--replay`).
use super::{
    CoverageCapture, LawCapture, LawReport, RunCoverage, Runner, TestOutcome, execute,
    private_directory,
};
use crate::{CheckProduct, NativeOptions, NativeStop};
use hirsel_core_ids::DefPath;
use std::time::Duration;

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
        self.run_test_replay(test, options, timeout, runner, expect_verdict, false)
    }

    pub(super) fn run_test_replay(
        &self,
        test: &DefPath,
        options: &NativeOptions,
        timeout: Option<Duration>,
        runner: &Runner,
        expect_verdict: Option<&str>,
        laws: bool,
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
        let mut record_options = self.granted(test, options);
        record_options.recording = true;
        record_options.replay = None;
        let built = self.build(test, &record_options)?;
        let mut cmd = runner.test_command(&std::fs::canonicalize(&built.executable)?);
        for grant in &record_options.exec_grants {
            grant.approve(&mut cmd);
        }
        cmd.env("HIRSEL_RECORD", &tape_path);
        let observation = laws
            .then(|| LawCapture::attach(&mut cmd, self, test, options.instance_limit))
            .flatten();
        // The profile is the recorded run's; the replay writes none.
        let capture = record_options
            .coverage
            .is_some()
            .then(|| CoverageCapture::attach(&mut cmd, built.coverage.as_ref()));
        let directory = private_directory(&mut cmd)?;
        let (output, timed_out, wall_time) = execute(&mut cmd, timeout)?;
        let laws = observation
            .as_ref()
            .map_or_else(LawReport::default, LawCapture::report);
        let coverage = capture
            .as_ref()
            .map_or_else(RunCoverage::default, CoverageCapture::report);
        drop(directory);
        if timed_out || !output.status.success() {
            return Ok(TestOutcome {
                definition: test.clone(),
                output,
                timed_out,
                wall_time,
                corpus: None,
                laws,
                coverage,
            });
        }
        let record_stderr = String::from_utf8_lossy(&output.stderr);
        if record_stderr.contains("record: truncated at ") {
            return Err(NativeStop::Entry(record_stderr.trim().to_owned()));
        }
        let mut replay_options = record_options;
        crate::package_dir::Grants::default().apply(&mut replay_options);
        replay_options.output = options.output.with_extension("replay");
        replay_options.replay = Some(crate::ReplayPlan {
            path: tape_path
                .to_str()
                .ok_or_else(|| NativeStop::Entry("tape path is not UTF-8".into()))?
                .to_owned(),
            diagnose: false,
        });
        let replay_built = self.build(test, &replay_options)?;
        let mut replay = runner.test_command(&std::fs::canonicalize(&replay_built.executable)?);
        let _directory = private_directory(&mut replay)?;
        let (replay_output, replay_timed_out, replay_wall_time) = execute(&mut replay, timeout)?;
        let wall_time = wall_time + replay_wall_time;
        if replay_timed_out {
            return Ok(TestOutcome {
                definition: test.clone(),
                output: replay_output,
                timed_out: true,
                wall_time,
                corpus: None,
                laws,
                coverage,
            });
        }
        let expected = expect_verdict.unwrap_or("Sealed");
        if expected == "Sealed" {
            return Ok(TestOutcome {
                definition: test.clone(),
                output: replay_output,
                timed_out: false,
                wall_time,
                corpus: None,
                laws,
                coverage,
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
                corpus: None,
                laws,
                coverage,
            });
        }
        Err(NativeStop::Entry(format!(
            "expected replay verdict `{expected}`, got status {:?}: {}",
            replay_output.status.code(),
            replay_stderr.trim()
        )))
    }
}
