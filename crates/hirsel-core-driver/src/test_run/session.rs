//! A `hirsel test` request as a session (`docs/spec/cli.md`, "The resident
//! compiler"): the driver checks and builds, and hands each unit's launches
//! to its caller, which runs them ([`super::launch::drive`]) and steps the unit
//! with what ran. Its rows print in unit order as units end; coverage and the
//! law lock fold at [`TestSession::finish`]. A one-shot request drives its
//! own session; a resident compiler's client drives the server's.
use super::launch::{LaunchSpec, Plan, Ran, Steps};
use super::replay::{self, Recording};
use super::{LawReport, RunCoverage, Runner, SEED_ENV, Source, TestCache, TestLaunch, TestOutcome};
use crate::meter::Meter;
use crate::property_corpus::{self, Corpus, SavedEntry};
use crate::{CheckProduct, NativeOptions, NativeStop};
use hirsel_core_ids::{ContentHash, DefPath};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

type TestResult = Result<(TestOutcome, Source), NativeStop>;

/// What every unit of a request shares.
pub(super) struct Context<'a> {
    pub runner: &'a Runner,
    pub timeout: Option<Duration>,
    pub replay: bool,
    pub expect_verdict: Option<&'a str>,
    pub cache: Option<&'a TestCache>,
    pub rerun: bool,
    pub laws: bool,
    pub plain_runtime: Option<&'a Path>,
    pub corpus: Option<&'a Corpus>,
}

static NATIVE: Runner = Runner::Native;

impl Default for Context<'_> {
    fn default() -> Self {
        Self {
            runner: &NATIVE,
            timeout: None,
            replay: false,
            expect_verdict: None,
            cache: None,
            rerun: false,
            laws: false,
            plain_runtime: None,
            corpus: None,
        }
    }
}

impl Context<'_> {
    fn law_runtime(&self) -> Option<Option<&Path>> {
        self.laws.then_some(self.plain_runtime)
    }
}

/// One test of one round, from its configured options to its outcome.
pub(super) struct Unit {
    test: DefPath,
    seed: Option<u64>,
    phase: Phase,
}

enum Phase {
    Start(Result<NativeOptions, NativeStop>),
    Run,
    Record(Recording),
    Replay(Recording, TestOutcome),
    Saved {
        entry: property_corpus::Entry,
        executable: PathBuf,
        options: NativeOptions,
    },
    Search {
        executable: PathBuf,
        stale: bool,
        replay_time: Duration,
    },
    Done,
}

/// A unit's next launch, or its end.
type Next = Result<LaunchSpec, (TestOutcome, Source)>;

impl Unit {
    pub(super) fn new(
        test: DefPath,
        seed: Option<u64>,
        options: Result<NativeOptions, NativeStop>,
    ) -> Self {
        Self {
            test,
            seed,
            phase: Phase::Start(options),
        }
    }

    /// The launch this unit needs after `ran` (None to start it), or its result.
    fn step(
        &mut self,
        product: &CheckProduct,
        cx: &Context<'_>,
        ran: Option<Result<Ran, String>>,
    ) -> Result<LaunchSpec, TestResult> {
        match self.advance(product, cx, ran) {
            Ok(Ok(spec)) => Ok(spec),
            Ok(Err(done)) => Err(Ok(done)),
            Err(stop) => {
                self.phase = Phase::Done;
                Err(Err(stop))
            }
        }
    }

    /// Run every launch of this unit here.
    pub(super) fn run_here(mut self, product: &CheckProduct, cx: &Context<'_>) -> TestResult {
        let mut ran = None;
        loop {
            match self.step(product, cx, ran.take()) {
                Ok(spec) => ran = Some(spec.run().map_err(|error| error.to_string())),
                Err(result) => return result,
            }
        }
    }

