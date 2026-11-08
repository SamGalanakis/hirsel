use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_engine_store as store;
use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use hirsel_intern::jobs::with_compiler_stack;

use hirsel_query_products::{LoweringPolicy, QueryKind};
use hirsel_tir::Constant;
fn path(name: &str) -> DefPath {
    let mut p = entry_path(&default_context());
    p.0.name = name.into();
    p
}
fn engine(source: &str) -> QueryEngine {
    let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
    let file = engine.sources.add("comptime.hir", source.into()).unwrap();
    engine.parse_file(file, &default_context()).unwrap();
    engine
}

fn lowering_text(
    engine: &QueryEngine,
    value: Result<QueryOutput<hirsel_tir::DefinitionRef>, QueryError>,
) -> Result<(String, String, Vec<SignatureDependency>), String> {
    value
        .map(|output| {
            let cx = hirsel_proof_export::Cx::new(
                std::rc::Rc::new(|_| hirsel_proof_export::Location::unknown()),
                engine.types().clone(),
            );
            (
                hirsel_codec::to_text_with(
                    &hirsel_proof_export::DefinitionSection {
                        path: output.value.path.path().clone(),
                        locations: vec![],
                        tir: (*output.value).clone(),
                        tir_digest: hirsel_syntax::ContentHash::default(),
                        references: output.dependencies.clone(),
                        operations: vec![],
                        obligations: vec![],
                    },
                    cx,
                )
                .0,
                placed_evidence(&engine.sources, format!("{:?}", output.diagnostics)),
                output.dependencies,
            )
        })
        .map_err(|error| placed_evidence(&engine.sources, format!("{error:?}")))
}

fn materialized_constant_publishes_and_validates_on_compiler_thread() {
    let mut engine = engine("const VALUE: u32 = comptime (2 + 3)\n");
    let definition = engine.lower_to_tir(&path("VALUE")).unwrap().value;
    assert_eq!(
        definition.bodies[0].rvalues,
        vec![hirsel_tir::Rvalue::Constant(Constant::Integer(5))]
    );
}
#[test]
fn dependent_constants_materialize_before_their_readers_and_refresh_after_edits() {
    hirsel_intern::jobs::with_compiler_stack(dependent_constants_materialize_before_their_readers_and_refresh_after_edits_on_compiler_thread);
}
fn dependent_constants_materialize_before_their_readers_and_refresh_after_edits_on_compiler_thread()
{
    let mut engine =
        engine("const FIRST: u32 = comptime 3\nconst SECOND: u32 = comptime (FIRST + FIRST)\n");
    assert_eq!(
        engine.materialize_constant(&path("SECOND")).unwrap(),
        Constant::Integer(6)
    );
    let file = engine
        .sources
        .add(
            "comptime.hir",
            "const FIRST: u32 = comptime 8\nconst SECOND: u32 = comptime (FIRST + FIRST)\n".into(),
        )
        .unwrap();
    engine.parse_file(file, &default_context()).unwrap();
    assert_eq!(
        engine.materialize_constant(&path("SECOND")).unwrap(),
        Constant::Integer(16)
    );
}
#[test]
fn evaluation_budget_diagnostics_retry_after_budget_changes() {
    hirsel_intern::jobs::with_compiler_stack(
        evaluation_budget_diagnostics_retry_after_budget_changes_on_compiler_thread,
    );
}
fn evaluation_budget_diagnostics_retry_after_budget_changes_on_compiler_thread() {
    let mut engine = engine("fn five() -> u32 { 2 + 3 }\nconst VALUE: u32 = comptime five()\n");
    engine.comptime_limits.budget = 0;
    let Err(QueryError::Diagnostics(ds)) = engine.lower_to_tir(&path("VALUE")) else {
        panic!("expected budget exhaustion")
    };
    assert_eq!(ds[0].rule.as_str(), "CT-1");
    // Exhaustion is a language result, not a tooling limit.
    assert!(engine.last_comptime_limit.is_none());
    engine.comptime_limits.budget = 1;
    assert!(engine.lower_to_tir(&path("VALUE")).is_ok());
    assert_eq!(
        engine.materialize_constant(&path("VALUE")).unwrap(),
        Constant::Integer(5)
    );
}
#[test]
fn static_errors_keep_their_ordinary_owner() {
    hirsel_intern::jobs::with_compiler_stack(
        static_errors_keep_their_ordinary_owner_on_compiler_thread,
    );
}
fn static_errors_keep_their_ordinary_owner_on_compiler_thread() {
    let mut engine = engine("const VALUE: u32 = comptime missing\n");
    let Err(QueryError::Diagnostics(ds)) = engine.lower_to_tir(&path("VALUE")) else {
        panic!("expected name error")
    };
    assert_eq!(ds[0].rule.as_str(), "LANG-4");
}

fn comptime_and_runtime_use_the_same_scalar_semantics_on_compiler_thread() {
    for expression in [
        "(2 + 3)",
        "if true { 7 } else { 9 }",
        "if false { 1 / 0 } else { 11 }",
    ] {
        let mut engine = engine(&format!(
            "const VALUE: u32 = comptime {expression}\nfn runtime() -> unique u32 {{ {expression} }}\n"
        ));
        let constant = engine.materialize_constant(&path("VALUE")).unwrap();
        let sources = engine.sources.clone();
        let runtime = engine.runtime_types().unwrap();
        let execution = hirsel_interp::run_with_runtime(
            &mut engine,
            &sources,
            &RunConfiguration {
                entry: path("runtime"),
                hash_seed: 0,
                inputs: vec![],
            },
            vec![],
            Default::default(),
            &mut hirsel_interp::NoOperations,
            &runtime,
        )
        .unwrap();
        let hirsel_interp::Outcome::Returned {
            value: hirsel_interp::Value::Integer(value),
            ..
        } = execution.outcome
        else {
            panic!("expected scalar return")
        };
        assert_eq!(constant, Constant::Integer(value.bits()));
    }
}
#[test]
fn pure_library_operations_are_reused_without_root_capabilities() {
    hirsel_intern::jobs::with_compiler_stack(
        pure_library_operations_are_reused_without_root_capabilities_on_compiler_thread,
    );
}
fn pure_library_operations_are_reused_without_root_capabilities_on_compiler_thread() {
    let mut engine = engine("const VALUE: u32 = comptime int.div_floor(9, 2)\n");
    assert_eq!(
        engine.materialize_constant(&path("VALUE")).unwrap(),
        Constant::Integer(4)
    );
}
#[test]
fn nested_comptime_evaluations_materialize_inside_out() {
    hirsel_intern::jobs::with_compiler_stack(
        nested_comptime_evaluations_materialize_inside_out_on_compiler_thread,
    );
}
fn nested_comptime_evaluations_materialize_inside_out_on_compiler_thread() {
    let mut engine = engine("const VALUE: u32 = comptime (comptime (2 + 3) + 4)\n");
    assert_eq!(
        engine.materialize_constant(&path("VALUE")).unwrap(),
        Constant::Integer(9)
    );
}

#[test]
fn compiler_dependency_depth_is_bounded_without_a_language_failure() {
    hirsel_intern::jobs::with_compiler_stack(
        compiler_dependency_depth_is_bounded_without_a_language_failure_on_compiler_thread,
    );
}
fn compiler_dependency_depth_is_bounded_without_a_language_failure_on_compiler_thread() {
    for count in [32, 33] {
        let mut source = String::new();
        for i in 0..count - 1 {
            source.push_str(&format!("const C{i}: u32 = comptime C{}\n", i + 1));
        }
        source.push_str(&format!("const C{}: u32 = comptime 1\n", count - 1));
        let mut engine = engine(&source);
        let result = engine.materialize_constant(&path("C0"));
        if count == 32 {
            assert_eq!(result.unwrap(), Constant::Integer(1));
            assert!(engine.last_comptime_limit.is_none());
        } else {
            let Err(QueryError::Operational(error)) = result else {
                panic!("expected dependency guard")
            };
            assert_eq!(error.operation, "compile-time tooling limit");
            assert_eq!(
                engine.last_comptime_limit.as_ref().unwrap().kind,
                hirsel_tir::ToolingLimitKind::CallDepth
            );
            assert_eq!(engine.last_comptime_limit.as_ref().unwrap().limit, 32);
        }
    }
}

#[test]
fn same_signature_function_body_edits_invalidate_materialized_values() {
    hirsel_intern::jobs::with_compiler_stack(
        same_signature_function_body_edits_invalidate_materialized_values_on_compiler_thread,
    );
}
fn same_signature_function_body_edits_invalidate_materialized_values_on_compiler_thread() {
    let mut engine =
        engine("fn value() -> unique u32 { 7 }\nconst VALUE: u32 = comptime value()\n");
    assert_eq!(
        engine.materialize_constant(&path("VALUE")).unwrap(),
        Constant::Integer(7)
    );
    let file = engine
        .sources
        .add(
            "comptime.hir",
            "fn value() -> unique u32 { 9 }\nconst VALUE: u32 = comptime value()\n".into(),
        )
        .unwrap();
    engine.parse_file(file, &default_context()).unwrap();
    assert_eq!(
        engine.materialize_constant(&path("VALUE")).unwrap(),
        Constant::Integer(9)
    );
}

#[test]
fn same_signature_body_edits_retry_prior_compile_time_language_failures() {
    hirsel_intern::jobs::with_compiler_stack(
        same_signature_body_edits_retry_prior_compile_time_language_failures_on_compiler_thread,
    );
}
fn same_signature_body_edits_retry_prior_compile_time_language_failures_on_compiler_thread() {
    let mut engine =
        engine("fn value() -> unique u32 { 1 / 0 }\nconst VALUE: u32 = comptime value()\n");
    let Err(QueryError::Diagnostics(ds)) = engine.lower_to_tir(&path("VALUE")) else {
        panic!("expected comptime failure")
    };
    assert_eq!(ds[0].rule.as_str(), "CT-1");
    let file = engine
        .sources
        .add(
            "comptime.hir",
            "fn value() -> unique u32 { 7 }\nconst VALUE: u32 = comptime value()\n".into(),
        )
        .unwrap();
    engine.parse_file(file, &default_context()).unwrap();
    engine.lower_to_tir(&path("VALUE")).unwrap();
}

#[test]
fn compile_time_constants_do_not_depend_on_unexecuted_finalizer_bodies() {
    hirsel_intern::jobs::with_compiler_stack(
        compile_time_constants_do_not_depend_on_unexecuted_finalizer_bodies_on_compiler_thread,
    );
}
fn compile_time_constants_do_not_depend_on_unexecuted_finalizer_bodies_on_compiler_thread() {
    let mut engine = engine(
        "resource type Unused { value: u32 }\ndrop(x) { let _ = VALUE }\nconst VALUE: u32 = comptime 7\n",
    );
    assert_eq!(
        engine.materialize_constant(&path("VALUE")).unwrap(),
        Constant::Integer(7)
    );
    assert_eq!(engine.recompute_count(&path("Unused"), QueryKind::Lower), 0);
}

#[test]
fn executed_compile_time_finalizers_use_the_runtime_scheduler_and_failure_policy() {
    hirsel_intern::jobs::with_compiler_stack(executed_compile_time_finalizers_use_the_runtime_scheduler_and_failure_policy_on_compiler_thread);
}
fn executed_compile_time_finalizers_use_the_runtime_scheduler_and_failure_policy_on_compiler_thread()
 {
    let mut engine = engine(
        "resource type Token { value: u32 }\ndrop(x) { let _ = 1u32 / 0u32 }\nfn value() -> unique u32 { let token = Token(value: 0); 7 }\nconst VALUE: u32 = comptime value()\n",
    );
    let result = engine.materialize_constant(&path("VALUE"));
    let Err(QueryError::Diagnostics(ds)) = result else {
        panic!("expected executed finalizer failure: {result:?}")
    };
    assert_eq!(ds[0].rule.as_str(), "CT-1");
    assert!(ds[0].notes.iter().any(|n| n.contains("SEM-1")));
}

#[test]
fn compiled_constant_installation_is_dependency_first_with_source_order_ties() {
    with_compiler_stack(|| {
        use hirsel_tir::TirProvider;
        let mut engine = engine(
            "const FIRST:u32=comptime SECOND\nconst INDEPENDENT:u32=1\nconst SECOND:u32=7\n",
        );
        let order = engine.constant_definitions().unwrap();
        let order: Vec<_> = order
            .into_iter()
            .filter(|p| p.0.package == path("FIRST").0.package)
            .collect();
        assert_eq!(order, [path("INDEPENDENT"), path("SECOND"), path("FIRST")]);
        assert_eq!(
            engine.materialize_constant(&path("FIRST")).unwrap(),
            Constant::Integer(7)
        );
    });
}

#[test]
fn source_comptime_values_have_no_data_nesting_cutoff() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        for depth in [31, 32, 33] {
            let mut ty = "u32".to_string();
            let mut value = "7u32".to_string();
            for _ in 0..depth {
                ty = format!("({ty},)");
                value = format!("({value},)");
            }
            let mut engine = engine(&format!("const VALUE: {ty} = comptime {value}\n"));
            engine.lower_to_tir(&path("VALUE")).unwrap();
            assert!(engine.last_comptime_limit.is_none());
        }
    });
}

