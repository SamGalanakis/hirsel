//! Source requests for standalone executable compilation.
use hirsel_engine::ToolError;
use hirsel_native::*;
use std::path::Path;
/// Check a source or manifest, compile it and write a directly executable artifact.
/// Source/checker errors remain separate from native tool outcomes, as for native run.
/// With `evidence`, accepted proofs remove their checks (PS-13) and the result
/// is the proof report; without, it is empty.
pub fn build_path_native(
    source: &Path,
    output: &Path,
    target: TargetSpec,
    profile: BuildProfile,
    evidence: Option<&super::Evidence>,
) -> Result<Result<Vec<String>, NativeError>, ToolError> {
    build_path_native_backend(
        source,
        output,
        target,
        profile,
        super::Backend::Llvm,
        evidence,
        false,
        crate::session::Policy::default(),
    )
}

/// `build_path_native` with `backend`, which lowers the same Ready LIR.
/// Release builds require LLVM.
pub fn build_path_native_backend(
    source: &Path,
    output: &Path,
    target: TargetSpec,
    profile: BuildProfile,
    backend: super::Backend,
    evidence: Option<&super::Evidence>,
    fuel: bool,
    policy: crate::session::Policy,
) -> Result<Result<Vec<String>, NativeError>, ToolError> {
    build_path_native_backend_with_validation(
        source,
        output,
        target,
        profile,
        backend,
        evidence,
        fuel,
        policy,
        super::Validation::Ready,
    )
}

/// `hirsel export --c-header` (or `--native-abi`, the JSON descriptor): the program's
/// published native data ABI for `target` (MEM-7, PROG-4).
pub fn export_data_abi(
    source: &Path,
    target: TargetSpec,
    header: bool,
) -> Result<Result<String, NativeError>, ToolError> {
    let compiler = crate::COMPILER;
    let mut requests =
        crate::requests::CompilerRequests::for_host(compiler.clone(), compiler.configuration());
    let output = requests.execute(source, crate::requests::CompileOperation::DataAbi)?;
    let crate::requests::CompileResult::NativeSources(facts) = &output.result else {
        return Err(ToolError::Runtime(
            "data ABI request returned another operation".into(),
        ));
    };
    let declarations =
        match hirsel_native_cache::declarations::NativeDeclarations::from_front(facts.clone()) {
            Ok(declarations) => declarations,
            Err(error) => {
                return Ok(Err(NativeError::new(
                    StageName::Manifest,
                    format!("native source declarations: {error:?}"),
                )));
            }
        };
    Ok(super::c_abi::data_abi(
        &declarations.program.types,
        &declarations.program.signatures,
        declarations
            .sources
            .iter()
            .map(|(path, source)| (path, source.representation.as_ref())),
        &target,
    )
    .map(|abi| {
        if header {
            abi.header()
        } else {
            abi.descriptor()
        }
    }))
}

/// Build with an explicit LIR validation policy. Harnesses use `EveryStage`.
pub fn build_path_native_backend_with_validation(
    source: &Path,
    output: &Path,
    target: TargetSpec,
    profile: BuildProfile,
    backend: super::Backend,
    evidence: Option<&super::Evidence>,
    fuel: bool,
    policy: crate::session::Policy,
    validation: super::Validation,
) -> Result<Result<Vec<String>, NativeError>, ToolError> {
    let options = match profile.options(backend, target, validation) {
        Ok(options) => options,
        Err(error) => return Ok(Err(error)),
    };
    let selected = crate::session::request(
        source,
        policy,
        crate::requests::CompileOperation::NativeBuild(crate::requests::NativeRequest {
            options,
            run: None,
            definition: None,
            test_entries: vec![],
            supplemental_sources: vec![],
            output: crate::requests::NativeOutput::Executable,
            fuel,
            evidence: evidence.cloned().map(std::sync::Arc::new),
        }),
    )?;
    match &selected.result {
        crate::requests::CompileResult::Native(input) => {
            Ok(super::request_output::materialize_output(input, output))
        }
        crate::requests::CompileResult::NativeError(error) => Ok(Err(error.clone())),
        _ => Err(ToolError::Runtime(
            "native build request returned another operation".into(),
        )),
    }
}
