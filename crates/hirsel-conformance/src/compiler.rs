use crate::*;
use hirsel_engine::{QueryError, ToolError};
use hirsel_requests::CompileOperation;
use hirsel_requests::CompileOutput;
use hirsel_requests::CompileRequest;
use hirsel_requests::CompileResult;
use hirsel_requests::CompilerRequests;

struct RequestedProgram {
    path: std::path::PathBuf,
    inputs: hirsel_requests::CompileInputs,
    requests: CompilerRequests,
}
impl RequestedProgram {
    fn execute(
        &mut self,
        operation: CompileOperation,
    ) -> Result<std::sync::Arc<CompileOutput>, ToolError> {
        self.requests.execute_request(&CompileRequest {
            path: self.path.clone(),
            inputs: self.inputs.clone(),
            operation,
        })
    }

    fn check(
        &mut self,
        lowering: hirsel_engine::LoweringPolicy,
    ) -> Result<(hirsel_engine::CheckOutput, Option<String>), ToolError> {
        let output = self.execute(CompileOperation::Check { lowering })?;
        let CompileResult::Check(checked) = &output.result else {
            return Err(ToolError::Runtime(
                "corpus Check returned another operation".into(),
            ));
        };
        let phase = checked
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.severity == Severity::Error)
            .and_then(|diagnostic| output.diagnostic_phase(diagnostic));
        Ok((checked.clone(), phase))
    }
}

/// Bug-catching campaigns independently recompute every reusable result.
/// Each compiler owns an ephemeral memory provider, so unrelated cases cannot
/// accumulate results for the lifetime of a campaign. Oracle cold runs keep none@1.
pub(crate) fn verifying_compiler() -> hirsel_engine::Compiler {
    let config = hirsel_engine::CompileConfig::default();
    let cache_bytes = usize::try_from(config.hold_bytes).unwrap_or(usize::MAX);
    hirsel_front_phases::COMPILER.with_config(hirsel_engine::CompileConfig {
        retention: hirsel_engine::memo::Retention::Batch,
        store: std::sync::Arc::new(
            hirsel_engine::store::MemStore::bounded(cache_bytes).with_installed_products(),
        ),
        verify_store: hirsel_engine::store::VerifyStore::Recompute,
        ..config
    })
}

