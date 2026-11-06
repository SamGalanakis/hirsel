//! Independent read-once input discovery before a durable execution import.
use super::*;
use hirsel_engine::{Compiler, ToolError};
use std::{path::Path, sync::Arc};
pub fn import_replay_execution(
    path: &Path,
    compiler: Compiler,
    revision: [u8; 32],
    entry: DefPath,
) -> Result<Option<Arc<TirExecution>>, ToolError> {
    let config = compiler.configuration();
    let selector = CompilerRequests::for_host(compiler.clone(), config.clone());
    let (loaded, observed) = hirsel_engine::requests::observe_inputs(
        selector.input_limits(),
        selector.capture_attempts(),
        |inputs| compiler.load_check_package_inputs(path, inputs, false),
    )
    .map_err(|_| ToolError::Runtime("replay inputs kept changing during discovery".into()))?;
    let mut program = loaded?;
    let (imports, policy) = program.engine.execution_import_context();
    let options = hirsel_engine::execution_options(&config, program.engine.comptime_limits);
    let observations = CompileInputs::Frozen(observed).execution_identity();
    let allowance = selector
        .budget()
        .reserve_once(selector.limits().snapshot_bytes)
        .map_err(ToolError::Io)?;
    hirsel_engine::import_execution_product(
        revision,
        &entry,
        observations,
        imports,
        compiler.identity(),
        policy,
        options,
        allowance,
    )
    .map_err(|error| ToolError::Query {
        error,
        phase: None,
        sources: program.engine.sources.clone(),
    })
}
