//! Demand-driven ordinary Lower and constant evaluation. Rust futures retain
//! preparation and interpreter state on the admitted home worker. Publication
//! into a source namespace remains a separate ordered transaction.
use super::*;
mod carry;
mod constants;
mod dependencies;
mod depth;
pub(super) mod evidence;
mod inputs;
mod inspection;
mod reads;
mod registration;
mod staged;
pub(super) mod staged_runtime;
mod successor;
pub(crate) use successor::SuccessorCapture;
mod stored;
use crate::comptime::{EvaluationInput, PreparingSource, ValuePreparation};
use crate::memo::{DepthEvidence, dependency_limit};
use crate::runtime::*;
use crate::store::{Domain, EntryKey, Reference, ResultStore};
pub use carry::LowerCarry;
use constants::Constants;
use dependencies::{Dependencies, PackedDependencies};
use evidence::Evidence;
use hirsel_syntax::PackageId;
use hirsel_tir::{DefinitionRef, Materialized, MaterializedRef};
use inputs::Inputs;
pub use inspection::FrontLowerDependencies;
pub(in crate::queries) use registration::Data as GeneratorResult;
pub use registration::{
    FRONT_REGISTRATION_KIND, FrontRegistrationOutput, FrontRegistrationQueries,
};
use std::{
    cell::{Cell, RefCell},
    future::{Future, poll_fn},
    pin::Pin,
    rc::Rc,
    sync::Mutex,
    task::{Context, Poll, Waker},
};

pub const FRONT_LOWER_KIND: u32 = 0x4600_0003;
pub const FRONT_EVALUATE_KIND: u32 = 0x4600_0004;
const FAILURE: Domain = Domain {
    kind: 0x4600_0005,
    schema: 3,
};
const LOWER_DESCRIPTOR: Domain = Domain {
    kind: 0x4600_0006,
    schema: 10,
};
// Ordinary immutable substitution bytes, owned by the selected provider like
// the original body archive. Inspectors decode under their working allowance.
const LOWER_MATERIALIZED: Domain = Domain {
    kind: 0x4600_0032,
    schema: 1,
};
const CONSTANT_DESCRIPTOR: Domain = Domain {
    kind: 0x4600_0007,
    schema: 6,
};
const LOWER_VERIFICATION: Domain = Domain {
    kind: 0x4600_0033,
    schema: 1,
};
#[derive(Clone)]
struct StoredVerification {
    authority: hirsel_validate::StoredDefinitionOutput,
    metadata: Reference,
}
/// Physical producer work, including work before an eventual failure.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FrontLowerWork {
    pub lowerings: u64,
    pub initializers: u64,
    pub validation_runs: u64,
}
/// One original's retained decoded artifact and temporary decoding workspace.
/// Sequential inspectors retain every artifact but reuse one scratch region.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DefinitionDecodeLayout {
    pub retained: u64,
    pub transient: u64,
}
impl DefinitionDecodeLayout {
    pub fn total(self) -> Option<u64> {
        self.retained.checked_add(self.transient)
    }
}
struct Input {
    captured: Arc<namespace::BodyKeys>,
    waiting: [Mutex<Option<Span>>; 2],
}
#[derive(Clone)]
enum Value {
    Lower(Box<LowerProduct>),
    Constant(Box<Constants>),
}
type RawLowerOutput = (
    QueryOutput<DefinitionRef>,
    Vec<(hirsel_syntax::ExprId, Materialized)>,
);
struct Computed {
    value: Value,
    verification: Option<(DefinitionRef, hirsel_validate::VerifiedDefinition)>,
    raw: Option<RawLowerOutput>,
}
struct Publication {
    domain: Domain,
    bytes: hirsel_artifact_store::Bytes,
}
#[derive(Clone)]
struct LowerProduct {
    live: Option<WeakLowerProduct>,
    local_verified: Option<hirsel_validate::VerifiedDefinitionOutput>,
    definition: DefId,
    body: Arc<std::sync::OnceLock<Reference>>,
    diagnostics: Vec<Diagnostic>,
    dependencies: Dependencies,
    archived_metadata: Option<Box<reads::Archive>>,
    verified: Option<StoredVerification>,
    acceptance: Option<hirsel_validate::AcceptedArtifact>,
    materialized: Arc<std::sync::OnceLock<Reference>>,
    definition_decode_layout: DefinitionDecodeLayout,
    constant_selection_bytes: u64,
}
impl LowerProduct {
    fn verified(
        &self,
        owner: &FrontLowerQueries,
    ) -> Option<Option<hirsel_validate::VerifiedDefinitionOutput>> {
        if let Some(live) = self.live.as_ref().and_then(WeakLowerProduct::upgrade) {
            return Some(live.observations().verified.clone());
        }
        if self.local_verified.is_some() {
            return Some(self.local_verified.clone());
        }
        let Some(stored) = &self.verified else {
            return Some(None);
        };
        if stored.authority.decode_bytes() > owner.weight.scratch {
            return None;
        }
        let cache = owner.store.verifier()?;
        let bytes = owner.store.get_content(stored.metadata)?;
        stored
            .authority
            .load(
                &cache,
                owner.namespace.snapshot.types(),
                owner
                    .namespace
                    .snapshot
                    .validation(&self.definition.path().0.package)
                    .ok()?,
                &bytes,
                owner.weight.scratch,
            )
            .map(Some)
    }
    fn dependencies(&self, owner: &FrontLowerQueries) -> Option<Vec<SignatureDependency>> {
        if let Some(live) = self.live.as_ref().and_then(WeakLowerProduct::upgrade) {
            return Some(live.output().dependencies.clone());
        }
        match &self.archived_metadata {
            None => Some(self.dependencies.to_vec()),
            Some(archive) => archive.dependencies(
                &owner.records,
                &*owner.namespace.anchors,
                &*owner.store,
                owner.weight.scratch,
            ),
        }
    }
    fn decode(&self, owner: &FrontLowerQueries) -> Option<QueryOutput<DefinitionRef>> {
        if let Some(live) = self.live.as_ref().and_then(WeakLowerProduct::upgrade) {
            return Some(live.output().clone());
        }
        let definition = decode_definition(owner, *self.body.get()?)?;
        if definition.path != self.definition {
            return None;
        }
        Some(QueryOutput {
            value: definition,
            diagnostics: self.diagnostics.clone(),
            dependencies: self.dependencies(owner)?,
        })
    }
}
fn decode_definition(owner: &FrontLowerQueries, body: Reference) -> Option<DefinitionRef> {
    let source = owner.namespace.anchors.clone();
    Some(Arc::new(
        owner
            .records
            .open_tir(source.clone(), owner.store.get_content(body)?, body, source)?
            .materialize()?,
    ))
}
#[derive(Clone)]
struct Outcome {
    diagnostic_sources: Option<Arc<namespace::DiagnosticSources>>,
    result: Result<Value, QueryError>,
    reads: Reads,
    reads_decode_bytes: u64,
    selections: staged::Selections,
    depth: depth::Depth,
    executed: bool,
    execution: Option<hirsel_validate::ExecutionReceipt>,
    execution_subject: ContentHash,
    action: hirsel_artifact_store::Action,
    observations: Option<ProductRef>,
}
impl Outcome {
    fn archive(&self) -> Option<&reads::Archive> {
        match &self.result {
            Ok(Value::Lower(lower)) => lower.archived_metadata.as_deref(),
            Ok(Value::Constant(value)) => value.archived_reads.as_deref(),
            _ => None,
        }
    }
    fn read_len(&self) -> usize {
        if let Ok(Value::Lower(lower)) = &self.result
            && let Some(live) = lower.live.as_ref().and_then(WeakLowerProduct::upgrade)
        {
            return live.reads().len();
        }
        self.archive()
            .map_or_else(|| self.reads.len(), |archive| archive.len)
    }
    fn reads(&self, owner: &FrontLowerQueries) -> Option<Reads> {
        if let Ok(Value::Lower(lower)) = &self.result
            && let Some(live) = lower.live.as_ref().and_then(WeakLowerProduct::upgrade)
        {
            return Some(live.reads().clone());
        }
        match self.archive() {
            None => Some(self.reads.clone()),
            Some(archive) => evidence::current_reads(
                archive.load(
                    &owner.records,
                    &*owner.namespace.anchors,
                    &*owner.store,
                    owner.weight.scratch,
                )?,
                self.execution.as_ref(),
            ),
        }
    }
    fn archive_reads(&mut self, reference: Reference) {
        let descriptor_reads = self.reads.len();
        let read_bytes = self
            .reads
            .allocations()
            .map(|(_, bytes)| bytes)
            .sum::<usize>();
        let archive_bytes = size_of::<reads::Archive>() + 2 * size_of::<usize>();
        match &mut self.result {
            Ok(Value::Lower(lower)) if self.reads_decode_bytes != 0 => {
                // One descriptor owns both sequences; neither retained copy is
                // useful once their joint archive is smaller.
                if read_bytes.saturating_add(lower.dependencies.retained_bytes()) > archive_bytes {
                    lower.archived_metadata = Some(Box::new(reads::Archive {
                        reference,
                        decode_bytes: self.reads_decode_bytes,
                        len: descriptor_reads,
                        dependencies: lower.dependencies.len(),
                    }));
                    self.reads = Reads::empty();
                    lower.dependencies = Vec::new().into();
                }
            }
            Ok(Value::Constant(value)) if read_bytes > archive_bytes => {
                value.archived_reads = Some(Box::new(reads::Archive {
                    reference,
                    decode_bytes: value.working_bytes(),
                    len: descriptor_reads,
                    dependencies: 0,
                }));
                self.reads = Reads::empty();
            }
            _ => {}
        }
    }

    fn retained_bytes(&self) -> Option<u64> {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(size_of::<Self>() + 2 * size_of::<usize>());
        for (_, bytes) in self.reads.allocations() {
            weight.add(bytes);
        }
        weight.add(self.depth.retained_bytes());
        weight.add(staged::selection_bytes(&self.selections).try_into().ok()?);
        match &self.result {
            Ok(Value::Lower(lower)) => {
                weight.add(size_of::<LowerProduct>() + 2 * size_of::<usize>());
                weight.add(
                    crate::memo::retained_lowering_output_bytes(&lower.diagnostics, &Vec::new())
                        .try_into()
                        .ok()?,
                );
                weight.add(lower.dependencies.retained_bytes());
                if lower.archived_metadata.is_some() {
                    weight.add(size_of::<reads::Archive>() + 2 * size_of::<usize>());
                }
                weight.add(
                    2 * (size_of::<std::sync::OnceLock<Reference>>() + 2 * size_of::<usize>()),
                );
                if let Some(verified) = &lower.local_verified {
                    weight.add(verified.retained_bytes()?.try_into().ok()?);
                }
                // AcceptedArtifact and every other inline product field are
                // included in the LowerProduct allocation above.
            }
            Ok(Value::Constant(value)) => {
                weight.add(size_of::<Constants>() + 2 * size_of::<usize>());
                weight.add(value.retained_bytes().try_into().ok()?);
                if value.archived_reads.is_some() {
                    weight.add(size_of::<reads::Archive>() + 2 * size_of::<usize>());
                }
            }
            Err(error) => weight.add(
                crate::memo::retained::retained_query_error_bytes(error)
                    .try_into()
                    .ok()?,
            ),
        }
        if let Some(receipt) = &self.execution {
            weight.add(receipt.retained_bytes()?.try_into().ok()?);
        }
        Some(weight.bytes())
    }
}
// Upper bound for products.rs's temporary required-constant map. Count every
// occurrence, including duplicates and references that will eventually refuse.
fn constant_selection_bytes(definition: &hirsel_tir::Definition) -> Result<u64, QueryError> {
    let mut weight = crate::NativeMetadataWeight::default();
    let mut rows = 1usize; // a constant definition may also require its own value
    weight.path(definition.path.path());
    weight.path(definition.path.path());
    for value in definition.bodies.iter().flat_map(|body| &body.rvalues) {
        if let hirsel_tir::Rvalue::ReadConstant {
            definition,
            arguments,
        } = value
        {
            rows = rows.checked_add(1).ok_or(QueryError::NotImplemented)?;
            weight.path(definition);
            weight.path(definition);
            weight.path(definition); // constant_instance_path's temporary clone
            weight.add(
                arguments
                    .len()
                    .checked_mul(3 * size_of::<u64>() + size_of::<String>() + 80)
                    .ok_or(QueryError::NotImplemented)?,
            );
            weight.add(definition.0.name.len().saturating_mul(2).saturating_add(32));
        }
    }
    weight.tree::<DefPath, (DefPath, Vec<u64>)>(rows);
    Ok(weight.bytes())
}

fn outcome_table_bytes(rows: usize) -> u64 {
    if rows == 0 {
        return 0;
    }
    let mut weight = crate::NativeMetadataWeight::default();
    weight.tree::<(u32, ContentHash), (Option<Reference>, Box<Outcome>, u64)>(rows);
    weight.bytes()
}

