//! Actual resolve/check/modes body work as admitted S5 tasks. These reuse the
//! existing immutable Worker and scalar canonical codec; no batch pool or
//! mutable engine is invoked by a factory or a poll. Publication is separate.
use super::*;
use crate::products::{
    LiveAnalyzedProduct, LiveSummaryProduct, ProductSource, SharedProductSource,
    WeakAnalyzedProduct,
};
use crate::snapshot::OwnedSourceContext;
use hirsel_engine_runtime::*;
use hirsel_engine_store::{Domain, EntryKey, Reference, ResultStore, VariantRef};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicU64, Ordering::Relaxed},
};

mod carry;
mod compact_store;
pub(in crate::queries) use compact_store::load_publication as load_scheduled_publication;
mod lint_facts;
mod observations;
mod retained;
use carry::CarryChanges;
pub use carry::FrontCarry;
use lint_facts::LintFacts;
mod inputs;
mod source;
use inputs::Inputs;
pub(in crate::queries) use inputs::OperationFrontInputs;
mod storage;
pub use source::FrontInputFootprint;
use source::{SiblingFile, SiblingSources};
pub use storage::{FRONT_STORAGE_KIND, FrontStorageOutput, FrontStorageQueries};
pub const FRONT_BODY_KIND: u32 = 0x4600_0001;
const FAILURE: Domain = Domain {
    kind: 0x4600_0002,
    schema: 1,
};
type Key = ContentHash;
type CheckedProduct = Option<(Arc<CheckedSummary>, Arc<Reads>, Arc<OrderedSignatureReads>)>;
struct SignatureInput {
    signature: Result<Arc<QueryOutput<SignatureRef>>, QueryError>,
    reads: Arc<Reads>,
}
enum FrontState {
    Failed {
        record: Reference,
        error: Box<QueryError>,
    },
    Settled {
        archive: Option<Reference>,
        publication: ProductRef,
        body: Option<WeakAnalyzedProduct>,
        diagnostics: Vec<Diagnostic>,
        lowering: lowering::LoweringUse,
    },
}
struct PendingBody {
    product: LiveAnalyzedProduct,
    charge: u64,
}
struct FrontOutcome {
    state: FrontState,
    terminal: Option<Reference>,
    pending_body: Option<PendingBody>,
    facts: Arc<FrontFacts>,
}
impl std::ops::Deref for FrontOutcome {
    type Target = FrontFacts;
    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}
