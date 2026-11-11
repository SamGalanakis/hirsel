//! The native differential gate: every executable corpus case in the baseline
//! is run by the heap executor and as native code under
//! a named configuration matrix and compared with the reference observation.
//! Large library evidence can explicitly select a stdout sidecar instead;
//! those cases run only compiled configurations.
//!
//! Promotion is set-wise. B is the executable corpus cases outside the gate exclusions;
//! N is the cases every configuration of an engine agrees on. Every case and
//! configuration that did not agree must be exactly one row of
//! `native-tests/regressions/exclusions.tsv`, naming the configuration, a
//! reason class and a reason; a mismatch's reason fingerprints its observation.
//! A disagreement without a row, a row whose reason changed and a row whose
//! configuration now agrees all fail the gate, so support only grows.
pub mod artifact;
pub mod corpus;
pub mod cranelift;
pub mod diet;
pub mod generate;
mod oracle;
pub mod scratch;
pub(crate) mod source_request;

pub use scratch::{runtime_cache, scratch};

use hirsel_native::NativeError;
use hirsel_native::OBSERVATION_SCHEMA;
use hirsel_native::Observation;
use hirsel_native::StageName;
use hirsel_native::Toolchain;
use hirsel_native_testing::Configuration;
use hirsel_native_testing::Engine;
use hirsel_native_testing::MATRIX_VERSION;
use hirsel_native_testing::Outcome;
use hirsel_native_testing::ReadyExecution;
use hirsel_native_testing::ReadySupplier;
use hirsel_native_testing::differential_ready;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const EXCLUSIONS: &str = "native-tests/regressions/exclusions.tsv";

/// One executable corpus, regression or library case.
#[derive(Clone, Debug)]
pub struct Case {
    /// The source path relative to the repository root: the case ID.
    pub id: String,
    pub source: String,
    pub path: Option<PathBuf>,
}

impl Case {
    /// An explicit compiled-only library oracle, or the ordinary interpreter default.
    pub fn expected_stdout(&self) -> Result<Option<Observation>, String> {
        oracle::expected_stdout(&self.id, &self.source, self.path.as_deref())
    }
    /// Run this case with its package graph under a case-specific plan.
    pub fn run(&self, matrix: &[Configuration], toolchain: &Toolchain, dir: &Path) -> CaseResult {
        run_source_path(
            &self.id,
            &self.source,
            self.path.as_deref(),
            matrix,
            toolchain,
            dir,
            Probe::default(),
        )
    }

    /// Run one complete clean execution and a disjoint fault-campaign partition.
    pub fn run_heap_partition(
        &self,
        matrix: &[Configuration],
        toolchain: &Toolchain,
        dir: &Path,
        partition: Option<hirsel_native::HeapCampaignPartition>,
    ) -> CaseResult {
        run_source_path(
            &self.id,
            &self.source,
            self.path.as_deref(),
            matrix,
            toolchain,
            dir,
            Probe {
                heap_partition: partition,
                ..Probe::default()
            },
        )
    }

    /// `--trace-roots` (S-14): run this case under `matrix` with every engine tracing its
    /// `Shared` roots, so a configuration agrees only when its trace equals the
    /// interpreter's line for line.
    pub fn run_traced(
        &self,
        matrix: &[Configuration],
        toolchain: &Toolchain,
        dir: &Path,
    ) -> CaseResult {
        run_source_path(
            &self.id,
            &self.source,
            self.path.as_deref(),
            matrix,
            toolchain,
            dir,
            Probe {
                trace_roots: true,
                ..Probe::default()
            },
        )
    }

    /// `--migrate` (design §9 W1): run this case under `matrix` with its native executables
    /// on several compute workers, every task moving to another worker at every suspension,
    /// so a configuration agrees only when migration changes no observation.
    pub fn run_migrated(
        &self,
        matrix: &[Configuration],
        toolchain: &Toolchain,
        dir: &Path,
    ) -> CaseResult {
        run_source_path(
            &self.id,
            &self.source,
            self.path.as_deref(),
            matrix,
            toolchain,
            dir,
            Probe {
                migrate: true,
                ..Probe::default()
            },
        )
    }
}

/// What a differential run probes beyond the observation.
#[derive(Clone, Copy, Debug, Default)]
struct Probe {
    heap_partition: Option<hirsel_native::HeapCampaignPartition>,
    /// Every engine traces its `Shared` roots (`--trace-roots`).
    trace_roots: bool,
    /// Native executables migrate every suspension (`--migrate`).
    migrate: bool,
}

/// B of the set rule: every discovered executable corpus case outside the gate
/// exclusions (a `pending.txt` line or an open `// spec: #N` issue).
pub fn baseline(root: &Path) -> Result<Vec<Case>, String> {
    let rules = crate::spec_rules(root)?;
    let pending = crate::pending(root)?;
    let mut cases = Vec::new();
    for file in crate::Selection::default().files(root)? {
        let id = crate::relative(root, &file);
        let source = std::fs::read_to_string(&file).map_err(|e| format!("{id}: {e}"))?;
        let header = crate::parse_header(Path::new(&id), &source, &rules)?;
        if matches!(
            header.expect,
            crate::Expect::Test
                | crate::Expect::Run { .. }
                | crate::Expect::Fail { .. }
                | crate::Expect::Blocked { .. }
                | crate::Expect::Signaled { .. }
        ) && crate::gate_exclusions(&pending, &id, &source).is_empty()
        {
            cases.push(Case {
                id,
                path: Some(file),
                source,
            });
        }
    }
    Ok(cases)
}

/// The engine class a configuration belongs to; each has its own pass set.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Class {
    Heap,
    Llvm,
    Cranelift,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Self::Heap => "heap",
            Self::Llvm => "llvm",
            Self::Cranelift => "cranelift",
        }
    }
    pub fn of(configuration: &Configuration) -> Self {
        match configuration.engine {
            Engine::Heap { .. } => Self::Heap,
            Engine::Native { .. } => Self::Llvm,
            Engine::Cranelift { .. } => Self::Cranelift,
        }
    }
}

