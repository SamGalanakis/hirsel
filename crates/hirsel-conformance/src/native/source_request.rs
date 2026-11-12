use hirsel_engine::{
    CheckOutput, ToolError, loader_inputs::FrozenInputs, requests::observe_inputs,
};
use hirsel_native::Backend;
use hirsel_native::BuildOptions;
use hirsel_native::NativeError;
use hirsel_native::OptLevel;
use hirsel_native::Options;
use hirsel_native::StageName;
use hirsel_query_products::LoweringPolicy;
use hirsel_requests::*;
use hirsel_syntax::DefPath;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

pub(crate) struct Source {
    path: PathBuf,
    inputs: FrozenInputs,
    text: Arc<str>,
    primary: PathBuf,
    /// One request at a time; campaigns on other threads wait for it.
    requests: Mutex<CompilerRequests>,
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
                let path = directory.as_ref().unwrap().dir().join("case.hir");
                std::fs::write(&path, source)?;
                let entry = crate::selected_entry(source, &hirsel_engine::default_context());
                std::fs::write(
                    path.with_extension("hir.json"),
                    serde_json::json!({
                        "schema":1,"package":{"name":"standalone","version":"0"},
                        "sources":[{"path":"case.hir","module":[]}],"dependencies":[],
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
            text: source.into(),
            primary,
            entry,
            requests: Mutex::new(requests),
            _directory: directory,
        })
    }
    fn requests(&self) -> MutexGuard<'_, CompilerRequests> {
        self.requests.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub fn warm(&self, text: &str) -> Result<(), ToolError> {
        let mut requests = self.requests();
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
        self.requests().execute_request(&CompileRequest {
            path: self.path.clone(),
            inputs: self.inputs.clone().into(),
            operation,
        })
    }
    /// Temporary transport paths are not the standalone case's observation
    /// name. Keep every placed anchor and its logical artifact identity while
    /// giving reference and native observations the same stable diagnostic name.
    fn observation_sources(
        &self,
        sources: &hirsel_span::SourceMap,
    ) -> Result<hirsel_span::SourceMap, ToolError> {
        let mut relocated = sources.clone();
        if self._directory.is_none() {
            return Ok(relocated);
        }
        let placed = sources
            .anchors()
            .filter(|(_, file, _)| {
                sources
                    .file(*file)
                    .is_some_and(|file| file.path == self.primary)
            })
            .collect::<Vec<_>>();
        let mut aliases = BTreeMap::new();
        for (anchor, file, base) in placed {
            let alias = if let Some(alias) = aliases.get(&file) {
                *alias
            } else {
                let source = sources.file(file).ok_or(ToolError::SourceTooLarge)?;
                let alias = relocated
                    .add_logical("case.hir", source.logical_path(), source.text().to_owned())
                    .ok_or(ToolError::SourceTooLarge)?;
                aliases.insert(file, alias);
                alias
            };
            relocated.move_anchor(anchor, alias, base);
        }
        Ok(relocated)
    }
    pub fn check(&self) -> Result<CheckOutput, ToolError> {
        let output = self.execute(CompileOperation::Check {
            lowering: LoweringPolicy::Reachable,
        })?;
        match &output.result {
            CompileResult::Check(checked) => {
                let mut checked = checked.clone();
                checked.sources = self.observation_sources(&checked.sources)?;
                Ok(checked)
            }
            _ => Err(ToolError::Runtime("native source expected Check".into())),
        }
    }
    pub fn tir(&self) -> Result<Arc<TirExecution>, ToolError> {
        let output = self.execute(CompileOperation::Tir {
            definition: Some(self.entry.clone()),
        })?;
        match &output.result {
            CompileResult::Tir(input) => {
                let mut input = input.clone();
                let sources = self.observation_sources(&input.sources)?;
                Arc::make_mut(&mut input).sources = sources;
                Ok(input)
            }
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
        // The scratch file selects the package. Put the actual body in a named
        // source input before S0 so native sites retain the case's name, too.
        let (inputs, supplemental_sources) = if self._directory.is_some() {
            let requests = self.requests();
            let inputs = self
                .inputs
                .replace_sources(
                    &BTreeMap::from([(self.primary.clone(), Arc::<[u8]>::from([]))]),
                    requests.input_limits().bytes,
                )
                .map_err(|error| failure(error.to_string()))?;
            (
                inputs,
                vec![NativeSourceText {
                    name: "case.hir".into(),
                    context: hirsel_syntax::FileContext {
                        package: self.entry.0.package.clone(),
                        modules: self.entry.0.modules.clone(),
                    },
                    text: self.text.clone(),
                }],
            )
        } else {
            (self.inputs.clone(), vec![])
        };
        let output = self
            .requests()
            .execute_request(&CompileRequest {
                path: self.path.clone(),
                inputs: inputs.into(),
                operation: CompileOperation::NativeBuild(NativeRequest {
                    definition: Some(self.entry.clone()),
                    test_entries: vec![],
                    supplemental_sources,
                    output: NativeOutput::Ready,
                    options: BuildOptions::new(options.clone(), Backend::Llvm, OptLevel::Default),
                    run: None,

                    evidence: None,
                    platform: None,
                }),
            })
            .map_err(|error| failure(error.to_string()))?;
        match &output.result {
            CompileResult::NativeReady(ready) => Ok(ready.clone()),
            CompileResult::NativeError(error) => Err(error.clone()),
            _ => Err(failure("native source expected Ready".into())),
        }
    }
}