#[test]
fn size_constant_instances_cache_separately_and_refresh_executed_dependencies() {
    with_compiler_stack(|| {
        use hirsel_tir::{Rvalue, TirProvider, ValueKind};
        let source = |factor| {
            format!(
                "fn square(n:u64)->u64{{n*n*{factor}u64}}\nfn scaled[n:size](xs:Inline[u8,n])->u64{{let _=xs;comptime square(n)}}\n"
            )
        };
        let mut engine = engine(&source(1));
        let definition = engine.lower_to_tir(&path("scaled")).unwrap().value;
        let constant = definition.bodies[0]
            .rvalues
            .iter()
            .find_map(|value| match value {
                Rvalue::ReadConstant {
                    definition,
                    arguments,
                } if !arguments.is_empty() => Some(definition.clone()),
                _ => None,
            })
            .unwrap();
        for (size, expected) in [(2, 4), (4, 16), (2, 4)] {
            let value = engine.constant_instance(&constant, &[size]).unwrap();
            assert_eq!(
                value.root.kind,
                ValueKind::Constant(Constant::Integer(expected))
            );
        }
        let file = engine.sources.add("comptime.hir", source(2)).unwrap();
        engine.parse_file(file, &default_context()).unwrap();
        assert_eq!(
            engine.constant_instance(&constant, &[2]).unwrap().root.kind,
            ValueKind::Constant(Constant::Integer(8))
        );
        assert_eq!(
            engine.constant_instance(&constant, &[4]).unwrap().root.kind,
            ValueKind::Constant(Constant::Integer(32))
        );
        // Each size instance's evaluation charges its one call.
        engine.comptime_limits.budget = 0;
        let Err(QueryError::Diagnostics(ds)) = engine.constant_instance(&constant, &[2]) else {
            panic!("expected CT-1 after budget change")
        };
        assert_eq!(ds[0].rule.as_str(), "CT-1");
        engine.comptime_limits.budget = 1;
        assert_eq!(
            engine.constant_instance(&constant, &[2]).unwrap().root.kind,
            ValueKind::Constant(Constant::Integer(8))
        );
    });
}

#[test]
fn a_failed_size_instantiation_retains_its_selecting_site() {
    use hirsel_requests::CompileOperation;
    use hirsel_requests::CompileResult;
    use hirsel_requests::CompilerRequests;
    with_compiler_stack(|| {
        let source = include_str!("../../../../conformance/CT/CT-1-size-instantiation-bad.hir");
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(directory.path().join("comptime.hir"), source).unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"standalone","version":"0"},"sources":[{"path":"comptime.hir","module":[]}],"dependencies":[]}"#).unwrap();
        for jobs in [1, 2, 8] {
            let config = CompileConfig {
                jobs: hirsel_intern::jobs::Jobs::new(jobs),
                store: std::sync::Arc::new(store::MemStore::default()),
                hold_bytes: 0,
                ..Default::default()
            };
            for warm in [false, true] {
                let mut requests = CompilerRequests::for_host(
                    COMPILER.with_config(config.clone()),
                    config.clone(),
                );
                let output = requests
                    .execute(
                        &manifest,
                        CompileOperation::Check {
                            lowering: LoweringPolicy::Comptime,
                        },
                    )
                    .unwrap_or_else(|error| panic!("jobs={jobs} warm={warm}: {error}"));
                let CompileResult::Check(checked) = &output.result else {
                    panic!("expected checked source");
                };
                let diagnostic = checked
                    .diagnostics
                    .iter()
                    .find(|d| d.rule.as_str() == "CT-1")
                    .unwrap();
                assert!(
                    checked
                        .sources
                        .slice(diagnostic.primary)
                        .unwrap()
                        .starts_with("comptime")
                );
                assert!(
                    diagnostic
                        .secondary
                        .iter()
                        .any(
                            |label| checked.sources.slice(label.span) == Some("inverse(second)")
                                && label.message.contains("[0]")
                        ),
                    "{diagnostic:?}"
                );
            }
        }
    });
}

#[test]
fn staged_constants_survive_proof_export_and_unrelated_item_insertion() {
    with_compiler_stack(|| {
        let source = "fn scaled[n:size](xs:Inline[u8,n])->u64{let _=xs;comptime n*n}\npub fn main()->Result[(),()]{let xs:Inline[u8,2]=[1,2];let _=scaled(xs);Ok(())}\n";
        let mut first = engine(source);
        let text = first.export_tir(&path("main")).unwrap();
        let export = first.import_tir(&text).unwrap();
        assert!(export.skipped.is_empty(), "{:?}", export.skipped);
        let constant = export
            .signatures
            .iter()
            .find(|s| s.path.0.name.starts_with("$comptime:scaled:"))
            .unwrap();
        assert_eq!(constant.public.generics().len(), 1);
        assert!(
            export
                .constants
                .iter()
                .any(|c| c.path == hirsel_tir::constant_instance_path(&constant.path, &[2]))
        );
        assert_eq!(hirsel_proof_export::export(&export.to_input()), text);
        let mut second = engine(&format!("fn unrelated()->u64{{3}}\n{source}"));
        let export = second.proof_export_input(&path("main")).unwrap();
        assert!(export.signatures.contains_key(&constant.path));
        assert_eq!(
            export.signatures[&constant.path].public.fingerprint,
            constant.public.fingerprint
        );
    });
}
#[test]
fn generated_bodies_regenerate_after_shape_and_generator_edits() {
    hirsel_intern::jobs::with_compiler_stack(
        generated_bodies_regenerate_after_shape_and_generator_edits_on_compiler_thread,
    );
}
fn generated_bodies_regenerate_after_shape_and_generator_edits_on_compiler_thread() {
    // CT-5: the text is cached under what the generator executed and the shape it read.
    let source = |fields: &str, three: &str| {
        format!(
            "type P {{ {fields} }}\n\
             fn gen[T](s: TypeShape[T]) -> Result[Body[fn(T) -> u64 uses {{}}], String] {{\n\
             let Record(fields) = type_shape.info(s).body else {{ return Error(\"record\") }}\n\
             if list.len(fields) == 2 {{ Ok(codegen.body(\"fn(p: standalone@0/P) -> u64 uses {{}} {{ p.x + p.y }}\")) }}\n\
             else {{ Ok(codegen.body(\"{three}\")) }}\n\
             }}\n\
             fn sum(p: P) -> u64 uses {{}} = comptime gen(shape(P))\n"
        )
    };
    let text = |engine: &mut QueryEngine, source: String| {
        let file = engine.sources.add("comptime.hir", source).unwrap();
        engine.parse_file(file, &default_context()).unwrap();
        engine.check_definition(&path("sum")).unwrap();
        engine.generated_source(&path("sum")).unwrap().text.clone()
    };
    let three = "fn(p: standalone@0/P) -> u64 uses {} { p.x + p.y + p.z }";
    let mut edited_engine = engine(&source("x: u64, y: u64", three));
    edited_engine.check_definition(&path("sum")).unwrap();
    let two = edited_engine
        .generated_source(&path("sum"))
        .unwrap()
        .text
        .clone();
    assert!(two.ends_with("{ p.x + p.y }"), "{two}");
    assert_eq!(
        text(&mut edited_engine, source("x: u64, y: u64, z: u64", three)),
        three
    );
    let edited = "fn(q: standalone@0/P) -> u64 uses {} { q.z }";
    assert_eq!(
        text(&mut edited_engine, source("x: u64, y: u64, z: u64", edited)),
        edited
    );
    let fresh = |source: String| {
        let mut engine = engine(&source);
        engine.check_definition(&path("sum")).unwrap();
        engine.generated_source(&path("sum")).unwrap().text.clone()
    };
    assert_eq!(fresh(source("x: u64, y: u64, z: u64", edited)), edited);
}
#[test]
fn generated_body_diagnostics_name_the_literal_parameter() {
    hirsel_intern::jobs::with_compiler_stack(
        generated_body_diagnostics_name_the_literal_parameter_on_compiler_thread,
    );
}
fn generated_body_diagnostics_name_the_literal_parameter_on_compiler_thread() {
    // CT-5: the body binds the declared parameter `t` by its literal's name `tag`,
    // so OWN-1 names and points at `tag`, the binding the generated text uses.
    let mut engine = engine(
        "type Tag { name: String }\n\
         fn renaming() -> Result[Body[fn(Tag) -> () uses {}], String] {\n\
         Ok(codegen.body(\"fn(tag: standalone@0/Tag) -> () uses {} { tag.name = \\\"x\\\" }\"))\n\
         }\n\
         fn rename(t: Tag) -> () uses {} = comptime renaming()\n",
    );
    let diagnostics = match engine.mode_definition(&path("rename")) {
        Ok(out) => out.diagnostics,
        Err(QueryError::Diagnostics(ds)) => ds,
        Err(e) => panic!("{e:?}"),
    };
    let own = diagnostics
        .iter()
        .find(|d| d.rule.as_str() == "OWN-1")
        .unwrap_or_else(|| panic!("{diagnostics:?}"));
    assert!(
        own.message.contains("`tag` is a borrowed parameter"),
        "{own:?}"
    );
    let generated = engine.generated_source(&path("rename")).unwrap().file;
    let provided = own
        .secondary
        .iter()
        .find(|l| l.message.starts_with("provided"))
        .unwrap_or_else(|| panic!("{own:?}"));
    assert_eq!(
        engine.sources.resolve(provided.span).file,
        generated,
        "{own:?}"
    );
    assert_eq!(engine.sources.slice(provided.span), Some("tag"), "{own:?}");
}

#[test]
fn generic_generated_instances_follow_their_declaration() {
    hirsel_intern::jobs::with_compiler_stack(
        generic_generated_instances_follow_their_declaration_on_compiler_thread,
    );
}
fn generic_generated_instances_follow_their_declaration_on_compiler_thread() {
    // CT-5: evaluation selects width[Point] lazily; editing the declaration
    // retires that instance, so the next evaluation generates it again.
    let source = |generator: &str| {
        format!(
            "type Point {{ x: u64 }}\n\
             fn two[T](s: TypeShape[T]) -> Result[Body[fn(T) -> u64 uses {{}}], String] {{\n\
             Ok(codegen.body(\"fn(v: standalone@0/Point) -> u64 uses {{}} {{ 2 }}\"))\n\
             }}\n\
             fn twelve[T](s: TypeShape[T]) -> Result[Body[fn(T) -> u64 uses {{}}], String] {{\n\
             Ok(codegen.body(\"fn(v: standalone@0/Point) -> u64 uses {{}} {{ 12 }}\"))\n\
             }}\n\
             fn width[T](v: T) -> u64 uses {{}} = comptime {generator}(shape(T))\n\
             const VALUE: u64 = comptime width(Point(x: 1))\n"
        )
    };
    let mut engine = engine(&source("two"));
    assert_eq!(
        engine.materialize_constant(&path("VALUE")).unwrap(),
        Constant::Integer(2)
    );
    let file = engine
        .sources
        .add("comptime.hir", source("twelve"))
        .unwrap();
    engine.parse_file(file, &default_context()).unwrap();
    assert_eq!(
        engine.materialize_constant(&path("VALUE")).unwrap(),
        Constant::Integer(12)
    );
}
/// Lowered helper hits must replay the compile-time requests that produced
/// their constants at the new caller's logical dependency height.
#[test]
fn cached_helper_lowering_preserves_dependency_depth() {
    with_compiler_stack(|| {
        let source = "const LEAF: u32 = 1u32\nfn helper() -> u32 { comptime LEAF }\nconst C2: u32 = comptime helper()\nconst C1: u32 = comptime C2\nconst C0: u32 = comptime C1\n";
        for warm in [false, true] {
            let mut engine = engine(source);
            engine.comptime_limits.call_depth = 3;
            if warm {
                engine.lower_to_tir(&path("helper")).unwrap();
            }
            let result = engine.materialize_constant(&path("C0"));
            assert!(
                matches!(result, Err(QueryError::Operational(_))),
                "warm={warm}: {result:?}"
            );
            let limit = engine.last_comptime_limit.as_ref().unwrap();
            assert_eq!(limit.kind, hirsel_tir::ToolingLimitKind::CallDepth);
            assert_eq!(limit.limit, 3);
            let site = engine.sources.resolve(limit.site.unwrap());
            assert_eq!(
                site.start,
                source.find("comptime LEAF").unwrap() as u32 + "comptime ".len() as u32
            );
        }
    });
}

/// Anchor numbers belong to one source map. Compare every error field and
/// limit site after replacing them by their actual source path and base.
fn placed_evidence(sources: &hirsel_span::SourceMap, text: String) -> String {
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("Anchor(") {
        out.push_str(&rest[..at]);
        rest = &rest[at + "Anchor(".len()..];
        let end = rest.find(')').unwrap();
        let anchor = hirsel_span::Anchor(rest[..end].parse().unwrap());
        let place = sources
            .anchor(anchor)
            .map(|(file, base)| (sources.file(file).unwrap().path.clone(), base));
        out.push_str(&format!("Anchor({place:?})"));
        rest = &rest[end + 1..];
    }
    out + rest
}
fn outcome_text(
    engine: &QueryEngine,
    result: Result<hirsel_tir::Materialized, QueryError>,
) -> Result<String, String> {
    result
        .map(|value| materialized_text(engine, &value))
        .map_err(|error| placed_evidence(&engine.sources, format!("{error:?}")))
}
fn materialized_text(engine: &QueryEngine, value: &hirsel_tir::Materialized) -> String {
    let context = hirsel_proof_export::Cx::new(
        std::rc::Rc::new(|_| hirsel_proof_export::Location::unknown()),
        engine.types().clone(),
    );
    hirsel_codec::to_text_with(value, context).0
}

