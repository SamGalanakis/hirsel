//! CT-5 source registration must settle before freezing an admitted front.
use hirsel_front_phases::COMPILER;
use hirsel_intern::jobs::with_compiler_stack;

#[test]
fn admitted_generator_text_waits_on_lower_with_one_worker_and_keeps_detached_evidence() {
    use hirsel_engine::*;
    use hirsel_engine_runtime::*;
    use hirsel_engine_store::MemStore;

    use hirsel_query_products::reads::ReadKey;
    use hirsel_query_products::{LoweringPolicy, QueryKind};

    use std::sync::Arc;
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(directory.path().join("main.hir"),
            "fn make()->Result[Body[fn()->u8 uses {}],String]{Ok(codegen.body(\"fn()->u8 uses {} {7u8}\"))}\npub fn main()->u8 uses {} = comptime make()\n").unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"generated_front","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
        for jobs in [1, 2] {
            let compiler = COMPILER
                .with_config(CompileConfig {
                    store: Arc::new(MemStore::default()),
                    jobs: hirsel_intern::jobs::Jobs::new(jobs),
                    hold_bytes: 0,
                    ..Default::default()
                })
                .with_owned_engines();
            let mut scalar = compiler.load_program(&manifest).unwrap();
            assert!(
                scalar
                    .check(LoweringPolicy::Comptime)
                    .unwrap()
                    .diagnostics
                    .is_empty()
            );
            let expected = scalar
                .engine
                .generated_source(&scalar.entry)
                .unwrap()
                .text
                .clone();
            drop(scalar);
            let mut program = compiler.load_program(&manifest).unwrap();
            let paths = program.engine.front_source_roots();
            let ordinary: Vec<_> = paths.into_iter().filter(|p| p != &program.entry).collect();
            let generators = vec![program.entry.clone()];
            let budget = hirsel_engine_admission::ProcessBudget::new(1 << 30);
            let errors = crate::scheduled_front::errors();
            let weight = MemoryWeight {
                retained: 2 << 20,
                scratch: 8 << 20,
                stack: 0,
            };
            let mut runtime = AdmittedRuntime::new(
                hirsel_intern::jobs::Jobs::new(jobs),
                budget.reserve(512 << 20).unwrap(),
                errors,
            )
            .unwrap();
            let front = program
                .engine
                .front_body_queries(&ordinary, weight, &budget, errors)
                .unwrap();
            let lower = program
                .engine
                .front_lower_queries(&ordinary, front.clone(), weight, 32, &budget, errors)
                .unwrap();
            let required = FrontRegistrationQueries::input_bytes(&generators);
            assert!(
                program
                    .engine
                    .front_registration_queries(
                        &generators,
                        lower.clone(),
                        weight,
                        &hirsel_engine_admission::ProcessBudget::new(required - 1),
                        errors
                    )
                    .is_err()
            );
            let registration = program
                .engine
                .front_registration_queries(&generators, lower.clone(), weight, &budget, errors)
                .unwrap();
            runtime.register(FRONT_BODY_KIND, front.factory()).unwrap();
            runtime
                .register_local(FRONT_LOWER_KIND, lower.factory())
                .unwrap();
            runtime
                .register_local(FRONT_EVALUATE_KIND, lower.factory())
                .unwrap();
            runtime
                .register_local(FRONT_REGISTRATION_KIND, registration.factory())
                .unwrap();
            let query = registration.query(&program.entry).unwrap();
            let request = runtime.request(&[query]);
            let products = runtime.wait(request).unwrap_or_else(|reference| {
                panic!(
                    "generator: {:?}",
                    registration
                        .failed(reference)
                        .map(|o| o.text().map(str::to_owned))
                )
            });
            let output = registration.outcome((products[0]).clone()).unwrap();
            drop(products);
            assert_eq!(output.text().unwrap(), expected);
            assert!(output.has_execution_acceptance());
            assert!(output.reads().iter().any(
                |(key, _)| matches!(key, ReadKey::ExecutedBody(id) if id.path().0.name == "make")
            ));
            assert!(
                program.engine.generated_source(&program.entry).is_none(),
                "private text never publishes a namespace"
            );
            runtime.shutdown();
            drop(runtime);
            drop(registration);
            drop(lower);
            drop(front);
            assert!(
                budget.reserved() > 0,
                "detached text keeps its original owners"
            );
            assert_eq!(output.text().unwrap(), expected);
            drop(output);
            assert_eq!(budget.reserved(), 0);
        }
    });
}

#[test]
fn admitted_generator_text_preserves_typed_errors_and_leaves_unrequested_siblings_idle() {
    generator_refusal_control(false);
}

#[test]
fn admitted_generator_text_refuses_zero_depth_before_lowering_or_execution() {
    generator_refusal_control(true);
}

