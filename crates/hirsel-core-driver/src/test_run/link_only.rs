//! Build the selected test executables without launching them.
use super::{ReleaseLaunch, Runner, TestLaunch, session::configure};
use crate::cli::{CompilerOption as Opt, Request};
use crate::package_dir::TestRuns;
use crate::{CheckProduct, NativeOptions, NativeStop};
use hirsel_core_ids::DefPath;
use hirsel_core_layout::Target;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) fn requested(invocation: &Request<Opt>) -> io::Result<bool> {
    if !invocation.options.iter().any(|id| *id == Opt::LinkOnly) {
        return Ok(false);
    }
    for option in &invocation.options {
        let conflict = match option {
            Opt::Laws => "Laws",
            Opt::WriteLaws => "WriteLaws",
            Opt::LawsBaseline(_) => "LawsBaseline",
            Opt::Coverage => "Coverage",
            Opt::CoverageOut(_) => "CoverageOut",
            Opt::CoverageBase(_) => "CoverageBase",
            Opt::CoverageTargets(_) => "CoverageTargets",
            Opt::CoverageRuns => "CoverageRuns",
            Opt::Sim => "Sim",
            Opt::SimSeed(_) => "SimSeed",
            Opt::SimSeeds(_) => "SimSeeds",
            Opt::SeedBase(_) => "SeedBase",
            Opt::PropertyCorpus(_) => "PropertyCorpus",
            Opt::Replay => "Replay",
            Opt::TestJobs(_) => "TestJobs",
            Opt::TestTimeout(_) => "TestTimeout",
            Opt::TestCache(_) => "TestCache",
            Opt::Rerun => "Rerun",
            Opt::Emulator(_) => "Emulator",
            Opt::Sysroot(_) => "Sysroot",
            Opt::LogicalChargeReport(_) => "LogicalChargeReport",
            Opt::Metrics(_) => "Metrics",
            _ => continue,
        };
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("test --link-only cannot be combined with {conflict}"),
        ));
    }
    Ok(true)
}

/// Build each selected test. Release tests publish their recipes first; the
/// checked product is then dropped, so no assembly overlaps the checking
/// state (docs/internal/release-memory.md, slice 1).
pub(crate) fn link_only(
    product: Arc<CheckProduct>,
    options: &NativeOptions,
    runs: &TestRuns,
    tests: &[DefPath],
    json: bool,
) -> i32 {
    let staged = tests
        .iter()
        .map(|test| stage(&product, options, runs, test))
        .collect::<Vec<_>>();
    drop(product);
    let mut rows = Vec::new();
    let mut failed = tests.is_empty();
    for (test, staged) in tests.iter().zip(staged) {
        let stopped = staged.and_then(link).err().map(|error| error.to_string());
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

/// A test linked while the product was held, or its published release
/// recipe still to assemble.
enum Staged {
    Linked(PathBuf, String),
    Release(ReleaseLaunch),
}

fn stage(
    product: &CheckProduct,
    options: &NativeOptions,
    runs: &TestRuns,
    test: &DefPath,
) -> Result<Staged, NativeStop> {
    let options = configure(product, runs, options, test, false, false, false)?;
    if options.llvm_release() {
        return product
            .publish_link_only(test, &options)
            .map(Staged::Release);
    }
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
    Ok(Staged::Linked(executable, options.target))
}

fn link(staged: Staged) -> Result<(), NativeStop> {
    let (executable, target) = match staged {
        Staged::Linked(executable, target) => (executable, target),
        Staged::Release(release) => {
            let target = release.target().to_owned();
            let built = release.assemble()?;
            (std::fs::canonicalize(&built.executable)?, target)
        }
    };
    match Target::from_triple(&target) {
        Some(Target::Aarch64Darwin) => crate::native_debug::macho_structure(&executable)?,
        Some(Target::X86_64Linux | Target::Aarch64Linux) => {}
        Some(Target::Aarch64Ios | Target::Aarch64IosSim) | None => {
            return Err(NativeStop::Entry(format!("unsupported target {target}")));
        }
    }
    Ok(())
}
