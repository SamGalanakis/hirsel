//! Per-definition native source observations from actual admitted P/Lower/
//! Evaluate products. This is ordinary data; V's token is passed through from
//! the exact Lower completion and no native C/Ready authority is constructed.
use super::*;
use crate::native_metadata::NativeMetadataExt;
use hirsel_engine_runtime::*;
use hirsel_engine_store::{Domain, Reference, ResultStore};
use hirsel_types::{SignatureKind, SignatureRef};
use std::{sync::Mutex, task::Poll};

pub const FRONT_NATIVE_DEFINITION_KIND: u32 = 0x4600_0017;
const OUTPUT: Domain = Domain {
    kind: 0x4600_0018,
    schema: 4,
};
const FAILURE: Domain = Domain {
    kind: 0x4600_0019,
    schema: 1,
};
struct SourceOwnership {
    _allowance: Arc<hirsel_engine_admission::MemoryReservation>,
    _operation: OperationLease,
    _front: Arc<FrontBodyQueries>,
    _lower: Arc<FrontLowerQueries>,
}
struct Input {
    generated: Option<AcceptedNativeSource>,
}
struct DemandedInput<'a> {
    signature: SignatureRef,
    representation: Option<crate::representation::OwnerRecord>,
    generated: Option<&'a AcceptedNativeSource>,
}
struct AcceptedNativeSource {
    source: crate::NativeSource,
    registration: Arc<AcceptedFrontRegistration>,
}
/// Prepared bodyless catalogue inputs. Sizing and installation consume the
/// same private data; the caller cannot supply source V or native authority.
pub struct FrontNativeDefinitionInputs {
    definitions: Vec<DefPath>,
    operation: OperationLease,
    original_context: crate::NativeOriginalContext,
    inputs: BTreeMap<DefPath, Input>,
    ids: BTreeMap<DefPath, QueryId>,
    records: crate::NativeRecords,
    anchors: crate::NativeRecordContext,
    store: Arc<dyn ResultStore>,
    required: u64,
}
impl FrontNativeDefinitionInputs {
    pub fn input_bytes(&self) -> u64 {
        self.required
    }
}
struct Data {
    product: Arc<crate::NativeDiscoveryProduct>,
    reads: Arc<Reads>,
    original: Option<Arc<crate::NativeOriginalProduct>>,
}
/// Prepared decoded catalogue backing. Its retained layout is fixed before
/// publication; Original metadata and active archive readers have separate leases.
pub struct PreparedNativeDefinition {
    pub source: Arc<crate::NativeDefinitionProduct>,
    pub contracts: Option<crate::NativeBodyContracts>,
    pub requirements: InstanceGraph,
    pub reads: Arc<Reads>,
    original: Option<(ContentHash, ContentHash)>,
    records: crate::NativeRecords,
    anchors: crate::NativeRecordContext,
    archive: Mutex<Option<Reference>>,
    store: Arc<dyn ResultStore>,
    remaining: Arc<Mutex<u64>>,
    decoded_bytes: u64,
    archive_bytes: u64,
    charge: u64,
    _ownership: Arc<SourceOwnership>,
    _capture: Arc<FrontLowerQueries>,
}
impl Drop for PreparedNativeDefinition {
    fn drop(&mut self) {
        *self.remaining.lock().unwrap_or_else(|e| e.into_inner()) += self.charge;
    }
}
impl PreparedNativeDefinition {
    /// The prepared decoded backing, excluding independently leased archives
    /// and Original metadata. Planning reuses this exact admitted layout.
    pub fn retained_bytes(&self) -> u64 {
        self.decoded_bytes
    }
    /// Publication must leave room for this decoded value and one retained
    /// archive reader. The reader debits only its own backing when opened.
    pub fn publication_bytes(&self) -> Option<u64> {
        self.decoded_bytes.checked_add(self.archive_bytes)
    }
    fn prepare_layout(
        &mut self,
        output_contracts: &Option<crate::NativeBodyContracts>,
        output_requirements: &InstanceGraph,
    ) {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(
            size_of::<Self>()
                + size_of::<Data>()
                + size_of::<crate::NativeDiscoveryProduct>()
                + size_of::<crate::NativeDefinitionProduct>()
                + size_of::<Reads>()
                + 10 * size_of::<usize>(),
        );
        weight.source(&self.source);
        for (_, bytes) in self.reads.allocations() {
            weight.add(bytes);
        }
        for (contracts, requirements) in [
            (&self.contracts, &self.requirements),
            (output_contracts, output_requirements),
        ] {
            if let Some(contracts) = contracts {
                weight.flat_vec(&contracts.source_anchors);
                weight.flat_vec(&contracts.entry_parameters);
                weight.flat_vec(&contracts.caller_bodies);
                weight.vec(&contracts.scoped_parameters, |w, body| {
                    w.vec(body, |w, (_, parameters)| w.flat_vec(parameters))
                });
            }
            weight.vec(&requirements.edges, |w, edge| {
                w.path(&edge.owner);
                w.path(&edge.callee);
                w.flat_vec(&edge.arguments);
            });
        }
        self.decoded_bytes = weight.bytes();
    }
    fn encode(&self) -> Option<Vec<u8>> {
        let source = &self.source;
        let lower_identity = source
            .lower
            .as_ref()
            .map(|v| (v.reference(), v.observation_identity()));
        self.records.encode(
            &self.anchors,
            &(
                &source.definition,
                &source.signature,
                &source.signature_dependencies,
                source.source.syntax.text_hash,
                source
                    .source
                    .generated
                    .as_ref()
                    .map(|source| (&source.text, source.site)),
                &source.summary,
                source.summary_identity,
                (self.original, lower_identity),
                &source.audit,
                &source.materialized,
                source.representation.as_ref().map(|r| {
                    (
                        &r.definition,
                        r.representation_revision,
                        &r.generics,
                        r.private_shape,
                        &r.layout_promises,
                        r.resource,
                        &r.finalizer,
                        r.unsafe_ffi_package,
                    )
                }),
                &source.constant,
                &self.contracts,
                &self.requirements,
                &*self.reads,
            ),
        )
    }
}
impl LiveProductValue for PreparedNativeDefinition {
    fn domain(&self) -> Domain {
        OUTPUT
    }
    fn archive(&self, admission: ArchiveAdmission<'_>) -> std::io::Result<ProductArchive> {
        let cached = self.archive.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(reference) = *cached {
            return ProductArchive::open_retained(
                &*self.store,
                reference,
                self._ownership._allowance.clone(),
                self.remaining.clone(),
            );
        }
        drop(cached);
        ProductArchive::materialize(OUTPUT, admission, |grant| {
            if grant.bytes() < self.charge {
                return Err(std::io::ErrorKind::OutOfMemory.into());
            }
            let bytes = self
                .encode()
                .ok_or_else(|| std::io::Error::other("native definition encoding"))?;
            if bytes.len() as u64 > grant.bytes() {
                return Err(std::io::ErrorKind::OutOfMemory.into());
            }
            Ok(bytes)
        })
    }
}
/// Shares the settled product its owner charged to the output budget, so a
/// reader holds handles rather than another decoded copy.
pub struct FrontNativeDefinitionOutput {
    pub product: Arc<crate::NativeDiscoveryProduct>,
    pub reads: Arc<Reads>,
    pub original: Option<Arc<crate::NativeOriginalProduct>>,
    _owner: Arc<Data>,
}
pub struct FrontNativeDefinitionQueries {
    selection: Option<(Arc<FrontRunQueries>, DefPath)>,
    inputs: BTreeMap<DefPath, Input>,
    ids: BTreeMap<DefPath, QueryId>,
    original_context: Arc<crate::NativeOriginalContext>,
    front: Arc<FrontBodyQueries>,
    lower: Arc<FrontLowerQueries>,
    records: crate::NativeRecords,
    anchors: crate::NativeRecordContext,
    store: Arc<dyn ResultStore>,
    completed: Mutex<BTreeMap<ContentHash, (ProductRef, Arc<Data>)>>,
    failures: Mutex<BTreeMap<ContentHash, (Reference, QueryError)>>,
    remaining: Arc<Mutex<u64>>,
    weight: MemoryWeight,
    errors: RuntimeErrors,
    _ownership: Arc<SourceOwnership>,
}
impl FrontNativeDefinitionQueries {
    #[track_caller]
    fn protocol_failure(&self, query: QueryId, detail: &str) -> Reference {
        crate::requests::source_graph::protocol_failure(
            &*self.store,
            self.errors,
            Some(query),
            detail,
        )
    }
    /// Fixed inputs and currently held output or archive-reader charges.
    pub fn retained_bytes(&self) -> u64 {
        self._ownership._allowance.bytes()
            - *self.remaining.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn input(&self, path: &DefPath) -> Result<DemandedInput<'_>, QueryError> {
        self.input_in(path, self.front.namespace())
    }
    fn input_in<'a>(
        &'a self,
        path: &DefPath,
        namespace: &NamespaceCapture,
    ) -> Result<DemandedInput<'a>, QueryError> {
        let signature = namespace.header(path)?.output.value.clone();
        let representation = namespace.representation(path);
        if representation.as_ref().is_some_and(|record| {
            namespace
                .bodies
                .get(&DefId::intern(path))
                .map(|input| input.implementation)
                != Some(record.text_hash)
        }) {
            return Err(native_selection_error(format!(
                "native representation revision differs from source: {path}"
            )));
        }
        Ok(DemandedInput {
            signature,
            representation,
            generated: self
                .inputs
                .get(path)
                .ok_or(QueryError::NotImplemented)?
                .generated
                .as_ref(),
        })
    }
    pub(crate) fn with_selection(
        mut self: Arc<Self>,
        run: Arc<FrontRunQueries>,
        root: DefPath,
    ) -> Result<Arc<Self>, QueryError> {
        if !run.matches_inputs(&self.front, &self.lower) {
            return Err(native_selection_error(
                "native catalogue and Run have different immutable inputs",
            ));
        }
        let query = run
            .preparation_query(&root)
            .ok_or_else(|| native_selection_error("native selection root is missing"))?;
        let this = Arc::get_mut(&mut self).ok_or_else(|| {
            native_selection_error("native selection requires an unshared catalogue")
        })?;
        for id in this.ids.values_mut() {
            id.address = hirsel_artifact_store::Address(ContentHash::of_parts(
                "hirsel.front.native-selected-address.v1",
                [id.address.0.0.as_slice(), query.address.0.0.as_slice()],
            ));
        }
        this.selection = Some((run, root));
        Ok(self)
    }
    pub(crate) fn inputs_contains(&self, path: &DefPath) -> bool {
        self.inputs.contains_key(path)
    }
    pub(crate) fn constant_reads(&self, path: &DefPath) -> Option<Arc<Reads>> {
        self.lower.settled_reads(path, QueryKind::Evaluate)
    }
    pub(crate) fn staged_static_query(
        &self,
        selected: &[DefPath],
        completed: &[QueryId],
    ) -> Option<QueryId> {
        self.lower.staged_static_query(selected, completed)
    }
    pub(crate) fn staged_static_inputs(
        &self,
        selected: &BTreeSet<DefPath>,
        allowance: u64,
    ) -> Result<(crate::NativeStaticInputs, u64), QueryError> {
        self.lower.staged_static_inputs(selected, allowance)
    }
    pub fn original_context(&self) -> Arc<crate::NativeOriginalContext> {
        self.original_context.clone()
    }

    pub(crate) fn lower(&self) -> &Arc<FrontLowerQueries> {
        &self.lower
    }

    pub fn signatures(&self) -> Arc<SignatureSnapshot> {
        self.lower.signatures()
    }

    pub fn input_bytes(definitions: &[DefPath]) -> Option<u64> {
        definitions.iter().try_fold(
            (std::mem::size_of::<Self>()
                + std::mem::size_of::<SourceOwnership>()
                + std::mem::size_of::<crate::NativeOriginalContext>()
                + std::mem::size_of::<BTreeMap<DefPath, ContentHash>>()
                + 4 * std::mem::size_of::<usize>()
                + std::mem::size_of::<Mutex<u64>>()
                + std::mem::size_of::<hirsel_engine_admission::MemoryReservation>()
                + 4 * std::mem::size_of::<usize>()) as u64,
            |total, path| {
                total
                    .checked_add(bincode::serialized_size(path).ok()?.checked_mul(3)?)?
                    .checked_add(
                        (std::mem::size_of::<Input>() + 16 * std::mem::size_of::<usize>()) as u64,
                    )
            },
        )
    }
    pub fn query(&self, path: &DefPath) -> Option<QueryId> {
        self.ids.get(path).copied()
    }
    pub fn factory(self: &Arc<Self>) -> Arc<dyn QueryFactory> {
        Arc::new(Factory(self.clone()))
    }
    pub fn outcome(self: &Arc<Self>, product: ProductRef) -> Option<FrontNativeDefinitionOutput> {
        let values = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        let (selected, data) = values.get(&product.address.0)?;
        (*selected == product).then(|| FrontNativeDefinitionOutput {
            product: data.product.clone(),
            original: data.original.clone(),
            reads: data.reads.clone(),
            _owner: data.clone(),
        })
    }
    pub(crate) fn export_archive(
        &self,
        product: &ProductRef,
        scratch: u64,
    ) -> Result<ProductArchive, QueryError> {
        if !self
            .completed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&product.address.0)
            .is_some_and(|(selected, _)| selected == product)
        {
            return Err(native_selection_error("native export has another producer"));
        }
        let live = product
            .live_value::<PreparedNativeDefinition>()
            .ok_or(QueryError::NotImplemented)?;
        let mut cached = live.archive.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(reference) = *cached {
            return ProductArchive::open_retained(
                &*self.store,
                reference,
                self._ownership._allowance.clone(),
                self.remaining.clone(),
            )
            .map_err(|_| native_selection_error("native export exceeds retained admission"));
        }
        if live.charge > scratch {
            return Err(native_selection_error(
                "native export exceeds scratch admission",
            ));
        }
        let bytes = live.encode().ok_or_else(|| {
            native_selection_error(format!(
                "native export encoding for {}",
                live.source.definition
            ))
        })?;
        if bytes.len() as u64 > scratch {
            return Err(native_selection_error(
                "native export encoding exceeds scratch admission",
            ));
        }
        let record = self
            .store
            .put_content(OUTPUT, &bytes)
            .ok_or(QueryError::NotImplemented)?;
        let archive = ProductArchive::from_retained_bytes(
            record,
            bytes.into(),
            self._ownership._allowance.clone(),
            self.remaining.clone(),
        )
        .map_err(|_| native_selection_error("native export exceeds retained admission"))?;
        if self.store.persistence().retains() {
            self.store
                .associate_product(product.address, product.action, record);
        }
        *cached = Some(record);
        Ok(archive)
    }
    pub fn failure(&self, reference: Reference) -> Option<QueryError> {
        self.failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&reference.output.0)
            .and_then(|(selected, error)| (*selected == reference).then(|| error.clone()))
            .or_else(|| self.lower.failure(reference))
            .or_else(|| {
                self.front
                    .outcome(Err(reference))
                    .and_then(|o| o.outcome.result.err())
            })
    }
    fn fail(&self, error: QueryError) -> Reference {
        let Some(bytes) = self.records.encode(&self.anchors, &error) else {
            return self.errors.resource;
        };
        if bytes.len() as u64 > self.weight.scratch {
            return self.errors.resource;
        }
        let Some(reference) = self.store.put_content(FAILURE, &bytes) else {
            return self.errors.resource;
        };
        self.failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(reference.output.0, (reference, error));
        reference
    }
}
struct Factory(Arc<FrontNativeDefinitionQueries>);
impl QueryFactory for Factory {
    fn create(&self, id: QueryId) -> Result<Box<dyn QueryTask>, Reference> {
        let path = self
            .0
            .ids
            .iter()
            .find_map(|(path, query)| (*query == id).then(|| path.clone()))
            .ok_or_else(|| {
                self.0
                    .protocol_failure(id, "native definition has no immutable input")
            })?;
        let terminal = self
            .0
            .completed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id.address.0)
            .map(|(p, _)| Ok(p.clone()));
        Ok(Box::new(Task {
            owner: self.0.clone(),
            path,
            id,
            waiting: None,
            terminal,
        }))
    }
}
struct Task {
    owner: Arc<FrontNativeDefinitionQueries>,
    path: DefPath,
    id: QueryId,
    waiting: Option<QueryId>,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl QueryTask for Task {
    fn id(&self) -> QueryId {
        self.id
    }
    fn weight(&self) -> MemoryWeight {
        self.owner.weight
    }
    /// A settled output is charged to the owner's output budget, so only
    /// this frame holds the runtime grant.
    fn continuation_bytes(&self) -> u64 {
        self.owner.weight.retained
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        let result = if let Some(result) = self.terminal.clone() {
            result
        } else {
            match self.advance(cx) {
                Poll::Pending => return QueryPoll::Waiting(self.waiting.into_iter().collect()),
                Poll::Ready(result) => {
                    self.terminal = Some(result.clone());
                    result
                }
            }
        };
        if result.is_ok() {
            for query in [
                self.owner.front.query(&self.path),
                self.owner.front.body_query(&self.path),
                self.owner.lower.query(&self.path),
                self.owner.lower.evaluate_query(&self.path),
            ]
            .into_iter()
            .flatten()
            {
                cx.consume(query);
            }
        }
        match result {
            Ok(product) => QueryPoll::Complete(product),
            Err(error) => QueryPoll::Failed(error),
        }
    }
}
impl Task {
    fn read(
        &mut self,
        cx: &mut dyn QueryContext,
        id: QueryId,
    ) -> Poll<Result<ProductRef, Reference>> {
        match cx.read(id) {
            Poll::Pending => {
                self.waiting = Some(id);
                Poll::Pending
            }
            Poll::Ready(result) => {
                self.waiting = None;
                Poll::Ready(result)
            }
        }
    }
    fn advance(&mut self, cx: &mut dyn QueryContext) -> Poll<Result<ProductRef, Reference>> {
        let owner = self.owner.clone();
        let input = match owner.input(&self.path) {
            Ok(input) => input,
            Err(error) => return Poll::Ready(Err(owner.fail(error))),
        };
        let Some(id) = owner.front.query(&self.path) else {
            return Poll::Ready(Err(owner.protocol_failure(
                self.id,
                "native definition producer or observation is unavailable",
            )));
        };
        let selected = match self.read(cx, id) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(r)) => return Poll::Ready(Err(r)),
            Poll::Ready(Ok(p)) => p,
        };
        let front = match owner.front.outcome(Ok(selected.clone())) {
            Some(front) => front,
            None => {
                let Some(id) = owner.front.body_query(&self.path) else {
                    return Poll::Ready(Err(owner.protocol_failure(
                        self.id,
                        "native definition producer or observation is unavailable",
                    )));
                };
                let selected = match self.read(cx, id) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Ready(Ok(product)) => product,
                };
                let Some(front) = owner.front.outcome(Ok(selected.clone())) else {
                    return Poll::Ready(Err(owner.protocol_failure(
                        self.id,
                        "native definition producer or observation is unavailable",
                    )));
                };
                front
            }
        };
        let analyzed = match &front.outcome.result {
            Ok(body) => body,
            Err(error) => return Poll::Ready(Err(owner.fail(error.clone()))),
        };
        if has_errors(&analyzed.diagnostics) {
            return Poll::Ready(Err(
                owner.fail(QueryError::Diagnostics(analyzed.diagnostics.clone()))
            ));
        }
        let syntax = analyzed.value.typed.syntax.clone();
        let executable = generation::executable(&syntax)
            && native_declaration(owner.lower.signatures().types(), &input.signature);
        let mut reads = ReadsBuilder::new();
        let mut observations = Vec::new();
        let mut ordinary = None;
        let lower = if executable {
            let Some(id) = owner.lower.query(&self.path) else {
                return Poll::Ready(Err(owner.protocol_failure(
                    self.id,
                    "native definition producer or observation is unavailable",
                )));
            };
            let selected = match self.read(cx, id) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(r)) => return Poll::Ready(Err(r)),
                Poll::Ready(Ok(p)) => p,
            };
            ordinary = owner.lower.native_product(selected.clone());
            if let Some(original) = ordinary.as_mut() {
                let admission = crate::native_lower::OriginalAdmission {
                    _allowance: owner._ownership._allowance.clone(),
                    remaining: owner.remaining.clone(),
                };
                let Some(body) = original.body.clone().admit(&admission) else {
                    return Poll::Ready(Err(owner.errors.resource));
                };
                original.body = body;
            }
            let Some(output) = owner.lower.outcome(selected) else {
                return Poll::Ready(Err(owner.protocol_failure(
                    self.id,
                    "native definition producer or observation is unavailable",
                )));
            };
            observations.push((id, Arc::new(output.reads.clone())));
            Some(output)
        } else {
            None
        };
        let finalizer = if let Some(path) = input
            .representation
            .as_ref()
            .and_then(|r| r.representation.declaration.drop.as_ref())
        {
            let Some(id) = owner.lower.query(path) else {
                return Poll::Ready(Err(owner.protocol_failure(
                    self.id,
                    "native definition producer or observation is unavailable",
                )));
            };
            let selected = match self.read(cx, id) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(r)) => return Poll::Ready(Err(r)),
                Poll::Ready(Ok(p)) => p,
            };
            let Some(output) = owner.lower.outcome(selected) else {
                return Poll::Ready(Err(owner.protocol_failure(
                    self.id,
                    "native definition producer or observation is unavailable",
                )));
            };
            observations.push((id, Arc::new(output.reads.clone())));
            Some((path.clone(), output.output.value.entry))
        } else {
            None
        };
        let constant = if matches!(&input.signature.kind,SignatureKind::Const{generics,..} if generics.is_empty())
        {
            let Some(id) = owner.lower.evaluate_query(&self.path) else {
                return Poll::Ready(Err(owner.protocol_failure(
                    self.id,
                    "native definition producer or observation is unavailable",
                )));
            };
            match self.read(cx, id) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(r)) => return Poll::Ready(Err(r)),
                Poll::Ready(Ok(_)) => {}
            }
            let Some(observed) = owner.lower.settled_reads(&self.path, QueryKind::Evaluate) else {
                return Poll::Ready(Err(owner.protocol_failure(
                    self.id,
                    "native definition producer or observation is unavailable",
                )));
            };
            observations.push((id, observed));
            match owner.lower.settled_constant(&self.path, &[]) {
                Ok(value) => Some((*value).clone()),
                Err(error) => return Poll::Ready(Err(owner.fail(error))),
            }
        } else {
            None
        };
        if let Some((run, root)) = &owner.selection {
            let Some(id) = run.preparation_query(root) else {
                return Poll::Ready(Err(owner.protocol_failure(
                    self.id,
                    "native definition producer or observation is unavailable",
                )));
            };
            let product = match self.read(cx, id) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(product)) => product,
            };
            let Some(output) = run.outcome(product) else {
                return Poll::Ready(Err(owner.protocol_failure(
                    self.id,
                    "native definition producer or observation is unavailable",
                )));
            };
            if !output.semantic.semantic_contains(&self.path) {
                return Poll::Ready(Err(owner.fail(native_selection_error(
                    "definition is outside the native source selection",
                ))));
            }
        }
        // Lower/Evaluate and Run preparation can commit source registrations.
        // Read Front in that committed successor and join exact settled reads
        // through ordinary semantic checking and V revalidation.
        let current = owner.lower.clone();
        let current_front = current.body_front();
        let Some(id) = current_front.body_query(&self.path) else {
            return Poll::Ready(Err(
                owner.protocol_failure(self.id, "native successor Front body is unavailable")
            ));
        };
        let selected = match self.read(cx, id) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(product)) => product,
        };
        let Some(front) = current_front.outcome(Ok(selected)) else {
            return Poll::Ready(Err(owner.protocol_failure(
                self.id,
                "native successor Front outcome is unavailable",
            )));
        };
        reads.merge(&front.outcome.reads);
        for (id, observed) in observations {
            let observed = match owner.lower.check_observations(id, observed) {
                Ok(observed) => observed,
                Err(error) => return Poll::Ready(Err(owner.fail(error))),
            };
            reads.merge(&observed);
        }
        let result = (|| {
            let reads = Arc::new(reads.finish().map_err(|error| {
                owner.protocol_failure(
                    self.id,
                    &format!(
                        "native definition {} has conflicting observations: {error:?}",
                        self.path
                    ),
                )
            })?);
            let (source, contracts, requirements) = self
                .complete(current_front, &front, &lower, finalizer, constant)
                .map_err(|e| owner.fail(e))?;
            let action = hirsel_artifact_store::Action(ContentHash::of_parts(
                "hirsel.front.native-definition-action.v2",
                [
                    self.id.address.0.0.as_slice(),
                    source.source.syntax.text_hash.0.as_slice(),
                    source.signature.fingerprint.0.as_slice(),
                    source
                        .summary_identity
                        .as_ref()
                        .map_or(&[][..], |identity| identity.0.as_slice()),
                    reads.digest().0.as_slice(),
                ],
            ));
            let source = Arc::new(source);
            let mut prepared = PreparedNativeDefinition {
                source: source.clone(),
                contracts: contracts.clone(),
                requirements: requirements.clone(),
                reads: reads.clone(),
                original: ordinary
                    .as_ref()
                    .map(|p| (p.implementation_revision, p.signature)),
                records: owner.records.clone(),
                anchors: (*current.namespace().anchors).clone(),
                archive: Mutex::new(None),
                store: owner.store.clone(),
                remaining: owner.remaining.clone(),
                decoded_bytes: 0,
                archive_bytes: 0,
                charge: 0,
                _ownership: owner._ownership.clone(),
                _capture: current.clone(),
            };
            prepared.prepare_layout(&contracts, &requirements);
            if owner.store.persistence().retains() {
                let bytes = prepared.encode().ok_or(owner.errors.resource)?;
                if bytes.len() as u64 > owner.weight.scratch {
                    return Err(owner.errors.resource);
                }
                let record = owner
                    .store
                    .put_content(OUTPUT, &bytes)
                    .ok_or(owner.errors.resource)?;
                let backing = owner
                    .store
                    .get_content(record)
                    .ok_or(owner.errors.resource)?;
                prepared.archive_bytes = ProductArchive::retained_required_bytes(record, &backing)
                    .map_err(|_| owner.errors.resource)?;
                *prepared.archive.lock().unwrap_or_else(|e| e.into_inner()) = Some(record);
            }
            let required = prepared.publication_bytes().ok_or(owner.errors.resource)?;
            {
                let mut remaining = owner.remaining.lock().unwrap_or_else(|e| e.into_inner());
                if *remaining < required {
                    return Err(owner.errors.resource);
                }
                *remaining -= prepared.decoded_bytes;
                prepared.charge = prepared.decoded_bytes;
            }
            let live = Arc::new(prepared);
            if let Some(record) = *live.archive.lock().unwrap_or_else(|e| e.into_inner()) {
                owner
                    .store
                    .associate_product(self.id.address, action, record);
            }
            let product = ProductRef::live(self.id.address, action, live);
            owner
                .completed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    self.id.address.0,
                    (
                        product.clone(),
                        Arc::new(Data {
                            product: Arc::new(crate::NativeDiscoveryProduct {
                                product: Some(product.clone()),
                                source,
                                contracts,
                                requirements,
                            }),
                            reads,
                            original: ordinary.map(Arc::new),
                        }),
                    ),
                );
            Ok(product)
        })();
        if result.is_ok() {
            cx.consume(id);
        }
        Poll::Ready(result)
    }
    fn complete(
        &self,
        front_owner: &FrontBodyQueries,
        front: &FrontBodyOutput,
        lower: &Option<FrontLowerOutput>,
        finalizer: Option<(DefPath, hirsel_tir::BodyId)>,
        constant: Option<hirsel_tir::Materialized>,
    ) -> Result<
        (
            crate::NativeDefinitionProduct,
            Option<crate::NativeBodyContracts>,
            InstanceGraph,
        ),
        QueryError,
    > {
        let owner = &self.owner;
        let input = owner.input_in(&self.path, front_owner.namespace())?;
        let analyzed = front.outcome.result.as_ref().map_err(Clone::clone)?;
        let (syntax, signature) = front_owner
            .native_input(&self.path)
            .ok_or(QueryError::NotImplemented)?;
        let summary = front
            .outcome
            .summary
            .clone()
            .ok_or(QueryError::NotImplemented)?;
        let representation = input
            .representation
            .clone()
            .map(|record| {
                crate::representation::completed_owner_record(
                    owner.lower.signatures().types(),
                    &self.path,
                    &input.signature,
                    record,
                    finalizer,
                )
            })
            .transpose()?;
        let executable = generation::executable(&syntax)
            && native_declaration(owner.lower.signatures().types(), &input.signature);
        let source = crate::NativeDefinitionProduct {
            definition: self.path.clone(),
            signature: input.signature.clone(),
            signature_dependencies: signature.dependencies.clone(),
            source: input.generated.as_ref().map_or_else(
                || crate::NativeSource {
                    implementation_revision: syntax.text_hash,
                    syntax,
                    generated: None,
                },
                |accepted| accepted.source.clone(),
            ),
            summary_identity: summaries::summary_archive_identity(
                owner.lower.signatures().types().clone(),
                &summary.summary,
                &summary.growth,
                summary.checked.as_deref(),
            ),
            summary,
            lower: lower.as_ref().and_then(|o| o.verified.clone()),
            audit: executable.then(|| {
                crate::SourceAudit::completed(
                    owner.lower.signatures().types(),
                    &input.signature,
                    analyzed.value.typed.syntax.text_hash,
                    &analyzed.value,
                    representation.as_ref(),
                )
            }),
            materialized: lower
                .as_ref()
                .map_or_else(Vec::new, |o| o.materialized.clone()),
            representation,
            constant,
        };
        let body = lower.as_ref().map(|o| &*o.output.value);
        let requirements = crate::generic_graph::native_definition(
            owner.lower.signatures().types(),
            &source,
            body,
            &mut |path| {
                if !owner.inputs.contains_key(path) {
                    return Ok(None);
                }
                if let Some((run, root)) = &owner.selection {
                    let selected = run
                        .preparation_query(root)
                        .and_then(|id| run.completed_product(id));
                    let Some(selected) = selected.and_then(|p| run.outcome(p)) else {
                        return Err(QueryError::NotImplemented);
                    };
                    if !selected.semantic.semantic_contains(path) {
                        return Ok(None);
                    }
                }
                if !native_declaration(
                    owner.lower.signatures().types(),
                    &owner.input(path)?.signature,
                ) {
                    return Ok(None);
                }
                let (syntax, _) = front_owner
                    .native_input(path)
                    .ok_or(QueryError::NotImplemented)?;
                Ok(Some(syntax.owner().site(hirsel_types::NameSite::Item)))
            },
        )?;
        let contracts = body.map(crate::NativeBodyContracts::of).transpose()?;
        Ok((source, contracts, requirements))
    }
}
impl QueryEngine {
    /// Capture only existing ordinary source headers. Generated namespaces must
    /// supply their actual accepted registration view; they are never invented.
    pub fn front_native_definition_queries(
        &mut self,
        definitions: &[DefPath],
        front: Arc<FrontBodyQueries>,
        lower: Arc<FrontLowerQueries>,
        weight: MemoryWeight,
        allowance: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
    ) -> Result<Arc<FrontNativeDefinitionQueries>, QueryError> {
        let inputs = self.front_native_definition_inputs_in(
            definitions,
            front.namespace().clone(),
            allowance.bytes(),
        )?;
        self.front_native_definition_queries_with_inputs(
            front, lower, inputs, weight, allowance, errors,
        )
    }

