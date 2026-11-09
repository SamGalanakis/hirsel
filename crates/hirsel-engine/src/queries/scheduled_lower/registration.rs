//! Private generator execution on an admitted home worker. Text remains a
//! tentative product: only the ordered source transaction may publish syntax.
use super::*;
use crate::queries::generation::task::PreparedGenerator;

pub const FRONT_REGISTRATION_KIND: u32 = 0x4601_0004;
const OUTPUT: Domain = Domain {
    kind: 0x4601_0006,
    schema: 3,
};
const FAILURE: Domain = Domain {
    kind: 0x4601_0007,
    schema: 2,
};
struct Input {
    key: EntryKey,
    definition: DefId,
    syntax: Arc<ItemSyntax>,
    implementation: ContentHash,
    site: Span,
    instance_reads: Reads,
    checked_prefix: Vec<SignatureDependency>,
    declared_header: ContentHash,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    manifest: ContentHash,
    output: Reference,
    decode_bytes: u64,
}
pub(in crate::queries) struct Data {
    diagnostic_sources: Option<Arc<namespace::DiagnosticSources>>,
    definition: DefId,
    manifest: ContentHash,
    text: Result<String, QueryError>,
    reads: Reads,
    dependencies: Vec<SignatureDependency>,
    depth: DepthEvidence<Span>,
    acceptance: Option<hirsel_validate::ExecutionReceipt>,
    failure: Option<Reference>,
    _allowance: Option<hirsel_engine_admission::MemoryReservation>,
}
type Completed = (ProductRef, Arc<Data>, Arc<NamespaceCapture>);
/// Text and evidence retain their immutable consumption capture. Normalization
/// and ordered publication still decide whether the syntax becomes visible.
#[derive(Clone)]
pub struct FrontRegistrationOutput {
    data: Arc<Data>,
    namespace: Arc<NamespaceCapture>,
}
impl serde::Serialize for Data {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(
            &(
                &self.definition,
                &self.text,
                &self.reads,
                &self.depth,
                &self.dependencies,
            ),
            serializer,
        )
    }
}
impl Data {
    pub(in crate::queries) fn manifest(&self) -> ContentHash {
        self.manifest
    }
    pub(in crate::queries) fn definition(&self) -> &DefPath {
        self.definition.path()
    }
    pub(in crate::queries) fn text(&self) -> Result<&str, QueryError> {
        self.text.as_deref().map_err(Clone::clone)
    }
    pub(in crate::queries) fn depth(&self) -> &DepthEvidence<Span> {
        &self.depth
    }
    pub(in crate::queries) fn acceptance(&self) -> Option<&hirsel_validate::ExecutionReceipt> {
        self.acceptance.as_ref()
    }
}
impl FrontRegistrationOutput {
    pub fn sources(&self) -> &SourceMap {
        self.data
            .diagnostic_sources
            .as_ref()
            .map_or(&self.namespace.sources, |sources| &sources.sources)
    }
    pub fn namespace(&self) -> &Arc<NamespaceCapture> {
        &self.namespace
    }
    pub(in crate::queries) fn into_data(self) -> Arc<Data> {
        self.data
    }
    pub fn definition(&self) -> &DefPath {
        self.data.definition()
    }
    pub fn text(&self) -> Result<&str, QueryError> {
        self.data.text()
    }
    pub fn reads(&self) -> &Reads {
        &self.data.reads
    }
    pub fn dependencies(&self) -> &[SignatureDependency] {
        &self.data.dependencies
    }
    pub fn depth(&self) -> &DepthEvidence<Span> {
        self.data.depth()
    }
    pub(in crate::queries) fn acceptance(&self) -> Option<&hirsel_validate::ExecutionReceipt> {
        self.data.acceptance()
    }
    pub fn has_execution_acceptance(&self) -> bool {
        self.data.acceptance.is_some()
    }
}
pub struct FrontRegistrationQueries {
    inputs: BTreeMap<ContentHash, Input>,
    lower: Arc<FrontLowerQueries>,
    signer: Arc<dyn SignaturePhase>,
    outcomes: Mutex<BTreeMap<ContentHash, Completed>>,
    weight: MemoryWeight,
    errors: RuntimeErrors,
    output_pool: Arc<hirsel_engine_admission::ProcessBudget>,
    _inputs: hirsel_engine_admission::MemoryReservation,
}
impl FrontRegistrationQueries {
    pub fn input_bytes(definitions: &[DefPath]) -> u64 {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(size_of::<Self>() + 2 * size_of::<usize>());
        weight.tree::<ContentHash, Input>(definitions.len());
        weight.tree::<ContentHash, Completed>(definitions.len());
        weight.bytes()
    }
    pub fn query(&self, path: &DefPath) -> Option<QueryId> {
        self.inputs.iter().find_map(|(address, input)| {
            (input.definition.path() == path).then_some(QueryId {
                kind: FRONT_REGISTRATION_KIND,
                address: hirsel_artifact_store::Address(*address),
            })
        })
    }
    pub fn factory(self: &Arc<Self>) -> Arc<dyn LocalQueryFactory> {
        Arc::new(Factory(self.clone()))
    }
    pub fn retained_bytes(&self) -> u64 {
        self._inputs.bytes()
            + self
                .outcomes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .map(|(_, data, _)| {
                    data._allowance
                        .as_ref()
                        .map_or(0, hirsel_engine_admission::MemoryReservation::bytes)
                })
                .sum::<u64>()
    }
    pub fn outcome(self: &Arc<Self>, product: ProductRef) -> Option<FrontRegistrationOutput> {
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        let (selected, data, namespace) = outcomes.get(&product.address.0)?;
        (*selected == product).then(|| FrontRegistrationOutput {
            data: data.clone(),
            namespace: namespace.clone(),
        })
    }
    pub fn failed(self: &Arc<Self>, reference: Reference) -> Option<FrontRegistrationOutput> {
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        let (_, (_, data, namespace)) = outcomes
            .iter()
            .find(|(_, (_, data, _))| data.failure == Some(reference))?;
        Some(FrontRegistrationOutput {
            data: data.clone(),
            namespace: namespace.clone(),
        })
    }
    pub(in crate::queries) fn run<'a>(
        self: Arc<Self>,
        id: QueryId,
        reads: &'a QueryReads<'a>,
    ) -> Result<ProductRef, Reference> {
        let result = self.clone().run_tentative(id, reads.clone());
        if let Ok(product) = &result {
            self.commit(product);
        }
        result
    }
    pub(in crate::queries) fn run_tentative<'a>(
        self: Arc<Self>,
        id: QueryId,
        reads: &'a QueryReads<'a>,
    ) -> Result<ProductRef, Reference> {
        let input = &self.inputs[&id.address.0];
        let (mut data, namespace) = if let Some(data) = self.restore(input) {
            (Arc::new(data), self.lower.namespace.clone())
        } else {
            let mut source = Source {
                id,
                root: input.definition,
                owner: self.lower.clone(),
                reads,
                observed: Rc::new(RefCell::new(ReadsBuilder::new())),
                selections: Rc::new(RefCell::new(Vec::new())),
                refusal: Rc::new(Cell::new(None)),
                depth: Rc::new(RefCell::new(DepthEvidence::leaf(dependency_limit(
                    self.lower.namespace.limits.call_depth,
                )))),
                framed: true,
                evidence: Rc::new(RefCell::new(Evidence::new(
                    self.lower.verify,
                    self.weight.retained,
                ))),
                successor: Rc::default(),
                restart: Rc::default(),
                diagnostic_sources: Rc::default(),
                phase: Rc::new(Cell::new(Some(QueryKind::Generate))),
            };
            source.record(
                ReadKey::ExecutedBody(input.definition),
                memos::optional(Some(input.implementation)),
            );
            source.merge(&input.instance_reads);
            source.record(
                ReadKey::DeclaredSignature(input.definition),
                input.declared_header,
            );
            let mut dependencies = Vec::new();
            let text = if source.depth.borrow().limit() == 0 {
                Err(source.overflow(input.site))
            } else {
                self.compute(input, &mut source, &mut dependencies)
                    .map_err(|error| {
                        lower_input_error(input.definition.path(), "executing a generator", error)
                    })
            };
            let restart = source.restart.borrow_mut().take();
            if let Some(restart) = restart {
                let producer = restart
                    .owner()
                    .demand_producer()
                    .ok_or(self.errors.protocol)?;
                let mut engine = producer
                    .assembly(restart.namespace())
                    .map_err(|_| self.errors.resource)?;
                let replay = engine
                    .front_registration_queries(
                        std::slice::from_ref(input.definition.path()),
                        restart.owner().clone(),
                        self.weight,
                        &self.output_pool,
                        self.errors,
                    )
                    .map_err(|_| self.errors.resource)?;
                let query = replay
                    .query(input.definition.path())
                    .ok_or(self.errors.protocol)?;
                let reads = source.reads.clone();
                drop(engine);
                drop(text);
                drop(source);
                let selected = replay.clone().run_tentative(query, reads);
                let (mut product, data, namespace) = replay
                    .outcomes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&query.address.0)
                    .cloned()
                    .ok_or_else(|| {
                        selected
                            .as_ref()
                            .err()
                            .copied()
                            .unwrap_or(self.errors.protocol)
                    })?;
                product.address = id.address;
                self.outcomes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id.address.0, (product.clone(), data, namespace));
                return selected.map(|_| product);
            }
            if let Some(reference) = source.refusal.get() {
                return Err(reference);
            }
            let observed = std::mem::take(&mut *source.observed.borrow_mut())
                .finish()
                .map_err(|_| self.errors.protocol)?;
            let acceptance = text
                .as_ref()
                .ok()
                .and_then(|_| source.evidence.borrow().finish(&observed));
            let mut depth = source.depth.borrow().clone();
            depth.first[0] = None;
            (
                Arc::new(Data {
                    diagnostic_sources: source.diagnostic_sources.borrow().clone(),
                    definition: input.definition,
                    manifest: source.capture().manifest,
                    text,
                    reads: observed,
                    dependencies,
                    depth,
                    acceptance,
                    failure: None,
                    _allowance: None,
                }),
                source.capture(),
            )
        };
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.arc(&data);
        weight.add(
            size_of::<live_record::LiveRecord<Arc<Data>>>()
                + size_of::<FrontRegistrationOutput>()
                + 4 * size_of::<usize>(),
        );
        match &data.text {
            Ok(text) => weight.add(text.capacity()),
            Err(error) => weight.add(
                hirsel_query_products::retained::retained_query_error_bytes(error)
                    .try_into()
                    .map_err(|_| self.errors.resource)?,
            ),
        }
        for (_, bytes) in data.reads.allocations() {
            weight.add(bytes);
        }
        weight.add(size_of_val(&*data.depth.first));
        weight.flat_vec(&data.dependencies);
        if let Some(acceptance) = &data.acceptance {
            weight.add(
                acceptance
                    .retained_bytes()
                    .ok_or(self.errors.resource)?
                    .try_into()
                    .map_err(|_| self.errors.resource)?,
            );
        }
        let allowance = self
            .output_pool
            .reserve(weight.bytes())
            .ok()
            .ok_or(self.errors.resource)?;
        Arc::get_mut(&mut data)
            .expect("unpublished generator output")
            ._allowance = Some(allowance);
        let failed = if data.text.is_err() {
            let bytes = self
                .lower
                .records
                .encode(
                    &namespace::DiagnosticRecordSource {
                        base: &namespace.anchors,
                        diagnostics: data.diagnostic_sources.as_deref(),
                    },
                    &*data,
                )
                .ok_or(self.errors.resource)?;
            if bytes.len() as u64 > self.weight.scratch {
                return Err(self.errors.resource);
            }
            let record = self
                .lower
                .store
                .put_content(FAILURE, &bytes)
                .ok_or(self.errors.resource)?;
            Arc::get_mut(&mut data)
                .expect("unpublished generator failure")
                .failure = Some(record);
            Some((record, bytes))
        } else {
            None
        };
        let identity = match &data.text {
            Ok(text) => ContentHash::of_bytes(text.as_bytes()),
            Err(_) => failed.as_ref().expect("encoded failure").0.output.0,
        };
        let action = hirsel_artifact_store::Action(ContentHash::of_parts(
            "hirsel.front.registration.action.v2",
            [
                id.address.0.0.as_slice(),
                namespace.manifest.0.as_slice(),
                data.reads.digest().0.as_slice(),
                identity.0.as_slice(),
            ],
        ));
        let failure = failed.as_ref().map(|(record, _)| *record);
        let product = if let Some((record, bytes)) = failed {
            ProductRef::import_archive(
                id.address,
                action,
                ProductArchive::from_encoded_in(record, bytes, &self.output_pool)
                    .map_err(|_| self.errors.resource)?,
            )
        } else {
            ProductRef::live(
                id.address,
                action,
                Arc::new(live_record::LiveRecord {
                    value: data.clone(),
                    domain: OUTPUT,
                    records: self.lower.records.clone(),
                    source: (*namespace.anchors).clone(),
                    serialization_working: self.weight.scratch,
                    _admission: Arc::new(FrontRegistrationOutput {
                        data: data.clone(),
                        namespace: namespace.clone(),
                    }),
                }),
            )
        };
        self.outcomes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.address.0, (product.clone(), data, namespace));
        failure.map_or(Ok(product), Err)
    }
    pub(in crate::queries) fn commit(&self, product: &ProductRef) {
        if !self.lower.store.persistence().publishes() {
            return;
        }
        let Some(input) = self.inputs.get(&product.address.0) else {
            return;
        };
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        let Some((current, data, namespace)) = outcomes.get(&product.address.0).cloned() else {
            return;
        };
        drop(outcomes);
        if &current != product || data.text.is_err() {
            return;
        }
        let Some((bytes, working)) = self
            .lower
            .records
            .encode_with_decode_bytes(&*namespace.anchors, &*data)
        else {
            return;
        };
        if bytes.len() as u64 > self.weight.scratch {
            return;
        }
        let Some(decode_bytes) = data
            ._allowance
            .as_ref()
            .and_then(|grant| grant.bytes().checked_mul(2))
            .and_then(|bytes| bytes.checked_add(working))
        else {
            return;
        };
        let Some(output) = self.lower.store.put_content(OUTPUT, &bytes) else {
            return;
        };
        self.remember(input, output, &data, decode_bytes, data.manifest);
    }
    fn storage_key(input: &Input) -> EntryKey {
        EntryKey(
            ContentHash::of_parts(
                "hirsel.front.registration.variants.v2",
                [input.key.0.0.as_slice()],
            ),
            input.key.1,
            input.key.2,
        )
    }
    fn observe(&self, input: &Input, read: &ReadKey) -> Option<ContentHash> {
        if *read == ReadKey::ExecutedBody(input.definition) {
            Some(memos::optional(Some(input.implementation)))
        } else {
            self.lower.observe(input.definition.path(), read)
        }
    }
    fn restore(&self, input: &Input) -> Option<Data> {
        use hirsel_engine_store::Entry;
        if self.lower.verify != Verify::On || !self.lower.store.persistence().retains() {
            return None;
        }
        let key = Self::storage_key(input);
        let variant = Entry::load(&*self.lower.store, &key).resolve(&*self.lower.store, &key)?;
        let stored: Stored = bincode::deserialize(&variant.payload).ok()?;
        // The key is the seed S; the candidate's typed dependencies must hold.
        if !variant
            .reads
            .iter()
            .all(|(read, value)| self.observe(input, read) == Some(*value))
            || !self
                .lower
                .namespace
                .manifest_holds(stored.manifest, self.weight.scratch)
            || stored.output.domain != OUTPUT
            || stored.output.len > self.weight.scratch
            || stored.decode_bytes > self.weight.scratch
        {
            return None;
        }
        let acceptance = if self.lower.verify == Verify::On {
            Some(self.lower.store.verifier()?.execution(
                persistent::evaluation_subject(&key, &variant.reads, &variant.payload),
                |package| {
                    self.lower
                        .namespace
                        .snapshot
                        .validation_input_digest(package)
                        .ok()
                },
            )?)
        } else {
            None
        };
        let bytes = self.lower.store.get_content(stored.output)?;
        let (definition, text, reads, depth, dependencies): (
            DefId,
            Result<String, QueryError>,
            Reads,
            DepthEvidence<Span>,
            Vec<SignatureDependency>,
        ) = self
            .lower
            .records
            .decode(&*self.lower.namespace.anchors, &bytes)?;
        let reads = evidence::current_reads(reads, acceptance.as_ref())?;
        if definition != input.definition
            || text.is_err()
            || reads.get(&ReadKey::DeclaredSignature(input.definition))
                != Some(input.declared_header)
            || reads
                .first_changed(|read| self.observe(input, read))
                .is_some()
            || depth.bounded
            || depth.first.len()
                != dependency_limit(self.lower.namespace.limits.call_depth) as usize + 1
            || depth
                .first
                .iter()
                .flatten()
                .any(|site| self.lower.namespace.sources.slice(*site).is_none())
        {
            return None;
        }
        let persisted = persistent::persisted_execution_reads(&reads, acceptance.as_ref())?;
        if persisted != variant.reads {
            return None;
        }
        Some(Data {
            diagnostic_sources: None,
            definition: input.definition,
            manifest: self.lower.namespace.manifest,
            text,
            reads,
            dependencies,
            depth,
            acceptance,
            failure: None,
            _allowance: None,
        })
    }
    fn remember(
        &self,
        input: &Input,
        output: Reference,
        data: &Data,
        decode_bytes: u64,
        manifest: ContentHash,
    ) {
        if !self.lower.store.persistence().publishes() || data.acceptance.is_none() {
            return;
        }
        let Some(reads) =
            persistent::persisted_execution_reads(&data.reads, data.acceptance.as_ref())
        else {
            return;
        };
        let key = Self::storage_key(input);
        let Ok(payload) = bincode::serialize(&Stored {
            manifest,
            output,
            decode_bytes,
        }) else {
            return;
        };
        // The local execution association binds both the checked output
        // reference and its measured decode bound before any restoration.
        if let Some(receipt) = &data.acceptance
            && let Some(cache) = self.lower.store.verifier()
        {
            cache.publish_execution(
                persistent::evaluation_subject(&key, &reads, &payload),
                receipt,
            );
        }
        self.lower.store.cache_query(&key, reads, &payload);
    }
    fn compute(
        &self,
        input: &Input,
        source: &mut Source,
        dependencies: &mut Vec<SignatureDependency>,
    ) -> Result<String, QueryError> {
        let syntax = input.syntax.clone();
        if syntax.item.definition != *input.definition.path()
            || syntax.text_hash != input.implementation
            || !syntax.structurally_valid(|span| self.lower.namespace.sources.slice(span).is_some())
        {
            return Err(QueryError::NotImplemented);
        }
        if crate::queries::generation::instance_bodied(&syntax) {
            return Err(QueryError::NotImplemented);
        }
        let prepared = self.lower.front.prepare_generator(
            syntax,
            &*self.signer,
            !input.instance_reads.is_empty(),
        );
        source.merge(&prepared.reads);
        let PreparedGenerator {
            input: generator,
            checked,
            detail,
            ..
        } = prepared.result?;
        let checked = checked?;
        dependencies.extend_from_slice(&input.checked_prefix);
        // The generator's checked headers are Signature reads through these
        // dependencies, as on the scalar path. Only bodies the evaluation
        // runs record ExecutedSignature.
        dependencies.extend_from_slice(&detail.signatures);
        let mut observed = ReadsBuilder::new();
        let staged = crate::comptime::prepare_staged_types(
            self.lower.namespace.snapshot.types(),
            &checked.analyzed,
            &mut FrozenProvider::new(
                &self.lower.namespace.snapshot,
                input.definition.path().0.package.clone(),
                &mut observed,
            ),
        );
        source.merge(&observed.finish().map_err(|_| QueryError::NotImplemented)?);
        let staged = staged?;
        let constants = crate::comptime::prepare_values(
            self.lower.namespace.snapshot.types(),
            &checked.analyzed,
            &staged,
            &checked.signature,
            &mut Values {
                source: source.clone(),
            },
        )?;
        let value = generator.value;
        let site = checked.analyzed.typed.syntax.tree.expressions[value.0 as usize]
            .origin
            .span;
        let lowered = EvaluationInput {
            analyzed: &checked.analyzed,
            staged: &staged,
            signature: &checked.signature,
            expression: value,
            child: value,
            ty: checked.ty,
            constants: &constants,
            sizes: &[],
        }
        .lower(
            self.lower.namespace.snapshot.types(),
            &self.lower.memo,
            self.lower
                .lowering
                .as_deref()
                .ok_or(QueryError::NotImplemented)?,
        )?;
        let admission = s1::ValidatedEvaluation::recorded_in(
            lowered,
            source.capture(),
            self.lower.verify,
            site,
        );
        source.merge(&admission.reads);
        source.evidence.borrow_mut().executed = true;
        let runtime = source.active_owner().runtime_types()?;
        let admission = admission.result?;
        source
            .evidence
            .borrow_mut()
            .accepted(admission.acceptance().cloned());
        let result = admission.run(
            &source.capture().sources,
            source.capture().limits,
            &runtime,
            source,
        );
        source.join(result.value.successor)?;
        source.merge(&result.value.reads);
        source.evidence.borrow_mut().accepted(result.acceptance);
        generator.decode(result.value.result?.0)
    }
}
struct Factory(Arc<FrontRegistrationQueries>);
impl LocalQueryFactory for Factory {
    fn weight(&self, id: QueryId) -> Result<MemoryWeight, Reference> {
        if id.kind != FRONT_REGISTRATION_KIND || !self.0.inputs.contains_key(&id.address.0) {
            return Err(self.0.errors.protocol);
        }
        Ok(self.0.weight)
    }
    fn create(&self, id: QueryId) -> Result<Box<dyn LocalQueryTask>, Reference> {
        let owner = self.0.clone();
        Ok(Box::new(hirsel_engine_runtime::LocalFnTask::new(
            id,
            self.weight(id)?,
            self.0.errors,
            move |reads| owner.run(id, reads),
        )))
    }
}
impl QueryEngine {
    /// Capture generator declarations only. Running a query evaluates privately;
    /// its output cannot install source until the source transaction accepts it.
    pub fn front_registration_queries(
        &mut self,
        definitions: &[DefPath],
        lower: Arc<FrontLowerQueries>,
        weight: MemoryWeight,
        request: &Arc<hirsel_engine_admission::ProcessBudget>,
        errors: RuntimeErrors,
    ) -> Result<Arc<FrontRegistrationQueries>, QueryError> {
        let output_pool = request.clone();
        let available = output_pool.free();
        let refused = || {
            QueryError::Operational(hirsel_types::OperationalFailure {
            operation: "generator registration admission".into(), message: "generator input inventory exceeds admission or differs from its source snapshot".into(),
        })
        };
        let mut required = FrontRegistrationQueries::input_bytes(definitions);
        if required > available || !Arc::ptr_eq(&self.types, lower.namespace.snapshot.types()) {
            return Err(refused());
        }
        let signer = self.adapters.signature.clone().ok_or_else(refused)?;
        let mut inputs = BTreeMap::new();
        for definition in definitions {
            self.materialize(definition)?;
            let syntax = self.snapshot(definition)?;
            if crate::queries::generation::generator(&syntax).is_none() {
                continue;
            }
            let key = self
                .store_key(definition, QueryKind::Generate)
                .ok_or_else(refused)?;
            required = required
                .checked_add(hirsel_query_products::retained::retained_syntax_bytes(
                    &syntax,
                ))
                .ok_or_else(refused)?;
            if required > available {
                return Err(refused());
            }
            // Generator declarations are absent from the ordinary-body front.
            // Capture their original header requests here, before workers run.
            let signature = self.signature_output(definition)?;
            let prefix_len = signature.dependencies.len();
            let declared_header = generation::declared_header_read(&self.types, &signature.value)?;
            required = required
                .checked_add(
                    prefix_len
                        .checked_mul(size_of::<SignatureDependency>())
                        .ok_or_else(refused)? as u64,
                )
                .ok_or_else(refused)?;
            if required > available {
                return Err(refused());
            }
            let mut checked_prefix = Vec::with_capacity(prefix_len);
            checked_prefix.extend_from_slice(&signature.dependencies);

            let instance_reads = self.front_instance_reads(definition)?;
            for (_, bytes) in instance_reads.allocations() {
                required = required.checked_add(bytes as u64).ok_or_else(refused)?;
            }
            if required > available {
                return Err(refused());
            }
            inputs.insert(
                key.0,
                Input {
                    key,
                    definition: DefId::intern(definition),
                    syntax: syntax.clone(),
                    implementation: syntax.text_hash,
                    site: syntax.item.name_span,
                    instance_reads,
                    checked_prefix,
                    declared_header,
                },
            );
        }
        let inputs_allowance = output_pool.reserve(required).ok().ok_or_else(refused)?;
        Ok(Arc::new(FrontRegistrationQueries {
            inputs,
            lower,
            signer,
            outcomes: Mutex::new(BTreeMap::new()),
            weight,
            errors,
            output_pool,
            _inputs: inputs_allowance,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_generator_data_releases_its_capture_after_the_last_pending_reader() {
        hirsel_intern::jobs::with_compiler_stack(|| {
            let mut engine =
                QueryEngine::new(default_environment(), crate::phases::engine_only_adapters())
                    .with_result_store(Arc::new(hirsel_engine_store::MemStore::default()));
            let file = engine
                .sources
                .add("main.hir", "pub fn main()->u8{1u8}\n".into())
                .unwrap();
            engine.index_source(file, &default_context()).unwrap();
            let definitions = engine.source_paths();
            let capture = engine
                .capture_namespace(&definitions, &[], 8 << 20)
                .unwrap();
            let manifest = capture.manifest;
            let weak = Arc::downgrade(&capture);
            let output = FrontRegistrationOutput {
                namespace: capture,
                data: Arc::new(Data {
                    diagnostic_sources: None,
                    definition: DefId::intern(&definitions[0]),
                    manifest,
                    text: Ok("fn()->u8{1u8}".into()),
                    reads: Reads::empty(),
                    dependencies: vec![],
                    depth: DepthEvidence::leaf(0),
                    acceptance: None,
                    failure: None,
                    _allowance: None,
                }),
            };
            let pending = output.clone();
            let accepted = output.into_data();
            drop(engine);
            assert!(
                weak.upgrade().is_some(),
                "a pending reader retains the exact consumption capture"
            );
            drop(pending);
            assert!(
                weak.upgrade().is_none(),
                "accepted immutable data must not retain its predecessor capture"
            );
            assert_eq!(accepted.text().unwrap(), "fn()->u8{1u8}");
            assert_eq!(accepted.manifest(), manifest);
            assert_eq!(accepted.definition(), &definitions[0]);
        });
    }
}
