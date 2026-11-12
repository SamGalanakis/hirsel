use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use hirsel_span::{SourceMap, Span};
use hirsel_syntax::{ContentHash, DefPath, ItemPath};
use hirsel_tir::LocalId;
use hirsel_tir::*;
use hirsel_types::*;

struct Fixture(Definition);
impl TirProvider for Fixture {
    fn constant(
        &mut self,
        _path: &hirsel_syntax::DefPath,
    ) -> Result<hirsel_tir::MaterializedRef, hirsel_types::QueryError> {
        Err(hirsel_types::QueryError::NotImplemented)
    }
    fn constant_definitions(
        &mut self,
    ) -> Result<Vec<hirsel_syntax::DefPath>, hirsel_types::QueryError> {
        Ok(vec![])
    }

    fn definition(&mut self, path: &DefPath) -> Result<DefinitionRef, QueryError> {
        (|| {
            assert_eq!(path, self.0.path.path());
            Ok(self.0.clone())
        })()
        .map(std::sync::Arc::new)
    }
}
fn fixture(fail: bool) -> (Fixture, SourceMap, RunConfiguration) {
    let context = default_context();
    let path = DefPath(ItemPath {
        package: context.package,
        modules: vec![],
        name: "main".into(),
    });
    let mut sources = SourceMap::default();
    let file = sources.add("runtime.hir", "\n\nfixture\n".into()).unwrap();
    let span = Span::new(sources.file_anchor(file), 2, 9).unwrap();
    let types = hirsel_registry::session_types();
    let unit = types.intern(Type {
        drop_allocation: hirsel_types::DropAllocation::MayAllocate,
        ..Type::with_kind(TypeKind::Tuple(vec![].into()))
    });
    let body = Body {
        arena_scope_grant: None,
        scoped_parameters: Vec::new(),
        parameter_scope: hirsel_types::LexicalOwner::first(&path)
            .site(hirsel_types::NameSite::Item),
        locals: vec![Local {
            ty: unit,
            storage: Storage::Temporary,
            scope: ScopeId(0),
            span,
        }],
        places: vec![Place {
            loan: None,
            binding: None,
            root: LocalId(0),
            projection: vec![].into(),
            ty: unit,
        }],
        scopes: vec![Scope { parent: None, span }],
        blocks: vec![Block {
            instructions: vec![Instruction {
                kind: InstructionKind::Assign {
                    destination: PlaceId(0),
                    value: RvalueId(0),
                },
                span,
                on_failure: CleanupId(0),
            }],
            terminator: if fail {
                Terminator::Fail {
                    rule: const { hirsel_diag::RuleId::of("FAIL-1") },
                    cleanup: CleanupId(0),
                }
            } else {
                Terminator::Return {
                    value: Operand {
                        place: PlaceId(0),
                        usage: UseKind::Take,
                    },
                    cleanup: CleanupId(0),
                }
            },
            span,
        }],
        rvalues: vec![Rvalue::Constant(Constant::Unit)],
        cleanup_nodes: vec![],
        cleanups: vec![Cleanup::default()],
        entry: BlockId(0),
        parameters: vec![],
        result: unit,
        borrow_cursors: vec![],
        tail_sites: vec![],
        matches: vec![],
    };
    let mut definition = Definition {
        caller_bodies: vec![],
        protocol_associations: vec![],
        types: std::sync::Arc::from([]),
        path: hirsel_syntax::DefId::intern(&path),
        signature_hash: ContentHash::default(),
        bodies: vec![body],
        entry: BodyId(0),
        contracts: vec![],
    };
    definition.types = hirsel_tir::definition_types(&types, &definition);
    (
        Fixture(definition),
        sources,
        RunConfiguration {
            entry: path,
            hash_seed: 123,
            inputs: vec![],
        },
    )
}

#[test]
fn strict_and_mixed_replacement_failures_keep_the_new_value_in_named_cleanup_order() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        for source in [
            include_str!("../../../../conformance/DROP/DROP-2-moved-storage-failure-strict.hir"),
            include_str!("../../../../conformance/DROP/DROP-2-moved-storage-failure-mixed.hir"),
        ] {
            let expected_line = source
                .lines()
                .find_map(|line| line.strip_prefix("// expect: fail SEM-1 at line "))
                .expect("fixture must expect SEM-1 failure")
                .parse::<usize>()
                .unwrap();
            let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
            let file = engine
                .sources
                .add("replace-failure.hir", source.into())
                .unwrap();
            let checked = source_check_request(&mut engine, file, &default_context()).unwrap();
            assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
            let mut stdout = vec![];
            let result = run_snapshot(
                &mut engine,
                &RunConfiguration {
                    entry: entry_path(&default_context()),
                    hash_seed: 0,
                    inputs: vec![],
                },
                SimulationConfig::default(),
                &mut stdout,
            )
            .unwrap();
            assert!(
                matches!(result, RunOutcome::Failed { primary: hirsel_run_schema::Failure { ref rule, ref site, .. }, .. } if rule.as_str()=="SEM-1" && site.line == expected_line as u64),
                "{result:?}"
            );
            assert_eq!(stdout, b"old\nlater\nnew\n");
        }
    });
}

#[test]
fn all_four_projection_controls_propagate_not_granted_without_dummy_capabilities() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        for (operation, source) in [
            ("stdout", include_str!("../fixtures/grants/stdout.hir")),
            ("tasks", include_str!("../fixtures/grants/tasks.hir")),
            ("net", include_str!("../fixtures/grants/net.hir")),
            ("random", include_str!("../fixtures/grants/random.hir")),
        ] {
            for granted in [false, true] {
                let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
                let file = engine.sources.add("grant.hir", source.into()).unwrap();
                let checked = source_check_request(&mut engine, file, &default_context()).unwrap();
                assert!(
                    checked.diagnostics.is_empty(),
                    "{operation}: {:?}",
                    checked.diagnostics
                );
                assert_eq!(checked.completion, CheckCompletion::Complete);
                let config = SimulationConfig {
                    projections: if granted {
                        [operation.to_owned()].into()
                    } else {
                        Default::default()
                    },
                    ..Default::default()
                };
                let mut stdout = vec![];
                let result = run_snapshot(
                    &mut engine,
                    &RunConfiguration {
                        entry: entry_path(&default_context()),
                        hash_seed: 0,
                        inputs: vec![],
                    },
                    config,
                    &mut stdout,
                )
                .unwrap();
                assert_eq!(
                    result,
                    RunOutcome::Returned(if granted {
                        hirsel_run_schema::Returned::Ok
                    } else {
                        hirsel_run_schema::Returned::Error
                    }),
                    "{operation}"
                );
                assert!(stdout.is_empty());
            }
        }
    });
}