/// Why a case is outside an engine's pass set.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Exclusion {
    /// `m2-unavailable`, `native-unavailable` (a stage reports the feature
    /// unsupported), `compiler-error` (a stage validator refused a pass's
    /// output, or a pass failed) or `known-bug` (the engine ran and disagreed).
    pub class: String,
    pub reason: String,
}

/// `text` with each debug-printed `DefPath(..)` replaced by its definition's
/// name and each run of digits by `N`: table indices, layout numbers and
/// offsets vary with unrelated edits, and a reason must not.
fn stable(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("DefPath(") {
        out.push_str(&rest[..start]);
        let body = &rest[start..];
        let mut depth = 0usize;
        let mut end = body.len();
        for (index, c) in body.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = index + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        let path = &body[..end];
        let name = path
            .rfind("name: \"")
            .map(|at| &path[at + 7..])
            .and_then(|tail| tail.split('"').next())
            .unwrap_or("?");
        out.push_str(name);
        rest = &body[end..];
    }
    out.push_str(rest);
    let mut normal = String::new();
    let mut digits = false;
    for c in out.chars() {
        if c.is_ascii_digit() {
            if !digits {
                normal.push('N');
            }
            digits = true;
        } else {
            digits = false;
            normal.push(c);
        }
    }
    normal
}

/// The stable part of a tool error.
fn reason(error: &NativeError) -> String {
    let message = &error.message;
    let quoted = |after: &str| {
        let start = message.find(after)? + after.len();
        let end = message[start..].find('"')? + start;
        Some(message[start..end].to_string())
    };
    let detail = if let Some(owner) = quoted("MissingOwner(\"") {
        format!("no native layout for {owner}")
    } else if let Some(name) = quoted("no native entry for operation \"") {
        format!("operation {name} has no native entry")
    } else if let Some(what) = quoted("Unsupported(\"") {
        stable(&what)
    } else if error.validator {
        let kind: String = message
            .chars()
            .take_while(|c| *c != '(' && *c != ' ')
            .collect();
        format!("validator: {kind}")
    } else {
        // A heap violation repeats its kind and ends with its site; an
        // emission error starts with its unit, which the error already names.
        let text = stable(message);
        let text = match text.split_once(": ") {
            Some((unit, tail)) if unit.starts_with("standalone@") => tail.to_string(),
            _ => text,
        };
        let text = if let Some(at) = text.find("MissingDefinition(") {
            // An operation taken as a function value: which one is noise.
            format!("{}MissingDefinition", &text[..at])
        } else {
            text
        };
        let text = text.split(" at ").next().unwrap_or(&text);
        match text.split_once(": ") {
            Some((kind, tail)) if tail.starts_with(kind) => tail.to_string(),
            _ => text.to_string(),
        }
    };
    let detail: String = detail.chars().take(110).collect();
    format!(
        "{}: {}",
        error.stage.name(),
        detail.replace(['\t', '\n'], " ")
    )
}

/// A mismatch's reason: its observation, abbreviated, and an FNV-1a digest
/// of all of it, so a listed wrong answer that changes no longer matches.
fn mismatch_reason(actual: &Observation) -> String {
    let text = actual.render();
    let digest = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    let summary: String = text
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join(" | ")
        .chars()
        .take(90)
        .collect();
    format!("observed {} ({digest:016x})", summary.replace('\t', " "))
}

pub fn exclusion(outcome: &Outcome) -> Option<Exclusion> {
    match outcome {
        Outcome::Agree => None,
        Outcome::Mismatch(actual) => Some(Exclusion {
            class: "known-bug".into(),
            reason: mismatch_reason(actual),
        }),
        Outcome::Unavailable(error) => {
            let text = reason(error);
            let m2 = [
                "(M2)", "Slots", "Tasks", "Channel", "Mutex", "Atomic", "Arena",
            ]
            .iter()
            .any(|m| error.message.contains(m));
            let unsupported = error.message.contains("Unsupported")
                || error.message.contains("the entry contract")
                || error.message.contains("MissingOwner")
                || error.message.contains("no native entry")
                || error.message.contains("does not support");
            let class = if m2 {
                "m2-unavailable"
            } else if error.stage == StageName::Heap && !unsupported {
                "known-bug"
            } else if unsupported {
                "native-unavailable"
            } else {
                "compiler-error"
            };
            Some(Exclusion {
                class: class.into(),
                reason: text,
            })
        }
    }
}

/// One case's row of the matrix.
#[derive(Clone, Debug)]
pub struct CaseResult {
    pub id: String,
    /// The source or oracle could not be prepared as a native snapshot.
    pub snapshot: Option<String>,
    pub outcomes: Vec<(String, Class, Outcome)>,
    /// The expected observation, from interpretation or an explicit library sidecar.
    pub reference: Option<Observation>,
    /// The case's wall time on its job, every configuration included;
    /// `run_cases_with` sets it.
    pub wall: std::time::Duration,
}

/// The configuration an exclusion row names for a case with no native
/// snapshot, which no configuration ran.
pub const SNAPSHOT: &str = "snapshot";

impl CaseResult {
    /// Every configuration of `classes` that did not agree, with why; a case
    /// with no snapshot is the one `snapshot` row.
    pub fn exclusions(&self, classes: &[Class]) -> Vec<(String, Exclusion)> {
        if let Some(reason) = &self.snapshot {
            return vec![(
                SNAPSHOT.into(),
                Exclusion {
                    class: "compiler-error".into(),
                    reason: reason.clone(),
                },
            )];
        }
        self.outcomes
            .iter()
            .filter(|(_, c, _)| classes.contains(c))
            .filter_map(|(name, _, outcome)| Some((name.clone(), exclusion(outcome)?)))
            .collect()
    }