#[derive(Debug, PartialEq, Eq)]
struct FrontierText {
    errors: std::collections::BTreeSet<String>,
    values: Option<Vec<String>>,
}
struct RequestFrontier {
    text: FrontierText,
    sources: Option<std::sync::Arc<hirsel_requests::PrecomputedSources>>,
    inspection: Option<std::sync::Arc<hirsel_requests::CompileInspection>>,
    budget: std::sync::Arc<hirsel_engine_admission::ProcessBudget>,
}
fn diagnostic_text(
    sources: &hirsel_span::SourceMap,
    diagnostic: &hirsel_diag::Diagnostic,
) -> String {
    format!(
        "{:?}",
        (
            diagnostic.rule.as_str(),
            diagnostic.severity,
            &diagnostic.message,
            sources.slice(diagnostic.primary),
            diagnostic
                .secondary
                .iter()
                .map(|label| (sources.slice(label.span), &label.message))
                .collect::<Vec<_>>(),
            &diagnostic.notes
        )
    )
}
fn record_case_error(text: &mut FrontierText, sources: &hirsel_span::SourceMap, error: QueryError) {
    match error {
        QueryError::Diagnostics(diagnostics) => {
            text.errors.extend(
                diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic_text(sources, diagnostic)),
            );
        }
        error => {
            text.errors.insert(format!("{error:?}"));
        }
    }
    text.values = None;
}
fn scalar_frontier_text(engine: &mut QueryEngine, paths: &[DefPath]) -> FrontierText {
    let mut text = FrontierText {
        errors: Default::default(),
        values: Some(vec![]),
    };
    for path in paths {
        match engine.materialize_value(path) {
            Ok(value) => {
                if let Some(values) = &mut text.values {
                    values.push(materialized_text(engine, &value));
                }
            }
            Err(error) => record_case_error(&mut text, &engine.sources, error),
        }
    }
    text
}
/// Each declaration has its own file. A failed root cannot withhold another
/// file, while Check remains one source-ordered request over the package.
fn frontier_text(engine: &mut QueryEngine, paths: Vec<DefPath>, jobs: usize) -> RequestFrontier {
    use hirsel_requests::CompileOperation;
    use hirsel_requests::CompileResult;
    use hirsel_requests::CompilerRequests;
    use std::sync::Arc;
    let file = engine
        .sources
        .anchors()
        .filter_map(|(_, file, _)| {
            engine
                .sources
                .file(file)
                .filter(|source| source.path.ends_with("comptime.hir"))
                .map(|_| file)
        })
        .max()
        .unwrap();
    let source = engine.sources.file(file).unwrap().text().to_owned();
    let items = engine
        .parse_file(file, &default_context())
        .unwrap()
        .value
        .items;
    let directory = tempfile::tempdir().unwrap();
    let mut entries = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let start = engine.sources.anchor(item.anchor).unwrap().1 as usize;
        let end = items.get(index + 1).map_or(source.len(), |next| {
            engine.sources.anchor(next.anchor).unwrap().1 as usize
        });
        let name = format!("{index}-{}.hir", item.item.definition.0.name);
        std::fs::write(directory.path().join(&name), &source[start..end]).unwrap();
        entries.push(serde_json::json!({"path":name,"module":[]}));
    }
    let manifest = directory.path().join("hirsel.json");
    std::fs::write(&manifest, serde_json::json!({"schema":1,"package":{"name":"standalone","version":"0"},"sources":entries,"dependencies":[]}).to_string()).unwrap();
    let store = engine
        .product_store()
        .unwrap_or_else(|| Arc::new(store::MemStore::default()));
    let config = CompileConfig {
        jobs: hirsel_intern::jobs::Jobs::new(jobs),
        store: store.clone(),
        verify: engine.verify,
        hold_bytes: engine.hold_bytes(),
        ..Default::default()
    };
    let compiler = COMPILER
        .with_config(config.clone())
        .with_comptime_limits(engine.comptime_limits);
    let mut requests = CompilerRequests::for_host(compiler, config);
    let budget = requests.budget().clone();
    let mut text = FrontierText {
        errors: Default::default(),
        values: None,
    };
    let mut sources = None;
    let mut inspection = None;
    match requests.execute(
        &manifest,
        CompileOperation::Check {
            lowering: LoweringPolicy::Comptime,
        },
    ) {
        Ok(output) => {
            let CompileResult::Check(checked) = &output.result else {
                panic!("expected Check")
            };
            text.errors.extend(
                checked
                    .diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.severity == hirsel_diag::Severity::Error)
                    .map(|diagnostic| diagnostic_text(&checked.sources, diagnostic)),
            );
            assert_eq!(checked.completion, CheckCompletion::Complete);
            if text.errors.is_empty() {
                let output = requests
                    .execute(&manifest, CompileOperation::Precompute)
                    .unwrap();
                let CompileResult::Precomputed {
                    sources: products, ..
                } = &output.result
                else {
                    panic!("expected Precompute")
                };
                text.values = Some(
                    paths
                        .iter()
                        .map(|path| {
                            let bytes = products.source_product_bytes(path).unwrap().unwrap();
                            let product = products
                                .source_products(path, budget.reserve(bytes).unwrap())
                                .unwrap()
                                .unwrap();
                            let context = hirsel_proof_export::Cx::new(
                                std::rc::Rc::new(|_| hirsel_proof_export::Location::unknown()),
                                products.signatures().types().clone(),
                            );
                            hirsel_codec::to_text_with(product.constant().unwrap(), context).0
                        })
                        .collect(),
                );
                sources = Some(products.clone());
            }
            let output = requests
                .execute(
                    &manifest,
                    CompileOperation::Inspect {
                        definitions: vec![],
                    },
                )
                .unwrap();
            let CompileResult::Inspect(observed) = &output.result else {
                panic!("expected Inspect")
            };
            inspection = Some(observed.clone());
        }
        Err(ToolError::Query { error, sources, .. }) => {
            record_case_error(&mut text, &sources, error)
        }
        Err(error) => panic!("request Check failed: {error}"),
    }
    let owned = std::mem::replace(
        engine,
        QueryEngine::new(default_environment(), Default::default()),
    );
    *engine = owned.with_result_store(store);
    RequestFrontier {
        text,
        sources,
        inspection,
        budget,
    }
}

/// Fresh requests over one store agree with scalar evaluation for duplicate
/// roots. An edit to the helper LEAF's evaluation executed recomputes the
/// stored LEAF value; an unchanged repeat reopens it.
#[test]
fn frozen_frontier_values_refresh_and_preserve_input_order() {
    use hirsel_requests::CompileOperation;
    use hirsel_requests::CompileResult;
    use hirsel_requests::CompilerRequests;
    use std::sync::Arc;
    with_compiler_stack(|| {
        let source = |leaf| {
            format!(
                "fn helper() -> u32 {{ {leaf} }}\nconst LEAF: u32 = comptime helper()\nconst A: u32 = comptime (LEAF + 1)\nconst B: u32 = comptime (LEAF + 2)\n"
            )
        };
        let paths = vec![path("B"), path("A"), path("B"), path("LEAF")];
        for jobs in [1, 2, 8] {
            let directory = tempfile::tempdir().unwrap();
            let manifest = directory.path().join("hirsel.json");
            std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"standalone","version":"0"},"sources":[{"path":"comptime.hir","module":[]}],"dependencies":[]}"#).unwrap();
            let config = CompileConfig {
                jobs: hirsel_intern::jobs::Jobs::new(jobs),
                store: Arc::new(store::DirStore::new(directory.path().join("products"))),
                hold_bytes: 0,
                ..Default::default()
            };
            // Each request is a fresh graph over the same store; the count is
            // how often it produced LEAF's value instead of reopening it.
            let precompute = |text: &str| {
                std::fs::write(directory.path().join("comptime.hir"), text).unwrap();
                let mut requests = CompilerRequests::for_host(
                    COMPILER.with_config(config.clone()),
                    config.clone(),
                );
                let output = requests
                    .execute(&manifest, CompileOperation::Precompute)
                    .unwrap_or_else(|error| panic!("jobs={jobs}: {error}"));
                let leaf = requests
                    .recompute_counts(&[(path("LEAF"), QueryKind::Lower)])
                    .unwrap();
                (output, leaf[0].2)
            };
            for leaf in [3, 9] {
                let text = source(leaf);
                let mut control = engine(&text);
                let expected: Vec<_> = paths
                    .iter()
                    .map(|p| {
                        let result = control.materialize_value(p);
                        outcome_text(&control, result)
                    })
                    .collect();
                for repeat in [false, true] {
                    let (output, computed) = precompute(&text);
                    // LEAF is produced cold and after the `helper` edit only;
                    // an unchanged repeat reopens the stored product.
                    assert_eq!(
                        computed > 0,
                        !repeat,
                        "jobs={jobs} leaf={leaf} repeat={repeat}"
                    );
                    let CompileResult::Precomputed { sources, .. } = &output.result else {
                        panic!("expected source precompute");
                    };
                    let actual: Vec<Result<String, String>> = paths
                        .iter()
                        .map(|path| {
                            let bound = sources.source_product_bytes(path).unwrap().unwrap();
                            let budget = hirsel_engine_admission::ProcessBudget::new(bound);
                            let products = sources
                                .source_products(path, budget.reserve(bound).unwrap())
                                .unwrap()
                                .unwrap();
                            let context = hirsel_proof_export::Cx::new(
                                std::rc::Rc::new(|_| hirsel_proof_export::Location::unknown()),
                                sources.signatures().types().clone(),
                            );
                            Ok(hirsel_codec::to_text_with(products.constant().unwrap(), context).0)
                        })
                        .collect();
                    assert_eq!(actual, expected, "jobs={jobs} leaf={leaf} repeat={repeat}");
                }
            }
        }
    });
}

#[test]
fn frozen_frontier_cycles_and_failures_match_serial() {
    with_compiler_stack(|| {
        let source = "const GOOD: u32 = comptime 7\nconst A: u32 = comptime B\nconst B: u32 = comptime C\nconst C: u32 = comptime A\nconst BAD: u32 = comptime missing\n";
        let paths = vec![path("C"), path("A"), path("GOOD"), path("B"), path("BAD")];
        for jobs in [1, 2, 8] {
            for warm in [false, true] {
                let mut control = engine(source);
                let expected = scalar_frontier_text(&mut control, &paths);
                let mut candidate = engine(source);
                if warm {
                    candidate.materialize_value(&path("GOOD")).unwrap();
                }
                let actual = frontier_text(&mut candidate, paths.clone(), jobs);
                assert_eq!(actual.text, expected, "jobs={jobs} warm={warm}");
            }
        }
    });
}

/// A rendezvous inside the real evaluation-lowering adapter proves independent
/// roots execute on distinct workers. A serial or whole-query-locked facade
/// times out here instead of accidentally passing the value differential.
#[test]
fn frozen_frontier_evaluation_tasks_overlap() {
    use hirsel_requests::CompileOperation;
    use hirsel_requests::CompileResult;
    use hirsel_requests::CompilerRequests;
    use std::sync::{Arc, Condvar, Mutex};
    struct Overlap {
        inner: Arc<dyn LoweringPhase>,
        workers: Mutex<Vec<std::thread::ThreadId>>,
        ready: Condvar,
    }
    impl LoweringPhase for Overlap {
        fn lower(
            &self,
            t: &hirsel_types::TypeInterner,
            m: &hirsel_types::TypeMemo,
            body: &hirsel_query_products::AnalyzedBody,
            comptime: &hirsel_query_products::Comptime,
            signature: &hirsel_types::Signature,
        ) -> Result<QueryOutput<hirsel_tir::Definition>, QueryError> {
            self.inner.lower(t, m, body, comptime, signature)
        }
        fn lower_evaluation(
            &self,
            t: &hirsel_types::TypeInterner,
            m: &hirsel_types::TypeMemo,
            body: &hirsel_query_products::AnalyzedBody,
            staged: &[(hirsel_syntax::ExprId, hirsel_types::TypeId)],
            signature: &hirsel_types::Signature,
            expression: hirsel_syntax::ExprId,
            result: hirsel_types::TypeId,
            constants: &[(hirsel_syntax::ExprId, hirsel_tir::Materialized)],
        ) -> Result<hirsel_tir::Definition, QueryError> {
            let mut workers = self.workers.lock().unwrap();
            let thread = std::thread::current().id();
            if !workers.contains(&thread) {
                workers.push(thread);
            }
            self.ready.notify_all();
            let (workers, _) = self
                .ready
                .wait_timeout_while(workers, std::time::Duration::from_secs(10), |workers| {
                    workers.len() < 2
                })
                .unwrap();
            assert!(workers.len() >= 2, "evaluation work did not overlap");
            drop(workers);
            self.inner
                .lower_evaluation(t, m, body, staged, signature, expression, result, constants)
        }
        fn entry_body(&self) -> hirsel_tir::BodyId {
            self.inner.entry_body()
        }
    }
    thread_local! {
        static OVERLAP: std::cell::RefCell<Option<Arc<Overlap>>> = const {
            std::cell::RefCell::new(None)
        };
    }
    fn overlap_adapters() -> PhaseAdapters {
        let mut adapters = compiler_adapters();
        adapters.lowering = OVERLAP.with(|overlap| {
            let overlap: Arc<dyn LoweringPhase> = overlap.borrow().as_ref().unwrap().clone();
            Some(overlap)
        });
        adapters
    }
    with_compiler_stack(|| {
        for jobs in [2, 8] {
            let overlap = Arc::new(Overlap {
                inner: compiler_adapters().lowering.unwrap(),
                workers: Mutex::new(vec![]),
                ready: Condvar::new(),
            });
            OVERLAP.with(|observed| *observed.borrow_mut() = Some(overlap.clone()));
            let directory = tempfile::tempdir().unwrap();
            let manifest = directory.path().join("hirsel.json");
            std::fs::write(
                directory.path().join("overlap.hir"),
                "const A: u32 = comptime 11\nconst B: u32 = comptime 12\n",
            )
            .unwrap();
            std::fs::write(
                &manifest,
                r#"{"schema":1,"package":{"name":"standalone","version":"0"},"sources":[{"path":"overlap.hir","module":[]}],"dependencies":[]}"#,
            )
            .unwrap();
            let config = CompileConfig {
                jobs: hirsel_intern::jobs::Jobs::new(jobs),
                ..CompileConfig::for_memory_cap(None)
            };
            let compiler = COMPILER.with_config(config.clone()).with_phase_adapters(
                overlap_adapters,
                hirsel_syntax::ContentHash::of_bytes(b"hirsel.overlapping-request-evaluations.v1")
                    .0,
            );
            let mut candidate = CompilerRequests::for_host(compiler, config);
            let output = candidate
                .execute(
                    &manifest,
                    CompileOperation::Check {
                        lowering: LoweringPolicy::Comptime,
                    },
                )
                .unwrap();
            let CompileResult::Check(checked) = &output.result else {
                panic!("expected Check");
            };
            assert_eq!(checked.completion, CheckCompletion::Complete);
            assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
            assert!(overlap.workers.lock().unwrap().len() >= 2);
            OVERLAP.with(|observed| *observed.borrow_mut() = None);
        }
    });
}

