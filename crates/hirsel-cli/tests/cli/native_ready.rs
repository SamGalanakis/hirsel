use hirsel_driver::native;
use hirsel_driver::requests;
use std::{path::Path, sync::Arc};

pub(crate) fn supplier(
    path: &Path,
    entry: &hirsel_engine::DefPath,
    toolchain: &hirsel_native::Toolchain,
) -> impl FnMut(&hirsel_native::Options) -> Result<Arc<requests::NativeReady>, hirsel_native::NativeError>
{
    let path = path.to_path_buf();
    let entry = entry.clone();
    let compiler = hirsel_front_phases::COMPILER;
    let mut compiler_requests =
        requests::CompilerRequests::for_host(compiler.clone(), compiler.configuration())
            .with_toolchain(Arc::new(toolchain.clone()));
    move |options| {
        let failed = |message| hirsel_native::NativeError {
            stage: hirsel_native::StageName::Ready,
            definition: None,
            validator: false,
            message,
        };
        let output = compiler_requests
            .execute(
                &path,
                requests::CompileOperation::NativeBuild(requests::NativeRequest {
                    definition: Some(entry.clone()),
                    test_entries: vec![],
                    supplemental_sources: vec![],
                    output: requests::NativeOutput::Ready,
                    options: hirsel_native::BuildOptions::new(
                        options.clone(),
                        hirsel_native::Backend::Llvm,
                        hirsel_native::OptLevel::Default,
                    ),
                    run: None,
                    fuel: false,
                    evidence: None,
                }),
            )
            .map_err(|error| failed(error.to_string()))?;
        match &output.result {
            requests::CompileResult::NativeReady(view) => Ok(view.clone()),
            requests::CompileResult::NativeError(error) => Err(error.clone()),
            requests::CompileResult::Check(checked)
            | requests::CompileResult::SafeCheck { checked, .. }
            | requests::CompileResult::CheckedProducts { checked, .. } => Err(failed(format!(
                "native request returned check result: completion={:?}, diagnostics={:?}",
                checked.completion, checked.diagnostics
            ))),
            _ => Err(failed(
                "native request returned an unexpected result".into(),
            )),
        }
    }
}
