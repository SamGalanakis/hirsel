use hirsel_diag::{Diagnostic, RuleId};
use hirsel_engine::memo::MemoKey;
use hirsel_engine::*;
use hirsel_engine_runtime::AdmittedRuntime;
use hirsel_engine_runtime::MemoryWeight;
use hirsel_engine_runtime::QueryRuntime;
use hirsel_engine_runtime::RuntimeErrors;
use hirsel_engine_store::MemStore;
use hirsel_intern::jobs::with_compiler_stack;

use hirsel_query_products::QueryKind;
use hirsel_query_products::reads::Reads;

use hirsel_intern::Symbol;
use hirsel_span::{FileId, SourceMap, Span};
use hirsel_syntax::Origin;
use hirsel_syntax::*;
use hirsel_types::*;
use std::sync::{Arc, Mutex};

fn out<T>(value: T) -> QueryOutput<T> {
    QueryOutput {
        value,
        diagnostics: vec![],
        dependencies: vec![],
    }
}
fn path(name: &str) -> DefPath {
    DefPath(ItemPath {
        package: default_context().package,
        modules: vec![],
        name: name.into(),
    })
}
fn error(span: Span) -> QueryError {
    QueryError::Diagnostics(vec![Diagnostic::error(
        const { RuleId::of("LANG-1") },
        span,
        "fixture rejection",
    )])
}
#[derive(Clone)]
struct Fake {
    events: Arc<Mutex<Vec<String>>>,
    reject: Option<&'static str>,
    missing_lookup: bool,
}
impl Fake {
    fn event(&self, phase: &str) {
        self.events.lock().unwrap().push(phase.into());
    }
    fn fail(&self, phase: &str, span: Span) -> Result<(), QueryError> {
        self.event(phase);
        if self.reject == Some("resolve+signature") && matches!(phase, "resolve" | "signature") {
            return Err(QueryError::Diagnostics(vec![Diagnostic::error(
                const { RuleId::of("LANG-1") },
                span,
                format!("{phase} fixture rejection"),
            )]));
        }
        if self.reject == Some(phase) {
            Err(error(span))
        } else {
            Ok(())
        }
    }
}
impl LexerPhase for Fake {
    fn lex(
        &self,
        file: FileId,
        sources: &SourceMap,
    ) -> Result<QueryOutput<Vec<Token>>, QueryError> {
        self.event("lex");
        for line in sources.file(file).unwrap().text().lines() {
            Symbol::intern(line.split_whitespace().nth(1).unwrap());
        }
        let mut result = out(vec![]);
        if self.reject == Some("lex") {
            let anchor = sources.file_anchor(file);
            let QueryError::Diagnostics(d) = error(Span::new(anchor, 0, 1).unwrap()) else {
                unreachable!()
            };
            result.diagnostics = d;
        }
        Ok(result)
    }
}
impl ParserPhase for Fake {
    fn parse(
        &self,
        file: FileId,
        tokens: Vec<Token>,
        context: &FileContext,
        sources: &mut SourceMap,
    ) -> Result<QueryOutput<SyntaxFile>, QueryError> {
        self.event("parse");
        let text = sources.file(file).unwrap().text().to_owned();
        let mut items = vec![];
        let mut offset = 0;
        for line in text.split_inclusive('\n') {
            // Each line is one item, anchored at its start.
            let anchor = sources.item_anchor(file, offset as u32);
            let name = line.split_whitespace().nth(1).unwrap();
            let name_start = line.find(name).unwrap();
            let body_start = line.find('{').unwrap();
            let mut tree = SyntaxTree::default();
            let body = ExprId(0);
            let origin = Origin {
                span: Span::new(anchor, 0, line.len() as u32).unwrap(),
                tokens: TokenRange::default(),
            };
            tree.expressions.push(Expr {
                origin: Origin {
                    span: Span::new(anchor, body_start as u32, origin.span.end).unwrap(),
                    tokens: TokenRange::default(),
                },
                kind: ExprKind::Literal(Literal::Bool(true)),
            });
            let item = Item {
                name_span: Span::new(anchor, name_start as u32, (name_start + name.len()) as u32)
                    .unwrap(),
                origin,
                name: Symbol::intern("item"),
                definition: DefPath(ItemPath {
                    package: context.package.clone(),
                    modules: context.modules.clone(),
                    name: name.into(),
                }),
                visibility: Visibility::Private,
                attributes: vec![],
                kind: ItemKind::Function {
                    signature: FunctionSyntax::default(),
                    body: Some(body),
                    abi: Some(hirsel_syntax::ForeignAbi {
                        literal: line[..line.find('{').unwrap()].into(),
                        contract: None,
                    }),
                },
                generated: None,
            };
            items.push(Arc::new(ItemSyntax {
                retention: Default::default(),
                item,
                tree,
                anchor,
                text_hash: ContentHash::of_bytes(line.as_bytes()),
                occurrence: 0,
            }));
            offset += line.len();
        }
        let mut result = out(SyntaxFile {
            has_syntax_errors: false,
            file,
            items,
            tokens,
        });
        if self.reject == Some("parse") {
            let anchor = sources.file_anchor(file);
            let QueryError::Diagnostics(d) = error(Span::new(anchor, 0, 1).unwrap()) else {
                unreachable!()
            };
            result.diagnostics = d;
        }
        Ok(result)
    }
}
impl ResolverPhase for Fake {
    fn resolve(
        &self,
        _t: &hirsel_types::TypeInterner,
        _m: &hirsel_types::TypeMemo,
        syntax: Arc<ItemSyntax>,
        _declarations: &Declarations,
        _products: &mut PhaseProducts,
        provider: &mut dyn SignatureProvider,
    ) -> Result<QueryOutput<Resolution>, QueryError> {
        let item = &syntax.item;
        self.fail("resolve", item.origin.span)?;
        if item.definition == path("b") {
            provider.signature(DefId::intern(&path("a")))?;
        }
        if self.missing_lookup {
            let name = Symbol::intern("missing");
            let _ = provider.lookup(
                &default_context(),
                &Path {
                    package: None,
                    segments: vec![name],
                },
            );
            let _ = provider.signature(DefId::intern(&path("missing")));
            return Err(error(item.origin.span));
        }
        Ok(out(Resolution::default()))
    }
}
impl SignaturePhase for Fake {
    fn signature(
        &self,
        t: &hirsel_types::TypeInterner,
        _m: &hirsel_types::TypeMemo,
        syntax: Arc<ItemSyntax>,
        _: &Declarations,
        _: &mut PhaseProducts,
        _provider: &mut dyn SignatureProvider,
    ) -> Result<QueryOutput<Signature>, QueryError> {
        let item = &syntax.item;
        self.fail("signature", item.origin.span)?;
        // A declared fixture signature depends on the header, never the body.
        let fingerprint = match &item.kind {
            ItemKind::Function {
                abi: Some(header), ..
            } => ContentHash::of_bytes(header.literal.as_bytes()),
            _ => unreachable!(),
        };
        let unit = t.intern(Type::with_kind(TypeKind::Tuple(vec![].into())));
        Ok(out(Signature {
            metadata: Default::default(),
            definition: DefId::intern(&item.definition),
            kind: SignatureKind::Function(FunctionType {
                state_promises: Default::default(),
                generics: Default::default(),
                parameters: Default::default(),
                result: unit,
                error: None,
                allocation: Allocation::MayAllocate,
                authority: AuthoritySummary::inert(),
                uses: Uses::Unrestricted,
                contracts: Default::default(),
                incomplete: false,
                foreign: None,
            }),
            fingerprint,
        }))
    }
}
impl CheckerPhase for Fake {
    fn check(
        &self,
        t: &hirsel_types::TypeInterner,
        _m: &hirsel_types::TypeMemo,
        syntax: Arc<ItemSyntax>,
        resolution: Resolution,
        _signature: &Signature,
        _provider: &mut dyn SignatureProvider,
    ) -> Result<QueryOutput<hirsel_query_products::TypedBody>, QueryError> {
        self.fail("check", syntax.item.origin.span)?;
        let unit = match _signature.kind {
            SignatureKind::Const { ty: unit, .. } => unit,
            SignatureKind::Function(ref f) => f.result,
            _ => unreachable!(),
        };
        let boolean = t.intern(Type {
            kind: TypeKind::Primitive(hirsel_types::Primitive::Bool),
            ..t.row(unit).clone()
        });
        Ok(out(hirsel_query_products::TypedBody {
            checked: CheckedBody {
                lexical: syntax.owner(),
                tails: vec![],
                places: vec![],
                calls: vec![],
                bindings: vec![],
                patterns: vec![],
                resolution,
                expressions: vec![],
                instantiations: vec![],
                closures: vec![],
            },
            syntax,
            plans: Default::default(),
            unit,
            boolean,
        }))
    }
}
impl ModesPhase for Fake {
    fn check_modes(
        &self,
        _t: &hirsel_types::TypeInterner,
        _m: &hirsel_types::TypeMemo,
        body: Arc<hirsel_query_products::TypedBody>,
        _signature: &Signature,
        _: &mut PhaseProducts,
        _provider: &mut dyn SignatureProvider,
    ) -> Result<QueryOutput<hirsel_query_products::AnalyzedBody>, QueryError> {
        self.fail("modes", body.syntax.item.origin.span)?;
        Ok(out(hirsel_query_products::AnalyzedBody {
            typed: body,
            ownership: Default::default(),
            results: vec![],
            call_allocations: vec![],
        }))
    }
}
impl LoweringPhase for Fake {
    fn lower(
        &self,
        t: &hirsel_types::TypeInterner,
        _m: &hirsel_types::TypeMemo,
        body: &hirsel_query_products::AnalyzedBody,
        _comptime: &hirsel_query_products::Comptime,
        signature: &Signature,
    ) -> Result<QueryOutput<hirsel_tir::Definition>, QueryError> {
        let syntax = &body.typed.syntax;
        self.fail("lower", syntax.item.origin.span)?;
        let mut definition = hirsel_tir::Definition {
            caller_bodies: vec![],
            protocol_associations: vec![],
            types: Arc::from([]),
            path: signature.definition,
            signature_hash: signature.fingerprint,
            bodies: if self.reject == Some("invalid-tir") {
                vec![]
            } else {
                vec![unit_body(signature, syntax, syntax.item.origin.span)]
            },
            entry: hirsel_tir::BodyId(0),
            contracts: vec![],
        };
        definition.types = hirsel_tir::definition_types(t, &definition);
        Ok(out(definition))
    }
    fn lower_evaluation(
        &self,
        _t: &hirsel_types::TypeInterner,
        _m: &hirsel_types::TypeMemo,
        _: &hirsel_query_products::AnalyzedBody,
        _staged: &[(hirsel_syntax::ExprId, hirsel_types::TypeId)],
        _: &Signature,
        _: ExprId,
        _: TypeId,
        _: &[(ExprId, hirsel_tir::Materialized)],
    ) -> Result<hirsel_tir::Definition, QueryError> {
        Err(QueryError::NotImplemented)
    }
    fn entry_body(&self) -> hirsel_tir::BodyId {
        hirsel_tir::BodyId(0)
    }
}
fn unit_body(signature: &Signature, syntax: &ItemSyntax, span: Span) -> hirsel_tir::Body {
    use hirsel_tir::*;
    let unit = match &signature.kind {
        SignatureKind::Const { ty: unit, .. } => *unit,
        SignatureKind::Function(function) => {
            assert!(function.parameters.is_empty());
            function.result
        }
        _ => unreachable!(),
    };
    Body {
        arena_scope_grant: None,
        scoped_parameters: Vec::new(),
        parameter_scope: syntax.owner().site(NameSite::Item),
        locals: vec![Local {
            ty: unit,
            storage: Storage::ReturnSlot,
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
            terminator: Terminator::Return {
                value: Operand {
                    place: PlaceId(0),
                    usage: UseKind::Take,
                },
                cleanup: CleanupId(0),
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
    }
}
fn make_engine(
    reject: Option<&'static str>,
    missing_lookup: bool,
) -> (QueryEngine, Arc<Mutex<Vec<String>>>) {
    let events = Arc::new(Mutex::new(vec![]));
    let fake = Arc::new(Fake {
        events: events.clone(),
        reject,
        missing_lookup,
    });
    let adapters = PhaseAdapters {
        lexer: Some(fake.clone()),
        parser: Some(fake.clone()),
        resolver: Some(fake.clone()),
        signature: Some(fake.clone()),
        checker: Some(fake.clone()),
        modes: Some(fake.clone()),
        lowering: Some(fake),
        ..Default::default()
    };
    let engine = QueryEngine::new(default_environment(), adapters);
    (engine, events)
}
fn make_engine_with_store(
    reject: Option<&'static str>,
    missing_lookup: bool,
) -> (QueryEngine, Arc<Mutex<Vec<String>>>) {
    let (engine, events) = make_engine(reject, missing_lookup);
    (
        engine.with_result_store(Arc::new(MemStore::default())),
        events,
    )
}
fn parse(engine: &mut QueryEngine, text: &str) -> FileId {
    let file = engine.sources.add("fixture.hir", text.into()).unwrap();
    engine.parse_file(file, &default_context()).unwrap();
    file
}
struct AdmittedFront {
    queries: Arc<FrontBodyQueries>,
    runtime: AdmittedRuntime,
    _budget: Arc<hirsel_engine_admission::ProcessBudget>,
}
impl AdmittedFront {
    fn request(&mut self, definitions: &[DefPath]) -> Vec<FrontBodyOutput> {
        let mut order: Vec<_> = (0..definitions.len()).collect();
        order.sort_by(|left, right| definitions[*left].cmp(&definitions[*right]));
        let roots: Vec<_> = order
            .iter()
            .map(|index| self.queries.query(&definitions[*index]).unwrap())
            .collect();
        let request = self.runtime.request(&roots);
        let mut outcomes: Vec<Option<FrontBodyOutput>> =
            std::iter::repeat_with(|| None).take(roots.len()).collect();
        let mut successful = Vec::new();
        match self.runtime.wait(request) {
            Ok(products) => {
                assert_eq!(products.len(), roots.len());
                successful.extend(order.iter().copied());
            }
            Err(failure) => {
                let first = self.queries.outcome(Err(failure)).unwrap_or_else(|| {
                    panic!("completed front failure request has no outcome: {failure:?}")
                });
                let first_index = definitions
                    .iter()
                    .position(|definition| definition == &first.outcome.definition)
                    .expect("failure outcome belongs to a requested definition");
                outcomes[first_index] = Some(first);
                for (index, root) in order.iter().copied().zip(roots) {
                    if outcomes[index].is_some() {
                        continue;
                    }
                    let request = self.runtime.request(&[root]);
                    match self.runtime.wait(request) {
                        Ok(products) => {
                            assert_eq!(products.len(), 1);
                            successful.push(index);
                        }
                        Err(failure) => {
                            let outcome = self
                                .queries
                                .outcome(Err(failure))
                                .expect("completed front failure request has an outcome");
                            assert_eq!(outcome.outcome.definition, definitions[index]);
                            outcomes[index] = Some(outcome);
                        }
                    }
                }
            }
        }
        successful.sort_by(|left, right| definitions[*left].cmp(&definitions[*right]));
        let body_roots: Vec<_> = successful
            .iter()
            .map(|index| self.queries.body_query(&definitions[*index]).unwrap())
            .collect();
        if !body_roots.is_empty() {
            let request = self.runtime.request(&body_roots);
            let products = self.runtime.wait(request).unwrap_or_else(|failure| {
                panic!("completed body materialization failed: {failure:?}")
            });
            assert_eq!(products.len(), body_roots.len());
            for (index, product) in successful.into_iter().zip(products) {
                outcomes[index] = Some(
                    self.queries
                        .outcome(Ok(product))
                        .expect("completed body request has an outcome"),
                );
            }
        }
        outcomes.into_iter().map(Option::unwrap).collect()
    }
}
fn admitted_front(engine: &mut QueryEngine, definitions: &[DefPath], jobs: usize) -> AdmittedFront {
    let budget = hirsel_engine_admission::ProcessBudget::new(1 << 30);
    let errors = runtime_errors();
    let queries = engine
        .front_body_queries(
            definitions,
            MemoryWeight {
                retained: 1 << 20,
                scratch: 8 << 20,
                stack: 0,
            },
            &budget,
            errors,
        )
        .unwrap();
    let mut runtime = AdmittedRuntime::new(
        hirsel_intern::jobs::Jobs::new(jobs),
        budget.reserve(512 << 20).unwrap(),
        errors,
    )
    .unwrap();
    runtime
        .register(FRONT_BODY_KIND, queries.factory())
        .unwrap();
    AdmittedFront {
        queries,
        runtime,
        _budget: budget,
    }
}
fn runtime_errors() -> RuntimeErrors {
    let reference = |text: &[u8]| {
        hirsel_engine_store::Domain {
            kind: 0x4600_00ff,
            schema: 1,
        }
        .reference(text)
        .unwrap()
    };
    RuntimeErrors {
        resource: reference(b"resource"),
        cycle: reference(b"cycle"),
        cancelled: reference(b"cancelled"),
        protocol: reference(b"protocol"),
        panicked: reference(b"panic"),
    }
}
fn admitted_memo_reads(
    front: &AdmittedFront,
    definition: &DefPath,
    kind: QueryKind,
) -> Option<Arc<Reads>> {
    front
        .queries
        .memo_snapshot()
        .expect("admitted Front observations remain readable")
        .into_iter()
        .find_map(|(key, reads)| {
            (key == MemoKey::new(kind, DefId::intern(definition))).then_some(reads)
        })
}
#[test]
fn invalid_tir_is_never_published_and_operational_failures_retry() {
    hirsel_intern::jobs::with_compiler_stack(
        invalid_tir_is_never_published_and_operational_failures_retry_on_compiler_thread,
    );
}
fn invalid_tir_is_never_published_and_operational_failures_retry_on_compiler_thread() {
    let (mut engine, events) = make_engine(Some("invalid-tir"), false);
    parse(&mut engine, "fn a () { 1 }\n");
    for _ in 0..2 {
        let failure = engine.lower_to_tir(&path("a")).unwrap_err();
        assert!(
            matches!(failure, QueryError::Operational(ref e) if e.operation == "TIR validation"),
            "{failure:?}"
        );
    }
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|phase| phase.as_str() == "lower")
            .count(),
        2
    );
    let (mut engine, events) = make_engine(Some("invalid-tir"), false);
    let file = parse(&mut engine, "fn a () { 1 }\n");
    for request in 1..=2 {
        let failure = check_snapshot(&mut engine, file, &default_context()).unwrap_err();
        assert!(
            matches!(failure, QueryError::Operational(ref e) if e.operation == "TIR validation"),
            "{failure:?}"
        );
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|phase| phase.as_str() == "lower")
                .count(),
            request,
            "one failed Lower per Check, retried by the next request"
        );
    }
}
/// `Verify::Off` is the one switch that skips V: the same invalid TIR lowers.
#[test]
fn invalid_tir_lowers_without_v() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let (mut engine, _) = make_engine(Some("invalid-tir"), false);
        engine.verify = Verify::Off;
        parse(&mut engine, "fn a () { 1 }\n");
        engine.lower_to_tir(&path("a")).unwrap();
    });
}
#[test]
fn phases_run_in_order_and_stop_at_every_error_boundary() {
    hirsel_intern::jobs::with_compiler_stack(
        phases_run_in_order_and_stop_at_every_error_boundary_on_compiler_thread,
    );
}
fn phases_run_in_order_and_stop_at_every_error_boundary_on_compiler_thread() {
    let phases = [
        "lex",
        "parse",
        "resolve",
        "signature",
        "check",
        "modes",
        "lower",
    ];
    for stop in 0..phases.len() {
        let (mut engine, events) = make_engine(Some(phases[stop]), false);
        let file = engine
            .sources
            .add("fixture.hir", "fn a () { 1 }\n".into())
            .unwrap();
        let result = check_snapshot(&mut engine, file, &default_context());
        assert!(
            matches!(result, Err(QueryError::Diagnostics(_))),
            "stop {}: {result:?}",
            phases[stop]
        );
        let expected = match stop {
            0 => vec!["lex"],
            1 => vec!["lex", "parse"],
            2 | 3 => vec!["lex", "parse", "signature", "resolve"],
            _ => [
                vec!["lex", "parse", "signature", "resolve"],
                phases[4..=stop].to_vec(),
            ]
            .concat(),
        };
        assert_eq!(
            *events.lock().unwrap(),
            expected,
            "program rejects at {}",
            phases[stop]
        );
    }
    let (mut engine, events) = make_engine(None, false);
    let file = engine
        .sources
        .add("fixture.hir", "fn a () { 1 }\n".into())
        .unwrap();
    assert!(
        check_snapshot(&mut engine, file, &default_context())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        *events.lock().unwrap(),
        [
            "lex",
            "parse",
            "signature",
            "resolve",
            "check",
            "modes",
            "lower"
        ]
    );
}
/// Direct body queries still demand resolution before the owner signature;
/// program checks freeze signatures in S0 before running the same body stages.
#[test]
fn scalar_queries_preserve_phase_order_and_stop_at_every_error_boundary() {
    with_compiler_stack(|| {
        let phases = [
            "lex",
            "parse",
            "resolve",
            "signature",
            "check",
            "modes",
            "lower",
        ];
        for stop in 0..phases.len() {
            let (mut engine, events) = make_engine(Some(phases[stop]), false);
            let file = engine
                .sources
                .add("fixture.hir", "fn a () { 1 }\n".into())
                .unwrap();
            let result = engine
                .parse_file(file, &default_context())
                .and_then(|parsed| {
                    if !parsed.diagnostics.is_empty() {
                        return Err(QueryError::Diagnostics(parsed.diagnostics));
                    }
                    engine.lower_to_tir(&path("a"))
                });
            assert!(matches!(result, Err(QueryError::Diagnostics(_))));
            assert_eq!(*events.lock().unwrap(), phases[..=stop]);
        }
        let (mut engine, events) = make_engine(None, false);
        parse(&mut engine, "fn a () { 1 }\n");
        engine.lower_to_tir(&path("a")).unwrap();
        assert_eq!(*events.lock().unwrap(), phases);
    });
}
#[test]
fn frozen_signature_preflight_preserves_resolution_rejection_precedence() {
    with_compiler_stack(|| {
        for jobs in [1, 2, 8] {
            let (mut engine, events) = make_engine_with_store(Some("resolve+signature"), false);
            parse(&mut engine, "fn a () { 1 }\n");
            let definitions = vec![path("a")];
            let mut front = admitted_front(&mut engine, &definitions, jobs);
            let outcomes = front.request(&definitions);
            assert_eq!(outcomes[0].outcome.phase, Some(QueryKind::Resolve));
            let Err(QueryError::Diagnostics(diagnostics)) = &outcomes[0].outcome.result else {
                panic!("expected resolution rejection");
            };
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].message, "resolve fixture rejection");
            assert_eq!(
                *events.lock().unwrap(),
                ["lex", "parse", "signature", "resolve"]
            );
        }
    });
}
#[test]
fn changing_one_body_recomputes_only_that_definition() {
    hirsel_intern::jobs::with_compiler_stack(
        changing_one_body_recomputes_only_that_definition_on_compiler_thread,
    );
}
fn changing_one_body_recomputes_only_that_definition_on_compiler_thread() {
    let (mut engine, _) = make_engine(None, false);
    parse(&mut engine, "fn a () { 1 }\nfn b () { 2 }\n");
    engine.lower_to_tir(&path("a")).unwrap();
    engine.lower_to_tir(&path("b")).unwrap();
    parse(&mut engine, "fn a () { 12345 }\nfn b () { 2 }\n");
    engine.lower_to_tir(&path("a")).unwrap();
    engine.lower_to_tir(&path("b")).unwrap();
    for kind in [
        QueryKind::Lex,
        QueryKind::Parse,
        QueryKind::Resolve,
        QueryKind::Check,
        QueryKind::Modes,
        QueryKind::Lower,
    ] {
        assert_eq!(engine.recompute_count(&path("a"), kind), 2, "{kind:?}");
        assert_eq!(engine.recompute_count(&path("b"), kind), 1, "{kind:?}");
    }
    assert_eq!(engine.recompute_count(&path("a"), QueryKind::Signature), 1);
    assert_eq!(engine.recompute_count(&path("b"), QueryKind::Signature), 1);
}
#[test]
fn changing_a_signature_recomputes_its_users() {
    hirsel_intern::jobs::with_compiler_stack(
        changing_a_signature_recomputes_its_users_on_compiler_thread,
    );
}
fn changing_a_signature_recomputes_its_users_on_compiler_thread() {
    let (mut engine, _) = make_engine(None, false);
    parse(&mut engine, "fn a () { 1 }\nfn b () { 2 }\n");
    engine.lower_to_tir(&path("b")).unwrap();
    parse(&mut engine, "fn a (changed) { 1 }\nfn b () { 2 }\n");
    engine.lower_to_tir(&path("b")).unwrap();
    for kind in [
        QueryKind::Resolve,
        QueryKind::Check,
        QueryKind::Modes,
        QueryKind::Lower,
    ] {
        assert_eq!(engine.recompute_count(&path("b"), kind), 2, "{kind:?}");
    }
}
#[test]
fn failed_provider_reads_survive_rejection_and_manifest_changes_invalidate() {
    hirsel_intern::jobs::with_compiler_stack(
        failed_provider_reads_survive_rejection_and_manifest_changes_invalidate_on_compiler_thread,
    );
}
fn failed_provider_reads_survive_rejection_and_manifest_changes_invalidate_on_compiler_thread() {
    let (mut engine, _) = make_engine(None, true);
    parse(&mut engine, "fn a () { 1 }\n");
    assert!(engine.resolve_definition(&path("a")).is_err());
    let reads = engine
        .dependency_reads(&path("a"), QueryKind::Resolve)
        .unwrap();
    assert_eq!(reads.lookups, vec![(path("missing").into(), false)]);
    assert_eq!(reads.failed_signatures, vec![path("missing")]);
    assert!(engine.resolve_definition(&path("a")).is_err());
    assert_eq!(engine.recompute_count(&path("a"), QueryKind::Resolve), 1);
    parse(&mut engine, "fn a () { 1 }\nfn missing () { 3 }\n");
    assert!(engine.resolve_definition(&path("a")).is_err());
    assert_eq!(engine.recompute_count(&path("a"), QueryKind::Resolve), 2);
    let reads = engine
        .dependency_reads(&path("a"), QueryKind::Resolve)
        .unwrap();
    assert_eq!(reads.lookups, vec![(path("missing").into(), true)]);
    assert_eq!(*reads.signatures[0].definition.path(), path("missing"));
}
#[test]
fn missing_stages_remain_unavailable_and_diagnostic_sources_survive_edits() {
    hirsel_intern::jobs::with_compiler_stack(
        missing_stages_remain_unavailable_and_diagnostic_sources_survive_edits_on_compiler_thread,
    );
}
fn missing_stages_remain_unavailable_and_diagnostic_sources_survive_edits_on_compiler_thread() {
    let (mut engine, _) = make_engine(Some("resolve"), false);
    let old = parse(&mut engine, "fn a () { 1 }\n");
    let Err(QueryError::Diagnostics(diagnostics)) = engine.resolve_definition(&path("a")) else {
        panic!("expected errors");
    };
    parse(&mut engine, "fn a () { 123 }\n");
    let output = CheckOutput {
        completion: Default::default(),

        sources: engine.sources.clone(),
        diagnostics,
        unsafe_ffi: vec![],
    };
    assert_eq!(output.sources.file(old).unwrap().text(), "fn a () { 1 }\n");
    assert_eq!(
        output
            .sources
            .location(output.diagnostics[0].primary)
            .unwrap()
            .line,
        1
    );
    let (mut engine, _) = make_engine(None, false);
    parse(&mut engine, "fn a () { 1 }\n");
    engine.adapters.checker = None;
    assert!(matches!(
        engine.check_definition(&path("a")),
        Err(QueryError::NotImplemented)
    ));
    assert_eq!(engine.unavailable, Some(QueryKind::Check));
    let (mut engine, _) = make_engine(None, false);
    parse(&mut engine, "fn a () { 1 }\n");
    engine.adapters.modes = None;
    assert!(matches!(
        engine.lower_to_tir(&path("a")),
        Err(QueryError::NotImplemented)
    ));
    assert_eq!(engine.unavailable, Some(QueryKind::Modes));
    let (mut engine, _) = make_engine(None, false);
    parse(&mut engine, "fn a () { 1 }\n");
    engine.adapters.lowering = None;
    assert!(matches!(
        engine.lower_to_tir(&path("a")),
        Err(QueryError::NotImplemented)
    ));
    assert_eq!(engine.unavailable, Some(QueryKind::Lower));
}

