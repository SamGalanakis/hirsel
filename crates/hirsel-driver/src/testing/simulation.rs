//! Fresh simulation state over immutable, already checked request products.
use crate::requests::TirExecution;
use hirsel_engine::{
    InterpreterPhase, ObservedExecution, RunConfiguration, SimulationConfig, ToolError,
};

/// Execute an already prepared entry without invoking the compiler. Each call
/// owns its interpreter, simulation, counters and checked-H state.
pub fn observe_tir(
    input: &TirExecution,
    run: &RunConfiguration,
    mut simulation: SimulationConfig,
    limits: hirsel_interp::Limits,
    checked: bool,
) -> Result<ObservedExecution, ToolError> {
    if run.entry != input.entry {
        return Err(ToolError::Runtime(
            "simulation entry differs from its prepared source".into(),
        ));
    }
    crate::tir_execution::refuse_foreign_interpretation(input)?;
    if !simulation.runners.is_empty() {
        simulation.callees = std::sync::Arc::new(input.callees.clone());
    }
    let mut runtime = (*input.runtime).clone();
    runtime.checked = if checked {
        // A computed checked runtime always names arena.with (MEM-4); a product
        // lacking those facts cannot run checked and must not run unchecked.
        if input.checked_runtime.arena_with.is_none() {
            return Err(ToolError::Runtime(
                "checked execution has no checked runtime".into(),
            ));
        }
        Some((*input.checked_runtime).clone())
    } else {
        None
    };
    let mut interpreter = hirsel_engine::InterpreterAdapter {
        limits,
        ..Default::default()
    };
    interpreter.prepare(input.signature.clone(), runtime, simulation);
    interpreter
        .run_observed_query(run, &mut input.provider(), &input.sources)
        .map_err(|error| match error {
            hirsel_engine::RuntimeError::Query(error) => ToolError::Query {
                error,
                phase: Some(hirsel_engine::QueryKind::Interpret),
                sources: input.sources.clone(),
            },
            hirsel_engine::RuntimeError::Tool(message) => ToolError::Runtime(message),
            hirsel_engine::RuntimeError::Io(error) => ToolError::Io(error),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::requests::{CompileOperation, CompileResult, CompilerRequests};

    #[test]
    fn prepared_simulations_match_scalar_engines_and_keep_state_per_execution() {
        hirsel_engine::with_compiler_stack(|| {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("hirsel.json");
            std::fs::write(
                directory.path().join("main.hir"),
                "pub fn main()->Result[(),()]{Ok(())}\n",
            )
            .unwrap();
            std::fs::write(&path, r#"{"schema":1,"package":{"name":"prepared_simulation","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
            let config = hirsel_engine::CompileConfig::default();
            let compiler = crate::COMPILER.with_config(config.clone());
            let mut scalar = compiler.load_program(&path).unwrap();
            let mut requests = CompilerRequests::for_host(compiler, config);
            let prepared = requests
                .execute(&path, CompileOperation::Tir { definition: None })
                .unwrap();
            let CompileResult::Tir(input) = &prepared.result else {
                panic!("expected TIR")
            };
            let run = RunConfiguration {
                entry: input.entry.clone(),
                hash_seed: 3,
                inputs: vec![],
            };
            for checked in [false, true, false] {
                let expected = if checked {
                    hirsel_engine::run_observed_checked_bounded(
                        &mut scalar.engine,
                        &run,
                        SimulationConfig::default(),
                        1_000_000,
                    )
                } else {
                    hirsel_engine::run_observed_snapshot_bounded(
                        &mut scalar.engine,
                        &run,
                        SimulationConfig::default(),
                        1_000_000,
                    )
                }
                .unwrap();
                let actual = observe_tir(
                    input,
                    &run,
                    SimulationConfig::default(),
                    hirsel_interp::Limits {
                        steps: Some(1_000_000),
                        ..Default::default()
                    },
                    checked,
                )
                .unwrap();
                assert_eq!(
                    format!("{:?}", actual.outcome.unwrap()),
                    format!("{:?}", expected.outcome.unwrap())
                );
                assert_eq!(actual.observation, expected.observation);
                assert_eq!(actual.step_usage, expected.step_usage);
            }
            let mut tampered = input.clone();
            std::sync::Arc::make_mut(&mut tampered).checked_runtime =
                std::sync::Arc::new(hirsel_interp::checked::CheckedRuntime::default());
            assert!(
                matches!(
                    observe_tir(&tampered, &run, SimulationConfig::default(), Default::default(), true),
                    Err(ToolError::Runtime(message)) if message.contains("no checked runtime")
                ),
                "checked=true without checked facts must fail loudly, not run unchecked"
            );
            observe_tir(
                &tampered,
                &run,
                SimulationConfig::default(),
                Default::default(),
                false,
            )
            .unwrap();
            drop(requests);
            let mut wrong = run;
            wrong.entry.0.name = "other".into();
            assert!(
                matches!(observe_tir(input, &wrong, SimulationConfig::default(), Default::default(), false), Err(ToolError::Runtime(message)) if message.contains("entry differs"))
            );
        });
    }
}
