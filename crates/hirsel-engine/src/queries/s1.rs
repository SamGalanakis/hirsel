//! Task-local dependency/read bridge for S1 evaluation. Shared sources expose
//! immutable inputs; the requestor owns its coordinator stack and read frames.
//! Like rustc's body tasks (rustc_hir_analysis/src/lib.rs:166-220), a dependency
//! returns products for its caller to merge, including when evaluation fails.
use super::*;
use hirsel_tir::{DefinitionRef, MaterializedRef, TirProvider};
use hirsel_types::GenericArgument;

/// A dependency's complete read record survives failure and cache hits alike.
/// The source owns depth admission and registration requests; this bridge only
/// transports its result and evidence to the enclosing evaluation.
pub(crate) struct DependencyProduct<T> {
    pub(crate) result: Result<T, QueryError>,
    pub(crate) reads: Reads,
    pub(crate) successor: Option<scheduled_lower::SuccessorCapture>,
}

/// A worker's request interface. Implementations keep mutable request state
/// local, over immutable shared phase/snapshot handles. No engine is required
/// by the evaluation provider, and no lock spans interpreter execution.
pub(crate) trait DependencySource {
    fn definition(&mut self, path: &DefPath) -> DependencyProduct<DefinitionRef>;
    fn constant(&mut self, path: &DefPath) -> DependencyProduct<MaterializedRef>;
    fn constant_instance(
        &mut self,
        path: &DefPath,
        arguments: &[u64],
    ) -> DependencyProduct<MaterializedRef>;
    fn instance_body(
        &mut self,
        path: &DefPath,
        arguments: &[GenericArgument],
    ) -> DependencyProduct<Option<DefPath>>;
}

/// An immutable private artifact after V admission. It can execute with any
/// task-local dependency source and a worker's runtime; it owns no session.
/// Lowering stays in EvaluationInput::lower, so a failed lower never forces
/// snapshot construction or runtime preparation in the serial delegate.
pub(crate) struct ValidatedEvaluation {
    definition: DefinitionRef,
    ty: hirsel_types::TypeId,
    site: Span,
    acceptance: Option<hirsel_validate::AcceptedArtifact>,
    capture: Option<Arc<NamespaceCapture>>,
    context: Vec<(hirsel_syntax::PackageId, ContentHash)>,
}

pub(crate) struct EvaluationOutcome {
    pub(crate) acceptance: Option<hirsel_validate::AcceptedArtifact>,
    pub(crate) value: DependencyProduct<(hirsel_interp::Value, hirsel_types::TypeId)>,
    pub(crate) limit: Option<hirsel_tir::ToolingLimit>,
}
impl ValidatedEvaluation {
    pub(crate) fn acceptance(&self) -> Option<&hirsel_validate::AcceptedArtifact> {
        self.acceptance.as_ref()
    }

