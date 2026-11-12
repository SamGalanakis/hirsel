//! Accepted CT-5 syntax. The private generator output and its admission owner
//! survive every front that consumes the normalized declaration.
use super::*;

/// A rejected registration retains the sources its diagnostics cite without
/// publishing generated syntax into the engine's accepted namespace.
pub struct FrontRegistrationFailure {
    pub definition: DefId,
    pub span: Option<Span>,
    pub error: QueryError,
    pub sources: SourceMap,
    pub(super) _allowance: Arc<hirsel_engine_admission::MemoryReservation>,
}
impl std::fmt::Debug for FrontRegistrationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrontRegistrationFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

pub struct AcceptedFrontRegistration {
    pub(crate) syntax: Arc<ItemSyntax>,
    pub(crate) reads: Arc<Reads>,
    output: Arc<scheduled_lower::GeneratorResult>,
    execution: Option<hirsel_validate::ExecutionReceipt>,
    original: (hirsel_span::Anchor, u32, ContentHash),
    generated: (FileId, Span),
    pub(super) instantiation: Option<Span>,
    schema_origins: Vec<(std::ops::Range<usize>, hirsel_diag::Label)>,
    checked_prefix: Vec<SignatureDependency>,
    _allowance: Arc<hirsel_engine_admission::MemoryReservation>,
    _refresh: Option<Arc<hirsel_engine_admission::MemoryReservation>>,
    retained: u64,
    engine_validation: std::sync::Mutex<Option<EngineValidation>>,
}

#[derive(Clone)]
struct EngineValidation {
    revision: hirsel_query_products::reads::Revision,
    unchanged: bool,
    snapshot: std::sync::Weak<crate::SignatureSnapshot>,
    inputs: std::sync::Weak<header_products::SourceInputs>,
    sources: native_records::NativeRecordIdentity,
    _lease: Arc<dyn Send + Sync>,
}
impl AcceptedFrontRegistration {
    pub(crate) fn in_engine(
        self: &Arc<Self>,
        engine: &mut QueryEngine,
        pool: &Arc<hirsel_engine_admission::ProcessBudget>,
    ) -> Result<Arc<Self>, QueryError> {
        if self.engine_validation_holds(engine, true) {
            return Ok(self.clone());
        }
        let (reads, execution) = self.validate_engine_inputs(engine)?;
        if reads == *self.reads {
            return Ok(self.clone());
        }
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(
            size_of::<Self>()
                + size_of::<Reads>()
                + 6 * size_of::<usize>()
                + size_of::<hirsel_engine_admission::MemoryReservation>(),
        );
        weight.vec(&self.schema_origins, |weight, (_, label)| {
            weight.add(label.message.capacity())
        });
        weight.flat_vec(&self.checked_prefix);
        for (_, bytes) in reads.allocations() {
            weight.add(bytes);
        }
        if let Some(execution) = &execution {
            weight.add(
                execution
                    .retained_bytes()
                    .and_then(|bytes| bytes.try_into().ok())
                    .ok_or_else(refusal)?,
            );
        }
        let refresh = Arc::new(pool.reserve(weight.bytes()).ok().ok_or_else(refusal)?);
        Ok(Arc::new(Self {
            syntax: self.syntax.clone(),
            reads: Arc::new(reads),
            output: self.output.clone(),
            execution,
            original: self.original,
            generated: self.generated,
            instantiation: self.instantiation,
            schema_origins: self.schema_origins.clone(),
            checked_prefix: self.checked_prefix.clone(),
            _allowance: self._allowance.clone(),
            retained: self
                ._allowance
                .bytes()
                .checked_add(refresh.bytes())
                .ok_or_else(refusal)?,
            _refresh: Some(refresh),
            engine_validation: Default::default(),
        }))
    }