struct ContractSignatures;
impl SignaturePhase for ContractSignatures {
    fn signature(
        &self,
        t: &hirsel_types::TypeInterner,
        _m: &hirsel_types::TypeMemo,
        syntax: Arc<ItemSyntax>,
        _declarations: &Declarations,
        _products: &mut PhaseProducts,
        provider: &mut dyn SignatureProvider,
    ) -> Result<QueryOutput<Signature>, QueryError> {
        let item = &syntax.item;
        let ItemKind::Function {
            abi: Some(header), ..
        } = &item.kind
        else {
            unreachable!()
        };
        let mut fingerprint = ContentHash::of_bytes(header.literal.as_bytes());
        let mut contracts = vec![];
        if item.definition == path("a") {
            let used = provider.signature(DefId::intern(&path("contract")))?;
            fingerprint = ContentHash::of_parts(
                "fixture.contract.signature",
                [fingerprint.0.as_slice(), used.fingerprint.0.as_slice()],
            );
            contracts.push(hirsel_types::Contract {
                kind: hirsel_syntax::ContractKind::Requires,
                ordinal: 0,
                code_hash: fingerprint,
            });
        }
        let result = t.intern(hirsel_types::Type {
            drop_allocation: hirsel_types::DropAllocation::MayAllocate,
            ..hirsel_types::Type::with_kind(TypeKind::Tuple(vec![].into()))
        });
        Ok(out(Signature {
            metadata: Default::default(),
            definition: DefId::intern(&item.definition),
            fingerprint,
            kind: SignatureKind::Function(FunctionType {
                state_promises: Default::default(),
                generics: vec![].into(),
                parameters: vec![].into(),
                result,
                error: None,
                allocation: Allocation::MayAllocate,
                authority: AuthoritySummary::inert(),
                uses: Uses::Unrestricted,
                contracts: contracts.into(),
                incomplete: false,
                foreign: None,
            }),
        }))
    }
}
#[test]
fn contract_signature_reads_invalidate_the_owner_and_its_users() {
    hirsel_intern::jobs::with_compiler_stack(
        contract_signature_reads_invalidate_the_owner_and_its_users_on_compiler_thread,
    );
}
fn contract_signature_reads_invalidate_the_owner_and_its_users_on_compiler_thread() {
    let (mut engine, _) = make_engine(None, false);
    engine.adapters.signature = Some(Arc::new(ContractSignatures));
    parse(
        &mut engine,
        "fn a () { 1 }\nfn b () { 2 }\nfn contract () { 3 }\n",
    );
    engine.lower_to_tir(&path("b")).unwrap();
    let reads = engine
        .dependency_reads(&path("a"), QueryKind::Signature)
        .unwrap();
    assert_eq!(*reads.signatures[0].definition.path(), path("contract"));
    let previous = engine.item_signature(&path("a")).unwrap().value.fingerprint;
    parse(
        &mut engine,
        "fn a () { 1 }\nfn b () { 2 }\nfn contract (changed) { 3 }\n",
    );
    engine.lower_to_tir(&path("b")).unwrap();
    assert_ne!(
        engine.item_signature(&path("a")).unwrap().value.fingerprint,
        previous
    );
    assert_eq!(engine.recompute_count(&path("a"), QueryKind::Signature), 2);
    assert_eq!(engine.recompute_count(&path("b"), QueryKind::Check), 2);
}

