use hirsel_driver::{
    native::{Backend, BuildOptions, NativeError, OptLevel, Options, StageName},
    requests::*,
};
use hirsel_engine::{
    CheckOutput, LoweringPolicy, ToolError, loader_inputs::FrozenInputs, requests::observe_inputs,
};
use hirsel_syntax::DefPath;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) struct Source {
    path: PathBuf,
    inputs: FrozenInputs,
    primary: PathBuf,
    requests: RefCell<CompilerRequests>,
    pub entry: DefPath,
    _directory: Option<super::scratch::RunRoot>,
}
impl Source {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn new(path: Option<&Path>, source: &str) -> Result<Self, ToolError> {
        let directory = if path.is_none() {
            Some(
                super::scratch::RunRoot::create_in(
                    &std::env::temp_dir().join("hirsel-native-source-requests"),
                )
                .map_err(ToolError::Runtime)?,
            )
        } else {
            None
        };
        let path = match path {
            Some(path) => path.to_owned(),
            None => {
                let path = directory.as_ref().unwrap().dir().join("source.hir");
                std::fs::write(&path, source)?;
                let entry = crate::selected_entry(source, &hirsel_engine::default_context());
                std::fs::write(
                    path.with_extension("hir.json"),
                    serde_json::json!({
                        "schema":1,"package":{"name":"standalone","version":"0"},
                        "sources":[{"path":"source.hir","module":[]}],"dependencies":[],
                        "entry":{"module":[],"name":entry.0.name.as_str()}
                    })
                    .to_string(),
                )?;
                path
            }
        };
        let compiler = crate::compiler::verifying_compiler();
        let requests = CompilerRequests::for_host(compiler.clone(), compiler.configuration());
        let (observed, inputs) = observe_inputs(
            requests.input_limits(),
            requests.capture_attempts(),
            |inputs| {
                let program = compiler.load_program_inputs(&path, inputs, false)?;
                Ok::<_, ToolError>((
                    program
                        .engine
                        .sources
                        .file(program.primary)
                        .unwrap()
                        .path
                        .clone(),
                    program.entry,
                ))
            },
        )
        .map_err(|_| ToolError::Runtime("native inputs changed during capture".into()))?;
        let (primary, entry) = observed?;
        let inputs = inputs.replace_sources(
            &BTreeMap::from([(primary.clone(), Arc::<[u8]>::from(source.as_bytes()))]),
            requests.input_limits().bytes,
        )?;
        Ok(Self {
            path,
            inputs,
            primary,
            entry,
            requests: RefCell::new(requests),
            _directory: directory,
        })
    }
    pub fn warm(&self, text: &str) -> Result<(), ToolError> {
        let mut requests = self.requests.borrow_mut();
        let inputs = self.inputs.replace_sources(
            &BTreeMap::from([(self.primary.clone(), Arc::<[u8]>::from(text.as_bytes()))]),
            requests.input_limits().bytes,
        )?;
        let _ = requests.execute_request(&CompileRequest {
            path: self.path.clone(),
            inputs: inputs.into(),
            operation: CompileOperation::Check {
                lowering: LoweringPolicy::Reachable,
            },
        });
        Ok(())
    }
    fn execute(&self, operation: CompileOperation) -> Result<Arc<CompileOutput>, ToolError> {
        self.requests.borrow_mut().execute_request(&CompileRequest {
            path: self.path.clone(),
            inputs: self.inputs.clone().into(),
            operation,
        })
    }
    pub fn check(&self) -> Result<CheckOutput, ToolError> {
        let output = self.execute(CompileOperation::Check {
            lowering: LoweringPolicy::Reachable,
        })?;
        match &output.result {
            CompileResult::Check(checked) => Ok(checked.clone()),
            _ => Err(ToolError::Runtime("native source expected Check".into())),
        }
    }
    pub fn tir(&self) -> Result<Arc<TirExecution>, ToolError> {
        let output = self.execute(CompileOperation::Tir {
            definition: Some(self.entry.clone()),
        })?;
        match &output.result {
            CompileResult::Tir(input) => Ok(input.clone()),
            _ => Err(ToolError::Runtime("native reference expected TIR".into())),
        }
    }
    pub fn ready(&self, options: &Options) -> Result<Arc<NativeReady>, NativeError> {
        let failure = |message: String| NativeError {
            stage: StageName::Ready,
            definition: None,
            validator: false,
            message,
        };
        let output = self
            .execute(CompileOperation::NativeBuild(NativeRequest {
                definition: Some(self.entry.clone()),
                test_entries: vec![],
                supplemental_sources: vec![],
                output: NativeOutput::Ready,
                options: BuildOptions::new(options.clone(), Backend::Llvm, OptLevel::Default),
                run: None,
                fuel: false,
                evidence: None,
            }))
            .map_err(|error| failure(error.to_string()))?;
        match &output.result {
            CompileResult::NativeReady(ready) => Ok(ready.clone()),
            CompileResult::NativeError(error) => Err(error.clone()),
            _ => Err(failure("native source expected Ready".into())),
        }
    }
}