struct FrontFacts {
    definition: DefId,
    phase: Option<QueryKind>,
    reads: Arc<Reads>,
    diagnostic_phases: Vec<(Diagnostic, String)>,
    checked: CheckedProduct,
    checked_reader: OnceLock<(
        Arc<dyn ResultStore>,
        crate::queries::reader_receipts::ReaderReceipt,
    )>,
    /// The package dead-arm lint's facts of a checked body.
    dead_arms: Option<LintFacts>,
    table_keys: Option<Arc<crate::table_keys::TableKeyUse>>,
    memos: Box<[(crate::memo::MemoKey, Arc<Reads>)]>,
    diagnostic_sources: Option<Arc<namespace::DiagnosticSources>>,
    _backing: Arc<LiveSourceBacking>,
}
enum FrontSlot {
    Live {
        outcome: FrontOutcome,
        observations: Option<Arc<FrontObservations>>,
    },
    Archived(ArchivedFront),
}
pub(crate) struct FrontCheckFacts {
    pub result: Result<Vec<Diagnostic>, QueryError>,
    pub phase: Option<QueryKind>,
    pub incomplete: Option<QueryKind>,
    pub reads: Arc<Reads>,
    pub source_rules: bool,
    pub lowering: Option<lowering::LoweringUse>,
    pub dead_arms: Option<crate::tools::DeadArmUse>,
}
impl FrontOutcome {
    fn failed(record: Reference, error: QueryError, facts: Arc<FrontFacts>) -> Self {
        Self {
            state: FrontState::Failed {
                record,
                error: Box::new(error),
            },
            terminal: None,
            pending_body: None,
            facts,
        }
    }
    fn settled(
        archive: Option<Reference>,
        publication: ProductRef,
        diagnostics: Vec<Diagnostic>,
        lowering: lowering::LoweringUse,
        facts: Arc<FrontFacts>,
    ) -> Self {
        Self {
            state: FrontState::Settled {
                archive,
                publication,
                body: None,
                diagnostics,
                lowering,
            },
            terminal: None,
            pending_body: None,
            facts,
        }
    }
}
impl FrontFacts {
    fn capture(
        definition: &DefPath,
        phase: Option<QueryKind>,
        reads: Arc<Reads>,
        diagnostic_phases: Vec<(Diagnostic, String)>,
        checked: CheckedProduct,
        dead_arms: Option<crate::tools::DeadArmUse>,
        table_keys: Option<Arc<crate::table_keys::TableKeyUse>>,
        memos: Vec<(crate::memo::MemoKey, Arc<Reads>)>,
        backing: Arc<LiveSourceBacking>,
    ) -> Arc<Self> {
        Arc::new(Self {
            definition: DefId::intern(definition),
            phase,
            reads,
            diagnostic_phases,
            checked,
            checked_reader: OnceLock::new(),
            dead_arms: dead_arms.map(LintFacts::capture),
            table_keys,
            memos: memos.into_boxed_slice(),
            diagnostic_sources: None,
            _backing: backing,
        })
    }
}
#[derive(Clone)]
struct ArchivedFront {
    facts: Reference,
    observations: Arc<FrontObservations>,
}
struct FrontObservations {
    rows: observations::Rows,
    _backing: Option<Arc<LiveSourceBacking>>,
}
#[derive(Clone, Copy, Debug)]
pub struct FrontMemoryRefusal {
    pub owner: &'static str,
    pub required: u64,
    pub available: u64,
    pub settled: usize,
    pub archived: usize,
    pub pinned: usize,
}
struct Input {
    definition: DefId,
    text: ContentHash,
    implementation: ContentHash,
    registration: Option<Arc<AcceptedFrontRegistration>>,
    key: EntryKey,
    weight: MemoryWeight,
}
/// One immutable namespace/source view. A source registration transaction must
/// select its view before constructing queries; producer completion cannot
/// publish declarations, fuel or a later source root's diagnostics.
struct FrontSourceBacking {
    products: SharedProductSource,
    admitted: u64,
    allowance: Arc<hirsel_engine_admission::MemoryReservationPool>,
    _inputs: hirsel_engine_admission::MemoryReservation,
    _sources: SourceMap,
}
impl header_products::HeaderAllowance for FrontSourceBacking {
    fn take_header(&self, bytes: u64) -> Option<hirsel_engine_admission::MemoryReservation> {
        self.allowance.take(bytes)
    }
    fn available_header_bytes(&self) -> u64 {
        self.allowance.available_bytes()
    }
}
struct LiveSourceBacking {
    _source: Arc<FrontSourceBacking>,
    retention: Arc<Mutex<u64>>,
    debit: Mutex<FrontDebit>,
}
struct FrontDebit {
    charge: u64,
    allowance: hirsel_engine_admission::MemoryReservation,
    observation: Option<Arc<LiveSourceBacking>>,
}
impl LiveSourceBacking {
    fn add(&self, allowance: hirsel_engine_admission::MemoryReservation) -> Option<()> {
        self.debit
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .allowance
            .join(allowance)
            .ok()
    }
    fn charge(&self) -> u64 {
        self.debit.lock().unwrap_or_else(|e| e.into_inner()).charge
    }
    fn absorb(&self, other: Arc<Self>) -> Option<()> {
        let mut incoming = other.debit.lock().unwrap_or_else(|e| e.into_inner());
        let mut debit = self.debit.lock().unwrap_or_else(|e| e.into_inner());
        let bytes = incoming.allowance.bytes();
        let allowance = incoming.allowance.split(bytes)?;
        debit.allowance.join(allowance).ok()?;
        debit.charge = debit.charge.checked_add(incoming.charge)?;
        incoming.charge = 0;
        Some(())
    }
    fn refund_to(&self, required: u64) -> bool {
        let mut debit = self.debit.lock().unwrap_or_else(|e| e.into_inner());
        let Some(bytes) = debit
            .charge
            .checked_sub(required)
            .filter(|bytes| *bytes != 0)
        else {
            return false;
        };
        let Some(allowance) = debit.allowance.split(bytes) else {
            return false;
        };
        debit.charge = required;
        *self.retention.lock().unwrap_or_else(|e| e.into_inner()) += bytes;
        drop(allowance);
        true
    }
    fn split(&self, bytes: u64) -> Option<Arc<Self>> {
        let mut debit = self.debit.lock().unwrap_or_else(|e| e.into_inner());
        let next = debit.charge.checked_sub(bytes)?;
        let allowance = debit.allowance.split(bytes)?;
        debit.charge = next;
        let observation = Arc::new(Self {
            _source: self._source.clone(),
            retention: self.retention.clone(),
            debit: Mutex::new(FrontDebit {
                charge: bytes,
                allowance,
                observation: debit.observation.take(),
            }),
        });
        // The original value also keeps the transferred read charge until its
        // last alias drops. The child owns no reference back to this lease.
        debit.observation = Some(observation.clone());
        Some(observation)
    }
}
impl Drop for LiveSourceBacking {
    fn drop(&mut self) {
        let mut retention = self.retention.lock().unwrap_or_else(|e| e.into_inner());
        *retention += self
            .debit
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .charge;
    }
}
#[derive(Clone)]
struct Forwarded {
    owner: Arc<FrontBodyQueries>,
    selected: Result<ProductRef, Reference>,
    alias: Result<ProductRef, Reference>,
    successor: scheduled_lower::SuccessorCapture,
}
pub struct FrontBodyQueries {
    demand: Mutex<std::sync::Weak<generation::demand::DemandProducer>>,
    forwarded: Mutex<BTreeMap<Key, Forwarded>>,
    /// A late dependency invalidates this whole consumer attempt.
    discovery_dependency: Mutex<Option<DefPath>>,
    inputs: Inputs,
    _operation_inputs: Arc<OperationFrontInputs>,
    types: Arc<hirsel_types::TypeInterner>,
    memo: Arc<hirsel_types::TypeMemo>,
    namespace: Arc<NamespaceCapture>,
    declarations: Arc<Declarations>,
    citations: OwnedSourceContext,
    resolver: Option<Arc<dyn ResolverPhase>>,
    checker: Option<Arc<dyn CheckerPhase>>,
    modes: Option<Arc<dyn ModesPhase>>,
    environment: ContentHash,
    candidates: operation_inputs::InputIndex<DefId, ContentHash>,
    records: Arc<persistent::TypeRecords>,
    input_records: crate::NativeRecords,
    store: Arc<dyn ResultStore>,
    terminal_cache: Option<Box<dyn Send + Sync>>,
    work: Mutex<BTreeMap<(DefId, QueryKind), u64>>,
    store_adoptions: AtomicU64,
    /// Full-rescan refusals of stored body entries: by the first changed read
    /// of the newest variant, in `ReadKind::ALL` order, then by own text.
    refusals: [AtomicU64; hirsel_query_products::reads::ReadKind::ALL.len()],
    text_refusals: AtomicU64,
    /// Stored entry validations plus memo rows written.
    memo_operations: AtomicU64,
    verify_store: hirsel_engine_store::VerifyStore,
    slots: Mutex<BTreeMap<Key, FrontSlot>>,
    input_slots: Mutex<(BTreeMap<DefId, usize>, Option<Arc<LiveSourceBacking>>)>,
    /// Optional body retention bound within the existing Front allowance.
    body_cache_limit: u64,
    carry: Mutex<Option<Arc<FrontCarry>>>,
    /// Subscription generations validated against this immutable source view.
    carry_changes: Mutex<Option<Arc<CarryChanges>>>,
    /// Memo rows adopted from the carry without validation. They were neither
    /// marked nor scanned, so inspection does not count them as touched.
    clean_memos: Mutex<BTreeSet<crate::memo::MemoKey>>,
    errors: RuntimeErrors,
    refusal: Mutex<Option<FrontMemoryRefusal>>,
    retention: Arc<Mutex<u64>>,
    // Own immutable snapshot and output metadata admission until every task and
    // external output reader releases this view.
    read_pool: std::sync::OnceLock<Mutex<hirsel_query_products::reads::ReadPool>>,
    _allowance: Arc<FrontSourceBacking>,
    _header_allowance: Option<Arc<dyn header_products::HeaderAllowance>>,
}
/// A held output shares its snapshot reservation; dropping the runtime cannot
/// release admission while a caller still retains the checked body or sources.
#[derive(Clone)]
pub struct FrontBodyOutput {
    pub outcome: ModeResult,
    _facts: Arc<FrontFacts>,
    _body_owner: Option<LiveAnalyzedProduct>,
}
/// Checked-only source-growth input. It survives a later Modes failure and
/// carries the checked stage's complete reads rather than a provisional union.
#[derive(Clone)]
pub struct FrontCheckedOutput {
    pub(crate) table_keys: Arc<crate::table_keys::TableKeyUse>,
    pub summary: Arc<CheckedSummary>,
    pub(crate) dependencies: Arc<OrderedSignatureReads>,
    pub reads: Arc<Reads>,
    _facts: Arc<FrontFacts>,
}
impl FrontBodyQueries {
    #[track_caller]
    fn resource_refusal(&self) -> Reference {
        if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
            let site = std::panic::Location::caller();
            crate::queries::operation_owner_trace(format_args!(
                "OWNER RESOURCE Front producer {}:{} in namespace {:?}",
                site.file(),
                site.line(),
                self.namespace.identity(),
            ));
        }
        self.errors.resource
    }

    pub(crate) fn runtime_error(&self, reference: Reference) -> Option<QueryError> {
        let message = if reference == self.errors.resource {
            format!(
                "Front producer exceeded its admitted resources; retained {} of {} bytes; refusal {:?}",
                self.retained_bytes(),
                self.admitted_bytes(),
                self.memory_refusal(),
            )
        } else if reference == self.errors.cycle {
            "Front producer has a query dependency cycle".into()
        } else if reference == self.errors.cancelled {
            "Front producer was cancelled".into()
        } else if reference == self.errors.panicked {
            "Front producer panicked".into()
        } else {
            crate::requests::source_graph::runtime_diagnostic(&*self.store, reference)?
        };
        Some(QueryError::Operational(hirsel_types::OperationalFailure {
            operation: "compiler query runtime".into(),
            message,
        }))
    }
    #[track_caller]
    fn protocol_refusal(&self) -> Reference {
        crate::requests::source_graph::protocol_failure(
            &*self.store,
            self.errors,
            None,
            &format!(
                "Front producer state is unavailable in namespace {:?}",
                self.namespace.identity()
            ),
        )
    }
    #[track_caller]
    fn protocol_failure(&self, query: QueryId, detail: &str) -> Reference {
        crate::requests::source_graph::protocol_failure(
            &*self.store,
            self.errors,
            Some(query),
            detail,
        )
    }
    fn delegated(&self, selected: &Result<ProductRef, Reference>) -> Option<Forwarded> {
        self.forwarded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .find(|row| &row.alias == selected)
            .cloned()
    }
    pub(crate) fn successor_for(
        &self,
        path: &DefPath,
    ) -> Option<scheduled_lower::SuccessorCapture> {
        let query = self.query(path)?;
        self.forwarded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&query.address.0)
            .map(|row| row.successor.clone())
    }
    pub(crate) fn bind_demand(
        &self,
        demand: &Arc<generation::demand::DemandProducer>,
    ) -> Result<(), QueryError> {
        let mut installed = self.demand.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = installed.upgrade()
            && !Arc::ptr_eq(&previous, demand)
        {
            return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "Front demand binding".into(),
                message: "immutable Front inputs already have a live demand producer".into(),
            }));
        }
        *installed = Arc::downgrade(demand);
        Ok(())
    }
    pub(crate) fn demand_producer(&self) -> Option<Arc<generation::demand::DemandProducer>> {
        if self.namespace.consumers_admitted() {
            return None;
        }
        self.demand
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade()
    }
    pub fn discovery_dependency(&self) -> Option<DefPath> {
        self.discovery_dependency
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    pub(crate) fn require_discovery(&self, path: &DefPath) {
        if self.namespace.consumers_admitted() {
            self.discovery_dependency
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get_or_insert_with(|| path.clone());
        }
    }
    pub(in crate::queries) fn definition(&self, query: QueryId) -> Option<DefId> {
        (query.kind == FRONT_BODY_KIND).then(|| {
            self.inputs
                .get(&query.address.0)
                .map(|input| input.definition)
        })?
    }
    pub(crate) fn has_cached_query(&self, query: QueryId) -> bool {
        query.kind == FRONT_BODY_KIND
            && self
                .slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&query.address.0)
    }
    pub(crate) fn owns_query(&self, query: QueryId) -> bool {
        query.kind == FRONT_BODY_KIND
            && (self.inputs.contains_key(&query.address.0)
                || self.inputs.demand(&query.address.0).is_some())
    }
    pub(crate) fn diagnostic_phase(&self, diagnostic: &Diagnostic) -> Option<String> {
        let matches = |candidate: &Diagnostic| {
            candidate.rule == diagnostic.rule && candidate.primary == diagnostic.primary
        };
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let mut live = slots.values().filter_map(|slot| match slot {
            FrontSlot::Live { outcome, .. } => Some(outcome),
            FrontSlot::Archived(_) => None,
        });
        live.clone()
            .find_map(|outcome| {
                outcome
                    .diagnostic_phases
                    .iter()
                    .find(|(candidate, _)| matches(candidate))
                    .map(|(_, phase)| phase.clone())
            })
            .or_else(|| {
                live.find_map(|outcome| {
                    let diagnostics = match &outcome.state {
                        FrontState::Settled { diagnostics, .. } => diagnostics.as_slice(),
                        FrontState::Failed { error, .. } => match error.as_ref() {
                            QueryError::Diagnostics(diagnostics) => diagnostics.as_slice(),
                            _ => &[],
                        },
                    };
                    diagnostics
                        .iter()
                        .any(matches)
                        .then(|| outcome.phase.map(|phase| phase.phase_name().to_owned()))
                        .flatten()
                })
            })
    }
    pub fn namespace(&self) -> &Arc<NamespaceCapture> {
        &self.namespace
    }
    pub(crate) fn covers_scope(
        &self,
        definitions: &[DefPath],
        namespace: &Arc<NamespaceCapture>,
        weight: MemoryWeight,
    ) -> bool {
        Arc::ptr_eq(&self.namespace, namespace)
            && self.inputs.len() == definitions.len()
            && definitions.iter().all(|path| self.query(path).is_some())
            && self.inputs.values().all(|input| input.weight == weight)
    }
    pub(crate) fn snapshot(&self) -> &Arc<SignatureSnapshot> {
        &self.namespace.snapshot
    }
    /// Retain a typed settled Summary under its own grant. Its producer keeps
    /// the existing complete source/type reservation alive through the view.
    pub fn live_summary(
        self: &Arc<Self>,
        product: ProductRef,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Option<crate::products::LiveSummaryProduct> {
        let query = QueryId {
            kind: FRONT_BODY_KIND,
            address: product.address,
        };
        if self.completed_product(query)?.ok()? != product {
            return None;
        }
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let outcome = match slots.get(&product.address.0)? {
            FrontSlot::Archived(_) => return None,
            FrontSlot::Live { outcome, .. } => outcome,
        };
        let value = match &outcome.state {
            FrontState::Failed { .. } => return None,
            FrontState::Settled { publication, .. } => publication
                .live_value::<crate::products::LiveSummaryProduct>()?
                .value_arc(),
        };
        crate::products::LiveSummaryProduct::new(
            (product.address, product.action),
            value,
            ProductSource::prepare(
                self.namespace.anchors.as_ref().clone(),
                self.input_records.clone(),
                self._allowance.clone(),
            )?,
            allowance,
        )
    }
    pub(crate) fn prepare_generator(
        &self,
        syntax: Arc<ItemSyntax>,
        signer: &dyn SignaturePhase,
        instance: bool,
    ) -> s1::DependencyProduct<generation::task::PreparedGenerator> {
        let (Some(resolver), Some(checker), Some(modes)) = (
            self.resolver.as_deref(),
            self.checker.as_deref(),
            self.modes.as_deref(),
        ) else {
            return s1::DependencyProduct {
                successor: None,
                result: Err(QueryError::NotImplemented),
                reads: Reads::empty(),
            };
        };
        let _validation = match self.namespace.demand_validation() {
            Ok(validation) => validation,
            Err(error) => {
                return s1::DependencyProduct {
                    successor: None,
                    result: Err(error),
                    reads: Reads::empty(),
                };
            }
        };
        let path = syntax.item.definition.clone();
        // A closed instance is a parser-created single-declaration source. Its
        // private generator reads that source, which is not an ordinary body
        // input in this view. Keep the source context explicit and local to
        // preparation, under the admitted worker's existing scratch grant.
        let declarations = if instance {
            self.declarations
                .with_archived_siblings(Arc::new(generation::task::InstanceSiblings(
                    syntax.clone(),
                )))
        } else {
            self.declarations.clone()
        };
        generation::task::prepare(
            &path,
            syntax,
            &self.types,
            &self.memo,
            &self.namespace.snapshot,
            &declarations,
            &self.citations,
            self.environment,
            &generation::task::GeneratorPhases {
                signature: signer,
                resolver,
                checker,
                modes,
            },
        )
    }
    pub(crate) fn stage_source(&self) -> crate::queries::scheduled_stage::StageSource {
        crate::queries::scheduled_stage::StageSource {
            snapshot: self.namespace.snapshot.clone(),
            memo: self.memo.clone(),
            records: self.input_records.clone(),
            anchors: self.namespace.anchors.clone(),
            store: self.store.clone(),
        }
    }
    /// The exact immutable signature universe selected with these P inputs.
    pub fn signatures(&self) -> Arc<SignatureSnapshot> {
        self.namespace.snapshot.clone()
    }
    fn signature(&self, input: &Input) -> Result<Arc<QueryOutput<SignatureRef>>, QueryError> {
        if let Some(product) = self
            .namespace
            .snapshot
            .header_products()
            .and_then(|headers| headers.product(input.definition))
        {
            return product?.result.clone();
        }
        match self.namespace.snapshot.signature(input.definition) {
            Some(Ok(signature)) => Ok(Arc::new(output(signature.clone()))),
            Some(Err(error)) => Err(error.clone()),
            None => Err(QueryError::NotImplemented),
        }
    }
    fn signature_input(
        &self,
        input: &Input,
        cx: &mut dyn QueryContext,
    ) -> Result<SignatureInput, QueryError> {
        let mut reads = ReadsBuilder::new();
        let product = self
            .namespace
            .snapshot
            .header_products()
            .and_then(|headers| headers.working_product(input.definition, cx))
            .transpose()?;
        if let Some(product) = &product
            && let Some(entry) = &product.entry
        {
            reads.merge(entry.reads());
        }
        if let Some(registration) = &input.registration {
            reads.merge(&registration.reads);
        }
        let signature =
            product.map_or_else(|| self.signature(input), |product| product.result.clone());
        reads
            .finish()
            .map(|reads| SignatureInput {
                signature,
                reads: Arc::new(reads),
            })
            .map_err(|read| {
                QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "front inherited reads".into(),
                    message: format!("inconsistent registration read: {read:?}"),
                })
            })
    }
    fn syntax(&self, input: &Input) -> Result<Arc<ItemSyntax>, QueryError> {
        // Accepted literals are parser-created immutable source, and legitimately
        // combine the written header's anchor with generated body anchors. They
        // never enter the ordinary single-source untrusted archive decoder.
        if let Some(registration) = &input.registration {
            let syntax = &registration.syntax;
            // Instance bodies retain the declaring source's lexical owner,
            // as on the scalar path. The opaque registration binds the query
            // identity; the syntax's owner is not a second instance identity.
            return (registration.definition() == input.definition.path()
                && syntax.text_hash == input.implementation)
                .then(|| syntax.clone())
                .ok_or_else(|| {
                    QueryError::Operational(hirsel_types::OperationalFailure {
                        operation: "Front syntax recovery".into(),
                        message: format!(
                            "accepted registration differs from the input for {}",
                            input.definition.path()
                        ),
                    })
                });
        }
        let syntax = self.namespace.syntax(input.definition.path())?;
        if syntax.text_hash != input.implementation {
            return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "Front syntax recovery".into(),
                message: format!(
                    "captured implementation differs from the input for {}",
                    input.definition.path()
                ),
            }));
        }
        if generation::instance_bodied(&syntax) {
            let mut header = (*syntax).clone();
            if let hirsel_syntax::ItemKind::Function { body, .. } = &mut header.item.kind {
                *body = None;
            }
            return Ok(Arc::new(header));
        }
        Ok(syntax)
    }

    /// Potential task, result and counter slots. Completed read/summary
    /// metadata consumes the remaining allowance only on demand. Archive bytes
    /// belong to the selected provider, not to these reference-only rows.
    pub fn input_bytes(definitions: &[DefPath]) -> Option<u64> {
        (definitions.len() as u64)
            .checked_mul(
                (std::mem::size_of::<Input>()
                    + std::mem::size_of::<FrontSlot>()
                    + 3 * std::mem::size_of::<(crate::memo::MemoKey, Arc<Reads>)>()
                    + 8 * std::mem::size_of::<crate::memo::MemoKey>()
                    // Includes the per-root archived observation owner.
                    // Its memo keys use the reserved memo slots above.
                    + 32 * std::mem::size_of::<usize>()
                    + std::mem::size_of::<(DefId, Key)>()
                    + std::mem::size_of::<(Key, Key)>()) as u64,
            )?
            .checked_add(NamespaceCapture::input_bytes(definitions)?)?
            .checked_add(
                (1328 // Frozen pre-owner Front planning budget.
                    + 104 // Frozen source backing planning budget.
                    + std::mem::size_of::<Mutex<u64>>()
                    + 4 * std::mem::size_of::<usize>()) as u64
                    + hirsel_engine_admission::MemoryReservationPool::metadata_bytes()
                    + ProductSource::metadata_bytes(),
            )
    }
    /// Compact diagnostic retention within this source's original allowance.
    /// Ordinary requests retain their flat read representation.
    pub fn compact_retained_reads(&self) {
        self.read_pool.get_or_init(Mutex::default);
    }
    pub(crate) fn retained_reads(
        &self,
        cx: &mut dyn QueryContext,
        reads: &Arc<Reads>,
    ) -> Option<Arc<Reads>> {
        if Reads::tracked_by(reads, &self.retention) {
            return Some(reads.clone());
        }
        let Some(pool) = self.read_pool.get() else {
            return Some(reads.clone());
        };
        self.retained_reads_in(cx, reads, pool)
    }
    /// Admit a consumer's shared observations without selecting compact read
    /// retention for ordinary Front body producers.
    pub(crate) fn retained_reads_in(
        &self,
        cx: &mut dyn QueryContext,
        reads: &Arc<Reads>,
        pool: &Mutex<hirsel_query_products::reads::ReadPool>,
    ) -> Option<Arc<Reads>> {
        let pool = self.read_pool.get().unwrap_or(pool);
        if Reads::tracked_by(reads, &self.retention) {
            return Some(reads.clone());
        }
        loop {
            let charge = pool
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .required_tracked_bytes(reads)?;
            if self.make_room(cx, charge).is_none() {
                let available = (*self.retention.lock().unwrap_or_else(|e| e.into_inner()))
                    .min(self._allowance.allowance.available_bytes());
                self.record_refusal("read observations", charge, available);
                return None;
            }
            let mut pool = pool.lock().unwrap_or_else(|e| e.into_inner());
            let admission = pool.prepare_tracked(reads)?;
            let charge = admission.required_bytes();
            let mut remaining = self.retention.lock().unwrap_or_else(|e| e.into_inner());
            if *remaining < charge {
                continue;
            }
            let available = self._allowance.allowance.available_bytes();
            let mut allowance = self._allowance.allowance.take(available)?;
            let retained = Arc::new(admission.intern(
                &mut allowance,
                &mut remaining,
                self.retention.clone(),
            )?);
            Reads::bind_tracked_owner(&retained);
            return Some(retained);
        }
    }
    fn reclaim_reads(&self) -> bool {
        let Some(pool) = self.read_pool.get() else {
            return false;
        };
        let reclaimed = pool.lock().unwrap_or_else(|e| e.into_inner()).reclaim();
        *self.retention.lock().unwrap_or_else(|e| e.into_inner()) += reclaimed;
        reclaimed != 0
    }

    fn pool_completion_reads(
        &self,
        cx: &mut dyn QueryContext,
        outcome: &mut ModeResult,
        checked: &mut CheckedProduct,
        memos: &mut [(crate::memo::MemoKey, Arc<Reads>)],
    ) -> Option<()> {
        self.pool_observed_reads(cx, &mut outcome.reads, &mut outcome.summary, checked, memos)
    }
    fn pool_observed_reads(
        &self,
        cx: &mut dyn QueryContext,
        reads: &mut Arc<Reads>,
        summary: &mut Option<Arc<PublishedSummary>>,
        checked: &mut CheckedProduct,
        memos: &mut [(crate::memo::MemoKey, Arc<Reads>)],
    ) -> Option<()> {
        if let Some(summary) = summary.as_mut() {
            let summary = Arc::make_mut(summary);
            summary.share_checked_storage();
            if summary.reads.as_ref() == reads.as_ref()
                && summary.reads.observed().eq(reads.observed())
            {
                summary.reads = reads.clone();
            }
        }
        if let Some((_, checked_reads, _)) = checked
            && checked_reads.as_ref() == reads.as_ref()
            && checked_reads.observed().eq(reads.observed())
        {
            *checked_reads = reads.clone();
        }
        for (_, observed) in memos.iter_mut() {
            if observed.as_ref() == reads.as_ref() && observed.observed().eq(reads.observed()) {
                *observed = reads.clone();
            } else if let Some((_, checked_reads, _)) = checked
                && observed.as_ref() == checked_reads.as_ref()
                && observed.observed().eq(checked_reads.observed())
            {
                *observed = checked_reads.clone();
            }
        }
        if self.read_pool.get().is_none() {
            return Some(());
        }
        // Arrays carry refundable leases; the source-local dictionary owns
        // each distinct observed row once across settled and archived facts.
        let mut shared = BTreeMap::<usize, Arc<Reads>>::new();
        let mut intern = |reads: &mut Arc<Reads>| -> Option<()> {
            let identity = Arc::as_ptr(reads) as usize;
            let retained = if let Some(retained) = shared.get(&identity) {
                retained.clone()
            } else {
                let retained = self.retained_reads(cx, reads)?;
                shared.insert(identity, retained.clone());
                retained
            };
            *reads = retained;
            Some(())
        };
        intern(reads)?;
        if let Some(summary) = summary {
            intern(&mut Arc::make_mut(summary).reads)?;
        }
        if let Some((_, reads, _)) = checked {
            intern(reads)?;
        }
        for (_, reads) in memos {
            intern(reads)?;
        }
        Some(())
    }
    fn slot_inventory(len: usize, pending: usize) -> u64 {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        if len != 0 {
            weight.tree::<Key, FrontSlot>(len);
        }
        if pending != 0 {
            weight.tree::<DefId, usize>(pending);
        }
        if len != 0 {
            weight.tree::<(DefId, QueryKind), u64>(3 * len);
            weight.tree::<crate::memo::MemoKey, ()>(3 * len);
        }
        weight.add(
            len * (3 * size_of::<(crate::memo::MemoKey, Arc<Reads>)>()
                + 8 * size_of::<crate::memo::MemoKey>()
                + 32 * size_of::<usize>()
                + 2 * size_of::<Arc<LiveSourceBacking>>()),
        );
        weight.bytes()
    }
    fn admit_input_slot(&self, cx: &mut dyn QueryContext, definition: DefId) -> Option<()> {
        let key = self.inputs.key_for(definition)?;
        loop {
            let mut ledger = self.input_slots.lock().unwrap_or_else(|e| e.into_inner());
            let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(readers) = ledger.0.get_mut(&definition) {
                *readers = readers.checked_add(1)?;
                return Some(());
            }
            let state = (slots.len(), ledger.0.len());
            let roots = state.0 + state.1 + usize::from(!slots.contains_key(&key));
            let required = Self::slot_inventory(roots, state.1 + 1).max(
                self.slot_metadata_bytes(&ledger.0, &slots)
                    .checked_add(Self::slot_inventory(1, 1))?,
            );
            let held = ledger.1.as_ref().map_or(0, |backing| backing.charge());
            let bytes = required.saturating_sub(held);
            drop(slots);
            drop(ledger);
            let admitted = if bytes != 0 {
                Some(self.owner_grant(cx, bytes, "Front demand slots")?)
            } else {
                None
            };
            let mut ledger = self.input_slots.lock().unwrap_or_else(|e| e.into_inner());
            let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(readers) = ledger.0.get_mut(&definition) {
                *readers = readers.checked_add(1)?;
                return Some(());
            }
            if state != (slots.len(), ledger.0.len())
                || held != ledger.1.as_ref().map_or(0, |backing| backing.charge())
            {
                continue;
            }
            if let Some((allowance, backing)) = admitted {
                backing.add(allowance)?;
                if let Some(owner) = &ledger.1 {
                    owner.absorb(backing)?;
                } else {
                    ledger.1 = Some(backing);
                }
            }
            ledger.0.insert(definition, 1);
            return Some(());
        }
    }
    fn slot_metadata_bytes(
        &self,
        pending: &BTreeMap<DefId, usize>,
        slots: &BTreeMap<Key, FrontSlot>,
    ) -> u64 {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        if !slots.is_empty() {
            weight.tree::<Key, FrontSlot>(slots.len());
        }
        if !pending.is_empty() {
            weight.tree::<DefId, usize>(pending.len());
        }
        let work = self.work.lock().unwrap_or_else(|e| e.into_inner());
        if !work.is_empty() {
            weight.tree::<(DefId, QueryKind), u64>(work.len());
        }
        let clean = self.clean_memos.lock().unwrap_or_else(|e| e.into_inner());
        if !clean.is_empty() {
            weight.tree::<crate::memo::MemoKey, ()>(clean.len());
        }
        for slot in slots.values() {
            let observations = match slot {
                FrontSlot::Archived(archived) => Some(&archived.observations),
                FrontSlot::Live { observations, .. } => observations.as_ref(),
            };
            if let Some(observations) = observations
                && observations._backing.is_none()
            {
                weight.add(observations.rows.metadata_bytes() as usize);
            }
        }
        if weight.bytes() != 0 {
            weight.add(
                hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<
                    LiveSourceBacking,
                >::uninit()) as usize,
            );
        }
        let bytes = weight.bytes();
        if pending.is_empty() {
            bytes
        } else {
            // An admitted running task can still add its completion rows and
            // phase counters. Pressure may only trim the idle planning surplus.
            bytes.max(Self::slot_inventory(
                slots.len() + pending.len(),
                pending.len(),
            ))
        }
    }
    /// Persistent completion index and work counters. Reconstructible cache
    /// payloads and pending demand identities are not completed observations.
    pub fn retained_observation_metadata_bytes(&self) -> u64 {
        let ledger = self.input_slots.lock().unwrap_or_else(|e| e.into_inner());
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let archives = slots
            .values()
            .filter_map(|slot| match slot {
                FrontSlot::Archived(archived) => Some(&archived.observations),
                FrontSlot::Live { observations, .. } => observations.as_ref(),
            })
            .filter(|observations| observations._backing.is_some())
            .map(|observations| {
                observations.rows.metadata_bytes()
                    + hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<
                        LiveSourceBacking,
                    >::uninit())
            })
            .sum::<u64>();
        self.slot_metadata_bytes(&ledger.0, &slots) + archives
    }
    fn trim_slot_admission(&self) -> bool {
        let mut ledger = self.input_slots.lock().unwrap_or_else(|e| e.into_inner());
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let required = self.slot_metadata_bytes(&ledger.0, &slots);
        if required == 0 {
            return ledger.1.take().is_some();
        }
        ledger
            .1
            .as_ref()
            .is_some_and(|backing| backing.refund_to(required))
    }
    fn release_pending_slot(&self, definition: DefId) {
        {
            let mut ledger = self.input_slots.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(readers) = ledger.0.get_mut(&definition) {
                *readers -= 1;
                if *readers == 0 {
                    ledger.0.remove(&definition);
                }
            }
            if ledger.0.is_empty() {
                ledger.0 = BTreeMap::new();
            }
        }
        self.trim_slot_admission();
    }
    fn retain(
        &self,
        cx: &mut dyn QueryContext,
        outcome: &ModeResult,
        checked: &CheckedProduct,
        table_keys: Option<&Arc<crate::table_keys::TableKeyUse>>,
        memos: &Vec<(crate::memo::MemoKey, Arc<Reads>)>,
        dead_arms: Option<&crate::tools::DeadArmUse>,
    ) -> Option<Arc<LiveSourceBacking>> {
        let mut charge = retained::compact_metadata(outcome, checked, table_keys, memos)?;
        if let Some(summary) = &outcome.summary {
            charge = charge.checked_add(LiveSummaryProduct::shared_payload_bytes(summary)?)?;
        }
        charge = charge
            .checked_add(hirsel_engine_admission::retained_arc_bytes(
                &std::mem::MaybeUninit::<FrontFacts>::uninit(),
            ))?
            .checked_add(
                (memos.len() * std::mem::size_of::<(crate::memo::MemoKey, Arc<Reads>)>()) as u64,
            )?
            .checked_add(if outcome.result.is_err() {
                std::mem::size_of::<QueryError>() as u64
            } else {
                0
            })?
            .checked_add(dead_arms.map_or(0, LintFacts::capture_bytes))?;
        let (allowance, backing) = self.owner_grant(cx, charge, "compact facts")?;
        // The complete payload and its lease are one owner. No later failure
        // can consume a debit without retaining the corresponding reservation.
        backing.add(allowance)?;
        Some(backing)
    }
    fn install_outcome(&self, key: Key, mut outcome: FrontOutcome, scratch: u64) {
        if outcome.terminal.is_none() {
            outcome.terminal = self
                .slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
                .and_then(|slot| match slot {
                    FrontSlot::Live { outcome, .. } => outcome.terminal,
                    FrontSlot::Archived(archived) => Some(archived.facts),
                });
            if outcome.terminal.is_none() {
                self.store_compact(key, &mut outcome, scratch);
            }
        }
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if self.body_cache_limit < self._allowance.admitted
            && let Some(charge) = outcome.pending_body.as_ref().map(|body| body.charge)
        {
            let mut used: u64 = slots
                .iter()
                .filter_map(|(stored, slot)| match slot {
                    FrontSlot::Live { outcome, .. } if *stored != key => {
                        outcome.pending_body.as_ref().map(|body| body.charge)
                    }
                    _ => None,
                })
                .sum();
            while charge > self.body_cache_limit.saturating_sub(used) {
                let retired = slots.iter_mut().find_map(|(stored, slot)| match slot {
                    FrontSlot::Live { outcome, .. }
                        if *stored != key
                            && matches!(
                                outcome.state,
                                FrontState::Settled {
                                    archive: Some(_),
                                    ..
                                }
                            ) =>
                    {
                        outcome.pending_body.take()
                    }
                    _ => None,
                });
                let Some(retired) = retired else {
                    outcome.pending_body.take();
                    break;
                };
                used -= retired.charge;
                drop(retired);
            }
        }
        match slots.entry(key) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                // This slot belongs to one immutable input view. Restoring
                // its facts preserves the original required checkpoint.
                outcome.terminal = outcome.terminal.or_else(|| match entry.get() {
                    FrontSlot::Live { outcome, .. } => outcome.terminal,
                    FrontSlot::Archived(archived) => Some(archived.facts),
                });
                let observations = match entry.get() {
                    FrontSlot::Live { observations, .. } => observations.clone(),
                    FrontSlot::Archived(archived) => Some(archived.observations.clone()),
                };
                entry.insert(FrontSlot::Live {
                    outcome,
                    observations,
                });
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(FrontSlot::Live {
                    outcome,
                    observations: None,
                });
            }
        }
        drop(slots);
    }
    /// Actual phase invocations, including failed or discarded producers.
    fn completed_product(&self, query: QueryId) -> Option<Result<ProductRef, Reference>> {
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let outcome = match slots.get(&query.address.0)? {
            FrontSlot::Archived(_) => return None,
            FrontSlot::Live { outcome, .. } => outcome,
        };
        Some(match &outcome.state {
            FrontState::Failed { record, .. } => Err(*record),
            FrontState::Settled { publication, .. } => Ok(publication.clone()),
        })
    }
    fn make_room(&self, cx: &mut dyn QueryContext, charge: u64) -> Option<()> {
        // Headers and source syntax borrow from the same pool without reducing
        // this metadata counter. Reclaim idle consumers on either limit.
        while *self.retention.lock().unwrap_or_else(|e| e.into_inner()) < charge
            || self._allowance.allowance.available_bytes() < charge
        {
            if self.reclaim_reads() {
                continue;
            }
            if self.evict_pending_body() {
                continue;
            }
            if !cx.evict_body()
                && !self.evict_idle(cx.scratch_bytes())
                && !self
                    .namespace
                    .snapshot
                    .header_products()
                    .is_some_and(|headers| headers.evict())
            {
                return None;
            }
        }
        Some(())
    }
    pub(in crate::queries) fn evict_idle(&self, scratch: u64) -> bool {
        if self.evict_pending_body() {
            return true;
        }
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let selected = slots.iter().find_map(|(key, slot)| match slot {
            FrontSlot::Archived(_) => None,
            FrontSlot::Live { outcome, .. } => match &outcome.state {
                FrontState::Failed { .. } => None,
                FrontState::Settled { publication, .. } => (publication
                    .live_value::<crate::products::LiveSummaryProduct>()
                    .is_some_and(|summary| {
                        publication.live_owners() == 1 && summary.cache_owners(1)
                    })
                    && Arc::strong_count(&outcome.facts) == 2)
                    .then_some(*key),
            },
        });
        let Some(key) = selected else {
            return false;
        };
        let (outcome, observations) = match slots.get(&key) {
            Some(FrontSlot::Archived(_)) | None => return false,
            Some(FrontSlot::Live {
                outcome,
                observations,
            }) => (outcome, observations),
        };
        let Some(archived) = self.archive_facts(key, outcome, observations.clone(), scratch) else {
            return false;
        };
        slots.insert(key, FrontSlot::Archived(archived));
        drop(slots);
        self.reclaim_reads();
        true
    }
    pub fn memory_refusal(&self) -> Option<FrontMemoryRefusal> {
        *self.refusal.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn record_refusal(&self, owner: &'static str, charge: u64, available: u64) {
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let (mut settled, mut archived, mut pinned) = (0, 0, 0);
        for slot in slots.values() {
            match slot {
                FrontSlot::Archived(_) => archived += 1,
                FrontSlot::Live { outcome, .. } => {
                    settled += 1;
                    let pinned_live = match &outcome.state {
                        FrontState::Failed { .. } => true,
                        FrontState::Settled { publication, .. } => {
                            Arc::strong_count(&outcome.facts) != 2
                                || publication
                                    .live_value::<crate::products::LiveSummaryProduct>()
                                    .is_none_or(|summary| {
                                        publication.live_owners() != 1 || !summary.cache_owners(1)
                                    })
                        }
                    };
                    if pinned_live {
                        pinned += 1;
                    }
                }
            }
        }
        self.refusal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(FrontMemoryRefusal {
                owner,
                required: charge,
                available,
                settled,
                archived,
                pinned,
            });
    }
    fn owner_grant(
        &self,
        cx: &mut dyn QueryContext,
        value_bytes: u64,
        owner: &'static str,
    ) -> Option<(
        hirsel_engine_admission::MemoryReservation,
        Arc<LiveSourceBacking>,
    )> {
        let backing_bytes = hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<
            LiveSourceBacking,
        >::uninit());
        let charge = value_bytes.checked_add(backing_bytes)?;
        if self.make_room(cx, charge).is_none() {
            let available = (*self.retention.lock().unwrap_or_else(|e| e.into_inner()))
                .min(self._allowance.allowance.available_bytes());
            self.record_refusal(owner, charge, available);
            return None;
        }
        let mut remaining = self.retention.lock().unwrap_or_else(|e| e.into_inner());
        let next = remaining.checked_sub(charge)?;
        let mut allowance = self._allowance.allowance.take(charge)?;
        let backing_allowance = allowance.split(backing_bytes)?;
        *remaining = next;
        drop(remaining);
        Some((
            allowance,
            Arc::new(LiveSourceBacking {
                _source: self._allowance.clone(),
                retention: self.retention.clone(),
                debit: Mutex::new(FrontDebit {
                    charge,
                    allowance: backing_allowance,
                    observation: None,
                }),
            }),
        ))
    }
    fn admit_summary(
        &self,
        cx: &mut dyn QueryContext,
        address: hirsel_artifact_store::Address,
        action: hirsel_artifact_store::Action,
        summary: Arc<PublishedSummary>,
        facts: Arc<FrontFacts>,
    ) -> Option<ProductRef> {
        let tuple_bytes = hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<(
            Arc<FrontFacts>,
            Arc<LiveSourceBacking>,
        )>::uninit());
        let (mut allowance, backing) = self.owner_grant(
            cx,
            LiveSummaryProduct::shared_owner_bytes().checked_add(tuple_bytes)?,
            "Summary publication",
        )?;
        let tuple_grant = allowance.split(tuple_bytes)?;
        backing.add(tuple_grant)?;
        let backing = Arc::new((facts, backing));
        let product = LiveSummaryProduct::new_backed(
            (address, action),
            summary,
            ProductSource::backed(self._allowance.products.clone(), backing),
            &mut allowance,
        )?;
        product.publish(self.store.as_ref()).ok()
    }
    fn publish_facts(
        &self,
        cx: &mut dyn QueryContext,
        key: Key,
        reference: Option<Reference>,
        action: hirsel_artifact_store::Action,
        mut outcome: ModeResult,
        mut checked: CheckedProduct,
        dead_arms: Option<crate::tools::DeadArmUse>,
        table_keys: Option<Arc<crate::table_keys::TableKeyUse>>,
        mut memos: Vec<(crate::memo::MemoKey, Arc<Reads>)>,
    ) -> Option<FrontOutcome> {
        self.pool_completion_reads(cx, &mut outcome, &mut checked, &mut memos)?;
        let backing = self.retain(
            cx,
            &outcome,
            &checked,
            table_keys.as_ref(),
            &memos,
            dead_arms.as_ref(),
        )?;
        let facts = FrontFacts::capture(
            &outcome.definition,
            outcome.phase,
            outcome.reads.clone(),
            outcome.diagnostic_phases.clone(),
            checked,
            dead_arms,
            table_keys,
            memos,
            backing,
        );
        match outcome.result {
            Err(error) => Some(FrontOutcome::failed(reference?, error, facts)),
            Ok(output) => {
                let publication = self.admit_summary(
                    cx,
                    hirsel_artifact_store::Address(key),
                    action,
                    outcome.summary?,
                    facts.clone(),
                )?;
                Some(FrontOutcome::settled(
                    reference,
                    publication,
                    output.diagnostics.clone(),
                    lowering::LoweringUse::capture(&output.value.typed),
                    facts,
                ))
            }
        }
    }
    fn admit_body(
        &self,
        cx: &mut dyn QueryContext,
        address: hirsel_artifact_store::Address,
        action: hirsel_artifact_store::Action,
        outcome: ModeResult,
        summary: LiveSummaryProduct,
    ) -> Option<ProductRef> {
        let (mut allowance, backing) = self.owner_grant(
            cx,
            LiveAnalyzedProduct::shared_required_bytes(&outcome)?,
            "analyzed body",
        )?;
        let product = LiveAnalyzedProduct::new(
            (address, action),
            outcome,
            summary,
            ProductSource::backed(self._allowance.products.clone(), backing),
            &mut allowance,
        )?;
        product.publish(self.store.as_ref()).ok()
    }
    /// Captured rows and actual settled metadata owned by this producer view.
    /// Provider content bytes and active decode scratch have separate owners.
    pub fn retained_bytes(&self) -> u64 {
        // Runtime eviction drops its final Summary alias after the factory
        // returns. Collect dictionary rows that became unused on that drop
        // before reporting the retained live set.
        self.reclaim_reads();
        let remaining = self.retention.lock().unwrap_or_else(|e| e.into_inner());
        self._allowance.admitted - *remaining
    }

    /// The settled proof of the private module declaring `definition`,
    /// demanded again through the ordinary producer if it was evicted.
    pub fn module_proof(
        &self,
        definition: &DefPath,
    ) -> Option<Result<header_products::ModuleProof, QueryError>> {
        self.namespace
            .snapshot
            .header_products()?
            .module_proof(DefId::intern(definition))
            .transpose()
    }
    /// Whether that proof is currently retained in the admitted cache.
    pub fn module_proof_resident(&self, definition: &DefPath) -> bool {
        self.namespace
            .snapshot
            .header_products()
            .is_some_and(|headers| headers.module_proof_resident(DefId::intern(definition)))
    }

    /// How many times each private module's proof was inferred in this
    /// front, evictions included, by a representative definition.
    pub fn module_inferences(&self) -> Vec<(DefPath, u64)> {
        self.namespace
            .snapshot
            .header_products()
            .map_or_else(Vec::new, |headers| headers.module_inferences())
    }
    /// Whether `definition`'s settled header row is retained in the admitted cache.
    pub fn header_resident(&self, definition: &DefPath) -> bool {
        self.namespace
            .snapshot
            .header_products()
            .is_some_and(|headers| headers.ready(DefId::intern(definition)).is_some())
    }

    /// Immutable producer grant, including input rows and retained outcomes.
    pub fn admitted_bytes(&self) -> u64 {
        self._allowance.admitted
    }

    pub fn store_adoptions(&self) -> u64 {
        self.store_adoptions.load(Relaxed)
    }
    /// Cumulative refusals, as `MemoStats` invalidation counters, and raw memo
    /// operations. Each refused entry is one memo identity.
    pub fn refusal_stats(&self) -> (crate::memo::MemoStats, u64) {
        let mut stats = crate::memo::MemoStats::default();
        for (total, count) in stats.invalidated.iter_mut().zip(&self.refusals) {
            *total = count.load(Relaxed);
        }
        stats.invalidated_text = self.text_refusals.load(Relaxed);
        (stats, self.memo_operations.load(Relaxed))
    }
    /// Successful lookup targets are semantic closure edges even when a
    /// primitive/alias adds no normalized signature fingerprint dependency.
    pub(crate) fn lookup_definition(
        &self,
        key: hirsel_query_products::reads::LookupKey,
    ) -> Option<DefPath> {
        Self::lookup_in(&self.namespace.snapshot, &self.declarations, key)
    }
    pub(crate) fn lookup_in(
        snapshot: &SignatureSnapshot,
        declarations: &Declarations,
        key: hirsel_query_products::reads::LookupKey,
    ) -> Option<DefPath> {
        use hirsel_query_products::reads::LookupSource;
        let context = key.context();
        let path = hirsel_syntax::Path {
            package: key.qualifier(),
            segments: key
                .segments()
                .iter()
                .map(|part| Symbol::intern(part))
                .collect(),
        };
        match key.source {
            LookupSource::Provider => snapshot.lookup(&context, &path).result.ok(),
            LookupSource::Manifest => declarations
                .index(context.package.clone())
                .lookup(&context, &path)
                .ok(),
            LookupSource::Head | LookupSource::Candidates => None,
        }
    }
    fn provisional_generated_header(&self, id: DefId) -> bool {
        self.namespace
            .bodies
            .get(&id)
            .is_some_and(|body| body.generated)
            && !self.namespace.registrations.contains_key(&id)
            && self
                .declarations
                .metadata()
                .definitions
                .get(id.path())
                .is_some_and(|metadata| !metadata.public)
    }
    pub(crate) fn observe(&self, owner: &DefPath, read: &ReadKey) -> Option<ContentHash> {
        self.observe_in(&owner.0.package, read)
    }
    fn observe_in(
        &self,
        package: &hirsel_syntax::PackageId,
        read: &ReadKey,
    ) -> Option<ContentHash> {
        if let ReadKey::Signature(id) | ReadKey::ExecutedSignature(id) = read
            && self.provisional_generated_header(*id)
        {
            return None;
        }
        use hirsel_query_products::reads::LookupSource;
        Some(match *read {
            ReadKey::PrivateSummary(id) => memos::private_summary_value(
                &self
                    .namespace
                    .snapshot
                    .signature(id)
                    .and_then(Result::ok)?
                    .metadata
                    .inferred_unique,
            ),
            ReadKey::DeclaredSignature(id) => {
                let signature = self.namespace.snapshot.signature(id).and_then(Result::ok)?;
                memos::declared_signature_value(&self.types, &signature)?
            }
            ReadKey::Signature(id) | ReadKey::ExecutedSignature(id) => memos::signature_value(
                self.namespace
                    .snapshot
                    .signature(id)
                    .and_then(Result::ok)
                    .map(|signature| signature.fingerprint),
            ),
            ReadKey::FailedSignature(id) => {
                if self
                    .namespace
                    .snapshot
                    .signature(id)
                    .is_some_and(|result| result.is_ok())
                {
                    ContentHash::of_parts("hirsel.read.signature-ok", std::iter::empty::<&[u8]>())
                } else {
                    self.namespace.snapshot.failed_signature_receipt(id)
                }
            }
            ReadKey::Lookup(key) => {
                let context = key.context();
                let qualifier = key.qualifier();
                let segments = key.segments();
                let path = hirsel_syntax::Path {
                    package: qualifier.clone(),
                    segments: segments.iter().map(|part| Symbol::intern(part)).collect(),
                };
                let index = self.declarations.index(context.package.clone());
                match key.source {
                    LookupSource::Provider => memos::provider_value(
                        &self.namespace.snapshot.lookup(&context, &path).result,
                    ),
                    LookupSource::Manifest => {
                        // As the resolver recorded it: a name the manifest index
                        // misses (a prelude name) is the provider's, if visible.
                        let mut result = index.lookup(&context, &path);
                        if let Err(
                            hirsel_resolve::LookupError::Missing
                            | hirsel_resolve::LookupError::MissingMember(_),
                        ) = &result
                        {
                            match self.namespace.snapshot.lookup(&context, &path).result {
                                Ok(found) => {
                                    if !matches!(&result, Err(hirsel_resolve::LookupError::MissingMember(candidates)) if !candidates.contains(&found))
                                    {
                                        result = index.access(&context, &found);
                                    }
                                }
                                Err(QueryError::Diagnostics(ds)) if !ds.is_empty() => {
                                    result = Err(hirsel_resolve::LookupError::Provider(ds));
                                }
                                _ => {}
                            }
                        }
                        let namespace = result
                            .as_ref()
                            .ok()
                            .and_then(|path| index.entry(path))
                            .map(|entry| entry.namespace);
                        memos::manifest_value(&result, namespace)
                    }
                    LookupSource::Candidates => {
                        index.lookup_candidates_revision(&context, qualifier.as_ref(), &segments)
                    }
                    LookupSource::Head => ContentHash::of_parts(
                        "hirsel.read.head",
                        [&[u8::from(index.has_head(&context, segments.first()?))][..]],
                    ),
                }
            }
            ReadKey::Representation(id) => self
                .namespace
                .snapshot
                .representation(id)
                .and_then(Result::ok)
                .map_or_else(ContentHash::default, |(_, digest)| digest.0),
            ReadKey::CitedText(id) => self.citations.cited_text(id),
            ReadKey::DeclarationCandidates(id) => self
                .namespace
                .snapshot
                .declaration_candidates(id)
                .or_else(|| self.candidates.get(&id).copied())
                .unwrap_or_else(memos::no_candidates),
            ReadKey::Environment => self.environment,
            // A refused V universe observes nothing: the read does not hold.
            ReadKey::ValidationView(id) => self
                .namespace
                .snapshot
                .validation(package)
                .ok()?
                .view(id)
                .ok()?
                .map_or_else(ContentHash::default, |input| input.digest().0),
            ReadKey::ValidationUniverse(package) => self
                .namespace
                .snapshot
                .validation_input_digest(&package.id())
                .ok()?,
            ReadKey::UniverseErrors => {
                self.namespace
                    .snapshot
                    .validation(package)
                    .ok()?
                    .universe_errors()
                    .0
            }
            ReadKey::HeapRole => self
                .namespace
                .snapshot
                .validation(package)
                .ok()?
                .heap_observation()
                .ok()?
                .fingerprint(),
            // A front product must not execute a dependency to decide a hit.
            // Unknown source/registration facts safely miss this ordinary codec.
            ReadKey::NameSet(package, module) => self.declarations.name_set(package, module),
            ReadKey::ExecutedBody(_) | ReadKey::LayoutBody(_) => {
                return None;
            }
        })
    }
    /// Memo rows adopted through unchanged input subscriptions.
    pub fn clean_memos(&self) -> BTreeSet<crate::memo::MemoKey> {
        self.clean_memos
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Actual completed phase reads. This inspection executes no query.
    pub fn memo_keys(&self) -> Vec<crate::memo::MemoKey> {
        let mut keys = BTreeSet::new();
        for slot in self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            match slot {
                FrontSlot::Archived(archived) => {
                    keys.extend(archived.observations.rows.keys());
                }
                FrontSlot::Live {
                    outcome,
                    observations,
                } => {
                    if let Some(observations) = observations {
                        keys.extend(observations.rows.keys());
                    }
                    keys.extend(outcome.memos.iter().map(|(key, _)| *key));
                }
            }
        }
        keys.into_iter().collect()
    }
    pub fn memo_snapshot(&self) -> Option<Vec<(crate::memo::MemoKey, Arc<Reads>)>> {
        let mut rows = BTreeMap::new();
        for slot in self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            match slot {
                FrontSlot::Archived(archived) => {
                    rows.extend(archived.observations.load(&*self.store)?);
                }
                FrontSlot::Live {
                    outcome,
                    observations,
                } => {
                    if let Some(observations) = observations {
                        rows.extend(observations.load(&*self.store)?);
                    }
                    rows.extend(outcome.memos.iter().cloned());
                }
            }
        }
        Some(rows.into_iter().collect())
    }
    pub fn work_snapshot(&self) -> Vec<(DefPath, QueryKind, u64)> {
        let mut work = self
            .work
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|((_, kind), _)| *kind != QueryKind::PrivateInference)
            .map(|((path, kind), count)| (path.path().clone(), *kind, *count))
            .collect::<Vec<_>>();
        if let Some(headers) = self.namespace.snapshot.header_products() {
            work.extend(headers.work_snapshot());
        }
        work.extend(self.namespace.operation.private_work_snapshot());
        work
    }
    pub fn recompute_count(&self, definition: &DefPath, kind: QueryKind) -> u64 {
        self.work_snapshot()
            .into_iter()
            .filter(|(path, selected, _)| path == definition && *selected == kind)
            .map(|(_, _, count)| count)
            .sum()
    }
    /// Discovery consumes only an actual settled product from this admitted view.
    /// Check own text separately: the read set need not contain its owner's body.
    pub(crate) fn instance_discovery_summary(
        self: &Arc<Self>,
        engine: &mut QueryEngine,
        path: &DefPath,
        scratch: u64,
    ) -> Result<Option<FrontCheckedOutput>, QueryError> {
        let Some(input) = self
            .inputs
            .values()
            .find(|input| input.definition.path() == path)
        else {
            return Ok(None);
        };
        let query = self.query(path).ok_or(QueryError::NotImplemented)?;
        let archived = matches!(
            self.slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&query.address.0),
            Some(FrontSlot::Archived(_))
        );
        if self.checked_definition(path).is_none() && archived {
            let mut context = DetachedFrontContext {
                queries: self,
                scratch,
            };
            self.admit_input_slot(&mut context, input.definition)
                .ok_or_else(|| {
                    self.runtime_error(self.errors.resource)
                        .expect("resource error is local")
                })?;
            let mut task = BodyTask {
                queries: self.clone(),
                query,
                input: query.address.0,
                materialize: false,
                weight: MemoryWeight {
                    scratch: scratch.min(input.weight.scratch),
                    ..input.weight
                },
                terminal: None,
                demand: None,
                slot_admitted: true,
            };
            let restored = task.load_evicted(&mut context).unwrap_or_else(|| {
                task.run(&mut context).map_err(|error| match error {
                    BodyRunError::Refused(reference) => reference,
                    BodyRunError::Demand(_) => self.protocol_refusal(),
                })
            });
            if let Err(reference) = restored {
                return Err(self
                    .outcome(Err(reference))
                    .and_then(|o| o.outcome.result.err())
                    .unwrap_or_else(|| {
                        QueryError::Operational(hirsel_types::OperationalFailure {
                            operation: "generated instance discovery".into(),
                            message: format!(
                                "Front restoration refused: {:?}; retained {}/{} bytes",
                                self.memory_refusal(),
                                self.retained_bytes(),
                                self.admitted_bytes()
                            ),
                        })
                    }));
            }
        }
        let Some(output) = self.checked_definition(path) else {
            return Ok(None);
        };
        if !Arc::ptr_eq(&self.types, &engine.types)
            || engine.key_text(path, QueryKind::Modes)? != input.text
            || output
                .reads
                .first_changed(|key| engine.observe(path, key))
                .is_some()
        {
            return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "generated instance discovery".into(),
                message: format!("stale front summary for {path}"),
            }));
        }
        if let Some(accepted) = &input.registration {
            accepted.validate(engine)?;
        }
        Ok(Some(output))
    }
    /// Actual settled checked summary, including after a later Modes failure.
    pub fn checked_definition(self: &Arc<Self>, path: &DefPath) -> Option<FrontCheckedOutput> {
        let query = self.query(path)?;
        let selected = self.completed_product(query)?;
        self.checked(selected)
    }
    fn selection_key(
        &self,
        selected: &Result<ProductRef, Reference>,
        slots: &BTreeMap<Key, FrontSlot>,
    ) -> Option<Key> {
        match selected {
            Err(reference) => slots.iter().find_map(|(key, slot)| match slot {
                FrontSlot::Archived(_) => None,
                FrontSlot::Live { outcome, .. } => match &outcome.state {
                    FrontState::Failed { record, .. } if record == reference => Some(*key),
                    FrontState::Failed { .. } | FrontState::Settled { .. } => None,
                },
            }),
            Ok(product) => {
                let key = if self.inputs.contains_key(&product.address.0) {
                    product.address.0
                } else {
                    self.inputs.demand(&product.address.0)?
                };
                let outcome = match slots.get(&key)? {
                    FrontSlot::Archived(_) => return None,
                    FrontSlot::Live { outcome, .. } => outcome,
                };
                match &outcome.state {
                    FrontState::Failed { .. } => None,
                    FrontState::Settled { publication, .. } => {
                        if publication == product {
                            return Some(key);
                        }
                        let analyzed =
                            product.live_value::<crate::products::LiveAnalyzedProduct>()?;
                        (product.action == publication.action
                            && analyzed.address() == product.address
                            && analyzed.action() == product.action
                            && analyzed.summary().same_owner(
                                publication.live_value::<crate::products::LiveSummaryProduct>()?,
                            ))
                        .then_some(key)
                    }
                }
            }
        }
    }
    pub fn checked(
        self: &Arc<Self>,
        selected: Result<ProductRef, Reference>,
    ) -> Option<FrontCheckedOutput> {
        if let Some(row) = self.delegated(&selected) {
            return row.owner.checked(row.selected);
        }

        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let key = self.selection_key(&selected, &slots)?;
        let outcome = match slots.get(&key)? {
            FrontSlot::Archived(_) => return None,
            FrontSlot::Live { outcome, .. } => outcome,
        };
        let (summary, reads, dependencies) = outcome.checked.as_ref()?;
        Some(FrontCheckedOutput {
            table_keys: outcome.table_keys.as_ref()?.clone(),
            summary: summary.clone(),
            dependencies: dependencies.clone(),
            reads: reads.clone(),
            _facts: outcome.facts.clone(),
        })
    }

    pub fn query(&self, definition: &DefPath) -> Option<QueryId> {
        let address = self.inputs.address(DefId::get(definition)?, false)?;
        Some(QueryId {
            kind: FRONT_BODY_KIND,
            address: hirsel_artifact_store::Address(address),
        })
    }
    /// A separately admitted body materialization after the compact P settles.
    /// This address is demanded only when a body consumer lost its archive.
    pub fn body_query(&self, definition: &DefPath) -> Option<QueryId> {
        Some(QueryId {
            kind: FRONT_BODY_KIND,
            address: hirsel_artifact_store::Address(
                self.inputs.address(DefId::get(definition)?, true)?,
            ),
        })
    }
    /// Materialize through the compact Check latch in the consumer's admitted
    /// continuation, without allocating another scheduler node.
    pub(crate) fn body_task(self: &Arc<Self>, definition: &DefPath) -> Option<Box<dyn QueryTask>> {
        let query = self.query(definition)?;
        Some(Box::new(BodyTask {
            queries: self.clone(),
            query,
            input: query.address.0,
            materialize: true,
            weight: self.inputs.get(&query.address.0)?.weight,
            terminal: None,
            demand: None,
            slot_admitted: false,
        }))
    }
    pub fn factory(self: &Arc<Self>) -> Arc<dyn QueryFactory> {
        Arc::new(Factory(self.clone()))
    }
    /// Encode an exact checked observation once while its producer is live.
    /// The receipt owns no read arrays, and its inline cell is charged in the
    /// existing FrontFacts allocation. Other stores and views encode normally.
    pub(in crate::queries) fn reader_receipt(
        &self,
        query: QueryId,
        reads: &Reads,
        store: &Arc<dyn ResultStore>,
        scratch: u64,
    ) -> Option<crate::queries::reader_receipts::ReaderReceipt> {
        use crate::queries::reader_receipts::ReaderReceipt;
        let key = &query.address.0;
        let facts = if query.kind == FRONT_BODY_KIND
            && Arc::ptr_eq(&self.store, store)
            && !self
                .forwarded
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(key)
        {
            let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            match slots.get(key) {
                Some(FrontSlot::Live { outcome, .. }) => Some(outcome.facts.clone()),
                _ => None,
            }
        } else {
            None
        };
        let cache = facts.as_ref().and_then(|facts| {
            let expected = &facts.checked.as_ref()?.1;
            (std::ptr::eq(reads, &**expected)
                || reads == &**expected && reads.observed().eq(expected.observed()))
            .then_some(&facts.checked_reader)
        });
        if let Some(cache) = cache {
            if let Some((owner, receipt)) = cache.get()
                && Arc::ptr_eq(owner, store)
            {
                return Some(receipt.clone());
            }
            let receipt = ReaderReceipt::capture(&**store, reads, scratch)?;
            let _ = cache.set((store.clone(), receipt.clone()));
            return Some(receipt);
        }
        ReaderReceipt::capture(&**store, reads, scratch)
    }
    /// Inspect compact reads without decoding the checked body.
    pub(crate) fn selected_reads(
        &self,
        selected: Result<ProductRef, Reference>,
    ) -> Option<Arc<Reads>> {
        if let Some(row) = self.delegated(&selected) {
            return row.owner.selected_reads(row.selected);
        }

        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let key = self.selection_key(&selected, &slots)?;
        match slots.get(&key)? {
            FrontSlot::Archived(_) => None,
            FrontSlot::Live { outcome, .. } => Some(outcome.reads.clone()),
        }
    }
    /// Compact dependency classification for Check's speculative window.
    pub(in crate::queries) fn lowering_inputs(
        &self,
        path: &DefPath,
    ) -> Option<lowering::LoweringUse> {
        let query = self.query(path)?;
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let FrontSlot::Live { outcome, .. } = slots.get(&query.address.0)? else {
            return None;
        };
        match &outcome.state {
            FrontState::Settled { lowering, .. } => Some(lowering.clone()),
            FrontState::Failed { .. } => None,
        }
    }
    /// Settled Check inputs own no analyzed body and never read its archive.
    pub(crate) fn check_facts(
        &self,
        selected: Result<ProductRef, Reference>,
    ) -> Option<FrontCheckFacts> {
        if let Some(row) = self.delegated(&selected) {
            return row.owner.check_facts(row.selected);
        }

        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let key = self.selection_key(&selected, &slots)?;
        let selected = match slots.get(&key)? {
            FrontSlot::Archived(_) => return None,
            FrontSlot::Live { outcome, .. } => outcome,
        };
        let (result, source_rules, lowering) = match &selected.state {
            FrontState::Failed { error, .. } => (Err((**error).clone()), false, None),
            FrontState::Settled {
                publication,
                diagnostics,
                lowering,
                ..
            } => (
                Ok(diagnostics.clone()),
                publication
                    .live_value::<crate::products::LiveSummaryProduct>()
                    .is_some_and(|summary| {
                        summary
                            .value()
                            .summary
                            .calls
                            .iter()
                            .any(|call| call.callee.path().is_core("region.with"))
                    }),
                Some(lowering.clone()),
            ),
        };
        // A rejected front result cannot certify a later unavailable stage.
        // An accepted stored result already completed those stages, even when
        // this host has no adapters with which to recompute them.
        let incomplete = matches!(&result, Err(QueryError::Diagnostics(_)))
            .then(|| {
                [
                    (QueryKind::Resolve, self.resolver.is_some()),
                    (QueryKind::Check, self.checker.is_some()),
                    (QueryKind::Modes, self.modes.is_some()),
                ]
                .into_iter()
                .find_map(|(phase, available)| (!available).then_some(phase))
            })
            .flatten();
        Some(FrontCheckFacts {
            result,
            phase: selected.phase,
            incomplete,
            reads: selected.reads.clone(),
            source_rules,
            lowering,
            dead_arms: selected.dead_arms.as_ref().map(LintFacts::materialize),
        })
    }
    /// Failure of an already settled body, without decoding or demanding it.
    pub(crate) fn diagnostic_sources(
        &self,
        reference: Reference,
    ) -> Option<Arc<namespace::DiagnosticSources>> {
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.values().find_map(|slot| match slot {
            FrontSlot::Live { outcome, .. } if matches!(&outcome.state, FrontState::Failed { record, .. } if *record == reference) => outcome.diagnostic_sources.clone(),
            _ => None,
        })
    }
    pub(crate) fn completed_front(
        &self,
        path: &DefPath,
    ) -> Option<Result<(), (Reference, QueryError)>> {
        Some(match self.completed_product(self.query(path)?)? {
            Ok(_) => Ok(()),
            Err(reference) => Err((reference, self.completed_error(path)?)),
        })
    }
    pub(crate) fn completed_error(&self, path: &DefPath) -> Option<QueryError> {
        let query = self.query(path)?;
        let outcomes = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let outcome = match outcomes.get(&query.address.0)? {
            FrontSlot::Archived(_) => return None,
            FrontSlot::Live { outcome, .. } => outcome,
        };
        match &outcome.state {
            FrontState::Failed { error, .. } => Some((**error).clone()),
            FrontState::Settled { diagnostics, .. } => {
                has_errors(diagnostics).then(|| QueryError::Diagnostics(diagnostics.clone()))
            }
        }
    }
    pub(crate) fn with_settled_summary<T>(
        &self,
        path: &DefPath,
        read: impl FnOnce(&crate::DefinitionSummary) -> T,
    ) -> Option<T> {
        let query = self.query(path)?;
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let outcome = match slots.get(&query.address.0)? {
            FrontSlot::Archived(_) => return None,
            FrontSlot::Live { outcome, .. } => outcome,
        };
        match &outcome.state {
            FrontState::Failed { .. } => None,
            FrontState::Settled { publication, .. } => Some(read(
                &publication
                    .live_value::<crate::products::LiveSummaryProduct>()?
                    .value()
                    .summary,
            )),
        }
    }

    /// An immutable outcome, including exact error-phase and read evidence.
    /// This lookup never adopts a product into a mutable source namespace.
    pub fn publication(&self, selected: Result<ProductRef, Reference>) -> Option<PublishedResult> {
        if let Some(row) = self.delegated(&selected) {
            return row.owner.publication(row.selected);
        }

        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let key = self.selection_key(&selected, &slots)?;
        let outcome = match slots.get(&key)? {
            FrontSlot::Archived(_) => return None,
            FrontSlot::Live { outcome, .. } => outcome,
        };
        let result = match &outcome.state {
            FrontState::Failed { error, .. } => Err((**error).clone()),
            FrontState::Settled {
                publication,
                diagnostics,
                ..
            } => Ok(PublishedDefinition {
                summary: publication
                    .live_value::<crate::products::LiveSummaryProduct>()?
                    .value_arc(),
                diagnostics: diagnostics.clone(),
            }),
        };
        Some(PublishedResult {
            definition: outcome.definition.path().clone(),
            result,
            phase: outcome.phase,
            reads: outcome.reads.clone(),
            diagnostic_phases: outcome.diagnostic_phases.clone(),
        })
    }
    pub fn outcome(
        self: &Arc<Self>,
        selected: Result<ProductRef, Reference>,
    ) -> Option<FrontBodyOutput> {
        if let Some(row) = self.delegated(&selected) {
            return row.owner.outcome(row.selected);
        }

        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let key = self.selection_key(&selected, &slots)?;
        let outcome = match slots.get_mut(&key)? {
            FrontSlot::Archived(_) => return None,
            FrontSlot::Live { outcome, .. } => outcome,
        };
        let (result, summary, body_owner) = match &outcome.state {
            FrontState::Failed { error, .. } => (Err((**error).clone()), None, None),
            FrontState::Settled {
                publication, body, ..
            } => {
                let owner = selected
                    .as_ref()
                    .ok()?
                    .live_value::<crate::products::LiveAnalyzedProduct>()
                    .cloned()
                    .or_else(|| body.as_ref()?.upgrade())?;
                (
                    Ok(owner.output().clone()),
                    Some(
                        publication
                            .live_value::<crate::products::LiveSummaryProduct>()?
                            .value_arc(),
                    ),
                    Some(owner),
                )
            }
        };
        outcome.pending_body.take();
        Some(FrontBodyOutput {
            outcome: ModeResult {
                definition: outcome.definition.path().clone(),
                result,
                phase: outcome.phase,
                summary,
                reads: outcome.reads.clone(),
                diagnostic_phases: outcome.diagnostic_phases.clone(),
            },
            _facts: outcome.facts.clone(),
            _body_owner: body_owner,
        })
    }
    pub(crate) fn registration_handle(
        &self,
        path: &DefPath,
    ) -> Option<&Arc<AcceptedFrontRegistration>> {
        self.inputs
            .definition(DefId::get(path)?)?
            .registration
            .as_ref()
    }

    /// Immutable ordinary input transport. Decoding starts only on demand;
    /// this performs no compiler phase or namespace publication.
    pub(crate) fn native_input(
        &self,
        path: &DefPath,
    ) -> Option<(
        Arc<ItemSyntax>,
        Arc<QueryOutput<hirsel_types::SignatureRef>>,
    )> {
        let input = self
            .inputs
            .values()
            .find(|input| input.definition.path() == path)?;
        Some((self.syntax(input).ok()?, self.signature(input).ok()?))
    }

    /// Distinct decoded bodies held for demand or by active output consumers.
    /// Settled archives and compact checked summaries contain no body owner.
    pub fn live_body_count(&self) -> usize {
        self.slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|slot| match slot {
                FrontSlot::Archived(_) => false,
                FrontSlot::Live { outcome, .. } => match &outcome.state {
                    FrontState::Failed { .. } => false,
                    FrontState::Settled { body, .. } => body
                        .as_ref()
                        .and_then(WeakAnalyzedProduct::upgrade)
                        .is_some(),
                },
            })
            .count()
    }
    fn evict_pending_body(&self) -> bool {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let retired = slots.values_mut().find_map(|slot| match slot {
            FrontSlot::Live { outcome, .. }
                if matches!(
                    outcome.state,
                    FrontState::Settled {
                        archive: Some(_),
                        ..
                    }
                ) && outcome
                    .pending_body
                    .as_ref()
                    .is_some_and(|body| body.product.cache_only()) =>
            {
                outcome.pending_body.take()
            }
            _ => None,
        });
        drop(slots);
        let removed = retired.is_some();
        drop(retired);
        removed
    }
    /// Discard decoded bodies only when their request checkpoint survives.
    pub(crate) fn release_pending_bodies(&self) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        for slot in slots.values_mut() {
            if let FrontSlot::Live { outcome, .. } = slot
                && matches!(
                    outcome.state,
                    FrontState::Settled {
                        archive: Some(_),
                        ..
                    }
                )
            {
                outcome.pending_body.take();
            }
        }
    }
    #[cfg(test)]
    pub(super) fn outcomes_for_test(&self) -> Vec<Reference> {
        self.slots
            .lock()
            .unwrap()
            .values()
            .filter_map(|slot| match slot {
                FrontSlot::Archived(_) => None,
                FrontSlot::Live { outcome, .. } => match &outcome.state {
                    FrontState::Failed { record, .. } => Some(*record),
                    FrontState::Settled { archive, .. } => *archive,
                },
            })
            .collect()
    }
    pub(crate) fn registration(&self, path: &DefPath) -> Option<&AcceptedFrontRegistration> {
        self.registration_handle(path).map(Arc::as_ref)
    }
    fn worker(&self) -> Worker<'_> {
        Worker {
            owners: (*self.citations.owners).clone(),
            types: &self.types,
            memo: &self.memo,
            snapshot: &self.namespace.snapshot,
            declarations: &self.declarations,
            citations: &self.citations,
            sources: &self.citations.sources,
            items: &self.citations.items,
            resolver: self.resolver.as_deref(),
            checker: self.checker.as_deref(),
            modes: self.modes.as_deref(),
        }
    }
}
struct DetachedFrontContext<'a> {
    queries: &'a FrontBodyQueries,
    scratch: u64,
}
impl QueryContext for DetachedFrontContext<'_> {
    fn scratch_bytes(&self) -> u64 {
        self.scratch
    }
    fn evict_body(&mut self) -> bool {
        self.queries.evict_idle(self.scratch)
    }
    fn read(&mut self, _: QueryId) -> std::task::Poll<Result<ProductRef, Reference>> {
        std::task::Poll::Ready(Err(self.queries.protocol_refusal()))
    }
}
struct Factory(Arc<FrontBodyQueries>);
impl QueryFactory for Factory {
    fn evict_cache(&self, _scratch: u64) -> bool {
        self.0.evict_pending_body()
            || self
                .0
                .namespace
                .snapshot
                .header_products()
                .is_some_and(|headers| headers.evict())
            || self.0.namespace.snapshot.evict_cached()
            || self.0.inputs.clear_addresses()
            || self.0.trim_slot_admission()
            || self.0.reclaim_reads()
    }
    fn evict(&self, query: QueryId, product: &ProductRef, scratch: u64) -> bool {
        if !self.0.has_cached_query(query)
            && let Some(front) = self
                .0
                .demand_producer()
                .and_then(|demand| demand.front_for_eviction(query))
        {
            return front.factory().evict(query, product, scratch);
        }
        let mut slots = self.0.slots.lock().unwrap_or_else(|e| e.into_inner());
        let (outcome, observations) = match slots.get_mut(&query.address.0) {
            Some(FrontSlot::Archived(_)) | None => return false,
            Some(FrontSlot::Live {
                outcome,
                observations,
            }) => (outcome, observations),
        };
        let pending = outcome.pending_body.as_ref();
        let (publication, can_evict) = match &outcome.state {
            FrontState::Failed { .. } => (None, false),
            FrontState::Settled { publication, .. } => (
                Some(publication),
                publication
                    .live_value::<crate::products::LiveSummaryProduct>()
                    .is_some_and(|summary| {
                        publication.live_owners() == 2
                            && pending.is_none_or(|body| body.product.cache_only())
                            && summary.cache_owners(1 + usize::from(pending.is_some()))
                    })
                    && Arc::strong_count(&outcome.facts) == 2,
            ),
        };
        if !can_evict || publication != Some(product) {
            return false;
        }
        let Some(archived) =
            self.0
                .archive_facts(query.address.0, outcome, observations.clone(), scratch)
        else {
            return false;
        };
        slots.insert(query.address.0, FrontSlot::Archived(archived));
        drop(slots);
        self.0.reclaim_reads();
        true
    }