fn generator_refusal_control(zero_depth: bool) {
    use hirsel_engine::*;
    use hirsel_engine_runtime::*;
    use hirsel_engine_store::MemStore;

    use hirsel_query_products::reads::ReadKey;
    use hirsel_query_products::{LoweringPolicy, QueryKind};

    use std::sync::Arc;
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(directory.path().join("main.hir"),
            "fn fail()->Result[Body[fn()->u8 uses {}],String]{Error(\"generator stop\")}\nfn untouched()->u8{9u8}\npub fn main()->u8 uses {} = comptime fail()\n").unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"generated_refusal","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
        for jobs in [1, 2] {
            let compiler = COMPILER
                .with_config(CompileConfig {
                    store: Arc::new(MemStore::default()),
                    jobs: hirsel_intern::jobs::Jobs::new(jobs),
                    hold_bytes: 0,
                    ..Default::default()
                })
                .with_comptime_limits(ComptimeLimits {
                    call_depth: if zero_depth { 0 } else { usize::MAX },
                    ..Default::default()
                })
                .with_owned_engines();
            let mut program = compiler.load_program(&manifest).unwrap();
            let ordinary: Vec<_> = program
                .engine
                .front_source_roots()
                .into_iter()
                .filter(|p| p != &program.entry)
                .collect();
            let budget = hirsel_engine_admission::ProcessBudget::new(1 << 30);
            let errors = crate::scheduled_front::errors();
            let weight = MemoryWeight {
                retained: 2 << 20,
                scratch: 8 << 20,
                stack: 0,
            };
            let mut runtime = AdmittedRuntime::new(
                hirsel_intern::jobs::Jobs::new(jobs),
                budget.reserve(512 << 20).unwrap(),
                errors,
            )
            .unwrap();
            let front = program
                .engine
                .front_body_queries(&ordinary, weight, &budget, errors)
                .unwrap();
            let lower = program
                .engine
                .front_lower_queries(&ordinary, front.clone(), weight, 32, &budget, errors)
                .unwrap();
            let registration = program
                .engine
                .front_registration_queries(
                    &[program.entry.clone()],
                    lower.clone(),
                    weight,
                    &budget,
                    errors,
                )
                .unwrap();
            runtime.register(FRONT_BODY_KIND, front.factory()).unwrap();
            runtime
                .register_local(FRONT_LOWER_KIND, lower.factory())
                .unwrap();
            runtime
                .register_local(FRONT_EVALUATE_KIND, lower.factory())
                .unwrap();
            runtime
                .register_local(FRONT_REGISTRATION_KIND, registration.factory())
                .unwrap();
            let request = runtime.request(&[registration.query(&program.entry).unwrap()]);
            let failed = runtime
                .wait(request)
                .expect_err("refusal must not publish successful text");
            let output = registration.failed(failed).unwrap();
            match output.text().unwrap_err() {
                QueryError::Operational(error) if zero_depth => {
                    assert_eq!(error.operation, "compile-time tooling limit");
                    assert!(error.message.contains("CallDepth"));
                }
                QueryError::Diagnostics(diagnostics) if !zero_depth => {
                    assert_eq!(diagnostics.len(), 1);
                    assert_eq!(diagnostics[0].rule.as_str(), "CT-1");
                    assert!(
                        diagnostics[0]
                            .notes
                            .iter()
                            .any(|note| note == "generator stop")
                    );
                }
                error => panic!("unexpected generator refusal: {error:?}"),
            }
            assert!(!output.has_execution_acceptance());
            assert!(program.engine.generated_source(&program.entry).is_none());
            let mut untouched = program.entry.clone();
            untouched.0.name = "untouched".into();
            assert_eq!(front.recompute_count(&untouched, QueryKind::Check), 0);
            if zero_depth {
                for path in &ordinary {
                    assert_eq!(front.recompute_count(path, QueryKind::Check), 0);
                    assert_eq!(lower.work(path).lowerings, 0);
                }
            }
            runtime.shutdown();
        }
    });
}

