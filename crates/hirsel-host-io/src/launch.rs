//! Bind explicit host launch policy to immutable checked source facts.
use hirsel_engine::ToolError;
use std::path::Path;
pub mod binding;
/// Explicit production launch input. Templates bind to the code selected by this launch.
#[derive(Clone, Copy)]
pub enum LaunchConfig<'a> {
    /// CLI placement override, applied to the validated record or grants template.
    Cores {
        input: &'a std::ffi::OsStr,
        grants: bool,
        arguments: Option<&'a [std::ffi::OsString]>,
        cores: u64,
    },
    Record(&'a std::ffi::OsStr),
    Grants {
        input: &'a std::ffi::OsStr,
        arguments: Option<&'a [std::ffi::OsString]>,
    },
}
impl LaunchConfig<'_> {
    fn read(
        self,
        revision: &hirsel_run_schema::ProgramRevision,
        launch: hirsel_run_schema::LaunchKind,
    ) -> Result<hirsel_run_config::ValidatedRun, hirsel_run_schema::RunRefusal> {
        match self {
            Self::Cores {
                input,
                grants,
                arguments,
                cores,
            } => {
                let config = if grants {
                    Self::Grants { input, arguments }
                } else {
                    Self::Record(input)
                };
                config.read(revision, launch)?.with_cores(cores)
            }
            Self::Record(input) => hirsel_run_config::RunConfig::record_input(input),
            Self::Grants { input, arguments } => {
                let run = hirsel_run_config::RunConfig::grants_input(
                    input,
                    revision.digest(),
                    revision.entry().into(),
                    launch,
                )?;
                match arguments {
                    None => Ok(run),
                    Some(arguments) => {
                        let mut record = run.record().clone();
                        record.args =
                            Some(hirsel_run_schema::snapshot_args(arguments.iter().cloned())?);
                        record.validated()
                    }
                }
            }
        }
    }
}

/// A production host launch: freshly admitted grants, optionally streaming every Io
/// completion to a tape, or the replay of such a tape (DET-1). A replay binds the run
/// its tape recorded, needs no grants and performs no host effect.
#[derive(Clone, Copy)]
pub enum HostLaunch<'a> {
    Live {
        config: LaunchConfig<'a>,
        record: Option<&'a Path>,
        revocations: Option<&'a Path>,
    },
    Replay(&'a Path),
    /// A callee run a runner host admits for an arrived call (RUN-10): its validated `call`
    /// record, and the tape it records to.
    Callee {
        run: &'a hirsel_run_config::ValidatedRun,
        record: Option<&'a Path>,
    },
}
impl HostLaunch<'static> {
    /// The launch without a record: the empty-grant record with all seeds zero
    /// (determinism.md, Run configuration), admitted like any template.
    pub fn empty() -> Self {
        Self::Live {
            config: LaunchConfig::Grants {
                input: std::ffi::OsStr::new("{}"),
                arguments: None,
            },
            record: None,
            revocations: None,
        }
    }
}

/// A host launch bound to the checked program, before entry.
pub enum HostRun {
    Live {
        admitted: hirsel_run_config::AdmittedRun,
        io: hirsel_rt::io_real::RealIo,
    },
    Replay {
        run: hirsel_run_config::ValidatedRun,
        io: hirsel_rt::io_sim::SimIo,
    },
}
/// A host launch bound to the checked program, before host admission.
pub enum BoundHostRunParts {
    Live(hirsel_run_config::BoundRun),
    Replay {
        run: hirsel_run_config::ValidatedRun,
        io: hirsel_rt::io_sim::SimIo,
    },
}

/// A launch whose grants or replay identity have bound to checked source facts.
pub struct BoundHostRun(BoundHostRunParts);
impl BoundHostRun {
    fn live(run: hirsel_run_config::BoundRun) -> Self {
        Self(BoundHostRunParts::Live(run))
    }
    fn replay(run: hirsel_run_config::ValidatedRun, io: hirsel_rt::io_sim::SimIo) -> Self {
        Self(BoundHostRunParts::Replay { run, io })
    }
    pub fn run(&self) -> &hirsel_run_config::ValidatedRun {
        match &self.0 {
            BoundHostRunParts::Live(run) => run.run(),
            BoundHostRunParts::Replay { run, .. } => run,
        }
    }
    pub fn is_replay(&self) -> bool {
        matches!(self.0, BoundHostRunParts::Replay { .. })
    }
    pub fn into_parts(self) -> BoundHostRunParts {
        self.0
    }
}

