use hirsel_engine::{ToolError, requests::observe_inputs};
use hirsel_native::Backend;
use hirsel_native::BuildOptions;
use hirsel_native::NativeError;
use hirsel_native::OptLevel;
use hirsel_native::Options;
use hirsel_native::StageName;
use hirsel_native::Toolchain;
use hirsel_requests::CompileOperation;
use hirsel_requests::CompileRequest;
use hirsel_requests::CompileResult;
use hirsel_requests::CompilerRequests;
use hirsel_requests::NativeOutput;
use hirsel_requests::NativeReady;
use hirsel_requests::NativeRequest;
use std::{collections::BTreeMap, path::Path, sync::Arc};

pub fn supplier(
    path: &Path,
    replacement: Option<&str>,
    entry: &hirsel_engine::DefPath,
    toolchain: &Toolchain,
) -> Result<
    impl FnMut(&Options) -> Result<Arc<NativeReady>, NativeError> + 'static + use<>,
    ToolError,
> {
    let compiler = hirsel_front_phases::COMPILER;
    let mut requests = CompilerRequests::for_host(compiler.clone(), compiler.configuration())
        .with_toolchain(Arc::new(toolchain.clone()));
    let path = path.to_path_buf();
    let (primary, inputs) = observe_inputs(
        requests.input_limits(),
        requests.capture_attempts(),
        |inputs| {
            let program = compiler.load_program_inputs(&path, inputs, false)?;
            let primary = program
                .engine
                .sources
                .file(program.primary)
                .ok_or(ToolError::SourceTooLarge)?
                .path
                .clone();
            drop(program);
            Ok::<_, ToolError>(primary)
        },
    )
    .map_err(|_| ToolError::Runtime("compiler inputs kept changing during discovery".into()))?;
    let primary = primary?;
    let inputs = match replacement {
        Some(source) => inputs.replace_sources(
            &BTreeMap::from([(primary, Arc::<[u8]>::from(source.as_bytes()))]),
            requests.input_limits().bytes,
        )?,
        None => inputs,
    };
    let entry = entry.clone();
    Ok(move |options: &Options| {
        let failed = |message: String| NativeError {
            stage: StageName::Ready,
            definition: None,
            validator: false,
            message,
        };
        let output = requests
            .execute_request(&CompileRequest {
                path: path.clone(),
                inputs: inputs.clone().into(),
                operation: CompileOperation::NativeBuild(NativeRequest {
                    definition: Some(entry.clone()),
                    test_entries: vec![],
                    supplemental_sources: vec![],
                    output: NativeOutput::Ready,
                    options: BuildOptions::new(options.clone(), Backend::Llvm, OptLevel::Default),
                    run: None,
                    fuel: false,
                    evidence: None,
                }),
            })
            .map_err(|error| failed(error.to_string()))?;
        match &output.result {
            CompileResult::NativeReady(ready) => Ok(ready.clone()),
            CompileResult::NativeError(error) => Err(error.clone()),
            _ => Err(failed("another operation".into())),
        }
    })
}

pub fn source_supplier(
    source: &str,
    entry: &hirsel_engine::DefPath,
    toolchain: &Toolchain,
) -> Result<
    impl FnMut(&Options) -> Result<Arc<NativeReady>, NativeError> + 'static + use<>,
    ToolError,
> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("main.hir");
    std::fs::write(&path, source)?;
    std::fs::write(
        path.with_extension("hir.json"),
        serde_json::json!({
            "schema":1,"package":{"name":entry.0.package.name,"version":entry.0.package.version},
            "sources":[{"path":"main.hir","module":entry.0.modules}],"dependencies":[],
            "entry":{"module":entry.0.modules,"name":entry.0.name.as_str()}
        })
        .to_string(),
    )?;
    let mut ready = supplier(&path, None, entry, toolchain)?;
    Ok(move |options: &Options| {
        let _source_owner = &directory;
        ready(options)
    })
}
