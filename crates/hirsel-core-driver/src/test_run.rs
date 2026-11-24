//! PROG-4: a checked test uses the ordinary entry ABI, in a fresh process. A run
//! stop can end that process without ending another test's run.
use crate::native::EntryDecl;
use crate::property_corpus;
use crate::{CheckProduct, NativeOptions, NativeStop};
use hirsel_core_ids::{ContentHash, DefPath};

mod cache;
mod laws;
pub use cache::TestCache;
use cache::test_key;
use laws::LawCapture;
pub(crate) use laws::{LawReport, LawRow, class_rank};
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
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

/// PROG-4: each test run starts in a working directory of its own, empty
/// when it starts; a relative directory grant resolves against it.
fn private_directory(command: &mut Command) -> io::Result<tempfile::TempDir> {
    let directory = tempfile::Builder::new().prefix("hirsel-test-").tempdir()?;
    command.current_dir(directory.path());
    Ok(directory)
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

type TestResult = Result<(TestOutcome, Source), NativeStop>;

enum TestLaunch<'a> {
    Cached(TestOutcome),
    Run(TestProcess<'a>),
}

impl TestLaunch<'_> {
    fn finish(self, test: &DefPath, timeout: Option<Duration>) -> TestResult {
        match self {
            TestLaunch::Cached(outcome) => Ok((outcome, Source::Cached)),
            TestLaunch::Run(process) => process.run(test, timeout),
        }
    }
}

struct TestProcess<'a> {
    command: Command,
    executable: PathBuf,
    pass: Option<(&'a TestCache, ContentHash)>,
    /// Removed once the run is collected.
    _directory: tempfile::TempDir,
    laws: Option<LawCapture>,
    foreign: bool,
}

impl TestProcess<'_> {
    fn run(mut self, test: &DefPath, timeout: Option<Duration>) -> TestResult {
        let (output, timed_out, wall_time) = execute(&mut self.command, timeout)?;
        let outcome = TestOutcome {
            definition: test.clone(),
            output,
            timed_out,
            wall_time,
            corpus: None,
            laws: if self.foreign {
                LawReport::Foreign
            } else {
                self.laws
                    .as_ref()
                    .map_or_else(LawReport::default, LawCapture::report)
            },
        };
        if outcome.passed()
            && (self.laws.is_none() || matches!(outcome.laws, LawReport::Observed(_)))
            && let Some((cache, key)) = self.pass
        {
            cache.put(key, &outcome.output, &outcome.laws);
        }
        Ok((outcome, Source::Executed))
    }
}

pub(crate) struct TestBatch<'a> {
    pub runs: &'a crate::package_dir::TestRuns,
    pub jobs: NonZeroUsize,
    pub timeout: Option<Duration>,
    pub runner: &'a Runner,
    pub replay: bool,
    pub laws: bool,
    pub plain_runtime: Option<PathBuf>,
    pub expect_verdict: Option<&'a str>,
    pub cache: Option<&'a TestCache>,
    pub rerun: bool,
    pub seed: Option<u64>,
    pub corpus: Option<&'a property_corpus::Corpus>,
}

