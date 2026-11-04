use hirsel_driver::testing;
use hirsel_driver::testing::Selection;
use hirsel_driver::testing::Test;
use hirsel_requests::*;
use std::{path::Path, sync::Arc};
pub fn suite(path: &Path, seed: Option<u64>) -> (Vec<Test>, Arc<TirBatchExecution>) {
    let (discovery, mut requests) = testing::discover_with_selector(
        path,
        &[],
        seed,
        Selection::Tests,
        hirsel_engine::Verify::On,
    )
    .unwrap();
    let inputs = discovery
        .execution_inputs(requests.input_limits().bytes)
        .unwrap();
    let tests = discovery.tests;
    let output = requests
        .execute_request(&CompileRequest {
            path: path.to_owned(),
            inputs: inputs.into(),
            operation: CompileOperation::TirBatch {
                definitions: tests.iter().map(|test| test.entry.clone()).collect(),
            },
        })
        .unwrap();
    let CompileResult::TirBatch(batch) = &output.result else {
        panic!("expected TIR batch")
    };
    (tests, batch.clone())
}
pub fn entry(path: &Path, definition: &hirsel_syntax::DefPath) -> Arc<TirExecution> {
    let compiler = hirsel_front_phases::COMPILER;
    let mut requests = CompilerRequests::for_host(compiler.clone(), compiler.configuration());
    let output = requests
        .execute(
            path,
            CompileOperation::Tir {
                definition: Some(definition.clone()),
            },
        )
        .unwrap();
    let CompileResult::Tir(input) = &output.result else {
        panic!("expected TIR")
    };
    input.clone()
}
