//! PROG-4: a checked test uses the ordinary entry ABI, in a fresh process. A run
//! stop can end that process without ending another test's run.
use crate::native::EntryDecl;
use crate::property_corpus;
use crate::{CheckProduct, NativeOptions, NativeStop};
use hirsel_core_ids::DefPath;

mod cache;
mod coverage;
mod launch;
mod laws;
mod replay;
mod session;
mod slots;
pub use cache::TestCache;
use cache::test_key;
use coverage::CoverageCapture;
pub(crate) use coverage::{Flags, RunCoverage, Session, header, records};
use launch::Observe;
pub(crate) use launch::{LaunchSpec, Plan, Ran, Steps, drive};
pub(crate) use laws::{LawReport, LawRow, class_rank};
pub(crate) use session::Prepare;
pub use session::TestSession;
use slots::InheritedSlots;
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

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
const SEED_ENV: &str = "HIRSEL_TEST_SIM_SEED";

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
            // Darwin images run only on a Mac, never under an emulator.
            Some(hirsel_core_layout::Target::Aarch64Darwin) | None => {
                return Err(io::Error::other(format!(
                    "no emulated execution for target {target}"
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

    fn test_command(&self, executable: &Path) -> Command {
        let mut command = self.command(executable);
        // Tests are standalone runs. A place launcher owns its descriptor
        // numbers, parent PID, scratch path, socket activation and tape.
        for name in [
            "HIRSEL_HOST_PID",
            "HIRSEL_CONTROL_FD",
            "HIRSEL_HOST_CONTROL_FD",
            "HIRSEL_SCRATCH",
            "LISTEN_PID",
            "LISTEN_FDS",
            "LISTEN_FDNAMES",
            "LISTEN_PIDFDID",
            "HIRSEL_RECORD",
            "HIRSEL_RECORD_ID",
            "HIRSEL_LAWS",
            coverage::ENV,
            SEED_ENV,
            property_corpus::ENV,
        ] {
            command.env_remove(name);
        }
        command
    }
}

#[derive(Debug)]
pub struct TestOutcome {
    pub definition: DefPath,
    pub output: Output,
    pub timed_out: bool,
    /// Time spent launching, running and collecting the child or replay pair.
    /// A cached result has no child time.
    pub wall_time: Duration,
    pub(crate) corpus: Option<&'static str>,
    pub(crate) laws: LawReport,
    pub(crate) coverage: RunCoverage,
}

impl TestOutcome {
    /// PROG-3 maps every Error, failure, stop and not-started outcome to nonzero.
    pub fn passed(&self) -> bool {
        !self.timed_out && self.output.status.success()
    }
}

/// What a run's nonzero exit status means (PROG-3), for the driver's line on
/// a run or test that did not succeed, after any line the run wrote for it.
pub(crate) fn outcome_text(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code @ 1..=100), _) => format!("returned an error, exit status {code}"),
        (Some(101), _) => "failed, exit status 101".into(),
        (Some(102), _) => "stopped, exit status 102".into(),
        (Some(103), _) => "did not start, exit status 103".into(),
        (Some(code), _) => format!("ended with exit status {code}"),
        (None, Some(signal)) => format!("was terminated by signal {signal}"),
        (None, None) => "ended without an exit status".into(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    Cached,
    Executed,
}

impl Source {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Cached => "cached",
            Self::Executed => "executed",
        }
    }
}