    pub(crate) fn recorded_in(
        prepared: crate::comptime::PreparedEvaluation,
        capture: Arc<NamespaceCapture>,
        verify: Verify,
        site: Span,
    ) -> DependencyProduct<Self> {
        let mut product = Self::recorded(prepared, &capture.snapshot, verify, site);
        if let Ok(evaluation) = &mut product.result {
            evaluation.capture = Some(capture);
        }
        product
    }
    /// A worker may publish across later registration epochs. The package's
    /// complete base V universe identifies every input of the private overlay,
    /// including formerly valid declarations whose overlay verdict can change.
    /// Record before admission so failures keep the same proof as successes.
    pub(crate) fn recorded(
        prepared: crate::comptime::PreparedEvaluation,
        snapshot: &SignatureSnapshot,
        verify: Verify,
        site: Span,
    ) -> DependencyProduct<Self> {
        let mut reads = ReadsBuilder::new();
        // A refused universe read refuses the evaluation it would identify.
        let universe = if verify == Verify::On {
            FrozenProvider::new(
                snapshot,
                prepared.definition.path.path().0.package.clone(),
                &mut reads,
            )
            .validation_universe()
            .map(drop)
        } else {
            Ok(())
        };
        let reads = reads.finish().expect("one immutable validation universe");
        let result = universe.and_then(|()| {
            let mut evaluation = Self::new(prepared, Some(snapshot), verify, site)?;
            evaluation.context = reads
                .iter()
                .filter_map(|(key, hash)| match key {
                    ReadKey::ValidationUniverse(package) => Some((package.id(), *hash)),
                    _ => None,
                })
                .collect();
            Ok(evaluation)
        });
        DependencyProduct {
            successor: None,
            result,
            reads,
        }
    }
    pub(crate) fn new(
        prepared: crate::comptime::PreparedEvaluation,
        snapshot: Option<&SignatureSnapshot>,
        verify: Verify,
        site: Span,
    ) -> Result<Self, QueryError> {
        let acceptance = if verify == Verify::On {
            crate::comptime::validate_evaluation(
                snapshot.expect("verification requires a frozen snapshot"),
                &prepared.definition,
                prepared.signature,
                verify,
                prepared.origin.as_ref(),
            )?
        } else {
            None
        };
        Ok(Self {
            acceptance,
            definition: Arc::new(prepared.definition),
            ty: prepared.ty,
            site,
            capture: None,
            context: Vec::new(),
        })
    }
    /// The caller clones RuntimeTypes once per worker. Nested evaluations
    /// reuse that runtime so Serialization's recorder stack remains nested.
    /// The caller clones RuntimeTypes once per worker. Nested evaluations
    /// reuse that runtime so Serialization's recorder stack remains nested.
    pub(crate) fn run(
        self,
        sources: &SourceMap,
        limits: crate::ComptimeLimits,
        runtime: &hirsel_interp::aggregate::RuntimeTypes,
        source: &mut impl DependencySource,
    ) -> EvaluationOutcome {
        let configuration = RunConfiguration {
            entry: self.definition.path.path().clone(),
            hash_seed: 0,
            inputs: vec![],
        };
        let mut provider = EvaluationProvider::new(self.definition, source);
        provider.capture = self.capture;
        provider.execution = self.acceptance.as_ref().and_then(|acceptance| {
            hirsel_validate::ExecutionReceipt::compose(
                &self.context,
                std::slice::from_ref(acceptance),
            )
        });
        let product = crate::comptime::EvaluationTask {
            sources,
            configuration,
            limits,
            runtime,
            site: self.site,
            ty: self.ty,
        }
        .run(&mut provider);
        let mut reads = ReadsBuilder::new();
        let successor = provider.successor.clone();
        let acceptance = provider
            .execution
            .as_ref()
            .and_then(|execution| execution.artifacts().first())
            .cloned()
            .or(self.acceptance);
        let recorded = provider.finish();
        if let Ok(recorded) = &recorded {
            reads.merge(recorded);
        }
        for (path, digest) in product.reflected {
            reads.record(ReadKey::Representation(DefId::intern(&path)), digest);
        }
        let reads = reads.finish();
        let (result, reads) = match (recorded, reads) {
            (Ok(_), Ok(reads)) => (product.result, reads),
            _ => (Err(QueryError::NotImplemented), Reads::empty()),
        };
        EvaluationOutcome {
            acceptance,
            value: DependencyProduct {
                successor,
                result,
                reads,
            },
            limit: product.limit,
        }
    }
}