/// A held Lower product keeps immutable source/output admission alive. The
/// ordinary archive is data; native consumers independently validate it.
#[derive(Clone)]
pub struct FrontLowerOutput {
    pub output: QueryOutput<DefinitionRef>,
    pub reads: Reads,
    pub depth: DepthEvidence<Span>,
    pub execution: Option<hirsel_validate::ExecutionReceipt>,
    pub execution_subject: ContentHash,
    pub executed: bool,
    pub selections: Vec<(DefId, Vec<u64>)>,
    pub acceptance: Option<hirsel_validate::AcceptedArtifact>,
    pub implementation_revision: ContentHash,
    pub signature: ContentHash,
    pub verified: Option<hirsel_validate::VerifiedDefinitionOutput>,
    pub materialized: Vec<(hirsel_syntax::ExprId, Materialized)>,
    query_observations: Option<ProductRef>,
    body_archive: Arc<std::sync::OnceLock<Reference>>,
    constants_archive: Arc<std::sync::OnceLock<Reference>>,
    owner: Arc<LowerSource>,
}
pub(crate) struct LowerPartition {
    allowance: Arc<hirsel_engine_admission::MemoryReservation>,
    remaining: Arc<Mutex<u64>>,
}
impl LowerPartition {
    pub(crate) fn new(allowance: hirsel_engine_admission::MemoryReservation) -> Self {
        Self {
            remaining: Arc::new(Mutex::new(allowance.bytes())),
            allowance: Arc::new(allowance),
        }
    }
}
struct LowerInputCharge {
    remaining: Arc<Mutex<u64>>,
    bytes: u64,
}
impl Drop for LowerInputCharge {
    fn drop(&mut self) {
        *self.remaining.lock().unwrap_or_else(|e| e.into_inner()) += self.bytes;
    }
}
struct LowerSource {
    _inputs: LowerInputCharge,
    anchors: crate::NativeRecordContext,
    records: crate::NativeRecords,
    store: Arc<dyn ResultStore>,
    _sources: SourceMap,
    _allowance: Arc<hirsel_engine_admission::MemoryReservation>,
}
impl FrontLowerOutput {
    /// Move the typed ordinary result into an admitted live owner. Verifier
    /// capabilities are independent products and cannot be minted by this view.
    pub fn into_live(
        self,
        product: ProductRef,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Option<crate::runtime::LiveLowerProduct> {
        let source = ProductSource::prepare(
            self.owner.anchors.clone(),
            self.owner.records.clone(),
            self.owner.clone(),
        )?;
        crate::runtime::LiveLowerProduct::new(
            (product.address, product.action),
            self,
            source,
            allowance,
        )
    }

    pub fn store(&self) -> &Arc<dyn ResultStore> {
        &self.owner.store
    }
}
#[derive(Clone)]
struct ForwardedLower {
    id: QueryId,
    owner: Arc<FrontLowerQueries>,
    selected: Result<ProductRef, Reference>,
    alias: Result<ProductRef, Reference>,
}
pub struct FrontLowerQueries {
    demand: std::sync::OnceLock<std::sync::Weak<generation::demand::DemandProducer>>,
    demand_owner: Mutex<Option<Arc<generation::demand::DemandProducer>>>,
    forwarded: Mutex<BTreeMap<(u32, ContentHash), ForwardedLower>>,
    namespace: Arc<NamespaceCapture>,
    inputs: Inputs,
    generator: Option<(DefId, Span)>,
    staged: staged::Staged,
    paths: BTreeMap<(u32, ContentHash), DefId>,
    /// Implementation hashes of the opaque types a lowered body can read the
    /// private representation of: its LayoutBody inputs.
    layouts: BTreeMap<DefId, ContentHash>,
    front: Arc<FrontBodyQueries>,
    memo: Arc<hirsel_types::TypeMemo>,
    lowering: Option<Arc<dyn LoweringPhase>>,
    runtime: std::sync::OnceLock<Result<Arc<hirsel_interp::aggregate::RuntimeTypes>, QueryError>>,
    verify: Verify,
    /// A recompute-verifying request rebuilds every product through its
    /// producer instead of reopening carried or stored descriptors.
    verify_store: crate::store::VerifyStore,
    records: crate::NativeRecords,
    store: Arc<dyn ResultStore>,
    outcomes: Mutex<BTreeMap<(u32, ContentHash), (Option<Reference>, Box<Outcome>, u64)>>,
    carry: Mutex<Option<Arc<LowerCarry>>>,
    carry_changes: Mutex<Option<Arc<carry::CarryChanges>>>,
    clean_memos: Mutex<BTreeSet<crate::memo::MemoKey>>,
    work: Mutex<BTreeMap<DefPath, FrontLowerWork>>,
    weight: MemoryWeight,
    max_reads: usize,
    errors: RuntimeErrors,
    retention: Arc<Mutex<u64>>,
    _allowance: Arc<hirsel_engine_admission::MemoryReservation>,
    source: Arc<LowerSource>,
}
impl FrontLowerQueries {
    pub(crate) fn bind_demand(self: &Arc<Self>, demand: &Arc<generation::demand::DemandProducer>) {
        let _ = self.demand.set(Arc::downgrade(demand));
        self.front.bind_demand(demand);
    }
    pub(crate) fn keep_demand(&self, demand: Arc<generation::demand::DemandProducer>) {
        *self.demand_owner.lock().unwrap_or_else(|e| e.into_inner()) = Some(demand);
    }
    pub(crate) fn owns_query(&self, id: QueryId) -> bool {
        self.path(id).is_some()
    }
    fn demand_producer(&self) -> Option<Arc<generation::demand::DemandProducer>> {
        self.demand.get()?.upgrade()
    }
    fn delegated(
        &self,
        id: QueryId,
        selected: &Result<ProductRef, Reference>,
    ) -> Option<ForwardedLower> {
        self.forwarded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(id.kind, id.address.0))
            .filter(|row| &row.alias == selected)
            .cloned()
    }
    pub(crate) fn capture(&self) -> &Arc<NamespaceCapture> {
        &self.namespace
    }
    pub(crate) fn body_front(&self) -> &Arc<FrontBodyQueries> {
        &self.front
    }
    /// Compact potential dependency inventory; no Lower/Evaluate output or
    /// maximum continuation is reserved until that query is actually demanded.
    pub fn input_bytes(definitions: &[DefPath]) -> Option<u64> {
        let mut indexes = crate::NativeMetadataWeight::default();
        indexes.tree::<(u32, ContentHash), DefId>(definitions.len().checked_mul(2)?);
        indexes.add(size_of::<Self>());
        indexes.add(
            size_of::<LowerSource>()
                + size_of::<hirsel_engine_admission::MemoryReservation>()
                + 4 * size_of::<usize>(),
        );
        indexes.add(size_of::<Mutex<u64>>() + 2 * size_of::<usize>());
        definitions.iter().try_fold(indexes.bytes(), |total, path| {
            let path_bytes = bincode::serialized_size(path).ok()?;
            total.checked_add(path_bytes.checked_add(
                (std::mem::size_of::<(DefPath, Input)>() + 6 * std::mem::size_of::<usize>()) as u64,
            )?)
        })
    }
    /// One completed descriptor plus its independently stored ordinary body,
    /// materialized value, reads/depth and opaque acceptance metadata.
    pub fn output_bytes(weight: MemoryWeight) -> Option<u64> {
        weight.retained.checked_mul(2)?.checked_add(
            (std::mem::size_of::<Outcome>() + std::mem::size_of::<Reference>() + 128) as u64,
        )
    }

