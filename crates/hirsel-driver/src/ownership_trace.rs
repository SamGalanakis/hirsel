//! Resource-only host execution diagnostics. This staged producer retains the
//! source outcome separately from trace refusal; it does not certify transports
//! that the reference observer cannot yet see.
use crate::{HostLaunch, RunOutcome, ToolError};
use hirsel_engine::InterpreterPhase;
use hirsel_interp::ownership_trace::TraceError;
use hirsel_run_schema::ownership_trace::Trace;
use std::path::Path;

pub struct ResourceRun {
    pub outcome: RunOutcome,
    /// Semantic program identity bound by the admitted host run.
    pub program: [u8; 32],
    pub trace: Result<Trace, TraceError>,
}

pub(crate) struct Capture {
    pub owned_data: bool,
    pub event_limit: usize,
    pub program: Option<[u8; 32]>,
    pub trace: Option<Result<Trace, TraceError>>,
}

/// Run the actual admitted source under reference execution or checked H.
/// Unsupported resource transitions return a trace error alongside the original
/// source outcome. They cannot turn the language run into a success or failure.
pub fn run_resources_on_host(
    path: &Path,
    launch: HostLaunch<'_>,
    max_steps: Option<u64>,
    policy: crate::session::Policy,
    checked: bool,
    event_limit: usize,
) -> Result<ResourceRun, ToolError> {
    run_on_host(path, launch, max_steps, policy, checked, event_limit, false)
}

/// Observe supported owned leaves under the same admitted source, grants and
/// checked execution as resource tracing. Unobserved producers fail closed.
pub fn run_owned_on_host(
    path: &Path,
    launch: HostLaunch<'_>,
    max_steps: Option<u64>,
    policy: crate::session::Policy,
    checked: bool,
    event_limit: usize,
) -> Result<ResourceRun, ToolError> {
    run_on_host(path, launch, max_steps, policy, checked, event_limit, true)
}

fn run_on_host(
    path: &Path,
    launch: HostLaunch<'_>,
    max_steps: Option<u64>,
    policy: crate::session::Policy,
    checked: bool,
    event_limit: usize,
    owned_data: bool,
) -> Result<ResourceRun, ToolError> {
    let mut capture = Capture {
        owned_data,
        event_limit,
        program: None,
        trace: None,
    };
    let selection = crate::launch_binding::LaunchSelection::read(launch)?;
    let input = crate::request_tir(path, policy, selection.entry().cloned())?;
    let (outcome, _) = crate::tir_execution::run_tir_on_host_traced(
        &input,
        launch,
        selection,
        max_steps,
        None,
        None,
        checked,
        None,
        Some(&mut capture),
    )?;
    Ok(ResourceRun {
        outcome,
        program: capture
            .program
            .ok_or_else(|| ToolError::Runtime("resource trace has no admitted program".into()))?,
        trace: capture.trace.ok_or_else(|| {
            ToolError::Runtime("resource trace execution did not complete".into())
        })?,
    })
}

pub struct ResourceSimulation {
    pub execution: hirsel_engine::ObservedExecution,
    pub program: [u8; 32],
    pub trace: Result<Trace, TraceError>,
}

pub fn execute_test_resources(
    input: &crate::requests::TirExecution,
    test: &crate::testing::Test,
    checked: bool,
    hash_seed: u64,
    limits: hirsel_interp::Limits,
    event_limit: usize,
) -> Result<ResourceSimulation, ToolError> {
    let program = input.revision.digest();
    crate::tir_execution::refuse_foreign_interpretation(input)?;
    let mut runtime = (*input.runtime).clone();
    if !checked {
        runtime.checked = None;
    }
    let mut adapter = hirsel_engine::InterpreterAdapter {
        limits,
        resource_binding: Some(hirsel_interp::ownership_trace::ResourceBinding {
            program,
            event_limit,
        }),
        ..Default::default()
    };
    adapter.prepare(
        input.signature.clone(),
        runtime,
        hirsel_engine::SimulationConfig {
            grants: Default::default(),
            test_seed: test.seed,
            scheduler: test.scheduler.clone(),
            debug: test.simulation_debug,
            ..Default::default()
        },
    );
    let config = hirsel_tir::RunConfiguration {
        entry: test.entry.clone(),
        hash_seed,
        inputs: vec![],
    };
    let execution = adapter
        .run_observed_query(&config, &mut input.provider(), &input.sources)
        .map_err(|error| match error {
            hirsel_engine::RuntimeError::Query(error) => ToolError::Query {
                error,
                phase: Some(hirsel_engine::QueryKind::Interpret),
                sources: input.sources.clone(),
            },
            hirsel_engine::RuntimeError::Tool(message) => ToolError::Runtime(message),
            hirsel_engine::RuntimeError::Io(error) => ToolError::Io(error),
        });
    let trace = adapter.take_resource_trace().unwrap_or_else(|| {
        Err(TraceError::Unsupported(
            "simulation did not complete ownership observations".into(),
        ))
    });
    Ok(ResourceSimulation {
        execution: execution?,
        program,
        trace,
    })
}

pub struct TestResources {
    pub result: crate::testing::TestResult,
    pub program: [u8; 32],
    pub trace: Result<Trace, TraceError>,
}

/// `hirsel test --trace-ownership`: each selected test runs once under the
/// ordinary simulator and is judged exactly as an untraced run judges it.
pub fn test_path_resources(
    path: &Path,
    filters: &[String],
    override_seed: Option<u64>,
    max_steps: Option<u64>,
    verify: hirsel_engine::Verify,
    event_limit: usize,
) -> Result<Vec<TestResources>, ToolError> {
    use crate::testing::{Selection, outcome_failure};
    let limits = hirsel_interp::Limits {
        steps: max_steps,
        ..Default::default()
    };
    {
        let (discovery, selector) = crate::testing::discover_with_selector(
            path,
            filters,
            override_seed,
            Selection::Tests,
            verify,
        )?;
        let execution = crate::testing::Executions::new(path, &discovery, selector, max_steps)?;
        let mut execution = execution.prepare_all()?;
        let mut results = vec![];
        for test in &discovery.tests {
            let input = execution.prepare(&test.entry)?;
            let traced = execute_test_resources(&input, test, false, 0, limits, event_limit)?;
            let stdout = match &traced.execution.observation {
                Some(observation) => observation.output.clone(),
                None => traced.execution.tooling_output.clone(),
            };
            let failure = match &traced.execution.outcome {
                Ok(outcome) => outcome_failure(outcome, &input.sources),
                Err(error) => Some(format!("runtime error: {error:?}")),
            };
            results.push(TestResources {
                result: test.result(failure, &stdout, false),
                program: traced.program,
                trace: traced.trace,
            });
        }
        return Ok(results);
    }
}