#[test]
fn accepted_generator_view_checks_and_lowers_the_literal_without_changing_raw_source() {
    use hirsel_engine::*;
    use hirsel_engine_runtime::*;
    use hirsel_engine_store::MemStore;

    use hirsel_query_products::reads::ReadKey;
    use hirsel_query_products::{LoweringPolicy, QueryKind};

    use std::sync::Arc;
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(directory.path().join("main.hir"),
            "fn make()->Result[Body[fn()->u8 uses {}],String]{Ok(codegen.body(\"fn()->u8 uses {} {7u8}\"))}\npub fn main()->u8 uses {} = comptime make()\n").unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"generated_front","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
        for jobs in [1, 2] {
            let compiler = COMPILER
                .with_config(CompileConfig {
                    store: Arc::new(MemStore::default()),
                    jobs: hirsel_intern::jobs::Jobs::new(jobs),
                    hold_bytes: 0,
                    ..Default::default()
                })
                .with_owned_engines();
            let mut scalar = compiler.load_program(&manifest).unwrap();
            assert!(
                scalar
                    .check(LoweringPolicy::Comptime)
                    .unwrap()
                    .diagnostics
                    .is_empty()
            );
            let expected = scalar
                .engine
                .generated_source(&scalar.entry)
                .unwrap()
                .text
                .clone();
            drop(scalar);
            let mut program = compiler.load_program(&manifest).unwrap();
            let paths = program.engine.front_source_roots();
            let ordinary: Vec<_> = paths.into_iter().filter(|p| p != &program.entry).collect();
            let generators = vec![program.entry.clone()];
            let budget = hirsel_engine_admission::ProcessBudget::new(1 << 30);
            let errors = crate::scheduled_front::errors();
            let weight = MemoryWeight {
                retained: 2 << 20,
                scratch: 8 << 20,
                stack: 0,
            };
            let mut runtime = AdmittedRuntime::new(
                hirsel_intern::jobs::Jobs::new(jobs),
                budget.reserve(512 << 20).unwrap(),
                errors,
            )
            .unwrap();
            let front = program
                .engine
                .front_body_queries(&ordinary, weight, &budget, errors)
                .unwrap();
            let lower = program
                .engine
                .front_lower_queries(&ordinary, front.clone(), weight, 32, &budget, errors)
                .unwrap();
            let required = FrontRegistrationQueries::input_bytes(&generators);
            assert!(
                program
                    .engine
                    .front_registration_queries(
                        &generators,
                        lower.clone(),
                        weight,
                        &hirsel_engine_admission::ProcessBudget::new(required - 1),
                        errors
                    )
                    .is_err()
            );
            let registration = program
                .engine
                .front_registration_queries(&generators, lower.clone(), weight, &budget, errors)
                .unwrap();
            runtime.register(FRONT_BODY_KIND, front.factory()).unwrap();
            runtime
                .register_local(FRONT_LOWER_KIND, lower.factory())
                .unwrap();
            runtime
                .register_local(FRONT_EVALUATE_KIND, lower.factory())
                .unwrap();
            runtime
                .register_local(FRONT_REGISTRATION_KIND, registration.factory())
                .unwrap();
            let query = registration.query(&program.entry).unwrap();
            let request = runtime.request(&[query]);
            let products = runtime.wait(request).unwrap_or_else(|reference| {
                panic!(
                    "generator: {:?}",
                    registration
                        .failed(reference)
                        .map(|o| o.text().map(str::to_owned))
                )
            });
            let output = registration.outcome((products[0]).clone()).unwrap();
            drop(products);
            assert_eq!(output.text().unwrap(), expected);
            assert!(output.has_execution_acceptance());
            let raw = program.engine.definition_items(&program.entry).unwrap()[0].clone();
            let accepted = program
                .engine
                .accept_front_registration(output.clone(), &budget)
                .unwrap();
            assert_eq!(
                program.engine.definition_items(&program.entry).unwrap()[0].text_hash,
                raw.text_hash
            );
            let all = program.engine.front_source_roots();
            assert!(
                program
                    .engine
                    .front_body_queries(&all, weight, &budget, errors)
                    .is_err()
            );
            let accepted_front = program
                .engine
                .front_body_queries_with_registrations(&all, &[accepted], weight, &budget, errors)
                .unwrap();
            let accepted_lower = program
                .engine
                .front_lower_queries(&all, accepted_front.clone(), weight, 64, &budget, errors)
                .unwrap();
            let mut checked_runtime = AdmittedRuntime::new(
                hirsel_intern::jobs::Jobs::new(jobs),
                budget.reserve(256 << 20).unwrap(),
                errors,
            )
            .unwrap();
            checked_runtime
                .register(FRONT_BODY_KIND, accepted_front.factory())
                .unwrap();
            checked_runtime
                .register_local(FRONT_LOWER_KIND, accepted_lower.factory())
                .unwrap();
            checked_runtime
                .register_local(FRONT_EVALUATE_KIND, accepted_lower.factory())
                .unwrap();
            let lowered = accepted_lower.query(&program.entry).unwrap();
            let request = checked_runtime.request(&[lowered]);
            let result = checked_runtime
                .wait(request)
                .unwrap_or_else(|failure| panic!("accepted Lower failed: {failure:?}, resource={}, protocol={}, typed={:?}, work={:?}, runtime={:?}", failure == errors.resource, failure == errors.protocol, accepted_lower.failure(failure), accepted_front.work_snapshot(), checked_runtime.stats()));
            let actual = accepted_lower.outcome((result[0]).clone()).unwrap();
            drop(result);
            assert_eq!(actual.output.value.path.path(), &program.entry);
            assert!(actual.execution.is_some());
            checked_runtime.shutdown();
            drop(checked_runtime);
            drop(actual);
            drop(accepted_lower);
            drop(accepted_front);

            runtime.shutdown();
            drop(runtime);
            drop(registration);
            drop(lower);
            drop(front);
            assert!(
                budget.reserved() > 0,
                "detached text keeps its original owners"
            );
            assert_eq!(output.text().unwrap(), expected);
            drop(output);
            drop(program);
            assert_eq!(budget.reserved(), 0);
        }
    });
}

const ACCEPTED_SOURCE: &str = "fn make()->Result[Body[fn()->u8 uses {}],String]{Ok(codegen.body(\"fn()->u8 uses {} {7u8}\"))}\npub fn main()->u8 uses {} = comptime make()\n";

fn accepted_output_control(
    jobs: usize,
    run: impl FnOnce(
        &mut hirsel_engine::LoadedProgram,
        &std::sync::Arc<hirsel_engine_admission::ProcessBudget>,
        hirsel_engine::FrontRegistrationOutput,
    ),
) {
    accepted_output_control_with(jobs, hirsel_engine::Verify::On, ACCEPTED_SOURCE, run);
}