/// Staged registrations require a new snapshot round, but their evaluations
/// must return to workers. A caller-thread retry is observable here.
#[test]
fn frozen_frontier_staged_rounds_evaluate_on_workers() {
    use std::sync::Arc;
    struct WorkerOnly(Arc<dyn LoweringPhase>, bool);
    impl LoweringPhase for WorkerOnly {
        fn lower(
            &self,
            t: &hirsel_types::TypeInterner,
            m: &hirsel_types::TypeMemo,
            body: &hirsel_query_products::AnalyzedBody,
            comptime: &hirsel_query_products::Comptime,
            signature: &hirsel_types::Signature,
        ) -> Result<QueryOutput<hirsel_tir::Definition>, QueryError> {
            self.0.lower(t, m, body, comptime, signature)
        }
        fn lower_evaluation(
            &self,
            t: &hirsel_types::TypeInterner,
            m: &hirsel_types::TypeMemo,
            body: &hirsel_query_products::AnalyzedBody,
            staged: &[(hirsel_syntax::ExprId, hirsel_types::TypeId)],
            signature: &hirsel_types::Signature,
            expression: hirsel_syntax::ExprId,
            result: hirsel_types::TypeId,
            constants: &[(hirsel_syntax::ExprId, hirsel_tir::Materialized)],
        ) -> Result<hirsel_tir::Definition, QueryError> {
            if self.1 {
                assert_eq!(
                    std::thread::current().name(),
                    Some("hirsel-query"),
                    "staged demand fell back to caller evaluation"
                );
            }
            self.0
                .lower_evaluation(t, m, body, staged, signature, expression, result, constants)
        }
        fn entry_body(&self) -> hirsel_tir::BodyId {
            self.0.entry_body()
        }
    }
    thread_local! {
        static REQUIRE_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    fn worker_adapters() -> PhaseAdapters {
        let mut adapters = compiler_adapters();
        adapters.lowering = Some(Arc::new(WorkerOnly(
            adapters.lowering.take().unwrap(),
            REQUIRE_WORKER.with(|required| required.get()),
        )));
        adapters
    }
    with_compiler_stack(|| {
        let staged = "fn scaled[n:size](xs:Inline[u8,n])->u64{let _=xs;comptime n*n}\nconst A:u64=comptime {let xs:Inline[u8,2]=[1,2];scaled(xs)}\nconst B:u64=comptime {let xs:Inline[u8,4]=[1,2,3,4];scaled(xs)}\n";
        let generated = "type Seed {x:u32}\nfn body_text()->String{\"fn()->u32 uses {}{11u32}\"}\nfn make(s:TypeShape[Seed])->Result[Body[fn()->u32 uses {}],String]{Ok(codegen.body(body_text()))}\nfn chosen()->u32 uses {}=comptime make(shape(Seed))\nconst A:u32=comptime chosen()\nconst B:u32=comptime chosen()\n";
        let generic = "type Point {x:u64}\nfn make[T](s:TypeShape[T])->Result[Body[fn(T)->u64 uses {}],String]{Ok(codegen.body(\"fn(p:standalone@0/Point)->u64 uses {}{12u64}\"))}\nfn width[T](p:T)->u64 uses {}=comptime make(shape(T))\nconst A:u64=comptime width(Point(x:1))\nconst B:u64=comptime width(Point(x:2))\n";
        for source in [staged, generated, generic] {
            for jobs in [1, 2, 8] {
                let paths = vec![path("B"), path("A"), path("B")];
                let mut control = engine(source);
                let expected: Vec<_> = paths
                    .iter()
                    .map(|p| {
                        let result = control.materialize_value(p);
                        outcome_text(&control, result)
                    })
                    .collect();
                assert!(
                    expected.iter().all(Result::is_ok),
                    "{expected:?}; jobs={jobs}; source={source}; phase={:?}",
                    control.last_phase
                );
                use hirsel_requests::CompileOperation;
                use hirsel_requests::CompileResult;
                use hirsel_requests::CompilerRequests;
                let directory = tempfile::tempdir().unwrap();
                let manifest = directory.path().join("hirsel.json");
                std::fs::write(directory.path().join("comptime.hir"), source).unwrap();
                std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"standalone","version":"0"},"sources":[{"path":"comptime.hir","module":[]}],"dependencies":[]}"#).unwrap();
                let config = CompileConfig {
                    jobs: hirsel_intern::jobs::Jobs::new(jobs),
                    store: Arc::new(store::DirStore::new(directory.path().join("products"))),
                    hold_bytes: 0,
                    ..Default::default()
                };
                REQUIRE_WORKER.with(|required| required.set(jobs > 1));
                let compiler = COMPILER.with_config(config.clone()).with_phase_adapters(
                    worker_adapters,
                    hirsel_syntax::ContentHash::of_bytes(b"hirsel.staged-request-worker.v1").0,
                );
                let mut candidate = CompilerRequests::for_host(compiler, config);
                for warm in [false, true] {
                    let output = candidate
                        .execute(&manifest, CompileOperation::Precompute)
                        .unwrap_or_else(|error| panic!("jobs={jobs} warm={warm}: {error}"));
                    let CompileResult::Precomputed { sources, .. } = &output.result else {
                        panic!("expected source precompute");
                    };
                    let actual: Vec<Result<String, String>> = paths
                        .iter()
                        .map(|path| {
                            let bound = sources.source_product_bytes(path).unwrap().unwrap();
                            let budget = hirsel_engine_admission::ProcessBudget::new(bound);
                            let products = sources
                                .source_products(path, budget.reserve(bound).unwrap())
                                .unwrap()
                                .unwrap();
                            let context = hirsel_proof_export::Cx::new(
                                std::rc::Rc::new(|_| hirsel_proof_export::Location::unknown()),
                                sources.signatures().types().clone(),
                            );
                            Ok(hirsel_codec::to_text_with(products.constant().unwrap(), context).0)
                        })
                        .collect();
                    assert_eq!(actual, expected, "jobs={jobs} warm={warm}");
                    if source == generated {
                        let scalar = control.check_definition(&path("chosen")).unwrap();
                        let bound = sources
                            .checked_dependency_decode_bytes(&path("chosen"))
                            .unwrap();
                        let budget = hirsel_engine_admission::ProcessBudget::new(bound);
                        let worker = sources
                            .checked_dependencies(&path("chosen"), budget.reserve(bound).unwrap())
                            .unwrap();
                        assert!(
                            scalar
                                .dependencies
                                .iter()
                                .any(|d| *d.definition.path() == path("make"))
                        );
                        assert_eq!(
                            worker.dependencies(),
                            &*scalar.dependencies,
                            "checked dependencies jobs={jobs} warm={warm}"
                        );
                    }
                }
            }
        }
    });
}

#[test]
fn frozen_frontier_generator_failures_keep_scalar_cycle_and_literal_sites() {
    with_compiler_stack(|| {
        let make = |body: &str| {
            format!(
                "type Seed {{x:u32}}\nfn make(s:TypeShape[Seed])->Result[Body[fn()->u32 uses {{}}],String]{{{body}}}\nfn chosen()->u32 uses {{}}=comptime make(shape(Seed))\nconst A:u32=comptime chosen()\nconst B:u32=comptime chosen()\n"
            )
        };
        for (rule, source) in [
            (
                "CT-4",
                make("let _=A;Ok(codegen.body(\"fn()->u32 uses {}{11u32}\"))"),
            ),
            ("CT-1", make("Error(\"expected generator failure\")")),
            (
                "LANG-7",
                make("Ok(codegen.body(\"fn()->u64 uses {}{11u64}\"))"),
            ),
        ] {
            let paths = vec![path("B"), path("A"), path("B")];
            let mut control = engine(&source);
            let expected = scalar_frontier_text(&mut control, &paths);
            assert!(
                (!expected.errors.is_empty()
                    && expected.errors.iter().all(|error| error.contains(rule))),
                "{expected:?}; expected {rule}"
            );
            for jobs in [1, 2, 8] {
                let mut candidate = engine(&source);
                let actual = frontier_text(&mut candidate, paths.clone(), jobs);
                assert_eq!(actual.text, expected, "jobs={jobs} source={source}");
            }
        }
    });
}

#[test]
fn frozen_frontier_generator_helper_edits_refresh_text_and_execution_reads() {
    with_compiler_stack(|| {
        let source = "type Seed {x:u32}\nfn body_text()->String{\"fn()->u32 uses {}{11u32}\"}\nfn make(s:TypeShape[Seed])->Result[Body[fn()->u32 uses {}],String]{Ok(codegen.body(body_text()))}\nfn chosen()->u32 uses {}=comptime make(shape(Seed))\nconst A:u32=comptime chosen()\nconst B:u32=comptime chosen()\n";
        let paths = vec![path("B"), path("A"), path("B")];
        for jobs in [1, 2, 8] {
            let mut candidate = engine(source);
            for revision in [
                source.to_owned(),
                source.replace("11u32", "23u32"),
                source.replace("codegen.body(body_text())", "codegen.body(missing_text())"),
                source.to_owned(),
            ] {
                let file = candidate
                    .sources
                    .add("comptime.hir", revision.clone())
                    .unwrap();
                candidate.parse_file(file, &default_context()).unwrap();
                let mut control = engine(&revision);
                let expected = scalar_frontier_text(&mut control, &paths);
                let actual = frontier_text(&mut candidate, paths.clone(), jobs);
                assert_eq!(actual.text, expected, "jobs={jobs} source={revision}");
                if revision.contains("missing_text()") {
                    assert!(!actual.text.errors.is_empty());
                }
                if actual.text.errors.is_empty() {
                    let scalar = control.check_definition(&path("chosen")).unwrap();
                    let sources = actual.sources.as_ref().unwrap();
                    let bytes = sources
                        .checked_dependency_decode_bytes(&path("chosen"))
                        .unwrap();
                    let worker = sources
                        .checked_dependencies(
                            &path("chosen"),
                            actual.budget.reserve(bytes).unwrap(),
                        )
                        .unwrap();
                    assert_eq!(worker.dependencies(), &*scalar.dependencies);
                    let reads = actual
                        .inspection
                        .as_ref()
                        .unwrap()
                        .memos
                        .iter()
                        .find(|(key, _)| {
                            key.def.path() == &path("chosen") && key.kind == QueryKind::Check
                        })
                        .map(|(_, reads)| reads)
                        .unwrap();
                    assert!(
                        reads.iter().any(|(key, _)| *key
                            == hirsel_query_products::reads::ReadKey::ExecutedBody(
                                hirsel_syntax::DefId::intern(&path("body_text"))
                            )),
                        "jobs={jobs} revision={revision}\nworker={reads:?}\nscalar={:?}",
                        control.memo_reads(&path("chosen"), QueryKind::Check)
                    );
                }
            }
        }
    });
}

#[test]
fn frozen_frontier_generic_generator_failures_keep_selecting_site_labels() {
    with_compiler_stack(|| {
        for (rule, body, nested) in [
            ("CT-1", "Error(\"generic generator failed\")", false),
            ("CT-4", "let _=A;Error(\"unreachable\")", false),
            (
                "CT-1",
                "let _=inner(Point(x:0));Error(\"unreachable\")",
                true,
            ),
            (
                "LANG-7",
                "Ok(codegen.body(\"fn(p:standalone@0/Point)->u32 uses {}{12u32}\"))",
                false,
            ),
        ] {
            let source = format!(
                "type Point {{x:u64}}\nfn make[T](s:TypeShape[T])->Result[Body[fn(T)->u64 uses {{}}],String]{{{body}}}\nfn width[T](p:T)->u64 uses {{}}=comptime make(shape(T))\nconst A:u64=comptime width(Point(x:1))\nconst B:u64=comptime width(Point(x:2))\n"
            );
            let source = if nested {
                source
                    + "fn inner_make[T](s:TypeShape[T])->Result[Body[fn(T)->u64 uses {}],String]{Error(\"inner failed\")}\nfn inner[T](p:T)->u64 uses {}=comptime inner_make(shape(T))\n"
            } else {
                source
            };
            for jobs in [1, 2, 8] {
                for warm in [false, true] {
                    let paths = vec![path("B"), path("A"), path("B")];
                    let mut control = engine(&source);
                    let mut candidate = engine(&source);
                    if warm {
                        assert!(control.materialize_value(&path("A")).is_err());
                        assert!(candidate.materialize_value(&path("A")).is_err());
                    }
                    let expected = scalar_frontier_text(&mut control, &paths);
                    assert!(
                        (!expected.errors.is_empty()
                            && expected.errors.iter().all(|error| error.contains(rule)))
                    );
                    if !warm {
                        assert!(
                            expected
                                .errors
                                .iter()
                                .any(|error| error.contains("selects instance"))
                        );
                    }
                    let actual = frontier_text(&mut candidate, paths, jobs);
                    assert_eq!(actual.text, expected, "jobs={jobs} warm={warm} rule={rule}");
                }
            }
        }
    });
}

