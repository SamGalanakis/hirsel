//! Ready programs built by the request pipeline production uses, for the
//! harnesses that execute them: observation matrices, heap campaigns and
//! real-Io replay. A supplier owns one request engine and builds the selected
//! entry's Ready once per pass options it is asked for.
use super::{
    CompileOperation, CompileRequest, CompileResult, CompilerRequests, NativeOutput, NativeReady,
    NativeRequest, NativeSourceText,
};
use hirsel_engine::{Compiler, ToolError, loader_inputs::FrozenInputs, requests::observe_inputs};
use hirsel_native::{Backend, BuildOptions, NativeError, OptLevel, Options, StageName, Toolchain};
use hirsel_syntax::DefPath;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Builds `entry`'s Ready for each pass options.
pub type ReadyRequests =
    Box<dyn FnMut(&Options) -> Result<Arc<NativeReady>, NativeError> + Send + 'static>;

/// The project at `path`, with its primary source's bytes replaced by
/// `replacement` when given.
pub fn ready_supplier(
    path: &Path,
    replacement: Option<&str>,
    entry: &DefPath,
    toolchain: &Toolchain,
) -> Result<ReadyRequests, ToolError> {
    ready_supplier_with_compiler(
        path,
        replacement,
        entry,
        toolchain,
        hirsel_front_phases::COMPILER,
    )
}

pub fn ready_supplier_with_compiler(
    path: &Path,
    replacement: Option<&str>,
    entry: &DefPath,
    toolchain: &Toolchain,
    compiler: Compiler,
) -> Result<ReadyRequests, ToolError> {
    let (requests, primary, inputs) = capture(path, toolchain, compiler)?;
    let inputs = match replacement {
        Some(source) => inputs.replace_sources(
            &BTreeMap::from([(primary, Arc::<[u8]>::from(source.as_bytes()))]),
            requests.input_limits().bytes,
        )?,
        None => inputs,
    };
    Ok(supplier(
        requests,
        path.to_path_buf(),
        inputs,
        vec![],
        entry,
    ))
}

/// The project at `path` with its primary source replaced by `source`, whose
/// native sites name it `name`, as a reference run that loaded the project's
/// dependencies and held `source` as `name` does.
pub fn named_ready_supplier(
    path: &Path,
    source: &str,
    name: &str,
    entry: &DefPath,
    toolchain: &Toolchain,
) -> Result<ReadyRequests, ToolError> {
    let (requests, primary, inputs) = capture(path, toolchain, hirsel_front_phases::COMPILER)?;
    let inputs = inputs.replace_sources(
        &BTreeMap::from([(primary, Arc::<[u8]>::from([]))]),
        requests.input_limits().bytes,
    )?;
    Ok(supplier(
        requests,
        path.to_path_buf(),
        inputs,
        vec![entry_source(source, name, entry)],
        entry,
    ))
}

fn entry_source(source: &str, name: &str, entry: &DefPath) -> NativeSourceText {
    NativeSourceText {
        name: name.into(),
        context: hirsel_syntax::FileContext {
            package: entry.0.package.clone(),
            modules: entry.0.modules.clone(),
        },
        text: source.into(),
    }
}

/// One standalone source in `entry`'s package and modules, whose native sites
/// name it `name`, as a reference run over a source map holding it as `name`
/// does.
pub fn source_ready_supplier(
    source: &str,
    name: &str,
    entry: &DefPath,
    toolchain: &Toolchain,
) -> Result<ReadyRequests, ToolError> {
    source_ready_supplier_with_compiler(
        source,
        name,
        entry,
        toolchain,
        hirsel_front_phases::COMPILER,
    )
}

pub fn source_ready_supplier_with_compiler(
    source: &str,
    name: &str,
    entry: &DefPath,
    toolchain: &Toolchain,
    compiler: Compiler,
) -> Result<ReadyRequests, ToolError> {
    sources_ready_supplier_with_compiler(
        vec![entry_source(source, name, entry)],
        entry,
        toolchain,
        compiler,
    )
}