    fn create(&self, query: QueryId) -> Result<Box<dyn QueryTask>, Reference> {
        if !self.0.owns_query(query)
            && let Some(front) = self
                .0
                .demand_producer()
                .and_then(|demand| demand.front_for(query))
        {
            return front.factory().create(query);
        }
        if query.kind != FRONT_BODY_KIND {
            return Err(self
                .0
                .protocol_failure(query, "Front factory received another query kind"));
        }
        let (key, input, materialize) = if let Some(input) = self.0.inputs.get(&query.address.0) {
            (query.address.0, input, false)
        } else {
            let key = self.0.inputs.demand(&query.address.0).ok_or_else(|| {
                self.0.protocol_failure(
                    query,
                    "Front input is unavailable in the registered immutable views",
                )
            })?;
            (key, &self.0.inputs[&key], true)
        };
        Ok(Box::new(BodyTask {
            queries: self.0.clone(),
            query,
            input: key,
            materialize,
            weight: input.weight,
            terminal: None,
            demand: None,
            slot_admitted: false,
        }))
    }
}
struct BodyTask {
    queries: Arc<FrontBodyQueries>,
    query: QueryId,
    input: Key,
    materialize: bool,
    weight: MemoryWeight,
    terminal: Option<Result<ProductRef, Reference>>,
    demand: Option<DefId>,
    slot_admitted: bool,
}
impl Drop for BodyTask {
    fn drop(&mut self) {
        if self.slot_admitted {
            self.queries
                .release_pending_slot(self.queries.inputs[&self.input].definition);
        }
    }
}
enum BodyRunError {
    Refused(Reference),
    Demand(DefId),
}
impl From<Reference> for BodyRunError {
    fn from(reference: Reference) -> Self {
        Self::Refused(reference)
    }
}
impl QueryTask for BodyTask {
    fn id(&self) -> QueryId {
        self.query
    }
    fn weight(&self) -> MemoryWeight {
        MemoryWeight {
            retained: (std::mem::size_of::<Self>() + std::mem::size_of::<ProductRef>()) as u64,
            scratch: self.weight.scratch,
            stack: self.weight.stack,
        }
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        if let Some(result) = self.terminal.clone() {
            return finish(result);
        }
        let definition = self.queries.inputs[&self.input].definition;
        if !self.slot_admitted {
            if self.queries.admit_input_slot(cx, definition).is_none() {
                return finish(Err(self.queries.resource_refusal()));
            }
            self.slot_admitted = true;
        }
        let pending = self
            .queries
            .namespace
            .bodies
            .get(&definition)
            .is_some_and(|body| body.generated)
            && self.queries.inputs[&self.input].registration.is_none();
        if pending {
            self.demand = Some(definition);
        }
        if let Some(dependency) = self.demand {
            let Some(producer) = self.queries.demand_producer() else {
                if self.queries.namespace.consumers_admitted() {
                    self.queries.require_discovery(dependency.path());
                    let result = self.publish_generation_failure(
                        cx,
                        QueryError::Operational(hirsel_types::OperationalFailure {
                            operation: "consumer discovery dependency".into(),
                            message: format!(
                                "{} requires an accepted registration",
                                dependency.path()
                            ),
                        }),
                        Arc::new(Reads::empty()),
                        None,
                    );
                    self.terminal = Some(result.clone());
                    return finish(result);
                }
                return finish(Err(self.queries.protocol_failure(self.query, &format!(
                    "generated declaration {} has no accepted registration and no live demand producer",
                    definition.path(),
                ))));
            };
            let demand = match producer.query(
                self.queries.namespace.clone(),
                dependency.path(),
                self.queries
                    .namespace
                    .syntax(dependency.path())
                    .map_or(Span::default(), |syntax| syntax.item.name_span),
            ) {
                Ok(query) => query,
                Err(_) => return finish(Err(self.queries.resource_refusal())),
            };
            let selected = match cx.read(demand) {
                std::task::Poll::Pending => return QueryPoll::Waiting(vec![demand]),
                std::task::Poll::Ready(selected) => selected,
            };
            if let Some(failure) = producer.failed(demand) {
                cx.release_body(demand);
                let result = self.publish_demand_failure(cx, failure);
                self.terminal = Some(result.clone());
                return finish(result);
            }
            let successor = match producer.completed(demand) {
                Some(Ok(successor)) => successor,
                _ => {
                    return finish(Err(selected.err().unwrap_or_else(|| {
                        self.queries.protocol_failure(
                            self.query,
                            "generation demand settled without a successor or failure",
                        )
                    })));
                }
            };
            let owner = successor.owner().body_front().clone();
            let Some(query) = (if self.materialize {
                owner.body_query(definition.path())
            } else {
                owner.query(definition.path())
            }) else {
                return finish(Err(self.queries.protocol_failure(
                    self.query,
                    &format!(
                        "accepted successor has no Front producer for {}",
                        definition.path(),
                    ),
                )));
            };
            let selected = match cx.read(query) {
                std::task::Poll::Pending => return QueryPoll::Waiting(vec![query]),
                std::task::Poll::Ready(selected) => selected,
            };
            let alias = selected.clone().map(|mut product| {
                product.address = self.query.address;
                product
            });
            self.queries
                .forwarded
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    // Summary and materialization queries must retain distinct aliases.
                    self.query.address.0,
                    Forwarded {
                        owner,
                        selected,
                        alias: alias.clone(),
                        successor,
                    },
                );
            cx.release_body(demand);
            self.terminal = Some(alias.clone());
            return finish(alias);
        }
        if self.materialize {
            let parent = QueryId {
                kind: FRONT_BODY_KIND,
                address: hirsel_artifact_store::Address(self.input),
            };
            match cx.read(parent) {
                std::task::Poll::Pending => return QueryPoll::Waiting(vec![parent]),
                std::task::Poll::Ready(Err(error)) => {
                    self.terminal = Some(Err(error));
                    return finish(Err(error));
                }
                std::task::Poll::Ready(Ok(_)) => {}
            }
        }
        let result = match self.run(cx) {
            Ok(product) => Ok(product),
            Err(BodyRunError::Refused(reference)) => Err(reference),
            Err(BodyRunError::Demand(dependency)) => {
                self.demand = Some(dependency);
                return self.poll(cx);
            }
        };
        self.terminal = Some(result.clone());
        finish(result)
    }
}
fn finish(result: Result<ProductRef, Reference>) -> QueryPoll {
    match result {
        Ok(product) => QueryPoll::Complete(product),
        Err(error) => QueryPoll::Failed(error),
    }
}
impl BodyTask {
    fn publish_demand_failure(
        &self,
        cx: &mut dyn QueryContext,
        failure: generation::demand::DemandFailure,
    ) -> Result<ProductRef, Reference> {
        self.publish_generation_failure(cx, failure.error, failure.reads, Some(failure.sources))
    }
    fn publish_generation_failure(
        &self,
        cx: &mut dyn QueryContext,
        error: QueryError,
        reads: Arc<Reads>,
        diagnostic_sources: Option<Arc<namespace::DiagnosticSources>>,
    ) -> Result<ProductRef, Reference> {
        let queries = &self.queries;
        let input = &queries.inputs[&self.input];
        let phase = Some(QueryKind::Generate);
        let outcome = ModeResult {
            definition: input.definition.path().clone(),
            result: Err(error.clone()),
            phase,
            summary: None,
            reads: reads.clone(),
            diagnostic_phases: vec![],
        };
        let bytes = queries
            .records
            .encode_record(
                &(
                    input.definition,
                    Some(&error),
                    phase.map(QueryKind::phase_name),
                    Vec::<(Diagnostic, String)>::new(),
                    &*reads,
                ),
                |anchor| {
                    crate::NativeRecordSource::record_owner(&*queries.namespace.anchors, anchor)
                        .or_else(|| {
                            diagnostic_sources
                                .as_ref()
                                .and_then(|sources| sources.owner(anchor))
                        })
                },
            )
            .ok_or_else(|| queries.resource_refusal())?;
        if bytes.len() as u64 > self.weight.scratch {
            return Err(queries.resource_refusal());
        }
        let reference = queries
            .store
            .put_content(FAILURE, &bytes)
            .ok_or_else(|| queries.resource_refusal())?;
        let action = hirsel_engine_store::query_action(&input.key, input.key.1, &reads);
        let mut cached = queries
            .publish_facts(
                cx,
                self.input,
                Some(reference),
                action,
                outcome,
                None,
                None,
                None,
                vec![],
            )
            .ok_or_else(|| queries.resource_refusal())?;
        Arc::get_mut(&mut cached.facts)
            .ok_or_else(|| queries.protocol_refusal())?
            .diagnostic_sources = diagnostic_sources;
        queries.install_outcome(self.input, cached, self.weight.scratch);
        Err(reference)
    }
    fn publish_body(
        &self,
        cx: &mut dyn QueryContext,
        mut outcome: ModeResult,
        bytes: Option<&[u8]>,
    ) -> Result<ProductRef, Reference> {
        let queries = &self.queries;
        let input = &queries.inputs[&self.input];
        let (mut reference, reads, summary) = {
            let slots = queries.slots.lock().unwrap_or_else(|e| e.into_inner());
            let old = match slots
                .get(&self.input)
                .ok_or_else(|| queries.protocol_refusal())?
            {
                FrontSlot::Archived(_) => return Err(queries.protocol_refusal()),
                FrontSlot::Live { outcome, .. } => outcome,
            };
            let (reference, publication, diagnostics) = match &old.state {
                FrontState::Failed { .. } => return Err(queries.protocol_refusal()),
                FrontState::Settled {
                    archive,
                    publication,
                    diagnostics,
                    ..
                } => (*archive, publication, diagnostics),
            };
            if outcome.result.is_err() || old.reads.as_ref() != outcome.reads.as_ref() {
                return Err(queries.protocol_refusal());
            }
            let summary = publication
                .live_value::<crate::products::LiveSummaryProduct>()
                .ok_or_else(|| queries.protocol_refusal())?
                .clone();
            let old_summary = summary.value();
            let current_summary = outcome
                .summary
                .as_ref()
                .ok_or_else(|| queries.protocol_refusal())?;
            if !crate::SummaryView::from(old_summary).equivalent(
                &queries.types,
                crate::SummaryView::from(&**current_summary),
                &queries.types,
            ) || old.diagnostic_phases != outcome.diagnostic_phases
                || diagnostics.as_slice()
                    != outcome
                        .result
                        .as_ref()
                        .map_err(|_| queries.protocol_refusal())?
                        .diagnostics
                        .as_slice()
            {
                return Err(queries.protocol_refusal());
            }
            outcome.phase = old.phase;
            (reference, old.reads.clone(), summary)
        };
        if let Some(bytes) = bytes {
            let domain = hirsel_engine_store::product_domain(QueryKind::Modes);
            let encoded = domain
                .reference(bytes)
                .ok_or_else(|| queries.protocol_refusal())?;
            if reference.is_some_and(|previous| previous != encoded) {
                return Err(queries.protocol_refusal());
            }
            reference = Some(
                queries
                    .store
                    .put_content(domain, bytes)
                    .ok_or_else(|| queries.resource_refusal())?,
            );
        }
        let action = hirsel_engine_store::query_action(&input.key, input.key.1, &reads);
        let product = queries
            .admit_body(cx, self.query.address, action, outcome, summary)
            .ok_or_else(|| queries.resource_refusal())?;
        let body = product
            .live_value::<crate::products::LiveAnalyzedProduct>()
            .ok_or_else(|| queries.protocol_refusal())?;
        let mut slots = queries.slots.lock().unwrap_or_else(|e| e.into_inner());
        let current = match slots
            .get_mut(&self.input)
            .ok_or_else(|| queries.protocol_refusal())?
        {
            FrontSlot::Archived(_) => return Err(queries.protocol_refusal()),
            FrontSlot::Live { outcome, .. } => outcome,
        };
        match &mut current.state {
            FrontState::Failed { .. } => return Err(queries.protocol_refusal()),
            FrontState::Settled {
                archive,
                body: owner,
                ..
            } => {
                let changed = *archive != reference;
                *archive = reference;
                *owner = Some(body.downgrade());
                if changed {
                    current.terminal = None;
                    queries.store_compact(self.input, current, self.weight.scratch);
                }
            }
        }
        drop(slots);
        if let Some(reference) = reference {
            queries.store.associate_written_product(
                hirsel_artifact_store::Address(input.key.0),
                action,
                reference,
            );
        }
        Ok(product)
    }