#[test]
fn frozen_frontier_releases_completed_typed_bodies_before_next_root() {
    use std::sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    };
    struct ObservedModes {
        inner: Arc<dyn ModesPhase>,
        bodies: Mutex<Vec<Weak<hirsel_query_products::TypedBody>>>,
        peak: AtomicUsize,
    }
    impl ModesPhase for ObservedModes {
        fn check_modes(
            &self,
            t: &hirsel_types::TypeInterner,
            m: &hirsel_types::TypeMemo,
            body: Arc<hirsel_query_products::TypedBody>,
            signature: &hirsel_types::Signature,
            products: &mut PhaseProducts,
            provider: &mut dyn hirsel_types::SignatureProvider,
        ) -> Result<QueryOutput<hirsel_query_products::AnalyzedBody>, QueryError> {
            let output = self
                .inner
                .check_modes(t, m, body, signature, products, provider)?;
            let mut bodies = self.bodies.lock().unwrap();
            bodies.retain(|body| body.strong_count() != 0);
            bodies.push(Arc::downgrade(&output.value.typed));
            self.peak.fetch_max(bodies.len(), Ordering::Relaxed);
            Ok(output)
        }
    }
    with_compiler_stack(|| {
        for jobs in [1, 2, 8] {
            let mut adapters = compiler_adapters();
            let observed = Arc::new(ObservedModes {
                inner: adapters.modes.take().unwrap(),
                bodies: Mutex::new(vec![]),
                peak: AtomicUsize::new(0),
            });
            adapters.modes = Some(observed.clone());
            let mut candidate = QueryEngine::new(default_environment(), adapters);
            let source: String = (0..64)
                .map(|i| format!("const task{i}:u64=comptime {i}u64\n"))
                .collect();
            let file = candidate.sources.add("bounded-s1.hir", source).unwrap();
            candidate.parse_file(file, &default_context()).unwrap();
            let paths: Vec<_> = (0..64).rev().map(|i| path(&format!("task{i}"))).collect();
            let outcomes = crate::request_graph::constant_outcomes(
                &mut candidate,
                paths.clone(),
                hirsel_intern::jobs::Jobs::new(jobs),
            );
            assert!(outcomes.iter().all(|outcome| outcome.result.is_ok()));
            assert_eq!(
                outcomes.iter().map(|o| &o.definition).collect::<Vec<_>>(),
                paths.iter().collect::<Vec<_>>()
            );
            let peak = observed.peak.load(Ordering::Relaxed);
            assert!(
                peak <= jobs,
                "jobs={jobs}: {peak} typed bodies accumulated across64 S1 roots"
            );
            assert!(
                observed
                    .bodies
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|body| body.strong_count() == 0)
            );
            for path in paths {
                assert!(
                    candidate
                        .definition_summary(hirsel_syntax::DefId::intern(&path))
                        .is_ok()
                );
                assert_eq!(candidate.recompute_count(&path, QueryKind::Check), 1);
                assert_eq!(candidate.recompute_count(&path, QueryKind::Modes), 1);
            }
        }
    });
}

#[test]
fn frozen_lowering_frontier_matches_scalar_for_nonconstant_comptime_and_edits() {
    with_compiler_stack(|| {
        let source = "const SEED:u32=7u32\nfn helper()->u32{SEED}\nfn A()->u32{comptime helper()}\nfn scaled[n:size](xs:Inline[u8,n])->u64{let _=xs;comptime n*n}\nfn B()->u64{comptime {let xs:Inline[u8,2]=[1,2];scaled(xs)}}\nfn BAD()->u32{comptime missing()}\n";
        let paths = vec![path("B"), path("A"), path("BAD"), path("A")];
        for jobs in [1, 2, 8] {
            let mut candidate = engine(source);
            for revision in [
                source.to_owned(),
                source.replace("7u32", "23u32"),
                source.replace("missing()", "helper()"),
                source.to_owned(),
            ] {
                let file = candidate
                    .sources
                    .add("comptime.hir", revision.clone())
                    .unwrap();
                candidate.parse_file(file, &default_context()).unwrap();
                let mut control = engine(&revision);
                let expected: Vec<_> = paths
                    .iter()
                    .map(|path| {
                        let value = control.lower_to_tir(path);
                        lowering_text(&control, value)
                    })
                    .collect();
                assert!(
                    expected[0].is_ok() && expected[1].is_ok(),
                    "expected accepted nonconstant roots: {expected:?}"
                );
                let actual = crate::request_graph::lowers(
                    &mut candidate,
                    paths.clone(),
                    hirsel_intern::jobs::Jobs::new(jobs),
                );
                assert_eq!(
                    actual
                        .iter()
                        .map(|outcome| &outcome.definition)
                        .collect::<Vec<_>>(),
                    paths.iter().collect::<Vec<_>>()
                );
                for outcome in &actual {
                    assert!(
                        !outcome
                            .reads
                            .as_ref()
                            .expect("completed query has read evidence")
                            .is_empty()
                    );

                    assert_eq!(
                        outcome.phase,
                        Some(if outcome.result.is_ok() {
                            QueryKind::Lower
                        } else {
                            QueryKind::Resolve
                        })
                    );
                    if outcome.definition == path("A") {
                        for name in ["A", "helper", "SEED"] {
                            assert!(
                                outcome
                                    .reads
                                    .as_ref()
                                    .expect("completed query has read evidence")
                                    .iter()
                                    .any(|(key, _)| *key
                                        == hirsel_query_products::reads::ReadKey::ExecutedBody(
                                            hirsel_syntax::DefId::intern(&path(name))
                                        )),
                                "missing execution read {name}"
                            );
                        }
                    }
                }
                let actual: Vec<_> = actual
                    .into_iter()
                    .map(|outcome| lowering_text(&candidate, outcome.result))
                    .collect();
                assert_eq!(actual, expected, "jobs={jobs} source={revision}");
            }
        }
    });
}

#[test]
fn frozen_lowering_frontier_keeps_lower_warnings_and_current_hit_metadata() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct WarningLower {
        inner: Arc<dyn LoweringPhase>,
        guard: Arc<AtomicBool>,
    }
    impl LoweringPhase for WarningLower {
        fn lower(
            &self,
            t: &hirsel_types::TypeInterner,
            m: &hirsel_types::TypeMemo,
            body: &hirsel_query_products::AnalyzedBody,
            comptime: &hirsel_query_products::Comptime,
            signature: &hirsel_types::Signature,
        ) -> Result<QueryOutput<hirsel_tir::Definition>, QueryError> {
            if self.guard.load(Ordering::Relaxed) {
                assert_eq!(
                    std::thread::current().name(),
                    Some("hirsel-job"),
                    "nonconstant root fell back to caller lowering"
                );
            }
            let mut output = self.inner.lower(t, m, body, comptime, signature)?;
            let mut warning = hirsel_diag::Diagnostic::error(
                hirsel_diag::RuleId::of("LANG-1"),
                body.typed.syntax.item.name_span,
                "lowering fixture warning",
            );
            warning.severity = hirsel_diag::Severity::Warning;
            output.diagnostics.push(warning);
            Ok(output)
        }
        fn lower_evaluation(
            &self,
            t: &hirsel_types::TypeInterner,
            m: &hirsel_types::TypeMemo,
            body: &hirsel_query_products::AnalyzedBody,
            staged: &[(hirsel_syntax::ExprId, hirsel_types::TypeId)],
            signature: &hirsel_types::Signature,
            expression: hirsel_syntax::ExprId,
            result: hirsel_types::TypeId,
            constants: &[(hirsel_syntax::ExprId, hirsel_tir::Materialized)],
        ) -> Result<hirsel_tir::Definition, QueryError> {
            if self.guard.load(Ordering::Relaxed) {
                assert_eq!(
                    std::thread::current().name(),
                    Some("hirsel-job"),
                    "nonconstant evaluation fell back to caller"
                );
            }
            self.inner
                .lower_evaluation(t, m, body, staged, signature, expression, result, constants)
        }
        fn entry_body(&self) -> hirsel_tir::BodyId {
            self.inner.entry_body()
        }
    }
    let make = || {
        let mut adapters = compiler_adapters();
        let guard = Arc::new(AtomicBool::new(false));
        adapters.lowering = Some(Arc::new(WarningLower {
            inner: adapters.lowering.take().unwrap(),
            guard: guard.clone(),
        }));
        let mut engine = QueryEngine::new(default_environment(), adapters);
        let file = engine
            .sources
            .add(
                "comptime.hir",
                "const SEED:u32=7u32\nfn A()->u32{comptime SEED}\n".into(),
            )
            .unwrap();
        engine.parse_file(file, &default_context()).unwrap();
        (engine, guard)
    };
    with_compiler_stack(|| {
        for jobs in [1, 2, 8] {
            for warm in [false, true] {
                let (mut control, _) = make();
                let expected = control.lower_to_tir(&path("A")).unwrap();
                assert_eq!(expected.diagnostics.len(), 1);
                assert_eq!(
                    expected.diagnostics[0].severity,
                    hirsel_diag::Severity::Warning
                );
                let (mut depth_control, _) = make();
                let expected_depth = crate::request_graph::lowers(
                    &mut depth_control,
                    vec![path("A")],
                    hirsel_intern::jobs::Jobs::new(1),
                )
                .remove(0)
                .depth
                .unwrap();
                let scalar_reads = control.memo_reads(&path("A"), QueryKind::Lower).unwrap();
                let (mut candidate, guard) = make();
                if warm {
                    assert_eq!(
                        candidate
                            .lower_to_tir(&path("A"))
                            .unwrap()
                            .diagnostics
                            .len(),
                        1
                    );
                }
                guard.store(jobs > 1, Ordering::Relaxed);
                let outcomes = crate::request_graph::lowers(
                    &mut candidate,
                    vec![path("A"), path("A")],
                    hirsel_intern::jobs::Jobs::new(jobs),
                );
                for outcome in outcomes {
                    assert_eq!(outcome.phase, Some(QueryKind::Lower));

                    let depth = outcome
                        .depth
                        .as_ref()
                        .expect("successful lower returns depth");
                    assert_eq!(
                        placed_evidence(&candidate.sources, format!("{depth:?}")),
                        placed_evidence(&depth_control.sources, format!("{expected_depth:?}")),
                        "jobs={jobs} warm={warm}"
                    );
                    assert!(
                        depth.first.iter().any(Option::is_some),
                        "constant requests must survive in lower depth evidence"
                    );
                    let output = outcome.result.unwrap();
                    assert_eq!(output.dependencies, expected.dependencies);
                    assert_eq!(
                        placed_evidence(&candidate.sources, format!("{:?}", output.diagnostics)),
                        placed_evidence(&control.sources, format!("{:?}", expected.diagnostics))
                    );
                    for (key, value) in scalar_reads.iter() {
                        if matches!(
                            key,
                            hirsel_query_products::reads::ReadKey::ValidationView(_)
                                | hirsel_query_products::reads::ReadKey::UniverseErrors
                                | hirsel_query_products::reads::ReadKey::HeapRole
                        ) {
                            continue;
                        }
                        assert_eq!(
                            outcome
                                .reads
                                .as_ref()
                                .expect("completed query has read evidence")
                                .get(key),
                            Some(*value),
                            "jobs={jobs} warm={warm} missing {key:?}"
                        );
                    }
                    assert!(
                        outcome
                            .reads
                            .as_ref()
                            .expect("completed query has read evidence")
                            .iter()
                            .any(|(key, _)| matches!(
                                key,
                                hirsel_query_products::reads::ReadKey::ValidationUniverse(_)
                            ))
                    );
                }
                let compact = crate::request_graph::checked_prefix(
                    &mut candidate,
                    vec![path("A"), path("A")],
                    hirsel_intern::jobs::Jobs::new(jobs),
                );
                assert_eq!(compact.len(), 2);
                for outcome in compact {
                    assert_eq!(outcome.phase, Some(QueryKind::Lower));

                    assert_eq!(
                        placed_evidence(
                            &candidate.sources,
                            format!("{:?}", outcome.depth.as_ref().unwrap())
                        ),
                        placed_evidence(&depth_control.sources, format!("{expected_depth:?}"))
                    );
                    let output = outcome.result.unwrap();
                    assert_eq!(output.dependencies, expected.dependencies);
                    assert_eq!(
                        placed_evidence(&candidate.sources, format!("{:?}", output.diagnostics)),
                        placed_evidence(&control.sources, format!("{:?}", expected.diagnostics))
                    );
                    assert!(
                        outcome
                            .reads
                            .as_ref()
                            .expect("completed query has read evidence")
                            .iter()
                            .any(|(key, _)| matches!(
                                key,
                                hirsel_query_products::reads::ReadKey::ValidationUniverse(_)
                            ))
                    );
                }
            }
        }
    });
}