    fn advance(
        &mut self,
        product: &CheckProduct,
        cx: &Context<'_>,
        ran: Option<Result<Ran, String>>,
    ) -> Result<Next, NativeStop> {
        let test = self.test.clone();
        let test = &test;
        let ran = ran
            .map(|ran| ran.map(|ran| ran.outcome(test)))
            .transpose()
            .map_err(|error| NativeStop::Input(io::Error::other(error)))?;
        let done = |outcome| Ok(Err((outcome, Source::Executed)));
        match (std::mem::replace(&mut self.phase, Phase::Done), ran) {
            (Phase::Start(options), None) => {
                let options = options?;
                if cx.replay {
                    let (spec, recording) =
                        product.replay_record(test, &options, cx.runner, cx.laws, cx.timeout)?;
                    self.phase = Phase::Record(recording);
                    return Ok(Ok(spec));
                }
                if let Some(corpus) = cx.corpus {
                    corpus.check_grants(&product.granted(test, &options).directories)?;
                    return match corpus.read(test)? {
                        SavedEntry::Missing => {
                            self.search(product, cx, options, false, Duration::ZERO)
                        }
                        SavedEntry::Stale => {
                            corpus.retire(test)?;
                            self.search(product, cx, options, true, Duration::ZERO)
                        }
                        SavedEntry::Current(entry) => {
                            let (spec, executable) = corpus_launch(
                                product,
                                cx,
                                test,
                                &options,
                                entry.launch_seed,
                                &entry.input,
                            )?;
                            self.phase = Phase::Saved {
                                entry,
                                executable,
                                options,
                            };
                            Ok(Ok(spec))
                        }
                    };
                }
                match product.test_launch(
                    test,
                    &options,
                    cx.runner,
                    cx.cache,
                    cx.rerun,
                    self.seed,
                    cx.law_runtime(),
                    cx.timeout,
                )? {
                    TestLaunch::Cached(outcome) => Ok(Err((outcome, Source::Cached))),
                    TestLaunch::Run(spec, _) => {
                        self.phase = Phase::Run;
                        Ok(Ok(spec))
                    }
                }
            }
            (Phase::Run, Some(outcome)) => done(outcome),
            (Phase::Record(recording), Some(record)) => {
                match product
                    .replay_after_record(test, &recording, record, cx.runner, cx.timeout)?
                {
                    Ok(outcome) => done(outcome),
                    Err((spec, record)) => {
                        self.phase = Phase::Replay(recording, record);
                        Ok(Ok(spec))
                    }
                }
            }
            (Phase::Replay(_tape, record), Some(replayed)) => {
                done(replay::verdict(record, replayed, cx.expect_verdict)?)
            }
            (
                Phase::Saved {
                    entry,
                    executable,
                    options,
                },
                Some(mut outcome),
            ) => {
                let corpus = cx.corpus.expect("a corpus unit has its corpus");
                match property_corpus::report(&outcome.output, outcome.timed_out, &executable) {
                    Some(property_corpus::Report::Stale) => {
                        corpus.retire(test)?;
                        let replay_time = outcome.wall_time;
                        self.search(product, cx, options, true, replay_time)
                    }
                    Some(property_corpus::Report::Failure(failure)) => {
                        outcome.corpus = Some(if entry.confirms(&failure) {
                            "confirmed"
                        } else {
                            corpus.write(test, entry.launch_seed, failure)?;
                            "changed-failure"
                        });
                        done(outcome)
                    }
                    None => {
                        if outcome.passed() {
                            return Err(NativeStop::Entry(
                                "test did not verify its saved property corpus entry".into(),
                            ));
                        }
                        outcome.corpus = Some("unverified");
                        done(outcome)
                    }
                }
            }
            (
                Phase::Search {
                    executable,
                    stale,
                    replay_time,
                },
                Some(mut outcome),
            ) => {
                let corpus = cx.corpus.expect("a corpus unit has its corpus");
                outcome.wall_time += replay_time;
                if let Some(property_corpus::Report::Failure(failure)) =
                    property_corpus::report(&outcome.output, outcome.timed_out, &executable)
                {
                    corpus.write(test, self.seed, failure)?;
                    outcome.corpus = Some(if stale { "stale-and-saved" } else { "saved" });
                } else if stale {
                    outcome.corpus = Some("stale");
                }
                done(outcome)
            }
            _ => Err(NativeStop::Entry(format!(
                "{test}: the test session stepped a unit out of order"
            ))),
        }
    }