    /// Input rows plus actual settled metadata/receipt debits. Historical
    /// provider archive traffic and live runtime futures are separate owners.
    pub fn retained_bytes(&self) -> u64 {
        self._allowance.bytes() - *self.retention.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Outcomes adopted through an unchanged subscription generation, without
    /// inspecting the carried memo's individual reads.
    pub fn clean_memos(&self) -> BTreeSet<crate::memo::MemoKey> {
        self.clean_memos
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Immutable producer grant, including input rows and retained outcomes.
    pub fn admitted_bytes(&self) -> u64 {
        self._allowance.bytes()
    }

    /// Settled memo identities for work accounting. No read archive or body
    /// is decoded merely to count the producer work this graph demanded.
    pub fn memo_keys(&self) -> Vec<crate::memo::MemoKey> {
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        let mut keys = Vec::new();
        for (path, input) in &self.inputs {
            for (kind, phase, address) in [
                (FRONT_LOWER_KIND, QueryKind::Lower, input.captured.lower.0),
                (
                    FRONT_EVALUATE_KIND,
                    QueryKind::Evaluate,
                    input.captured.evaluate.0,
                ),
            ] {
                if outcomes.contains_key(&(kind, self.namespace.query_id(kind, address).address.0))
                {
                    keys.push(crate::memo::MemoKey::new(phase, DefId::intern(path)));
                }
            }
        }
        keys
    }

    pub fn memo_snapshot(&self) -> Option<Vec<(crate::memo::MemoKey, Arc<Reads>)>> {
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows = Vec::new();
        for (path, input) in &self.inputs {
            for (kind, phase, address) in [
                (FRONT_LOWER_KIND, QueryKind::Lower, input.captured.lower.0),
                (
                    FRONT_EVALUATE_KIND,
                    QueryKind::Evaluate,
                    input.captured.evaluate.0,
                ),
            ] {
                if let Some((_, outcome, _)) =
                    outcomes.get(&(kind, self.namespace.query_id(kind, address).address.0))
                {
                    rows.push((
                        crate::memo::MemoKey::new(phase, DefId::intern(path)),
                        Arc::new(outcome.reads(self)?),
                    ));
                }
            }
        }
        Some(rows)
    }
    /// Actual completed read evidence, with no producer or mutable-engine call.
    pub fn settled_reads(&self, path: &DefPath, phase: QueryKind) -> Option<Arc<Reads>> {
        let id = match phase {
            QueryKind::Lower => self.query(path)?,
            QueryKind::Evaluate => self.evaluate_query(path)?,
            _ => return None,
        };
        self.outcomes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(id.kind, id.address.0))
            .and_then(|(_, outcome, _)| outcome.reads(self).map(Arc::new))
    }
    pub(crate) fn selected_read_len(
        &self,
        id: QueryId,
        selected: Result<ProductRef, Reference>,
    ) -> Option<usize> {
        Some(self.selected(id, selected)?.read_len())
    }
    /// Complete settled reads from the exact selected owner.
    pub(crate) fn selected_reads(
        &self,
        id: QueryId,
        selected: Result<ProductRef, Reference>,
    ) -> Option<Arc<Reads>> {
        self.selected(id, selected)?.reads(self).map(Arc::new)
    }
    pub fn work_snapshot(&self) -> Vec<(DefPath, FrontLowerWork)> {
        self.work
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(path, work)| (path.clone(), *work))
            .collect()
    }
    pub fn sources(&self) -> &SourceMap {
        &self.namespace.sources
    }
    pub fn runtime_types(&self) -> Result<Arc<hirsel_interp::aggregate::RuntimeTypes>, QueryError> {
        self.runtime
            .get_or_init(|| {
                let lowering = self.lowering.as_deref().ok_or(QueryError::NotImplemented)?;
                Ok(Arc::new(
                    self.namespace
                        .snapshot
                        .comptime_runtime_types(Some(lowering.entry_body())),
                ))
            })
            .clone()
    }
    pub fn namespace(&self) -> &Arc<NamespaceCapture> {
        &self.namespace
    }
    pub fn signatures(&self) -> Arc<SignatureSnapshot> {
        self.namespace.snapshot.clone()
    }
    pub fn signature(&self, path: &DefPath) -> Result<hirsel_types::SignatureRef, QueryError> {
        self.namespace
            .snapshot
            .signature(DefId::intern(path))
            .ok_or(QueryError::NotImplemented)?
    }
    /// Captured physical decode working memory for an already settled original.
    /// Reads metadata only; no provider fetch, body decoder or query runs.
    pub fn settled_definition_decode_bytes(&self, path: &DefPath) -> Result<u64, QueryError> {
        self.settled_definition_decode_layout(path)?
            .total()
            .ok_or(QueryError::NotImplemented)
    }
    /// Captured layout only: no provider read, decode, body phase or clone.
    pub fn settled_definition_decode_layout(
        &self,
        path: &DefPath,
    ) -> Result<DefinitionDecodeLayout, QueryError> {
        let id = self.query(path).ok_or(QueryError::NotImplemented)?;
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        let (_, outcome, _) = outcomes
            .get(&(id.kind, id.address.0))
            .ok_or(QueryError::NotImplemented)?;
        match &outcome.result {
            Ok(Value::Lower(lower)) => Ok(lower.definition_decode_layout),
            Err(error) => Err(error.clone()),
            _ => Err(QueryError::NotImplemented),
        }
    }
    /// Prospective constant-selection maps captured while original TIR was live.
    /// This reads no archive and reconstructs no body.
    pub(crate) fn settled_constant_selection_bytes(
        &self,
        path: &DefPath,
    ) -> Result<u64, QueryError> {
        let id = self.query(path).ok_or(QueryError::NotImplemented)?;
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        match &outcomes
            .get(&(id.kind, id.address.0))
            .ok_or(QueryError::NotImplemented)?
            .1
            .result
        {
            Ok(Value::Lower(lower)) => Ok(lower.constant_selection_bytes),
            Err(error) => Err(error.clone()),
            _ => Err(QueryError::NotImplemented),
        }
    }
    /// Bound all settled constant clones without copying their payloads or paths.
    pub(crate) fn settled_constants_input_bytes(&self) -> Result<u64, QueryError> {
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        let mut weight = crate::NativeMetadataWeight::default();
        weight.tree::<DefPath, Materialized>(outcomes.len());
        for path in self.inputs.keys().cloned().chain(self.staged.instances()) {
            let Some(id) = self.evaluate_query(&path) else {
                continue;
            };
            let Some((_, outcome, _)) = outcomes.get(&(id.kind, id.address.0)) else {
                continue;
            };
            if let Ok(Value::Constant(value)) = &outcome.result {
                weight.path(&path);
                weight.add(
                    value
                        .root_clone_bytes()
                        .checked_mul(2)
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or(QueryError::NotImplemented)?,
                );
                weight.add(
                    value
                        .decode_bytes()
                        .try_into()
                        .map_err(|_| QueryError::NotImplemented)?,
                );
                weight.add(size_of::<Materialized>() + 2 * size_of::<usize>());
            }
        }
        Ok(weight.bytes())
    }
    /// Decode under an explicitly retained caller working reservation. The
    /// precheck precedes provider I/O; callers keep this grant alive while any
    /// returned body clone is held. No Lower work or query is requested.
    pub fn settled_definition_with_allowance(
        self: &Arc<Self>,
        path: &DefPath,
        allowance: &hirsel_engine_admission::MemoryReservation,
    ) -> Result<DefinitionRef, QueryError> {
        if self.settled_definition_decode_bytes(path)? > allowance.bytes() {
            return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "original body inspection".into(),
                message: "captured decode buffers exceed caller working admission".into(),
            }));
        }
        self.settled_definition(path)
    }
    pub fn settled_definition(
        self: &Arc<Self>,
        path: &DefPath,
    ) -> Result<DefinitionRef, QueryError> {
        let id = self.query(path).ok_or(QueryError::NotImplemented)?;
        let selected = self
            .outcomes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(id.kind, id.address.0))
            .cloned()
            .ok_or(QueryError::NotImplemented)?;
        match selected.1.result? {
            Value::Lower(lower) => lower
                .decode(self)
                .map(|output| output.value)
                .ok_or(QueryError::NotImplemented),
            _ => Err(QueryError::NotImplemented),
        }
    }
    pub(crate) fn settled_constant_clone_bytes(&self, path: &DefPath) -> Result<u64, QueryError> {
        let id = self
            .evaluate_query(path)
            .ok_or(QueryError::NotImplemented)?;
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        let (_, outcome, _) = outcomes
            .get(&(id.kind, id.address.0))
            .ok_or(QueryError::NotImplemented)?;
        match &outcome.result {
            Ok(Value::Constant(value)) => {
                let root = value
                    .root_clone_bytes()
                    .checked_add(
                        (std::mem::size_of::<Materialized>() + 2 * std::mem::size_of::<usize>())
                            as u64,
                    )
                    .and_then(|bytes| bytes.checked_mul(2))
                    .ok_or(QueryError::NotImplemented)?;
                root.checked_add(value.decode_bytes())
                    .ok_or(QueryError::NotImplemented)
            }
            Err(error) => Err(error.clone()),
            _ => Err(QueryError::NotImplemented),
        }
    }
    pub fn settled_constant(
        &self,
        path: &DefPath,
        arguments: &[u64],
    ) -> Result<MaterializedRef, QueryError> {
        if arguments.is_empty()
            && self.front.query(path).is_none()
            && let Some(value) = self
                .namespace
                .snapshot
                .builtin_dictionary_value(&self.memo, path)?
        {
            return Ok(Arc::new(value));
        }
        let selected = if arguments.is_empty() {
            path.clone()
        } else {
            hirsel_tir::constant_instance_path(path, arguments)
        };
        let completed = || {
            self.completed_owner(&selected, FRONT_EVALUATE_KIND)
                .ok_or(QueryError::NotImplemented)?
                .settled_constant(path, arguments)
        };
        let Some(id) = self.evaluate_query(&selected) else {
            return completed();
        };
        if let Some(row) = self
            .forwarded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(id.kind, id.address.0))
            .cloned()
        {
            row.selected.as_ref().map_err(|reference| {
                row.owner
                    .failure(*reference)
                    .unwrap_or(QueryError::NotImplemented)
            })?;
            return row.owner.settled_constant(path, arguments);
        }
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        let Some(selected) = outcomes.get(&(id.kind, id.address.0)) else {
            drop(outcomes);
            return completed();
        };
        match &selected.1.result {
            Ok(Value::Constant(value)) => value
                .decoded(
                    &self.records,
                    &*self.namespace.anchors,
                    &*self.store,
                    self.weight.scratch,
                )
                .map(|(value, _)| Arc::new(value))
                .ok_or(QueryError::NotImplemented),
            Err(error) => Err(error.clone()),
            _ => Err(QueryError::NotImplemented),
        }
    }
    pub fn settled_constants(&self) -> Vec<DefPath> {
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        self.inputs
            .keys()
            .cloned()
            .chain(self.staged.instances())
            .filter(|path| {
                self.evaluate_query(path).is_some_and(|id| {
                    outcomes
                        .get(&(id.kind, id.address.0))
                        .is_some_and(|(_, outcome, _)| {
                            matches!(outcome.result, Ok(Value::Constant(..)))
                        })
                })
            })
            .collect()
    }
    pub fn settled_instance(
        &self,
        path: &DefPath,
        arguments: &[hirsel_types::GenericArgument],
    ) -> Result<Option<DefPath>, QueryError> {
        self.namespace
            .instances
            .resolve(
                DefId::intern(path),
                self.namespace.snapshot.types(),
                arguments,
            )
            .map(|selected| selected.map(|id| id.path().clone()))
    }

    pub(crate) fn instance_definitions(&self) -> impl Iterator<Item = &DefPath> {
        self.namespace
            .instances
            .instances
            .values()
            .flat_map(|instances| instances.values())
            .map(|instance| instance.definition.path())
    }

    /// Close CT-5 calls through generic helpers using this view's settled
    /// summaries and instance table. Unlike source registration, Run cannot
    /// create a missing instance or execute its generator.
    pub(crate) fn selected_instances(
        &self,
        roots: impl IntoIterator<Item = DefPath>,
        limit: u64,
    ) -> Result<BTreeSet<DefPath>, QueryError> {
        if self.namespace.instances.declarations.is_empty() {
            return Ok(BTreeSet::new());
        }
        let refused = || {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "Run instance selection".into(),
                message: "closed instance selection exceeds admission or lacks a settled summary"
                    .into(),
            })
        };
        let mut pending: Vec<_> = roots
            .into_iter()
            .map(|p| (DefId::intern(&p), BTreeMap::new()))
            .collect();
        let mut seen = BTreeSet::new();
        let mut selected = BTreeSet::new();
        let mut work = crate::NativeMetadataWeight::default();
        work.flat_vec(&pending);
        while let Some((path, bindings)) = pending.pop() {
            let key: Vec<_> = bindings
                .iter()
                .map(
                    |(site, actuals): (&hirsel_types::SiteRef, &hirsel_types::Actuals)| {
                        (
                            *site,
                            hirsel_tir::InstanceBodies::key(
                                self.namespace.snapshot.types(),
                                &actuals.arguments,
                            ),
                        )
                    },
                )
                .collect();
            work.flat_vec(&key);
            if !seen.insert((path, key)) {
                continue;
            }
            work.add(
                size_of::<(
                    DefId,
                    Vec<(
                        hirsel_types::SiteRef,
                        Option<hirsel_types::GeneratedActualKey>,
                    )>,
                )>() + 16 * size_of::<usize>(),
            );
            if seen.len() > 65536 || work.bytes() > limit {
                return Err(refused());
            }
            if !self.inputs.contains_key(path.path())
                || self.namespace.instances.declarations.contains(&path)
            {
                continue;
            }
            let product = self.front.checked_definition(path.path()).ok_or_else(|| {
                self.front
                    .completed_error(path.path())
                    .unwrap_or_else(refused)
            })?;
            let summary = &product.summary.summary;
            if bindings.is_empty() && summary.frames[0].generic {
                continue;
            }
            for call in summary
                .calls
                .iter()
                .filter(|call| call.target_frame.is_none())
            {
                if !self.inputs.contains_key(call.callee.path()) {
                    continue;
                }
                let actuals = hirsel_types::close_arguments(
                    self.namespace.snapshot.types(),
                    &self.memo,
                    &call.arguments,
                    &bindings,
                )
                .map_err(|_| refused())?;
                let closed = hirsel_tir::InstanceBodies::key(
                    self.namespace.snapshot.types(),
                    &actuals.arguments,
                )
                .is_some();
                let next = if self.namespace.instances.declarations.contains(&call.callee) {
                    if !closed {
                        continue;
                    }
                    let instance = self
                        .namespace
                        .instances
                        .resolve(
                            call.callee,
                            self.namespace.snapshot.types(),
                            &actuals.arguments,
                        )?
                        .ok_or_else(refused)?;
                    if !selected.contains(instance.path()) {
                        work.tree::<DefPath, ()>(0);
                        work.path(instance.path());
                        if work.bytes() > limit {
                            return Err(refused());
                        }
                        selected.insert(instance.path().clone());
                    }
                    (instance, BTreeMap::new())
                } else if actuals.arguments.is_empty() {
                    (call.callee, BTreeMap::new())
                } else if closed {
                    (call.callee, BTreeMap::from([(call.binder, actuals)]))
                } else {
                    continue;
                };
                // Bound all visited and queued cursor allocations cumulatively;
                // released cursors never grant extra space to this walk.
                work.add(size_of_val(&next) + 16 * size_of::<usize>());
                work.tree::<hirsel_types::SiteRef, hirsel_types::Actuals>(next.1.len());
                for actuals in next.1.values() {
                    work.flat_vec(&actuals.arguments);
                }
                if work.bytes() > limit {
                    return Err(refused());
                }
                pending.push(next);
            }
        }
        Ok(selected)
    }

    pub fn work(&self, path: &DefPath) -> FrontLowerWork {
        self.work
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
            .copied()
            .unwrap_or_default()
    }
    fn count(&self, path: &DefPath, count: impl FnOnce(&mut FrontLowerWork)) {
        count(
            self.work
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(path.clone())
                .or_default(),
        );
    }
    pub fn query(&self, path: &DefPath) -> Option<QueryId> {
        self.id(path, FRONT_LOWER_KIND)
    }
    pub fn evaluate_query(&self, path: &DefPath) -> Option<QueryId> {
        self.id(path, FRONT_EVALUATE_KIND)
    }
    fn id(&self, path: &DefPath, kind: u32) -> Option<QueryId> {
        let Some(input) = self.inputs.get(path) else {
            return (kind == FRONT_EVALUATE_KIND)
                .then(|| self.staged.selected(path))
                .flatten();
        };
        let key = if kind == FRONT_LOWER_KIND {
            &input.captured.lower
        } else {
            &input.captured.evaluate
        };
        Some(self.namespace.query_id(kind, key.0))
    }
    fn path(&self, id: QueryId) -> Option<DefPath> {
        self.paths
            .get(&(id.kind, id.address.0))
            .map(|path| path.path().clone())
            .or_else(|| {
                self.staged
                    .selection(id)
                    .map(|selection| selection.owner.path().clone())
            })
    }
    pub fn factory(self: &Arc<Self>) -> Arc<dyn LocalQueryFactory> {
        Arc::new(Factory {
            owner: self.clone(),
            _demand: self
                .demand_owner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .or_else(|| self.demand_producer()),
        })
    }
    pub fn outcome(self: &Arc<Self>, product: ProductRef) -> Option<FrontLowerOutput> {
        if let Some(row) = self.delegated(
            QueryId {
                kind: FRONT_LOWER_KIND,
                address: product.address,
            },
            &Ok(product.clone()),
        ) {
            return row.owner.outcome(row.selected.ok()?);
        }

        if let Some(live) = product.lower_value() {
            self.selected(
                QueryId {
                    kind: FRONT_LOWER_KIND,
                    address: product.address,
                },
                Ok(product.clone()),
            )?;
            return Some(live.observations().clone());
        }
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        let (reference, outcome, _) = outcomes.get(&(FRONT_LOWER_KIND, product.address.0))?;
        if *reference
            != Some(
                product
                    .archive(ArchiveAdmission::Retained)
                    .ok()?
                    .reference(),
            )
        {
            return None;
        }
        let Value::Lower(lower) = outcome.result.as_ref().ok()? else {
            return None;
        };
        let lower = lower.clone();
        let reads = outcome.reads(self)?;
        let depth = outcome.depth.evidence();
        let execution = outcome.execution.clone();
        let execution_subject = outcome.execution_subject;
        let executed = outcome.executed;
        let selections = outcome.selections.clone();
        drop(outcomes);
        let output = lower.decode(self)?;
        let materialized = self.records.decode(
            &*self.namespace.anchors,
            &self.store.get_content(*lower.materialized.get()?)?,
        )?;
        let input = self.inputs.get(lower.definition.path())?;
        let signature = self
            .namespace
            .snapshot
            .signature(output.value.path)?
            .as_ref()
            .ok()?
            .fingerprint;
        Some(FrontLowerOutput {
            output,
            reads,
            depth,
            execution,
            execution_subject,
            executed,
            selections,
            acceptance: lower.acceptance.clone(),
            implementation_revision: input.captured.implementation,
            signature,
            verified: lower.verified(self)?,
            materialized,
            query_observations: Some(product.clone()),
            body_archive: lower.body.clone(),
            constants_archive: lower.materialized.clone(),
            owner: self.source.clone(),
        })
    }
    /// Native adaptation reads compact metadata only. Neither an unchanged
    /// Definition nor a native Value/Ready body is reconstructed here.
    pub fn native_product(&self, product: ProductRef) -> Option<crate::NativeOriginalProduct> {
        if let Some(row) = self.delegated(
            QueryId {
                kind: FRONT_LOWER_KIND,
                address: product.address,
            },
            &Ok(product.clone()),
        ) {
            return row.owner.native_product(row.selected.ok()?);
        }

        self.selected(
            QueryId {
                kind: FRONT_LOWER_KIND,
                address: product.address,
            },
            Ok(product.clone()),
        )?;
        let live = product.lower_value()?.clone();
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        let (_, outcome, _) = outcomes.get(&(FRONT_LOWER_KIND, product.address.0))?;
        let Ok(Value::Lower(lower)) = &outcome.result else {
            return None;
        };
        let body = crate::NativeOriginalHandle::bound(
            &live,
            lower.body.clone(),
            self.namespace.anchors.clone(),
            self.records.clone(),
            self.source.clone(),
            lower
                .definition_decode_layout
                .total()?
                .checked_add(lower.body.get().map_or(0, |body| body.len))?,
        );
        let observations = live.observations();
        Some(crate::NativeOriginalProduct {
            definition: live.output().value.path.path().clone(),
            implementation_revision: observations.implementation_revision,
            signature: observations.signature,
            verified: observations.verified.clone(),
            body,
        })
    }
    /// Complete settled query observations for Check. This retains the existing
    /// compact descriptor, without retaining or archiving the live TIR owner.
    pub(crate) fn observation_product(&self, product: ProductRef) -> Option<ProductRef> {
        if let Some(row) = self.delegated(
            QueryId {
                kind: FRONT_LOWER_KIND,
                address: product.address,
            },
            &Ok(product.clone()),
        ) {
            return row.owner.observation_product(row.selected.ok()?);
        }

        let id = QueryId {
            kind: FRONT_LOWER_KIND,
            address: product.address,
        };
        self.selected(id, Ok(product))?.observations
    }
    pub fn failure(&self, reference: Reference) -> Option<QueryError> {
        let local = self
            .outcomes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .find(|(r, _, _)| *r == Some(reference))
            .and_then(|(_, outcome, _)| outcome.result.as_ref().err().cloned());
        if local.is_some() {
            return local;
        }
        let owners = self
            .forwarded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|row| row.selected == Err(reference))
            .map(|row| row.owner.clone())
            .collect::<Vec<_>>();
        owners
            .into_iter()
            .find_map(|owner| owner.failure(reference))
    }
    pub fn failure_sources(&self, reference: Reference) -> Option<SourceMap> {
        let local = self
            .outcomes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .find(|(r, _, _)| *r == Some(reference))
            .map(|(_, outcome, _)| {
                outcome.diagnostic_sources.as_ref().map_or_else(
                    || self.namespace.sources.clone(),
                    |sources| sources.sources.clone(),
                )
            });
        if local.is_some() {
            return local;
        }
        let owners = self
            .forwarded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|row| row.selected == Err(reference))
            .map(|row| row.owner.clone())
            .collect::<Vec<_>>();
        owners
            .into_iter()
            .find_map(|owner| owner.failure_sources(reference))
    }
    fn selected(&self, id: QueryId, selected: Result<ProductRef, Reference>) -> Option<Outcome> {
        if let Some(row) = self.delegated(id, &selected) {
            return row.owner.selected(row.id, row.selected);
        }

        self.outcomes.lock().unwrap_or_else(|e| e.into_inner())
            .get(&(id.kind, id.address.0))
            .filter(|(reference, outcome, _)| match &selected {
                Ok(product) => {
                    if let Some(live) = product.lower_value() {
                        product.address == live.address() && product.action == live.action()
                            && matches!(&outcome.result, Ok(Value::Lower(lower)) if lower.live.as_ref().and_then(WeakLowerProduct::upgrade).is_some_and(|stored| stored.same_owner(live)))
                    } else if product.action == outcome.action && reference.is_none() {
                        true
                    } else {
                        product.archive(ArchiveAdmission::Retained).is_ok_and(|archive| Some(archive.reference()) == *reference)
                    }
                }
                Err(failure) => Some(*failure) == *reference,
            })
            .map(|(_, outcome, _)| outcome.as_ref().clone())
    }
    async fn run(
        self: Arc<Self>,
        id: QueryId,
        path: DefPath,
        reads: QueryReads,
    ) -> Result<ProductRef, Reference> {
        let _read_scope = self.store.read_scope();
        let _publication = self.store.publication_scope();
        let input = &self.inputs[&path];
        let selection = self.staged.selection(id);
        let key = if let Some(selection) = &selection {
            &selection.key
        } else if id.kind == FRONT_LOWER_KIND {
            &input.captured.lower
        } else {
            &input.captured.evaluate
        };
        // A constant's checked Lower body does not replace its value product.
        // Restore Evaluate first even when the Lower descriptor itself hits;
        // receipt readers and other settled consumers cannot request it later.
        let can_restore = if id.kind == FRONT_LOWER_KIND
            && self
                .namespace
                .snapshot
                .signature(DefId::intern(&path))
                .is_some_and(|signature| {
                    signature.is_ok_and(|signature| {
                        matches!(signature.kind, hirsel_types::SignatureKind::Const { .. })
                    })
                }) {
            let evaluate = self.evaluate_query(&path).ok_or(self.errors.protocol)?;
            reads.read_consuming(evaluate).await.is_ok()
        } else {
            true
        };
        if can_restore && self.verify_store == crate::store::VerifyStore::Off {
            if let Some(product) = self.load_carry(id, &path, key, &reads).await {
                match product {
                    // The compact descriptor can outlive V's independently
                    // evictable content. A current V miss needs the ordinary
                    // producer, even while the source observations still match.
                    Ok(product)
                        if id.kind == FRONT_LOWER_KIND
                            && self.native_product(product.clone()).is_none() => {}
                    product => return product,
                }
            }
            if let Some(product) = self.load_stored(id, &path, key, &reads).await {
                return product;
            }
        }
        // The live future and working bodies use this task's runtime weight.
        // Only actual settled descriptor/receipt ownership is retained below;
        // provider archives and unused maximum task weights are not ownership.
        let mut source = Source {
            id,
            root: DefId::intern(&path),
            owner: self.clone(),
            reads,
            observed: Rc::new(RefCell::new(ReadsBuilder::new())),
            selections: Rc::new(RefCell::new(Vec::new())),
            refusal: Rc::new(Cell::new(None)),
            depth: Rc::new(RefCell::new(DepthEvidence::leaf(dependency_limit(
                self.namespace.limits.call_depth,
            )))),
            framed: id.kind == FRONT_EVALUATE_KIND,
            evidence: Rc::new(RefCell::new(Evidence::new(
                self.verify,
                self.weight.retained,
            ))),
            successor: Rc::default(),
            restart: Rc::default(),
            diagnostic_sources: Rc::default(),
        };
        // A root observes its own body before preparation, including when
        // that preparation fails. Nested callers keep the same root receipt.
        source.record(
            ReadKey::ExecutedBody(DefId::intern(&path)),
            memos::optional(Some(self.inputs[&path].captured.implementation)),
        );
        let computed = if source.framed && source.depth.borrow().limit() == 0 {
            if self.verify == Verify::On {
                let observed = self
                    .validation_reads(&path.0.package, &source.reads)
                    .await?;
                source.merge(&observed.finish().map_err(|_| self.errors.protocol)?);
            }
            Err(source.overflow(self.inputs[&path].captured.site))
        } else {
            self.compute(id.kind, &path, selection.as_ref(), &mut source)
                .await
        };
        let restart = source.restart.borrow_mut().take();
        if let Some(successor) = restart {
            let owner = successor.owner().clone();
            let actual = match &selection {
                Some(selection) => owner
                    .staged_query(selection.declaration.path(), &selection.arguments)
                    .map_err(|_| self.errors.protocol)?,
                None => owner.id(&path, id.kind).ok_or(self.errors.protocol)?,
            };
            if actual == id {
                return Err(self.errors.protocol);
            }
            let reads = source.reads.clone();
            drop(computed);
            drop(source);
            let selected = reads.read_consuming(actual).await;
            let alias = selected.clone().map(|mut product| {
                product.address = id.address;
                product
            });
            self.forwarded
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    (id.kind, id.address.0),
                    ForwardedLower {
                        id: actual,
                        owner,
                        selected,
                        alias: alias.clone(),
                    },
                );
            return alias;
        }
        let owner = source.active_owner();
        if !Arc::ptr_eq(&owner, &self) {
            let actual = match &selection {
                Some(selection) => owner
                    .staged_query(selection.declaration.path(), &selection.arguments)
                    .map_err(|_| self.errors.protocol)?,
                None => owner.id(&path, id.kind).ok_or(self.errors.protocol)?,
            };
            let key = if let Some(selection) = &selection {
                selection.key.clone()
            } else if id.kind == FRONT_LOWER_KIND {
                owner.inputs[&path].captured.lower.clone()
            } else {
                owner.inputs[&path].captured.evaluate.clone()
            };
            let selected = owner
                .clone()
                .finish_computation(actual, path, key, source, computed)
                .await;
            let alias = selected.clone().map(|mut product| {
                product.address = id.address;
                product
            });
            self.forwarded
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    (id.kind, id.address.0),
                    ForwardedLower {
                        id: actual,
                        owner,
                        selected,
                        alias: alias.clone(),
                    },
                );
            return alias;
        }
        self.clone()
            .finish_computation(id, path, key.clone(), source, computed)
            .await
    }
    async fn finish_computation(
        self: Arc<Self>,
        id: QueryId,
        path: DefPath,
        key: EntryKey,
        source: Source,
        computed: Result<Computed, QueryError>,
    ) -> Result<ProductRef, Reference> {
        let (mut result, verification, raw) = match computed {
            Ok(computed) => (Ok(computed.value), computed.verification, computed.raw),
            Err(error) => (
                Err(lower_input_error(
                    &path,
                    if id.kind == FRONT_EVALUATE_KIND {
                        "evaluating a constant"
                    } else {
                        "lowering a body"
                    },
                    error,
                )),
                None,
                None,
            ),
        };
        // A missing dependency outcome is a scheduler/transport refusal, not
        // a typed body error. Preserve its exact identity through all clones
        // of the asynchronous source before constructing any descriptor.
        if let Some(reference) = source.refusal.get() {
            return Err(reference);
        }
        let mut depth = source.depth.borrow().clone();
        if source.framed {
            depth.first[0] = None;
        }
        let observed = std::mem::take(&mut *source.observed.borrow_mut())
            .finish()
            .map_err(|_| self.errors.protocol)?;
        let executed = source.evidence.borrow().executed;
        let execution = result
            .as_ref()
            .ok()
            .and_then(|_| source.evidence.borrow().finish(&observed));
        if result.is_ok() {
            let selections = std::mem::take(&mut *source.selections.borrow_mut());
            let action = self.output_action(id, &key, &observed);
            let execution_subject = action.0;
            if let Some((definition, evidence)) = verification
                && let Ok(Value::Lower(lower)) = &mut result
                && let Some(cache) = self.store.verifier()
            {
                lower.local_verified = self
                    .namespace
                    .snapshot
                    .validation(&path.0.package)
                    .ok()
                    .and_then(|views| {
                        cache.publish_definition(
                            execution_subject,
                            &definition,
                            self.namespace.snapshot.types().clone(),
                            self.namespace.snapshot.source_verification_policy(),
                            views,
                            &evidence,
                            &[],
                        )
                    });
            }
            let outcome = Outcome {
                diagnostic_sources: source.diagnostic_sources.borrow().clone(),
                result,
                reads: observed,
                reads_decode_bytes: 0,
                selections,
                depth: depth.into(),
                executed,
                execution,
                execution_subject,
                action,
                observations: None,
            };
            return self
                .publish_live(id, &key, outcome, raw, &source.reads)
                .await;
        }
        let selections = std::mem::take(&mut *source.selections.borrow_mut());
        let reads_decode_bytes = 0;
        let diagnostic_sources = source.diagnostic_sources.borrow().clone();
        let error = result.as_ref().err().ok_or(self.errors.protocol)?;
        let bytes = self
            .records
            .encode_at(
                &namespace::DiagnosticRecordSource {
                    base: &self.namespace.anchors,
                    diagnostics: diagnostic_sources.as_deref(),
                },
                &(&path, error, &observed, &depth, executed),
                crate::store::ArchiveBoundary::Ordinary,
            )
            .ok_or(self.errors.resource)?;

        // The archive is an active encoding buffer. Settled descriptor/value
        // metadata is debited below from this producer's separate grant.
        if bytes.len() as u64 > self.weight.scratch {
            return Err(self.errors.resource);
        }
        let domain = FAILURE;
        let execution_subject = persistent::evaluation_subject(&key, &bytes);
        let outcome = Outcome {
            diagnostic_sources: source.diagnostic_sources.borrow().clone(),
            result,
            reads: observed,
            reads_decode_bytes,
            selections,
            depth: depth.into(),
            executed,
            execution,
            execution_subject,
            action: hirsel_artifact_store::Action(execution_subject),
            observations: None,
        };
        self.publish(
            id,
            &key,
            outcome,
            Publication {
                domain,
                bytes: bytes.into(),
            },
            &source.reads,
        )
        .await
    }
    /// Settle one descriptor: store it, admit its outcome under this producer's
    /// retention and associate a successful product.
    async fn publish(
        &self,
        id: QueryId,
        key: &EntryKey,
        mut outcome: Outcome,
        publication: Publication,
        query_reads: &QueryReads,
    ) -> Result<ProductRef, Reference> {
        let Publication { domain, bytes } = publication;
        let record = self
            .store
            .put_content(domain, &bytes)
            .ok_or(self.errors.resource)?;
        let action = if outcome.result.is_ok() {
            self.output_action(id, key, &outcome.reads)
        } else {
            hirsel_artifact_store::Action(ContentHash::of_parts(
                "hirsel.front.scheduled-action.v1",
                [
                    key.0.0.as_slice(),
                    key.1.0.as_slice(),
                    self.manifest_for_outcome(id, &outcome.reads)
                        .ok_or(self.errors.resource)?
                        .output
                        .0
                        .0
                        .as_slice(),
                    crate::store::payload_reads_hash(&outcome.reads)
                        .0
                        .as_slice(),
                    domain.kind.to_le_bytes().as_slice(),
                    domain.schema.to_le_bytes().as_slice(),
                ],
            ))
        };
        outcome.action = action;
        if let Some(receipt) = &outcome.execution
            && let Some(cache) = self.store.verifier()
        {
            cache.publish_execution(outcome.execution_subject, receipt);
        }
        let failed = outcome.result.is_err();
        if let Ok(Value::Constant(value)) = &mut outcome.result {
            value.pack(record);
        }
        let raw = match &outcome.result {
            Ok(Value::Lower(lower)) => Some((
                lower.decode(self).ok_or(self.errors.protocol)?,
                self.records
                    .decode(
                        &*self.namespace.anchors,
                        &self
                            .store
                            .get_content(*lower.materialized.get().ok_or(self.errors.protocol)?)
                            .ok_or(self.errors.protocol)?,
                    )
                    .ok_or(self.errors.protocol)?,
            )),
            _ => None,
        };
        let mut original = if let Some((output, materialized)) = raw {
            Some({
                let lower = match outcome.result.as_ref().map_err(|_| self.errors.protocol)? {
                    Value::Lower(lower) => lower,
                    _ => return Err(self.errors.protocol),
                };
                let path = output.value.path;
                FrontLowerOutput {
                    output,
                    materialized,
                    reads: outcome.reads.clone(),
                    depth: outcome.depth.evidence(),
                    execution: outcome.execution.clone(),
                    execution_subject: outcome.execution_subject,
                    executed: outcome.executed,
                    selections: outcome.selections.clone(),
                    acceptance: lower.acceptance.clone(),
                    implementation_revision: self
                        .inputs
                        .get(path.path())
                        .ok_or(self.errors.protocol)?
                        .captured
                        .implementation,
                    signature: self
                        .namespace
                        .snapshot
                        .signature(path)
                        .ok_or(self.errors.protocol)?
                        .as_ref()
                        .map_err(|_| self.errors.protocol)?
                        .fingerprint,
                    verified: lower.verified(self).unwrap_or(None),
                    query_observations: None,
                    body_archive: lower.body.clone(),
                    constants_archive: lower.materialized.clone(),
                    owner: self.source.clone(),
                }
            })
        } else {
            None
        };
        outcome.archive_reads(record);
        let archive_bytes = if failed {
            0
        } else {
            ProductArchive::retained_required_bytes(record, &bytes)
                .map_err(|_| self.errors.resource)?
        };
        let live_bytes = if let Some(original) = &original {
            LiveLowerProduct::retained_required_bytes(original).ok_or(self.errors.resource)?
        } else {
            0
        };
        let required = outcome
            .retained_bytes()
            .and_then(|bytes| bytes.checked_add(archive_bytes))
            .and_then(|bytes| bytes.checked_add(live_bytes))
            .and_then(|bytes| bytes.checked_add(outcome_table_bytes(1)))
            .ok_or(self.errors.resource)?;
        // Each draw below debits the shared retained counter. A body whose
        // reservation does not fit waits for in-flight work to release bytes
        // (or for a settled body to evict) instead of refusing; it refuses
        // only when the runtime reports nothing in flight can free memory.
        let mut outcome = Some(outcome);
        let (archive, live) = loop {
            while *self.retention.lock().unwrap_or_else(|e| e.into_inner()) < required {
                if !query_reads.evict_body().await {
                    return Err(self.errors.resource);
                }
            }
            let archive = if failed {
                None
            } else {
                match ProductArchive::from_retained_bytes(
                    record,
                    bytes.clone(),
                    self._allowance.clone(),
                    self.retention.clone(),
                ) {
                    Ok(archive) => Some(archive),
                    // A peer's draw landed between the estimate and this take.
                    Err(_) => {
                        if !query_reads.evict_body().await {
                            return Err(self.errors.resource);
                        }
                        continue;
                    }
                }
            };
            if let Some(original) = &mut original {
                original.query_observations = archive
                    .clone()
                    .map(|archive| ProductRef::import_archive(id.address, action, archive));
            }
            let live = match original.take() {
                Some(output) => match LiveLowerProduct::from_retained(
                    (id.address, action),
                    output,
                    (*self.namespace.anchors).clone(),
                    self.records.clone(),
                    self.source.clone(),
                    self._allowance.clone(),
                    self.retention.clone(),
                ) {
                    LiveAdmission::Admitted(live) => Some(live),
                    LiveAdmission::Blocked(output) => {
                        original = Some(*output);
                        if !query_reads.evict_body().await {
                            return Err(self.errors.resource);
                        }
                        continue;
                    }
                    LiveAdmission::Refused => return Err(self.errors.resource),
                },
                None => None,
            };
            let outcome_ref = outcome.as_mut().expect("unadmitted outcome");
            outcome_ref.observations = archive
                .clone()
                .map(|archive| ProductRef::import_archive(id.address, action, archive));
            if let Ok(Value::Lower(lower)) = &mut outcome_ref.result {
                lower.live = live.as_ref().map(LiveLowerProduct::downgrade);
            }
            let charge = outcome_ref.retained_bytes().ok_or(self.errors.resource)?;
            let admitted = {
                let mut retention = self.retention.lock().unwrap_or_else(|e| e.into_inner());
                let mut outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
                let selected = (id.kind, id.address.0);
                let old = outcomes.get(&selected).map(|(_, _, charge)| *charge);
                let next_len = outcomes
                    .len()
                    .checked_add(usize::from(old.is_none()))
                    .ok_or(self.errors.resource)?;
                let table_growth = outcome_table_bytes(next_len)
                    .checked_sub(outcome_table_bytes(outcomes.len()))
                    .ok_or(self.errors.resource)?;
                retention
                    .checked_add(old.unwrap_or(0))
                    .and_then(|n| n.checked_sub(charge))
                    .and_then(|n| n.checked_sub(table_growth))
                    .map(|remaining| {
                        let admitted = outcome.take().expect("unadmitted outcome");
                        let replaced =
                            outcomes.insert(selected, (Some(record), Box::new(admitted), charge));
                        *retention = remaining;
                        replaced
                    })
            };
            if admitted.is_none() {
                if !query_reads.evict_body().await {
                    return Err(self.errors.resource);
                }
                continue;
            }
            drop(admitted);
            break (archive, live);
        };
        if failed {
            return Err(record);
        }
        self.store
            .associate_product(hirsel_artifact_store::Address(key.0), action, record);
        if let Some(live) = live {
            return Ok(ProductRef::lower(live));
        }
        Ok(ProductRef::import_archive(
            id.address,
            action,
            archive.ok_or(self.errors.protocol)?,
        ))
    }
    async fn validation_reads(
        &self,
        package: &PackageId,
        query_reads: &QueryReads,
    ) -> Result<ReadsBuilder, Reference> {
        // V shares Front's partition. Release optional body retention before
        // retrying the exact universe read, as for ordinary Lower admission.
        loop {
            {
                let mut observed = ReadsBuilder::new();
                if FrozenProvider::new(&self.namespace.snapshot, package.clone(), &mut observed)
                    .validation_universe()
                    .is_ok()
                {
                    return Ok(observed);
                }
            }
            if !self.front.evict_idle(self.weight.scratch) && !query_reads.evict_body().await {
                return Err(self.errors.resource);
            }
        }
    }
    async fn compute(
        &self,
        kind: u32,
        path: &DefPath,
        selection: Option<&staged::Selection>,
        source: &mut Source,
    ) -> Result<Computed, QueryError> {
        let front = self.front.query(path).ok_or(QueryError::NotImplemented)?;
        let selected = source.reads.read_consuming(front).await;
        if let Err(reference) = selected
            && let Some(sources) = self.front.diagnostic_sources(reference)
        {
            *source.diagnostic_sources.borrow_mut() = Some(sources);
        }
        if let Some(successor) = self.front.successor_for(path) {
            source.join(Some(successor))?;
            let owner = source.active_owner();
            return Box::pin(owner.compute(kind, path, selection, source)).await;
        }
        let body = match self.front.outcome(selected.clone()) {
            Some(body) => body,
            None if selected.is_ok() => {
                let materialize = self
                    .front
                    .body_task(path)
                    .ok_or(QueryError::NotImplemented)?;
                let product = source
                    .reads
                    .compose(materialize)
                    .await
                    .map_err(|error| source.refused(error))?;
                self.front
                    .outcome(Ok(product.clone()))
                    .ok_or_else(|| source.refused(self.errors.protocol))?
            }
            None => return Err(source.refused(selected.err().unwrap_or(self.errors.protocol))),
        };
        if let Some(producer) = self.demand_producer()
            && let Some(successor) = producer
                .stage_body(&source.capture(), path, source.reads.clone())
                .await?
        {
            source.join(Some(successor))?;
            let owner = source.active_owner();
            return Box::pin(owner.compute(kind, path, selection, source)).await;
        }
        source.merge(&body.outcome.reads);
        if self.verify == Verify::On {
            let observed = self
                .validation_reads(&path.0.package, &source.reads)
                .await
                .map_err(|error| source.refused(error))?;
            source.merge(
                &observed
                    .finish()
                    .map_err(|_| source.refused(self.errors.protocol))?,
            );
        }
        if let Some(registration) = self.front.registration(path) {
            source
                .evidence
                .borrow_mut()
                .dependency(true, registration.acceptance());
            source
                .depth
                .borrow_mut()
                .observe_transparent(registration.depth());
        }
        // As a materialization of the checked body, Lower also depends on the
        // private shapes that body read, beyond their representation digests.
        for (read, _) in body.outcome.reads.iter() {
            if let ReadKey::Representation(id) = read
                && let Some(hash) = self.layouts.get(id)
            {
                source.record(ReadKey::LayoutBody(*id), memos::optional(Some(*hash)));
            }
        }
        let analyzed = body.outcome.result?;
        source.execution_signatures(analyzed.dependencies.iter());
        let signature = source.signature(path)?;
        let mut observed = ReadsBuilder::new();
        let staged = crate::comptime::prepare_staged_types(
            self.namespace.snapshot.types(),
            &analyzed.value,
            &mut FrozenProvider::new(
                &self.namespace.snapshot,
                path.0.package.clone(),
                &mut observed,
            ),
        );
        source.merge(&observed.finish().map_err(|_| QueryError::NotImplemented)?);
        let staged = staged
            .map_err(|error| lower_input_error(path, "preparing staged type inputs", error))?;
        let mut values = Values {
            source: source.clone(),
            pending: None,
        };
        let mut preparation = ValuePreparation::new(&analyzed.value);
        let mut constants = if kind == FRONT_LOWER_KIND
            && matches!(signature.kind, hirsel_types::SignatureKind::Const { .. })
        {
            let (_, constants) = source.constant_value(path).await?;
            constants
        } else {
            poll_fn(|_| {
                preparation
                    .poll(
                        self.namespace.snapshot.types(),
                        &analyzed.value,
                        &staged,
                        &signature,
                        &mut values,
                    )
                    .map(|r| r.map(|v| v.to_vec()))
            })
            .await
            .map_err(|error| lower_input_error(path, "preparing compile-time values", error))?
        };
        if let Some(selection) = selection {
            let declaration = &self.staged.declarations[&selection.declaration];
            source.record(
                ReadKey::Signature(selection.declaration),
                memos::signature_value(Some(declaration.signature.fingerprint)),
            );
            let expression = declaration.expression;
            let hirsel_syntax::ExprKind::Comptime(child) =
                analyzed.value.typed.syntax.tree.expressions[expression.0 as usize].kind
            else {
                return Err(QueryError::NotImplemented);
            };
            let ty = analyzed
                .value
                .typed
                .checked
                .expressions
                .iter()
                .find(|row| row.expression == expression)
                .ok_or(QueryError::NotImplemented)?
                .ty;
            let sizes = declaration
                .inputs
                .iter()
                .copied()
                .zip(selection.arguments.iter().copied())
                .collect();
            let value = evaluate(
                source.clone(),
                analyzed,
                staged,
                signature,
                expression,
                child,
                ty,
                constants.clone(),
                sizes,
            )
            .await?
            .result?;
            return Ok(Computed {
                value: Value::Constant(Box::new(Constants::new(value, constants))),
                verification: None,
                raw: None,
            });
        }
        if kind == FRONT_EVALUATE_KIND {
            let ItemKind::Const {
                value: expression, ..
            } = analyzed.value.typed.syntax.item.kind
            else {
                return Err(QueryError::NotImplemented);
            };
            let dictionary = crate::dictionaries::declaration_async(
                self.namespace.snapshot.types(),
                &self.memo,
                &analyzed.value,
                &signature,
                &constants,
                source,
            )
            .await?;
            let is_dictionary = dictionary.is_some();
            let value = if let Some(dictionary) = dictionary {
                dictionary
            } else if let Some((_, value)) = constants.iter().find(|(id, _)| *id == expression) {
                value.clone()
            } else {
                let ty = analyzed
                    .value
                    .typed
                    .checked
                    .expressions
                    .iter()
                    .find(|r| r.expression == expression)
                    .ok_or(QueryError::NotImplemented)?
                    .ty;
                evaluate(
                    source.clone(),
                    analyzed.clone(),
                    staged,
                    signature,
                    expression,
                    expression,
                    ty,
                    constants.clone(),
                    vec![],
                )
                .await?
                .result?
            };
            if is_dictionary && !constants.iter().any(|(id, _)| *id == expression) {
                constants.push((expression, value.clone()));
            }
            return Ok(Computed {
                value: Value::Constant(Box::new(Constants::new(value, constants))),
                verification: None,
                raw: None,
            });
        }
        let completed = source.active_owner();
        let lowering = self.lowering.as_deref().ok_or(QueryError::NotImplemented)?;
        self.count(path, |work| work.lowerings += 1);
        let output = lowering
            .lower(
                completed.namespace.snapshot.types(),
                &self.memo,
                &analyzed.value,
                &Comptime {
                    materialized: constants.clone(),
                    staged,
                },
                &signature,
            )
            .map_err(|error| lower_input_error(path, "lowering the accepted body", error))?;
        let definition = Arc::new(output.value);
        for dependency in &output.dependencies {
            source.record(
                ReadKey::Signature(dependency.definition),
                dependency.fingerprint,
            );
        }
        let mut verification = None;
        let mut acceptance = None;
        if self.verify == Verify::On {
            let observed = completed
                .validation_reads(&path.0.package, &source.reads)
                .await
                .map_err(|error| source.refused(error))?;
            source.merge(&observed.finish().map_err(|_| QueryError::NotImplemented)?);
            let (report, ran, accepted, evidence) = completed
                .namespace
                .snapshot
                .validate_source_artifact(&definition, |anchor| {
                    completed.namespace.anchors.record_owner(anchor)
                })
                .map_err(|_| source.refused(self.errors.resource))?;
            acceptance = accepted;
            source.evidence.borrow_mut().accepted(acceptance.clone());
            if ran {
                timings::count(Count::VRuns);
                self.count(path, |work| work.validation_runs += 1);
            }
            if let Some(error) = hirsel_validate::tooling_limit_error(&report) {
                return Err(error);
            }
            if !report.violations.is_empty() {
                return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "TIR validation".into(),
                    message: format!("{:?}", report.violations),
                }));
            }
            verification = evidence.map(|evidence| (definition.clone(), evidence));
        }
        if has_errors(&output.diagnostics) {
            return Err(QueryError::Diagnostics(output.diagnostics));
        }
        let retained = crate::memo::retained::decoded_tir_bytes(&definition);
        let definition_decode_layout = DefinitionDecodeLayout {
            retained,
            transient: retained,
        };
        let body = Arc::new(std::sync::OnceLock::new());
        let materialized = Arc::new(std::sync::OnceLock::new());
        // Scalar Lower inherits the Modes detail and then appends its
        // analyzed input again before the lowering adapter's dependencies.
        let mut dependencies = analyzed.dependencies.clone();
        dependencies.extend_from_slice(&analyzed.dependencies);
        dependencies.extend(output.dependencies);
        let constant_selection_bytes = constant_selection_bytes(&definition)?;
        let raw = Some((
            QueryOutput {
                value: definition.clone(),
                diagnostics: output.diagnostics.clone(),
                dependencies: dependencies.clone(),
            },
            constants,
        ));
        Ok(Computed {
            raw,
            value: Value::Lower(Box::new(LowerProduct {
                live: None,
                local_verified: None,
                constant_selection_bytes,
                definition: definition.path,
                body,
                diagnostics: output.diagnostics,
                dependencies: dependencies.into(),
                archived_metadata: None,
                verified: None,
                acceptance,
                materialized,
                definition_decode_layout,
            })),
            verification,
        })
    }
}
struct LowerCharge {
    _allowance: Arc<hirsel_engine_admission::MemoryReservation>,
    remaining: Arc<Mutex<u64>>,
    bytes: u64,
}
impl Drop for LowerCharge {
    fn drop(&mut self) {
        *self.remaining.lock().unwrap_or_else(|e| e.into_inner()) += self.bytes;
    }
}
impl FrontLowerQueries {
    fn output_action(
        &self,
        id: QueryId,
        key: &EntryKey,
        reads: &Reads,
    ) -> hirsel_artifact_store::Action {
        hirsel_artifact_store::Action(ContentHash::of_parts(
            "hirsel.front.live-action.v1",
            [
                key.0.0.as_slice(),
                key.1.0.as_slice(),
                self.namespace.manifest.0.as_slice(),
                crate::store::payload_reads_hash(reads).0.as_slice(),
                id.kind.to_le_bytes().as_slice(),
            ],
        ))
    }
    async fn publish_live(
        &self,
        id: QueryId,
        key: &EntryKey,
        mut outcome: Outcome,
        raw: Option<RawLowerOutput>,
        query_reads: &QueryReads,
    ) -> Result<ProductRef, Reference> {
        let _ = self.persist_live(id, key, &outcome, &raw);
        self.remember_live_carry(id, key, &outcome, &raw);
        let action = self.output_action(id, key, &outcome.reads);
        outcome.action = action;
        let metadata = (
            outcome.reads.clone(),
            outcome.depth.evidence(),
            outcome.executed,
            outcome.selections.clone(),
            self.namespace.manifest,
        );
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(size_of_val(&metadata) + size_of::<LowerCharge>() + 8 * size_of::<usize>());
        for (_, bytes) in outcome.reads.allocations() {
            weight.add(bytes);
        }
        weight.add(size_of_val(&*metadata.1.first));
        weight.add(
            staged::selection_bytes(&metadata.3)
                .try_into()
                .map_err(|_| self.errors.resource)?,
        );
        let metadata_bytes = weight.bytes();
        let required = outcome
            .retained_bytes()
            .and_then(|bytes| bytes.checked_add(metadata_bytes))
            .and_then(|bytes| bytes.checked_add(outcome_table_bytes(1)))
            .ok_or(self.errors.resource)?;
        while *self.retention.lock().unwrap_or_else(|e| e.into_inner()) < required {
            if !query_reads.evict_body().await {
                return Err(self.errors.resource);
            }
        }
        let charge = loop {
            let mut remaining = self.retention.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(next) = remaining.checked_sub(metadata_bytes) {
                *remaining = next;
                break Arc::new(LowerCharge {
                    _allowance: self._allowance.clone(),
                    remaining: self.retention.clone(),
                    bytes: metadata_bytes,
                });
            }
            drop(remaining);
            if !query_reads.evict_body().await {
                return Err(self.errors.resource);
            }
        };
        let observations = ProductRef::live(
            id.address,
            action,
            Arc::new(live_record::LiveRecord {
                value: metadata,
                domain: Domain {
                    kind: 0x4600_0040,
                    schema: 1,
                },
                records: self.records.clone(),
                source: (*self.namespace.anchors).clone(),
                serialization_working: self.weight.scratch,
                _admission: charge,
            }),
        );
        outcome.observations = Some(observations.clone());
        let product = match (&mut outcome.result, raw) {
            (Ok(Value::Lower(lower)), Some((output, materialized))) => {
                let path = output.value.path;
                let mut output = Some(FrontLowerOutput {
                    output,
                    materialized,
                    reads: outcome.reads.clone(),
                    depth: outcome.depth.evidence(),
                    execution: outcome.execution.clone(),
                    execution_subject: outcome.execution_subject,
                    executed: outcome.executed,
                    selections: outcome.selections.clone(),
                    acceptance: lower.acceptance.clone(),
                    implementation_revision: self
                        .inputs
                        .get(path.path())
                        .ok_or(self.errors.protocol)?
                        .captured
                        .implementation,
                    signature: self
                        .namespace
                        .snapshot
                        .signature(path)
                        .ok_or(self.errors.protocol)?
                        .as_ref()
                        .map_err(|_| self.errors.protocol)?
                        .fingerprint,
                    verified: lower.local_verified.clone(),
                    query_observations: Some(observations),
                    body_archive: lower.body.clone(),
                    constants_archive: lower.materialized.clone(),
                    owner: self.source.clone(),
                });
                let live = loop {
                    match LiveLowerProduct::from_retained(
                        (id.address, action),
                        output.take().expect("unpublished Lower"),
                        (*self.namespace.anchors).clone(),
                        self.records.clone(),
                        self.source.clone(),
                        self._allowance.clone(),
                        self.retention.clone(),
                    ) {
                        LiveAdmission::Admitted(live) => break live,
                        LiveAdmission::Blocked(value) => {
                            output = Some(*value);
                            if !query_reads.evict_body().await {
                                return Err(self.errors.resource);
                            }
                        }
                        LiveAdmission::Refused => return Err(self.errors.resource),
                    }
                };
                lower.live = Some(live.downgrade());
                ProductRef::lower(live)
            }
            (Ok(Value::Constant(value)), _) => {
                let (root, substitutions) = value
                    .decoded(
                        &self.records,
                        &self.namespace.anchors,
                        &*self.store,
                        self.weight.scratch,
                    )
                    .ok_or(self.errors.resource)?;
                let bytes = value
                    .retained_bytes()
                    .checked_add(metadata_bytes)
                    .ok_or(self.errors.resource)?
                    .checked_add(size_of_val(&root) as u64)
                    .and_then(|bytes| bytes.checked_add(size_of::<LowerCharge>() as u64 + 128))
                    .ok_or(self.errors.resource)?;
                let charge = loop {
                    let mut remaining = self.retention.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(next) = remaining.checked_sub(bytes) {
                        *remaining = next;
                        break Arc::new(LowerCharge {
                            _allowance: self._allowance.clone(),
                            remaining: self.retention.clone(),
                            bytes,
                        });
                    }
                    drop(remaining);
                    if !query_reads.evict_body().await {
                        return Err(self.errors.resource);
                    }
                };
                ProductRef::live(
                    id.address,
                    action,
                    Arc::new(live_record::LiveRecord {
                        value: (
                            root,
                            substitutions,
                            outcome.reads.clone(),
                            outcome.depth.evidence(),
                            outcome.executed,
                            outcome.selections.clone(),
                            self.namespace.manifest,
                        ),
                        domain: Domain {
                            kind: 0x4600_0041,
                            schema: 1,
                        },
                        records: self.records.clone(),
                        source: (*self.namespace.anchors).clone(),
                        serialization_working: self.weight.scratch,
                        _admission: charge,
                    }),
                )
            }
            _ => return Err(self.errors.protocol),
        };
        loop {
            let charge = outcome.retained_bytes().ok_or(self.errors.resource)?;
            let inserted = {
                let mut remaining = self.retention.lock().unwrap_or_else(|e| e.into_inner());
                let mut outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
                let selected = (id.kind, id.address.0);
                let old = outcomes.get(&selected).map_or(0, |(_, _, bytes)| *bytes);
                let growth = outcome_table_bytes(
                    outcomes.len() + usize::from(!outcomes.contains_key(&selected)),
                )
                .checked_sub(outcome_table_bytes(outcomes.len()))
                .ok_or(self.errors.resource)?;
                if let Some(next) = remaining
                    .checked_add(old)
                    .and_then(|n| n.checked_sub(charge))
                    .and_then(|n| n.checked_sub(growth))
                {
                    *remaining = next;
                    Some(outcomes.insert(selected, (None, Box::new(outcome.clone()), charge)))
                } else {
                    None
                }
            };
            if let Some(replaced) = inserted {
                drop(replaced);
                break;
            }
            if !query_reads.evict_body().await {
                return Err(self.errors.resource);
            }
        }
        Ok(product)
    }
}
impl FrontLowerOutput {
    pub(crate) fn spill(&self, scratch: u64) -> Option<()> {
        if self.body_archive.get().is_some() && self.constants_archive.get().is_some() {
            return Some(());
        }
        let records = self
            .owner
            .records
            .for_boundary(crate::store::ArchiveBoundary::RequiredSpill);
        let bytes = records.encode_tir(&self.owner.anchors, &self.output.value)?;
        if bytes.len() as u64 > scratch {
            return None;
        }
        let body = self
            .owner
            .store
            .put_content(crate::NATIVE_ORIGINAL_BODY, &bytes)?;
        drop(bytes);
        let bytes = records.encode(&self.owner.anchors, &self.materialized)?;
        if bytes.len() as u64 > scratch {
            return None;
        }
        let constants = self.owner.store.put_content(LOWER_MATERIALIZED, &bytes)?;
        let _ = self.body_archive.set(body);
        let _ = self.constants_archive.set(constants);
        Some(())
    }
}