    /// Whether every configuration of `class` agreed.
    pub fn passes(&self, class: Class) -> bool {
        self.exclusions(&[class]).is_empty()
    }
}

struct Prepared {
    input: source_request::Source,
    entry: hirsel_engine::DefPath,
    reference: Observation,
    compiled_only: bool,
    /// A run with runners: what the launcher found about each callee entry.
    callees: std::collections::BTreeMap<String, hirsel_run_schema::CalleeFacts>,
}

impl Prepared {
    /// The case's fixture, with what the launcher found about its callees.
    fn fixture(&self, source: &str) -> hirsel_native::Fixture {
        hirsel_native::Fixture {
            callees: self.callees.clone(),
            ..crate::fixture(source).unwrap()
        }
    }
    /// A campaign's result on this case: its outcomes, or why it could not run.
    fn result(&self, id: &str, outcomes: Result<Outcomes, String>) -> CaseResult {
        match outcomes {
            Ok(outcomes) => CaseResult {
                id: id.to_string(),
                snapshot: None,
                outcomes,
                reference: Some(self.reference.clone()),
                wall: Default::default(),
            },
            Err(reason) => CaseResult::failed(id, reason),
        }
    }
}

/// A campaign's outcome under each configuration, by name and engine class.
type Outcomes = Vec<(String, Class, Outcome)>;

impl CaseResult {
    /// A case that could not be prepared or run, for `reason`.
    fn failed(id: &str, reason: String) -> CaseResult {
        CaseResult {
            id: id.to_string(),
            snapshot: Some(reason),
            outcomes: Vec::new(),
            reference: None,
            wall: Default::default(),
        }
    }
}

fn checked(
    warm: Option<&str>,
    id: &str,
    source: &str,
    path: Option<&Path>,
) -> Result<Prepared, String> {
    checked_with_config(warm, id, source, path, Default::default(), false)
}

fn checked_with_config(
    warm: Option<&str>,
    id: &str,
    source: &str,
    path: Option<&Path>,
    launch: hirsel_native::LaunchInputs,
    trace_roots: bool,
) -> Result<Prepared, String> {
    let expected = oracle::expected_stdout(id, source, path)?;
    let compiled_only = expected.is_some();
    let input = source_request::Source::new(path, source).map_err(|error| error.to_string())?;
    if let Some(warm) = warm {
        input.warm(warm).map_err(|error| error.to_string())?;
    }
    let output = input.check().map_err(|error| format!("check: {error}"))?;
    if let Some(diagnostic) = output
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.severity == hirsel_diag::Severity::Error)
    {
        return Err(format!("check: {} {}", diagnostic.rule, diagnostic.message));
    }
    if output.completion != hirsel_engine::CheckCompletion::Complete {
        return Err(format!("check: {:?}", output.completion));
    }
    let entry = input.entry.clone();
    let mut callees = BTreeMap::new();
    let reference = if let Some(expected) = expected {
        crate::fixture(source)?;
        expected
    } else {
        let tir = input.tir().map_err(|error| format!("reference: {error}"))?;
        callees = tir.callees.clone();
        let config = hirsel_engine::RunConfiguration {
            entry: entry.clone(),
            hash_seed: launch.hash_seed,
            inputs: vec![],
        };
        let execution = hirsel_driver::testing::observe_tir(
            &tir,
            &config,
            hirsel_engine::SimulationConfig {
                random_seed: launch.random_seed,
                test_seed: launch.test_seed,
                trace_roots,
                ..crate::simulation(source)?
            },
            crate::interpreter(source)?.limits,
            false,
        )
        .map_err(|error| format!("reference: {error}"))?;
        execution
            .observation
            .as_ref()
            .map(Observation::of_reference)
            .ok_or("reference: no observation")?
    };
    Ok(Prepared {
        input,
        entry,
        reference,
        compiled_only,
        callees,
    })
}

/// Run one source under `matrix`. Must be called on a compiler stack.
pub fn run_source(
    id: &str,
    source: &str,
    matrix: &[Configuration],
    toolchain: &Toolchain,
    dir: &Path,
) -> CaseResult {
    run_source_path(id, source, None, matrix, toolchain, dir, Probe::default())
}
fn run_source_path(
    id: &str,
    source: &str,
    path: Option<&Path>,
    matrix: &[Configuration],
    toolchain: &Toolchain,
    dir: &Path,
    probe: Probe,
) -> CaseResult {
    let _progress = hirsel_intern::test_progress::enter("native-case", || id.to_string());
    if source.lines().any(|line| line.trim() == "// expect: test") {
        return run_test_suite(id, source, path, matrix, toolchain, dir, probe);
    }
    let launch = matrix
        .first()
        .map(|c| c.options.launch.clone())
        .unwrap_or_default();
    match checked_with_config(None, id, source, path, launch, probe.trace_roots) {
        Err(reason) => CaseResult::failed(id, reason),
        Ok(prepared) => {
            let outcomes = matrix_campaign(
                &mut |options| prepared.input.ready(options),
                &prepared,
                source,
                matrix,
                toolchain,
                dir,
                probe,
            );
            prepared.result(id, outcomes)
        }
    }
}

/// `matrix` on a prepared case. A compiled-only oracle skips the heap executor
/// and needs a compiled configuration.
fn matrix_campaign<P: ReadyExecution>(
    ready: &mut ReadySupplier<'_, P>,
    prepared: &Prepared,
    source: &str,
    matrix: &[Configuration],
    toolchain: &Toolchain,
    dir: &Path,
    probe: Probe,
) -> Result<Outcomes, String> {
    let matrix: Vec<_> = matrix
        .iter()
        .filter(|c| !prepared.compiled_only || !matches!(c.engine, Engine::Heap { .. }))
        .cloned()
        .collect();
    if prepared.compiled_only && matrix.is_empty() {
        return Err("expected-stdout oracle requires a compiled native configuration".into());
    }
    Ok(differential_ready(
        ready,
        &prepared.entry,
        &prepared.reference,
        &matrix,
        toolchain,
        dir,
        &hirsel_native::Fixture {
            heap_campaign_partition: probe.heap_partition,
            trace_roots: probe.trace_roots,
            migrate: probe.migrate,
            ..prepared.fixture(source)
        },
    )
    .into_iter()
    .zip(&matrix)
    .map(|((name, outcome), configuration)| (name, Class::of(configuration), outcome))
    .collect())
}

