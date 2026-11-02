//! Owner bodies validate against the private signature input of their own package.
use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_span::Span;
use hirsel_tir::*;
use hirsel_types::{Destination, Primitive, SignatureKind, Type, TypeKind, UseKind};
use hirsel_validate::SignatureMap;

#[test]
fn hand_built_opaque_construction_needs_the_owner_input_and_checks_fields() {
    with_compiler_stack(|| {
        let context = default_context();
        let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
        let file = engine.sources.add("owner.hir", "pub opaque type Hidden: plain uses {} { value: u64 }\nfn build() -> Hidden { Hidden(value: 1) }\n".into()).unwrap();
        engine.parse_file(file, &context).unwrap();
        let mut hidden = entry_path(&context);
        hidden.0.name = "Hidden".into();
        let mut build = hidden.clone();
        build.0.name = "build".into();
        let public = engine.item_signature(&hidden).unwrap().value;
        assert!(matches!(&public.kind, SignatureKind::Type(t) if t.representation.is_none()));
        let snapshot = engine.signature_snapshot();
        let function = engine.item_signature(&build).unwrap().value;
        let types = engine.types().clone();
        let mut row = Type::unit();
        row.kind = TypeKind::Primitive(Primitive::U64);
        let number = types.intern(row);
        let SignatureKind::Function(header) = &function.kind else {
            unreachable!()
        };
        let nominal = header.result;
        let span = Span::default();
        let instruction = |kind| Instruction {
            kind,
            span,
            on_failure: CleanupId(0),
        };
        let take = |place| Operand {
            place: PlaceId(place),
            usage: UseKind::Take,
        };
        let mut definition = Definition {
            caller_bodies: vec![],
            protocol_associations: vec![],
            path: hirsel_syntax::DefId::intern(&build),
            inferred_unique: function.metadata.inferred_unique.clone(),
            signature_hash: function.fingerprint,
            types: std::sync::Arc::from([]),
            entry: BodyId(0),
            contracts: vec![],
            bodies: vec![Body {
                arena_scope_grant: None,
                scoped_parameters: Vec::new(),
                parameter_scope: hirsel_types::LexicalOwner::first(&build)
                    .site(hirsel_types::NameSite::Item),
                locals: vec![
                    Local {
                        ty: number,
                        storage: Storage::Temporary,
                        scope: ScopeId(0),
                        span,
                    },
                    Local {
                        ty: nominal,
                        storage: Storage::ReturnSlot,
                        scope: ScopeId(0),
                        span,
                    },
                ],
                places: [number, nominal]
                    .into_iter()
                    .enumerate()
                    .map(|(i, ty)| Place {
                        loan: None,
                        binding: None,
                        root: LocalId(i as u32),
                        projection: Default::default(),
                        ty,
                    })
                    .collect(),
                scopes: vec![Scope { parent: None, span }],
                blocks: vec![Block {
                    instructions: vec![
                        instruction(InstructionKind::StorageLive(LocalId(0))),
                        instruction(InstructionKind::Assign {
                            destination: PlaceId(0),
                            value: RvalueId(0),
                        }),
                        instruction(InstructionKind::Assign {
                            destination: PlaceId(1),
                            value: RvalueId(1),
                        }),
                    ],
                    terminator: Terminator::Return {
                        value: take(1),
                        cleanup: CleanupId(0),
                    },
                    span,
                }],
                rvalues: vec![
                    Rvalue::Constant(Constant::Integer(1)),
                    Rvalue::Construct {
                        constructor: Constructor::Record(hidden.clone()),
                        fields: vec![ConstructField {
                            field: 0,
                            value: take(0),
                        }],
                        destination: Destination::Heap,
                    },
                ],
                cleanup_nodes: vec![],
                cleanups: vec![Cleanup::default()],
                entry: BlockId(0),
                parameters: vec![],
                result: nominal,
                borrow_cursors: vec![],
                tail_sites: vec![],
                matches: vec![],
            }],
        };
        definition.types = hirsel_tir::definition_types(&types, &definition);
        // This program fails with the previous public-only transport.
        let mut public_input = SignatureMap::new(types.clone());
        public_input.insert(&hidden, (*public).clone());
        public_input.insert(function.definition, (*function).clone());
        definition.types = hirsel_tir::definition_types(engine.types(), &definition);
        assert!(
            !hirsel_validate::validate(&definition, engine.types(), &public_input, &mut vec![])
                .unwrap()
                .violations
                .is_empty()
        );
        let owner_input = snapshot.validation(&context.package).unwrap();
        definition.types = hirsel_tir::definition_types(engine.types(), &definition);
        let report =
            hirsel_validate::validate(&definition, engine.types(), owner_input, &mut vec![])
                .unwrap();
        assert!(report.violations.is_empty(), "{:?}", report.violations);
        let foreign = hirsel_syntax::PackageId {
            name: context.package.name.clone(),
            version: "1".into(),
        };
        definition.types = hirsel_tir::definition_types(engine.types(), &definition);
        assert!(
            !hirsel_validate::validate(
                &definition,
                engine.types(),
                snapshot.validation(&foreign).unwrap(),
                &mut vec![]
            )
            .unwrap()
            .violations
            .is_empty()
        );
        let mut forged = definition;
        let Rvalue::Construct { fields, .. } = &mut forged.bodies[0].rvalues[1] else {
            unreachable!()
        };
        fields[0].field = 1;
        forged.types = hirsel_tir::definition_types(engine.types(), &forged);
        assert!(
            !hirsel_validate::validate(&forged, engine.types(), owner_input, &mut vec![])
                .unwrap()
                .violations
                .is_empty()
        );
        assert_eq!(public, engine.item_signature(&hidden).unwrap().value);
    });
}