pub(super) fn accepted_output_control_with(
    jobs: usize,
    verify: hirsel_engine::Verify,
    source: &str,
    run: impl FnOnce(
        &mut hirsel_engine::LoadedProgram,
        &std::sync::Arc<hirsel_engine_admission::ProcessBudget>,
        hirsel_engine::FrontRegistrationOutput,
    ),
) {
    use hirsel_engine::*;
    use hirsel_engine_runtime::*;
    use hirsel_engine_store::MemStore;

    use hirsel_query_products::reads::ReadKey;
    use hirsel_query_products::{LoweringPolicy, QueryKind};

    use std::sync::Arc;
    let directory = tempfile::tempdir().unwrap();
    let manifest = directory.path().join("hirsel.json");
    std::fs::write(directory.path().join("main.hir"), source).unwrap();
    std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"accepted_front","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
    let compiler = COMPILER
        .with_config(CompileConfig {
            store: Arc::new(MemStore::default()),
            jobs: hirsel_intern::jobs::Jobs::new(jobs),
            hold_bytes: 0,
            verify,
            ..Default::default()
        })
        .with_owned_engines();
    let mut program = compiler.load_program(&manifest).unwrap();
    let ordinary: Vec<_> = program
        .engine
        .front_source_roots()
        .into_iter()
        .filter(|path| path != &program.entry)
        .collect();
    let budget = hirsel_engine_admission::ProcessBudget::new(1 << 30);
    let errors = crate::scheduled_front::errors();
    let weight = MemoryWeight {
        retained: 2 << 20,
        scratch: 8 << 20,
        stack: 0,
    };
    let mut runtime = AdmittedRuntime::new(
        hirsel_intern::jobs::Jobs::new(jobs),
        budget.reserve(512 << 20).unwrap(),
        errors,
    )
    .unwrap();
    let front = program
        .engine
        .front_body_queries(&ordinary, weight, &budget, errors)
        .unwrap();
    let lower = program
        .engine
        .front_lower_queries(&ordinary, front.clone(), weight, 32, &budget, errors)
        .unwrap();
    let registration = program
        .engine
        .front_registration_queries(
            &[program.entry.clone()],
            lower.clone(),
            weight,
            &budget,
            errors,
        )
        .unwrap();
    runtime.register(FRONT_BODY_KIND, front.factory()).unwrap();
    runtime
        .register_local(FRONT_LOWER_KIND, lower.factory())
        .unwrap();
    runtime
        .register_local(FRONT_EVALUATE_KIND, lower.factory())
        .unwrap();
    runtime
        .register_local(FRONT_REGISTRATION_KIND, registration.factory())
        .unwrap();
    let query = registration.query(&program.entry).unwrap();
    let request = runtime.request(&[query]);
    let complete = runtime.wait(request).unwrap();
    let output = registration.outcome((complete[0]).clone()).unwrap();
    drop(complete);
    runtime.shutdown();
    drop(runtime);
    drop(registration);
    drop(lower);
    drop(front);
    run(&mut program, &budget, output);
}

#[test]
fn accepted_generator_view_refuses_short_grants_without_publishing_sources_and_keeps_last_source_owner()
 {
    use hirsel_engine::*;
    use hirsel_engine_runtime::*;

    use hirsel_query_products::reads::ReadKey;
    use hirsel_query_products::{LoweringPolicy, QueryKind};

    with_compiler_stack(|| {
        for jobs in [1, 2] {
            accepted_output_control(jobs, |program, budget, output| {
                let before = program.engine.sources.retained_payload_bytes();
                let reserved = budget.reserved();
                assert!(
                    program
                        .engine
                        .accept_front_registration(
                            output.clone(),
                            &hirsel_engine_admission::ProcessBudget::new(0)
                        )
                        .is_err()
                );
                assert_eq!(budget.reserved(), reserved);
                assert_eq!(program.engine.sources.retained_payload_bytes(), before);
                assert!(program.engine.generated_source(&program.entry).is_none());
                let accepted = program
                    .engine
                    .accept_front_registration(output.clone(), &budget)
                    .unwrap();
                assert!(accepted.retained_bytes() > 0);
                assert!(accepted.retained_bytes() <= 16 << 20);
                let retained = accepted.retained_bytes();
                let detached = program.engine.sources.clone();
                drop(accepted);
                drop(output);
                assert!(budget.reserved() > 0);
                // The engine and this detached source both retain the new source grant.
                drop(detached);
                assert_eq!(budget.reserved(), retained);
            });
        }
    });
}

#[test]
fn accepted_generator_view_refuses_changed_executed_helper_reads() {
    accepted_edit_control(false);
}

#[test]
fn accepted_generator_view_refuses_changed_original_declarations() {
    accepted_edit_control(true);
}