#[test]
fn failed_signature_changes_invalidate_a_rejected_user() {
    hirsel_intern::jobs::with_compiler_stack(
        failed_signature_changes_invalidate_a_rejected_user_on_compiler_thread,
    );
}
fn failed_signature_changes_invalidate_a_rejected_user_on_compiler_thread() {
    let (mut engine, _) = make_engine(Some("signature"), false);
    parse(&mut engine, "fn a () { 1 }\nfn b () { 2 }\n");
    assert!(engine.resolve_definition(&path("b")).is_err());
    let old_key = engine
        .dependency_reads(&path("b"), QueryKind::Resolve)
        .unwrap()
        .failed_signature_keys
        .clone();
    parse(&mut engine, "fn a (changed) { 1 }\nfn b () { 2 }\n");
    assert!(engine.resolve_definition(&path("b")).is_err());
    assert_eq!(engine.recompute_count(&path("b"), QueryKind::Resolve), 2);
    assert_ne!(
        engine
            .dependency_reads(&path("b"), QueryKind::Resolve)
            .unwrap()
            .failed_signature_keys,
        old_key
    );
}
struct ObserveDuplicates;
impl SignaturePhase for ObserveDuplicates {
    fn signature(
        &self,
        _t: &hirsel_types::TypeInterner,
        _m: &hirsel_types::TypeMemo,
        _syntax: Arc<ItemSyntax>,
        _: &Declarations,
        _: &mut PhaseProducts,
        _provider: &mut dyn SignatureProvider,
    ) -> Result<QueryOutput<Signature>, QueryError> {
        unreachable!("candidate-aware entry point must be used")
    }
    fn signature_items(
        &self,
        _t: &hirsel_types::TypeInterner,
        _m: &hirsel_types::TypeMemo,
        candidates: &[Arc<ItemSyntax>],
        _: &Declarations,
        _: &mut PhaseProducts,
        _provider: &mut dyn SignatureProvider,
    ) -> Result<QueryOutput<Signature>, QueryError> {
        assert_eq!(candidates.len(), 2);
        let later = &candidates[1];
        assert_eq!(later.occurrence, 1);
        Err(QueryError::Diagnostics(vec![Diagnostic::error(
            const { RuleId::of("LANG-7") },
            later.item.origin.span,
            "fixture overloading",
        )]))
    }
}
#[test]
fn duplicate_items_keep_one_identity_and_reach_the_signature_phase() {
    hirsel_intern::jobs::with_compiler_stack(
        duplicate_items_keep_one_identity_and_reach_the_signature_phase_on_compiler_thread,
    );
}
fn duplicate_items_keep_one_identity_and_reach_the_signature_phase_on_compiler_thread() {
    let (mut engine, _) = make_engine(None, false);
    parse(&mut engine, "fn a () { 1 }\n");
    let duplicate = engine
        .sources
        .add("other.hir", "fn a () { 2 }\n".into())
        .unwrap();
    engine.parse_file(duplicate, &default_context()).unwrap();
    let candidates = engine.definition_items(&path("a")).unwrap();
    assert_eq!(candidates.len(), 2);
    let file = |syntax: &ItemSyntax| engine.sources.anchor(syntax.anchor).unwrap().0;
    assert_ne!(file(&candidates[0]), file(&candidates[1]));
    for (occurrence, syntax) in candidates.iter().enumerate() {
        assert_eq!(syntax.item.definition, path("a"));
        assert_eq!(syntax.occurrence, occurrence as u32);
    }
    engine.adapters.signature = Some(Arc::new(ObserveDuplicates));
    let Err(QueryError::Diagnostics(errors)) = engine.item_signature(&path("a")) else {
        panic!("signature phase owns overloading");
    };
    assert_eq!(errors[0].rule.as_str(), "LANG-7");
    assert_eq!(engine.sources.resolve(errors[0].primary).file, duplicate);
}