/// One test of a test corpus case under `matrix`, at the test's seed.
fn test_matrix_campaign<P: ReadyExecution>(
    ready: &mut ReadySupplier<'_, P>,
    test: &hirsel_driver::testing::Test,
    reference: &Observation,
    matrix: &[Configuration],
    toolchain: &Toolchain,
    dir: &Path,
    probe: Probe,
) -> Outcomes {
    differential_ready(
        ready,
        &test.entry,
        reference,
        &test_seeded(matrix, test),
        toolchain,
        dir,
        &hirsel_native::Fixture {
            heap_campaign_partition: probe.heap_partition,
            trace_roots: probe.trace_roots,
            migrate: probe.migrate,
            ..Default::default()
        },
    )
    .into_iter()
    .zip(matrix)
    .map(|((_, outcome), c)| (c.name(), Class::of(c), outcome))
    .collect()
}

/// `matrix` launched at `test`'s seed.
fn test_seeded(
    matrix: &[Configuration],
    test: &hirsel_driver::testing::Test,
) -> Vec<Configuration> {
    let mut configurations = matrix.to_vec();
    for c in &mut configurations {
        c.options.launch.test_seed = test.seed;
    }
    configurations
}

const REPLAY: &str = "x86_64/real-io-record-and-replay";

/// Real-Io record and replay of `entry` under `launch`.
fn replay_campaign<P: ReadyExecution>(
    ready: &mut ReadySupplier<'_, P>,
    entry: &hirsel_engine::DefPath,
    reference: &Observation,
    toolchain: &Toolchain,
    dir: &Path,
    launch: hirsel_native::LaunchInputs,
    fixture: &hirsel_native::Fixture,
) -> Outcomes {
    vec![(
        REPLAY.into(),
        Class::Llvm,
        hirsel_native_testing::real_io_replay_ready(
            ready, entry, reference, toolchain, dir, launch, fixture,
        ),
    )]
}

/// The runner oracle of `entry` under `matrix`'s native configurations,
/// cross-replaying tapes under the first.
fn oracle_campaign<P: ReadyExecution>(
    ready: &mut ReadySupplier<'_, P>,
    entry: &hirsel_engine::DefPath,
    matrix: &[Configuration],
    toolchain: &Toolchain,
    dir: &Path,
    fixture: &hirsel_native::Fixture,
) -> Outcomes {
    matrix
        .iter()
        .filter(|c| !matches!(c.engine, Engine::Heap { .. }))
        .enumerate()
        .flat_map(|(index, configuration)| {
            hirsel_native_testing::runner_oracle_ready(
                ready,
                entry,
                std::slice::from_ref(configuration),
                toolchain,
                dir,
                fixture,
                index == 0,
            )
            .into_iter()
            .map(|(name, outcome)| (name, Class::of(configuration), outcome))
        })
        .collect()
}

/// A test corpus case exercises every selected function in every configuration.
fn run_test_suite(
    id: &str,
    source: &str,
    path: Option<&Path>,
    matrix: &[Configuration],
    toolchain: &Toolchain,
    dir: &Path,
    probe: Probe,
) -> CaseResult {
    visit_test_suite(
        id,
        source,
        path,
        dir,
        matrix
            .first()
            .map(|c| c.options.launch.clone())
            .unwrap_or_default(),
        probe.trace_roots,
        toolchain,
        |ready, test, reference, test_dir| {
            test_matrix_campaign(ready, test, reference, matrix, toolchain, test_dir, probe)
        },
    )
}