/// The frozen signature universe changes only with its inputs; ordinary body
/// invalidation must retain its Arc and all the per-view V verdicts.
#[test]
fn snapshot_reuses_body_edits_and_tracks_signature_inputs() {
    with_compiler_stack(|| {
        use std::sync::Arc;
        let context = default_context();
        let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
        let parse = |engine: &mut QueryEngine, source: &str| {
            let file = engine
                .sources
                .add("snapshot-edits.hir", source.into())
                .unwrap();
            engine.parse_file(file, &context).unwrap();
        };
        parse(&mut engine, "fn main() -> u64 { 1 }\n");
        let first = engine.signature_snapshot();
        parse(&mut engine, "fn main() -> u64 { 2 }\n");
        let body = engine.signature_snapshot();
        assert!(Arc::ptr_eq(&first, &body));
        assert_eq!(
            engine.recompute_count(&entry_path(&context), QueryKind::Signature),
            1
        );

        parse(&mut engine, "fn main() -> u8 { 2u8 }\n");
        let header = engine.signature_snapshot();
        assert!(!Arc::ptr_eq(&body, &header));
        assert_ne!(
            body.signature(hirsel_syntax::DefId::intern(&entry_path(&context))),
            header.signature(hirsel_syntax::DefId::intern(&entry_path(&context)))
        );

        // The default may be unlimited. Still select a distinct finite input
        // rather than overflowing while testing snapshot invalidation.
        engine.comptime_limits.call_depth = engine
            .comptime_limits
            .call_depth
            .checked_add(1)
            .unwrap_or(32);
        let limits = engine.signature_snapshot();
        assert!(!Arc::ptr_eq(&header, &limits));
        engine.module_inputs_mut().manifest_revision =
            hirsel_syntax::ContentHash::of_bytes(b"new manifest");
        let manifest = engine.signature_snapshot();
        assert!(!Arc::ptr_eq(&limits, &manifest));
        assert!(Arc::ptr_eq(&manifest, &engine.signature_snapshot()));

        // A new later candidate preserves the first header, but affects its
        // duplicate diagnostic and the declaration-candidate read.
        parse(
            &mut engine,
            "fn main() -> u8 { 2u8 }\nfn main() -> u8 { 3u8 }\n",
        );
        let duplicate = engine.signature_snapshot();
        assert!(!Arc::ptr_eq(&manifest, &duplicate));
        assert!(
            duplicate
                .signature(hirsel_syntax::DefId::intern(&entry_path(&context)))
                .unwrap()
                .is_err()
        );
    });
}

#[test]
fn snapshot_private_shape_edits_change_the_view_without_a_public_fingerprint_change() {
    with_compiler_stack(|| {
        use std::sync::Arc;
        let context = default_context();
        let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
        let mut hidden = entry_path(&context);
        hidden.0.name = "Hidden".into();
        let hidden = hirsel_syntax::DefId::intern(&hidden);
        let mut previous = None;
        for field in ["u64", "u8"] {
            let file = engine
                .sources
                .add(
                    "opaque-edits.hir",
                    format!("pub opaque type Hidden: plain uses {{}} {{ value: {field} }}\n"),
                )
                .unwrap();
            engine.parse_file(file, &context).unwrap();
            let snapshot = engine.signature_snapshot();
            if let Some(old) = previous {
                let old: Arc<hirsel_engine::SignatureSnapshot> = old;
                assert!(!Arc::ptr_eq(&old, &snapshot));
                assert_eq!(
                    old.signature(hidden).unwrap().unwrap().fingerprint,
                    snapshot.signature(hidden).unwrap().unwrap().fingerprint
                );
                assert_ne!(
                    old.representation(hidden).unwrap().unwrap().1,
                    snapshot.representation(hidden).unwrap().unwrap().1
                );
            }
            previous = Some(snapshot);
        }
    });
}