#[test]
fn environment_revisions_invalidate_semantic_queries() {
    hirsel_intern::jobs::with_compiler_stack(
        environment_revisions_invalidate_semantic_queries_on_compiler_thread,
    );
}
fn environment_revisions_invalidate_semantic_queries_on_compiler_thread() {
    let (mut engine, _) = make_engine(None, false);
    parse(&mut engine, "fn a () { 1 }\n");
    engine.lower_to_tir(&path("a")).unwrap();
    engine.environment.interface_version += 1;
    engine.lower_to_tir(&path("a")).unwrap();
    engine.environment.compiler_version.push_str("-new");
    engine.lower_to_tir(&path("a")).unwrap();
    engine.environment.spec_hash = ContentHash::of_bytes(b"new spec");
    engine.lower_to_tir(&path("a")).unwrap();
    engine.environment.package_manifest_hash = ContentHash::of_bytes(b"new manifest");
    engine.lower_to_tir(&path("a")).unwrap();
    engine.environment.builtin_signatures_hash = ContentHash::of_bytes(b"new builtins");
    engine.lower_to_tir(&path("a")).unwrap();
    assert_eq!(engine.recompute_count(&path("a"), QueryKind::Check), 6);
    assert_eq!(engine.recompute_count(&path("a"), QueryKind::Signature), 6);
}

