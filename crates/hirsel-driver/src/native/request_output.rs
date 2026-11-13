//! Host use of a completed immutable native request. Build destinations use
//! the selected ordinary provider; runs share the existing process launcher
//! and observation decoder without reconstructing a native program.
use crate::requests::NativeExecution;
use crate::{BoundHostRun, HostLaunch, RunOutcome, ToolError};
use hirsel_host_io::launch::BoundHostRunParts;
use hirsel_native::NativeError;
use hirsel_native::Scratch;
use hirsel_native::StageName;
use hirsel_native::link;
use hirsel_native::object_queries;
use std::{io::Write, path::Path};

pub(crate) fn materialize_output(
    input: &NativeExecution,
    destination: &Path,
) -> Result<Vec<String>, NativeError> {
    let error = |message| NativeError::new(StageName::Link, message);
    let linked = object_queries::linked_product(input.store.as_ref(), input.product.clone())
        .map_err(error)?;
    object_queries::materialize_executable(input.store.as_ref(), &linked, destination)
        .map_err(error)?;
    input.validate_image(destination).map_err(error)?;
    Ok(input.report.clone())
}

/// What one run launches: a program image and the tables its launch reads. A
/// fresh request ([`Execution::request`]) and a run that a program record
/// answers ([`Execution::recorded`]) each construct one; [`run_bound_output`]
/// launches either the same way.
pub(crate) struct Execution<'a> {
    image: Image<'a>,
    pub target: &'a hirsel_native::TargetSpec,
    pub toolchain: &'a hirsel_native::Toolchain,
    /// The build's report lines, printed before the launch.
    pub report: &'a [String],
    /// The identity of the validated run the image was linked for.
    pub run: Option<[u8; 32]>,
    /// A reachable definition that calls an extern with no deterministic
    /// model, beside that extern.
    pub unmodeled: Option<(String, String)>,
    pub observation: &'a hirsel_native::NativeObservation,
    pub sources: &'a hirsel_span::SourceMap,
    /// Called once the image is written and validated, before the launch.
    pub validated: Option<&'a dyn Fn(&NativeExecution)>,
}

enum Image<'a> {
    /// A fresh request's linked product, validated as it is written.
    Request(&'a NativeExecution),
    /// A record's executable, already written to its scratch with the
    /// record's checks before the run committed to the record.
    Written {
        scratch: Scratch,
        runnable: hirsel_native::Runnable,
    },
}

impl<'a> Execution<'a> {
    pub(crate) fn request(input: &'a NativeExecution) -> Self {
        Self {
            image: Image::Request(input),
            target: &input.request.options.passes.target,
            toolchain: &input.toolchain,
            report: &input.report,
            run: input.run.as_ref().map(|run| run.identity()),
            unmodeled: input
                .foreign
                .unmodeled
                .first_key_value()
                .map(|(user, extern_)| (user.to_string(), extern_.to_string())),
            observation: &input.observation,
            sources: &input.sources,
            validated: None,
        }
    }

    /// Write `hit`'s executable to a new scratch as `program`, hashed against
    /// the record, with `launcher`'s runtime. The caller has checked that
    /// `hit` binds its image validation to `launcher`'s platform. An error
    /// is a miss: nothing has been launched or printed.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn recorded(
        hit: &hirsel_artifact_store::programs::ProgramHit,
        launcher: &'a crate::requests::HostLauncher,
        report: &'a [String],
        run: [u8; 32],
        unmodeled: Option<(String, String)>,
        observation: &'a hirsel_native::NativeObservation,
        sources: &'a hirsel_span::SourceMap,
    ) -> Result<Self, String> {
        let scratch = Scratch::new().map_err(|error| error.to_string())?;
        let program = scratch.0.join("program");
        hit.materialize(&program)
            .map_err(|error| error.to_string())?;
        let runnable = launcher.executable(program)?;
        Ok(Self {
            image: Image::Written { scratch, runnable },
            target: launcher.target(),
            toolchain: launcher.toolchain(),
            report,
            run: Some(run),
            unmodeled,
            observation,
            sources,
            validated: None,
        })
    }
}

