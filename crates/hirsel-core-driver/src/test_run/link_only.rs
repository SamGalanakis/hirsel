//! Build the selected test executables without launching them.
use super::{Runner, TestLaunch, session::configure};
use crate::cli::{Invocation, OptionId as Opt};
use crate::package_dir::TestRuns;
use crate::{CheckProduct, NativeOptions, NativeStop};
use hirsel_core_ids::DefPath;
use hirsel_core_layout::Target;
use std::io;

pub(crate) fn requested(invocation: &Invocation) -> io::Result<bool> {
    if !invocation
        .options
        .iter()
        .any(|(id, _)| *id == Opt::LinkOnly)
    {
        return Ok(false);
    }
    for (option, _) in &invocation.options {
        if matches!(
            option,
            Opt::Laws
                | Opt::WriteLaws
                | Opt::LawsBaseline
                | Opt::Coverage
                | Opt::CoverageOut
                | Opt::CoverageBase
                | Opt::CoverageTargets
                | Opt::CoverageRuns
                | Opt::Sim
                | Opt::SimSeed
                | Opt::SimSeeds
                | Opt::SeedBase
                | Opt::PropertyCorpus
                | Opt::Replay
                | Opt::TestJobs
                | Opt::TestTimeout
                | Opt::TestCache
                | Opt::Rerun
                | Opt::Emulator
                | Opt::Sysroot
                | Opt::LogicalChargeReport
                | Opt::Metrics
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("test --link-only cannot be combined with {option:?}"),
            ));
        }
    }
    Ok(true)
}

pub(crate) fn link_only(
    product: &CheckProduct,
    options: &NativeOptions,
    runs: &TestRuns,
    tests: &[DefPath],
    json: bool,
) -> i32 {
    let mut rows = Vec::new();
    let mut failed = tests.is_empty();
    for test in tests {
        let link = || -> Result<(), NativeStop> {
            let options = configure(product, runs, options, test, false, false, false)?;
            let TestLaunch::Run(_, executable) = product.test_launch(
                test,
                &options,
                &Runner::Native,
                None,
                true,
                None,
                None,
                None,
            )?
            else {
                unreachable!("link-only tests have no pass cache");
            };
            match Target::from_triple(&options.target) {
                Some(Target::Aarch64Darwin | Target::Aarch64Ios | Target::Aarch64IosSim) => {
                    crate::native_debug::macho_structure(&executable)?
                }
                Some(Target::X86_64Linux | Target::Aarch64Linux) => {}
                None => {
                    return Err(NativeStop::Entry(format!(
                        "unsupported target {}",
                        options.target
                    )));
                }
            }
            Ok(())
        };
        let stopped = link().err().map(|error| error.to_string());
        failed |= stopped.is_some();
        if json {
            rows.push(serde_json::json!({
                "definition": test.to_string(),
                "linked": stopped.is_none(),
                "stopped": stopped,
            }));
        } else if let Some(error) = stopped {
            println!("link failed {test}: {error}");
        } else {
            println!("linked {test}");
        }
    }
    if json {
        println!("{}", serde_json::json!({"linked": !failed, "tests": rows}));
    } else if tests.is_empty() {
        eprintln!("no tests found");
    }
    if tests.is_empty() {
        2
    } else {
        i32::from(failed)
    }
}