#[test]
fn provider_results_share_graphs_and_manifest_misses_have_revisions() {
    hirsel_intern::jobs::with_compiler_stack(
        provider_results_share_graphs_and_manifest_misses_have_revisions_on_compiler_thread,
    );
}
fn provider_results_share_graphs_and_manifest_misses_have_revisions_on_compiler_thread() {
    let (mut engine, _) = make_engine(None, false);
    parse(&mut engine, "fn a {}\n");
    let a = engine.signature(DefId::intern(&path("a"))).unwrap();
    let b = engine.signature(DefId::intern(&path("a"))).unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
    let a = engine.definition(&path("a")).unwrap();
    let b = engine.definition(&path("a")).unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
    let lookup = Path {
        package: None,
        segments: vec![Symbol::intern("missing")],
    };
    let before = engine.lookup(&default_context(), &lookup);
    assert!(before.result.is_err());
    parse(&mut engine, "fn a {}\nfn missing {}\n");
    let after = engine.lookup(&default_context(), &lookup);
    assert!(after.result.is_ok());
    assert_ne!(before.manifest, after.manifest);
}
#[test]
fn operational_signature_failures_retry_and_are_not_rejection_cache_entries() {
    hirsel_intern::jobs::with_compiler_stack(
        operational_signature_failures_retry_and_are_not_rejection_cache_entries_on_compiler_thread,
    );
}
fn operational_signature_failures_retry_and_are_not_rejection_cache_entries_on_compiler_thread() {
    struct Flaky {
        original: Arc<dyn SignaturePhase>,
        calls: std::sync::atomic::AtomicU32,
    }
    impl SignaturePhase for Flaky {
        fn signature(
            &self,
            t: &hirsel_types::TypeInterner,
            m: &hirsel_types::TypeMemo,
            syntax: Arc<ItemSyntax>,
            _declarations: &Declarations,
            _products: &mut PhaseProducts,
            provider: &mut dyn SignatureProvider,
        ) -> Result<QueryOutput<Signature>, QueryError> {
            if self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                == 0
            {
                return Err(QueryError::Operational(OperationalFailure {
                    operation: "signature provider".into(),
                    message: "temporarily offline".into(),
                }));
            }
            self.original
                .signature(t, m, syntax, _declarations, _products, provider)
        }
    }
    let (mut engine, _) = make_engine(None, false);
    let adapter = Arc::new(Flaky {
        original: engine.adapters.signature.take().unwrap(),
        calls: std::sync::atomic::AtomicU32::new(0),
    });
    engine.adapters.signature = Some(adapter.clone());
    parse(&mut engine, "fn a {}\n");
    assert!(matches!(
        engine.signature(DefId::intern(&path("a"))),
        Err(QueryError::Operational(_))
    ));
    assert!(engine.signature(DefId::intern(&path("a"))).is_ok());
    assert!(engine.signature(DefId::intern(&path("a"))).is_ok());
    assert_eq!(adapter.calls.load(std::sync::atomic::Ordering::Relaxed), 2);
}
#[test]
fn partial_check_preserves_diagnostics_and_reports_the_missing_phase() {
    hirsel_intern::jobs::with_compiler_stack(
        partial_check_preserves_diagnostics_and_reports_the_missing_phase_on_compiler_thread,
    );
}
fn partial_check_preserves_diagnostics_and_reports_the_missing_phase_on_compiler_thread() {
    let (mut engine, _) = make_engine(Some("resolve"), false);
    engine.adapters.checker = None;
    let file = parse(&mut engine, "fn a {}\n");
    let output = source_check_request(&mut engine, file, &default_context()).unwrap();
    assert_eq!(output.diagnostics.len(), 1);
    assert_eq!(
        output.completion,
        CheckCompletion::Incomplete {
            phase: QueryKind::Check
        }
    );
    assert!(
        output
            .sources
            .location(output.diagnostics[0].primary)
            .is_some()
    );
}

#[test]
fn signature_hits_track_later_duplicate_headers_and_positions() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let (mut engine, _) = make_engine(None, false);
        parse(&mut engine, "fn a () { 1 }\n");
        let duplicate = engine
            .sources
            .add("other.hir", "fn a () { 2 }\n".into())
            .unwrap();
        engine.parse_file(duplicate, &default_context()).unwrap();
        engine.adapters.signature = Some(Arc::new(ObserveDuplicates));
        let diagnose = |engine: &mut QueryEngine| {
            let Err(QueryError::Diagnostics(errors)) = engine.item_signature(&path("a")) else {
                panic!("duplicate signature must reject");
            };
            assert_eq!(errors[0].rule.as_str(), "LANG-7");
            errors[0].primary
        };
        let old = diagnose(&mut engine);
        assert_eq!(diagnose(&mut engine), old);
        assert_eq!(engine.recompute_count(&path("a"), QueryKind::Signature), 1);
        assert!(
            engine
                .memo_reads(&path("a"), QueryKind::Signature)
                .unwrap()
                .iter()
                .any(|(key, _)| *key
                    == hirsel_query_products::reads::ReadKey::DeclarationCandidates(
                        DefId::intern(&path("a"))
                    ))
        );
        // The first candidate is byte-identical. Only the later candidate's
        // header changes and its position moves behind an unrelated item.
        let duplicate = engine
            .sources
            .add(
                "other.hir",
                "fn unrelated () { 3 }\nfn a (changed) { 2 }\n".into(),
            )
            .unwrap();
        engine.parse_file(duplicate, &default_context()).unwrap();
        let current = diagnose(&mut engine);
        assert_ne!(current, old);
        let expected = engine.definition_items(&path("a")).unwrap()[1]
            .item
            .origin
            .span;
        assert_eq!(current, expected);
        assert_eq!(engine.sources.location(current).unwrap().line, 2);
        assert_eq!(engine.recompute_count(&path("a"), QueryKind::Signature), 2);
        assert_eq!(diagnose(&mut engine), current);
        engine.environment.interface_version += 1;
        assert_eq!(diagnose(&mut engine), current);
        assert_eq!(engine.recompute_count(&path("a"), QueryKind::Signature), 3);
    });
}

