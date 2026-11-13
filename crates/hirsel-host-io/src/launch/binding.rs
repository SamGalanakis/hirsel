//! Host-owned launch selection and binding over immutable checked source facts.
//! Reading a tape selects its entry before compiler roots are requested. Binding
//! grants or a recording never needs a retained mutable compiler program.
use super::*;
use hirsel_engine::ToolError;
pub use hirsel_engine_admission::EntryFacts;
use hirsel_run_schema::{CodeReach, LaunchKind, ProgramRevision};
use hirsel_syntax::DefPath;
use hirsel_types::{Signature, TypeInterner};

pub struct LaunchSelection {
    entry: Option<DefPath>,
    replay: Option<(hirsel_rt::io_sim::SimIo, hirsel_run_config::ValidatedRun)>,
    captured: Option<CapturedConfig>,
}

fn refused(error: hirsel_run_schema::RunRefusal) -> ToolError {
    ToolError::Runtime(format!("replay refused: {error}"))
}

impl LaunchSelection {
    pub fn read(launch: HostLaunch<'_>) -> Result<Self, ToolError> {
        let (entry, replay) = match launch {
            HostLaunch::Replay(tape) => {
                let (io, run) = hirsel_rt::host_launch::replay(tape).map_err(refused)?;
                (None, Some((io, run)))
            }
            HostLaunch::Live { .. } => (None, None),
        };
        Ok(Self {
            entry,
            replay,
            captured: None,
        })
    }

    /// Capture launch policy once. Its inputs select diagnostic emission, but
    /// only binding to the selected source below can admit an execution.
    pub fn read_for_compilation(launch: HostLaunch<'_>) -> Result<Self, ToolError> {
        let mut selected = Self::read(launch)?;
        selected.captured = match launch {
            HostLaunch::Live { config, .. } => Some(
                CapturedConfig::read(config)
                    .map_err(|error| ToolError::Runtime(error.to_string()))?,
            ),
            HostLaunch::Replay(_) => None,
        };
        Ok(selected)
    }

    pub fn compiler_inputs(&self) -> Result<hirsel_run_schema::Inputs, ToolError> {
        let record = self
            .captured
            .as_ref()
            .map(|captured| &captured.record)
            .or_else(|| self.replay.as_ref().map(|(_, run)| run.record()))
            .ok_or_else(|| ToolError::Runtime("ownership launch policy was not captured".into()))?;
        Ok(record.inputs.clone())
    }

    pub fn replay_product(&self) -> Option<([u8; 32], DefPath)> {
        let (_, run) = self.replay.as_ref()?;
        Some((
            {
                let text = &run.record().program;
                if text.len() != 64 {
                    return None;
                }
                let mut bytes = [0; 32];
                for (at, byte) in bytes.iter_mut().enumerate() {
                    *byte = u8::from_str_radix(&text[at * 2..at * 2 + 2], 16).ok()?;
                }
                bytes
            },
            super::def_path(&run.record().entry)?,
        ))
    }

    pub fn entry(&self) -> Option<&DefPath> {
        self.entry.as_ref()
    }

    pub fn requires_replayable(&self, launch: HostLaunch<'_>) -> bool {
        self.replay.is_some()
            || matches!(
                launch,
                HostLaunch::Live {
                    record: Some(_),
                    ..
                }
            )
    }

    pub fn bind(
        self,
        launch: HostLaunch<'_>,
        types: &TypeInterner,
        signature: &Signature,
        revision: &ProgramRevision,
        code: CodeReach,
    ) -> Result<BoundHostRun, ToolError> {
        let facts = self.facts(launch, types, signature, revision, code)?;
        self.bind_facts(launch, &facts)
    }

    /// The checked program's binding facts for this launch's kind.
    pub fn facts(
        &self,
        launch: HostLaunch<'_>,
        types: &TypeInterner,
        signature: &Signature,
        revision: &ProgramRevision,
        code: CodeReach,
    ) -> Result<EntryFacts, ToolError> {
        let kind = hirsel_engine_admission::entry_launch_kind(types, signature)
            .map_err(ToolError::Launch)?;
        Ok(EntryFacts::of(types, signature, revision, kind, code))
    }