    fn restore_body(&self, cx: &mut dyn QueryContext) -> Option<Result<ProductRef, Reference>> {
        let queries = &self.queries;
        let input = &queries.inputs[&self.input];
        let (reference, reads) = {
            let slots = queries.slots.lock().unwrap_or_else(|e| e.into_inner());
            let old = match slots.get(&self.input)? {
                FrontSlot::Archived(_) => return None,
                FrontSlot::Live { outcome, .. } => outcome,
            };
            match &old.state {
                FrontState::Failed { .. } => return None,
                FrontState::Settled { archive, .. } => {
                    (archive.as_ref().copied()?, old.reads.clone())
                }
            }
        };
        if reference.domain != hirsel_engine_store::product_domain(QueryKind::Modes)
            || reference.len > self.weight.scratch
        {
            return None;
        }
        let bytes = queries.store.get_content(reference)?;
        if reference.domain.reference(&bytes) != Some(reference) {
            return None;
        }
        let signature = queries
            .namespace
            .snapshot
            .signature(input.definition)
            .and_then(Result::ok);
        let restored = persistent::restore_frozen_mode(
            &queries.records,
            &queries.types,
            input.definition,
            queries.syntax(input).ok()?,
            &queries.citations,
            input.key.1,
            &bytes,
            reads,
            signature.as_deref(),
        )?;
        Some(self.publish_body(cx, restored.outcome, None))
    }