/// A private evaluation root plus its task-local dependency requestor. Every
/// request merges its complete reads before returning its success or failure.
/// Constants remain lazy: interpreting a root never enumerates other constants.
pub(crate) struct EvaluationProvider<'a, S> {
    root: DefinitionRef,
    source: &'a mut S,
    reads: ReadsBuilder,
    capture: Option<Arc<NamespaceCapture>>,
    successor: Option<scheduled_lower::SuccessorCapture>,
    execution: Option<hirsel_validate::ExecutionReceipt>,
}
impl<'a, S> EvaluationProvider<'a, S> {
    pub(crate) fn new(root: DefinitionRef, source: &'a mut S) -> Self {
        Self {
            root,
            source,
            reads: ReadsBuilder::new(),
            capture: None,
            successor: None,
            execution: None,
        }
    }
    fn merge<T>(&mut self, product: DependencyProduct<T>) -> Result<T, QueryError> {
        if let Some(successor) = product.successor {
            if !self
                .capture
                .as_ref()
                .is_some_and(|capture| successor.follows(capture))
            {
                return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "private evaluation capture join".into(),
                    message: "dependency successor does not descend from the executing capture"
                        .into(),
                }));
            }
            let reads = self
                .reads
                .clone()
                .finish()
                .map_err(|_| QueryError::NotImplemented)?;
            let (reads, execution) =
                successor.revalidate(self.root.path.path(), &reads, self.execution.as_ref())?;
            if self.execution.is_none() && !self.contextless() {
                return Err(QueryError::NotImplemented);
            }
            self.reads = ReadsBuilder::new();
            self.reads.merge(&reads);
            self.execution = execution;
            self.capture = Some(successor.namespace().clone());
            self.successor = Some(successor);
        }
        self.reads.merge(&product.reads);
        product.result
    }
    fn contextless(&self) -> bool {
        self.capture
            .as_ref()
            .is_some_and(|capture| capture.verification() == Verify::Off)
    }
    pub(crate) fn finish(self) -> Result<Reads, QueryError> {
        self.reads.finish().map_err(|_| QueryError::NotImplemented)
    }
}
impl<S: DependencySource> TirProvider for EvaluationProvider<'_, S> {
    fn definition(&mut self, path: &DefPath) -> Result<DefinitionRef, QueryError> {
        if path == self.root.path.path() {
            return Ok(self.root.clone());
        }
        let product = self.source.definition(path);
        self.merge(product)
    }
    fn constant(&mut self, path: &DefPath) -> Result<MaterializedRef, QueryError> {
        let product = self.source.constant(path);
        self.merge(product)
    }
    fn constant_instance(
        &mut self,
        path: &DefPath,
        arguments: &[u64],
    ) -> Result<MaterializedRef, QueryError> {
        let product = self.source.constant_instance(path, arguments);
        self.merge(product)
    }
    fn instance_body(
        &mut self,
        path: &DefPath,
        arguments: &[GenericArgument],
    ) -> Result<Option<DefPath>, QueryError> {
        let product = self.source.instance_body(path, arguments);
        self.merge(product)
    }
    fn constant_definitions(&mut self) -> Result<Vec<DefPath>, QueryError> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_query_products::reads::Revision;
    use hirsel_tir::BodyId;

    struct Failed {
        reads: Reads,
        calls: usize,
    }
    impl Failed {
        fn product<T>(&mut self) -> DependencyProduct<T> {
            self.calls += 1;
            DependencyProduct {
                successor: None,
                result: Err(QueryError::NotImplemented),
                reads: self.reads.clone(),
            }
        }
    }
    impl DependencySource for Failed {
        fn definition(&mut self, _: &DefPath) -> DependencyProduct<DefinitionRef> {
            self.product()
        }
        fn constant(&mut self, _: &DefPath) -> DependencyProduct<MaterializedRef> {
            self.product()
        }
        fn constant_instance(
            &mut self,
            _: &DefPath,
            _: &[u64],
        ) -> DependencyProduct<MaterializedRef> {
            self.product()
        }
        fn instance_body(
            &mut self,
            _: &DefPath,
            _: &[GenericArgument],
        ) -> DependencyProduct<Option<DefPath>> {
            self.product()
        }
    }

    /// A real private constant artifact that reads a dependency through the
    /// interpreter. V admission and the returned type must remain independent
    /// of whether the provider later succeeds or fails.
    pub(super) fn reading_artifact(
        types: &hirsel_types::TypeInterner,
        root: &DefPath,
        dependency: &DefPath,
    ) -> crate::comptime::PreparedEvaluation {
        use hirsel_tir::*;
        let ty = types.intern(hirsel_types::Type::unit());
        let site = Span::default();
        let body = Body {
            arena_scope_grant: None,
            scoped_parameters: Vec::new(),
            parameter_scope: hirsel_syntax::LexicalOwner::first(root)
                .site(hirsel_types::NameSite::Item),
            locals: vec![Local {
                ty,
                storage: Storage::Temporary,
                scope: ScopeId(0),
                span: site,
            }],
            places: vec![Place {
                loan: None,
                binding: None,
                root: LocalId(0),
                projection: vec![].into(),
                ty,
            }],
            scopes: vec![Scope {
                parent: None,
                span: site,
            }],
            blocks: vec![Block {
                instructions: vec![
                    Instruction {
                        kind: InstructionKind::StorageLive(LocalId(0)),
                        span: site,
                        on_failure: CleanupId(0),
                    },
                    Instruction {
                        kind: InstructionKind::Assign {
                            destination: PlaceId(0),
                            value: RvalueId(0),
                        },
                        span: site,
                        on_failure: CleanupId(0),
                    },
                ],
                terminator: Terminator::Return {
                    value: Operand {
                        place: PlaceId(0),
                        usage: hirsel_types::UseKind::Take,
                    },
                    cleanup: CleanupId(0),
                },
                span: site,
            }],
            rvalues: vec![Rvalue::ReadConstant {
                definition: dependency.clone(),
                arguments: vec![],
            }],
            cleanups: vec![Cleanup::default()],
            cleanup_nodes: vec![],
            entry: BlockId(0),
            parameters: vec![],
            result: ty,
            borrow_cursors: vec![],
            tail_sites: vec![],
            matches: vec![],
        };
        let mut definition = Definition {
            caller_bodies: vec![],
            protocol_associations: vec![],
            types: Arc::from([]),
            path: DefId::intern(root),
            signature_hash: ContentHash::default(),
            bodies: vec![body],
            entry: BodyId(0),
            contracts: vec![],
        };
        definition.types = definition_types(types, &definition);
        crate::comptime::PreparedEvaluation {
            definition,
            origin: None,
            signature: Signature {
                definition: DefId::intern(root),
                kind: hirsel_types::SignatureKind::Const {
                    ty,
                    generics: Default::default(),
                },
                metadata: Default::default(),
                fingerprint: ContentHash::default(),
            },
            ty,
        }
    }

    struct DeferredSource<'r, 'c> {
        dependencies: &'r hirsel_engine_runtime::QueryReads<'c>,
        query: hirsel_engine_runtime::QueryId,
        types: Arc<hirsel_types::TypeInterner>,
        reads: Reads,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl DependencySource for DeferredSource<'_, '_> {
        fn definition(&mut self, _: &DefPath) -> DependencyProduct<DefinitionRef> {
            DependencyProduct {
                successor: None,
                result: Err(QueryError::NotImplemented),
                reads: Reads::empty(),
            }
        }
        fn constant(&mut self, _: &DefPath) -> DependencyProduct<MaterializedRef> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let result = self
                .dependencies
                .read(self.query)
                .map(|_| {
                    Arc::new(hirsel_tir::Materialized {
                        root: hirsel_tir::ValueNode {
                            ty: self.types.intern(hirsel_types::Type::unit()),
                            kind: hirsel_tir::ValueKind::Constant(hirsel_tir::Constant::Unit),
                        },
                    })
                })
                .map_err(|_| QueryError::NotImplemented);
            DependencyProduct {
                successor: None,
                result,
                reads: self.reads.clone(),
            }
        }
        fn constant_instance(
            &mut self,
            path: &DefPath,
            _: &[u64],
        ) -> DependencyProduct<MaterializedRef> {
            self.constant(path)
        }
        fn instance_body(
            &mut self,
            _: &DefPath,
            _: &[GenericArgument],
        ) -> DependencyProduct<Option<DefPath>> {
            DependencyProduct {
                successor: None,
                result: Ok(None),
                reads: Reads::empty(),
            }
        }
    }
    fn reference(text: &[u8]) -> hirsel_engine_store::Reference {
        hirsel_engine_store::Domain {
            kind: 0x5300_00ff,
            schema: 1,
        }
        .reference(text)
        .unwrap()
    }
    fn errors() -> hirsel_engine_runtime::RuntimeErrors {
        hirsel_engine_runtime::RuntimeErrors {
            resource: reference(b"resource"),
            protocol: reference(b"protocol"),
            cycle: reference(b"cycle"),
            panicked: reference(b"panicked"),
            cancelled: reference(b"cancelled"),
        }
    }
    fn query(kind: u32) -> hirsel_engine_runtime::QueryId {
        hirsel_engine_runtime::QueryId {
            kind,
            address: hirsel_engine_store::Address(ContentHash::of_bytes(&kind.to_le_bytes())),
        }
    }
    fn product(query: hirsel_engine_runtime::QueryId) -> hirsel_engine_runtime::ProductRef {
        hirsel_engine_runtime::test_product(
            query.address,
            hirsel_engine_store::Action(query.address.0),
            hirsel_engine_store::Domain { kind: 1, schema: 1 },
            &query.address.0.0,
        )
    }
    struct Leaf(bool);
    impl hirsel_engine_runtime::QueryFactory for Leaf {
        fn create(
            &self,
            query: hirsel_engine_runtime::QueryId,
        ) -> Result<Box<dyn hirsel_engine_runtime::QueryTask>, hirsel_engine_store::Reference>
        {
            Ok(Box::new(LeafTask {
                query,
                fail: self.0,
            }))
        }
    }
    struct LeafTask {
        query: hirsel_engine_runtime::QueryId,
        fail: bool,
    }
    impl hirsel_engine_runtime::QueryTask for LeafTask {
        fn id(&self) -> hirsel_engine_runtime::QueryId {
            self.query
        }
        fn weight(&self) -> hirsel_engine_runtime::MemoryWeight {
            Default::default()
        }
        fn run(
            &mut self,
            _: &mut dyn hirsel_engine_runtime::QueryContext,
        ) -> hirsel_engine_runtime::QueryOutcome {
            if self.fail {
                Err(errors().protocol)
            } else {
                Ok(product(self.query))
            }
        }
    }
    struct EvaluationFactory {
        types: Arc<hirsel_types::TypeInterner>,
        snapshot: Arc<SignatureSnapshot>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        fail: bool,
    }
    impl hirsel_engine_runtime::LocalQueryFactory for EvaluationFactory {
        fn weight(
            &self,
            _: hirsel_engine_runtime::QueryId,
        ) -> Result<hirsel_engine_runtime::MemoryWeight, hirsel_engine_store::Reference> {
            Ok(hirsel_engine_runtime::MemoryWeight {
                retained: 1 << 20,
                scratch: 1 << 20,
                stack: 0,
            })
        }
        fn create(
            &self,
            query: hirsel_engine_runtime::QueryId,
        ) -> Result<Box<dyn hirsel_engine_runtime::LocalQueryTask>, hirsel_engine_store::Reference>
        {
            let types = self.types.clone();
            let snapshot = self.snapshot.clone();
            let calls = self.calls.clone();
            let fail = self.fail;
            let task = hirsel_engine_runtime::LocalFnTask::new(
                query,
                self.weight(query)?,
                errors(),
                move |dependencies| {
                    let root = DefPath::core("saved-private-root");
                    let dependency = DefPath::core("saved-private-dependency");
                    let prepared = reading_artifact(&types, &root, &dependency);
                    let admitted = ValidatedEvaluation::recorded(
                        prepared,
                        &snapshot,
                        Verify::On,
                        Span::default(),
                    );
                    assert!(
                        admitted
                            .reads
                            .get(&ReadKey::ValidationUniverse(
                                hirsel_query_products::reads::PackageKey::of(&root.0.package)
                            ))
                            .is_some()
                    );
                    let validated = admitted.result.unwrap();
                    let runtime = hirsel_interp::aggregate::RuntimeTypes::new(types.clone());
                    let expected = Reads::from_vec(vec![(
                        ReadKey::ExecutedBody(DefId::intern(&dependency)),
                        ContentHash::of_bytes(b"selected leaf"),
                    )])
                    .unwrap();
                    let mut source = DeferredSource {
                        dependencies,
                        query: super::tests::query(1),
                        types,
                        reads: expected.clone(),
                        calls,
                    };
                    let outcome = validated.run(
                        &SourceMap::default(),
                        Default::default(),
                        &runtime,
                        &mut source,
                    );
                    assert_eq!(outcome.value.reads, expected);
                    assert!(outcome.limit.is_none());
                    if fail {
                        assert!(matches!(
                            outcome.value.result,
                            Err(QueryError::NotImplemented)
                        ));
                    } else {
                        assert!(matches!(
                            outcome.value.result,
                            Ok((hirsel_interp::Value::Unit, _))
                        ));
                    }
                    Ok(product(query))
                },
            );
            Ok(Box::new(task))
        }
    }
    #[test]
    fn validated_private_evaluation_reads_s5_once_with_complete_failure_reads() {
        use hirsel_engine_runtime::AdmittedRuntime;
        use hirsel_engine_runtime::QueryRuntime;

        hirsel_intern::jobs::with_compiler_stack(|| {
            for fail in [false, true] {
                let types = hirsel_types::TypeInterner::new(None);
                let dependency = DefPath::core("saved-private-dependency");
                let prepared =
                    reading_artifact(&types, &DefPath::core("saved-private-root"), &dependency);
                let mut signature = prepared.signature;
                signature.definition = DefId::intern(&dependency);
                let snapshot = Arc::new(SignatureSnapshot::new(
                    Revision::FIRST,
                    types.clone(),
                    [(signature.definition, Ok(Arc::new(signature)))],
                    [],
                    None,
                ));
                let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let budget = hirsel_engine_admission::ProcessBudget::new(256 << 20);
                let mut runtime = AdmittedRuntime::new(
                    hirsel_intern::jobs::Jobs::new(1),
                    budget.reserve(256 << 20).unwrap(),
                    errors(),
                )
                .unwrap();
                runtime.register(1, Arc::new(Leaf(fail))).unwrap();
                runtime
                    .register_local(
                        2,
                        Arc::new(EvaluationFactory {
                            types,
                            snapshot,
                            calls: calls.clone(),
                            fail,
                        }),
                    )
                    .unwrap();
                let request = runtime.request(&[query(2)]);
                assert_eq!(runtime.wait(request), Ok(vec![product(query(2))]));
                assert_eq!(runtime.stats().claims, 2);
                assert_eq!(
                    calls.load(std::sync::atomic::Ordering::SeqCst),
                    1,
                    "resume must not reissue or replay the initializer's source demand"
                );
            }
        });
    }
    #[test]
    fn immutable_evaluation_keeps_dependency_and_reflection_reads_after_failure() {
        hirsel_intern::jobs::with_compiler_stack(|| {
            let types = hirsel_types::TypeInterner::new(None);
            let root = DefPath::core("private-evaluation");
            let dependency = DefPath::core("constant-dependency");
            let prepared = reading_artifact(&types, &root, &dependency);
            let mut signature = prepared.signature.clone();
            signature.definition = DefId::intern(&dependency);
            let snapshot = SignatureSnapshot::new(
                Revision::FIRST,
                types.clone(),
                [(signature.definition, Ok(Arc::new(signature)))],
                [],
                None,
            );
            let runtime = hirsel_interp::aggregate::RuntimeTypes::new(types);
            let mut source = Failed {
                calls: 0,
                reads: Reads::from_vec(vec![(
                    ReadKey::ExecutedBody(DefId::intern(&dependency)),
                    ContentHash::of_bytes(b"body"),
                )])
                .unwrap(),
            };
            let admitted =
                ValidatedEvaluation::new(prepared, Some(&snapshot), Verify::On, Span::default())
                    .unwrap();
            let result = admitted.run(
                &SourceMap::default(),
                Default::default(),
                &runtime,
                &mut source,
            );
            assert!(matches!(
                result.value.result,
                Err(QueryError::NotImplemented)
            ));
            assert_eq!(source.calls, 1);
            assert_eq!(result.value.reads, source.reads);
            assert!(result.limit.is_none());
            assert!(runtime.serialization.finish().is_empty());
        });
    }

    #[test]
    fn failed_private_admission_keeps_its_originating_universe_receipt() {
        hirsel_intern::jobs::with_compiler_stack(|| {
            let types = hirsel_types::TypeInterner::new(None);
            let root = DefPath::core("private-admission");
            let dependency = DefPath::core("constant-dependency");
            let snapshot = SignatureSnapshot::new(Revision::FIRST, types.clone(), [], [], None);
            let mut artifact = reading_artifact(&types, &root, &dependency);
            artifact.definition.entry = BodyId(100);
            let product =
                ValidatedEvaluation::recorded(artifact, &snapshot, Verify::On, Span::default());
            assert!(matches!(product.result, Err(QueryError::Operational(_))));
            assert_eq!(
                product.reads.get(&ReadKey::ValidationUniverse(
                    hirsel_query_products::reads::PackageKey::of(&root.0.package)
                )),
                Some(snapshot.validation_input_digest(&root.0.package).unwrap())
            );
            let artifact = reading_artifact(&types, &root, &dependency);
            let unchecked =
                ValidatedEvaluation::recorded(artifact, &snapshot, Verify::Off, Span::default());
            assert!(unchecked.result.is_ok());
            assert!(
                unchecked.reads.is_empty(),
                "verification off creates no acceptance proof"
            );
        });
    }

    #[test]
    fn dependency_failures_preserve_reads_and_private_roots_are_local() {
        let path = DefPath::core("task-root");
        let dependency = DefPath::core("dependency");
        let root = Arc::new(hirsel_tir::Definition {
            caller_bodies: vec![],
            protocol_associations: vec![],
            types: Arc::from([]),
            path: DefId::intern(&path),
            signature_hash: ContentHash::default(),
            bodies: vec![],
            entry: BodyId(0),
            contracts: vec![],
        });
        let reads = Reads::from_vec(vec![(
            ReadKey::ExecutedBody(DefId::intern(&dependency)),
            ContentHash::of_bytes(b"body"),
        )])
        .unwrap();
        let mut source = Failed {
            reads: reads.clone(),
            calls: 0,
        };
        let mut provider = EvaluationProvider::new(root.clone(), &mut source);
        assert!(Arc::ptr_eq(&provider.definition(&path).unwrap(), &root));
        assert!(provider.constant_definitions().unwrap().is_empty());
        assert!(provider.definition(&dependency).is_err());
        assert!(provider.constant(&dependency).is_err());
        assert!(provider.constant_instance(&dependency, &[3]).is_err());
        assert!(provider.instance_body(&dependency, &[]).is_err());
        assert_eq!(provider.finish().unwrap(), reads);
        assert_eq!(source.calls, 4);
    }
}