    fn engine_validation_holds(&self, engine: &mut QueryEngine, unchanged: bool) -> bool {
        let previous = self
            .engine_validation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(previous) = previous else {
            return false;
        };
        // A cold or changed owner needs no reconstruction of source inputs just
        // to discover that it has no reusable result.
        let holds = (!unchanged || previous.unchanged)
            && previous.revision == engine.revision
            && previous.sources.matches(engine)
            && previous
                .snapshot
                .ptr_eq(&Arc::downgrade(&engine.signature_inputs()))
            && header_products::SourceInputs::reusable(engine)
                .is_some_and(|inputs| previous.inputs.ptr_eq(&Arc::downgrade(&inputs)))
            && previous.revision == engine.revision
            && previous.sources.matches(engine);
        if !holds {
            self.engine_validation
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
        }
        holds
    }

    fn remember_engine_validation(
        &self,
        engine: &QueryEngine,
        snapshot: &Arc<crate::SignatureSnapshot>,
        inputs: &Arc<header_products::SourceInputs>,
        sources: native_records::NativeRecordIdentity,
        revision: hirsel_query_products::reads::Revision,
        unchanged: bool,
    ) {
        if engine.revision != revision
            || !header_products::SourceInputs::reusable(engine)
                .is_some_and(|current| Arc::ptr_eq(&current, inputs))
            || !sources.matches(engine)
        {
            return;
        }
        let Some(headers) = snapshot.header_products() else {
            return;
        };
        // A weak key keeps its Arc control allocation after the payload drops.
        // Charge those allocations before retaining this optional result.
        let bytes = (size_of::<crate::SignatureSnapshot>()
            + size_of::<header_products::SourceInputs>()
            + 4 * size_of::<usize>()) as u64
            + native_records::NativeRecordIdentity::control_bytes();
        let Ok(lease) = headers.try_validation_lease(bytes) else {
            return;
        };
        *self
            .engine_validation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(EngineValidation {
            revision,
            unchanged,
            snapshot: Arc::downgrade(snapshot),
            inputs: Arc::downgrade(inputs),
            sources,
            _lease: lease,
        });
    }
    pub fn definition(&self) -> &DefPath {
        self.output.definition()
    }
    pub(crate) fn explain<T: Clone>(
        &self,
        result: &mut Result<Arc<QueryOutput<T>>, QueryError>,
        sources: &SourceMap,
    ) {
        let diagnostics = match result {
            Ok(output) if !output.diagnostics.is_empty() => &mut Arc::make_mut(output).diagnostics,
            Err(QueryError::Diagnostics(diagnostics)) => diagnostics,
            _ => return,
        };
        for diagnostic in diagnostics {
            let at = sources.resolve(diagnostic.primary);
            if at.file != self.generated.0 {
                continue;
            }
            for (range, label) in &self.schema_origins {
                if at.start as usize >= range.start
                    && at.end as usize <= range.end
                    && !diagnostic.secondary.contains(label)
                {
                    diagnostic.secondary.push(label.clone());
                }
            }
            let origin = hirsel_diag::Label {
                span: self.generated.1,
                message: "generated by this `= comptime` body".into(),
            };
            if !diagnostic.secondary.contains(&origin) {
                diagnostic.secondary.push(origin);
            }
            if let Some(site) = self.instantiation {
                diagnostic
                    .notes
                    .push(format!("generated text span {}..{}", at.start, at.end));
                diagnostic.primary = site;
            }
        }
    }
    /// Complete declaration, normalization and private execution observations.
    pub fn reads(&self) -> &Reads {
        &self.reads
    }
    /// Revalidate against an immutable snapshot observer. This is a reuse
    /// predicate, not authority to install syntax; installation still validates
    /// the original source binding in front_body_queries_with_registrations.
    pub fn validate_current(
        &self,
        capture: &NamespaceCapture,
        mut observe: impl FnMut(&DefPath, &ReadKey) -> Option<ContentHash>,
    ) -> Result<(Reads, Option<hirsel_validate::ExecutionReceipt>), QueryError> {
        let id = DefId::intern(self.definition());
        let original = if capture.bodies.contains_key(&id) {
            let syntax = capture.syntax(self.definition())?;
            Some((syntax.anchor, syntax.occurrence, syntax.text_hash))
        } else {
            capture.snapshot.header_products().and_then(|headers| {
                Some((
                    crate::NativeRecordSource::record_source_anchor(
                        &*capture.anchors,
                        self.definition(),
                        self.original.1,
                    )?,
                    self.original.1,
                    headers.captured_text(id)?.implementation?,
                ))
            })
        };
        self.validate_exact(
            original,
            capture.verification(),
            &mut observe,
            |_, receipt| capture.revalidate_execution(receipt),
            |observe, definition, key| observe(definition, key),
        )
    }
    fn validate_exact<C>(
        &self,
        original: Option<(hirsel_span::Anchor, u32, ContentHash)>,
        verify: Verify,
        context: &mut C,
        revalidate: impl FnOnce(
            &mut C,
            &hirsel_validate::ExecutionReceipt,
        )
            -> Result<Option<hirsel_validate::ExecutionReceipt>, QueryError>,
        mut observe: impl FnMut(&mut C, &DefPath, &ReadKey) -> Option<ContentHash>,
    ) -> Result<(Reads, Option<hirsel_validate::ExecutionReceipt>), QueryError> {
        let invalid = |reason: &str| {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "generated syntax validation".into(),
                message: reason.into(),
            })
        };
        hirsel_intern::work::count(hirsel_intern::work::Counter::RegistrationComparisons);
        hirsel_intern::timings::count(hirsel_intern::timings::Count::RegistrationComparisons);
        if original != Some(self.original) {
            return Err(invalid("original declaration changed"));
        }
        let execution = if verify == Verify::On {
            let receipt = self
                .acceptance()
                .ok_or_else(|| invalid("missing private execution acceptance"))?;
            Some(revalidate(context, receipt)?.ok_or_else(|| {
                invalid("V refused the private artifacts in the current environment")
            })?)
        } else {
            None
        };
        if let Some(execution) = &execution {
            for (package, digest) in execution.contexts() {
                let key = ReadKey::ValidationUniverse(
                    hirsel_query_products::reads::PackageKey::of(package),
                );
                if observe(context, self.definition(), &key) != Some(*digest) {
                    return Err(invalid(
                        "observer does not identify the current validation universe",
                    ));
                }
            }
        }
        if self
            .reads
            .first_changed(|key| {
                if matches!(key, ReadKey::ValidationUniverse(_)) && execution.is_some() {
                    self.reads.get(key)
                } else {
                    hirsel_intern::work::count(
                        hirsel_intern::work::Counter::RegistrationReadComparisons,
                    );
                    hirsel_intern::timings::count(
                        hirsel_intern::timings::Count::RegistrationReadComparisons,
                    );
                    observe(context, self.definition(), key)
                }
            })
            .is_some()
        {
            return Err(invalid("semantic generator or normalization input changed"));
        }
        let reads =
            scheduled_lower::evidence::current_reads((*self.reads).clone(), execution.as_ref())
                .ok_or_else(|| {
                    invalid("execution does not cover the registration validation reads")
                })?;
        Ok((reads, execution))
    }
    /// Engine-backed check, also used when constructing a fresh front view.
    pub fn validate_in_engine(&self, engine: &mut QueryEngine) -> Result<(), QueryError> {
        self.validate(engine)
    }
    pub fn retained_bytes(&self) -> u64 {
        self.retained
    }
    pub(crate) fn checked_prefix(&self) -> &[SignatureDependency] {
        &self.checked_prefix
    }
    pub(crate) fn source(&self) -> GeneratedSource {
        GeneratedSource {
            file: self.generated.0,
            text: self.text().to_owned(),
            site: self.generated.1,
        }
    }
    pub(crate) fn text(&self) -> &str {
        self.output
            .text()
            .expect("accepted successful generator text")
    }
    pub(crate) fn generation_manifest(&self) -> ContentHash {
        self.output.manifest()
    }
    pub(crate) fn depth(&self) -> &hirsel_query_products::depth::DepthEvidence<Span> {
        self.output.depth()
    }
    pub(crate) fn acceptance(&self) -> Option<&hirsel_validate::ExecutionReceipt> {
        self.execution.as_ref()
    }
    pub(crate) fn validate(&self, engine: &mut QueryEngine) -> Result<(), QueryError> {
        if self.engine_validation_holds(engine, false) {
            return Ok(());
        }
        self.validate_engine_inputs(engine).map(|_| ())
    }
    fn validate_engine_inputs(
        &self,
        engine: &mut QueryEngine,
    ) -> Result<(Reads, Option<hirsel_validate::ExecutionReceipt>), QueryError> {
        let syntax = engine.snapshot(self.definition())?;
        let original = Some((syntax.anchor, syntax.occurrence, syntax.text_hash));
        let snapshot = engine.signature_inputs();
        let anchors = engine.source_records();
        let inputs = header_products::SourceInputs::reusable(engine);
        let source_identity = anchors.identity();
        let revision = engine.revision;
        let result = self.validate_exact(
            original,
            engine.verify,
            engine,
            |engine, receipt| {
                snapshot.revalidate_execution(receipt, |path, occurrence| {
                    crate::NativeRecordSource::record_source_anchor(&anchors, path, occurrence)
                        .or_else(|| {
                            engine.items.iter().find_map(|(owner, candidates)| {
                                (generation::task::GeneratorInput::private_path(owner) == *path
                                    && occurrence == 0)
                                    .then(|| candidates.first().map(|syntax| syntax.anchor))
                                    .flatten()
                            })
                        })
                })
            },
            |engine, definition, key| match key {
                // General engine observation may prepare another private
                // snapshot. This guard belongs to the exact owner V replayed.
                ReadKey::ValidationUniverse(package) => {
                    snapshot.validation_input_digest(&package.id()).ok()
                }
                ReadKey::ValidationView(package, id) => Some(
                    snapshot
                        .validation(&package.id())
                        .ok()?
                        .view(*id)
                        .ok()?
                        .map_or_else(ContentHash::default, |view| view.digest().0),
                ),
                ReadKey::UniverseErrors(package) => {
                    Some(snapshot.validation(&package.id()).ok()?.universe_errors().0)
                }
                ReadKey::HeapRole(package) => Some(
                    snapshot
                        .validation(&package.id())
                        .ok()?
                        .heap_observation()
                        .ok()?
                        .fingerprint(),
                ),
                _ => engine.observe(definition, key),
            },
        )?;
        let unchanged = result.0 == *self.reads
            && result.1.as_ref().map(|receipt| receipt.contexts())
                == self.execution.as_ref().map(|receipt| receipt.contexts());
        if let Some(inputs) = &inputs {
            self.remember_engine_validation(
                engine,
                &snapshot,
                inputs,
                source_identity,
                revision,
                unchanged,
            );
        }
        Ok(result)
    }
}