    pub fn front_native_definition_inputs(
        &mut self,
        definitions: &[DefPath],
        limit: u64,
    ) -> Result<FrontNativeDefinitionInputs, QueryError> {
        self.prepare_private_summaries();
        let namespace = self.capture_namespace(definitions, &[], limit)?;
        self.front_native_definition_inputs_in(definitions, namespace, limit)
    }
    pub fn front_native_definition_inputs_in(
        &mut self,
        definitions: &[DefPath],
        namespace: Arc<NamespaceCapture>,
        limit: u64,
    ) -> Result<FrontNativeDefinitionInputs, QueryError> {
        self.front_native_definition_inputs_with_lease(definitions, namespace.lease(), limit)
    }
    /// Prepare only source authority published by this exact operation. The
    /// capture's undeveloped declarations remain available to semantic lookup.
    pub fn front_native_catalogue_inputs(
        &mut self,
        operation: OperationLease,
        limit: u64,
    ) -> Result<FrontNativeDefinitionInputs, QueryError> {
        let definitions = operation.native_sources().cloned().collect::<Vec<_>>();
        self.front_native_definition_inputs_with_lease(&definitions, operation, limit)
    }
    fn front_native_definition_inputs_with_lease(
        &mut self,
        definitions: &[DefPath],
        operation: OperationLease,
        limit: u64,
    ) -> Result<FrontNativeDefinitionInputs, QueryError> {
        let namespace = operation.namespace();
        namespace.require_environment(self)?;
        let registration =
            |path: &DefPath| namespace.registrations.get(&DefId::intern(path)).cloned();
        let refuse = || {
            QueryError::Operational(hirsel_types::OperationalFailure{operation:"native source admission".into(),message:"native source inputs exceed admission or differ from the pinned source namespace".into()})
        };
        let mut required =
            FrontNativeDefinitionQueries::input_bytes(definitions).ok_or_else(refuse)?;
        if required > limit {
            return Err(refuse());
        }
        for path in definitions {
            if let Some(accepted) = registration(path) {
                accepted.validate(self)?;
            }
        }
        let original_context = self.native_original_context_in(definitions, namespace)?;
        let store = self.result_store.clone().ok_or_else(refuse)?;
        let records = crate::NativeRecords::new(self.types.clone(), store.clone());
        let anchors = (*namespace.anchors).clone();
        let mut inputs = BTreeMap::new();
        let mut ids = BTreeMap::new();
        let selection_identity = ContentHash::of_parts(
            "hirsel.front.native-definition-selection.v1",
            [serde_json::to_vec(definitions)
                .map_err(|_| refuse())?
                .as_slice()],
        );
        for path in definitions {
            let generated = if namespace
                .bodies
                .get(&DefId::intern(path))
                .is_some_and(|input| input.generated)
            {
                let accepted = registration(path).ok_or_else(|| {
                    native_selection_error(format!("generated native catalogue input: {path}"))
                })?;
                let syntax = namespace.syntax(path)?;
                let source = accepted.source();
                let mut weight = hirsel_query_products::retained::MetadataWeight::default();
                weight.add(
                    hirsel_query_products::retained::retained_syntax_bytes(&syntax)
                        .try_into()
                        .map_err(|_| refuse())?,
                );
                // String::clone copies only the current bytes. Admit that copy
                // and the retained original syntax before allocating the text.
                weight.add(source.text.len());
                required = required.checked_add(weight.bytes()).ok_or_else(refuse)?;
                if required > limit {
                    return Err(refuse());
                }
                Some(AcceptedNativeSource {
                    source: crate::NativeSource {
                        implementation_revision: syntax.text_hash,
                        syntax,
                        generated: Some(source),
                    },
                    registration: accepted,
                })
            } else {
                if registration(path).is_some() {
                    return Err(refuse());
                }
                None
            };
            let bytes = serde_json::to_vec(path).map_err(|_| refuse())?;
            let id = QueryId {
                kind: FRONT_NATIVE_DEFINITION_KIND,
                address: hirsel_artifact_store::Address(ContentHash::of_parts(
                    "hirsel.front.native-definition-address.v2",
                    [
                        operation.identity().0.as_slice(),
                        selection_identity.0.as_slice(),
                        bytes.as_slice(),
                    ],
                )),
            };
            ids.insert(path.clone(), id);
            inputs.insert(path.clone(), Input { generated });
        }
        if required > limit {
            return Err(native_selection_error(format!(
                "native catalogue inputs exceed admission: required {required}, reserved {}",
                limit
            )));
        }
        Ok(FrontNativeDefinitionInputs {
            definitions: definitions.to_vec(),
            operation,
            original_context,
            inputs,
            ids,
            records,
            anchors,
            store,
            required,
        })
    }