#[test]
fn frozen_frontier_hold_budgets_preserve_staged_generated_cycle_and_depth_outcomes() {
    with_compiler_stack(|| {
        let cases = [
            (
                8,
                "const LEAF:u32=1u32\nfn helper()->u32{comptime LEAF}\nconst A:u32=comptime helper()\nconst B:u32=comptime A\n",
            ),
            (
                2,
                "const LEAF:u32=1u32\nfn helper()->u32{comptime LEAF}\nconst A:u32=comptime helper()\nconst B:u32=comptime A\n",
            ),
            (8, "const A:u32=comptime B\nconst B:u32=comptime A\n"),
            (
                8,
                "fn scaled[n:size](xs:Inline[u8,n])->u64{let _=xs;comptime n*n}\nconst A:u64=comptime {let xs:Inline[u8,2]=[1,2];scaled(xs)}\nconst B:u64=comptime A\n",
            ),
            (
                8,
                "type Point{x:u64}\nfn gen[T](s:TypeShape[T])->Result[Body[fn(T)->u64 uses {}],String]{Ok(codegen.body(\"fn(v:standalone@0/Point)->u64 uses {}{12}\"))}\nfn width[T](v:T)->u64 uses {}=comptime gen(shape(T))\nconst A:u64=comptime width(Point(x:1))\nconst B:u64=comptime A\n",
            ),
        ];
        let roots = vec![path("B"), path("A"), path("B")];
        for (limit, text) in cases {
            let mut control = engine(text);
            control.comptime_limits.call_depth = limit;
            let expected: Vec<_> = roots
                .iter()
                .map(|root| {
                    let result = control.materialize_value(root);
                    outcome_text(&control, result)
                })
                .collect();
            for jobs in [1, 2, 8] {
                for bytes in [0, 1, 8192] {
                    let mut candidate = engine(text).with_config(CompileConfig {
                        hold_bytes: bytes,
                        retention: memo::Retention::Resident,
                        ..CompileConfig::for_memory_cap(None)
                    });
                    candidate.comptime_limits.call_depth = limit;
                    for warm in [false, true] {
                        let actual: Vec<_> = crate::request_graph::constant_outcomes(
                            &mut candidate,
                            roots.clone(),
                            hirsel_intern::jobs::Jobs::new(jobs),
                        )
                        .into_iter()
                        .map(|outcome| outcome_text(&candidate, outcome.result))
                        .collect();
                        assert_eq!(
                            actual, expected,
                            "jobs={jobs} budget={bytes} warm={warm} source={text}"
                        );
                    }
                }
            }
        }
    });
}

#[test]
fn frozen_lowering_prefix_withholds_later_generated_and_staged_publication() {
    with_compiler_stack(|| {
        let source = "type Point{x:u64}\nfn gen[T](s:TypeShape[T])->Result[Body[fn(T)->u64 uses {}],String]{Ok(codegen.body(\"fn(v:standalone@0/Point)->u64 uses {}{12}\"))}\nfn width[T](v:T)->u64 uses {}=comptime gen(shape(T))\nfn scaled[n:size](xs:Inline[u8,n])->u64{let _=xs;comptime n*n}\nfn FIRST()->u64{comptime {let zero=0u64;1u64/zero}}\nconst LATER:u64=comptime {let xs:Inline[u8,2]=[1,2];scaled(xs)+width(Point(x:1))}\n";
        for jobs in [1, 2, 8] {
            let mut control = engine(source);
            let expected = control.lower_to_tir(&path("FIRST")).unwrap_err();
            let mut candidate = engine(source);
            let outcomes = crate::request_graph::checked_prefix(
                &mut candidate,
                vec![path("FIRST"), path("LATER")],
                hirsel_intern::jobs::Jobs::new(jobs),
            );
            assert_eq!(outcomes.len(), 1);
            assert_eq!(
                placed_evidence(
                    &candidate.sources,
                    format!("{:?}", outcomes[0].result.as_ref().unwrap_err())
                ),
                placed_evidence(&control.sources, format!("{expected:?}"))
            );
            assert_eq!(
                candidate.generated_sources().count(),
                control.generated_sources().count(),
                "jobs={jobs}: suffix generated source escaped the prefix"
            );
            assert!(
                candidate
                    .memo_reads(&path("LATER"), QueryKind::Summary)
                    .is_none(),
                "jobs={jobs}: later root published a check after the first failure"
            );
            assert!(
                candidate
                    .memo_reads(&path("scaled"), QueryKind::Summary)
                    .is_none(),
                "jobs={jobs}: later staged helper published"
            );
            assert!(
                candidate
                    .memo_reads(&path("gen"), QueryKind::Summary)
                    .is_none(),
                "jobs={jobs}: later generic generator published"
            );
        }
    });
}

#[test]
fn frozen_lowering_prefix_keeps_demanded_closure_and_final_outcomes_across_edits() {
    with_compiler_stack(|| {
        let source = "type Point{x:u64}\nconst SEED:u64=12\nfn gen[T](s:TypeShape[T])->Result[Body[fn(T)->u64 uses {}],String]{let _=SEED;Ok(codegen.body(\"fn(v:standalone@0/Point)->u64 uses {}{12}\"))}\nfn width[T](v:T)->u64 uses {}=comptime gen(shape(T))\nfn scaled[n:size](xs:Inline[u8,n])->u64{let _=xs;comptime n*n}\nfn FIRST()->u64{comptime LATER}\nconst LATER:u64=comptime {let xs:Inline[u8,2]=[1,2];scaled(xs)+width(Point(x:1))}\nfn BAD()->u64{comptime {let zero=0u64;1u64/zero}}\nfn UNUSED()->u64{comptime 99}\n";
        let roots = vec![
            path("FIRST"),
            path("LATER"),
            path("FIRST"),
            path("BAD"),
            path("UNUSED"),
        ];
        for jobs in [1, 2, 8] {
            let mut candidate = engine(source);
            for revision in [
                source.to_owned(),
                source.replace("SEED:u64=12", "SEED:u64=23"),
                source.replace("1u64/zero", "1u64+zero"),
            ] {
                let file = candidate
                    .sources
                    .add("comptime.hir", revision.clone())
                    .unwrap();
                candidate.parse_file(file, &default_context()).unwrap();
                let mut control = engine(&revision);
                let mut depth_control = engine(&revision);
                let mut expected = vec![];
                let mut depths = vec![];
                for root in &roots {
                    let result = control.lower_to_tir(root);
                    let failed = result.is_err();
                    expected.push(lowering_text(&control, result));
                    depths.push(if failed {
                        None
                    } else {
                        crate::request_graph::lowers(
                            &mut depth_control,
                            vec![root.clone()],
                            hirsel_intern::jobs::Jobs::ONE,
                        )
                        .remove(0)
                        .depth
                    });
                    if failed {
                        break;
                    }
                }
                for warm in [false, true] {
                    let actual = crate::request_graph::lower_prefix(
                        &mut candidate,
                        roots.clone(),
                        hirsel_intern::jobs::Jobs::new(jobs),
                    );
                    assert_eq!(
                        actual.len(),
                        expected.len(),
                        "jobs={jobs} warm={warm} serial failures={:?} actual failures={:?}",
                        expected
                            .iter()
                            .filter_map(|result| result.as_ref().err())
                            .collect::<Vec<_>>(),
                        actual
                            .iter()
                            .filter_map(|outcome| outcome.result.as_ref().err())
                            .collect::<Vec<_>>()
                    );
                    for (outcome, depth) in actual.iter().zip(&depths) {
                        assert!(
                            !outcome
                                .reads
                                .as_ref()
                                .expect("completed query has read evidence")
                                .is_empty()
                        );
                        assert_eq!(
                            outcome.phase,
                            Some(QueryKind::Lower),
                            "jobs={jobs} warm={warm} root={} revision={revision} result={:?}",
                            outcome.definition,
                            outcome.result
                        );

                        assert_eq!(
                            placed_evidence(&candidate.sources, format!("{:?}", outcome.depth)),
                            placed_evidence(&depth_control.sources, format!("{depth:?}")),
                            "jobs={jobs} warm={warm} root={} revision={revision}",
                            outcome.definition
                        );
                    }
                    let actual: Vec<_> = actual
                        .into_iter()
                        .map(|outcome| lowering_text(&candidate, outcome.result))
                        .collect();
                    assert_eq!(
                        actual, expected,
                        "jobs={jobs} warm={warm} revision={revision}"
                    );
                    assert_eq!(
                        candidate.generated_sources().count(),
                        control.generated_sources().count()
                    );
                    if expected.len() < roots.len() {
                        assert!(
                            candidate
                                .memo_reads(&path("UNUSED"), QueryKind::Summary)
                                .is_none(),
                            "suffix summary must remain unpublished"
                        );
                    }
                }
            }
        }
        let mut candidate = engine(source);
        let missing = crate::request_graph::lower_prefix(
            &mut candidate,
            vec![path("MISSING")],
            hirsel_intern::jobs::Jobs::ONE,
        )
        .remove(0);
        assert!(missing.reads.is_none());
        let Err(QueryError::Operational(failure)) = missing.result else {
            panic!("missing read evidence must refuse the request")
        };
        assert_eq!(failure.operation, "settled query evidence");
    });
}

#[test]
fn frozen_lowering_prefix_generator_rejections_keep_serial_selection_and_limits() {
    with_compiler_stack(|| {
        for body in [
            "Error(\"failed\")",
            "let _=A;Error(\"unreachable\")",
            "Ok(codegen.body(\"fn(v:standalone@0/Point)->u32 uses {}{12u32}\"))",
        ] {
            let source = format!(
                "type Point{{x:u64}}\nfn gen[T](s:TypeShape[T])->Result[Body[fn(T)->u64 uses {{}}],String]{{{body}}}\nfn width[T](v:T)->u64 uses {{}}=comptime gen(shape(T))\nconst A:u64=comptime width(Point(x:1))\nconst B:u64=comptime width(Point(x:2))\nconst UNUSED:u64=99\n"
            );
            for jobs in [1, 2, 8] {
                for warm in [false, true] {
                    let mut control = engine(&source);
                    let mut candidate = engine(&source);
                    if warm {
                        assert!(control.lower_to_tir(&path("A")).is_err());
                        assert!(candidate.lower_to_tir(&path("A")).is_err());
                    }
                    let expected = control.lower_to_tir(&path("B"));
                    if body == "Error(\"failed\")" {
                        let Err(QueryError::Diagnostics(diagnostics)) = &expected else {
                            panic!("the rejected generator must report its CT-1 obligation");
                        };
                        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
                        assert_eq!(diagnostics[0].rule.as_str(), "CT-1");
                        assert_eq!(diagnostics[0].notes, ["failed"]);
                    }
                    let actual = crate::request_graph::lower_prefix(
                        &mut candidate,
                        vec![path("B"), path("A"), path("UNUSED")],
                        hirsel_intern::jobs::Jobs::new(jobs),
                    );
                    assert_eq!(actual.len(), 1);
                    assert_eq!(
                        lowering_text(&candidate, actual[0].result.clone()),
                        lowering_text(&control, expected),
                        "jobs={jobs} warm={warm} source={source}"
                    );
                    assert!(
                        candidate
                            .memo_reads(&path("UNUSED"), QueryKind::Summary)
                            .is_none()
                    );
                }
            }
        }
        let source = "const LEAF:u64=1\nconst INNER:u64=comptime LEAF\nfn FIRST()->u64{comptime INNER}\nconst UNUSED:u64=99\n";
        for jobs in [1, 2, 8] {
            for limit in [0, 1, 2, 3] {
                let mut control = engine(source);
                let mut candidate = engine(source);
                control.comptime_limits.call_depth = limit;
                candidate.comptime_limits.call_depth = limit;
                let expected = control.lower_to_tir(&path("FIRST"));
                let failed = expected.is_err();
                let actual = crate::request_graph::lower_prefix(
                    &mut candidate,
                    vec![path("FIRST"), path("UNUSED")],
                    hirsel_intern::jobs::Jobs::new(jobs),
                );
                assert_eq!(actual.len(), if failed { 1 } else { 2 });
                assert_eq!(
                    lowering_text(&candidate, actual[0].result.clone()),
                    lowering_text(&control, expected)
                );
            }
        }
    });
}

#[test]
fn frozen_s1_prefix_tir_receipts_and_diagnostics_are_invariant_under_jobs_and_salts() {
    fn run() -> String {
        with_compiler_stack(|| {
            let cases = [
                "const LEAF:u64=7\nfn helper()->u64{comptime LEAF}\nfn A()->u64{comptime helper()}\nconst B:u64=comptime A()\n",
                "fn scaled[n:size](xs:Inline[u8,n])->u64{let _=xs;comptime n*n}\nconst A:u64=comptime {let xs:Inline[u8,2]=[1,2];scaled(xs)}\nconst B:u64=comptime A\n",
                "type Point{x:u64}\nfn gen[T](s:TypeShape[T])->Result[Body[fn(T)->u64 uses {}],String]{Ok(codegen.body(\"fn(v:standalone@0/Point)->u64 uses {}{12}\"))}\nfn width[T](v:T)->u64 uses {}=comptime gen(shape(T))\nconst A:u64=comptime width(Point(x:1))\nconst B:u64=comptime A\n",
                "type Seed{x:u64}\nfn gen(s:TypeShape[Seed])->Result[Body[fn()->u64 uses {}],String]{let _=A;Error(\"unreachable\")}\nfn chosen()->u64 uses {}=comptime gen(shape(Seed))\nconst A:u64=comptime chosen()\nconst B:u64=comptime A\n",
            ];
            let mut transcript = String::new();
            for source in cases {
                let mut expected = None;
                for jobs in [1, 2, 8] {
                    for budget in [0, 1, 8192, 1 << 20] {
                        let roots = vec![path("B"), path("A"), path("B")];
                        let mut scalar = engine(source);
                        let mut scalar_values = Vec::new();
                        for root in &roots {
                            let value = scalar.lower_to_tir(root);
                            let failed = value.is_err();
                            scalar_values.push(lowering_text(&scalar, value));
                            if failed {
                                break;
                            }
                        }
                        let mut candidate = engine(source).with_config(CompileConfig {
                            hold_bytes: budget,
                            retention: memo::Retention::Resident,
                            ..CompileConfig::for_memory_cap(None)
                        });
                        for warm in [false, true] {
                            let outcomes = crate::request_graph::lower_prefix(
                                &mut candidate,
                                roots.clone(),
                                hirsel_intern::jobs::Jobs::new(jobs),
                            );
                            let mut current = String::new();
                            assert_eq!(outcomes.len(), scalar_values.len());
                            for (outcome, expected_value) in
                                outcomes.into_iter().zip(&scalar_values)
                            {
                                let value = lowering_text(&candidate, outcome.result);
                                assert_eq!(
                                    &value, expected_value,
                                    "scalar cold jobs={jobs} budget={budget} warm={warm}"
                                );
                                current.push_str(&format!("{value:?}\n{:?}\n", outcome.phase));
                                current.push_str(&placed_evidence(
                                    &candidate.sources,
                                    format!("{:?}\n{:?}\n", outcome.depth, outcome.reads),
                                ));
                            }
                            if let Some(expected) = &expected {
                                assert_eq!(
                                    &current, expected,
                                    "jobs={jobs} budget={budget} warm={warm} source={source}"
                                );
                            } else {
                                expected = Some(current);
                            }
                        }
                    }
                }
                transcript.push_str(&expected.unwrap());
            }
            transcript
        })
    }
    let Some(outputs) = hirsel_intern::under_salts(
        "comptime::frozen_s1_prefix_tir_receipts_and_diagnostics_are_invariant_under_jobs_and_salts",
        &[1, 2, 3],
        run,
    ) else {
        return;
    };
    assert!(outputs[0].contains("CT-4"));
    assert!(outputs[0].contains("DefinitionSection") || outputs[0].contains("Definition"));
    assert!(outputs[0].contains("ValidationUniverse"));
    assert!(outputs.iter().all(|output| output == &outputs[0]));
}