#[derive(Clone, Debug, Default)]
pub struct Observation {
    pub actual: String,
    pub phase: String,
    /// Checked execution differed from normal execution.
    pub checked: Option<String>,
    /// First diagnostic/failure's token identity, independent of formatted lines.
    pub primary: Option<crate::corpus_format::Primary>,
    /// Its coordinates belong to a dependency, so formatting this case cannot move them.
    pub primary_external: bool,
}
pub fn evaluate(path: &Path, expect: &Expect) -> (Status, Observation) {
    evaluate_with_hash_seed(path, expect, 0)
}
pub fn evaluate_with_hash_seed(
    path: &Path,
    expect: &Expect,
    hash_seed: u64,
) -> (Status, Observation) {
    hirsel_engine::with_compiler_stack(|| evaluate_on_compiler_thread(path, expect, hash_seed))
}
/// A diagnostic raised while indexing package sources, before any Check output.
fn source_rejection(
    expect: &Expect,
    d: &hirsel_diag::Diagnostic,
    sources: &hirsel_span::SourceMap,
) -> (Status, Observation) {
    let line = sources
        .location(d.primary)
        .map(|l| l.line as usize)
        .unwrap_or(0);
    let observation = Observation {
        actual: format!("{} at line {line}: {}", d.rule, d.message),
        phase: "source indexing".into(),
        ..Default::default()
    };
    let actual = Actual::Rejected {
        rule: d.rule.clone(),
        line,
    };
    (verdict(expect, &actual, &observation.actual), observation)
}
fn evaluate_on_compiler_thread(
    path: &Path,
    expect: &Expect,
    hash_seed: u64,
) -> (Status, Observation) {
    let compiler = verifying_compiler();
    let requests = CompilerRequests::for_host(compiler.clone(), compiler.configuration());
    let input_compiler = requests.compiler().clone();
    let (loaded, inputs) = match hirsel_engine::requests::observe_inputs(
        requests.input_limits(),
        requests.capture_attempts(),
        |inputs| input_compiler.load_program_inputs(path, inputs, false),
    ) {
        Ok(observed) => observed,
        Err(_) => {
            return (
                Status::Fail("corpus inputs changed during capture".into()),
                Observation::default(),
            );
        }
    };
    let mut requested = RequestedProgram {
        path: path.to_owned(),
        inputs: inputs.into(),
        requests,
    };
    let mut program = match loaded {
        Ok(program) => program,
        Err(ToolError::Query {
            error: QueryError::Diagnostics(diagnostics),
            sources,
            ..
        }) if !diagnostics.is_empty() => {
            return source_rejection(expect, &diagnostics[0], &sources);
        }
        Err(error) => {
            return (
                Status::Fail(format!("tool error: {error}")),
                Observation::default(),
            );
        }
    };
    let text = program
        .engine
        .sources
        .file(program.primary)
        .map(|source| source.text().to_owned())
        .unwrap_or_default();
    let limits = match crate::interpreter(&text) {
        Ok(interpreter) => {
            let limits = interpreter.limits;
            program.engine.adapters.interpreter = Some(interpreter);
            limits
        }
        Err(error) => return (Status::Fail(error), Observation::default()),
    };
    // Interpretation refuses a definition outside the entry's semantic closure
    // in every build, so every executed corpus file checks the closure.
    program.engine.closure_assertions = true;
    if matches!(expect, Expect::Test) {
        let mut observation = Observation {
            phase: "test execution".into(),
            ..Default::default()
        };
        let discovered = (|| {
            let discovery = hirsel_driver::testing::discover(
                &mut requested.requests,
                path,
                requested.inputs.clone(),
                &[],
                None,
                hirsel_driver::testing::Selection::Tests,
            )?;
            requested.inputs =
                discovery.execution_inputs(requested.requests.input_limits().bytes)?;
            let tests = discovery.tests;
            let output = requested.execute(CompileOperation::TirBatch {
                definitions: tests.iter().map(|test| test.entry.clone()).collect(),
            })?;
            let CompileResult::TirBatch(batch) = &output.result else {
                return Err(ToolError::Runtime(
                    "corpus tests returned another operation".into(),
                ));
            };
            Ok((tests, batch.clone()))
        })();
        let (tests, batch) = match discovered {
            Ok((tests, batch)) if !tests.is_empty() => (tests, batch),
            Ok(_) => {
                return (
                    Status::Fail("expect: test selected no @test functions".into()),
                    observation,
                );
            }
            Err(error) => return (Status::Fail(format!("test check: {error}")), observation),
        };
        for test in &tests {
            let mut execute = |checked| {
                let input = batch.entry(&test.entry)?;
                hirsel_driver::testing::execute(&input, test, checked, hash_seed, limits)
            };
            let ordinary = execute(false);
            let checked = execute(true);
            match (ordinary, checked) {
                (Ok(ordinary), Ok(checked)) => {
                    observation.checked = checked_divergence(&ordinary, &checked);
                    if !matches!(
                        ordinary.outcome,
                        Ok(hirsel_engine::RunOutcome::Returned(
                            hirsel_run_schema::Returned::Ok
                        ))
                    ) || observation.checked.is_some()
                    {
                        return (
                            Status::Fail(format!(
                                "{}: {:?}; checked: {:?}",
                                test.definition, ordinary.outcome, observation.checked
                            )),
                            observation,
                        );
                    }
                }
                (ordinary, checked) => {
                    return (
                        Status::Fail(format!(
                            "{}: ordinary {ordinary:?}; checked {checked:?}",
                            test.definition
                        )),
                        observation,
                    );
                }
            }
        }
        observation.actual = format!("{} tests", tests.len());
        return (Status::Pass, observation);
    }
    let context = program
        .files
        .iter()
        .find(|(file, _)| *file == program.primary)
        .unwrap()
        .1
        .clone();
    let entry = crate::selected_entry(&text, &context);
    // The verdict is `hirsel check`'s, which lowers only what compile-time
    // evaluation executes. Run and build then lower and validate every
    // reachable body, which must not change a single diagnostic.
    // A rejected program is checked again from a fresh load: a second check
    // of the same session sees the instances its first check selected.
    let mut diagnostic_phase = None;
    let mut rejected = None;
    let checked = (|| {
        use hirsel_engine::LoweringPolicy::{Comptime, Reachable};
        {
            let (comptime, phase) = match requested.check(Comptime) {
                Ok(checked) => checked,
                Err(ToolError::Query {
                    error: QueryError::Diagnostics(diagnostics),
                    sources,
                    ..
                }) if !diagnostics.is_empty() => {
                    rejected = Some(source_rejection(expect, &diagnostics[0], &sources));
                    return Err(String::new());
                }
                Err(e) => return Err(format!("{e:?}")),
            };
            diagnostic_phase = phase;
            let reachable = if comptime
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error)
            {
                let mut fresh = RequestedProgram {
                    path: requested.path.clone(),
                    inputs: requested.inputs.clone(),
                    requests: CompilerRequests::for_host(
                        compiler.clone(),
                        compiler.configuration(),
                    ),
                };
                fresh.check(Reachable)
            } else {
                requested.check(Reachable)
            };
            return Ok((comptime, reachable.map_err(|e| format!("{e:?}"))?.0));
        }
    })();
    if let Some(rejected) = rejected {
        return rejected;
    }
    let primary_path = program
        .engine
        .sources
        .file(program.primary)
        .map(|file| file.path.clone());
    let mut engine = program.engine;
    let mut observation = Observation {
        phase: engine
            .last_phase
            .map(|p| p.phase_name())
            .unwrap_or("tool input")
            .into(),
        ..Default::default()
    };
    let output = match checked {
        Ok((comptime, reachable))
            if located(&comptime.diagnostics, &comptime.sources)
                != located(&reachable.diagnostics, &reachable.sources)
                || comptime.completion != reachable.completion =>
        {
            observation.actual = format!(
                "check reported {:?} ({:?}), the reachable lowering {:?} ({:?})",
                comptime.diagnostics,
                comptime.completion,
                reachable.diagnostics,
                reachable.completion
            );
            return (Status::Fail(observation.actual.clone()), observation);
        }
        Ok((comptime, _)) => comptime,
        Err(error) => {
            observation.actual = format!("tool error: {error}");
            return (Status::Fail(observation.actual.clone()), observation);
        }
    };
    engine.sources = output.sources.clone();
    if let Some(d) = output
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error)
    {
        observation.primary_external = output
            .sources
            .file(output.sources.resolve(d.primary).file)
            .map(|file| &file.path)
            != primary_path.as_ref();
        observation.primary =
            crate::corpus_format::primary_at_span(&mut engine, d.rule.as_str(), d.primary);
        let line = output
            .sources
            .location(d.primary)
            .map(|l| l.line)
            .unwrap_or(0);
        observation.actual = format!("{} at line {line}: {}", d.rule, d.message);
        if let Some(phase) = diagnostic_phase.or_else(|| engine.diagnostic_phase(d)) {
            observation.phase = phase;
        }
        let actual = Actual::Rejected {
            rule: d.rule.clone(),
            line: line as usize,
        };
        return (verdict(expect, &actual, &observation.actual), observation);
    }
    if let hirsel_engine::CheckCompletion::Incomplete { phase } = output.completion {
        observation.actual = "not implemented".into();
        observation.phase = phase.phase_name().into();
        return (Status::NotImplemented, observation);
    }
    if matches!(expect, Expect::RefuseEntry) {
        use hirsel_engine::DefinitionQueries;
        let refused = engine.item_signature(&entry).is_ok_and(|signature| {
            matches!(hirsel_engine_admission::entry_launch_kind(engine.types(), &signature.value),
                Err(hirsel_run_schema::RunRefusal::Invalid { field, .. }) if field == "entry")
        });
        observation.phase = "entry admission".into();
        observation.actual = if refused {
            "refused entry"
        } else {
            "accepted entry"
        }
        .into();
        return (
            verdict(
                expect,
                &if refused {
                    Actual::RefusedEntry
                } else {
                    Actual::Accepted
                },
                &observation.actual,
            ),
            observation,
        );
    }
    observation.actual = "accept".into();
    if !matches!(
        expect,
        Expect::Run { .. } | Expect::Fail { .. } | Expect::Blocked { .. } | Expect::Signaled { .. }
    ) {
        return (
            verdict(expect, &Actual::Accepted, &observation.actual),
            observation,
        );
    }
    let config = hirsel_engine::RunConfiguration {
        entry,
        hash_seed,
        inputs: vec![],
    };
    let simulation = match crate::simulation(&text) {
        Ok(simulation) => simulation,
        Err(error) => return (Status::Fail(error), observation),
    };
    // The accepted TIR supplies the execution's source view. Release the
    // completed Check view before admitting that independent operation.
    engine.sources = Default::default();
    drop(output);
    observation.phase = "TIR preparation".into();
    let prepared = {
        let prepared = requested.execute(CompileOperation::Tir {
            definition: Some(config.entry.clone()),
        });
        match prepared {
            Ok(output) => match &output.result {
                CompileResult::Tir(input) => {
                    engine.sources = input.sources.clone();
                    input.clone()
                }
                _ => {
                    observation.actual = "corpus TIR returned another operation".into();
                    return (Status::Fail(observation.actual.clone()), observation);
                }
            },
            Err(ToolError::Query {
                error: QueryError::NotImplemented,
                phase,
                ..
            }) => {
                observation.actual = format!("TIR request unavailable at {phase:?}");
                return (Status::NotImplemented, observation);
            }
            Err(error) => {
                observation.actual = format!("tool error: {error}");
                return (Status::Fail(observation.actual.clone()), observation);
            }
        }
    };
    observation.phase = "interpretation".into();
    let mut execute = |checked| {
        hirsel_driver::testing::observe_tir(&prepared, &config, simulation.clone(), limits, checked)
    };
    let execution = match execute(false) {
        Ok(outcome) => outcome,
        Err(ToolError::Query {
            error: QueryError::NotImplemented,
            ..
        }) => {
            observation.actual = "not implemented".into();
            return (Status::NotImplemented, observation);
        }
        Err(error) => {
            observation.actual = format!("tool error: {error}");
            return (Status::Fail(observation.actual.clone()), observation);
        }
    };
    // Third witness: the same IR under checked execution must observe the same.
    observation.checked = match execute(true) {
        Ok(checked) => checked_divergence(&execution, &checked),
        Err(error) => Some(format!("checked execution: tool error: {error}")),
    };
    let stdout = execution
        .observation
        .as_ref()
        .map(|o| o.output.as_slice())
        .unwrap_or(&execution.tooling_output);
    if let Some(failure) = execution
        .observation
        .as_ref()
        .and_then(|o| o.failures.first())
    {
        let file = engine.sources.anchors().find_map(|(_, file, _)| {
            (engine.sources.file(file)?.path.to_string_lossy() == failure.stable_site.file)
                .then_some(file)
        });
        if let Some(file) = file {
            observation.primary_external =
                engine.sources.file(file).map(|file| &file.path) != primary_path.as_ref();
            observation.primary = crate::corpus_format::primary_at_site(
                &mut engine,
                file,
                failure.rule.as_str(),
                failure.stable_site.line as usize,
                failure.stable_site.column as usize,
            );
        }
    }
    let outcome = match execution.outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            let missing = matches!(
                &error,
                hirsel_engine::RuntimeError::Query(QueryError::NotImplemented)
            );
            let text = match error {
                hirsel_engine::RuntimeError::Query(error) => ToolError::Query {
                    error,
                    phase: Some(hirsel_engine::QueryKind::Interpret),
                    sources: prepared.sources.clone(),
                }
                .to_string(),
                hirsel_engine::RuntimeError::Io(error) => error.to_string(),
                hirsel_engine::RuntimeError::Tool(message) => message,
            };
            observation.actual = format!(
                "tool error: {text}; stdout {:?}",
                String::from_utf8_lossy(stdout)
            );
            return (
                if missing {
                    Status::NotImplemented
                } else {
                    Status::Fail(observation.actual.clone())
                },
                observation,
            );
        }
    };
    observation.actual = format!("{outcome:?}; stdout {:?}", String::from_utf8_lossy(stdout));
    let end = match execution.observation.as_ref() {
        Some(actual) => match (actual.failures.first(), actual.exit_code) {
            (Some(failure), _) => End::Failure {
                rule: failure.rule.clone(),
                line: failure.stable_site.line as usize,
            },
            (None, Some(code)) => End::Exit(code),
            (None, None)
                if actual.blocked.is_some() && matches!(outcome, RunOutcome::Blocked(_)) =>
            {
                End::Blocked
            }
            (None, None) => match (actual.signaled, &outcome) {
                (Some(kind), RunOutcome::Signaled(k)) if kind == *k => End::Signaled(kind),
                _ => End::Other,
            },
        },
        None => End::Other,
    };
    let actual = Actual::Ran {
        end,
        stdout: stdout.to_vec(),
    };
    (verdict(expect, &actual, &observation.actual), observation)
}
/// The status for what was observed, decided by the header's own matcher.
fn verdict(expect: &Expect, actual: &Actual, observed: &str) -> Status {
    if expect.matches(actual) {
        Status::Pass
    } else {
        Status::Fail(format!("expected {expect:?}; got {observed}"))
    }
}
/// Checked execution either reproduces the normal execution exactly or traps.
pub(crate) fn checked_divergence(
    normal: &hirsel_engine::ObservedExecution,
    checked: &hirsel_engine::ObservedExecution,
) -> Option<String> {
    if let Ok(RunOutcome::Trapped(trap)) = &checked.outcome {
        return Some(trap.to_string());
    }
    // Checked execution can run an operation normal execution still lacks (M2).
    if matches!(
        &normal.outcome,
        Err(hirsel_engine::RuntimeError::Query(
            QueryError::NotImplemented
        ))
    ) {
        return None;
    }
    let outcome = |e: &hirsel_engine::ObservedExecution| match &e.outcome {
        Ok(outcome) => format!("{outcome:?}"),
        Err(error) => format!("error {error:?}"),
    };
    let (normal_outcome, checked_outcome) = (outcome(normal), outcome(checked));
    if normal_outcome != checked_outcome {
        return Some(format!(
            "checked execution diverged: outcome {checked_outcome}, normally {normal_outcome}"
        ));
    }
    if normal.observation != checked.observation || normal.tooling_output != checked.tooling_output
    {
        return Some("checked execution diverged: observation differs".into());
    }
    None
}