/// The batch API is the same memo computation at every worker count, including
/// per-input failures, typed receipts, reversed publication/input order and hits.
#[test]
fn frozen_frontier_preserves_reads_failures_and_warm_hits() {
    with_compiler_stack(|| {
        for jobs in [1, 2, 8] {
            for reject in [
                None,
                Some("resolve"),
                Some("signature"),
                Some("check"),
                Some("modes"),
            ] {
                let (mut serial, _) = make_engine(reject, false);
                let (mut parallel, _) = make_engine_with_store(reject, false);
                let text = "fn a () { 1 }\nfn b () { 2 }\n";
                parse(&mut serial, text);
                parse(&mut parallel, text);
                let paths = vec![path("b"), path("a"), path("b")];
                let expected: Vec<_> = paths
                    .iter()
                    .map(|path| serial.mode_definition(path))
                    .collect();
                let mut front = admitted_front(&mut parallel, &paths, jobs);
                let results = front.request(&paths);
                for ((path, expected), actual) in paths.iter().zip(expected).zip(results) {
                    let actual = actual.outcome;
                    assert_eq!(&actual.definition, path);
                    let phase = match reject {
                        Some("resolve") => QueryKind::Resolve,
                        Some("signature") if path.0.name == "b" => QueryKind::Resolve,
                        Some("signature") => QueryKind::Signature,
                        Some("check") => QueryKind::Check,
                        _ => QueryKind::Modes,
                    };
                    assert_eq!(actual.phase, Some(phase));
                    match (expected, actual.result) {
                        (Ok(a), Ok(b)) => {
                            assert_eq!(a.dependencies, b.dependencies);
                            assert_eq!(a.diagnostics.len(), b.diagnostics.len());
                            assert_eq!(
                                a.value.typed.checked.resolution,
                                b.value.typed.checked.resolution
                            );
                        }
                        (Err(QueryError::Diagnostics(a)), Err(QueryError::Diagnostics(b))) => {
                            assert_eq!(a.len(), b.len());
                            for (a, b) in a.iter().zip(&b) {
                                assert_eq!((&a.rule, &a.message), (&b.rule, &b.message));
                                let a = serial.sources.resolve(a.primary);
                                let b = parallel.sources.resolve(b.primary);
                                assert_eq!((a.start, a.end), (b.start, b.end));
                            }
                        }
                        (a, b) => panic!("jobs {jobs}, reject {reject:?}: {a:?} != {b:?}"),
                    }
                    for kind in [QueryKind::Resolve, QueryKind::Check, QueryKind::Modes] {
                        let scalar_reads = serial.memo_reads(path, kind);
                        let scheduled_reads = admitted_memo_reads(&front, path, kind);
                        assert_eq!(
                            scalar_reads,
                            scheduled_reads.as_deref(),
                            "jobs {jobs}, reject {reject:?}, {kind:?}, {path:?}"
                        );
                    }
                }
                let before: Vec<_> = paths
                    .iter()
                    .flat_map(|p| {
                        [QueryKind::Resolve, QueryKind::Check, QueryKind::Modes]
                            .map(|k| front.queries.recompute_count(p, k))
                    })
                    .collect();
                let _ = front.request(&paths);
                let after: Vec<_> = paths
                    .iter()
                    .flat_map(|p| {
                        [QueryKind::Resolve, QueryKind::Check, QueryKind::Modes]
                            .map(|k| front.queries.recompute_count(p, k))
                    })
                    .collect();
                assert_eq!(before, after, "warm jobs {jobs}, reject {reject:?}");
            }
        }
    });
}

#[test]
fn frozen_frontier_reports_each_missing_phase_and_retries_it() {
    with_compiler_stack(|| {
        for jobs in [1, 2, 8] {
            for missing in [
                QueryKind::Resolve,
                QueryKind::Signature,
                QueryKind::Check,
                QueryKind::Modes,
            ] {
                let paths = vec![path("b"), path("a")];
                let mut attempts = vec![0; paths.len()];
                for _ in 0..2 {
                    let (mut engine, _) = make_engine_with_store(None, false);
                    match missing {
                        QueryKind::Resolve => engine.adapters.resolver = None,
                        QueryKind::Signature => engine.adapters.signature = None,
                        QueryKind::Check => engine.adapters.checker = None,
                        QueryKind::Modes => engine.adapters.modes = None,
                        _ => unreachable!(),
                    }
                    parse(&mut engine, "fn a () { 1 }\nfn b () { 2 }\n");

                    let mut front = admitted_front(&mut engine, &paths, jobs);
                    for result in front.request(&paths) {
                        assert_eq!(result.outcome.phase, Some(missing));
                        assert!(matches!(
                            result.outcome.result,
                            Err(QueryError::NotImplemented)
                        ));
                        assert!(
                            admitted_memo_reads(&front, &result.outcome.definition, missing)
                                .is_none(),
                            "missing {missing:?} memo for {} with jobs={jobs}",
                            result.outcome.definition
                        );
                    }
                    for (attempts, path) in attempts.iter_mut().zip(&paths) {
                        *attempts += if missing == QueryKind::Signature {
                            u64::try_from(engine.recompute_count(path, missing)).unwrap()
                        } else {
                            front.queries.recompute_count(path, missing)
                        };
                    }
                }
                for count in attempts {
                    assert!(count >= 2);
                }
            }
        }
    });
}

#[test]
fn frozen_frontier_retries_operational_headers_only_in_the_next_snapshot() {
    struct Flaky {
        original: Arc<dyn SignaturePhase>,
        calls: std::sync::atomic::AtomicU32,
    }
    impl SignaturePhase for Flaky {
        fn signature(
            &self,
            t: &TypeInterner,
            m: &TypeMemo,
            syntax: Arc<ItemSyntax>,
            d: &Declarations,
            p: &mut PhaseProducts,
            provider: &mut dyn SignatureProvider,
        ) -> Result<QueryOutput<Signature>, QueryError> {
            if syntax.item.definition.0.name == "a"
                && self
                    .calls
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    == 0
            {
                return Err(QueryError::Operational(OperationalFailure {
                    operation: "snapshot signature".into(),
                    message: "temporarily unavailable".into(),
                }));
            }
            self.original.signature(t, m, syntax, d, p, provider)
        }
    }
    with_compiler_stack(|| {
        for jobs in [1, 2, 8] {
            let (mut engine, _) = make_engine_with_store(None, false);
            let adapter = Arc::new(Flaky {
                original: engine.adapters.signature.take().unwrap(),
                calls: std::sync::atomic::AtomicU32::new(0),
            });
            engine.adapters.signature = Some(adapter.clone());
            parse(&mut engine, "fn a () { 1 }\nfn b () { 2 }\n");
            let paths = vec![path("b"), path("a")];
            let mut front = admitted_front(&mut engine, &paths, jobs);
            let results = front.request(&paths);
            assert!(
                results
                    .iter()
                    .all(|r| matches!(r.outcome.result, Err(QueryError::Operational(_))))
            );
            assert_eq!(
                adapter.calls.load(std::sync::atomic::Ordering::Relaxed),
                1,
                "no post-freeze retry"
            );
            for p in &paths {
                assert!(admitted_memo_reads(&front, p, QueryKind::Modes).is_none());
            }
            drop(front);
            let (mut engine, _) = make_engine_with_store(None, false);
            engine.adapters.signature = Some(adapter.clone());
            parse(&mut engine, "fn a () { 1 }\nfn b () { 2 }\n");
            engine.item_signature(&path("a")).unwrap();
            let mut front = admitted_front(&mut engine, &paths, jobs);
            let results = front.request(&paths);
            assert!(results.iter().all(|r| r.outcome.result.is_ok()));
            assert_eq!(
                adapter.calls.load(std::sync::atomic::Ordering::Relaxed),
                2,
                "jobs={jobs}"
            );
        }
    });
}