#[test]
fn frozen_s1_prefix_resident_and_second_process_store_equal_cold_after_edits() {
    use std::{path::PathBuf, sync::Arc};
    fn run(
        engine: &mut QueryEngine,
        source: &str,
        jobs: usize,
        limit: usize,
        verify: Verify,
        manifest: u8,
        require_restore: bool,
    ) -> String {
        engine.verify = verify;
        engine.comptime_limits.call_depth = limit;
        engine.environment.package_manifest_hash =
            hirsel_syntax::ContentHash::of_bytes(&[manifest]);
        let file = engine.sources.add("comptime.hir", source.into()).unwrap();
        engine.parse_file(file, &default_context()).unwrap();
        // This ordinary helper is a real persisted P hit before the S1 demand.
        let _ = engine.mode_definition(&path("plain"));
        if require_restore {
            assert_eq!(
                engine.recompute_count(&path("plain"), QueryKind::Check),
                0,
                "child must restore the persisted helper before S1 expansion"
            );
        }
        let outcomes = crate::request_graph::lower_prefix(
            engine,
            vec![path("A"), path("B"), path("A")],
            hirsel_intern::jobs::Jobs::new(jobs),
        );
        let mut transcript = String::new();
        for outcome in outcomes {
            transcript.push_str(&format!(
                "{:?}\n{:?}\n",
                lowering_text(engine, outcome.result),
                outcome.phase
            ));
            transcript.push_str(&placed_evidence(
                &engine.sources,
                format!("{:?}\n{:?}\n", outcome.depth, outcome.reads),
            ));
        }
        transcript
    }
    if let Some(root) = std::env::var_os("HIRSEL_S1_O2_ROOT") {
        let root = PathBuf::from(root);
        let jobs: usize = std::env::var("HIRSEL_S1_O2_JOBS").unwrap().parse().unwrap();
        let source = std::fs::read_to_string(root.join("source.hir")).unwrap();
        let settings: Vec<usize> = std::fs::read_to_string(root.join("settings"))
            .unwrap()
            .split_whitespace()
            .map(|s| s.parse().unwrap())
            .collect();
        let output = with_compiler_stack(|| {
            let mut engine = QueryEngine::new(default_environment(), compiler_adapters())
                .with_config(CompileConfig {
                    store: Arc::new(store::DirStore::new(root.join("store"))),
                    retention: memo::Retention::Resident,
                    hold_bytes: 0,
                    ..CompileConfig::for_memory_cap(None)
                });
            let output = run(
                &mut engine,
                &source,
                jobs,
                settings[0],
                if settings[1] == 0 {
                    Verify::Off
                } else {
                    Verify::On
                },
                settings[2] as u8,
                settings[3] == 0,
            );
            output
        });
        std::fs::write(root.join(format!("output-{jobs}")), output).unwrap();
        return;
    }
    with_compiler_stack(|| {
        let fixture = crate::image_fixture::Fixture::new();
        let root = &fixture.0;
        let base = "pub opaque type Hidden:plain uses {}{value:u64}\nconst LEAF:u64=7\nfn plain()->u64{LEAF}\nfn private(v:Hidden)->u64{v.value}\nfn body_text()->String{\"fn(v:standalone@0/Hidden)->u64 uses {}{12}\"}\nfn gen[T](s:TypeShape[T])->Result[Body[fn(T)->u64 uses {}],String]{Ok(codegen.body(body_text()))}\nfn width[T](v:T)->u64 uses {}=comptime gen(shape(T))\nfn scaled[n:size](xs:Inline[u8,n])->u64{let _=xs;comptime n*n}\nconst A:u64=comptime plain()+private(Hidden(value:1))\nconst B:u64=comptime {let xs:Inline[u8,2]=[1,2];scaled(xs)+width(Hidden(value:1))}\n";
        let edits = [
            (base.to_owned(), 8, Verify::On, 0),
            (base.replace("LEAF:u64=7", "LEAF:u64=23"), 8, Verify::On, 0),
            (base.replace("uses {}{12}", "uses {}{19}"), 8, Verify::On, 0),
            (
                base.replace("fn plain()->u64", "fn plain()->u32"),
                8,
                Verify::On,
                0,
            ),
            (base.replace("value:u64", "value:u32"), 8, Verify::On, 0),
            (format!("fn earlier_item(){{}}\n{base}"), 8, Verify::On, 0),
            (base.replace("{LEAF}", "{absent}"), 8, Verify::On, 0),
            (
                format!("{base}\nfn plain()->u64{{LEAF}}\n"),
                8,
                Verify::On,
                0,
            ),
            (base.to_owned(), 2, Verify::On, 0),
            (base.to_owned(), 8, Verify::Off, 0),
            (base.to_owned(), 8, Verify::On, 0),
            (base.to_owned(), 8, Verify::On, 1),
            (base.to_owned(), 8, Verify::On, 0),
        ];
        let mut resident = QueryEngine::new(default_environment(), compiler_adapters())
            .with_config(CompileConfig {
                store: Arc::new(store::DirStore::new(root.join("store"))),
                retention: memo::Retention::Resident,
                hold_bytes: 0,
                ..CompileConfig::for_memory_cap(None)
            });
        for (revision, (source, limit, verify, manifest)) in edits.into_iter().enumerate() {
            let mut cold = QueryEngine::new(default_environment(), compiler_adapters())
                .with_config(CompileConfig {
                    store: Arc::new(store::NoneStore),
                    retention: memo::Retention::Resident,
                    hold_bytes: 0,
                    ..CompileConfig::for_memory_cap(None)
                });
            let expected = run(&mut cold, &source, 1, limit, verify, manifest, false);
            if revision == 0 {
                assert_eq!(
                    expected
                        .lines()
                        .filter(|line| line.starts_with("Ok("))
                        .count(),
                    3,
                    "base fixture must accept all three requested roots: {expected}"
                );
            }
            let actual = run(&mut resident, &source, 8, limit, verify, manifest, false);
            assert_eq!(actual, expected, "resident revision={revision}");
            std::fs::write(root.join("source.hir"), &source).unwrap();
            std::fs::write(
                root.join("settings"),
                format!(
                    "{limit} {} {manifest} {revision}",
                    if verify == Verify::Off { 0 } else { 1 }
                ),
            )
            .unwrap();
            for jobs in [1, 2, 8] {
                let child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["comptime::frozen_s1_prefix_resident_and_second_process_store_equal_cold_after_edits", "--exact", "--nocapture", "--test-threads=1"])
                    .env("HIRSEL_S1_O2_ROOT", root).env("HIRSEL_S1_O2_JOBS", jobs.to_string()).output().unwrap();
                assert!(
                    child.status.success(),
                    "revision={revision} jobs={jobs}\n{}\n{}",
                    String::from_utf8_lossy(&child.stdout),
                    String::from_utf8_lossy(&child.stderr)
                );
                let actual = std::fs::read_to_string(root.join(format!("output-{jobs}"))).unwrap();
                assert_eq!(
                    actual, expected,
                    "second-process revision={revision} jobs={jobs}"
                );
            }
        }
        assert!(
            std::fs::read_dir(root.join("store"))
                .unwrap()
                .next()
                .is_some(),
            "the oracle must exercise durable products"
        );
    });
}

#[test]
fn batch_staged_exports_keep_cached_instances_after_unrelated_edits() {
    use std::sync::Arc;
    fn make(jobs: usize) -> QueryEngine {
        QueryEngine::new(default_environment(), compiler_adapters()).with_config(CompileConfig {
            jobs: hirsel_intern::jobs::Jobs::new(jobs),
            hold_bytes: 0,
            retention: memo::Retention::Batch,
            store: Arc::new(store::NoneStore),
            ..CompileConfig::for_memory_cap(None)
        })
    }
    fn check(engine: &mut QueryEngine, source: &str) {
        let file = engine
            .sources
            .add("staged-export.hir", source.into())
            .unwrap();
        let checked = source_check_request(engine, file, &default_context()).unwrap();
        assert_eq!(checked.completion, CheckCompletion::Complete);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    }
    with_compiler_stack(|| {
        let source = "fn scaled[n:size](xs:Inline[u8,n])->u64{let _=xs;comptime n*n}\nconst VALUE:u64=comptime {let xs:Inline[u8,2]=[1,2];scaled(xs)}\nconst GOOD:u64=comptime 7\n";
        for jobs in [1, 2, 8] {
            let mut warm = make(jobs);
            check(&mut warm, source);
            for revision in [
                source.replace("comptime 7", "comptime 9"),
                source.replace("n*n", "n+n+1"),
                format!("fn earlier_item(){{}}\n{source}"),
                source.to_owned(),
            ] {
                check(&mut warm, &revision);
                let mut cold = make(jobs);
                check(&mut cold, &revision);
                let expected = cold.export_tir(&path("VALUE")).unwrap();
                let actual = warm.export_tir(&path("VALUE")).unwrap();
                assert!(expected.contains("$constant:$comptime:scaled"));
                assert_eq!(
                    warm.materialize_constant(&path("VALUE")).unwrap(),
                    Constant::Integer(if revision.contains("n+n+1") { 5 } else { 4 })
                );
                assert!(
                    actual == expected,
                    "jobs={jobs}: full export mismatch after {revision}; first differing lines {:?}",
                    actual.lines().zip(expected.lines()).find(|(a, b)| a != b)
                );
            }
        }
    });
}

#[test]
fn frozen_lowering_prefix_publishes_exact_staged_instances_through_cache_hits() {
    fn selected_constants(engine: &mut QueryEngine) -> Vec<(DefPath, String)> {
        let input = engine
            .native_input_for(&[path("FIRST"), path("scaled")])
            .unwrap();
        input
            .snapshot()
            .constants
            .iter()
            .filter(|(path, _)| path.0.package == default_context().package)
            .map(|(path, value)| (path.clone(), materialized_text(engine, value)))
            .collect()
    }
    with_compiler_stack(|| {
        let source = "fn scaled[n:size](xs:Inline[u8,n])->u64{let _=xs;comptime n*n}\nconst HELPER:u64=comptime {let xs:Inline[u8,2]=[1,2];scaled(xs)}\nconst FIRST:u64=comptime HELPER\nfn BAD()->u64{comptime {let zero=0u64;1u64/zero}}\nfn LATER()->u64{comptime {let xs:Inline[u8,3]=[1,2,3];scaled(xs)}}\n";
        for jobs in [1, 2, 8] {
            for bytes in [0, 8192] {
                for compact in [false, true] {
                    let mut control = engine(source);
                    control.lower_to_tir(&path("FIRST")).unwrap();
                    control.lower_to_tir(&path("BAD")).unwrap_err();
                    let expected = selected_constants(&mut control);
                    assert!(
                        expected.iter().any(|(path, _)| path
                            .0
                            .name
                            .contains("$constant:$comptime:scaled:2:2"))
                    );
                    assert!(
                        !expected.iter().any(|(path, _)| path
                            .0
                            .name
                            .contains("$constant:$comptime:scaled:2:3"))
                    );
                    let mut candidate = engine(source).with_config(CompileConfig {
                        hold_bytes: bytes,
                        ..CompileConfig::for_memory_cap(None)
                    });
                    for warm in [false, true] {
                        let roots = vec![path("FIRST"), path("FIRST"), path("BAD"), path("LATER")];
                        let outcomes: Vec<_> = if compact {
                            crate::request_graph::checked_prefix(
                                &mut candidate,
                                roots,
                                hirsel_intern::jobs::Jobs::new(jobs),
                            )
                            .into_iter()
                            .map(|outcome| outcome.result.map(|_| ()))
                            .collect()
                        } else {
                            crate::request_graph::lower_prefix(
                                &mut candidate,
                                roots,
                                hirsel_intern::jobs::Jobs::new(jobs),
                            )
                            .into_iter()
                            .map(|outcome| outcome.result.map(|_| ()))
                            .collect()
                        };
                        assert_eq!(outcomes.len(), 3);
                        assert!(outcomes[0].is_ok() && outcomes[1].is_ok() && outcomes[2].is_err());
                        let actual = selected_constants(&mut candidate);
                        assert_eq!(
                            actual, expected,
                            "jobs={jobs} bytes={bytes} compact={compact} warm={warm}: staged values escaped or were dropped"
                        );
                    }
                }
            }
        }
    });
}