struct Factory {
    owner: Arc<FrontLowerQueries>,
    _demand: Option<Arc<generation::demand::DemandProducer>>,
}
// This producer's settled Outcome allocations and provider archives have
// independent owners. Only the saved future/mailbox/working state belongs to
// the runtime continuation, and S5 retires it after destruction at home.
struct LowerTask(LocalFutureTask);
impl LocalQueryTask for LowerTask {
    fn id(&self) -> QueryId {
        self.0.id()
    }
    fn weight(&self) -> MemoryWeight {
        self.0.weight()
    }
    fn continuation_bytes(&self) -> u64 {
        self.0.weight().retained
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        self.0.poll(cx)
    }
}
impl LocalQueryFactory for Factory {
    fn weight(&self, id: QueryId) -> Result<MemoryWeight, Reference> {
        if let Some(demand) = self
            .owner
            .demand_producer()
            .filter(|demand| demand.owns_demand(id))
        {
            return Ok(demand.task_weight());
        }
        if self.owner.path(id).is_none()
            && let Some(owner) = self
                .owner
                .demand_producer()
                .and_then(|demand| demand.lower_for(id))
        {
            return owner.factory().weight(id);
        }
        if !matches!(id.kind, FRONT_LOWER_KIND | FRONT_EVALUATE_KIND)
            || self.owner.path(id).is_none()
        {
            return Err(self.owner.errors.protocol);
        }
        Ok(self.owner.weight)
    }
    fn create(&self, id: QueryId) -> Result<Box<dyn LocalQueryTask>, Reference> {
        if let Some(demand) = self
            .owner
            .demand_producer()
            .filter(|demand| demand.owns_demand(id))
        {
            return demand.task(id);
        }
        if self.owner.path(id).is_none()
            && let Some(owner) = self
                .owner
                .demand_producer()
                .and_then(|demand| demand.lower_for(id))
        {
            return owner.factory().create(id);
        }
        let weight = self.weight(id)?;
        let path = self.owner.path(id).ok_or(self.owner.errors.protocol)?;
        let owner = self.owner.clone();
        Ok(Box::new(LowerTask(LocalFutureTask::new(
            id,
            weight,
            self.owner.max_reads,
            self.owner.errors,
            move |reads| owner.run(id, path, reads),
        )?)))
    }
}
#[derive(Clone)]
struct Source {
    id: QueryId,
    root: DefId,
    owner: Arc<FrontLowerQueries>,
    reads: QueryReads,
    observed: Rc<RefCell<ReadsBuilder>>,
    selections: Rc<RefCell<staged::Selections>>,
    refusal: Rc<Cell<Option<Reference>>>,
    depth: Rc<RefCell<DepthEvidence<Span>>>,
    framed: bool,
    evidence: Rc<RefCell<Evidence>>,
    successor: Rc<RefCell<Option<SuccessorCapture>>>,
    diagnostic_sources: Rc<RefCell<Option<Arc<namespace::DiagnosticSources>>>>,
    restart: Rc<RefCell<Option<SuccessorCapture>>>,
}
struct PendingRequest<'a> {
    owner: &'a FrontLowerQueries,
    id: QueryId,
}
impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        self.owner.with_request_site(self.id, |site| *site = None);
    }
}
impl FrontLowerQueries {
    fn with_request_site<R>(
        &self,
        id: QueryId,
        update: impl FnOnce(&mut Option<Span>) -> R,
    ) -> Option<R> {
        if let Some(path) = self.paths.get(&(id.kind, id.address.0)) {
            let input = self.inputs.get_id(*path)?;
            let slot = usize::from(id.kind == FRONT_EVALUATE_KIND);
            return Some(update(
                &mut input.waiting[slot]
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()),
            ));
        }
        self.staged.with_request_site(id, update)
    }
}
impl Source {
    fn active_owner(&self) -> Arc<FrontLowerQueries> {
        self.successor
            .borrow()
            .as_ref()
            .map_or_else(|| self.owner.clone(), |next| next.owner().clone())
    }
    fn capture(&self) -> Arc<NamespaceCapture> {
        self.successor.borrow().as_ref().map_or_else(
            || self.owner.namespace.clone(),
            |successor| successor.namespace().clone(),
        )
    }
    fn successor_since(
        &self,
        previous: &NamespaceCapture,
    ) -> Result<Option<SuccessorCapture>, QueryError> {
        let successor = self.successor.borrow();
        match successor.as_ref() {
            Some(current) if current.namespace().identity() != previous.identity() => {
                SuccessorCapture::new(previous, current.owner().clone()).map(Some)
            }
            _ => Ok(None),
        }
    }
    fn join(&self, successor: Option<SuccessorCapture>) -> Result<(), QueryError> {
        let Some(successor) = successor else {
            return Ok(());
        };
        if successor.namespace().identity() == self.capture().identity() {
            return Ok(());
        }
        if !successor.follows(&self.capture()) {
            return Err(successor::refused(
                "dependency does not descend from its consumer capture",
            ));
        }
        let reads = self
            .observed
            .borrow()
            .clone()
            .finish()
            .map_err(|_| successor::refused("inconsistent reads before joining"))?;
        if successor
            .changed_input(
                self.root.path(),
                &reads,
                self.evidence.borrow().finish(&reads).is_some(),
            )
            .is_some()
        {
            *self.restart.borrow_mut() = Some(successor);
            return Err(successor::refused(
                "changed semantic inputs require the consumer to restart",
            ));
        }
        let joined = self.evidence.borrow_mut().join(
            &successor,
            self.root.path(),
            &reads,
            self.owner.verify,
        );
        let reads = match joined {
            Ok(reads) => reads,
            Err(error) => {
                *self.restart.borrow_mut() = Some(successor);
                return Err(error);
            }
        };
        let mut observed = ReadsBuilder::new();
        observed.merge(&reads);
        *self.observed.borrow_mut() = observed;
        *self.successor.borrow_mut() = Some(SuccessorCapture::new(
            &self.owner.namespace,
            successor.owner().clone(),
        )?);
        Ok(())
    }
    fn select(&self, rows: &[(DefId, Vec<u64>)]) -> Result<(), QueryError> {
        let mut selections = self.selections.borrow_mut();
        let count = selections
            .len()
            .checked_add(rows.len())
            .ok_or_else(|| self.refused(self.owner.errors.resource))?;
        let arguments = selections
            .iter()
            .chain(rows)
            .try_fold(0u64, |sum, (_, values)| {
                sum.checked_add(size_of_val(values.as_slice()) as u64)
            })
            .ok_or_else(|| self.refused(self.owner.errors.resource))?;
        let bytes = (count as u64)
            .checked_mul(size_of::<(DefId, Vec<u64>)>() as u64)
            .and_then(|bytes| bytes.checked_add(arguments))
            .and_then(|bytes| bytes.checked_mul(2))
            .ok_or_else(|| self.refused(self.owner.errors.resource))?;
        if bytes > self.owner.weight.scratch {
            return Err(self.refused(self.owner.errors.resource));
        }
        selections.extend_from_slice(rows);
        Ok(())
    }
    fn refused(&self, reference: Reference) -> QueryError {
        if self.refusal.get().is_none() {
            self.refusal.set(Some(reference));
        }
        // The internal phase result only unwinds the shared computation. The
        // owner returns the original runtime reference before publication.
        QueryError::NotImplemented
    }
    fn overflow(&self, site: Span) -> QueryError {
        let limit = hirsel_tir::ToolingLimit {
            kind: hirsel_tir::ToolingLimitKind::CallDepth,
            limit: dependency_limit(self.owner.namespace.limits.call_depth) as u64,
            site: Some(site),
        };
        QueryError::Operational(hirsel_types::OperationalFailure {
            operation: "compile-time tooling limit".into(),
            message: format!("{limit:?}"),
        })
    }
    fn merge(&self, reads: &Reads) {
        self.observed.borrow_mut().merge(reads);
    }
    fn record(&self, key: ReadKey, hash: ContentHash) {
        self.observed.borrow_mut().record(key, hash);
    }
    fn execution_signatures<'a>(
        &self,
        dependencies: impl IntoIterator<Item = &'a SignatureDependency>,
    ) {
        let owner = self.active_owner();
        for dependency in dependencies {
            if let Some(signature) = owner.namespace.snapshot.signature(dependency.definition) {
                self.record(
                    ReadKey::ExecutedSignature(dependency.definition),
                    memos::signature_value(signature.ok().map(|s| s.fingerprint)),
                );
            }
        }
    }
    fn signature(&self, path: &DefPath) -> Result<hirsel_types::SignatureRef, QueryError> {
        let owner = self.active_owner();
        let mut observed = ReadsBuilder::new();
        let result = SignatureProvider::signature(
            &mut FrozenProvider::new(
                &owner.namespace.snapshot,
                path.0.package.clone(),
                &mut observed,
            ),
            DefId::intern(path),
        );
        self.merge(&observed.finish().map_err(|_| QueryError::NotImplemented)?);
        result
    }
    async fn value(&self, path: &DefPath, kind: u32, site: Span) -> Result<Value, QueryError> {
        let owner = self.active_owner();
        if kind == FRONT_LOWER_KIND
            && let Some((generator, site)) = owner.generator
            && generator.path() == path
        {
            return Err(QueryError::Diagnostics(vec![Diagnostic::error(
                const { RuleId::of("CT-4") },
                site,
                "compile-time dependency cycle: this generator runs its own declaration",
            )]));
        }
        let id = owner.id(path, kind).ok_or_else(|| {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "Lower dependency".into(),
                message: format!(
                    "no {} producer is registered for {path}",
                    if kind == FRONT_EVALUATE_KIND {
                        "constant evaluation"
                    } else {
                        "body lowering"
                    }
                ),
            })
        })?;
        self.selected_value_in(owner, id, path, kind, site).await
    }
    async fn selected_value(
        &self,
        id: QueryId,
        path: &DefPath,
        kind: u32,
        site: Span,
    ) -> Result<Value, QueryError> {
        self.selected_value_in(self.active_owner(), id, path, kind, site)
            .await
    }
    async fn selected_value_in(
        &self,
        owner: Arc<FrontLowerQueries>,
        id: QueryId,
        path: &DefPath,
        kind: u32,
        site: Span,
    ) -> Result<Value, QueryError> {
        owner.with_request_site(self.id, |pending| {
            *pending = (kind == FRONT_EVALUATE_KIND).then_some(site);
        });
        let pending = PendingRequest {
            owner: &owner,
            id: self.id,
        };
        let selected = self.reads.read_consuming(id).await;

        drop(pending);
        if selected == Err(owner.errors.cycle) {
            self.record(
                ReadKey::ExecutedBody(DefId::intern(path)),
                memos::optional(Some(owner.inputs[path].captured.implementation)),
            );
            let members = self.reads.take_cycle();
            if members.is_empty() {
                return Err(self.refused(owner.errors.resource));
            }
            // Match the scalar coordinator's cycle_sites: pending evaluation
            // requests contribute their expression sites, not declaration names.
            let mut diagnostics: Vec<_> = members
                .into_iter()
                .filter_map(|id| owner.with_request_site(id, |site| *site).flatten())
                .chain((kind == FRONT_EVALUATE_KIND).then_some(site))
                .map(|site| {
                    Diagnostic::error(
                        const { hirsel_diag::RuleId::of("CT-4") },
                        site,
                        "compile-time dependency cycle",
                    )
                })
                .collect();
            diagnostics = crate::select_diagnostics(diagnostics, Some(&owner.namespace.sources));
            if diagnostics.is_empty() {
                return Err(self.refused(owner.errors.protocol));
            }
            return Err(QueryError::Diagnostics(vec![diagnostics.remove(0)]));
        }
        let mut owner = owner;
        let mut selected = selected;
        let mut selected_id = id;
        while let Some(row) = owner.delegated(selected_id, &selected) {
            owner = row.owner;
            selected = row.selected;
            selected_id = row.id;
        }
        if self.capture().identity() != owner.namespace.identity() {
            self.join(Some(SuccessorCapture::new(&self.capture(), owner.clone())?))?;
        }
        let outcome = owner
            .selected(selected_id, selected.clone())
            .ok_or_else(|| self.refused(selected.err().unwrap_or(owner.errors.protocol)))?;
        self.merge(
            &outcome
                .reads(&owner)
                .ok_or_else(|| self.refused(owner.errors.resource))?,
        );
        if let Some(sources) = &outcome.diagnostic_sources {
            *self.diagnostic_sources.borrow_mut() = Some(sources.clone());
        }
        self.select(&outcome.selections)?;
        self.evidence
            .borrow_mut()
            .dependency(outcome.executed, outcome.execution.as_ref());
        if let Ok(Value::Lower(lower)) = &outcome.result {
            self.evidence
                .borrow_mut()
                .accepted(lower.acceptance.clone());
        }
        let n = u32::from(self.framed);
        let depth = outcome.depth.evidence();
        let overflow = if kind == FRONT_EVALUATE_KIND {
            self.depth.borrow_mut().observe_frame(&depth, site);
            depth.request(n, site).err()
        } else {
            self.depth.borrow_mut().observe_transparent(&depth);
            depth.first[n.min(depth.limit()) as usize]
        };
        if let Some(site) = overflow {
            return Err(self.overflow(site));
        }
        self.record(
            ReadKey::ExecutedBody(DefId::intern(path)),
            memos::optional(Some(owner.inputs[path].captured.implementation)),
        );
        outcome.result
    }
    async fn constant_value(
        &self,
        path: &DefPath,
    ) -> Result<(Materialized, Vec<(hirsel_syntax::ExprId, Materialized)>), QueryError> {
        self.constant_at(
            path,
            self.owner
                .inputs
                .get(path)
                .map_or(Span::default(), |input| input.captured.site),
        )
        .await
    }
    async fn constant_at(
        &self,
        path: &DefPath,
        site: Span,
    ) -> Result<(Materialized, Vec<(hirsel_syntax::ExprId, Materialized)>), QueryError> {
        let active = self.active_owner();
        if active.front.query(path).is_none() {
            if let Some(value) = active
                .namespace
                .snapshot
                .builtin_dictionary_value(&active.memo, path)?
            {
                self.signature(path)?;
                return Ok((value, vec![]));
            }
        }
        match self.value(path, FRONT_EVALUATE_KIND, site).await? {
            Value::Constant(value) => value
                .decoded(
                    &self.active_owner().records,
                    &*self.capture().anchors,
                    &*self.active_owner().store,
                    self.active_owner().weight.scratch,
                )
                .ok_or(QueryError::NotImplemented),
            _ => Err(QueryError::NotImplemented),
        }
    }
}
impl s1::AsyncDependencySource for Source {
    async fn definition(&mut self, path: &DefPath) -> s1::DependencyProduct<DefinitionRef> {
        let previous = self.capture();
        let result = match self.value(path, FRONT_LOWER_KIND, Span::default()).await {
            Ok(Value::Lower(lower)) => match lower.decode(&self.active_owner()) {
                Some(output) => {
                    self.execution_signatures(output.dependencies.iter());
                    Ok(output.value)
                }
                None => Err(QueryError::NotImplemented),
            },
            Err(e) => Err(e),
            _ => Err(QueryError::NotImplemented),
        };
        let successor = self.successor_since(&previous);
        s1::DependencyProduct {
            successor: successor.as_ref().ok().and_then(Clone::clone),
            result: successor.and(result),
            reads: Reads::empty(),
        }
    }
    async fn constant(&mut self, path: &DefPath) -> s1::DependencyProduct<MaterializedRef> {
        let previous = self.capture();
        let result = self.constant_value(path).await.map(|(v, _)| Arc::new(v));
        let successor = self.successor_since(&previous);
        s1::DependencyProduct {
            successor: successor.as_ref().ok().and_then(Clone::clone),
            result: successor.and(result),
            reads: Reads::empty(),
        }
    }
    async fn constant_instance(
        &mut self,
        path: &DefPath,
        arguments: &[u64],
    ) -> s1::DependencyProduct<MaterializedRef> {
        if arguments.is_empty() {
            return s1::AsyncDependencySource::constant(self, path).await;
        }
        let previous = self.capture();
        let result = async {
            let active = self.active_owner();
            let id = active.staged_query(path, arguments)?;
            self.select(&[(DefId::intern(path), arguments.to_vec())])?;
            let declaration = active
                .staged
                .declarations
                .get(&DefId::intern(path))
                .ok_or(QueryError::NotImplemented)?;
            let owner = declaration.owner.clone();
            let site = active.inputs[&owner].captured.site;
            let Value::Constant(value) = self
                .selected_value_in(active, id, &owner, FRONT_EVALUATE_KIND, site)
                .await?
            else {
                return Err(QueryError::NotImplemented);
            };
            value
                .decoded(
                    &self.active_owner().records,
                    &*self.capture().anchors,
                    &*self.active_owner().store,
                    self.active_owner().weight.scratch,
                )
                .map(|(value, _)| Arc::new(value))
                .ok_or(QueryError::NotImplemented)
        }
        .await;
        let successor = self.successor_since(&previous);
        s1::DependencyProduct {
            successor: successor.as_ref().ok().and_then(Clone::clone),
            result: successor.and(result),
            reads: Reads::empty(),
        }
    }
    async fn instance_body(
        &mut self,
        path: &DefPath,
        arguments: &[hirsel_types::GenericArgument],
    ) -> s1::DependencyProduct<Option<DefPath>> {
        let previous = self.capture();
        let active = self.active_owner();
        let result = if let Some(producer) = active.demand_producer() {
            match producer
                .demand(
                    &previous,
                    path,
                    arguments,
                    previous
                        .syntax(path)
                        .map_or(Span::default(), |syntax| syntax.item.name_span),
                    self.reads.clone(),
                )
                .await
            {
                Ok(product) => {
                    if let Err(error) = &product.result {
                        if let Ok(failure) = producer.demand_failure(
                            path,
                            &previous,
                            error.clone(),
                            product.reads.clone(),
                        ) {
                            *self.diagnostic_sources.borrow_mut() = Some(failure.sources);
                        }
                    }
                    let joined = self.join(product.successor);
                    self.merge(&product.reads);
                    joined.and(product.result)
                }
                Err(error) => {
                    if let Ok(failure) =
                        producer.demand_failure(path, &previous, error.clone(), Reads::empty())
                    {
                        *self.diagnostic_sources.borrow_mut() = Some(failure.sources);
                    }
                    Err(error)
                }
            }
        } else {
            active
                .namespace
                .instances
                .resolve(
                    DefId::intern(path),
                    active.namespace.snapshot.types(),
                    arguments,
                )
                .map(|selected| selected.map(|id| id.path().clone()))
        };
        let successor = self.successor_since(&previous);
        s1::DependencyProduct {
            successor: successor.as_ref().ok().and_then(Clone::clone),
            result: successor.and(result),
            reads: Reads::empty(),
        }
    }
}
impl crate::dictionaries::DeclarationSource for Source {
    fn signature(&mut self, path: &DefPath) -> Result<hirsel_types::SignatureRef, QueryError> {
        Source::signature(self, path)
    }
    async fn evaluate(&mut self, input: EvaluationInput<'_>) -> Result<Materialized, QueryError> {
        evaluate(
            self.clone(),
            Arc::new(QueryOutput {
                value: input.analyzed.clone(),
                diagnostics: vec![],
                dependencies: vec![],
            }),
            input.staged.to_vec(),
            Arc::new(input.signature.clone()),
            input.expression,
            input.child,
            input.ty,
            input.constants.to_vec(),
            input.sizes.to_vec(),
        )
        .await?
        .result
    }
}
type Evaluation =
    Pin<Box<dyn Future<Output = Result<s1::DependencyProduct<Materialized>, QueryError>>>>;