#[test]
fn snapshot_retires_staged_signatures_when_only_the_owner_body_changes() {
    with_compiler_stack(|| {
        use std::sync::Arc;
        let context = default_context();
        let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
        let mut owner = entry_path(&context);
        owner.0.name = "value".into();
        let source = "fn value[n: size]() -> u64 { comptime (n + 1) }\n";
        let file = engine
            .sources
            .add("staged-edits.hir", source.into())
            .unwrap();
        engine.parse_file(file, &context).unwrap();
        let analyzed = engine.mode_definition(&owner).unwrap().value;
        let expression = analyzed
            .typed
            .checked
            .expressions
            .iter()
            .find(|row| {
                matches!(
                    analyzed.typed.syntax.tree.expressions[row.expression.0 as usize].kind,
                    hirsel_syntax::ExprKind::Comptime(_)
                )
            })
            .unwrap()
            .expression;
        let staged =
            hirsel_check_protocol::body::comptime_path(&owner, &analyzed.typed.checked, expression);
        let staged = hirsel_syntax::DefId::intern(&staged);
        let initial = engine.signature_snapshot();
        assert!(initial.signature(staged).is_none());
        engine.lower_to_tir(&owner).unwrap();
        let published = engine.signature_snapshot();
        assert!(!Arc::ptr_eq(&initial, &published));
        assert!(published.signature(staged).unwrap().is_ok());

        let file = engine
            .sources
            .add(
                "staged-edits.hir",
                "fn value[n: size]() -> u64 { n + 1 }\n".into(),
            )
            .unwrap();
        engine.parse_file(file, &context).unwrap();
        let retired = engine.signature_snapshot();
        assert!(!Arc::ptr_eq(&published, &retired));
        assert!(retired.signature(staged).is_none());
        assert!(published.signature(staged).unwrap().is_ok());
    });
}

#[test]
fn live_and_frozen_bodies_record_the_same_private_view_receipt() {
    with_compiler_stack(|| {
        let context = default_context();
        let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
        let mut hidden = entry_path(&context);
        hidden.0.name = "Hidden".into();
        let mut build = hidden.clone();
        build.0.name = "build".into();
        let hidden = hirsel_syntax::DefId::intern(&hidden);
        let mut previous = None;
        for field in ["u64", "u8"] {
            let source = format!(
                "pub opaque type Hidden: plain uses {{}} {{ value: {field} }}\nfn build() -> Hidden {{ Hidden(value: 1) }}\n"
            );
            let file = engine.sources.add("private-receipts.hir", source).unwrap();
            engine.parse_file(file, &context).unwrap();
            engine.check_definition(&build).unwrap();
            let serial = engine.memo_reads(&build, QueryKind::Check).unwrap().clone();
            let snapshot = engine.signature_snapshot();
            let mut frozen = ReadsBuilder::new();
            let mut provider =
                FrozenProvider::for_body(&snapshot, context.package.clone(), &mut frozen);
            let view = provider.signature(hidden).unwrap();
            assert!(matches!(&view.kind, SignatureKind::Type(t) if t.representation.is_some()));
            let frozen = frozen.finish().unwrap();
            for (key, value) in frozen.iter() {
                assert!(
                    serial
                        .iter()
                        .any(|(read, observed)| read == key && observed == value),
                    "{key:?}"
                );
            }
            let digest = frozen
                .iter()
                .find_map(|(key, value)| {
                    (*key == ReadKey::Representation(hidden)).then_some(*value)
                })
                .unwrap();
            assert_eq!(
                digest,
                snapshot.representation(hidden).unwrap().unwrap().1.0
            );
            if let Some(old) = previous {
                assert_ne!(old, digest);
            }
            previous = Some(digest);
        }
        assert_eq!(engine.recompute_count(&build, QueryKind::Check), 2);
    });
}