/// U3: a run holds runtime type rows only for types its lowered closure
/// reaches. hello never names std's `bigint.BigInt`, so running hello against
/// installed std leaves it without a row, while hello's own error type, which
/// its closure reaches, is answered.
#[test]
fn running_hello_builds_runtime_type_rows_only_for_types_its_closure_reaches() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("hello.hir"),
            "pub type HelloError = Grant(GrantError) | Output(OutputError)\n\
             pub fn main(rt: Io) -> Result[(), HelloError] {\n  \
             let output = io.stdout(rt) ? Grant\n  \
             out.print(output, \"Hello, world\\n\") ? Output\n  Ok(())\n}\n",
        )
        .unwrap();
        let stdlib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../stdlib/std@1/hirsel.json")
            .canonicalize()
            .unwrap();
        let manifest = dir.path().join("hirsel.json");
        std::fs::write(&manifest, serde_json::json!({"schema":1,"package":{"name":"hello","version":"0"},"sources":[{"path":"hello.hir","module":[]}],"dependencies":[{"manifest":stdlib,"package":{"name":"std","version":"1"},"expose":[]}],"entry":{"module":[],"name":"main"}}).to_string()).unwrap();
        let prepared =
            hirsel_driver::request_tir(&manifest, hirsel_driver::session::Policy::default(), None)
                .unwrap();
        let config = RunConfiguration {
            entry: prepared.entry.clone(),
            hash_seed: 0,
            inputs: vec![],
        };
        for checked in [false, true] {
            let execution = hirsel_driver::testing::observe_tir(
                &prepared,
                &config,
                SimulationConfig::default(),
                hirsel_interp::Limits::default(),
                checked,
            )
            .unwrap();
            let observation =
                hirsel_native::Observation::of_reference(execution.observation.as_ref().unwrap());
            assert_eq!(observation.exit_code, Some(0), "{observation:?}");
        }
        let mut error = prepared.entry.clone();
        error.0.name = "HelloError".into();
        assert!(prepared.runtime.get(&error).unwrap().is_some());
        let big = DefPath(hirsel_syntax::ItemPath {
            package: hirsel_syntax::PackageId {
                name: "std".into(),
                version: "1".into(),
            },
            modules: vec!["bigint".into()],
            name: "BigInt".into(),
        });
        assert!(
            !prepared.runtime.has_row(&big),
            "hello's run built a row for std BigInt, which its closure never reaches"
        );
    });
}

#[test]
fn frozen_frontier_generator_demand_skips_a_dead_compile_time_branch() {
    use hirsel_engine_runtime::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct CountGenerators(Arc<dyn LoweringPhase>, Arc<AtomicUsize>);
    impl LoweringPhase for CountGenerators {
        fn lower(
            &self,
            types: &hirsel_types::TypeInterner,
            memo: &hirsel_types::TypeMemo,
            body: &hirsel_query_products::AnalyzedBody,
            comptime: &hirsel_query_products::Comptime,
            signature: &hirsel_types::Signature,
        ) -> Result<QueryOutput<hirsel_tir::Definition>, QueryError> {
            self.0.lower(types, memo, body, comptime, signature)
        }
        fn lower_evaluation(
            &self,
            types: &hirsel_types::TypeInterner,
            memo: &hirsel_types::TypeMemo,
            body: &hirsel_query_products::AnalyzedBody,
            staged: &[(hirsel_syntax::ExprId, hirsel_types::TypeId)],
            signature: &hirsel_types::Signature,
            expression: hirsel_syntax::ExprId,
            result: hirsel_types::TypeId,
            constants: &[(hirsel_syntax::ExprId, hirsel_tir::Materialized)],
        ) -> Result<hirsel_tir::Definition, QueryError> {
            if matches!(
                body.typed.syntax.item.generated,
                Some(hirsel_syntax::Generated::Generator(_))
            ) {
                self.1.fetch_add(1, Ordering::Relaxed);
            }
            self.0.lower_evaluation(
                types, memo, body, staged, signature, expression, result, constants,
            )
        }
        fn entry_body(&self) -> hirsel_tir::BodyId {
            self.0.entry_body()
        }
    }
    with_compiler_stack(|| {
        for jobs in [1, 2, 8] {
            for selected in [false, true] {
                let source = format!(
                    "type Point{{x:u64}}\nfn make[T](s:TypeShape[T])->Result[Body[fn(T)->u64 uses {{}}],String]{{let _=s;Ok(codegen.body(\"fn(p:standalone@0/Point)->u64 uses {{}}{{let _=p;7u64}}\"))}}\nfn width[T](p:T)->u64 uses {{}}=comptime make(shape(T))\nconst A:u64=comptime if {selected} {{width(Point(x:1u64))}} else {{42u64}}\n"
                );
                let mut engine =
                    engine(&source).with_result_store(Arc::new(store::MemStore::default()));
                let generations = Arc::new(AtomicUsize::new(0));
                engine.adapters.lowering = Some(Arc::new(CountGenerators(
                    engine.adapters.lowering.take().unwrap(),
                    generations.clone(),
                )));
                let paths = engine.indexed_definitions();
                let domain = store::Domain {
                    kind: 0x4601_00f1,
                    schema: 1,
                };
                let errors = RuntimeErrors {
                    resource: domain.reference(b"resource").unwrap(),
                    cycle: domain.reference(b"cycle").unwrap(),
                    cancelled: domain.reference(b"cancelled").unwrap(),
                    protocol: domain.reference(b"protocol").unwrap(),
                    panicked: domain.reference(b"panicked").unwrap(),
                };
                let budget = hirsel_engine_admission::ProcessBudget::new(512 << 20);
                let weight = MemoryWeight {
                    retained: 128 << 10,
                    scratch: 8 << 20,
                    stack: 0,
                };
                let front = engine
                    .front_body_queries(&paths, weight, &budget, errors)
                    .unwrap();
                let lower = engine
                    .front_lower_queries(&paths, front.clone(), weight, 64, &budget, errors)
                    .unwrap();
                let mut runtime = AdmittedRuntime::new(
                    hirsel_intern::jobs::Jobs::new(jobs),
                    budget.reserve(256 << 20).unwrap(),
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
                let request = runtime.request(&[lower.evaluate_query(&path("A")).unwrap()]);
                let result = runtime.wait(request);
                assert!(
                    result.is_ok(),
                    "selected={selected} jobs={jobs}: {result:?}, {:?}",
                    result
                        .as_ref()
                        .err()
                        .and_then(|reference| lower.failure(*reference))
                );
                assert_eq!(
                    lower.settled_constant(&path("A"), &[]).unwrap().root.kind,
                    hirsel_tir::ValueKind::Constant(Constant::Integer(if selected {
                        7
                    } else {
                        42
                    }))
                );
                assert!(engine.generated_source(&path("width")).is_none());
                assert_eq!(
                    generations.load(Ordering::Relaxed) > 0,
                    selected,
                    "an unselected branch performs no generator execution",
                );
                drop(result);
                drop(runtime);
                drop(lower);
                drop(front);
                drop(engine);
                assert_eq!(
                    budget.reserved(),
                    0,
                    "completed demand owners release their grants"
                );
            }
        }
    });
}
#[test]
fn frozen_generator_instance_allowance_counts_current_selections_including_hits() {
    use hirsel_engine::FrontInstanceRequest;

    use hirsel_types::{Actuals, GenericArgument, Size};
    with_compiler_stack(|| {
        let source = "fn make[n:size]()->u64 uses {}=comptime Ok(codegen.body(\"fn()->u64 uses {}{7u64}\"))\n";
        let mut engine = engine(source);
        let revision = engine
            .sources
            .add("revision.hir", "const UNUSED:u64=1\n".into())
            .unwrap();
        engine.parse_file(revision, &default_context()).unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(64 << 20);
        let select = |engine: &mut QueryEngine, n| {
            engine.register_front_instance(
                &FrontInstanceRequest {
                    declaration: path("make"),
                    actuals: Actuals {
                        arguments: vec![GenericArgument::Size(Size::Constant(n))],
                    },
                    site: hirsel_span::Span::default(),
                },
                &budget,
            )
        };
        for n in 0..256 {
            select(&mut engine, n).unwrap();
        }
        assert!(matches!(select(&mut engine, 256), Err(failure)
            if matches!(failure.error, QueryError::Diagnostics(ref ds) if ds.iter().any(|d| d.rule.as_str() == "CT-1"))));
        let file = engine
            .sources
            .add("revision.hir", "const UNUSED:u64=2\n".into())
            .unwrap();
        engine.parse_file(file, &default_context()).unwrap();
        let retained = engine.indexed_definitions().len();
        select(&mut engine, 0).unwrap();
        assert_eq!(
            engine.indexed_definitions().len(),
            retained,
            "a selected cache hit retains its syntax"
        );
        for n in 256..511 {
            select(&mut engine, n).unwrap();
        }
        assert!(
            matches!(select(&mut engine, 511), Err(failure)
            if matches!(failure.error, QueryError::Diagnostics(ref ds) if ds.iter().any(|d| d.rule.as_str() == "CT-1"))),
            "the reused instance consumes one of this program's 256 selections"
        );
        drop(engine);
        assert_eq!(budget.reserved(), 0);
    });
}

#[test]
fn frozen_generator_acceptance_cancellation_publishes_no_registration() {
    use hirsel_engine_runtime::*;
    use std::sync::{Arc, Mutex, mpsc};
    with_compiler_stack(|| {
        let source = "type Point{x:u64}\nfn gen[T](s:TypeShape[T])->Result[Body[fn(T)->u64 uses {}],String]{let _=s;Ok(codegen.body(\"fn(v:standalone@0/Point)->u64 uses {}{let _=v;7u64}\"))}\nfn width[T](v:T)->u64 uses {}=comptime gen(shape(T))\nconst A:u64=comptime width(Point(x:1u64))\n";
        let store = Arc::new(store::MemStore::default());
        let mut engine = engine(source).with_result_store(store.clone());
        let (entered, ready) = mpsc::channel();
        let (release, released) = mpsc::channel();
        struct BlockingSignature(
            Arc<dyn SignaturePhase>,
            mpsc::Sender<()>,
            Mutex<Option<mpsc::Receiver<()>>>,
        );
        impl SignaturePhase for BlockingSignature {
            fn signature_items(
                &self,
                t: &hirsel_types::TypeInterner,
                m: &hirsel_types::TypeMemo,
                items: &[Arc<hirsel_syntax::ItemSyntax>],
                declarations: &hirsel_engine::Declarations,
                products: &mut PhaseProducts,
                provider: &mut dyn hirsel_types::SignatureProvider,
            ) -> Result<QueryOutput<hirsel_types::Signature>, QueryError> {
                self.0
                    .signature_items(t, m, items, declarations, products, provider)
            }
            fn signature(
                &self,
                t: &hirsel_types::TypeInterner,
                m: &hirsel_types::TypeMemo,
                syntax: Arc<hirsel_syntax::ItemSyntax>,
                declarations: &hirsel_engine::Declarations,
                products: &mut PhaseProducts,
                provider: &mut dyn hirsel_types::SignatureProvider,
            ) -> Result<QueryOutput<hirsel_types::Signature>, QueryError> {
                if syntax.item.definition.0.name.ends_with("$literal") {
                    self.1.send(()).unwrap();
                    self.2.lock().unwrap().take().unwrap().recv().unwrap();
                }
                self.0
                    .signature(t, m, syntax, declarations, products, provider)
            }
        }
        engine.adapters.signature = Some(Arc::new(BlockingSignature(
            engine.adapters.signature.take().unwrap(),
            entered,
            Mutex::new(Some(released)),
        )));
        let paths = engine.indexed_definitions();
        let domain = store::Domain {
            kind: 0x4601_00f2,
            schema: 1,
        };
        let errors = RuntimeErrors {
            resource: domain.reference(b"resource").unwrap(),
            cycle: domain.reference(b"cycle").unwrap(),
            cancelled: domain.reference(b"cancelled").unwrap(),
            protocol: domain.reference(b"protocol").unwrap(),
            panicked: domain.reference(b"panicked").unwrap(),
        };
        let budget = hirsel_engine_admission::ProcessBudget::new(512 << 20);
        let weight = MemoryWeight {
            retained: 128 << 10,
            scratch: 8 << 20,
            stack: 0,
        };
        let front = engine
            .front_body_queries(&paths, weight, &budget, errors)
            .unwrap();
        let lower = engine
            .front_lower_queries(&paths, front.clone(), weight, 64, &budget, errors)
            .unwrap();
        let mut runtime = AdmittedRuntime::new(
            hirsel_intern::jobs::Jobs::new(1),
            budget.reserve(256 << 20).unwrap(),
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
        let request = runtime.request(&[lower.evaluate_query(&path("A")).unwrap()]);
        ready
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap();
        runtime.cancel(request);
        release.send(()).unwrap();
        runtime.shutdown();
        assert_eq!(
            runtime.poll(request),
            std::task::Poll::Ready(Err(errors.cancelled))
        );
        assert!(
            store.take().keys().all(|key| key.2 != QueryKind::Generate),
            "cancelled private execution publishes no generator registration variant"
        );
        assert!(engine.generated_source(&path("width")).is_none());
        drop(runtime);
        drop(lower);
        drop(front);
        drop(engine);
        assert_eq!(
            budget.reserved(),
            0,
            "cancelled demand releases every grant"
        );
    });
}