    fn search(
        &mut self,
        product: &CheckProduct,
        cx: &Context<'_>,
        options: NativeOptions,
        stale: bool,
        replay_time: Duration,
    ) -> Result<Next, NativeStop> {
        let (spec, executable) =
            corpus_launch(product, cx, &self.test, &options, self.seed, "search")?;
        self.phase = Phase::Search {
            executable,
            stale,
            replay_time,
        };
        Ok(Ok(spec))
    }
}

fn corpus_launch(
    product: &CheckProduct,
    cx: &Context<'_>,
    test: &DefPath,
    options: &NativeOptions,
    seed: Option<u64>,
    input: &str,
) -> Result<(LaunchSpec, PathBuf), NativeStop> {
    let TestLaunch::Run(mut spec, executable) = product.test_launch(
        test,
        options,
        cx.runner,
        None,
        true,
        seed,
        cx.law_runtime(),
        cx.timeout,
    )?
    else {
        unreachable!("corpus runs have no pass cache")
    };
    spec.set_env(property_corpus::ENV, input);
    Ok((spec, executable))
}

/// What `hirsel test` decided before its session starts.
pub(crate) struct Prepare {
    pub product: Arc<CheckProduct>,
    pub options: NativeOptions,
    pub runs: crate::package_dir::TestRuns,
    pub tests: Vec<DefPath>,
    pub law_inventory: Option<crate::law_lock::Inventory>,
    pub law_directory: PathBuf,
    pub law_baseline: Option<crate::law_lock::Baseline>,
    pub corpus: Option<Corpus>,
    pub cache: Option<TestCache>,
    pub sim: bool,
    pub replay: bool,
    pub laws: bool,
    pub write_laws: bool,
    pub rerun: bool,
    pub coverage: Option<super::Flags>,
    pub jobs: NonZeroUsize,
    pub timeout: Option<Duration>,
    pub seeds: u64,
    pub seed: Option<u64>,
    pub seed_base: u64,
    pub runner: Runner,
    pub plain_runtime: Option<PathBuf>,
    pub expect_verdict: Option<String>,
    pub json: bool,
    pub meter: Meter,
    pub metrics: Option<PathBuf>,
    pub output: Option<crate::store::Scratch>,
}

/// A `hirsel test` request between its check and its exit status.
pub struct TestSession {
    product: Arc<CheckProduct>,
    options: NativeOptions,
    runner: Runner,
    timeout: Option<Duration>,
    replay: bool,
    expect_verdict: Option<String>,
    cache: Option<TestCache>,
    rerun: bool,
    laws: bool,
    write_laws: bool,
    plain_runtime: Option<PathBuf>,
    corpus: Option<Corpus>,
    units: Vec<Unit>,
    plan: Plan,
    /// Ended units not yet reported, until every earlier one is.
    ended: BTreeMap<usize, TestResult>,
    reported: usize,
    rows: Vec<serde_json::Value>,
    failed: bool,
    observations: BTreeMap<String, LawReport>,
    coverage: Option<(super::Session, super::Flags)>,
    no_tests: bool,
    law_inventory: Option<crate::law_lock::Inventory>,
    law_directory: PathBuf,
    law_baseline: Option<crate::law_lock::Baseline>,
    json: bool,
    meter: Meter,
    metrics: Option<PathBuf>,
    /// Where the request's tests are built; removed with the session.
    _output: Option<crate::store::Scratch>,
}