fn accepted_edit_control(own: bool) {
    use hirsel_engine::*;
    use hirsel_engine_runtime::*;

    use hirsel_query_products::reads::ReadKey;
    use hirsel_query_products::{LoweringPolicy, QueryKind};

    with_compiler_stack(|| {
        for jobs in [1, 2] {
            accepted_output_control(jobs, |program, budget, output| {
                let accepted = program
                    .engine
                    .accept_front_registration(output.clone(), &budget)
                    .unwrap();
                let primary = program
                    .engine
                    .sources
                    .file(program.primary)
                    .unwrap()
                    .path
                    .clone();
                let edited = if own {
                    ACCEPTED_SOURCE.replace("comptime make()", "comptime {make()}")
                } else {
                    ACCEPTED_SOURCE.replace("{7u8}", "{8u8}")
                };
                assert!(program.replace_source(&primary, edited).unwrap());
                program.parse_sources().unwrap();
                let paths = program.engine.front_source_roots();
                let weight = MemoryWeight {
                    retained: 2 << 20,
                    scratch: 8 << 20,
                    stack: 0,
                };
                let error = program
                    .engine
                    .front_body_queries_with_registrations(
                        &paths,
                        &[accepted],
                        weight,
                        &budget,
                        crate::scheduled_front::errors(),
                    )
                    .err()
                    .expect("stale view must refuse");
                let QueryError::Operational(failure) = error else {
                    panic!("unexpected rejection: {error:?}")
                };
                assert_eq!(failure.operation, "generated syntax validation");
                if own {
                    assert!(failure.message.contains("original declaration changed"));
                } else {
                    assert!(
                        failure.message.contains("read changed"),
                        "{}",
                        failure.message
                    );
                }
                assert!(
                    program
                        .engine
                        .accept_front_registration(output, &budget)
                        .is_err()
                );
            });
        }
    });
}

#[test]
fn accepted_generator_view_accepts_the_exact_grant_and_refuses_n_minus_one() {
    with_compiler_stack(|| {
        for jobs in [1, 2] {
            let mut required = 0;
            accepted_output_control(jobs, |program, budget, output| {
                let accepted = program
                    .engine
                    .accept_front_registration(output, &budget)
                    .unwrap();
                required = accepted.retained_bytes();
            });
            accepted_output_control(jobs, |program, budget, output| {
                let before = program.engine.sources.retained_payload_bytes();
                assert!(
                    program
                        .engine
                        .accept_front_registration(
                            output,
                            &hirsel_engine_admission::ProcessBudget::new(required - 1)
                        )
                        .is_err()
                );
                assert_eq!(program.engine.sources.retained_payload_bytes(), before);
                assert!(program.engine.generated_source(&program.entry).is_none());
            });
            accepted_output_control(jobs, |program, budget, output| {
                let accepted = program
                    .engine
                    .accept_front_registration(
                        output,
                        &hirsel_engine_admission::ProcessBudget::new(required),
                    )
                    .unwrap();
                assert_eq!(accepted.retained_bytes(), required);
            });
        }
    });
}

#[test]
fn accepted_generator_view_rejects_verify_off_text_when_validation_is_required() {
    with_compiler_stack(|| {
        for jobs in [1, 2] {
            accepted_output_control_with(
                jobs,
                hirsel_engine::Verify::Off,
                ACCEPTED_SOURCE,
                |program, budget, output| {
                    assert!(!output.has_execution_acceptance());
                    program.engine.verify = hirsel_engine::Verify::On;
                    let before = program.engine.sources.retained_payload_bytes();
                    assert!(
                        program
                            .engine
                            .accept_front_registration(output, &budget)
                            .is_err()
                    );
                    assert_eq!(program.engine.sources.retained_payload_bytes(), before);
                    assert!(program.engine.generated_source(&program.entry).is_none());
                },
            );
        }
    });
}

#[test]
fn accepted_generator_view_rejects_a_literal_header_that_differs_from_its_declaration() {
    with_compiler_stack(|| {
        let source = ACCEPTED_SOURCE.replace("fn()->u8 uses {} {7u8}", "fn()->u16 uses {} {7u16}");
        for jobs in [1, 2] {
            accepted_output_control_with(
                jobs,
                hirsel_engine::Verify::On,
                &source,
                |program, budget, output| {
                    assert!(output.has_execution_acceptance());
                    let before = program.engine.sources.retained_payload_bytes();
                    let error = program
                        .engine
                        .accept_front_registration(output, &budget)
                        .err()
                        .expect("literal type differs");
                    let hirsel_engine::QueryError::Diagnostics(diagnostics) = error.error else {
                        panic!("{error:?}")
                    };
                    assert!(
                        diagnostics
                            .iter()
                            .any(|diagnostic| diagnostic.rule.as_str() == "LANG-7"),
                        "{diagnostics:?}"
                    );
                    assert_eq!(program.engine.sources.retained_payload_bytes(), before);
                    assert!(program.engine.generated_source(&program.entry).is_none());
                },
            );
        }
    });
}