/// Each diagnostic with its spans resolved to a file path and byte range, so a
/// fresh load's diagnostics, whose anchors are its own, compare by position.
fn located(
    diagnostics: &[hirsel_diag::Diagnostic],
    sources: &hirsel_span::SourceMap,
) -> Vec<(
    hirsel_diag::Diagnostic,
    Vec<Option<(std::path::PathBuf, u32, u32)>>,
)> {
    use hirsel_diag::Obligation;
    diagnostics
        .iter()
        .map(|diagnostic| {
            let mut diagnostic = diagnostic.clone();
            let mut sites = Vec::new();
            let mut locate = |span: &mut hirsel_span::Span| {
                let at = sources.resolve(*span);
                sites.push(
                    sources
                        .file(at.file)
                        .map(|file| (file.path.clone(), at.start, at.end)),
                );
                *span = hirsel_span::Span::default();
            };
            locate(&mut diagnostic.primary);
            diagnostic
                .secondary
                .iter_mut()
                .for_each(|label| locate(&mut label.span));
            if let Some(fix) = &mut diagnostic.fix {
                locate(&mut fix.span);
            }
            for obligation in &mut diagnostic.obligations {
                match obligation {
                    Obligation::GenericBound { arguments } => {
                        arguments.iter_mut().for_each(&mut locate)
                    }
                    Obligation::ParameterMode { argument } => locate(argument),
                    Obligation::Multiplicity | Obligation::CopyProof => {}
                }
            }
            (diagnostic, sites)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inner_unavailable_execution_preserves_stdout_in_conformance_observation() {
        hirsel_engine::with_compiler_stack(|| {
            let path = std::env::temp_dir().join(format!(
                "hirsel-conformance-api-error-{}.hir",
                std::process::id()
            ));
            fs::write(&path, "fn charge[T: copyable](x: T) -> u8 { match arena.bounded(64, fn(a: Arena) -> u8 { let c = copy_to(a, x); 0u8 }) { Ok(n) => n, Error(e) => 1u8 } }\npub fn main(rt: Io, args: Args) -> Result[(), GrantError] { let _ = out.print(io.stdout(rt)?, \"before unavailable\\n\"); let n = charge(7u8); Ok(()) }").unwrap();
            let (status, observation) = evaluate(
                &path,
                &Expect::Run {
                    stdout: "before unavailable\n".into(),
                    exit: 0,
                },
            );
            // Detailed interpreter refusals (a51a3c77) remain failures, with
            // their original cause and all stdout retained by both witnesses.
            assert_eq!(
                observation.actual,
                "tool error: launch unavailable: a bounded-arena charge of a generic type; stdout \"before unavailable\\n\""
            );
            assert_eq!(status, Status::Fail(observation.actual.clone()));
            assert!(observation.checked.is_none(), "{:?}", observation.checked);
            fs::remove_file(path).unwrap();
        });
    }
}