/// A source edit's private-view subscriptions and warm reads survive real
/// concurrent phases, rather than merely exercising the provider in isolation.
#[test]
fn frozen_frontier_matches_serial_private_edits_and_tir() {
    with_compiler_stack(|| {
        use hirsel_intern::jobs::Jobs;
        for jobs in [1, 2, 8] {
            let mut serial = QueryEngine::new(default_environment(), compiler_adapters());
            let mut parallel = QueryEngine::new(default_environment(), compiler_adapters());
            let context = default_context();
            let paths: Vec<_> = ["read", "build", "main"]
                .into_iter()
                .map(|name| {
                    let mut path = entry_path(&context);
                    path.0.name = name.into();
                    path
                })
                .collect();
            for (field, value) in [("u64", 1), ("u8", 1), ("u8", 2)] {
                let text = format!(
                    "pub opaque type Hidden: plain uses {{}} {{ value: {field} }}\nfn build() -> Hidden {{ Hidden(value: {value}) }}\nfn read(x: Hidden) -> {field} {{ x.value }}\nfn main() -> {field} {{ read(build()) }}\n"
                );
                for engine in [&mut serial, &mut parallel] {
                    let file = engine
                        .sources
                        .add("parallel-private.hir", text.clone())
                        .unwrap();
                    engine.parse_file(file, &context).unwrap();
                }
                let expected: Vec<_> = paths
                    .iter()
                    .map(|path| serial.mode_definition(path))
                    .collect();
                let mut graph = crate::request_graph::Graph::new(&mut parallel, Jobs::new(jobs));
                let actual = graph.bodies(&paths);
                for ((path, expected), actual) in paths.iter().zip(expected).zip(actual) {
                    expected.unwrap();
                    actual.result.unwrap();
                    for kind in [QueryKind::Resolve, QueryKind::Check, QueryKind::Modes] {
                        assert_eq!(
                            serial.memo_reads(path, kind),
                            graph
                                .installed
                                .front
                                .memo_snapshot()
                                .into_iter()
                                .find_map(|(key, reads)| (key
                                    == hirsel_engine::memo::MemoKey::new(
                                        kind,
                                        DefId::intern(path)
                                    ))
                                .then_some(reads))
                                .as_deref(),
                            "{jobs}/{field}/{value}/{path:?}/{kind:?}"
                        );
                    }
                    let a = serial.definition_summary(DefId::intern(path)).unwrap();
                    let b = actual.summary.unwrap();
                    assert_eq!(a.summary.frames, b.summary.frames);
                    assert_eq!(a.summary.calls.len(), b.summary.calls.len());
                    for (a, b) in a.summary.calls.iter().zip(&b.summary.calls) {
                        assert_eq!(
                            (a.callee, a.binder, a.class, a.owning_frame, a.target_frame),
                            (b.callee, b.binder, b.class, b.owning_frame, b.target_frame)
                        );
                        let a = serial.sources.resolve(a.site);
                        let b = parallel.sources.resolve(b.site);
                        assert_eq!((a.start, a.end), (b.start, b.end));
                    }
                    assert_eq!(a.growth.edges.len(), b.growth.edges.len());
                    assert_eq!(
                        serial.export_tir(path).unwrap(),
                        parallel.export_tir(path).unwrap()
                    );
                }
                graph.finish();
            }
        }
    });
}