struct Values {
    source: Source,
    pending: Option<Evaluation>,
}
impl PreparingSource for Values {
    fn signature(
        &mut self,
        path: &DefPath,
    ) -> Poll<Result<hirsel_types::SignatureRef, QueryError>> {
        Poll::Ready(self.source.signature(path))
    }
    fn constant(&mut self, path: &DefPath, site: Span) -> Poll<Result<Materialized, QueryError>> {
        if self.pending.is_none() {
            let source = self.source.clone();
            let path = path.clone();
            self.pending = Some(Box::pin(async move {
                Ok(s1::DependencyProduct {
                    successor: None,
                    result: source.constant_at(&path, site).await.map(|(v, _)| v),
                    reads: Reads::empty(),
                })
            }));
        }
        self.advance()
    }
    fn evaluate(&mut self, input: EvaluationInput<'_>) -> Poll<Result<Materialized, QueryError>> {
        if self.pending.is_none() {
            self.pending = Some(Box::pin(evaluate(
                self.source.clone(),
                Arc::new(QueryOutput {
                    value: input.analyzed.clone(),
                    diagnostics: vec![],
                    dependencies: vec![],
                }),
                input.staged.to_vec(),
                Arc::new(input.signature.clone()),
                input.expression,
                input.child,
                input.ty,
                input.constants.to_vec(),
                input.sizes.to_vec(),
            )));
        }
        self.advance()
    }
}
impl Values {
    fn advance(&mut self) -> Poll<Result<Materialized, QueryError>> {
        match self
            .pending
            .as_mut()
            .unwrap()
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.pending = None;
                Poll::Ready(result.and_then(|p| {
                    self.source.join(p.successor)?;
                    self.source.merge(&p.reads);
                    p.result
                }))
            }
        }
    }
}
#[allow(clippy::too_many_arguments)]
async fn evaluate(
    mut source: Source,
    analyzed: Arc<QueryOutput<AnalyzedBody>>,
    staged: Vec<(hirsel_syntax::ExprId, hirsel_types::TypeId)>,
    signature: hirsel_types::SignatureRef,
    expression: hirsel_syntax::ExprId,
    child: hirsel_syntax::ExprId,
    ty: hirsel_types::TypeId,
    constants: Vec<(hirsel_syntax::ExprId, Materialized)>,
    sizes: Vec<(hirsel_types::SizeParameterRef, u64)>,
) -> Result<s1::DependencyProduct<Materialized>, QueryError> {
    let owner = source.active_owner();
    let site = analyzed.value.typed.syntax.tree.expressions[expression.0 as usize]
        .origin
        .span;
    let lowering = owner
        .lowering
        .as_deref()
        .ok_or(QueryError::NotImplemented)?;
    owner.count(signature.definition.path(), |work| work.initializers += 1);
    source.evidence.borrow_mut().executed = true;
    let prepared = EvaluationInput {
        analyzed: &analyzed.value,
        staged: &staged,
        signature: &signature,
        expression,
        child,
        ty,
        constants: &constants,
        sizes: &sizes,
    }
    .lower(owner.namespace.snapshot.types(), &owner.memo, lowering)?;
    let previous = source.capture();
    let admission =
        s1::ValidatedEvaluation::recorded_in(prepared, previous.clone(), owner.verify, site);
    source.merge(&admission.reads);
    let runtime = owner.runtime_types()?;
    let admission = admission.result?;
    source
        .evidence
        .borrow_mut()
        .accepted(admission.acceptance().cloned());
    let product = admission
        .run_async(
            &owner.namespace.sources,
            owner.namespace.limits,
            &runtime,
            &mut source,
        )
        .await;
    source.join(product.value.successor)?;
    source.merge(&product.value.reads);
    source.evidence.borrow_mut().accepted(product.acceptance);
    let owner = source.active_owner();
    let result = product.value.result.and_then(|(value, ty)| {
        let mut observed = ReadsBuilder::new();
        let result = crate::materialized::reify_value(
            owner.namespace.snapshot.types(),
            &owner.memo,
            &mut Materializer {
                provider: FrozenProvider::new(
                    &owner.namespace.snapshot,
                    signature.definition.path().0.package.clone(),
                    &mut observed,
                ),
                source: &source,
            },
            value,
            ty,
            site,
        );
        source.merge(&observed.finish().map_err(|_| QueryError::NotImplemented)?);
        result
    });
    Ok(s1::DependencyProduct {
        successor: source.successor_since(&previous)?,
        result,
        reads: Reads::empty(),
    })
}