/// Share package checking, isolated reference launches and outcome aggregation
/// between differential execution and real-Io record/replay.
fn visit_test_suite(
    id: &str,
    source: &str,
    path: Option<&Path>,
    dir: &Path,
    launch: hirsel_native::LaunchInputs,
    trace_roots: bool,
    toolchain: &Toolchain,
    probe: impl Fn(
        &mut hirsel_native_testing::ReadySupplier<'_, std::sync::Arc<hirsel_requests::NativeReady>>,
        &hirsel_driver::testing::Test,
        &Observation,
        &Path,
    ) -> Vec<(String, Class, Outcome)>,
) -> CaseResult {
    let run = || -> Result<CaseResult, String> {
        if oracle::expected_stdout(id, source, path)?.is_some() {
            return Err("expected-stdout oracle requires a main entry, not expect: test".into());
        }
        let fallback = dir.join("test-suite.hir");
        if path.is_none() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            std::fs::write(&fallback, source).map_err(|e| e.to_string())?;
        }
        use hirsel_requests::CompileOperation;
        use hirsel_requests::CompileRequest;
        use hirsel_requests::CompileResult;
        use hirsel_requests::CompilerRequests;
        use hirsel_requests::NativeOutput;
        use hirsel_requests::NativeRequest;
        let path = path.unwrap_or(&fallback);
        let compiler = crate::compiler::verifying_compiler();
        let mut requests = CompilerRequests::for_host(compiler.clone(), compiler.configuration())
            .with_toolchain(std::sync::Arc::new(toolchain.clone()));
        let (primary, inputs) = hirsel_engine::requests::observe_inputs(
            requests.input_limits(),
            requests.capture_attempts(),
            |inputs| {
                let program = compiler.load_program_inputs(path, inputs, false)?;
                Ok::<_, hirsel_engine::ToolError>(
                    program
                        .engine
                        .sources
                        .file(program.primary)
                        .unwrap()
                        .path
                        .clone(),
                )
            },
        )
        .map_err(|_| "test inputs changed during capture".to_string())?;
        let inputs = inputs
            .replace_sources(
                &BTreeMap::from([(
                    primary.map_err(|e| e.to_string())?,
                    std::sync::Arc::<[u8]>::from(source.as_bytes()),
                )]),
                requests.input_limits().bytes,
            )
            .map_err(|e| e.to_string())?;
        let discovery = hirsel_driver::testing::discover(
            &mut requests,
            path,
            inputs,
            &[],
            None,
            hirsel_driver::testing::Selection::Tests,
        )
        .map_err(|e| e.to_string())?;
        if discovery.tests.is_empty() {
            return Err("expect: test selected no @test functions".into());
        }
        let inputs = discovery
            .execution_inputs(requests.input_limits().bytes)
            .map_err(|e| e.to_string())?;
        let tests = discovery.tests;
        let output = requests
            .execute_request(&CompileRequest {
                path: path.to_owned(),
                inputs: inputs.clone().into(),
                operation: CompileOperation::TirBatch {
                    definitions: tests.iter().map(|test| test.entry.clone()).collect(),
                },
            })
            .map_err(|e| e.to_string())?;
        let CompileResult::TirBatch(batch) = &output.result else {
            return Err("test request returned another operation".into());
        };
        let limits = crate::interpreter(source)?.limits;
        let mut outcomes: Vec<(String, Class, Outcome)> = Vec::new();
        let mut first_reference = None;
        for (index, test) in tests.iter().enumerate() {
            let config = hirsel_engine::RunConfiguration {
                entry: test.entry.clone(),
                hash_seed: launch.hash_seed,
                inputs: vec![],
            };
            let simulation = hirsel_engine::SimulationConfig {
                grants: Default::default(),
                random_seed: launch.random_seed,
                test_seed: test.seed,
                trace_roots,
                ..Default::default()
            };
            let input = batch.entry(&test.entry).map_err(|e| e.to_string())?;
            let execution =
                hirsel_driver::testing::observe_tir(&input, &config, simulation, limits, false)
                    .map_err(|e| e.to_string())?;
            let reference = execution
                .observation
                .as_ref()
                .map(Observation::of_reference)
                .ok_or("test reference has no observation")?;
            if reference.exit_code != Some(0) {
                return Err(format!("{} failed: {reference:?}", test.definition));
            }
            let test_dir = dir.join(format!("test-{index}"));
            std::fs::create_dir_all(&test_dir).map_err(|e| e.to_string())?;
            let mut ready = |options: &hirsel_native::Options| {
                let failure = |message: String| NativeError {
                    stage: StageName::Ready,
                    definition: None,
                    validator: false,
                    message,
                };
                let output = requests
                    .execute_request(&CompileRequest {
                        path: path.to_owned(),
                        inputs: inputs.clone().into(),
                        operation: CompileOperation::NativeBuild(NativeRequest {
                            definition: Some(test.entry.clone()),
                            test_entries: vec![],
                            supplemental_sources: vec![],
                            output: NativeOutput::Ready,
                            options: hirsel_native::BuildOptions::new(
                                options.clone(),
                                hirsel_native::Backend::Llvm,
                                hirsel_native::OptLevel::Default,
                            ),
                            run: None,

                            evidence: None,
                        }),
                    })
                    .map_err(|e| failure(e.to_string()))?;
                match &output.result {
                    CompileResult::NativeReady(ready) => Ok(ready.clone()),
                    CompileResult::NativeError(error) => Err(error.clone()),
                    _ => Err(failure(
                        "test native request returned another operation".into(),
                    )),
                }
            };
            let results = probe(&mut ready, test, &reference, &test_dir);
            if outcomes.is_empty() {
                outcomes = results;
            } else {
                for ((_, _, current), (_, _, result)) in outcomes.iter_mut().zip(results) {
                    if matches!(current, Outcome::Agree) {
                        *current = result;
                    }
                }
            }
            if first_reference.is_none() {
                first_reference = Some(reference);
            }
        }
        Ok(CaseResult {
            id: id.into(),
            snapshot: None,
            outcomes,
            reference: first_reference,
            wall: Default::default(),
        })
    };
    run().unwrap_or_else(|reason| CaseResult {
        id: id.into(),
        snapshot: Some(reason),
        outcomes: vec![],
        reference: None,
        wall: Default::default(),
    })
}

/// Run one source's real-Io record and replay (`real_io_replay`). Must be
/// called on a compiler stack.
pub fn replay_source(id: &str, source: &str, toolchain: &Toolchain, dir: &Path) -> CaseResult {
    replay_source_with_config(id, source, None, toolchain, dir, Default::default())
}
/// `path` selects the case's sidecar package graph, as `Case::run` does.
pub fn replay_source_with_config(
    id: &str,
    source: &str,
    path: Option<&Path>,
    toolchain: &Toolchain,
    dir: &Path,
    launch: hirsel_native::LaunchInputs,
) -> CaseResult {
    if source.lines().any(|line| line.trim() == "// expect: test") {
        return visit_test_suite(
            id,
            source,
            path,
            dir,
            launch.clone(),
            false,
            toolchain,
            |ready, test, reference, test_dir| {
                let launch = hirsel_native::LaunchInputs {
                    test_seed: test.seed,
                    ..launch.clone()
                };
                replay_campaign(
                    ready,
                    &test.entry,
                    reference,
                    toolchain,
                    test_dir,
                    launch,
                    &Default::default(),
                )
            },
        );
    }
    match checked_with_config(None, id, source, path, launch.clone(), false) {
        Err(reason) => CaseResult::failed(id, reason),
        Ok(prepared) => {
            let outcomes = replay_campaign(
                &mut |options| prepared.input.ready(options),
                &prepared.entry,
                &prepared.reference,
                toolchain,
                dir,
                launch,
                &prepared.fixture(source),
            );
            prepared.result(id, Ok(outcomes))
        }
    }
}