#[test]
fn frozen_frontier_diagnostics_and_tir_are_identical_at_three_salts() {
    fn run() -> String {
        with_compiler_stack(|| {
            use hirsel_intern::jobs::Jobs;
            fn transcript(engine: &mut QueryEngine, results: Vec<ModeResult>) -> String {
                let mut text = String::new();
                let accepted = results.iter().all(|result| result.result.is_ok());
                for result in results {
                    text.push_str(&format!("{:?} {:?}\n", result.definition, result.phase));
                    let diagnostics = match result.result {
                        Ok(output) => {
                            if accepted {
                                let tir = engine.export_tir(&result.definition).unwrap();
                                text.push_str(&tir);
                            }
                            output.diagnostics.clone()
                        }
                        Err(QueryError::Diagnostics(ds)) => ds,
                        Err(error) => panic!("unexpected operational failure {error:?}"),
                    };
                    for diagnostic in diagnostics {
                        text.push_str(&diagnostic.to_json(&engine.sources).to_string());
                        text.push('\n');
                    }
                }
                text
            }
            let programs = [
                "fn id[T](take v: T) -> T { v }\nfn left() -> u64 { id(1u64) }\nfn right() -> u8 { id(2u8) }\n",
                "pub opaque type Hidden: plain uses {} { value: u64 }\nfn make() -> Hidden { Hidden(value: 1) }\nfn read(v: Hidden) -> u64 { v.value }\n",
                "fn bad() -> u64 { absent }\nfn good() -> u64 { 1 }\n",
            ];
            let mut all = String::new();
            for source in programs {
                let mut serial = QueryEngine::new(default_environment(), compiler_adapters());
                let file = serial
                    .sources
                    .add("frozen-oracle.hir", source.into())
                    .unwrap();
                let parsed = serial.parse_file(file, &default_context()).unwrap();
                let paths: Vec<_> = parsed
                    .value
                    .items
                    .iter()
                    .filter(|s| matches!(s.item.kind, hirsel_syntax::ItemKind::Function { .. }))
                    .map(|s| s.item.definition.clone())
                    .rev()
                    .collect();
                let expected: Vec<_> = paths
                    .iter()
                    .map(|path| {
                        serial.last_phase = None;
                        serial.unavailable = None;
                        let result = serial.mode_definition(path).map(std::sync::Arc::new);
                        ModeResult {
                            summary: None,
                            reads: std::sync::Arc::new(Reads::empty()),
                            diagnostic_phases: vec![],
                            definition: path.clone(),
                            phase: serial
                                .unavailable
                                .or(serial.last_phase)
                                .or(Some(QueryKind::Modes)),
                            result,
                        }
                    })
                    .collect();
                let expected = transcript(&mut serial, expected);
                for jobs in [1, 2, 8] {
                    let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
                    let file = engine
                        .sources
                        .add("frozen-oracle.hir", source.into())
                        .unwrap();
                    engine.parse_file(file, &default_context()).unwrap();
                    for _ in 0..2 {
                        let results = crate::request_graph::front(
                            &mut engine,
                            paths.clone(),
                            Jobs::new(jobs),
                        );
                        assert_eq!(
                            transcript(&mut engine, results),
                            expected,
                            "jobs {jobs}: {source}"
                        );
                    }
                }
                all.push_str(&expected);
            }
            all
        })
    }
    let Some(outputs) = hirsel_intern::under_salts(
        "signature_input::frozen_frontier_diagnostics_and_tir_are_identical_at_three_salts",
        &[1, 2, 3],
        run,
    ) else {
        return;
    };
    assert!(outputs[0].contains("(Export\n"));
    assert!(outputs[0].contains("\"severity\":\"error\""));
    assert!(outputs.iter().all(|output| output == &outputs[0]));
}

#[test]
fn frozen_frontier_keeps_serial_generation_between_source_frontiers() {
    with_compiler_stack(|| {
        use hirsel_driver::requests::{CompileOperation, CompileResult, CompilerRequests};
        use hirsel_engine::memo::MemoKey;
        let source = "fn make() -> Result[Body[fn() -> u64 uses {}], String] uses {} { Ok(codegen.body(\"fn() -> u64 uses {} { 7 }\")) }\nfn first() -> u64 uses {} { 1 }\nfn generated() -> u64 uses {} = comptime make()\nfn last() -> u64 uses {} { generated() }\n";
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("parallel-generated.hir");
        std::fs::write(&source_path, source).unwrap();
        let manifest = directory.path().join("hirsel.json");
        let manifest_base: serde_json::Value = serde_json::from_str(
            r#"{"schema":1,"package":{"name":"standalone","version":"0"},"sources":[{"path":"parallel-generated.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"last"}}"#,
        )
        .unwrap();
        std::fs::write(&manifest, manifest_base.to_string()).unwrap();
        for jobs in [1, 2, 8] {
            let paths: Vec<_> = ["first", "generated", "last"]
                .into_iter()
                .map(|name| {
                    let mut p = entry_path(&default_context());
                    p.0.name = name.into();
                    p
                })
                .collect();
            let mut scalar = QueryEngine::new(default_environment(), compiler_adapters());
            let file = scalar.sources.add(&source_path, source.into()).unwrap();
            scalar.parse_file(file, &default_context()).unwrap();
            for p in &paths {
                scalar.mode_definition(p).unwrap();
            }
            let config = CompileConfig {
                jobs: hirsel_intern::jobs::Jobs::new(jobs),
                verify: Verify::On,
                retention: memo::Retention::Resident,
                ..CompileConfig::default()
            };
            let mut requests =
                CompilerRequests::for_host(COMPILER.clone().with_config(config.clone()), config);
            let checked = requests
                .execute(
                    &manifest,
                    CompileOperation::Check {
                        lowering: LoweringPolicy::Comptime,
                    },
                )
                .unwrap();
            let CompileResult::Check(checked) = &checked.result else {
                panic!("expected Check");
            };
            assert!(checked.diagnostics.is_empty());
            assert_eq!(checked.completion, CheckCompletion::Complete);
            let inspected = requests
                .execute(
                    &manifest,
                    CompileOperation::Inspect {
                        definitions: paths.clone(),
                    },
                )
                .unwrap();
            let CompileResult::Inspect(inspection) = &inspected.result else {
                panic!("expected Inspect");
            };
            for p in &paths {
                let key = MemoKey::new(QueryKind::Modes, DefId::intern(p));
                let reads = inspection
                    .memos
                    .iter()
                    .find_map(|(memo, reads)| (*memo == key).then(|| reads.clone()));
                assert_eq!(scalar.memo_reads(p, QueryKind::Modes), reads.as_deref());
                let mut selected = manifest_base.clone();
                selected["entry"]["name"] = p.0.name.clone().into();
                std::fs::write(&manifest, selected.to_string()).unwrap();
                let exported = requests
                    .execute(&manifest, CompileOperation::ExportTir)
                    .unwrap();
                let CompileResult::ExportTir(tir) = &exported.result else {
                    panic!("expected ExportTir");
                };
                assert_eq!(tir, &scalar.export_tir(p).unwrap());
            }
        }
    });
}