    pub fn front_native_definition_queries_with_inputs(
        &mut self,
        front: Arc<FrontBodyQueries>,
        lower: Arc<FrontLowerQueries>,
        inputs: FrontNativeDefinitionInputs,
        weight: MemoryWeight,
        allowance: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
    ) -> Result<Arc<FrontNativeDefinitionQueries>, QueryError> {
        inputs.operation.capture().require_consistent(front.namespace()).map_err(|_| native_selection_error("native catalogue and front use different accepted registrations or namespace captures"))?;
        inputs
            .operation
            .capture()
            .require_consistent(lower.namespace())
            .map_err(|_| {
                native_selection_error(
                    "native catalogue and Lower use different namespace captures",
                )
            })?;
        if inputs
            .definitions
            .iter()
            .any(|path| front.query(path).is_none() || lower.query(path).is_none())
        {
            return Err(native_selection_error(
                "native catalogue is outside front/Lower namespace",
            ));
        }
        for (path, input) in &inputs.inputs {
            if let Some(accepted) = &input.generated {
                accepted.registration.validate(self)?;
                if !front
                    .registration_handle(path)
                    .is_some_and(|registration| Arc::ptr_eq(registration, &accepted.registration))
                {
                    return Err(native_selection_error(
                        "native catalogue and front use different accepted registrations",
                    ));
                }
            } else if front.registration_handle(path).is_some() {
                return Err(native_selection_error(
                    "native catalogue omitted an accepted front registration",
                ));
            }
        }
        if inputs.required > allowance.bytes() {
            return Err(native_selection_error(
                "native catalogue inputs exceed original admission",
            ));
        }
        let FrontNativeDefinitionInputs {
            definitions: _,
            operation,
            original_context,
            inputs,
            ids,
            records,
            anchors,
            store,
            required,
        } = inputs;
        let output_budget = allowance.bytes() - required;
        let ownership = Arc::new(SourceOwnership {
            _operation: operation,
            _front: front.clone(),
            _lower: lower.clone(),
            _allowance: Arc::new(allowance),
        });
        let remaining = Arc::new(Mutex::new(output_budget));
        let original_context = Arc::new(original_context.retain_owner(
            ownership.clone(),
            crate::native_lower::OriginalAdmission {
                _allowance: ownership._allowance.clone(),
                remaining: remaining.clone(),
            },
        ));
        Ok(Arc::new(FrontNativeDefinitionQueries {
            selection: None,
            inputs,
            ids,
            original_context,
            front,
            lower,
            records,
            anchors,
            store,
            completed: Mutex::new(BTreeMap::new()),
            failures: Mutex::new(BTreeMap::new()),
            remaining,
            weight,
            errors,
            _ownership: ownership,
        }))
    }
}

fn native_selection_error(message: impl Into<String>) -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "native source selection".into(),
        message: message.into(),
    })
}
pub(crate) fn native_declaration(
    types: &hirsel_types::TypeInterner,
    signature: &SignatureRef,
) -> bool {
    !matches!(&signature.kind,SignatureKind::Function(function) if function.generics.is_empty() && function.parameters.iter().map(|p|p.ty).chain([function.result]).any(|ty|hirsel_types::mentions_compile_time_only(types,ty)))
}