    /// Bind this launch to the program's facts, computed from the checked program or read
    /// from the record that answers the run.
    pub fn bind_facts(
        self,
        launch: HostLaunch<'_>,
        facts: &EntryFacts,
    ) -> Result<BoundHostRun, ToolError> {
        let (revision, kind) = (facts.revision(), facts.launch());
        match (launch, self.replay) {
            (HostLaunch::Live { config, .. }, _) => {
                let requested = match self.captured {
                    Some(captured) => captured.bind(revision, kind),
                    None => config.read(revision, kind),
                }
                .map_err(|e| ToolError::Runtime(e.to_string()))?;
                let bound = facts.bind(Some(&requested)).map_err(ToolError::Launch)?;
                Ok(BoundHostRun::live(bound))
            }
            (HostLaunch::Replay(_), Some((io, run))) => {
                let roots = facts.roots().map_err(refused)?;
                let run = run
                    .bind(&hirsel_run_config::ProgramFacts::of(
                        revision,
                        kind,
                        roots,
                        facts.code().clone(),
                    ))
                    .map_err(refused)?
                    .into_run();
                Ok(BoundHostRun::replay(run, io))
            }
            (HostLaunch::Replay(_), None) => unreachable!("a replay reads its tape first"),
        }
    }
}

// A grants template has no program authority until its source binding is filled
// and validated. A supplied bound record retains its original identity fields.
struct CapturedConfig {
    record: hirsel_run_config::RunConfig,
    template: bool,
    cores: Option<u64>,
}
impl CapturedConfig {
    fn read(config: super::LaunchConfig<'_>) -> Result<Self, hirsel_run_schema::RunRefusal> {
        use super::LaunchConfig;
        let (input, template, arguments, cores) = match config {
            LaunchConfig::Record(input) => (input, false, None, None),
            LaunchConfig::Grants { input, arguments } => (input, true, arguments, None),
            LaunchConfig::Cores {
                input,
                grants,
                arguments,
                cores,
            } => (input, grants, arguments, Some(cores)),
        };
        let mut record = if template {
            hirsel_run_config::RunConfig::grants_template(
                input,
                [0; 32],
                String::new(),
                LaunchKind::Main,
            )?
        } else {
            hirsel_run_config::RunConfig::record_template(input)?
        };
        if template && let Some(arguments) = arguments {
            record.args = Some(hirsel_run_schema::snapshot_args(arguments.iter().cloned())?);
        }
        Ok(Self {
            record,
            template,
            cores,
        })
    }
    fn bind(
        mut self,
        revision: &ProgramRevision,
        kind: LaunchKind,
    ) -> Result<hirsel_run_config::ValidatedRun, hirsel_run_schema::RunRefusal> {
        if self.template {
            self.record.program = hirsel_run_schema::hex(&revision.digest());
            self.record.entry = revision.entry().into();
            self.record.launch = kind;
        }
        let run = self.record.validated()?;
        match self.cores {
            Some(cores) => run.with_cores(cores),
            None => Ok(run),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_engine::{DefinitionQueries, QueryEngine};
    use std::sync::Arc;

    fn fixture(
        name: &str,
        parameters: &str,
    ) -> (
        Arc<TypeInterner>,
        hirsel_types::SignatureRef,
        ProgramRevision,
    ) {
        let mut engine = QueryEngine::new(
            hirsel_engine::default_environment(),
            hirsel_front_phases::compiler_adapters(),
        );
        let file = engine
            .sources
            .add(
                "immutable-launch.hir",
                format!("pub fn {name}({parameters})->Result[(),()]{{Ok(())}}"),
            )
            .unwrap();
        let context = hirsel_engine::default_context();
        assert!(
            hirsel_engine::check_snapshot(&mut engine, file, &context)
                .unwrap()
                .is_empty()
        );
        let mut path = hirsel_engine::entry_path(&context);
        path.0.name = name.into();
        let signature = engine.item_signature(&path).unwrap().value;
        let revision = engine.semantic_program_revision(&path).unwrap();
        (engine.types().clone(), signature, revision)
    }

    #[test]
    fn immutable_launch_binding_denies_ungranted_roots_and_binds_explicit_grants_after_engine_drop()
    {
        hirsel_intern::jobs::with_compiler_stack(|| {
            let (types, signature, revision) = fixture("main", "args:Args");
            let launch = HostLaunch::empty();
            let selected = LaunchSelection::read(launch).unwrap();
            assert!(selected.entry().is_none());
            assert!(!selected.requires_replayable(launch));
            assert!(matches!(
                selected.bind(launch, &types, &signature, &revision, CodeReach::Safe),
                Err(ToolError::Launch(
                    hirsel_run_schema::RunRefusal::NotGranted { .. }
                ))
            ));
            let granted = HostLaunch::Live {
                config: super::LaunchConfig::Grants {
                    input: std::ffi::OsStr::new("{\"args\":[]}"),
                    arguments: None,
                },
                record: None,
                revocations: None,
            };
            let selected = LaunchSelection::read(granted).unwrap();
            let BoundHostRunParts::Live(bound) = selected
                .bind(granted, &types, &signature, &revision, CodeReach::Safe)
                .unwrap()
                .into_parts()
            else {
                panic!("grants must bind a live launch")
            };
            assert_eq!(bound.run().record().entry, revision.entry());
            assert_eq!(bound.run().record().launch, LaunchKind::Main);
        });
    }

    #[test]
    fn ownership_policy_capture_keeps_inputs_and_grants_after_the_file_changes() {
        hirsel_intern::jobs::with_compiler_stack(|| {
            let (types, signature, revision) = fixture("main", "args:Args");
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("grants.json");
            std::fs::write(&path, r#"{"args":["kept"],"inputs":{"hash_seed":17,"random_seed":0,"test_seed":0,"scheduler":null},"budgets":{"provider":"budgets.limits@3","memory_bytes":1000}}"#).unwrap();
            let launch = HostLaunch::Live {
                config: super::LaunchConfig::Grants {
                    input: path.as_os_str(),
                    arguments: None,
                },
                record: None,
                revocations: None,
            };
            let selection = LaunchSelection::read_for_compilation(launch).unwrap();
            let inputs = selection.compiler_inputs().unwrap();
            assert_eq!(inputs.hash_seed, 17);
            std::fs::write(&path, "no longer a grants template").unwrap();
            let BoundHostRunParts::Live(bound) = selection
                .bind(launch, &types, &signature, &revision, CodeReach::Safe)
                .unwrap()
                .into_parts()
            else {
                panic!("captured grants bind a live run")
            };
            assert_eq!(bound.run().record().inputs, inputs);
            assert_eq!(bound.run().record().args.as_ref().unwrap(), &["kept"]);
            assert_eq!(bound.run().budgets().unwrap().memory_bytes, Some(1000));
            assert_eq!(bound.run().record().entry, revision.entry());
        });
    }

    #[test]
    fn ownership_policy_capture_keeps_a_bound_records_original_program_identity() {
        hirsel_intern::jobs::with_compiler_stack(|| {
            let (types, signature, revision) = fixture("main", "");
            let mut record = hirsel_run_config::RunConfig::for_program(&revision, LaunchKind::Main);
            record.program = hirsel_run_schema::hex(&[7; 32]);
            let json = serde_json::to_string(&record).unwrap();
            let launch = HostLaunch::Live {
                config: super::LaunchConfig::Record(std::ffi::OsStr::new(&json)),
                record: None,
                revocations: None,
            };
            let selection = LaunchSelection::read_for_compilation(launch).unwrap();
            assert!(
                selection
                    .bind(launch, &types, &signature, &revision, CodeReach::Safe)
                    .is_err()
            );
        });
    }
}