#[test]
fn validation_universe_receipts_distinguish_valid_changes_and_originating_private_packages() {
    with_compiler_stack(|| {
        use hirsel_engine::memo::PackageKey;
        use hirsel_validate::SignatureLookup;
        let context = default_context();
        let foreign = hirsel_syntax::PackageId {
            name: "foreign".into(),
            version: "1".into(),
        };
        let base = "pub opaque type Hidden: plain uses {} { value: u8 }\nfn f() -> u8 { 1u8 }\n";
        let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
        let file = engine.sources.add("universe.hir", base.into()).unwrap();
        engine.parse_file(file, &context).unwrap();
        let initial = engine.signature_snapshot();
        let initial_errors = initial
            .validation(&context.package)
            .unwrap()
            .universe_errors()
            .0;
        let mut reads = ReadsBuilder::new();
        let own = FrozenProvider::for_body(&initial, context.package.clone(), &mut reads)
            .validation_universe()
            .unwrap();
        let foreign_before = FrozenProvider::for_body(&initial, foreign.clone(), &mut reads)
            .validation_universe()
            .unwrap();
        let reads = reads.finish().unwrap();
        assert_eq!(
            reads.get(&ReadKey::ValidationUniverse(PackageKey::of(
                &context.package
            ))),
            Some(own)
        );
        assert_eq!(
            reads.get(&ReadKey::ValidationUniverse(PackageKey::of(&foreign))),
            Some(foreign_before)
        );
        assert_ne!(
            own, foreign_before,
            "private views have an explicit originating package"
        );
        let body = base.replace("{ 1u8 }", "{ 2u8 }");
        let file = engine.sources.add("universe.hir", body.clone()).unwrap();
        engine.parse_file(file, &context).unwrap();
        let body_snapshot = engine.signature_snapshot();
        assert_eq!(
            body_snapshot
                .validation_input_digest(&context.package)
                .unwrap(),
            own
        );
        let private = body.replace("value: u8", "value: u16");
        let file = engine.sources.add("universe.hir", private.clone()).unwrap();
        engine.parse_file(file, &context).unwrap();
        let private_snapshot = engine.signature_snapshot();
        assert_eq!(
            private_snapshot
                .validation(&context.package)
                .unwrap()
                .universe_errors()
                .0,
            initial_errors
        );
        assert_ne!(
            private_snapshot
                .validation_input_digest(&context.package)
                .unwrap(),
            own
        );
        assert_eq!(
            private_snapshot.validation_input_digest(&foreign).unwrap(),
            foreign_before
        );
        let header = private.replace("fn f() -> u8 { 2u8 }", "fn f() -> u16 { 2u16 }");
        let file = engine.sources.add("universe.hir", header).unwrap();
        engine.parse_file(file, &context).unwrap();
        let changed = engine.signature_snapshot();
        assert_eq!(
            changed
                .validation(&context.package)
                .unwrap()
                .universe_errors()
                .0,
            initial_errors
        );
        assert_ne!(
            changed.validation_input_digest(&foreign).unwrap(),
            foreign_before,
            "valid changed rows change the input receipt even with the same empty error digest"
        );
    });
}