impl TestSession {
    pub(crate) fn prepare(prepare: Prepare) -> Self {
        let Prepare {
            product,
            options,
            runs,
            tests,
            sim,
            seeds,
            seed,
            seed_base,
            corpus,
            replay,
            ..
        } = &prepare;
        let rounds = if tests.is_empty() { 0 } else { *seeds };
        let mut units = Vec::new();
        let mut exclusive = Vec::new();
        for round in 0..rounds {
            let seed = sim.then_some(seed.unwrap_or(seed_base + round));
            for test in tests {
                let configured = configure(
                    product,
                    runs,
                    options,
                    test,
                    seed.is_some(),
                    corpus.is_some(),
                    *replay,
                );
                exclusive.push(configured.as_ref().is_ok_and(|test_options| {
                    product.test_needs_exclusive_run(test, test_options)
                }));
                units.push(Unit::new(test.clone(), seed, configured));
            }
        }
        let serial = tests.len() <= 1
            || *replay
            || corpus.is_some()
            || (options.recording && !prepare.laws)
            || options.replay.is_some()
            || options.logical_charge_report.is_some()
            || !matches!(prepare.runner, Runner::Native);
        let jobs = if serial { 1 } else { prepare.jobs.get() };
        let coverage = prepare
            .coverage
            .map(|flags| (super::Session::new(&prepare.options, flags.runs), flags));
        let no_tests = prepare.tests.is_empty();
        Self {
            product: prepare.product,
            options: prepare.options,
            runner: prepare.runner,
            timeout: prepare.timeout,
            replay: prepare.replay,
            expect_verdict: prepare.expect_verdict,
            cache: prepare.cache,
            rerun: prepare.rerun,
            laws: prepare.laws,
            write_laws: prepare.write_laws,
            plain_runtime: prepare.plain_runtime,
            corpus: prepare.corpus,
            units,
            plan: Plan { jobs, exclusive },
            ended: BTreeMap::new(),
            reported: 0,
            rows: Vec::new(),
            failed: false,
            observations: BTreeMap::new(),
            coverage,
            no_tests,
            law_inventory: prepare.law_inventory,
            law_directory: prepare.law_directory,
            law_baseline: prepare.law_baseline,
            json: prepare.json,
            meter: prepare.meter,
            metrics: prepare.metrics,
            _output: prepare.output,
        }
    }

    pub(crate) fn plan(&self) -> &Plan {
        &self.plan
    }

    /// The launch `unit` needs after `ran` (None to start it); None once it
    /// ended, its row reported as soon as every earlier unit's is.
    pub(crate) fn step(
        &mut self,
        unit: usize,
        ran: Option<Result<Ran, String>>,
    ) -> Option<LaunchSpec> {
        let cx = Context {
            runner: &self.runner,
            timeout: self.timeout,
            replay: self.replay,
            expect_verdict: self.expect_verdict.as_deref(),
            cache: self.cache.as_ref(),
            rerun: self.rerun,
            laws: self.laws,
            plain_runtime: self.plain_runtime.as_deref(),
            corpus: self.corpus.as_ref(),
        };
        let result = match self.units.get_mut(unit)?.step(&self.product, &cx, ran) {
            Ok(spec) => return Some(spec),
            Err(result) => result,
        };
        self.ended.insert(unit, result);
        while let Some(result) = self.ended.remove(&self.reported) {
            self.report(self.reported, result);
            self.reported += 1;
        }
        None
    }

    /// Drive every unit here, then finish.
    pub(crate) fn conclude(mut self) -> i32 {
        let plan = std::mem::replace(
            &mut self.plan,
            Plan {
                jobs: 1,
                exclusive: Vec::new(),
            },
        );
        if let Err(error) = super::launch::drive(&plan, &mut self) {
            return crate::request::report_request_error(&error);
        }
        self.finish()
    }

    /// Fold the law lock, baseline and coverage, print the report, and give
    /// the request's exit status.
    pub(crate) fn finish(self) -> i32 {
        self.fold()
            .unwrap_or_else(|error| crate::request::report_request_error(error.as_ref()))
    }