impl Image<'_> {
    /// The scratch the run launches in and its launchable image; a request's
    /// image is written and validated here, then `validated` sees it.
    fn write(
        self,
        validated: Option<&dyn Fn(&NativeExecution)>,
    ) -> Result<(Scratch, hirsel_native::Runnable), NativeError> {
        match self {
            Image::Request(input) => {
                let scratch = Scratch::new()?;
                #[cfg(test)]
                super::tool::before_request_link(&scratch.0);
                let program = scratch.0.join("program");
                materialize_output(input, &program)?;
                let runnable = input
                    .executable(program)
                    .map_err(|message| NativeError::new(StageName::Launch, message))?;
                if let Some(validated) = validated {
                    validated(input);
                }
                Ok((scratch, runnable))
            }
            Image::Written { scratch, runnable } => Ok((scratch, runnable)),
        }
    }
}

pub(crate) fn run_bound_output(
    input: Execution<'_>,
    launch: HostLaunch<'_>,
    bound: BoundHostRun,
    output: super::tool::Output<'_>,
    trace_events: Option<&Path>,
) -> Result<Result<RunOutcome, NativeError>, ToolError> {
    if (bound.is_replay()
        || matches!(
            launch,
            HostLaunch::Live {
                record: Some(_),
                ..
            }
        ))
        && let Some((user, extern_)) = &input.unmodeled
    {
        return Err(ToolError::Runtime(format!(
            "launch: {user} calls the extern {extern_} and is not a wrapper with a \
             deterministic model, so this run cannot be recorded or replayed (PROG-4)"
        )));
    }
    let (run, tape) = match (bound.into_parts(), launch) {
        (
            BoundHostRunParts::Live(bound),
            HostLaunch::Live {
                record,
                revocations,
                ..
            },
        ) => {
            hirsel_rt::host_launch::recording_level(bound.run(), record.is_some())
                .map_err(|e| ToolError::Runtime(format!("launch: {e}")))?;
            (
                bound.into_run(),
                link::Tape::Live {
                    record,
                    revocations,
                },
            )
        }
        (BoundHostRunParts::Replay { run, .. }, HostLaunch::Replay(path)) => {
            (run, link::Tape::Replay(path))
        }
        _ => unreachable!("selected launch binding preserves its form"),
    };
    let (mut host_stdout, mut host_stderr) = (std::io::stdout(), std::io::stderr());
    let (inherit, json, stdout, stderr): (bool, bool, &mut dyn Write, &mut dyn Write) = match output
    {
        super::tool::Output::Inherit { json } => (true, json, &mut host_stdout, &mut host_stderr),
        super::tool::Output::Streams { stdout, stderr } => (false, false, stdout, stderr),
    };
    let observed = (|| {
        if input.run != Some(run.identity()) {
            return Err(NativeError::new(
                StageName::Launch,
                "native launch differs from the selected validated run",
            ));
        }

        let (scratch, runnable) = input.image.write(input.validated)?;
        std::fs::write(scratch.0.join("run.json"), run.launch_bytes())
            .map_err(|error| NativeError::new(StageName::Launch, error.to_string()))?;
        for line in input.report {
            writeln!(stderr, "hirsel: {line}")
                .map_err(|e| NativeError::new(StageName::Launch, e.to_string()))?;
        }
        let launched = link::launch_live(
            &runnable,
            input.target,
            input.toolchain,
            &scratch.0,
            tape,
            link::LiveRun {
                run: &run,
                trace_events,
                inherit,
                stdout,
                stderr,
            },
        )?;
        link::observe_request(&launched, input.observation, input.sources)
    })();
    match observed {
        Ok(link::Observed::Stopped(outcome)) => Ok(Ok(outcome)),
        Ok(link::Observed::Refused(reason)) => {
            Err(match hirsel_run_schema::RunRefusal::parse(&reason) {
                Some(refusal) => ToolError::Launch(refusal),
                None => ToolError::Runtime(format!("launch: {reason}")),
            })
        }
        Ok(link::Observed::Ran(observation, waits)) => Ok((|| {
            let sites = super::Sites::from_anchors(
                input.sources,
                input
                    .observation
                    .anchors
                    .iter()
                    .map(|(path, anchors)| (path, anchors.clone()))
                    .collect(),
            );
            for failure in &observation.failures {
                writeln!(stderr, "{}", sites.report(failure, json)?)
                    .map_err(|e| NativeError::new(StageName::Launch, e.to_string()))?;
            }
            observation.outcome(&sites, waits)
        })()),
        Err(error) => Ok(Err(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_native::NativeObservation;
    use hirsel_run_schema::{BudgetKind, Returned};

    fn launched(body: &str, exit: i32) -> link::Launched {
        link::Launched {
            audit_required: true,
            exit: Some(exit),
            signal: None,
            stdout: b"output bytes".to_vec(),
            stderr: vec![],
            record: format!(
                "hirsel-observation {}\n{body}",
                hirsel_rt::launch::OBSERVATION_VERSION
            ),
        }
    }

    #[test]
    fn immutable_native_tables_preserve_exit_budget_audit_and_malformed_record_checks() {
        let sources = hirsel_span::SourceMap::default();
        let metadata = NativeObservation::default();
        let sites = super::super::Sites::from_anchors(&sources, Default::default());
        for (body, expected) in [
            (
                "status 0\nresult 00000000\naudit 0 0 0 0 0\n",
                RunOutcome::Returned(Returned::Ok),
            ),
            (
                "status 0\nresult 01000000\naudit 0 0 0 0 0\n",
                RunOutcome::Returned(Returned::Error),
            ),
            (
                "budget_exceeded memory\naudit unavailable\n",
                RunOutcome::BudgetExceeded(BudgetKind::Memory),
            ),
        ] {
            let process = launched(body, expected.exit_code());
            let link::Observed::Ran(observation, waits) =
                link::observe_request(&process, &metadata, &sources).unwrap()
            else {
                panic!("a valid observed run must decode")
            };
            assert_eq!(observation.output, b"output bytes");
            assert_eq!(observation.outcome(&sites, waits).unwrap(), expected);
            let wrong_exit = launched(body, (expected.exit_code() + 1) % 7);
            assert!(link::observe_request(&wrong_exit, &metadata, &sources).is_err());
        }
        for body in [
            "status 0\nresult 00000000\naudit 1 0 0 0 0\n",
            "status 0\nresult 00000000\nunknown record\n",
            "failure 0 1 0000000000000001 0000000000000000\naudit 0 0 0 0 0\n",
        ] {
            assert!(link::observe_request(&launched(body, 0), &metadata, &sources).is_err());
        }
    }

    #[test]
    fn immutable_native_failure_tables_resolve_selected_anchors_and_refuse_unknown_sites() {
        let mut sources = hirsel_span::SourceMap::default();
        let file = sources
            .add_logical(
                "/physical/main.hir",
                "standalone@0/main.hir",
                "fn main() {}".into(),
            )
            .unwrap();
        let anchor = sources.file(file).unwrap().anchor;
        let path = hirsel_engine::entry_path(&hirsel_engine::default_context());
        let metadata = NativeObservation {
            failure_rules: vec!["DIV-1".into()],
            sites: [(
                0xa5,
                (
                    hirsel_syntax::DefId::intern(&path),
                    hirsel_lir::ir::SiteRange {
                        source: 0,
                        start: 1,
                        end: 2,
                    },
                ),
            )]
            .into_iter()
            .collect(),
            anchors: [(path, vec![anchor])].into_iter().collect(),
        };
        let body = "status 1\nfailure 0 1 00000000000000a5 0000000000000000\naudit 0 0 0 0 0\n";
        let process = launched(body, hirsel_run_schema::exit::FAILED);
        let link::Observed::Ran(observed, _) =
            link::observe_request(&process, &metadata, &sources).unwrap()
        else {
            panic!("selected failure metadata must resolve")
        };
        assert_eq!(observed.failures.len(), 1);
        assert_eq!(observed.failures[0].rule, "DIV-1");
        assert_eq!(observed.failures[0].file, "/physical/main.hir");
        assert_eq!(
            (observed.failures[0].line, observed.failures[0].column),
            (1, 2)
        );
        let mut missing = metadata.clone();
        missing.sites.clear();
        assert!(link::observe_request(&process, &missing, &sources).is_err());
        let mut wrong_anchor = metadata;
        wrong_anchor.anchors.clear();
        assert!(link::observe_request(&process, &wrong_anchor, &sources).is_err());
    }
}