#[test]
fn installed_std_publishes_the_universe_summary_its_full_table_computes() {
    with_compiler_stack(|| {
        use hirsel_driver::requests::{CompileOperation, CompileResult, CompilerRequests};
        use hirsel_validate::{SignatureLookup, ValidatedSignatures};
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../stdlib/std@1/hirsel.json")
            .canonicalize()
            .unwrap();
        let config = CompileConfig {
            store: Arc::new(store::DirStore::new(dir.path().join("products"))),
            jobs: hirsel_intern::jobs::Jobs::ONE,
            hold_bytes: 0,
            ..Default::default()
        };
        let mut requests = CompilerRequests::for_host(COMPILER.with_config(config.clone()), config);
        let output = requests
            .execute(&manifest, CompileOperation::Precompute)
            .unwrap();
        let CompileResult::Precomputed { sources, .. } = &output.result else {
            panic!("expected precomputed std")
        };
        let std = hirsel_syntax::PackageId {
            name: "std".into(),
            version: "1".into(),
        };
        let published = sources.universe(&std).expect("std publishes its universe");
        // Oracle: V over the whole table as std's bodies see it, every view
        // identified afresh, with no previous universe or verdicts to reuse.
        let snapshot = sources.signatures();
        let types = snapshot.types();
        let table = ViewTable::from_views(
            types,
            snapshot.definitions().into_iter().filter_map(|d| {
                let signature = snapshot.body_view(&std, d)?.ok()?;
                Some(View::new(types, signature, None))
            }),
            &mut ValidatedSignatures::default(),
            None,
        )
        .unwrap();
        assert!(
            table.views().iter().any(|v| v.d.path().0.package == std),
            "the full table holds std's rows"
        );
        let (errors, violations) = table.universe_errors();
        assert_eq!(published.input, table.input_digest());
        assert_eq!(published.errors.0, errors);
        assert_eq!(&*published.errors.1, violations);
        assert_eq!(published.first_heap, table.first_heap());
        assert!(published.first_heap.is_some(), "std declares the heap");
    });
}

#[test]
fn hello_on_installed_std_answers_v_universe_questions_without_reading_std_rows() {
    with_compiler_stack(|| {
        use hirsel_driver::requests::{CompileOperation, CompilerRequests};
        use hirsel_validate::{SignatureLookup, ValidatedSignatures};
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../stdlib/std@1/hirsel.json")
            .canonicalize()
            .unwrap();
        let products = dir.path().join("products");
        let config = CompileConfig {
            store: Arc::new(store::DirStore::new(products.clone())),
            jobs: hirsel_intern::jobs::Jobs::ONE,
            hold_bytes: 0,
            ..Default::default()
        };
        CompilerRequests::for_host(COMPILER.with_config(config.clone()), config)
            .execute(&manifest, CompileOperation::Precompute)
            .unwrap();
        let path = dir.path().join("main.hir");
        std::fs::write(&path, "pub fn main()->u64{7u64}\n").unwrap();
        let mut program = COMPILER
            .with_config(CompileConfig {
                store: Arc::new(
                    store::DirStore::new(dir.path().join("reader")).with_read_only_root(products),
                ),
                jobs: hirsel_intern::jobs::Jobs::ONE,
                hold_bytes: 0,
                ..Default::default()
            })
            .load_program(&path)
            .unwrap();
        let hello = program.entry.0.package.clone();
        let std = hirsel_syntax::PackageId {
            name: "std".into(),
            version: "1".into(),
        };
        let snapshot = program.engine.signature_snapshot();
        let std_row = |name: &str| {
            let mut path = hirsel_syntax::DefPath::core(name);
            path.0.package = std.clone();
            path.0.modules = vec!["aes_gcm".into()];
            hirsel_syntax::DefId::intern(&path)
        };
        // Hello names nothing of std's AES-GCM: V's universe questions about
        // hello and about installed std read neither of these rows.
        let (unused, named) = (std_row("seal"), std_row("open"));
        assert!(snapshot.definitions().contains(&unused));
        let hello_heap = snapshot.validation(&hello).unwrap().first_heap();
        let hello_errors = snapshot.validation(&hello).unwrap().universe_errors().0;
        let std_heap = snapshot.validation(&std).unwrap().first_heap();
        let std_errors = snapshot.validation(&std).unwrap().universe_errors().0;
        let std_input = snapshot.validation_input_digest(&std).unwrap();
        assert!(!snapshot.header_ready(unused), "V read an unused std row");
        assert!(!snapshot.header_ready(named), "V read an unused std row");
        // A view read demands that row, and not its unused sibling.
        assert!(snapshot.validation(&hello).unwrap().view(named).is_some());
        assert!(snapshot.header_ready(named));
        assert!(!snapshot.header_ready(unused));
        // Oracle: V over each full table, every view identified afresh.
        let types = snapshot.types();
        let full = |package: &hirsel_syntax::PackageId,
                    admits: &dyn Fn(&hirsel_syntax::PackageId) -> bool| {
            ViewTable::from_views(
                types,
                snapshot
                    .definitions()
                    .into_iter()
                    .filter(|d| admits(&d.path().0.package))
                    .filter_map(|d| {
                        let signature = snapshot.body_view(package, d)?.ok()?;
                        Some(View::new(types, signature, None))
                    }),
                &mut ValidatedSignatures::default(),
                None,
            )
            .unwrap()
        };
        let everything = full(&hello, &|_| true);
        assert_eq!(hello_heap, everything.first_heap());
        assert_eq!(hello_errors, everything.universe_errors().0);
        let std_table = full(&std, &|package| *package != hello);
        assert_eq!(std_heap, std_table.first_heap());
        assert!(std_heap.is_some(), "std declares the heap");
        assert_eq!(std_errors, std_table.universe_errors().0);
        assert_eq!(std_input, std_table.input_digest());
    });
}