/// Run one source's Ready runner oracle under `matrix`'s native
/// configurations, cross-replaying tapes under the first. Must be called on a compiler stack.
pub fn oracle_source(
    id: &str,
    source: &str,
    path: Option<&Path>,
    matrix: &[Configuration],
    toolchain: &Toolchain,
    dir: &Path,
) -> CaseResult {
    let launch = matrix
        .first()
        .map(|c| c.options.launch.clone())
        .unwrap_or_default();
    if source.lines().any(|line| line.trim() == "// expect: test") {
        return visit_test_suite(
            id,
            source,
            path,
            dir,
            launch,
            false,
            toolchain,
            |ready, test, _, test_dir| {
                let matrix = test_seeded(matrix, test);
                oracle_campaign(
                    ready,
                    &test.entry,
                    &matrix,
                    toolchain,
                    test_dir,
                    &Default::default(),
                )
            },
        );
    }
    match checked_with_config(None, id, source, path, launch, false) {
        Err(reason) => CaseResult::failed(id, reason),
        Ok(prepared) => {
            let outcomes = oracle_campaign(
                &mut |options| prepared.input.ready(options),
                &prepared.entry,
                matrix,
                toolchain,
                dir,
                &prepared.fixture(source),
            );
            prepared.result(id, Ok(outcomes))
        }
    }
}

/// One case's smoke campaigns, each judged on its own.
pub struct Smoke {
    pub matrix: CaseResult,
    pub replay: CaseResult,
    /// For a case the smoke also builds with Cranelift.
    pub cranelift: Option<CaseResult>,
    pub oracle: CaseResult,
}

impl Smoke {
    /// Every campaign of `id` failing for `reason`.
    pub fn failed(id: &str, reason: &str, cranelift: bool) -> Smoke {
        let failed = || CaseResult::failed(id, reason.to_string());
        Smoke {
            matrix: failed(),
            replay: failed(),
            cranelift: cranelift.then(failed),
            oracle: failed(),
        }
    }
}

/// One case's smoke campaigns: `matrix`; real-Io record and replay under its
/// first launch inputs; `cranelift`, when given; and the runner oracle over its
/// native configurations, cross-replaying tapes under the first. The case is
/// checked and its reference run once, and the campaigns share every Ready
/// program and emission (`Builds`), so nothing is built twice. A run's
/// campaigns run on up to `jobs` threads; a test suite's run in turn. Must be
/// called on a compiler stack.
pub fn smoke_case(
    case: &Case,
    matrix: &[Configuration],
    cranelift: Option<&Configuration>,
    toolchain: &Toolchain,
    dir: &Path,
    jobs: usize,
) -> Smoke {
    let (id, source, path) = (case.id.as_str(), case.source.as_str(), case.path.as_deref());
    let _progress = hirsel_intern::test_progress::enter("native-case", || id.to_string());
    let launch = matrix
        .first()
        .map(|c| c.options.launch.clone())
        .unwrap_or_default();
    let cranelift = cranelift.map(std::slice::from_ref).unwrap_or_default();
    // Each campaign launches in its own directory, as it did in its own run.
    let campaigns = |dir: &Path| -> Result<[PathBuf; 4], String> {
        let dirs = ["matrix", "replay", "cranelift", "oracle"].map(|campaign| dir.join(campaign));
        for dir in &dirs {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(dirs)
    };
    if source.lines().any(|line| line.trim() == "// expect: test") {
        let natives = matrix
            .iter()
            .filter(|c| !matches!(c.engine, Engine::Heap { .. }))
            .count();
        let result = visit_test_suite(
            id,
            source,
            path,
            dir,
            launch.clone(),
            false,
            toolchain,
            |ready, test, reference, test_dir| {
                let builds = hirsel_native_testing::Builds::new(|options| ready(options));
                let ready = &mut |options: &hirsel_native::Options| builds.ready(options);
                let dirs = campaigns(test_dir).expect("test campaign scratch");
                let probe = Probe::default();
                let mut outcomes = test_matrix_campaign(
                    ready, test, reference, matrix, toolchain, &dirs[0], probe,
                );
                let launch = hirsel_native::LaunchInputs {
                    test_seed: test.seed,
                    ..launch.clone()
                };
                outcomes.extend(replay_campaign(
                    ready,
                    &test.entry,
                    reference,
                    toolchain,
                    &dirs[1],
                    launch,
                    &Default::default(),
                ));
                outcomes.extend(test_matrix_campaign(
                    ready, test, reference, cranelift, toolchain, &dirs[2], probe,
                ));
                outcomes.extend(oracle_campaign(
                    ready,
                    &test.entry,
                    &test_seeded(matrix, test),
                    toolchain,
                    &dirs[3],
                    &Default::default(),
                ));
                outcomes
            },
        );
        // Each test's outcomes are the campaigns' in order; split them back.
        let mut outcomes = result.outcomes.into_iter();
        let mut campaign = |count: usize| CaseResult {
            id: id.to_string(),
            snapshot: result.snapshot.clone(),
            outcomes: outcomes.by_ref().take(count).collect(),
            reference: result.reference.clone(),
            wall: Default::default(),
        };
        return Smoke {
            matrix: campaign(matrix.len()),
            replay: campaign(1),
            cranelift: (!cranelift.is_empty()).then(|| campaign(cranelift.len())),
            oracle: campaign(natives),
        };
    }
    let prepared = match checked_with_config(None, id, source, path, launch.clone(), false) {
        Err(reason) => return Smoke::failed(id, &reason, !cranelift.is_empty()),
        Ok(prepared) => prepared,
    };
    let dirs = match campaigns(dir) {
        Ok(dirs) => dirs,
        Err(reason) => return Smoke::failed(id, &reason, !cranelift.is_empty()),
    };
    let builds = hirsel_native_testing::Builds::new(|options| prepared.input.ready(options));
    let fixture = prepared.fixture(source);
    let probe = Probe::default();
    // The campaigns, costed by the native configurations each runs.
    let campaigns = [matrix.len() + 1, 1, cranelift.len(), matrix.len()];
    let tasks = (0..campaigns.len()).filter(|&c| campaigns[c] > 0).collect();
    let mut outcomes = hirsel_intern::jobs::map(
        hirsel_intern::jobs::Jobs::new(jobs),
        tasks,
        |&campaign| campaigns[campaign] as u64,
        |campaign| {
            let ready = &mut |options: &hirsel_native::Options| builds.ready(options);
            let dir = &dirs[campaign];
            match campaign {
                0 => matrix_campaign(ready, &prepared, source, matrix, toolchain, dir, probe),
                1 => Ok(replay_campaign(
                    ready,
                    &prepared.entry,
                    &prepared.reference,
                    toolchain,
                    dir,
                    launch.clone(),
                    &fixture,
                )),
                2 => matrix_campaign(ready, &prepared, source, cranelift, toolchain, dir, probe),
                _ => Ok(oracle_campaign(
                    ready,
                    &prepared.entry,
                    matrix,
                    toolchain,
                    dir,
                    &fixture,
                )),
            }
        },
    )
    .into_iter()
    .map(|outcomes| prepared.result(id, outcomes));
    let mut next = || outcomes.next().expect("one result per campaign");
    Smoke {
        matrix: next(),
        replay: next(),
        cranelift: (!cranelift.is_empty()).then(&mut next),
        oracle: next(),
    }
}

/// Run every case on `jobs` threads, each in its own scratch directory.
pub fn run_cases(
    cases: &[Case],
    matrix: &[Configuration],
    toolchain: &Toolchain,
    scratch: &Path,
    jobs: usize,
) -> Vec<CaseResult> {
    run_cases_with(cases, scratch, jobs, |case, dir| {
        case.run(matrix, toolchain, dir)
    })
}

/// `run_cases` with the per-case run given.
pub fn run_cases_with(
    cases: &[Case],
    scratch: &Path,
    jobs: usize,
    run: impl Fn(&Case, &Path) -> CaseResult + Sync,
) -> Vec<CaseResult> {
    run_each(
        cases,
        scratch,
        jobs,
        |case, dir| {
            let started = std::time::Instant::now();
            let mut result = run(case, dir);
            result.wall = started.elapsed();
            result
        },
        |case| CaseResult::failed(&case.id, "a native stage panicked".into()),
    )
}

/// Run every case on `jobs` threads, each in its own scratch directory and on a
/// compiler stack, in input order; a case whose run panics is `panicked`'s.
pub fn run_each<R: Send>(
    cases: &[Case],
    scratch: &Path,
    jobs: usize,
    run: impl Fn(&Case, &Path) -> R + Sync,
    panicked: impl Fn(&Case) -> R + Sync,
) -> Vec<R> {
    let (run, panicked) = (&run, &panicked);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for job in 0..jobs.max(1) {
            let (next, results) = (&next, &results);
            scope.spawn(move || {
                let dir = scratch.join(format!("job-{job}"));
                std::fs::create_dir_all(&dir).expect("scratch directory");
                hirsel_intern::jobs::with_compiler_stack(move || {
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(case) = cases.get(index) else { break };
                        std::fs::write(dir.join("case.txt"), &case.id)
                            .expect("scratch case identity");
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            run(case, &dir)
                        }))
                        .unwrap_or_else(|_| panicked(case));
                        results.lock().unwrap().push((index, result));
                    }
                });
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, r)| r).collect()
}