#[test]
fn compact_release_does_not_retry_an_unavailable_phase_through_an_old_checked_body() {
    with_compiler_stack(|| {
        let (mut engine, _) = make_engine_with_store(None, false);
        parse(&mut engine, "fn a () { 1 }\n");
        engine.mode_definition(&path("a")).unwrap();
        engine.adapters.resolver = None;
        parse(&mut engine, "fn a () { 2 }\n");
        let definitions = vec![path("a")];
        let mut front = admitted_front(&mut engine, &definitions, 2);
        let outputs = front.request(&definitions);
        assert!(matches!(
            outputs[0].outcome.result,
            Err(QueryError::NotImplemented)
        ));
        assert_eq!(outputs[0].outcome.phase, Some(QueryKind::Resolve));
        assert_eq!(
            front
                .queries
                .recompute_count(&path("a"), QueryKind::Resolve),
            1,
            "the new owner attempts resolution once"
        );
    });
}

mod read_names_wire {
    use hirsel_syntax::{ContentHash, DefPath, FileContext};
    use hirsel_types::*;
    #[derive(serde::Serialize, Debug)]
    enum WrittenRead {
        ProviderLookup {
            context: FileContext,
            package: Option<hirsel_syntax::PackageId>,
            segments: Vec<String>,
            output: LookupOutput,
        },
        Lookup {
            context: FileContext,
            package: Option<hirsel_syntax::PackageId>,
            segments: Vec<String>,
            manifest: ContentHash,
            result: Result<DefPath, LookupError>,
        },
        ModuleHead {
            context: FileContext,
            head: String,
            manifest: ContentHash,
            exposed: bool,
        },
    }

    #[test]
    fn interned_read_names_preserve_structural_json_bincode_and_lookup_failures() {
        let path = DefPath::core("read_names.value");
        let context = FileContext {
            package: path.0.package.clone(),
            modules: vec!["nested".into()],
        };
        let segments = vec!["nested".into(), "value".into()];
        let manifest = ContentHash::of_bytes(b"selected manifest");
        let output = LookupOutput {
            manifest,
            result: Ok(path.clone()),
        };
        let mut written = vec![WrittenRead::ProviderLookup {
            context: context.clone(),
            package: None,
            segments: segments.clone(),
            output,
        }];
        for result in [
            Ok(path.clone()),
            Err(LookupError::Missing),
            Err(LookupError::MissingMember(vec![path.clone()])),
            Err(LookupError::Invisible(path.clone())),
        ] {
            written.push(WrittenRead::Lookup {
                context: context.clone(),
                package: Some(path.0.package.clone()),
                segments: segments.clone(),
                manifest,
                result,
            });
        }
        written.push(WrittenRead::ModuleHead {
            context,
            head: "nested".into(),
            manifest,
            exposed: false,
        });
        for read in written {
            let bytes = bincode::serialize(&read).unwrap();
            let compact: DependencyRead = bincode::deserialize(&bytes).unwrap();
            assert_eq!(bincode::serialize(&compact).unwrap(), bytes);
            assert_eq!(format!("{compact:?}"), format!("{read:?}"));
            assert_eq!(
                serde_json::to_value(&compact).unwrap(),
                serde_json::to_value(&read).unwrap()
            );
            let shared: SharedRead = bincode::deserialize(&bytes).unwrap();
            assert_eq!(bincode::serialize(&shared).unwrap(), bytes);
            assert_eq!(format!("{shared:?}"), format!("{read:?}"));
        }
    }
}