#[test]
fn dependency_universe_summary_without_local_verifier_is_ignored() {
    with_compiler_stack(|| {
        use hirsel_driver::requests::{CompileOperation, CompileResult, CompilerRequests};
        use hirsel_engine::store::ResultStore;
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../stdlib/std@1/hirsel.json")
            .canonicalize()
            .unwrap();
        let products = Arc::new(store::MemStore::default());
        let config = CompileConfig {
            store: products.clone(),
            jobs: hirsel_intern::jobs::Jobs::ONE,
            hold_bytes: 0,
            ..Default::default()
        };
        let mut requests = CompilerRequests::for_host(COMPILER.with_config(config.clone()), config);
        let output = requests
            .execute(&manifest, CompileOperation::Precompute)
            .unwrap();
        let CompileResult::Precomputed { sources, .. } = &output.result else {
            panic!("expected precomputed std")
        };
        let std = hirsel_syntax::PackageId {
            name: "std".into(),
            version: "1".into(),
        };
        let published = sources.universe(&std).expect("std publishes its universe");
        let expected_heap = published.first_heap.clone();
        assert!(expected_heap.is_some(), "std declares the heap");
        let mut forged = published.clone();
        forged.first_heap = None;
        let summary = forged.encode().unwrap();
        let forged_bytes = bincode::serialize(&(summary.clone(), summary)).unwrap();
        let mut universe_products = products
            .take()
            .into_values()
            .filter_map(|bytes| {
                let (address, action, reference): (
                    hirsel_syntax::ContentHash,
                    hirsel_syntax::ContentHash,
                    store::Reference,
                ) = serde_json::from_slice(&bytes).ok()?;
                (reference.domain.kind == 0x4600_0038).then_some((address, action, reference))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            universe_products.len(),
            1,
            "std precompute publishes one universe product"
        );
        let (address, action, reference) = universe_products.pop().unwrap();
        let address = store::Address(address);
        let remote = store::store_by_name("remote@1", Some(dir.path().join("cas"))).unwrap();
        assert!(remote.verifier().is_none());
        remote.cache_product(
            address,
            store::Action(action),
            reference.domain,
            &forged_bytes,
        );
        let association = remote
            .lookup_product(address)
            .expect("remote store retains the planted universe association");
        let planted = remote
            .get_content(association.reference)
            .expect("remote store retains the planted summary bytes");
        assert_eq!(planted.as_ref(), forged_bytes.as_slice());

        let path = dir.path().join("main.hir");
        std::fs::write(&path, "pub fn main()->u64{7u64}\n").unwrap();
        let mut program = COMPILER
            .with_config(CompileConfig {
                store: remote,
                jobs: hirsel_intern::jobs::Jobs::ONE,
                hold_bytes: 0,
                ..Default::default()
            })
            .load_program(&path)
            .unwrap();
        let root = program.entry.0.package.clone();
        let snapshot = program.engine.signature_snapshot();
        let root_validation = snapshot.validation(&root).unwrap();
        let std_validation = snapshot.validation(&std).unwrap();
        assert!(root_validation.first_heap().is_some());
        assert_eq!(root_validation.first_heap(), expected_heap);
        assert!(root_validation.universe_errors().1.is_empty());
        assert!(std_validation.first_heap().is_some());
        assert_eq!(std_validation.first_heap(), expected_heap);
        assert!(std_validation.universe_errors().1.is_empty());
        for definition in snapshot
            .definitions()
            .into_iter()
            .filter(|definition| definition.path().0.package == std)
        {
            assert!(snapshot.header_ready(definition), "{definition:?}");
        }
        let checked = program.check(LoweringPolicy::Reachable).unwrap();
        assert_eq!(checked.completion, CheckCompletion::Complete);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    });
}