/// The checked-in exclusions: `(case, configuration) -> exclusion`.
pub type Exclusions = BTreeMap<(String, String), Exclusion>;

/// The engine class of a configuration an exclusion row names; `snapshot`
/// belongs to every class.
fn row_class(configuration: &str) -> Option<Option<Class>> {
    if configuration == SNAPSHOT {
        return Some(None);
    }
    artifact::configuration(configuration).map(|c| Some(Class::of(&c)))
}

/// Whether `case` has a row naming its snapshot or any configuration of
/// `class`.
pub fn listed_under(exclusions: &Exclusions, case: &str, class: Class) -> bool {
    exclusions
        .range((case.to_string(), String::new())..)
        .take_while(|((id, _), _)| id == case)
        .any(|((_, configuration), _)| {
            row_class(configuration).is_some_and(|c| c.is_none_or(|c| c == class))
        })
}

/// The checked-in exclusions, each naming a configuration of the current
/// matrix.
pub fn read_exclusions(root: &Path) -> Result<Exclusions, String> {
    let text =
        std::fs::read_to_string(root.join(EXCLUSIONS)).map_err(|e| format!("{EXCLUSIONS}: {e}"))?;
    let exclusions = parse_exclusions(&text)?;
    for (id, configuration) in exclusions.keys() {
        if row_class(configuration).is_none() {
            return Err(format!(
                "{EXCLUSIONS}: {id}: {configuration} is not a configuration of matrix {MATRIX_VERSION}"
            ));
        }
    }
    Ok(exclusions)
}

/// Exclusion rows as `render_exclusions` writes them, sorted and unique.
pub fn parse_exclusions(text: &str) -> Result<Exclusions, String> {
    let mut out = Exclusions::new();
    let mut previous = None;
    for (number, line) in text.lines().enumerate() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let [id, configuration, class, reason] = fields.as_slice() else {
            return Err(format!("{EXCLUSIONS}:{}: expected four fields", number + 1));
        };
        let key = (id.to_string(), configuration.to_string());
        if previous.as_ref().is_some_and(|p| *p >= key) {
            return Err(format!(
                "{EXCLUSIONS}:{}: rows must be sorted and unique",
                number + 1
            ));
        }
        previous = Some(key.clone());
        out.insert(
            key,
            Exclusion {
                class: class.to_string(),
                reason: reason.to_string(),
            },
        );
    }
    Ok(out)
}