#[test]
fn rejected_generator_header_retains_diagnostic_sources_without_namespace_publication() {
    with_compiler_stack(|| {
        let source = ACCEPTED_SOURCE.replace("fn()->u8 uses {} {7u8}", "fn()->u16 uses {} {7u16}");
        for jobs in [1, 2] {
            let mut retained_budget = None;
            accepted_output_control_with(
                jobs,
                hirsel_engine::Verify::On,
                &source,
                |program, budget, output| {
                    retained_budget = Some(budget.clone());
                    let before = program.engine.sources.retained_payload_bytes();
                    let producer_bytes = budget.reserved();
                    let failure = program
                        .engine
                        .accept_front_registration(output.clone(), &budget)
                        .err()
                        .expect("header rejection");
                    let hirsel_engine::QueryError::Diagnostics(diagnostics) = &failure.error else {
                        panic!("{failure:?}")
                    };
                    assert!(diagnostics.iter().any(|d| d.rule.as_str() == "LANG-7"));
                    for diagnostic in diagnostics {
                        assert!(failure.sources.slice(diagnostic.primary).is_some());
                        assert!(
                            diagnostic
                                .secondary
                                .iter()
                                .any(|label| failure.sources.slice(label.span).is_some())
                        );
                    }
                    assert_eq!(program.engine.sources.retained_payload_bytes(), before);
                    assert!(program.engine.generated_source(&program.entry).is_none());
                    let retained = budget.reserved() - producer_bytes;
                    assert!(retained > 0 && retained < 16 << 20);
                    let detached = failure.sources.clone();
                    drop(failure);
                    assert_eq!(budget.reserved(), producer_bytes + retained);
                    drop(detached);
                    assert_eq!(budget.reserved(), producer_bytes);
                    drop(output);
                },
            );
            assert_eq!(retained_budget.unwrap().reserved(), 0);
        }
    });
}

#[test]
fn accepted_generator_view_releases_unused_syntax_grant_and_keeps_clone_owned_bytes() {
    use hirsel_engine::*;
    use hirsel_engine_runtime::*;

    use hirsel_query_products::reads::ReadKey;
    use hirsel_query_products::{LoweringPolicy, QueryKind};

    with_compiler_stack(|| {
        for jobs in [1, 2] {
            let mut retained_budget = None;
            accepted_output_control(jobs, |program, budget, output| {
                let before = budget.reserved();
                let accepted = program
                    .engine
                    .accept_front_registration(output, &budget)
                    .unwrap();
                let retained = accepted.retained_bytes();
                assert!(retained > 0 && retained < 16 << 20);
                assert_eq!(
                    budget.reserved(),
                    before + retained,
                    "acceptance releases every unused syntax byte immediately"
                );
                let cloned = accepted.clone();
                let source = program.engine.sources.clone();
                drop(accepted);
                assert_eq!(budget.reserved(), before + retained);
                drop(cloned);
                assert_eq!(
                    budget.reserved(),
                    retained,
                    "only detached source ownership survives producer destruction"
                );
                drop(source);
                assert_eq!(
                    budget.reserved(),
                    retained,
                    "engine source still owns accepted bytes"
                );
                retained_budget = Some(budget.clone());
            });
            assert_eq!(
                retained_budget.unwrap().reserved(),
                0,
                "last engine source releases exact retained grant"
            );
        }
    });
}

#[test]
fn accepted_generated_native_catalogue_preserves_raw_source_and_actual_lower_authority() {
    use hirsel_engine::*;
    use hirsel_engine_runtime::*;

    use hirsel_query_products::reads::ReadKey;
    use hirsel_query_products::{LoweringPolicy, QueryKind};

    with_compiler_stack(|| {
        for jobs in [1, 2] {
            accepted_output_control(jobs, |program, budget, output| {
                let original = program
                    .engine
                    .parse_definition(&program.entry)
                    .unwrap()
                    .value;
                let accepted = program
                    .engine
                    .accept_front_registration(output, &budget)
                    .unwrap();
                let definitions = program.engine.front_source_roots();
                let weight = MemoryWeight {
                    retained: 2 << 20,
                    scratch: 8 << 20,
                    stack: 0,
                };
                let errors = crate::scheduled_front::errors();
                let front = program
                    .engine
                    .front_body_queries_with_registrations(
                        &definitions,
                        &[accepted],
                        weight,
                        &budget,
                        errors,
                    )
                    .unwrap();
                let lower = program
                    .engine
                    .front_lower_queries(&definitions, front.clone(), weight, 64, &budget, errors)
                    .unwrap();
                let native = program
                    .engine
                    .front_native_definition_queries(
                        &definitions,
                        front.clone(),
                        lower.clone(),
                        weight,
                        budget.reserve(16 << 20).unwrap(),
                        errors,
                    )
                    .unwrap();
                let mut runtime = AdmittedRuntime::new(
                    hirsel_intern::jobs::Jobs::new(jobs),
                    budget.reserve(512 << 20).unwrap(),
                    errors,
                )
                .unwrap();
                runtime.register(FRONT_BODY_KIND, front.factory()).unwrap();
                runtime
                    .register_local(FRONT_LOWER_KIND, lower.factory())
                    .unwrap();
                runtime
                    .register_local(FRONT_EVALUATE_KIND, lower.factory())
                    .unwrap();
                runtime
                    .register(FRONT_NATIVE_DEFINITION_KIND, native.factory())
                    .unwrap();
                let request = runtime.request(&[native.query(&program.entry).unwrap()]);
                let products = runtime
                    .wait(request)
                    .unwrap_or_else(|reference| panic!("{:?}", native.failure(reference)));
                let output = native.outcome((products[0]).clone()).unwrap();
                let source = &output.product.source;
                assert_eq!(source.definition, program.entry);
                assert_eq!(source.source.implementation_revision, original.text_hash);
                assert_eq!(source.source.syntax.text_hash, original.text_hash);
                assert_eq!(source.source.syntax.item, original.item);
                let generated = source
                    .source
                    .generated
                    .as_ref()
                    .expect("native origin retains generated text");
                let expected = program.engine.generated_source(&program.entry).unwrap();
                assert_eq!(generated, expected);
                assert!(
                    source.lower.is_some(),
                    "actual independently verified Lower output"
                );
                assert!(source.audit.is_some());
                let reader = native
                    .original_context()
                    .reader([output.original.as_deref().unwrap().clone()])
                    .unwrap();
                runtime.shutdown();
                let body = reader
                    .read(&program.entry)
                    .expect("original source identity matches admitted Lower");
                assert_eq!(body.definition().path.path(), &program.entry);
            });
        }
    });
}