struct AcceptanceInputs {
    original: Arc<ItemSyntax>,
    snapshot: Arc<SignatureSnapshot>,
    declarations: Arc<Declarations>,
    sources: SourceMap,
    citations: crate::snapshot::OwnedSourceContext,
    types: Arc<hirsel_types::TypeInterner>,
    memo: Arc<hirsel_types::TypeMemo>,
    lexer: Option<Arc<dyn LexerPhase>>,
    parser: Option<Arc<dyn ParserPhase>>,
    signer: Option<Arc<dyn SignaturePhase>>,
    current: Vec<(ReadKey, Option<ContentHash>)>,
    verify: Verify,
    schema_origins: Vec<(std::ops::Range<usize>, hirsel_diag::Label)>,
}

struct NormalizationProvider<'a> {
    frozen: FrozenProvider<'a>,
    dependencies: &'a mut Vec<SignatureDependency>,
}
impl SignatureProvider for NormalizationProvider<'_> {
    fn signature(&mut self, id: DefId) -> Result<hirsel_types::SignatureRef, QueryError> {
        let signature = self.frozen.signature(id)?;
        self.dependencies.push(SignatureDependency {
            definition: id,
            fingerprint: signature.fingerprint,
        });
        Ok(signature)
    }
    fn lookup(
        &mut self,
        context: &FileContext,
        path: &hirsel_syntax::Path,
    ) -> hirsel_types::LookupOutput {
        self.frozen.lookup(context, path)
    }
    fn cite_name(&mut self, path: &DefPath) -> Option<Span> {
        self.frozen.cite_name(path)
    }
}