    fn load(&self, cx: &mut dyn QueryContext) -> Option<Result<ProductRef, Reference>> {
        let queries = &self.queries;
        let input = queries.inputs.get(&self.input)?;
        let mut entry = hirsel_engine_store::Entry::load(&*queries.store, &input.key);
        if entry.variants.is_empty() {
            return None;
        }
        queries.memo_operations.fetch_add(1, Relaxed);
        entry
            .variants
            .retain(|variant| variant.inputs == input.key.1 && variant.valid(&input.key));
        let Some(newest) = entry.variants.first() else {
            queries.text_refusals.fetch_add(1, Relaxed);
            return None;
        };
        let observe = |read: &ReadKey| queries.observe(input.definition.path(), read);
        let Some(variant) = entry.hit(observe) else {
            if let Some(read) = newest.reads.first_changed(observe) {
                queries.refusals[read.kind() as usize].fetch_add(1, Relaxed);
            }
            return None;
        };
        if variant.output.len > self.weight.scratch {
            return None;
        }
        let bytes = queries.store.get_content(variant.output)?;
        let signature = queries
            .namespace
            .snapshot
            .signature(input.definition)
            .and_then(Result::ok);
        let mut restored = persistent::restore_frozen_mode(
            &queries.records,
            &queries.types,
            input.definition,
            queries.syntax(input).ok()?,
            &queries.citations,
            input.key.1,
            &bytes,
            Arc::new(variant.reads.clone()),
            signature.as_deref(),
        )?;
        let definition = input.definition;
        let mut memo_rows = vec![(
            crate::memo::MemoKey::new(QueryKind::Modes, definition),
            restored.outcome.reads.clone(),
        )];
        if let Some((_, reads, _)) = &restored.checked {
            memo_rows.push((
                crate::memo::MemoKey::new(QueryKind::Check, definition),
                reads.clone(),
            ));
        }
        if let Some(summary) = &mut restored.outcome.summary {
            Arc::make_mut(summary).share_checked_storage();
        }
        let action = variant.action(&input.key);
        let cached = queries.publish_facts(
            cx,
            self.input,
            Some(variant.output),
            action,
            restored.outcome,
            restored.checked,
            restored.dead_arms,
            restored.table_keys,
            memo_rows,
        )?;
        let terminal = match &cached.state {
            FrontState::Failed { record, .. } => Err(*record),
            FrontState::Settled { publication, .. } => Ok(publication.clone()),
        };
        queries
            .memo_operations
            .fetch_add(cached.memos.len() as u64, Relaxed);
        queries.install_outcome(self.input, cached, self.weight.scratch);
        queries.store_adoptions.fetch_add(1, Relaxed);
        timings::count(Count::StoreHits);
        timings::count(Count::StoreModeHits);
        match terminal {
            Err(reference) => Some(Err(reference)),
            Ok(publication) => {
                self.remember_carry();
                queries.store.associate_written_product(
                    hirsel_artifact_store::Address(input.key.0),
                    action,
                    variant.output,
                );
                Some(Ok(publication))
            }
        }
    }
    fn run(&mut self, cx: &mut dyn QueryContext) -> Result<ProductRef, BodyRunError> {
        let _read_scope = self.queries.store.read_scope();
        let _publication = self.queries.store.publication_scope();
        if !self.materialize
            && let Some(product) = self.queries.completed_product(self.query)
        {
            return product.map_err(Into::into);
        }
        if self.materialize {
            let product = {
                let slots = self.queries.slots.lock().unwrap_or_else(|e| e.into_inner());
                match slots.get(&self.input) {
                    Some(FrontSlot::Archived(_)) | None => None,
                    Some(FrontSlot::Live { outcome, .. }) => match &outcome.state {
                        FrontState::Failed { .. } => None,
                        FrontState::Settled { body, .. } => {
                            body.as_ref().and_then(WeakAnalyzedProduct::upgrade)
                        }
                    },
                }
            };
            if let Some(product) = product
                && product.address() == self.query.address
            {
                return product
                    .publish(self.queries.store.as_ref())
                    .map_err(|_| self.queries.resource_refusal().into());
            }
        }
        // Pin V only while this synchronous producer consumes it.
        let _validation = self
            .queries
            .namespace
            .demand_validation()
            .map_err(|error| {
                crate::queries::operation_owner_trace(format_args!(
                    "OWNER RESOURCE Front validation demand: {error:?}"
                ));
                self.queries.resource_refusal()
            })?;
        if self.materialize {
            if let Some(body) = self.restore_body(cx) {
                return body.map_err(Into::into);
            }
            // A carry adopted from a prior request may have only a summary:
            // its request-private body transport has ended. This run can
            // compute its first Check, but never repeat a current Check.
            if self.queries.recompute_count(
                self.queries.inputs[&self.input].definition.path(),
                QueryKind::Check,
            ) != 0
            {
                return Err(self.queries.resource_refusal().into());
            }
        }
        if !self.materialize
            && matches!(
                self.queries
                    .slots
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&self.input),
                Some(FrontSlot::Archived(_))
            )
        {
            // A local completion is authoritative in this immutable view,
            // just like completed_product above, including under verify-cache.
            // Decode/admission failure cannot authorize a second Check.
            return self
                .load_evicted(cx)
                .ok_or_else(|| self.queries.resource_refusal())?
                .map_err(Into::into);
        }
        if !self.materialize && self.queries.verify_store == hirsel_engine_store::VerifyStore::Off {
            if let Some(hit) = self.load_carry(cx) {
                return hit.map_err(Into::into);
            }
            if let Some(hit) = self.load_compact(cx) {
                return hit.map_err(Into::into);
            }
            if let Some(hit) = self.load(cx) {
                return hit.map_err(Into::into);
            }
            timings::count(Count::StoreMisses);
            timings::count(Count::StoreModeMisses);
        }
        let queries = &self.queries;
        let input = &queries.inputs[&self.input];
        let expected = if queries.verify_store == hirsel_engine_store::VerifyStore::Recompute {
            let mut entry = hirsel_engine_store::Entry::load(&*queries.store, &input.key);
            entry.variants.retain(|variant| {
                variant.inputs == input.key.1
                    && variant.valid(&input.key)
                    && variant.output.len <= self.weight.scratch
            });
            entry
                .hit(|read| queries.observe(input.definition.path(), read))
                .and_then(|variant| {
                    queries
                        .store
                        .get_content(variant.output)
                        .map(|bytes| (variant.reads.clone(), bytes))
                })
        } else {
            None
        };
        if !self.materialize
            && queries.verify_store == hirsel_engine_store::VerifyStore::Recompute
            && expected.is_none()
        {
            timings::count(Count::StoreMisses);
            timings::count(Count::StoreModeMisses);
        }
        let worker = queries.worker();
        let syntax = loop {
            match queries.syntax(input) {
                Ok(syntax) => break syntax,
                Err(error) => {
                    // Syntax and completed Front products borrow from the same
                    // allowance. Header eviction alone cannot release those
                    // products; retry only after reclaiming an actual owner.
                    if matches!(&error, QueryError::Operational(failure)
                        if failure.operation == "source syntax admission")
                        && (cx.evict_body() || queries.evict_idle(cx.scratch_bytes()))
                    {
                        continue;
                    }
                    return Err(queries
                        .protocol_failure(
                            self.query,
                            &format!(
                                "syntax recovery failed for {}: {error:?}",
                                input.definition.path(),
                            ),
                        )
                        .into());
                }
            }
        };
        let SignatureInput {
            signature,
            reads: signature_reads,
        } = queries.signature_input(input, cx).map_err(|error| {
            if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
                crate::queries::operation_owner_trace(format_args!(
                    "OWNER RESOURCE Front signature input {}: {error:?}",
                    input.definition.path()
                ));
            }
            queries.resource_refusal()
        })?;
        let mut done = worker.run(Task {
            index: 0,
            definition: input.definition.path().clone(),
            syntax,
            signature,
            signature_reads,
            registration: input.registration.clone(),
            text: input.text,
            resolution: None,
            checked: None,
        });
        // A body computed under a refused citation read is a resource
        // refusal: no failure record is published for it, and the next
        // demand computes it again.
        if let Some(refusal) = done.refused() {
            if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
                crate::queries::operation_owner_trace(format_args!(
                    "OWNER RESOURCE Front phase {}: {refusal:?}",
                    input.definition.path()
                ));
            }
            return Err(queries.resource_refusal().into());
        }
        if let Some(registration) = &input.registration {
            registration.explain(&mut done.result, &queries.citations.sources);
            if let Some(Product::Computed(stage)) = &mut done.resolution {
                registration.explain(&mut stage.result, &queries.citations.sources);
            }
            if let Some(Product::Computed(stage)) = &mut done.checked {
                registration.explain(&mut stage.result, &queries.citations.sources);
            }
            if let Some(Product::Computed(stage)) = &mut done.modes {
                registration.explain(&mut stage.result, &queries.citations.sources);
            }
        }
        for kind in done.recomputed() {
            *queries
                .work
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry((DefId::intern(&done.definition), kind))
                .or_default() += 1;
        }
        let table_keys = done
            .checked
            .as_ref()
            .and_then(|p| p.result().ok())
            .map(|checked| Arc::new(crate::table_keys::TableKeyUse::capture(&checked.value)));
        let result = done.result();
        let mut source = FrozenReadSource {
            declarations: &queries.declarations,
            citations: &queries.citations,
            candidates: queries
                .candidates
                .get(&DefId::intern(&done.definition))
                .copied(),
            environment: queries.environment,
        };
        // A request transport preserves a released body as required content,
        // independently of whether the backend keeps optional query variants.
        let capture_body = queries.store.persistence().retains()
            || matches!(
                queries.store.capability(),
                hirsel_engine_store::ProviderCapability::RequestLocal(_)
            );
        if capture_body && !queries.store.persistence().retains() {
            queries
                .store
                .archive_encoded(hirsel_engine_store::ArchiveBoundary::RequiredSpill);
        }
        let mut completed = CompactCompleted::capture(
            done,
            &worker,
            queries.types.clone(),
            &mut source,
            capture_body.then_some(&*queries.records),
        );
        // A generated private header is provisional until its accepted body
        // supplies the module proof. Do not publish a caller under that row.
        if queries.demand_producer().is_some()
            && let Some(dependency) = completed.outcome().reads.iter().find_map(|(key, _)| {
                let (ReadKey::Signature(id) | ReadKey::ExecutedSignature(id)) = key else {
                    return None;
                };
                queries.provisional_generated_header(*id).then_some(*id)
            })
        {
            return Err(BodyRunError::Demand(dependency));
        }
        let memo_rows = completed.memo_snapshot();
        let checked = completed.checked_product();
        let dead_arms = completed.take_dead_arms();
        let published = completed.outcome().clone();
        let outcome = ModeResult {
            definition: published.definition.clone(),
            result: match &published.result {
                Err(error) => Err(error.clone()),
                Ok(_) => result,
            },
            phase: published.phase,
            summary: published.result.as_ref().ok().map(|v| v.summary.clone()),
            reads: published.reads.clone(),
            diagnostic_phases: published.diagnostic_phases.clone(),
        };
        // A front failure is an immutable diagnostic record, never a successful
        // mode output, and includes source/phase/ordered-read provenance.
        let failed = published.result.is_err();
        let bytes = if failed {
            Some(
                queries
                    .records
                    .encode_record(
                        &(
                            published.definition,
                            published.result.err(),
                            published.phase.map(QueryKind::phase_name),
                            published.diagnostic_phases,
                            &*published.reads,
                        ),
                        |anchor| queries.citations.record_owner(anchor),
                    )
                    .ok_or_else(|| queries.resource_refusal())?,
            )
        } else {
            completed.take_payload()
        };
        if bytes
            .as_ref()
            .is_some_and(|bytes| bytes.len() as u64 > self.weight.scratch)
        {
            return Err(queries.resource_refusal().into());
        }
        if self.materialize {
            return self
                .publish_body(cx, outcome, bytes.as_deref())
                .map_err(Into::into);
        }
        if let Some((reads, expected)) = expected {
            if failed || reads != *outcome.reads || bytes.as_deref() != Some(expected.as_ref()) {
                let error = QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "verify-cache".into(),
                    message: format!("{}: Modes differs from its cached result", input.definition),
                });
                let failure = queries
                    .records
                    .encode_record(
                        &(
                            input.definition.path(),
                            &error,
                            QueryKind::Modes.phase_name(),
                            &*outcome.reads,
                        ),
                        |anchor| queries.citations.record_owner(anchor),
                    )
                    .ok_or_else(|| queries.resource_refusal())?;
                let record = queries
                    .store
                    .put_content(FAILURE, &failure)
                    .ok_or_else(|| queries.resource_refusal())?;
                let output = ModeResult {
                    definition: input.definition.path().clone(),
                    result: Err(error),
                    phase: Some(QueryKind::Modes),
                    summary: None,
                    reads: outcome.reads,
                    diagnostic_phases: vec![],
                };
                let cached = queries
                    .publish_facts(
                        cx,
                        self.input,
                        Some(record),
                        hirsel_artifact_store::Action(ContentHash::default()),
                        output,
                        None,
                        None,
                        None,
                        vec![],
                    )
                    .ok_or_else(|| queries.resource_refusal())?;
                queries.install_outcome(self.input, cached, self.weight.scratch);
                return Err(record.into());
            }
        }
        let (record, action) = if failed {
            let record = queries
                .store
                .put_content(
                    FAILURE,
                    bytes.as_deref().ok_or_else(|| queries.protocol_refusal())?,
                )
                .ok_or_else(|| queries.resource_refusal())?;
            (
                Some(record),
                hirsel_artifact_store::Action(ContentHash::of_parts(
                    "hirsel.front.failure-action.v1",
                    [
                        input.key.0.0.as_slice(),
                        input.key.1.0.as_slice(),
                        record.output.0.0.as_slice(),
                    ],
                )),
            )
        } else if let Some(bytes) = &bytes {
            let variant = VariantRef::publish(
                queries.store.as_ref(),
                &input.key,
                (*outcome.reads).clone(),
                &bytes,
            )
            .ok_or_else(|| queries.resource_refusal())?;
            let mut entry = hirsel_engine_store::Entry::load(&*queries.store, &input.key);
            entry.insert(variant.clone());
            queries.store.put_query(
                input.key.2,
                &input.key,
                entry.action(&input.key),
                &entry.encode_for(&input.key),
            );
            (Some(variant.output), variant.action(&input.key))
        } else {
            (
                None,
                hirsel_engine_store::query_action(&input.key, input.key.1, &outcome.reads),
            )
        };
        drop(bytes);
        let pending_body =
            (!failed && queries.body_cache_limit != 0 && !queries.store.persistence().retains())
                .then(|| outcome.clone());
        let mut cached = queries
            .publish_facts(
                cx, self.input, record, action, outcome, checked, dead_arms, table_keys, memo_rows,
            )
            .ok_or_else(|| queries.resource_refusal())?;
        queries.store_compact(self.input, &mut cached, self.weight.scratch);
        if let Some(outcome) = pending_body
            && let FrontState::Settled {
                publication, body, ..
            } = &mut cached.state
            && let Some(summary) = publication.live_value::<crate::products::LiveSummaryProduct>()
        {
            let charge = LiveAnalyzedProduct::shared_required_bytes(&outcome).and_then(|bytes| {
                bytes.checked_add(hirsel_engine_admission::retained_arc_bytes(
                    &std::mem::MaybeUninit::<LiveSourceBacking>::uninit(),
                ))
            });
            if charge.is_some_and(|bytes| {
                bytes <= queries.body_cache_limit
                    && bytes <= *queries.retention.lock().unwrap_or_else(|e| e.into_inner())
            }) && let Some(product) = queries.admit_body(
                cx,
                queries
                    .body_query(input.definition.path())
                    .ok_or_else(|| queries.protocol_refusal())?
                    .address,
                action,
                outcome,
                summary.clone(),
            ) && let Some(analyzed) =
                product.live_value::<crate::products::LiveAnalyzedProduct>()
            {
                *body = Some(analyzed.downgrade());
                cached.pending_body = Some(PendingBody {
                    product: analyzed.clone(),
                    charge: charge.expect("body admission checked its charge"),
                });
            }
        }
        let terminal = match &cached.state {
            FrontState::Failed { record, .. } => Err(*record),
            FrontState::Settled { publication, .. } => Ok(publication.clone()),
        };
        queries
            .memo_operations
            .fetch_add(cached.memos.len() as u64, Relaxed);
        queries.install_outcome(self.input, cached, self.weight.scratch);
        match terminal {
            Err(reference) => Err(reference.into()),
            Ok(publication) => {
                self.remember_carry();
                if let Some(record) = record {
                    queries.store.associate_written_product(
                        hirsel_artifact_store::Address(input.key.0),
                        action,
                        record,
                    );
                }
                Ok(publication)
            }
        }
    }
}
pub(in crate::queries) struct SpecializationDependencies {
    pub(in crate::queries) text: ContentHash,
    pub(in crate::queries) reads: Arc<Reads>,
    pub(in crate::queries) paths: Vec<DefPath>,
}
impl QueryEngine {
    pub(in crate::queries) fn specialization_dependencies(
        &mut self,
        definition: &DefPath,
    ) -> Option<Vec<DefPath>> {
        let id = DefId::intern(definition);
        let text = self.key_text(definition, QueryKind::Resolve).ok()?;
        if let Some(kept) = self.specialization_dependencies.remove(&id) {
            let valid = kept.text == text
                && kept
                    .reads
                    .first_changed(|read| self.observe(definition, read))
                    .is_none();
            let paths = valid.then(|| kept.paths.clone());
            self.specialization_dependencies.insert(id, kept);
            if paths.is_some() {
                return paths;
            }
        }
        let output = self.resolve_query(definition).ok()?;
        let paths: Vec<_> = output
            .value
            .dependencies
            .iter()
            .filter(|path| hirsel_types::specialization_parts(path).is_some())
            .cloned()
            .collect();
        if !has_errors(&output.diagnostics)
            && let Some(entry) = self.resolutions.get(&id)
            && entry.consistent
            && !self.graph.is_dirty(&entry.memo.key)
        {
            self.specialization_dependencies.insert(
                id,
                SpecializationDependencies {
                    text,
                    reads: Arc::new(entry.reads().flat_copy()),
                    paths: paths.clone(),
                },
            );
        }
        Some(paths)
    }
    /// The accepted registration view of closed `with` clauses (GEN-6): every
    /// specialization a selected body names, and those its own body closes,
    /// is installed before the immutable signature snapshot is captured, and
    /// each becomes a required source root. Callers register before selecting
    /// their roots. Only resolution runs here; a body that fails to resolve or names an
    /// unavailable specialization reports it from its own worker outcome.
    pub fn register_specializations(&mut self, definitions: &[DefPath]) {
        let mut pending: Vec<_> = definitions.iter().map(DefId::intern).collect();
        let mut seen = hirsel_syntax::DefMap::default();
        while let Some(id) = pending.pop() {
            if seen.insert(id, ()).is_some() {
                continue;
            }
            let definition = id.path();
            let facts = self.source_facts(definition);
            if facts.as_ref().is_some_and(|facts| !facts.specializes)
                && !self.specialization_dependencies.contains_key(&id)
            {
                continue;
            }
            let dependencies = if self.installed_definition(definition) {
                // The immutable package already published exact registrations.
                // Reading them does not resolve or reconstruct its With body.
                let Ok(Some(record)) = self
                    .installed_semantic_input(definition, hirsel_engine_store::MAX_RECORD as u64)
                else {
                    continue;
                };
                let Some(registration) = record.specializations else {
                    continue;
                };
                registration.definitions
            } else {
                if facts.is_none()
                    && !self
                        .snapshot(definition)
                        .is_ok_and(|syntax| crate::families::closes(&syntax))
                {
                    continue;
                }
                if self.materialize(definition).is_err() {
                    continue;
                }
                let Some(dependencies) = self.specialization_dependencies(definition) else {
                    continue;
                };
                dependencies
            };
            for dependency in dependencies {
                if self.materialize(&dependency).is_ok() {
                    pending.push(DefId::intern(&dependency));
                }
            }
        }
    }
    /// Capture only demanded body inputs and immutable phase handles. Body
    /// checking starts on the admitted worker, using the same ordinary worker
    /// and codec as scalar queries. Dynamic declarations require their accepted
    /// registration view first; construction never executes a generator.
    pub fn front_body_queries(
        &mut self,
        definitions: &[DefPath],
        weight: MemoryWeight,
        allowance: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
    ) -> Result<Arc<FrontBodyQueries>, QueryError> {
        self.front_body_queries_with_registrations(definitions, &[], weight, allowance, errors)
    }
    /// Freeze original bodies together with explicitly accepted generated views.
    pub fn front_body_queries_with_registrations(
        &mut self,
        definitions: &[DefPath],
        registrations: &[Arc<AcceptedFrontRegistration>],
        weight: MemoryWeight,
        allowance: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
    ) -> Result<Arc<FrontBodyQueries>, QueryError> {
        let capture = self.capture_namespace(definitions, registrations, allowance.bytes())?;
        for definition in definitions {
            if capture
                .bodies
                .get(&DefId::intern(definition))
                .is_some_and(|body| body.generated)
                && !capture
                    .registrations
                    .contains_key(&DefId::intern(definition))
            {
                return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "front query admission".into(),
                    message: "generated body requires an accepted registration view".into(),
                }));
            }
        }
        self.front_body_queries_in(definitions, capture, weight, allowance, errors)
    }
    pub fn front_body_queries_in(
        &mut self,
        definitions: &[DefPath],
        namespace: Arc<NamespaceCapture>,
        weight: MemoryWeight,
        allowance: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
    ) -> Result<Arc<FrontBodyQueries>, QueryError> {
        let pool = hirsel_engine_admission::MemoryReservationPool::new(allowance)
            .map_err(|_| QueryError::NotImplemented)?;
        self.front_body_queries_in_pool(
            definitions,
            namespace,
            weight,
            Arc::new(pool),
            errors,
            None,
        )
    }
    pub(crate) fn front_body_queries_in_pool(
        &mut self,
        definitions: &[DefPath],
        namespace: Arc<NamespaceCapture>,
        weight: MemoryWeight,
        allowance: Arc<hirsel_engine_admission::MemoryReservationPool>,
        errors: RuntimeErrors,
        _previous: Option<&FrontBodyQueries>,
    ) -> Result<Arc<FrontBodyQueries>, QueryError> {
        if let Some(previous) = namespace
            .operation
            .front
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
        {
            previous.inputs.clear_addresses();
        }
        let available = allowance.available_bytes();
        namespace.require_environment(self)?;
        let registrations = namespace
            .registrations
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let registrations = registrations.as_slice();
        let refusal = |reason: &str| {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "front query admission".into(),
                message: format!("immutable front inputs refused: {reason}"),
            })
        };
        let refused_inputs = |required: u64| {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "front query admission".into(),
                message: format!(
                    "immutable front inputs require {required} bytes; available {} bytes",
                    available
                ),
            })
        };
        // The pool has already reserved its metadata before exposing available
        // bytes. The public footprint includes that owner exactly once.
        let mut required = (size_of::<FrontBodyQueries>()
            + size_of::<OperationFrontInputs>()
            + 2 * size_of::<usize>()
            + size_of::<FrontSourceBacking>()
            + size_of::<Mutex<u64>>()
            + 4 * size_of::<usize>()) as u64
            + ProductSource::metadata_bytes();
        if required > available {
            return Err(refused_inputs(required));
        }
        let store = self.result_store.clone().ok_or_else(|| {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "front source transport".into(),
                message: "source inputs require a result provider".into(),
            })
        })?;
        let terminal_cache = store
            .persistence()
            .retains()
            .then(|| store.retention_scope())
            .flatten();
        for definition in definitions {
            if namespace
                .bodies
                .get(&DefId::intern(definition))
                .map(|input| input.implementation)
                != self.implementation_hash(definition)
            {
                return Err(refusal("written source differs from namespace capture"));
            }
        }
        let anchors = namespace.anchors.clone();
        required = required
            .checked_add(namespace.retained_bytes())
            .ok_or_else(|| refusal("capture size overflow"))?;
        if required > available {
            return Err(refused_inputs(required));
        }
        let input_records = crate::NativeRecords::new(self.types.clone(), store.clone());
        let declarations = namespace.declarations.clone();
        let mut siblings = BTreeMap::new();
        for (_, file, _) in namespace.sources.anchors() {
            siblings.entry(file).or_insert_with(SiblingFile::default);
        }
        required = required
            .checked_add(
                source::sibling_input_bytes(siblings.len())
                    .ok_or_else(|| refusal("sibling index overflow"))?,
            )
            .ok_or_else(|| refusal("sibling index overflow"))?;
        if required > available {
            return Err(refused_inputs(required));
        }
        let mut selected = Vec::new();
        for definition in definitions {
            let registration = registrations
                .iter()
                .find(|accepted| accepted.definition() == definition);
            let input = namespace
                .bodies
                .get(&DefId::intern(definition))
                .ok_or_else(|| refusal("definition outside accepted capture"))?;
            let implementation = registration.map_or(input.implementation, |registration| {
                registration.syntax.text_hash
            });
            let text = input.implementation;
            let key = namespace
                .bodies
                .get(&DefId::intern(definition))
                .and_then(|input| input.queries.as_ref())
                .ok_or_else(|| refusal("definition outside accepted capture"))?
                .modes
                .clone();
            selected.push((
                definition.clone(),
                text,
                implementation,
                key,
                registration.cloned(),
            ));
        }
        let snapshot = namespace.snapshot.clone();
        if let Some(headers) = snapshot.header_products() {
            headers.configure_private(weight.scratch, registrations);
        }
        let mut inputs = BTreeMap::new();
        for (definition, text, implementation, key, registration) in selected {
            let id = DefId::intern(&definition);
            inputs.insert(
                id,
                Input {
                    registration,
                    definition: id,
                    text,
                    implementation,
                    key,
                    weight,
                },
            );
        }
        let mut operation_front = namespace
            .operation
            .front
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let previous = operation_front.as_ref().and_then(std::sync::Weak::upgrade);
        let previous = previous.as_deref();
        let inputs = Inputs::new(
            inputs,
            namespace.clone(),
            previous.map(|previous| &previous.inputs),
        )?;
        let arm_fixes = inputs::capture_map(
            arm_fixes::capture(&self.sources, &self.items),
            previous.map(|previous| &previous.arm_fixes),
        )?;
        let cited = inputs::capture_map(
            self.source_paths_iter()
                .map(|path| (DefId::intern(path), self.cited_value(path)))
                .collect(),
            previous.map(|previous| &previous.cited),
        )?;
        let candidates = inputs::capture_map(
            inputs
                .values()
                .map(|input| {
                    (
                        input.definition,
                        self.candidates_value(input.definition.path()),
                    )
                })
                .collect(),
            previous.map(|previous| &previous.candidates),
        )?;
        let indexes = Arc::new(OperationFrontInputs {
            inputs: inputs.clone(),
            cited: cited.clone(),
            candidates: candidates.clone(),
            arm_fixes: arm_fixes.clone(),
        });
        required = required
            .checked_add(indexes.unadmitted_bytes())
            .ok_or_else(|| refusal("input size overflow"))?;
        if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
            crate::queries::operation_owner_trace(format_args!(
                "OWNER FRONT fixed={} namespace={} rows_pending={} required={} available={} definitions={}",
                FrontBodyQueries::input_bytes(&[]).unwrap_or(u64::MAX),
                namespace.retained_bytes(),
                inputs.unadmitted_bytes(),
                required,
                available,
                definitions.len()
            ));
        }
        if required > available {
            return Err(refused_inputs(required));
        }
        let retention = Arc::new(Mutex::new(available - required));
        let items = Arc::new(BTreeMap::new());
        // Include the pool owner as well as the bytes it lends. Reserving
        // this reported live set again must cover construction of its pool.
        let admitted = available
            .checked_add(hirsel_engine_admission::MemoryReservationPool::metadata_bytes())
            .ok_or_else(|| refusal("pool size overflow"))?;
        let mut inputs_allowance = allowance
            .take(required)
            .ok_or_else(|| refusal("shared Front input partition"))?;
        namespace.admit(&mut inputs_allowance, self.sources.clone())?;
        indexes.admit(&mut inputs_allowance, allowance.clone(), retention.clone())?;
        *operation_front = Some(Arc::downgrade(&indexes));
        drop(operation_front);
        let products = ProductSource::admit_shared(
            anchors.as_ref().clone(),
            input_records.clone(),
            &mut inputs_allowance,
        )
        .ok_or_else(|| refusal("shared product admission"))?;
        let backing = Arc::new(FrontSourceBacking {
            products,
            admitted,
            allowance,
            _inputs: inputs_allowance,
            _sources: self.sources.clone(),
        });
        let sibling_sources = Arc::new(SiblingSources {
            files: siblings,
            backing: backing.clone(),
            retention: retention.clone(),
            namespace: namespace.clone(),
        });
        let header_allowance = snapshot
            .header_products()
            .map(|headers| headers.configure_source_backing(backing.clone(), retention.clone()));
        let namespace_sources = namespace.sources.clone();
        Ok(Arc::new(FrontBodyQueries {
            demand: Default::default(),
            forwarded: Mutex::new(BTreeMap::new()),
            discovery_dependency: Mutex::new(None),
            inputs,
            _operation_inputs: indexes,
            types: self.types.clone(),
            memo: self.memo.clone(),
            namespace,
            declarations: declarations.with_archived_siblings(sibling_sources),
            citations: OwnedSourceContext {
                unique_failures: self.private_unique_failures.clone(),
                sources: namespace_sources,
                items,
                declarations: self.declarations.observations(),
                catalog: self.catalog.clone(),
                owners: anchors.clone(),
                cited: operation_inputs::SourceIndex::Operation(cited),
                arm_fixes: operation_inputs::SourceIndex::Operation(arm_fixes),
            },
            resolver: self.adapters.resolver.clone(),
            checker: self.adapters.checker.clone(),
            modes: self.adapters.modes.clone(),
            environment: self.environment_value(),
            candidates,
            records: self.type_records().ok_or_else(|| refusal("type records"))?,
            input_records,
            store,
            terminal_cache,
            slots: Mutex::new(BTreeMap::new()),
            input_slots: Mutex::new((BTreeMap::new(), None)),
            body_cache_limit: self.hold_bytes,
            carry: Mutex::new(None),
            carry_changes: Mutex::new(None),
            clean_memos: Mutex::new(BTreeSet::new()),
            work: Mutex::new(BTreeMap::new()),
            store_adoptions: AtomicU64::new(0),
            refusals: Default::default(),
            text_refusals: AtomicU64::new(0),
            memo_operations: AtomicU64::new(0),
            verify_store: self.verify_store,
            errors,
            refusal: Mutex::new(None),
            retention,
            read_pool: std::sync::OnceLock::new(),
            _allowance: backing,
            _header_allowance: header_allowance,
        }))
    }
}