#[test]
fn accepted_generated_native_inputs_bind_current_views_and_exact_metadata_grants() {
    use hirsel_engine::*;
    use hirsel_engine_runtime::*;

    use hirsel_query_products::reads::ReadKey;
    use hirsel_query_products::{LoweringPolicy, QueryKind};

    with_compiler_stack(|| {
        for jobs in [1, 2] {
            accepted_output_control(jobs, |program, budget, output| {
                let accepted = program
                    .engine
                    .accept_front_registration(output.clone(), &budget)
                    .unwrap();
                let definitions = program.engine.front_source_roots();
                let weight = MemoryWeight {
                    retained: 2 << 20,
                    scratch: 8 << 20,
                    stack: 0,
                };
                let errors = crate::scheduled_front::errors();
                let front = program
                    .engine
                    .front_body_queries_with_registrations(
                        &definitions,
                        &[accepted.clone()],
                        weight,
                        &budget,
                        errors,
                    )
                    .unwrap();
                let lower = program
                    .engine
                    .front_lower_queries(&definitions, front.clone(), weight, 64, &budget, errors)
                    .unwrap();
                let before = program.engine.work_stats();
                assert!(
                    program
                        .engine
                        .front_native_definition_inputs(&definitions, 16 << 20)
                        .is_err(),
                    "raw generated source alone grants no accepted namespace"
                );
                let inputs = program
                    .engine
                    .front_native_definition_inputs_in(
                        &definitions,
                        front.namespace().clone(),
                        16 << 20,
                    )
                    .unwrap();
                let required = inputs.input_bytes();
                assert!(required > 0);
                assert!(
                    program
                        .engine
                        .front_native_definition_inputs_in(
                            &definitions,
                            front.namespace().clone(),
                            required - 1
                        )
                        .is_err()
                );
                assert!(
                    program
                        .engine
                        .front_native_definition_queries_with_inputs(
                            front.clone(),
                            lower.clone(),
                            inputs,
                            weight,
                            budget.reserve(required - 1).unwrap(),
                            errors
                        )
                        .is_err()
                );
                let inputs = program
                    .engine
                    .front_native_definition_inputs_in(
                        &definitions,
                        front.namespace().clone(),
                        required,
                    )
                    .unwrap();
                let native = program
                    .engine
                    .front_native_definition_queries_with_inputs(
                        front.clone(),
                        lower.clone(),
                        inputs,
                        weight,
                        budget.reserve(required).unwrap(),
                        errors,
                    )
                    .unwrap();
                assert!(native.query(&program.entry).is_some());
                drop(native);
                assert_eq!(
                    program.engine.work_stats(),
                    before,
                    "catalogue capture performs no semantic producer work"
                );
                let other = program
                    .engine
                    .accept_front_registration(output, &budget)
                    .unwrap();
                let before = program.engine.work_stats();
                let other_namespace = program
                    .engine
                    .capture_namespace(&definitions, &[other], 16 << 20)
                    .unwrap();
                let mismatch = program
                    .engine
                    .front_native_definition_inputs_in(&definitions, other_namespace, 16 << 20)
                    .unwrap();
                let error = program
                    .engine
                    .front_native_definition_queries_with_inputs(
                        front,
                        lower,
                        mismatch,
                        weight,
                        budget.reserve(16 << 20).unwrap(),
                        errors,
                    )
                    .err()
                    .expect("distinct accepted source views cannot be silently mixed");
                let QueryError::Operational(error) = error else {
                    panic!("{error:?}")
                };
                assert!(
                    error.message.contains("different accepted registrations"),
                    "{error:?}"
                );
                assert_eq!(
                    program.engine.work_stats(),
                    before,
                    "catalogue capture performs no semantic producer work"
                );
                let path = program
                    .engine
                    .sources
                    .file(program.primary)
                    .unwrap()
                    .path
                    .clone();
                assert!(
                    program
                        .replace_source(&path, ACCEPTED_SOURCE.replace("{7u8}", "{8u8}"))
                        .unwrap()
                );
                program.parse_sources().unwrap();
                let error = program
                    .engine
                    .capture_namespace(&definitions, &[accepted], 16 << 20)
                    .err()
                    .expect("stale generator helper reads must refuse before native capture");
                let QueryError::Operational(error) = error else {
                    panic!("{error:?}")
                };
                assert_eq!(error.operation, "generated syntax validation");
            });
        }
    });
}