/// This fixture supplies checked call edges through the ordinary Front producer.
/// Growth solving, query identities, stores and scheduler are production code.
struct SccChecker {
    base: Arc<Fake>,
    calls: std::collections::BTreeMap<String, Vec<String>>,
}
impl CheckerPhase for SccChecker {
    fn check(
        &self,
        types: &TypeInterner,
        memo: &TypeMemo,
        syntax: Arc<ItemSyntax>,
        resolution: Resolution,
        signature: &Signature,
        provider: &mut dyn SignatureProvider,
    ) -> Result<QueryOutput<hirsel_query_products::TypedBody>, QueryError> {
        let mut output = self
            .base
            .check(types, memo, syntax, resolution, signature, provider)?;
        output.value.checked.instantiations = self
            .calls
            .get(&output.value.syntax.item.definition.0.name)
            .into_iter()
            .flatten()
            .map(|name| Instantiation {
                expression: ExprId(0),
                callee: path(name),
                arguments: Vec::new(),
            })
            .collect();
        Ok(output)
    }
}
fn scheduled_scc_run(
    text: &str,
    calls: &[(&str, &[&str])],
    store: Arc<dyn hirsel_engine_store::ResultStore>,
    jobs: usize,
    output_bytes: u64,
) -> (FrontGrowthOutput, u64, u64) {
    scheduled_scc_run_with_capacity(
        text,
        calls,
        store,
        jobs,
        output_bytes,
        None,
        hirsel_engine_admission::ProcessBudget::new(1 << 30),
    )
}
fn scheduled_scc_run_with_capacity(
    text: &str,
    calls: &[(&str, &[&str])],
    store: Arc<dyn hirsel_engine_store::ResultStore>,
    jobs: usize,
    output_bytes: u64,
    capacity: Option<usize>,
    budget: Arc<hirsel_engine_admission::ProcessBudget>,
) -> (FrontGrowthOutput, u64, u64) {
    let fake = Arc::new(Fake {
        events: Default::default(),
        reject: None,
        missing_lookup: false,
    });
    let adapters = PhaseAdapters {
        lexer: Some(fake.clone()),
        parser: Some(fake.clone()),
        resolver: Some(fake.clone()),
        signature: Some(fake.clone()),
        modes: Some(fake.clone()),
        lowering: Some(fake.clone()),
        checker: Some(Arc::new(SccChecker {
            base: fake,
            calls: calls
                .iter()
                .map(|(name, calls)| ((*name).into(), calls.iter().map(|s| (*s).into()).collect()))
                .collect(),
        })),
        ..Default::default()
    };
    let operation = store.operation_scope();
    let mut engine = QueryEngine::new(default_environment(), adapters).with_result_store(store);
    parse(&mut engine, text);
    let definitions = engine.front_source_roots();
    let weight = MemoryWeight {
        retained: 64 << 10,
        scratch: 8 << 20,
        stack: 0,
    };
    let errors = runtime_errors();
    let front = engine
        .front_body_queries(&definitions, weight, &budget, errors)
        .unwrap();
    let inventory = FrontGrowthQueries::input_bytes(&definitions, weight).unwrap();
    let growth = engine
        .front_growth_queries(
            &path("main"),
            front.clone(),
            weight,
            &hirsel_engine_admission::ProcessBudget::new(inventory + output_bytes),
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
        .register(FRONT_GROWTH_KIND, growth.factory())
        .unwrap();
    let request = runtime.request(&[growth.query()]);
    let products = runtime.wait(request).unwrap_or_else(|e| {
        panic!(
            "growth failed {e:?}, solves {}, memberships {}, runtime {:?}",
            growth.component_solves(),
            growth.membership_solves(),
            runtime.stats()
        )
    });
    let output = growth.outcome(products[0].clone()).unwrap();
    assert!(output.diagnostics.is_empty());
    // Membership, each SCC and table-key reduction read the same retained
    // Front products. Each definition is claimed once, plus one task per SCC,
    // the membership task and the aggregate Growth root.
    assert_eq!(
        runtime.stats().claims as usize,
        definitions.len() + output.facets.components.len() + 2
    );
    let solves = growth.component_solves();
    let memberships = growth.membership_solves();
    let streamed_peak = runtime.stats().peak_live_nodes;
    if capacity.is_some() {
        println!(
            "streamed SCC reduction complete: components={} peak_live_nodes={streamed_peak}",
            output.facets.components.len()
        );
    }
    // Retire and reopen the root, membership and every SCC from their admitted
    // owners, including with persistence disabled. No source solve repeats.
    runtime.evict_to(0);
    for (index, query) in
        std::iter::once(growth.query())
            .chain(std::iter::once(hirsel_engine_runtime::QueryId {
                kind: FRONT_GROWTH_KIND,
                address: output.facets.membership.address,
            }))
            .chain(output.facets.components.iter().map(|component| {
                hirsel_engine_runtime::QueryId {
                    kind: FRONT_GROWTH_KIND,
                    address: component.product.address,
                }
            }))
            .enumerate()
    {
        if capacity.is_some() && index % 1024 == 0 {
            println!("streamed SCC reopen progress: {index}");
        }
        let request = runtime.request(&[query]);
        let products = runtime.wait(request).unwrap();
        assert_eq!(products[0].address, query.address);
        runtime.evict_to(0);
    }
    assert_eq!(
        (growth.component_solves(), growth.membership_solves()),
        (solves, memberships)
    );
    if let Some(capacity) = capacity {
        assert!(runtime.stats().peak_live_nodes <= capacity);
        println!(
            "streamed_sccs={} inventory={capacity} stream_peak_live_nodes={streamed_peak} peak_live_nodes={} claims={}",
            output.facets.components.len(),
            runtime.stats().peak_live_nodes,
            runtime.stats().claims
        );
    }
    runtime.shutdown();
    operation.finish().unwrap();
    (output, solves, memberships)
}
fn scc_products(output: &FrontGrowthOutput) -> Vec<SourceGrowthIdentity> {
    output.facets.components.iter().map(|c| c.product).collect()
}
#[test]
fn scheduled_scc_equal_edit_solves_no_downstream_or_unaffected_component() {
    let store = Arc::new(MemStore::default());
    let text = "fn a {b}\nfn b {a}\nfn caller {a}\nfn main {caller}\n";
    let calls: &[(&str, &[&str])] = &[
        ("a", &["b"]),
        ("b", &["a"]),
        ("caller", &["a"]),
        ("main", &["caller"]),
    ];
    let (first, solves, memberships) = scheduled_scc_run(text, calls, store.clone(), 1, 8 << 20);
    assert_eq!((solves, memberships), (3, 1));
    let changed_text = text.replace("{b}", "{a;b}");
    let changed_calls: &[(&str, &[&str])] = &[
        ("a", &["a", "b"]),
        ("b", &["a"]),
        ("caller", &["a"]),
        ("main", &["caller"]),
    ];
    let (changed, solves, memberships) =
        scheduled_scc_run(&changed_text, changed_calls, store, 2, 8 << 20);
    assert_eq!(
        solves, 1,
        "only the edited SCC is solved; downstream and unrelated SCCs reuse products"
    );
    assert_eq!(
        memberships, 1,
        "a changed edge rebuilds membership even when its partition is equal"
    );
    assert_eq!(
        first.facets.membership.output,
        changed.facets.membership.output
    );
    for (a, b) in first
        .facets
        .components
        .iter()
        .zip(&changed.facets.components)
    {
        assert_eq!(a.product.output, b.product.output);
        if a.members.iter().any(|id| id.path().0.name == "a") {
            assert_ne!(a.product.action, b.product.action);
        } else {
            assert_eq!(a.product, b.product);
        }
    }
    let (cold, _, _) = scheduled_scc_run(
        &changed_text,
        changed_calls,
        Arc::new(MemStore::default()),
        1,
        8 << 20,
    );
    assert_eq!(scc_products(&changed), scc_products(&cold));
}
#[test]
fn scheduled_scc_split_merge_and_durable_reopen_agree_with_cold_solve() {
    let directory = tempfile::tempdir().unwrap();
    let open = || {
        Arc::new(hirsel_engine_store::DirStore::new(
            directory.path().to_path_buf(),
        )) as Arc<dyn hirsel_engine_store::ResultStore>
    };
    let run = |text: &str, calls: &[(&str, &[&str])], jobs| {
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 30);
        // Directory retention is disk-backed. Fund live transport from the
        // same process budget as the Front, growth and runtime reservations.
        let store = Arc::new(
            hirsel_engine_store::RequestStore::with_scratch(
                open(),
                budget.reserve(8 << 20).unwrap(),
                &std::env::temp_dir(),
                1 << 30,
            )
            .unwrap(),
        );
        scheduled_scc_run_with_capacity(text, calls, store, jobs, 8 << 20, None, budget)
    };
    let cases: &[(&str, &[(&str, &[&str])])] = &[
        (
            "fn a {b}\nfn b {a}\nfn main {a}\n",
            &[("a", &["b"]), ("b", &["a"]), ("main", &["a"])],
        ),
        (
            "fn a {b}\nfn b {}\nfn main {a}\n",
            &[("a", &["b"]), ("main", &["a"])],
        ),
        (
            "fn a {b}\nfn b {a}\nfn main {a}\n",
            &[("a", &["b"]), ("b", &["a"]), ("main", &["a"])],
        ),
    ];
    let mut previous_membership = None;
    let mut seen_inputs = std::collections::BTreeSet::new();
    for (text, calls) in cases {
        let (resident, _, memberships) = run(text, calls, 1);
        // The final merge revisits the first case's exact text and call edges.
        // Its durable membership already exists; only distinct inputs solve.
        assert_eq!(memberships, u64::from(seen_inputs.insert((*text, *calls))));
        if let Some(previous) = previous_membership {
            assert_ne!(resident.facets.membership.output, previous);
        }
        previous_membership = Some(resident.facets.membership.output);
        let cached_membership = open()
            .lookup_product(resident.facets.membership.address)
            .expect("membership association is durable");
        assert_eq!(
            cached_membership.action, resident.facets.membership.action,
            "case {text}"
        );
        assert_eq!(
            cached_membership.reference.domain,
            hirsel_engine::products::LIVE_SOURCE_MEMBERSHIP
        );
        let (cold, _, _) =
            scheduled_scc_run(text, calls, Arc::new(MemStore::default()), 1, 8 << 20);
        assert_eq!(scc_products(&resident), scc_products(&cold));
        let (reopened, solves, memberships) = run(text, calls, 2);
        assert_eq!(
            resident.facets.membership.action, reopened.facets.membership.action,
            "membership observes the same checked inputs after reopening"
        );
        assert_eq!(
            (solves, memberships),
            (0, 0),
            "a new provider and engine reopen each independently addressed product"
        );
        assert_eq!(scc_products(&resident), scc_products(&reopened));
        assert_eq!(resident.reads.as_ref(), reopened.reads.as_ref());
    }
}
#[test]
fn scheduled_scc_products_fit_the_existing_complete_growth_output_grant() {
    let mut text = String::new();
    for i in 0..256 {
        text.push_str(&format!("fn f{i} {{}}\n"));
    }
    text.push_str("fn main {}\n");
    let (output, solves, memberships) =
        scheduled_scc_run(&text, &[], Arc::new(MemStore::default()), 1, 106_172);
    assert_eq!(output.facets.components.len(), 257);
    assert_eq!((solves, memberships), (257, 1));
}

#[test]
fn scheduled_scc_stream_exceeds_8192_with_store_off_and_unchanged_inventory() {
    let mut text = String::new();
    for i in 0..8193 {
        text.push_str(&format!("fn f{i} {{}}\n"));
    }
    text.push_str("fn main {}\n");
    let transport = hirsel_engine_admission::ProcessBudget::new(64 << 20);
    let store = Arc::new(
        hirsel_engine_store::RequestStore::with_scratch(
            Arc::new(hirsel_engine_store::NoneStore),
            transport.reserve(64 << 20).unwrap(),
            &std::env::temp_dir(),
            1 << 30,
        )
        .unwrap(),
    );
    assert_eq!(
        hirsel_engine_store::ResultStore::persistence(&*store),
        hirsel_engine_store::Persistence::Disabled
    );
    let (output, solves, memberships) = scheduled_scc_run_with_capacity(
        &text,
        &[],
        store,
        1,
        8 << 20,
        Some(8192),
        hirsel_engine_admission::ProcessBudget::new(1 << 30),
    );
    assert_eq!(output.facets.components.len(), 8194);
    assert_eq!((solves, memberships), (8194, 1));
}

#[test]
fn scheduled_scc_store_off_edit_split_merge_and_reopen_agree_with_cold_solve() {
    let transport = hirsel_engine_admission::ProcessBudget::new(8 << 20);
    let store = Arc::new(
        hirsel_engine_store::RequestStore::with_scratch(
            Arc::new(hirsel_engine_store::NoneStore),
            transport.reserve(8 << 20).unwrap(),
            &std::env::temp_dir(),
            1 << 30,
        )
        .unwrap(),
    );
    let cases: &[(&str, &[(&str, &[&str])])] = &[
        (
            "fn a {b}\nfn b {a}\nfn main {a}\n",
            &[("a", &["b"]), ("b", &["a"]), ("main", &["a"])],
        ),
        (
            "fn a {b; b}\nfn b {a}\nfn main {a}\n",
            &[("a", &["b"]), ("b", &["a"]), ("main", &["a"])],
        ),
        (
            "fn a {b}\nfn b {}\nfn main {a}\n",
            &[("a", &["b"]), ("main", &["a"])],
        ),
        (
            "fn a {b}\nfn b {a}\nfn main {a}\n",
            &[("a", &["b"]), ("b", &["a"]), ("main", &["a"])],
        ),
    ];
    for (text, calls) in cases {
        let (resident, _, _) = scheduled_scc_run(text, calls, store.clone(), 1, 8 << 20);
        let (cold, _, _) =
            scheduled_scc_run(text, calls, Arc::new(MemStore::default()), 1, 8 << 20);
        assert_eq!(scc_products(&resident), scc_products(&cold));
        assert_eq!(*resident.reads, *cold.reads);
    }
}