fn refusal() -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "generated syntax acceptance".into(),
        message: "generator output is stale, unaccepted, or exceeds its reservation".into(),
    })
}
fn source_weight(
    sources: &SourceMap,
    generated: Option<&GeneratedSource>,
    previous_payload: u64,
) -> hirsel_query_products::retained::MetadataWeight {
    let mut weight = hirsel_query_products::retained::MetadataWeight::default();
    weight.add(size_of::<hirsel_engine_admission::MemoryReservation>() + 2 * size_of::<usize>());
    weight.tree::<DefPath, GeneratedSource>(1);
    if let Some(source) = generated {
        weight.add(source.text.capacity());
    }
    weight.add(
        sources
            .retained_payload_bytes()
            .saturating_sub(previous_payload)
            .try_into()
            .unwrap_or(usize::MAX),
    );
    weight.source_snapshot(sources);
    weight.tree::<hirsel_span::Anchor, (FileId, u32)>(sources.anchors().count());
    weight
}

impl AcceptanceInputs {
    fn accept(
        self,
        output: FrontRegistrationOutput,
        request: &Arc<hirsel_engine_admission::ProcessBudget>,
    ) -> Result<(Arc<AcceptedFrontRegistration>, SourceMap), FrontRegistrationFailure> {
        let definition = output.definition().clone();
        let source_before = self.sources.retained_payload_bytes();
        let mut sources = self.sources.clone();
        let mut generated = None;
        let result = (|| -> Result<Arc<AcceptedFrontRegistration>, QueryError> {
            let execution_refusal = |message: &str| {
                QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "generated syntax acceptance".into(),
                    message: message.into(),
                })
            };
            let execution = if self.verify == Verify::On {
                let receipt = output.acceptance().ok_or_else(|| {
                    execution_refusal("generator output has no private execution receipt")
                })?;
                Some(
                    self.snapshot
                        .revalidate_execution(receipt, |path, occurrence| {
                            crate::NativeRecordSource::record_source_anchor(
                                &*self.citations.owners,
                                path,
                                occurrence,
                            )
                            .or_else(|| {
                                self.citations.items.iter().find_map(|(owner, candidates)| {
                                    (task::GeneratorInput::private_path(owner) == *path
                                        && occurrence == 0)
                                        .then(|| candidates.first().map(|syntax| syntax.anchor))
                                        .flatten()
                                })
                            })
                        })?
                        .ok_or_else(|| execution_refusal("V refused the generator's private execution receipt in its accepting snapshot"))?,
                )
            } else {
                None
            };
            let execution_reads = scheduled_lower::evidence::current_reads(
                output.reads().clone(),
                execution.as_ref(),
            )
            .ok_or_else(refusal)?;
            let text = output.text()?.to_owned();
            let original = &self.original;
            if original.item.definition != definition {
                return Err(refusal());
            }
            if let Some((key, current)) = self
                .current
                .iter()
                .find(|(key, current)| execution_reads.get(key) != *current)
            {
                return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "generated syntax acceptance".into(),
                    message: format!(
                        "{definition}: changed generator read {key:?}: {:?} -> {current:?}",
                        execution_reads.get(key)
                    ),
                }));
            }
            if self.verify == Verify::On {
                let receipt = execution.as_ref().ok_or_else(refusal)?;
                for (package, digest) in receipt.contexts() {
                    if self.snapshot.validation_input_digest(package).ok() != Some(*digest) {
                        return Err(refusal());
                    }
                }
            }
            let body = generator(original).ok_or_else(refusal)?;
            let site = original.tree.expressions[body.0 as usize].origin.span;
            let logical = format!("<generated:{definition}>");
            let file = sources
                .add_logical(&logical, &logical, text.clone())
                .ok_or_else(refusal)?;
            generated = Some(GeneratedSource { file, text, site });
            let (Some(lexer), Some(parser), Some(signer)) = (
                self.lexer.as_deref(),
                self.parser.as_deref(),
                self.signer.as_deref(),
            ) else {
                return Err(QueryError::NotImplemented);
            };
            let mut observed = ReadsBuilder::new();
            observed.merge(&execution_reads);
            let mut dependencies = output.dependencies().to_vec();
            let normalize = (|| {
                let literal = syntax::parse_literal(
                    &definition,
                    original,
                    file,
                    &mut sources,
                    lexer,
                    parser,
                )?;
                let mut provider = NormalizationProvider {
                    frozen: FrozenProvider::for_definition(
                        &self.snapshot,
                        DefId::intern(&definition),
                        &self.citations,
                        &mut observed,
                    ),
                    dependencies: &mut dependencies,
                };
                let result = syntax::normalize_headers(&literal, &self.types, |syntax| {
                    signer.signature(
                        &self.types,
                        &self.memo,
                        syntax,
                        &self.declarations,
                        &mut PhaseProducts::default(),
                        &mut provider,
                    )
                });
                result?;
                let original_file = self.sources.anchor(original.anchor).ok_or_else(refusal)?.0;
                literal.finish(
                    original,
                    body,
                    original_file,
                    &generated.as_ref().unwrap().text,
                    &mut sources,
                    parser,
                )
            })();
            let syntax = normalize.map_err(|mut error| {
                if let QueryError::Diagnostics(diagnostics) = &mut error {
                    for diagnostic in diagnostics {
                        let at = sources.resolve(diagnostic.primary);
                        if at.file != file {
                            continue;
                        }
                        for (range, label) in &self.schema_origins {
                            if at.start as usize >= range.start && at.end as usize <= range.end {
                                diagnostic.secondary.push(label.clone());
                            }
                        }
                        diagnostic.secondary.push(hirsel_diag::Label {
                            span: site,
                            message: "generated by this `= comptime` body".into(),
                        });
                    }
                }
                error
            })?;
            let reads = Arc::new(observed.finish().map_err(|_| refusal())?);
            let mut weight = source_weight(&sources, generated.as_ref(), source_before);
            weight.path(&definition);
            weight.add(size_of::<AcceptedFrontRegistration>() + 2 * size_of::<usize>());
            weight.vec(&self.schema_origins, |weight, (_, label)| {
                weight.add(label.message.capacity())
            });
            weight.add(
                hirsel_query_products::retained::retained_syntax_bytes(&syntax)
                    .try_into()
                    .map_err(|_| refusal())?,
            );
            for (_, bytes) in reads.allocations() {
                weight.add(bytes);
            }
            weight.flat_vec(&dependencies);
            if let Some(execution) = &execution {
                weight.add(
                    execution
                        .retained_bytes()
                        .and_then(|bytes| bytes.try_into().ok())
                        .ok_or_else(refusal)?,
                );
            }
            let retained = weight.bytes();
            let retained_allowance = Arc::new(request.reserve(retained).map_err(|_| QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "generated syntax acceptance".into(),
                message: format!("{definition}: generated syntax requires {retained} retained bytes, {} free in the request footprint", request.free()),
            }))?);
            sources = sources.clone().retain_owner(retained_allowance.clone());
            Ok(Arc::new(AcceptedFrontRegistration {
                syntax,
                reads,
                output: output.into_data(),
                execution,
                original: (original.anchor, original.occurrence, original.text_hash),
                generated: (file, site),
                instantiation: None,
                schema_origins: self.schema_origins,
                checked_prefix: dependencies,
                _allowance: retained_allowance,
                _refresh: None,
                retained,
                engine_validation: Default::default(),
            }))
        })();
        match result {
            Ok(accepted) => Ok((accepted, sources)),
            Err(mut error) => {
                let mut weight = source_weight(&sources, generated.as_ref(), source_before);
                weight.path(&definition);
                weight.add(
                    hirsel_query_products::retained::retained_query_error_bytes(&error)
                        .try_into()
                        .unwrap_or(usize::MAX),
                );
                let retained = matches!(error, QueryError::Diagnostics(_))
                    .then(|| request.reserve(weight.bytes()).ok())
                    .flatten();
                let keep_sources = retained.is_some();
                let allowance = Arc::new(retained.unwrap_or_else(|| request.empty()));
                let sources = if keep_sources {
                    sources.retain_owner(allowance.clone())
                } else {
                    if matches!(error, QueryError::Diagnostics(_)) {
                        error = refusal();
                    }
                    self.sources.clone()
                };
                Err(FrontRegistrationFailure {
                    definition: DefId::intern(&definition),
                    span: Some(self.original.item.name_span),
                    error,
                    sources,
                    _allowance: allowance,
                })
            }
        }
    }
}
impl QueryEngine {
    /// Capture immutable normalization inputs, accept privately, then publish
    /// the owned source only after every check and its reservation succeeds.
    pub fn accept_front_registration(
        &mut self,
        output: FrontRegistrationOutput,
        request: &Arc<hirsel_engine_admission::ProcessBudget>,
    ) -> Result<Arc<AcceptedFrontRegistration>, FrontRegistrationFailure> {
        let definition = output.definition().clone();
        let captured = (|| {
            let original = self.snapshot(&definition)?;
            let mut current = Vec::new();
            for (key, _) in output.reads().iter() {
                let observed = self.observe(&definition, key);
                current.push((*key, observed));
            }
            let schema_origins = output.text().ok().map_or_else(Vec::new, |text| {
                crate::diagnostics::schema_hook_contexts(text)
                    .filter_map(|(range, parent, member, contract)| {
                        Some((
                            range,
                            hirsel_diag::Label {
                                span: self.schema_member_span(parent, member)?,
                                message: format!(
                                    "schema field `{parent}.{member}` requires `{contract}`"
                                ),
                            },
                        ))
                    })
                    .collect()
            });
            let snapshot = self.signature_inputs();
            if let Some((_, cache)) = self.snapshot.as_mut() {
                cache.release();
            }
            for (key, value) in &mut current {
                if let ReadKey::ValidationUniverse(package) = key {
                    *value = snapshot.validation_input_digest(&package.id()).ok();
                }
            }
            Ok((
                original,
                current,
                schema_origins,
                snapshot,
                self.frozen_declarations(),
            ))
        })();
        let (original, current, schema_origins, snapshot, declarations) = match captured {
            Ok(inputs) => inputs,
            Err(error) => {
                return Err(FrontRegistrationFailure {
                    span: self
                        .declarations
                        .sites(&definition)
                        .first()
                        .map(|site| site.name_span),
                    definition: DefId::intern(&definition),
                    error,
                    sources: self.sources.clone(),
                    _allowance: Arc::new(request.empty()),
                });
            }
        };
        let citations = crate::snapshot::OwnedSourceContext {
            sources: self.sources.clone(),
            items: self.items.observations(),
            declarations: self.declarations.observations(),
            catalog: self.catalog.clone(),
            owners: Arc::new(self.source_records()),
            cited: BTreeMap::new().into(),
            arm_fixes: BTreeMap::new().into(),
        };
        let inputs = AcceptanceInputs {
            original,
            current,
            schema_origins,
            snapshot,
            declarations,
            sources: self.sources.clone(),
            citations,
            types: self.types.clone(),
            memo: self.memo.clone(),
            verify: self.verify,
            lexer: self.adapters.lexer.clone(),
            parser: self.adapters.parser.clone(),
            signer: self.adapters.signature.clone(),
        };
        let (accepted, sources) = inputs.accept(output, request)?;
        self.sources = sources;
        self.generated.insert(definition, accepted.source());
        Ok(accepted)
    }
}