#[test]
fn detached_generator_text_releases_completed_front_and_lower_input_grants() {
    with_compiler_stack(|| {
        for jobs in [1, 2] {
            let mut detached = None;
            let mut retained_budget = None;
            accepted_output_control(jobs, |_, budget, output| {
                assert!(
                    budget.reserved() > 0,
                    "text/read/evidence payload stays charged"
                );
                assert!(
                    budget.reserved() < 16 << 20,
                    "detached text must release the three completed 16MiB input graphs"
                );
                assert_eq!(output.text().unwrap(), "fn()->u8 uses {} {7u8}");
                assert!(output.has_execution_acceptance());
                assert!(output.reads().iter().any(|(key, _)| matches!(key, hirsel_query_products::reads::ReadKey::ExecutedBody(id) if id.path().0.name == "make")));
                let copy = output.clone();
                drop(output);
                assert!(budget.reserved() > 0);
                detached = Some(copy);
                retained_budget = Some(budget.clone());
            });
            let budget = retained_budget.unwrap();
            let output = detached.unwrap();
            assert_eq!(output.text().unwrap(), "fn()->u8 uses {} {7u8}");
            assert!(output.has_execution_acceptance());
            assert!(budget.reserved() > 0 && budget.reserved() < 16 << 20);
            drop(output);
            assert_eq!(budget.reserved(), 0);
        }
    });
}

#[test]
fn namespace_capture_keeps_its_admitted_memory_after_front_release() {
    use hirsel_engine::*;
    use hirsel_engine_runtime::*;

    use hirsel_query_products::reads::ReadKey;
    use hirsel_query_products::{LoweringPolicy, QueryKind};

    with_compiler_stack(|| {
        accepted_output_control(1, |program, budget, output| {
            let accepted = program
                .engine
                .accept_front_registration(output, &budget)
                .unwrap();
            let definitions = program.engine.front_source_roots();
            let front = program
                .engine
                .front_body_queries_with_registrations(
                    &definitions,
                    &[accepted],
                    MemoryWeight {
                        retained: 2 << 20,
                        scratch: 8 << 20,
                        stack: 0,
                    },
                    &budget,
                    crate::scheduled_front::errors(),
                )
                .unwrap();
            let capture = front.namespace().clone();
            let identity = capture.identity();
            let manifest = capture.generation_manifest();
            let before = budget.reserved();
            drop(front);
            let held = budget.reserved();
            assert!(held < before, "Front's unretained partition must release");
            assert_eq!(capture.identity(), identity);
            assert_eq!(capture.generation_manifest(), manifest);
            drop(capture);
            assert!(
                budget.reserved() < held,
                "the capture must retain its own input reservation"
            );
        });
    });
}

#[test]
fn captured_check_refuses_an_edited_written_file_before_publication() {
    use hirsel_engine::*;
    use hirsel_engine_runtime::*;

    use hirsel_query_products::reads::ReadKey;
    use hirsel_query_products::{LoweringPolicy, QueryKind};

    with_compiler_stack(|| {
        accepted_output_control(1, |program, budget, output| {
            let accepted = program
                .engine
                .accept_front_registration(output, &budget)
                .unwrap();
            let definitions = program.engine.front_source_roots();
            let weight = MemoryWeight {
                retained: 2 << 20,
                scratch: 8 << 20,
                stack: 0,
            };
            let errors = crate::scheduled_front::errors();
            let front = program
                .engine
                .front_body_queries_with_registrations(
                    &definitions,
                    &[accepted],
                    weight,
                    &budget,
                    errors,
                )
                .unwrap();
            let lower = program
                .engine
                .front_lower_queries(&definitions, front.clone(), weight, 64, &budget, errors)
                .unwrap();
            let growth = program
                .engine
                .front_growth_queries(&program.entry, front.clone(), weight, &budget, errors)
                .unwrap();
            let primary = program
                .engine
                .sources
                .file(program.primary)
                .unwrap()
                .path
                .clone();
            assert!(
                program
                    .replace_source(&primary, ACCEPTED_SOURCE.replace("{7u8}", "{8u8}"))
                    .unwrap()
            );
            program.parse_sources().unwrap();
            let error = program
                .engine
                .front_check_queries(
                    FrontCheckPlan {
                        files: program
                            .files
                            .iter()
                            .map(|(file, context)| {
                                (*file, context.clone(), LoweringPolicy::Comptime)
                            })
                            .collect(),
                        front,
                        lower,
                        growth,
                        unsafe_ffi: vec![],
                        lint_package: None,
                    },
                    weight,
                    &budget,
                    errors,
                )
                .err()
                .expect("a Check cannot mix an older capture with edited written inputs");
            let QueryError::Operational(error) = error else {
                panic!("{error:?}");
            };
            assert_eq!(error.operation, "namespace capture");
        });
    });
}
