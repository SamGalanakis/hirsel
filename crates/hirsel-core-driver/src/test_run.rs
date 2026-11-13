//! PROG-4: a checked test uses the ordinary entry ABI, in a fresh process. A run
//! stop can end that process without ending another test's run.
use crate::{CheckProduct, NativeOptions, NativeStop};
use hirsel_core_ids::DefPath;
use std::process::{Command, Output};

#[derive(Debug)]
pub struct TestOutcome {
    pub definition: DefPath,
    pub output: Output,
}
impl TestOutcome {
    /// PROG-3 maps every Error, failure, stop and not-started outcome to nonzero.
    pub fn passed(&self) -> bool {
        self.output.status.success()
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
        if !self.test_entries.contains_key(test) {
            return Err(NativeStop::Entry(format!(
                "{test} is not a discovered @test"
            )));
        }
        let built = self.build(test, options)?;
        let output = Command::new(std::fs::canonicalize(&built.executable)?).output()?;
        Ok(TestOutcome {
            definition: test.clone(),
            output,
        })
    }
}