    fn fold(mut self) -> Result<i32, Box<dyn std::error::Error>> {
        if let Some(current) = &self.law_inventory {
            crate::law_lock::update(&self.law_directory, |lock| {
                lock.record(current, &self.observations)
            })?;
        }
        let regressions = self
            .law_baseline
            .as_ref()
            .map(|baseline| baseline.compare(&self.observations));
        let law_red = regressions.as_ref().is_some_and(|rows| !rows.is_empty());
        self.failed |= law_red;
        let passed = !self.no_tests && !self.failed;
        let coverage = self.coverage.take().and_then(|(session, flags)| {
            let scope = self.options.coverage.as_ref().unwrap();
            session.finish(&self.product, scope, &mut self.rows, &flags)
        });
        if self.json {
            let mut report = serde_json::json!({"passed":passed,"tests":self.rows});
            if let Some(coverage) = &coverage {
                report["coverage"] = coverage.document.clone();
            }
            if let Some(regressions) = regressions {
                report["law_regressions"] = serde_json::json!(regressions);
            }
            writeln!(io::stdout().lock(), "{report}")
                .map_err(|error| format!("write test report: {error}"))?;
        } else {
            for red in regressions.into_iter().flatten() {
                println!("{red}");
            }
            if let Some(coverage) = &coverage {
                print!("{}", coverage.text);
            }
        }
        if self.no_tests && !self.json {
            eprintln!("no tests found");
        }
        self.meter.finish("test execution/output");
        if let Some(path) = &self.metrics {
            self.meter.write_metrics(path.clone())?;
        }
        Ok(if self.no_tests && !law_red {
            2
        } else {
            i32::from(self.failed)
        })
    }

    fn report(&mut self, unit: usize, result: TestResult) {
        let Unit { test, seed, .. } = &self.units[unit];
        let (test, seed) = (test.clone(), *seed);
        let json = self.json;
        let label = seed.map_or_else(|| test.to_string(), |seed| format!("{test} (seed {seed})"));
        let mut row = match result {
            Ok((outcome, source)) => {
                let passed = outcome.passed();
                let wall_ms =
                    (source == Source::Executed).then(|| outcome.wall_time.as_secs_f64() * 1000.0);
                let source = source.as_str();
                self.failed |= !passed;
                let corpus_note = outcome
                    .corpus
                    .map_or_else(String::new, |state| format!(" [property corpus {state}]"));
                let strength = self.laws.then(|| outcome.laws.strength());
                let mut laws_note = strength.map_or_else(String::new, |class| format!(" {class}"));
                if let Some((session, _)) = &mut self.coverage {
                    laws_note.push_str(&session.run(
                        self.rows.len(),
                        &test,
                        seed,
                        passed,
                        &outcome.coverage,
                    ));
                }
                if !json {
                    print_outcome(
                        &outcome,
                        passed,
                        wall_ms,
                        source,
                        &label,
                        &corpus_note,
                        &laws_note,
                    );
                }
                let mut row = serde_json::json!({"definition":test.to_string(),"passed":passed,"source":source,"status":outcome.output.status.code(),"stdout":String::from_utf8_lossy(&outcome.output.stdout),"stderr":String::from_utf8_lossy(&outcome.output.stderr),"timed_out":outcome.timed_out,"wall_ms":wall_ms});
                if let Some(state) = outcome.corpus {
                    row["property_corpus"] = state.into();
                }
                if let Some(strength) = strength {
                    if !json {
                        outcome.laws.print();
                    }
                    row["law_strength"] = strength.into();
                    row["laws"] = outcome.laws.json_rows();
                }
                if self.write_laws || self.law_baseline.is_some() {
                    match self.observations.entry(test.to_string()) {
                        std::collections::btree_map::Entry::Occupied(mut held) => {
                            held.get_mut().merge(outcome.laws)
                        }
                        std::collections::btree_map::Entry::Vacant(slot) => {
                            slot.insert(outcome.laws);
                        }
                    }
                }
                row
            }
            Err(error) => {
                self.failed = true;
                if let Some((session, _)) = &mut self.coverage {
                    session.run(
                        self.rows.len(),
                        &test,
                        seed,
                        false,
                        &RunCoverage::Unobserved("stopped".into()),
                    );
                }
                let mut row = serde_json::json!({"definition":test.to_string(),"passed":false,"source":"executed","stopped":error.to_string(),"wall_ms":null});
                if self.laws {
                    row["law_strength"] = if matches!(error, NativeStop::ForeignRecording { .. }) {
                        "unobserved (FFI-8)"
                    } else {
                        "unobserved"
                    }
                    .into();
                    row["laws"] = serde_json::json!([]);
                }
                if let NativeStop::ForeignRecording { site, .. } = &error {
                    row["site"] = site.as_str().into();
                    if !json {
                        eprintln!("{site}: recording refusal site");
                        println!("fail executed {label}: {error}");
                    }
                } else if !json {
                    println!("fail executed {label}: {error} (time unavailable)");
                }
                row
            }
        };
        if let Some(seed) = seed {
            row["seed"] = seed.into();
        }
        self.rows.push(row);
    }
}