/// PROG-4: of its run's grants a test is granted only those its parameters
/// take, so a test without parameters is granted nothing and a grant one
/// test needs never reaches another test's launch.
pub(crate) fn grant_parameters(decl: &EntryDecl, options: &mut NativeOptions) {
    let kind = |ty: &str| ty.strip_prefix("core@1/core/").and_then(crate::grant_kind);
    let named = |name: &str| crate::grant_kind(name).unwrap();
    let mut taken = 0u64;
    let mut directories = 0;
    let mut exec = 0;
    for parameter in &decl.parameters {
        if let Some(kind) = kind(&parameter.ty) {
            taken |= 1 << kind;
            directories += usize::from(kind == named("Dir"));
            exec += usize::from(kind == named("Exec"));
        }
    }
    options.grant_mask &= taken;
    options.directories.truncate(directories);
    options.exec_grants.truncate(exec);
    if taken & (1 << named("Net")) == 0 {
        options.net_rules.clear();
        options.unix_roots.clear();
    }
    if taken & (1 << named("Env")) == 0 {
        options.env_patterns = None;
    }
    let takes_named = |ty, name: &str| {
        decl.parameters
            .iter()
            .any(|p| kind(&p.ty) == Some(named(ty)) && p.name == name)
    };
    options
        .secret_grants
        .retain(|grant| takes_named("Secret", &grant.parameter));
    options
        .cred_grants
        .retain(|grant| takes_named("Cred", &grant.parameter));
    options.foreign_grants.retain(|interface| {
        decl.parameters
            .iter()
            .any(|param| param.foreign_interface() == Some(interface))
    });
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

enum TestLaunch {
    Cached(TestOutcome),
    /// The launch and the executable it runs.
    Run(LaunchSpec, PathBuf),
}

pub(crate) fn parameterless_test(decl: &Result<EntryDecl, String>) -> Option<&EntryDecl> {
    decl.as_ref().ok().filter(|decl| decl.parameters.is_empty())
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
        self.run_test_reusing(test, options, timeout, runner, None, false)
            .map(|(outcome, _)| outcome)
    }

    /// Reuse a passing native test when its checked code and launch inputs match.
    pub fn run_test_reusing(
        &self,
        test: &DefPath,
        options: &NativeOptions,
        timeout: Option<Duration>,
        runner: &Runner,
        cache: Option<&TestCache>,
        rerun: bool,
    ) -> Result<(TestOutcome, Source), NativeStop> {
        let context = session::Context {
            runner,
            timeout,
            cache,
            rerun,
            ..Default::default()
        };
        session::Unit::new(test.clone(), None, Ok(options.clone())).run_here(self, &context)
    }

    /// A test's launch, or its stored pass. `laws` asks for law rows, with
    /// the plain runtime a foreign test builds with (FFI-8).
    #[allow(clippy::too_many_arguments)]
    fn test_launch(
        &self,
        test: &DefPath,
        options: &NativeOptions,
        runner: &Runner,
        cache: Option<&TestCache>,
        rerun: bool,
        seed: Option<u64>,
        laws: Option<Option<&Path>>,
        timeout: Option<Duration>,
    ) -> Result<TestLaunch, NativeStop> {
        let Some(decl) = self.test_entries.get(test) else {
            return Err(NativeStop::Entry(format!(
                "{test} is not a discovered @test"
            )));
        };
        let parameterless = parameterless_test(decl);
        // An unused launch grant could otherwise touch host state before the
        // test starts.
        let mut options = self.granted(test, options);
        let slots = InheritedSlots::take(&mut options);
        let mut reached = if options.foreign_grants.is_empty() || options.recording {
            self.reachable_for(test, &options)?
        } else {
            self.reachable(test, options.instance_limit)?
        };
        let foreign = laws.is_some() && reached.foreign_functions().next().is_some();
        if foreign {
            options.recording = false;
            if let Some(runtime) = laws.flatten() {
                options.runtime = runtime.to_owned();
            }
            reached = if options.foreign_grants.is_empty() {
                self.reachable_for(test, &options)?
            } else {
                self.reachable(test, options.instance_limit)?
            };
        }
        let options = &options;
        let pass = cache
            .filter(|_| {
                matches!(runner, Runner::Native)
                    && reached.foreign_functions().next().is_none()
                    && (!options.recording || laws.is_some())
                    && options.replay.is_none()
                    && options.logical_charge_report.is_none()
            })
            .zip(parameterless)
            .and_then(|(cache, decl)| {
                let sites = crate::native::recipe_build::test_site_digest(
                    self,
                    test,
                    options.instance_limit,
                )
                .ok()?;
                test_key(test, decl, &reached, sites, options).map(|key| (cache, key))
            });
        if !rerun
            && let Some((cache, key)) = pass
            && let Some((output, laws, coverage)) =
                cache.get(key, laws.is_some(), options.coverage.is_some())
        {
            return Ok(TestLaunch::Cached(TestOutcome {
                definition: test.clone(),
                output,
                timed_out: false,
                wall_time: Duration::ZERO,
                corpus: None,
                laws,
                coverage,
            }));
        }
        let built = if options.foreign_grants.is_empty() || options.recording {
            self.build_reachable(test, &reached, options)?
        } else {
            let store = crate::code_store::CodeStore::cache(&options.cas, "code")?;
            let publication = self.publish_release(
                test,
                &reached,
                options,
                &store,
                hirsel_core_ids::launch::DEFAULT_N_PLACE,
            )?;
            let lines = crate::native::recipe_build::foreign_grant_lines(
                publication.recipe(),
                publication.root(),
            )
            .collect::<Vec<_>>();
            let built = publication.assemble_with(options, reached.counters, Some(self), None)?;
            for line in lines {
                eprintln!("{line}");
            }
            built
        };
        if let Some(map) = &built.coverage {
            coverage::write_map(self, test, options, map);
        }
        let executable = std::fs::canonicalize(&built.executable)?;
        let mut command = runner.test_command(&executable);
        for grant in &options.exec_grants {
            grant.approve(&mut command);
        }
        if let Some(seed) = seed {
            command.env(SEED_ENV, seed.to_string());
        }
        let observe = laws.is_some() && !foreign;
        let sites = observe
            .then(|| laws::Sites::for_test(self, test, options.instance_limit).ok())
            .flatten();
        let pass = pass.filter(|_| !observe || sites.is_some());
        let observe = Observe {
            laws: sites,
            coverage: options
                .coverage
                .is_some()
                .then(|| built.coverage.as_deref().cloned()),
            pass: pass.map(|(cache, key)| (cache.root().to_owned(), key)),
            foreign,
        };
        let spec = LaunchSpec::new(&command, slots, observe, timeout);
        Ok(TestLaunch::Run(spec, executable))
    }

    /// `options` reduced to what `test`'s parameters take.
    fn granted(&self, test: &DefPath, options: &NativeOptions) -> NativeOptions {
        let mut options = options.clone();
        if let Some(Ok(decl)) = self.test_entries.get(test) {
            grant_parameters(decl, &mut options);
        }
        options
    }

    fn test_needs_exclusive_run(&self, test: &DefPath, options: &NativeOptions) -> bool {
        let Some(Ok(entry)) = self.test_entries.get(test) else {
            return false;
        };
        let options = &self.granted(test, options);
        let grants = |kind| options.grant_mask & (1 << crate::grant_kind(kind).unwrap()) != 0;
        entry
            .parameters
            .iter()
            .any(|param| match param.ty.as_str() {
                // Unbound grants can open host handles during startup, but only a
                // bound parameter can use one. Drain even unbound runs before
                // starting a test that could change another run's startup inputs.
                // A directory inside the run's own working directory is
                // the run's alone.
                "core@1/core/Dir" => !options
                    .directories
                    .iter()
                    .all(|d| crate::package_dir::private_directory(d)),
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
}