impl TestBatch<'_> {
    fn run_one(
        &self,
        product: &CheckProduct,
        test: &DefPath,
        options: &NativeOptions,
    ) -> TestResult {
        product
            .test_launch(
                test,
                options,
                self.runner,
                self.cache,
                self.rerun,
                self.seed,
                self.laws.then_some(self.plain_runtime.as_deref()),
            )?
            .finish(test, self.timeout)
    }

    fn run_corpus(
        &self,
        corpus: &property_corpus::Corpus,
        product: &CheckProduct,
        test: &DefPath,
        options: &NativeOptions,
    ) -> TestResult {
        corpus.check_grants(&product.granted(test, options).directories)?;
        let run = |seed, input: &str| {
            let TestLaunch::Run(mut process) = product.test_launch(
                test,
                options,
                self.runner,
                None,
                true,
                seed,
                self.laws.then_some(self.plain_runtime.as_deref()),
            )?
            else {
                unreachable!("corpus runs have no pass cache")
            };
            process.command.env(property_corpus::ENV, input);
            let executable = process.executable.clone();
            let (outcome, source) = process.run(test, self.timeout)?;
            Ok::<_, NativeStop>((outcome, source, executable))
        };
        let mut replay_time = Duration::ZERO;
        let stale = match corpus.read(test)? {
            property_corpus::SavedEntry::Missing => false,
            property_corpus::SavedEntry::Stale => {
                corpus.retire(test)?;
                true
            }
            property_corpus::SavedEntry::Current(saved) => {
                let (mut outcome, source, executable) = run(saved.launch_seed, &saved.input)?;
                match property_corpus::report(&outcome.output, outcome.timed_out, &executable) {
                    Some(property_corpus::Report::Stale) => {
                        corpus.retire(test)?;
                        replay_time = outcome.wall_time;
                        true
                    }
                    Some(property_corpus::Report::Failure(failure)) => {
                        outcome.corpus = Some(if saved.confirms(&failure) {
                            "confirmed"
                        } else {
                            corpus.write(test, saved.launch_seed, failure)?;
                            "changed-failure"
                        });
                        return Ok((outcome, source));
                    }
                    None => {
                        if outcome.passed() {
                            return Err(NativeStop::Entry(
                                "test did not verify its saved property corpus entry".into(),
                            ));
                        }
                        outcome.corpus = Some("unverified");
                        return Ok((outcome, source));
                    }
                }
            }
        };
        let (mut outcome, source, executable) = run(self.seed, "search")?;
        outcome.wall_time += replay_time;
        if let Some(property_corpus::Report::Failure(failure)) =
            property_corpus::report(&outcome.output, outcome.timed_out, &executable)
        {
            corpus.write(test, self.seed, failure)?;
            outcome.corpus = Some(if stale { "stale-and-saved" } else { "saved" });
        } else if stale {
            outcome.corpus = Some("stale");
        }
        Ok((outcome, source))
    }

    pub(crate) fn run(
        self,
        product: &CheckProduct,
        tests: &[DefPath],
        options: &NativeOptions,
        mut report: impl FnMut(&DefPath, TestResult),
    ) {
        // Each test's own run configuration; one test's never changes another's.
        let configure = |test: &DefPath| {
            let mut test_options = self.runs.options(test, options);
            let mut grant_env = |name| {
                let env = test_options
                    .env_patterns
                    .get_or_insert_with(Default::default);
                if !env.covers(name) {
                    env.extend(hirsel_core_net::EnvPatterns::parse(name).unwrap());
                }
                test_options.grant_mask |= 1 << crate::grant_kind("Env").unwrap();
            };
            if self.seed.is_some() || self.corpus.is_some() {
                grant_env(SEED_ENV);
            }
            if self.corpus.is_some() {
                grant_env(property_corpus::ENV);
            }
            if let Some(Ok(decl)) = product.test_entries.get(test) {
                let exec = decl
                    .parameters
                    .iter()
                    .filter(|p| p.ty == "core@1/core/Exec")
                    .count();
                if !self.replay {
                    for grant in test_options.exec_grants.iter_mut().take(exec) {
                        if !options
                            .exec_grants
                            .iter()
                            .any(|approved| approved.request.text() == grant.request.text())
                        {
                            return Err(NativeStop::Entry(grant.refusal()));
                        }
                        grant.workdir = Some(std::env::current_dir()?);
                        grant.seal_policy()?;
                    }
                }
                for parameter in &decl.parameters {
                    if let Some(interface) = parameter.foreign_interface()
                        && (!test_options.foreign_grants.contains(interface)
                            || !options.foreign_grants.contains(interface))
                        && !self.replay
                    {
                        return Err(NativeStop::Entry(format!(
                            "not granted: Foreign[{interface}]: pass --grant Foreign={interface} to hirsel test"
                        )));
                    }
                }
            }
            test_options.output = options
                .output
                .join(crate::request::hex(ContentHash::of_bytes(
                    test.to_string().as_bytes(),
                )));
            Ok(test_options)
        };
        if self.jobs.get() == 1
            || tests.len() <= 1
            || self.replay
            || self.corpus.is_some()
            || (options.recording && !self.laws)
            || options.replay.is_some()
            || options.logical_charge_report.is_some()
            || !matches!(self.runner, Runner::Native)
        {
            for test in tests {
                let test_options = match configure(test) {
                    Ok(options) => options,
                    Err(error) => {
                        report(test, Err(error));
                        continue;
                    }
                };
                let result = if self.replay {
                    product
                        .run_test_replay(
                            test,
                            &test_options,
                            self.timeout,
                            self.runner,
                            self.expect_verdict,
                            self.laws,
                        )
                        .map(|outcome| (outcome, Source::Executed))
                } else if let Some(corpus) = self.corpus {
                    self.run_corpus(corpus, product, test, &test_options)
                } else {
                    self.run_one(product, test, &test_options)
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
                let test_options = match configure(test) {
                    Ok(options) => options,
                    Err(error) => {
                        complete(index, Err(error));
                        continue;
                    }
                };
                let exclusive = product.test_needs_exclusive_run(test, &test_options);
                let slots = if exclusive { 0 } else { self.jobs.get() - 1 };
                while running > slots {
                    let (index, outcome) =
                        results.recv().expect("running child reports its result");
                    complete(index, unwrap(outcome));
                    running -= 1;
                }

                if exclusive {
                    complete(index, self.run_one(product, test, &test_options));
                    continue;
                }
                let process = match product.test_launch(
                    test,
                    &test_options,
                    self.runner,
                    self.cache,
                    self.rerun,
                    self.seed,
                    self.laws.then_some(self.plain_runtime.as_deref()),
                ) {
                    Ok(TestLaunch::Run(process)) => process,
                    Ok(TestLaunch::Cached(outcome)) => {
                        complete(index, Ok((outcome, Source::Cached)));
                        continue;
                    }
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
                        process.run(test, timeout)
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
        self.test_launch(test, options, runner, cache, rerun, None, None)?
            .finish(test, timeout)
    }

    #[allow(clippy::too_many_arguments)]
    fn test_launch<'a>(
        &self,
        test: &DefPath,
        options: &NativeOptions,
        runner: &Runner,
        cache: Option<&'a TestCache>,
        rerun: bool,
        seed: Option<u64>,
        laws: Option<Option<&Path>>,
    ) -> Result<TestLaunch<'a>, NativeStop> {
        let Some(decl) = self.test_entries.get(test) else {
            return Err(NativeStop::Entry(format!(
                "{test} is not a discovered @test"
            )));
        };
        let parameterless = parameterless_test(decl);
        // An unused launch grant could otherwise touch host state before the
        // test starts.
        let mut options = self.granted(test, options);
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
            && let Some((output, laws)) = cache.get(key, laws.is_some())
        {
            return Ok(TestLaunch::Cached(TestOutcome {
                definition: test.clone(),
                output,
                timed_out: false,
                wall_time: Duration::ZERO,
                corpus: None,
                laws,
            }));
        }
        let built = if options.foreign_grants.is_empty() || options.recording {
            self.build_reachable(test, &reached, options)?
        } else {
            let store = crate::code_store::CodeStore::cache(&options.store, "code")?;
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
        let executable = std::fs::canonicalize(&built.executable)?;
        let mut command = runner.test_command(&executable);
        for grant in &options.exec_grants {
            grant.approve(&mut command);
        }
        if let Some(seed) = seed {
            command.env(SEED_ENV, seed.to_string());
        }
        let directory = private_directory(&mut command)?;
        let observe = laws.is_some() && !foreign;
        let laws = observe
            .then(|| LawCapture::attach(&mut command, self, test, options.instance_limit))
            .flatten();
        let pass = pass.filter(|_| !observe || laws.is_some());
        Ok(TestLaunch::Run(TestProcess {
            command,
            executable,
            pass,
            _directory: directory,
            laws,
            foreign,
        }))
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

    fn run_test_replay(
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
        let directory = private_directory(&mut cmd)?;
        let (output, timed_out, wall_time) = execute(&mut cmd, timeout)?;
        let laws = observation
            .as_ref()
            .map_or_else(LawReport::default, LawCapture::report);
        drop(directory);
        if timed_out || !output.status.success() {
            return Ok(TestOutcome {
                definition: test.clone(),
                output,
                timed_out,
                wall_time,
                corpus: None,
                laws,
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