pub fn render_exclusions(results: &[CaseResult]) -> String {
    let mut text = String::from(
        "# Baseline-passing executable cases a native configuration does not agree on, with\n\
         # the reason. Each row is an unmet promotion obligation, not a\n\
         # waiver. Regenerate with `native --full --bless` only after reading the diff:\n\
         # a new row is a regression unless its reason is new work.\n\
         # case\tconfiguration\tclass\treason\n",
    );
    let mut rows: Vec<_> = results
        .iter()
        .flat_map(|result| {
            result
                .exclusions(&[Class::Heap, Class::Llvm])
                .into_iter()
                .map(|(configuration, exclusion)| (result.id.clone(), configuration, exclusion))
        })
        .collect();
    rows.sort();
    for (id, configuration, exclusion) in rows {
        text.push_str(&format!(
            "{id}\t{configuration}\t{}\t{}\n",
            exclusion.class, exclusion.reason
        ));
    }
    text
}

/// The set rule's verdict over one run.
#[derive(Debug, Default)]
pub struct Report {
    pub baseline: usize,
    pub configurations: usize,
    /// Cases every configuration of the engine agrees on.
    pub pass: BTreeMap<Class, usize>,
    /// Excluded `(case, configuration)` pairs by class.
    pub excluded: BTreeMap<String, usize>,
    /// Disagreeing `(case, configuration)` pairs with no row, or whose reason changed.
    pub unlisted: Vec<String>,
    /// Exclusion rows whose configuration now agrees, or whose case or
    /// configuration a complete run no longer has.
    pub stale: Vec<String>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.unlisted.is_empty() && self.stale.is_empty()
    }
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "matrix_version": MATRIX_VERSION,
            "observation_schema": OBSERVATION_SCHEMA,
            "baseline_executable": self.baseline,
            "configurations": self.configurations,
            "heap_pass": self.pass.get(&Class::Heap).copied().unwrap_or(0),
            "llvm_pass": self.pass.get(&Class::Llvm).copied().unwrap_or(0),
            "excluded": self.excluded,
            "baseline_minus_native_unlisted": self.unlisted,
            "stale_exclusions": self.stale,
        })
    }
}

/// Compare a run with the checked-in exclusions. `complete` says the run
/// covered the whole baseline, so rows for cases it did not run are stale.
pub fn judge(
    results: &[CaseResult],
    exclusions: &Exclusions,
    configurations: usize,
    complete: bool,
) -> Report {
    judge_classes(
        results,
        exclusions,
        configurations,
        complete,
        &[Class::Heap, Class::Llvm],
    )
}

/// Judge only the engine classes the selected configuration matrix actually
/// ran: each case and configuration of them against its row.
pub fn judge_classes(
    results: &[CaseResult],
    exclusions: &Exclusions,
    configurations: usize,
    complete: bool,
    classes: &[Class],
) -> Report {
    let mut report = Report {
        baseline: results.len(),
        configurations,
        ..Report::default()
    };
    let mut judged = BTreeSet::new();
    for result in results {
        for &class in classes {
            if result.passes(class) {
                *report.pass.entry(class).or_default() += 1;
            }
        }
        let found: BTreeMap<String, Exclusion> = result.exclusions(classes).into_iter().collect();
        let ran = std::iter::once(SNAPSHOT).chain(
            result
                .outcomes
                .iter()
                .filter(|(_, class, _)| classes.contains(class))
                .map(|(name, _, _)| name.as_str()),
        );
        for configuration in ran {
            let key = (result.id.clone(), configuration.to_string());
            let listed = exclusions.get(&key);
            judged.insert(key);
            match (found.get(configuration), listed) {
                (None, None) => {}
                (None, Some(_)) => report.stale.push(format!(
                    "{} under {configuration}: now agrees; delete its exclusion row",
                    result.id
                )),
                (Some(found), listed) => {
                    *report.excluded.entry(found.class.clone()).or_default() += 1;
                    if listed != Some(found) {
                        report.unlisted.push(format!(
                            "{} under {configuration}: {} ({}){}",
                            result.id,
                            found.reason,
                            found.class,
                            match listed {
                                Some(l) => format!("; listed as {} ({})", l.reason, l.class),
                                None => String::new(),
                            }
                        ));
                    }
                }
            }
        }
    }
    if complete {
        for (id, configuration) in exclusions.keys() {
            if !judged.contains(&(id.clone(), configuration.clone())) {
                report.stale.push(format!(
                    "{id} under {configuration}: not a baseline case and configuration of this run"
                ));
            }
        }
    }
    report
}

/// The versioned case/configuration matrix as TSV: one row per case and
/// configuration, with what the configuration did.
pub fn render_matrix(results: &[CaseResult]) -> String {
    let mut text = format!(
        "# matrix {MATRIX_VERSION} observation {OBSERVATION_SCHEMA}\n# case\tconfiguration\tstatus\tdetail\n"
    );
    for result in results {
        if let Some(reason) = &result.snapshot {
            text.push_str(&format!("{}\tsnapshot\tunavailable\t{reason}\n", result.id));
        }
        for (name, _, outcome) in &result.outcomes {
            let (status, detail) = match outcome {
                Outcome::Agree => ("agree", String::new()),
                Outcome::Mismatch(actual) => ("mismatch", actual.render().replace('\n', " | ")),
                Outcome::Unavailable(error) => ("unavailable", reason(error)),
            };
            text.push_str(&format!("{}\t{name}\t{status}\t{detail}\n", result.id));
        }
    }
    text
}

/// `case<TAB>seconds` for every case, in case order.
pub fn render_walls(results: &[CaseResult]) -> String {
    let mut text = String::from("case\tseconds\n");
    for result in results {
        text.push_str(&format!(
            "{}\t{:.1}\n",
            result.id,
            result.wall.as_secs_f64()
        ));
    }
    text
}