impl Steps for TestSession {
    fn step(
        &mut self,
        unit: usize,
        ran: Option<Result<Ran, String>>,
    ) -> io::Result<Option<LaunchSpec>> {
        Ok(TestSession::step(self, unit, ran))
    }
}

fn print_outcome(
    outcome: &TestOutcome,
    passed: bool,
    wall_ms: Option<f64>,
    source: &str,
    label: &str,
    corpus_note: &str,
    laws_note: &str,
) {
    let Some(wall_ms) = wall_ms else {
        println!("pass {source} {label}{laws_note}");
        return;
    };
    if passed {
        println!("pass {source} {label}{corpus_note}{laws_note} ({wall_ms:.3} ms)");
        return;
    }
    let mut reason = if outcome.timed_out {
        "execution timed out".to_owned()
    } else {
        super::outcome_text(outcome.output.status)
    };
    if !outcome.output.stderr.is_empty() {
        reason.push_str(": ");
        reason.push_str(
            &String::from_utf8_lossy(&outcome.output.stderr)
                .lines()
                .collect::<Vec<_>>()
                .join(" | "),
        );
    }
    println!("fail {source} {label}{corpus_note}{laws_note}: {reason} ({wall_ms:.3} ms)");
    for line in String::from_utf8_lossy(&outcome.output.stdout).lines() {
        println!("  stdout: {line}");
    }
}

/// Each test's own run configuration; one test's never changes another's.
fn configure(
    product: &CheckProduct,
    runs: &crate::package_dir::TestRuns,
    options: &NativeOptions,
    test: &DefPath,
    seeded: bool,
    corpus: bool,
    replay: bool,
) -> Result<NativeOptions, NativeStop> {
    let mut test_options = runs.options(test, options);
    let mut grant_env = |name| {
        let env = test_options
            .env_patterns
            .get_or_insert_with(Default::default);
        if !env.covers(name) {
            env.extend(hirsel_core_net::EnvPatterns::parse(name).unwrap());
        }
        test_options.grant_mask |= 1 << crate::grant_kind("Env").unwrap();
    };
    if seeded || corpus {
        grant_env(SEED_ENV);
    }
    if corpus {
        grant_env(property_corpus::ENV);
    }
    if let Some(Ok(decl)) = product.test_entries.get(test) {
        let exec = decl
            .parameters
            .iter()
            .filter(|p| p.ty == "core@1/core/Exec")
            .count();
        if !replay {
            // Every grant this test's command line does not approve,
            // Exec then Foreign, each in parameter order.
            let mut refusals = Vec::new();
            for grant in test_options.exec_grants.iter().take(exec) {
                if !options
                    .exec_grants
                    .iter()
                    .any(|approved| approved.request.text() == grant.request.text())
                {
                    refusals.push(grant.refusal());
                }
            }
            for interface in decl.parameters.iter().filter_map(|p| p.foreign_interface()) {
                let refusal = format!(
                    "not granted: Foreign[{interface}]: pass --grant Foreign={interface} to hirsel test"
                );
                if (!test_options.foreign_grants.contains(interface)
                    || !options.foreign_grants.contains(interface))
                    && !refusals.contains(&refusal)
                {
                    refusals.push(refusal);
                }
            }
            if !refusals.is_empty() {
                return Err(NativeStop::Entry(refusals.join("\n")));
            }
            for grant in test_options.exec_grants.iter_mut().take(exec) {
                grant.workdir = Some(std::env::current_dir()?);
                grant.seal_policy()?;
            }
        }
    }
    test_options.output = options
        .output
        .join(crate::request::hex(ContentHash::of_bytes(
            test.to_string().as_bytes(),
        )));
    Ok(test_options)
}