struct Materializer<'a> {
    provider: FrozenProvider<'a>,
    source: &'a Source,
}
impl crate::materialized::MaterializationSource for Materializer<'_> {
    fn signature(&mut self, id: DefId) -> Result<hirsel_types::SignatureRef, QueryError> {
        SignatureProvider::signature(&mut self.provider, id)
    }
    fn body_view(&mut self, id: DefId) -> Result<hirsel_types::SignatureRef, QueryError> {
        let public = SignatureProvider::signature(&mut self.provider, id)?;
        let private = self.provider.body_view(id)?;
        if !Arc::ptr_eq(&public, &private)
            && let Some(input) = self.source.owner.inputs.get(id.path())
        {
            self.source.record(
                ReadKey::ExecutedBody(id),
                memos::optional(Some(input.captured.implementation)),
            );
        }
        Ok(private)
    }
}
impl QueryEngine {
    /// Capture input identities and immutable phase handles. This does no body
    /// checking, lowering or interpretation, and never calls the S1 controller.
    pub fn front_lower_queries(
        &mut self,
        definitions: &[DefPath],
        front: Arc<FrontBodyQueries>,
        weight: MemoryWeight,
        max_reads: usize,
        allowance: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
    ) -> Result<Arc<FrontLowerQueries>, QueryError> {
        self.front_lower_queries_in_generator(
            definitions,
            front,
            weight,
            max_reads,
            allowance,
            errors,
            None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn front_lower_queries_in_generator(
        &mut self,
        definitions: &[DefPath],
        front: Arc<FrontBodyQueries>,
        weight: MemoryWeight,
        max_reads: usize,
        mut allowance: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
        generator: Option<(DefId, Span)>,
    ) -> Result<Arc<FrontLowerQueries>, QueryError> {
        front.namespace().require_environment(self)?;
        let refusal = || {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "front Lower admission".into(),
                message: "immutable Lower inputs exceed admission or lack phase/source bindings"
                    .into(),
            })
        };
        let pending = generator.is_none()
            && !self.private_bootstrap_active
            && (!self.front_staged_roots(definitions)?.is_empty()
                || !front.namespace().instances.declarations.is_empty()
                || definitions.iter().any(|path| {
                    front
                        .namespace()
                        .bodies
                        .get(&DefId::intern(path))
                        .is_some_and(|body| body.generated)
                        && !front
                            .namespace()
                            .registrations
                            .contains_key(&DefId::intern(path))
                }));
        let demand_admission = if pending {
            let part = allowance.bytes() / 6;
            let mut take = || allowance.split(part).ok_or_else(refusal);
            Some(generation::demand::DemandAdmission {
                carry: None,
                front: take()?,
                lower: take()?,
                text: take()?,
                syntax: take()?,
            })
        } else {
            None
        };
        let partition = LowerPartition::new(allowance);
        let lower = self.front_lower_queries_in_partition(
            definitions,
            front,
            weight,
            max_reads,
            &partition,
            errors,
            generator,
        )?;
        if let Some(admission) = demand_admission {
            generation::demand::DemandProducer::new(
                self,
                lower.clone(),
                admission,
                weight,
                max_reads,
                errors,
            )?;
        }
        Ok(lower)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn front_lower_queries_in_partition(
        &mut self,
        definitions: &[DefPath],
        front: Arc<FrontBodyQueries>,
        weight: MemoryWeight,
        max_reads: usize,
        partition: &LowerPartition,
        errors: RuntimeErrors,
        generator: Option<(DefId, Span)>,
    ) -> Result<Arc<FrontLowerQueries>, QueryError> {
        front.namespace().require_environment(self)?;
        let refusal = || {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "front Lower admission".into(),
                message: "immutable Lower inputs exceed admission or lack phase/source bindings"
                    .into(),
            })
        };
        let input_bytes = FrontLowerQueries::input_bytes(definitions).ok_or_else(refusal)?;
        let retention = partition.remaining.clone();
        {
            let mut remaining = retention.lock().unwrap_or_else(|e| e.into_inner());
            *remaining = remaining.checked_sub(input_bytes).ok_or_else(refusal)?;
        }
        let mut input_charge = LowerInputCharge {
            remaining: retention.clone(),
            bytes: input_bytes,
        };
        let store = self.result_store.clone().ok_or_else(refusal)?;
        let lowering = self.adapters.lowering.clone();
        // V's consuming universe can differ from Front's captured snapshot.
        // Its private proofs need the same admitted source owner, working
        // grant and accepted bodies as Front's own headers.
        if !front.namespace().registrations.is_empty() {
            let snapshot = self.signature_inputs();
            let registrations: Vec<_> = front.namespace().registrations.values().cloned().collect();
            if let (Some(headers), Some(previous)) = (
                snapshot.header_products(),
                front.namespace().snapshot.header_products(),
            ) {
                headers.inherit_registration_scope(previous, &registrations);
            }
        }
        let mut inputs = Vec::with_capacity(definitions.len());
        for path in definitions {
            if front.query(path).is_none() {
                return Err(refusal());
            }
            if let Some(accepted) = front.registration(path) {
                accepted.validate(self)?;
            }
            let captured = front
                .namespace()
                .bodies
                .get(&DefId::intern(path))
                .and_then(|input| input.queries.as_ref())
                .ok_or_else(refusal)?;
            inputs.push((
                DefId::intern(path),
                Input {
                    captured: captured.clone(),
                    waiting: [Mutex::new(None), Mutex::new(None)],
                },
            ));
        }
        let inputs = Inputs::new(inputs);
        if inputs
            .retained_bytes()
            .is_none_or(|bytes| bytes > input_bytes)
        {
            return Err(refusal());
        }
        let namespace = front.namespace().clone();
        let packages: std::collections::BTreeSet<_> =
            definitions.iter().map(|path| &path.0.package).collect();
        let opaque = self
            .declarations
            .opaque_paths()
            .cloned()
            .collect::<Vec<_>>();
        let layouts: BTreeMap<_, _> = opaque
            .into_iter()
            .filter(|path| packages.contains(&path.0.package))
            .filter_map(|path| Some((DefId::intern(&path), self.implementation_hash(&path)?)))
            .collect();
        let layout_bytes = (layouts.len() as u64)
            .checked_mul(
                (std::mem::size_of::<(DefId, ContentHash)>() + 6 * std::mem::size_of::<usize>())
                    as u64,
            )
            .ok_or_else(refusal)?;
        {
            let mut retention = retention.lock().unwrap_or_else(|e| e.into_inner());
            *retention = retention.checked_sub(layout_bytes).ok_or_else(refusal)?;
            input_charge.bytes += layout_bytes;
        }
        let staged = staged::Staged::new(
            namespace
                .snapshot
                .header_products()
                .map_or(&self.staged_constants, |headers| headers.staged_constants()),
        );
        {
            let mut remaining = retention.lock().unwrap_or_else(|error| error.into_inner());
            *remaining = remaining
                .checked_sub(staged.retained_bytes())
                .ok_or_else(refusal)?;
            input_charge.bytes += staged.retained_bytes();
        }
        let paths = inputs
            .iter()
            .flat_map(|(path, input)| {
                let path = DefId::intern(path);
                [
                    (
                        (
                            FRONT_LOWER_KIND,
                            namespace
                                .query_id(FRONT_LOWER_KIND, input.captured.lower.0)
                                .address
                                .0,
                        ),
                        path,
                    ),
                    (
                        (
                            FRONT_EVALUATE_KIND,
                            namespace
                                .query_id(FRONT_EVALUATE_KIND, input.captured.evaluate.0)
                                .address
                                .0,
                        ),
                        path,
                    ),
                ]
            })
            .collect();
        let allowance = partition.allowance.clone();
        let anchors = (*namespace.anchors).clone();
        let records = crate::NativeRecords::new(self.types.clone(), store.clone());
        let source = Arc::new(LowerSource {
            _inputs: input_charge,
            anchors: anchors.clone(),
            records: records.clone(),
            store: store.clone(),
            _sources: self.sources.clone(),
            _allowance: allowance.clone(),
        });
        let lower = Arc::new(FrontLowerQueries {
            demand: Default::default(),
            demand_owner: Default::default(),
            forwarded: Mutex::new(BTreeMap::new()),
            source,
            inputs,
            generator,
            staged,
            paths,
            layouts,
            front,
            namespace,
            memo: self.memo.clone(),
            lowering,
            runtime: std::sync::OnceLock::new(),
            verify: self.verify,
            verify_store: self.verify_store,
            records,
            store,
            outcomes: Mutex::new(BTreeMap::new()),
            carry: Mutex::new(None),
            carry_changes: Mutex::new(None),
            clean_memos: Mutex::new(BTreeSet::new()),
            work: Mutex::new(BTreeMap::new()),
            weight,
            max_reads,
            errors,
            retention,
            _allowance: allowance,
        });
        Ok(lower)
    }
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    #[test]
    fn retained_outcome_charges_capacity_and_compact_read_rows() {
        let mut bytes = Vec::with_capacity(64 << 10);
        bytes.push(1);
        let capacity = bytes.capacity();
        let substitutions = Vec::with_capacity(4);
        let substitutions_bytes =
            substitutions.capacity() * std::mem::size_of::<(hirsel_syntax::ExprId, Materialized)>();
        let outcome = Outcome {
            diagnostic_sources: None,
            result: Ok(Value::Constant(Box::new(Constants::new(
                Materialized {
                    root: hirsel_tir::ValueNode {
                        ty: hirsel_types::TypeId::DANGLING,
                        kind: hirsel_tir::ValueKind::Constant(hirsel_tir::Constant::Bytes(bytes)),
                    },
                },
                substitutions,
            )))),
            reads: Reads::from_vec(vec![(
                ReadKey::Signature(DefId::intern(&DefPath::core(&"long".repeat(4096)))),
                ContentHash::default(),
            )])
            .unwrap(),
            reads_decode_bytes: 0,
            selections: Vec::new(),
            depth: DepthEvidence::leaf(32).into(),
            executed: false,
            execution: None,
            execution_subject: ContentHash::default(),
            action: hirsel_artifact_store::Action(ContentHash::default()),
            observations: None,
        };
        let expected = size_of::<Outcome>()
            + 2 * size_of::<usize>()
            + size_of::<Constants>()
            + 2 * size_of::<usize>()
            + capacity
            + substitutions_bytes
            + std::mem::size_of::<(ReadKey, ContentHash)>()
                .next_multiple_of(std::mem::align_of::<usize>())
            + 2 * std::mem::size_of::<usize>()
            + outcome.depth.retained_bytes();
        assert_eq!(outcome.retained_bytes(), Some(expected as u64));
        assert_eq!(outcome_table_bytes(0), 0);
        assert!(outcome_table_bytes(1) >= std::mem::size_of::<(Reference, Outcome, u64)>() as u64);
        assert!(outcome_table_bytes(6) > outcome_table_bytes(1));
    }
}

fn lower_input_error(path: &DefPath, stage: &str, error: QueryError) -> QueryError {
    match error {
        QueryError::NotImplemented => QueryError::Operational(hirsel_types::OperationalFailure {
            operation: "Lower request".into(),
            message: format!("{stage} for {path} requires an unavailable compiler input"),
        }),
        error => error,
    }
}