/// Standalone sources, each in its own package and modules and named as a
/// reference's source map names it, with `entry` among them. Sources that
/// name `std@1/` see the toolchain's std.
pub fn sources_ready_supplier(
    sources: Vec<NativeSourceText>,
    entry: &DefPath,
    toolchain: &Toolchain,
) -> Result<ReadyRequests, ToolError> {
    sources_ready_supplier_with_compiler(sources, entry, toolchain, hirsel_front_phases::COMPILER)
}

pub fn sources_ready_supplier_with_compiler(
    sources: Vec<NativeSourceText>,
    entry: &DefPath,
    toolchain: &Toolchain,
    compiler: Compiler,
) -> Result<ReadyRequests, ToolError> {
    let directory = tempfile::tempdir()?;
    // The scratch package selects the package and entry; its file is empty at
    // build time and the bodies arrive as source inputs named as the
    // reference names them, so native sites agree with the reference's.
    // A source naming `std@1/` compiles against the toolchain's std, as a
    // reference engine holding std does; otherwise the package stands alone.
    let dependencies = if sources.iter().any(|source| source.text.contains("std@1/")) {
        serde_json::json!([{
            "manifest": hirsel_engine::toolchain_std_manifest()?,
            "package": {"name": "std", "version": "1"},
            "expose": [],
        }])
    } else {
        serde_json::json!([])
    };
    let path = directory.path().join("package.hir");
    std::fs::write(&path, "")?;
    std::fs::write(
        path.with_extension("hir.json"),
        serde_json::json!({
            "schema":1,"package":{"name":entry.0.package.name,"version":entry.0.package.version},
            "sources":[{"path":"package.hir","module":entry.0.modules}],
            "dependencies":dependencies,
            "entry":{"module":entry.0.modules,"name":entry.0.name.as_str()}
        })
        .to_string(),
    )?;
    let (requests, _, inputs) = capture(&path, toolchain, compiler)?;
    let mut ready = supplier(requests, path, inputs, sources, entry);
    Ok(Box::new(move |options: &Options| {
        let _scratch_owner = &directory;
        ready(options)
    }))
}

/// A request engine for `path` and the inputs it observed, with the primary
/// source's observed path.
fn capture(
    path: &Path,
    toolchain: &Toolchain,
    compiler: Compiler,
) -> Result<(CompilerRequests, PathBuf, FrozenInputs), ToolError> {
    let requests = CompilerRequests::for_host(compiler.clone(), compiler.configuration())
        .with_toolchain(Arc::new(toolchain.clone()));
    let (primary, inputs) = observe_inputs(
        requests.input_limits(),
        requests.capture_attempts(),
        |inputs| {
            let program = compiler.load_program_inputs(path, inputs, false)?;
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
    Ok((requests, primary?, inputs))
}

fn supplier(
    mut requests: CompilerRequests,
    path: PathBuf,
    inputs: FrozenInputs,
    supplemental_sources: Vec<NativeSourceText>,
    entry: &DefPath,
) -> ReadyRequests {
    let entry = entry.clone();
    Box::new(move |options: &Options| {
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
                    supplemental_sources: supplemental_sources.clone(),
                    output: NativeOutput::Ready,
                    options: BuildOptions::new(options.clone(), Backend::Llvm, OptLevel::Default),
                    run: None,
                    evidence: None,
                    platform: None,
                }),
            })
            .map_err(|error| match &error {
                ToolError::Query {
                    error: hirsel_engine::QueryError::Diagnostics(diagnostics),
                    ..
                } => failed(format!("{error}: {diagnostics:?}")),
                _ => failed(error.to_string()),
            })?;
        match &output.result {
            CompileResult::NativeReady(ready) => Ok(ready.clone()),
            CompileResult::NativeError(error) => Err(error.clone()),
            _ => Err(failed("another operation".into())),
        }
    })
}