/// Parse an exact canonical logical definition path from a run record.
pub fn def_path(spelled: &str) -> Option<hirsel_syntax::DefPath> {
    let (package, rest) = spelled.split_once('@')?;
    let mut parts = rest.split('/');
    let version = parts.next()?;
    let mut modules: Vec<String> = parts.map(str::to_owned).collect();
    let name = modules.pop()?;
    let path = hirsel_syntax::DefPath(hirsel_syntax::ItemPath {
        package: hirsel_syntax::PackageId {
            name: package.into(),
            version: version.into(),
        },
        modules,
        name,
    });
    (path.to_string() == spelled).then_some(path)
}

/// Admit the already bound live launch once on `host`; its Io and journal
/// open only after the immutable source facts and explicit grants have bound.
pub fn admit_bound_host_run(
    bound: BoundHostRun,
    launch: HostLaunch<'_>,
    mut host: hirsel_rt::host_launch::HostGrants,
) -> Result<HostRun, ToolError> {
    let (record, revocations) = match &launch {
        HostLaunch::Live {
            record,
            revocations,
            ..
        } => (*record, *revocations),
        HostLaunch::Callee { record, .. } => (*record, None),
        HostLaunch::Replay(_) => (None, None),
    };
    match bound.into_parts() {
        BoundHostRunParts::Live(bound) => {
            if let Some(path) = revocations {
                host.read_revocations(path)
                    .map_err(|e| ToolError::Runtime(format!("launch: {e}")))?;
            }
            let admitted = bound
                .admit(&mut host, crate::admission_time()?)
                .map_err(ToolError::Launch)?;
            let mut io = host.into_io();
            // The interpreter reaches every operation, so it takes the host network and
            // process providers at once, as a compiled program's entries hand them over.
            hirsel_rt::io::Adapter::net(&mut io, hirsel_rt::net_host::provider);
            hirsel_rt::io::Adapter::process(&mut io, hirsel_rt::process::host);
            if admitted
                .run()
                .record()
                .io
                .as_ref()
                .and_then(|io| io.tasks.as_deref())
                != Some("tasks.free@1")
            {
                hirsel_rt::host_launch::journal(&mut io, &admitted, record, &[])
                    .map_err(|e| ToolError::Runtime(format!("launch: {e}")))?;
            }
            Ok(HostRun::Live { admitted, io })
        }
        BoundHostRunParts::Replay { run, io } => Ok(HostRun::Replay { run, io }),
    }
}

/// A callee entry's call schemas and its argument's codec program, from its checked signature.
pub fn call_schemas(
    signature: &hirsel_types::Signature,
    runtime: &hirsel_interp::aggregate::RuntimeTypes,
) -> Result<
    (
        hirsel_interp::CallSchemas,
        std::sync::Arc<hirsel_interp::aggregate::CodecProgram>,
    ),
    ToolError,
> {
    let invalid = || ToolError::Runtime("launch: a callee entry's signature".into());
    let hirsel_types::SignatureKind::Function(function) = &signature.kind else {
        return Err(invalid());
    };
    let schema = |ty| {
        runtime
            .codec(&runtime.types, ty)
            .ok()
            .flatten()
            .ok_or_else(invalid)
    };
    let Some(hirsel_types::TypeKind::Named { arguments, .. }) =
        runtime.types.get(function.result).map(|t| &t.kind)
    else {
        return Err(invalid());
    };
    let [
        hirsel_types::GenericArgument::Type(ok),
        hirsel_types::GenericArgument::Type(error),
    ] = arguments.as_ref()
    else {
        return Err(invalid());
    };
    let argument = function.parameters.last().ok_or_else(invalid)?.ty;
    Ok((
        hirsel_interp::CallSchemas {
            result: schema(*ok)?,
            error: schema(*error)?,
            failure: hirsel_interp::library::runners::failure_schema(runtime)
                .map_err(|_| invalid())?,
        },
        schema(argument)?,
    ))
}
