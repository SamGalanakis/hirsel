//! Dependency admission and ordered publication of one complete source Check.
//! The admission producer streams P/Lower work independently of SCC acceptance.
//! This owner reduces settled observations in source order. An unavailable P or
//! failed Lower still withholds its publication suffix, never independent work.
use super::reader_receipts::{ReaderReceipt, ReaderSequence};
use super::*;
use hirsel_engine_runtime::*;
use hirsel_engine_store::{Domain, Reference, ResultStore};
use std::{sync::Mutex, task::Poll};
mod admission;
mod products;
pub use products::FrontCheckedInput;
mod scope;
pub use scope::FrontCheckScope;
pub const FRONT_CHECK_KIND: u32 = 0x4600_000a;
const FRONT_WINDOW: usize = 1;
const OUTPUT: Domain = Domain {
    kind: 0x4600_000b,
    schema: 4,
};
/// Engine-selected snapshots and source order. Clients submit operations and
/// frozen input observations; the compiler host constructs this plan.
pub struct FrontCheckPlan {
    pub files: Vec<(FileId, FileContext, LoweringPolicy)>,
    pub front: Arc<FrontBodyQueries>,
    pub lower: Arc<FrontLowerQueries>,
    pub growth: Arc<FrontGrowthQueries>,
    pub unsafe_ffi: Vec<UnsafeFfiPackage>,
    /// The root package whose never-constructed error arms (LANG-8) warn.
    pub lint_package: Option<hirsel_syntax::PackageId>,
}
/// LANG-8 inputs captured from the root package's source items: its private
/// error-type declarations with their arms, and every body that may use them.
struct DeadArmLint {
    declarations: BTreeMap<DefPath, (hirsel_span::Anchor, Vec<(String, u32, u32)>)>,
    bodies: Vec<DefPath>,
}
impl DeadArmLint {
    fn insert(&mut self, item: &super::catalog::SourceItem) {
        if !item.facts.arms.is_empty() {
            self.declarations.insert(
                item.definition.clone(),
                (item.anchor, item.facts.arms.clone()),
            );
        }
        if item.facts.body_kind != BodyKind::None {
            self.bodies.push(item.definition.clone());
        }
    }
    fn bytes(&self) -> u64 {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(std::mem::size_of::<Self>());
        weight.tree::<DefPath, (hirsel_span::Anchor, Vec<(String, u32, u32)>)>(
            self.declarations.len(),
        );
        weight.tree::<DefPath, ()>(self.declarations.len());
        weight.tree::<(DefPath, u32), ()>(
            self.declarations.values().map(|(_, arms)| arms.len()).sum(),
        );
        for (path, (_, arms)) in &self.declarations {
            weight.path(path);
            weight.path(path);
            for _ in arms {
                weight.path(path);
            }
            weight.vec(arms, |weight, (name, _, _)| weight.add(name.capacity()));
        }
        weight.vec(&self.bodies, |weight, path| weight.path(path));
        weight.bytes()
    }
}
#[derive(Clone)]
struct FileInput {
    paths: Vec<(DefPath, bool)>,
    lowering: LoweringPolicy,
    diagnostics: Vec<Diagnostic>,
    error: Option<QueryError>,
    phase: Option<QueryKind>,
}
/// Complete observations of one consumed source root, without its live Summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontObservation {
    pub address: hirsel_artifact_store::Address,
    pub action: hirsel_artifact_store::Action,
    domain: Domain,
    reads: ReaderReceipt,
}
impl FrontObservation {
    pub fn domain(&self) -> Domain {
        self.domain
    }
}
struct Data {
    checked: CheckOutput,
    front: Vec<(DefPath, FrontObservation)>,
    lowered: Vec<(DefPath, ProductRef)>,
    failed_front: Vec<(DefPath, Reference)>,
    failed_lowered: Vec<(DefPath, Reference)>,
    reads: ReaderReceipt,
    store: Arc<dyn ResultStore>,
}
struct FrontRows<'a>(&'a [(DefPath, FrontObservation)]);
impl serde::Serialize for FrontRows<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(
            self.0
                .iter()
                .map(|(path, p)| (path, p.address.0, p.action.0, p.domain())),
        )
    }
}
struct LowerRows<'a>(&'a [(DefPath, ProductRef)]);
impl serde::Serialize for LowerRows<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(
            self.0
                .iter()
                .map(|(path, p)| (path, p.address.0, p.action.0, p.domain())),
        )
    }
}
impl serde::Serialize for Data {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (
            match self.checked.completion {
                CheckCompletion::Complete => None,
                CheckCompletion::Incomplete { phase } => Some(phase.phase_name()),
            },
            &self.checked.diagnostics,
            FrontRows(&self.front),
            LowerRows(&self.lowered),
            &self.failed_front,
            &self.failed_lowered,
            self.reads
                .load(&*self.store)
                .ok_or_else(|| serde::ser::Error::custom("Check observations unavailable"))?,
        )
            .serialize(serializer)
    }
}
/// Source and receipt owners remain admitted until the last inspection or
/// execution consumer releases the completed check.
pub struct FrontCheckOutput {
    pub checked: CheckOutput,
    pub front: Vec<(DefPath, FrontObservation)>,
    pub lowered: Vec<(DefPath, ProductRef)>,
    pub reads: Arc<Reads>,
    _owner: Arc<FrontCheckQueries>,
}
pub struct FrontCheckQueries {
    query: QueryId,
    files: Vec<FileInput>,
    plan: FrontCheckPlan,
    storage: Option<Arc<FrontStorageGraphQueries>>,
    lint: Option<DeadArmLint>,
    constants: BTreeSet<DefId>,
    instances: Vec<DefPath>,
    sources: SourceMap,
    /// The indexed modules, read only to explain a missing name (PROG-1).
    declarations: Arc<crate::snapshot::Declarations>,
    records: crate::NativeRecords,
    anchors: crate::NativeRecordContext,
    store: Arc<dyn ResultStore>,
    completed: Mutex<Option<(ProductRef, Arc<Data>)>>,
    failures: Mutex<BTreeMap<ContentHash, (Reference, QueryError, Option<QueryKind>)>>,
    weight: MemoryWeight,
    errors: RuntimeErrors,
    _allowance: Arc<hirsel_engine_admission::MemoryReservation>,
}
impl FrontCheckQueries {
    #[track_caller]
    fn protocol_refusal(&self) -> Reference {
        crate::requests::source_graph::protocol_failure(
            &*self.store,
            self.errors,
            Some(self.query()),
            "Check producer state or selected product is unavailable",
        )
    }

    pub(super) fn matches_inputs(
        &self,
        front: &Arc<FrontBodyQueries>,
        lower: &Arc<FrontLowerQueries>,
    ) -> bool {
        Arc::ptr_eq(&self.plan.front, front) && Arc::ptr_eq(&self.plan.lower, lower)
    }
    /// Install the matching actual source storage producer before sharing this
    /// new root with any factory or consumer. No mutable graph switch exists.
    pub fn with_storage(
        mut self: Arc<Self>,
        storage: Arc<FrontStorageGraphQueries>,
    ) -> Result<Arc<Self>, QueryError> {
        let error = || {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "ordered source storage admission".into(),
                message: "storage graph must match an unshared Check root".into(),
            })
        };
        if !storage.matches(&self.plan.front, &self.plan.lower, &self.plan.growth) {
            return Err(error());
        }
        Arc::get_mut(&mut self).ok_or_else(error)?.storage = Some(storage);
        Ok(self)
    }

    /// Ordered file/path cursors, product-reference rows and separate retained
    /// live output bounds. Bodies keep their own producer admission.
    pub fn input_bytes(definitions: &[DefPath], files: usize, weight: MemoryWeight) -> Option<u64> {
        let path_slot = (std::mem::size_of::<(DefPath, bool)>()
            + std::mem::size_of::<(DefPath, ProductRef)>()
            + std::mem::size_of::<(DefPath, FrontObservation)>()
            + 2 * std::mem::size_of::<(DefPath, Reference)>()
            + std::mem::size_of::<DefId>()
            + 2 * std::mem::size_of::<DefPath>()
            + 128
            - (std::mem::size_of::<ReaderReceipt>() - std::mem::size_of::<Arc<Reads>>()))
            as u64;
        const {
            assert!(std::mem::size_of::<ReaderReceipt>() <= 128);
        };
        let base = weight
            .retained
            .checked_mul(2)?
            .checked_add(
                (files as u64).checked_mul((2 * std::mem::size_of::<FileInput>() + 128) as u64)?,
            )?
            .checked_add(
                (std::mem::size_of::<Mutex<u64>>()
                    + std::mem::size_of::<hirsel_engine_admission::MemoryReservation>()
                    + 4 * std::mem::size_of::<usize>()) as u64,
            )?;
        definitions.iter().try_fold(base, |total, path| {
            total.checked_add(
                path_slot.checked_add(bincode::serialized_size(path).ok()?.checked_mul(5)?)?,
            )
        })
    }
    /// The packages admitted to call C (PROG-4).
    pub(super) fn unsafe_ffi(&self) -> &[UnsafeFfiPackage] {
        &self.plan.unsafe_ffi
    }
    pub fn query(&self) -> QueryId {
        self.query
    }
    /// The bounded body stream is a dependency of Check, not an acceptance root.
    pub fn admission_query(&self) -> QueryId {
        QueryId {
            kind: FRONT_CHECK_KIND,
            address: hirsel_artifact_store::Address(ContentHash::of_parts(
                "hirsel.front.check-admission.address.v1",
                [self.query.address.0.0.as_slice()],
            )),
        }
    }
    pub fn diagnostic_phase(&self, diagnostic: &Diagnostic) -> Option<String> {
        let matches = |candidate: &Diagnostic| {
            candidate.rule == diagnostic.rule && candidate.primary == diagnostic.primary
        };
        self.plan
            .front
            .diagnostic_phase(diagnostic)
            .or_else(|| {
                self.files.iter().find_map(|file| {
                    file.diagnostics
                        .iter()
                        .any(matches)
                        .then(|| file.phase.map(|phase| phase.phase_name().to_owned()))
                        .flatten()
                })
            })
            .or_else(|| {
                let completed = self.completed.lock().unwrap_or_else(|e| e.into_inner());
                let (_, data) = completed.as_ref()?;
                data.failed_lowered.iter().find_map(|(_, reference)| {
                    let QueryError::Diagnostics(diagnostics) =
                        self.plan.lower.failure(*reference)?
                    else {
                        return None;
                    };
                    diagnostics
                        .iter()
                        .any(matches)
                        .then(|| QueryKind::Lower.phase_name().to_owned())
                })
            })
    }
    pub fn factory(self: &Arc<Self>) -> Arc<dyn QueryFactory> {
        Arc::new(Factory(self.clone()))
    }
    pub fn outcome(self: &Arc<Self>, product: ProductRef) -> Option<FrontCheckOutput> {
        let completed = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        let (selected, data) = completed.as_ref()?;
        if *selected != product {
            return None;
        }
        Some(FrontCheckOutput {
            checked: CheckOutput {
                completion: data.checked.completion,
                sources: data.checked.sources.clone(),
                diagnostics: data.checked.diagnostics.clone(),
                unsafe_ffi: data.checked.unsafe_ffi.clone(),
            },
            front: data.front.clone(),
            lowered: data.lowered.clone(),
            reads: data.reads.load(&*self.store)?,
            _owner: self.clone(),
        })
    }
    pub fn checked(&self, product: ProductRef) -> Option<CheckOutput> {
        let completed = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        let (selected, data) = completed.as_ref()?;
        if *selected != product {
            return None;
        }
        Some(CheckOutput {
            completion: data.checked.completion,
            sources: data.checked.sources.clone(),
            diagnostics: data.checked.diagnostics.clone(),
            unsafe_ffi: data.checked.unsafe_ffi.clone(),
        })
    }
    /// Inspect acceptance without cloning the ordered file/product inventory.
    /// Conditional host continuations use this only after reading this root.
    pub fn accepted(&self, product: ProductRef) -> Option<bool> {
        let completed = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        let (selected, data) = completed.as_ref()?;
        (*selected == product).then(|| {
            data.checked.completion == CheckCompletion::Complete
                && !has_errors(&data.checked.diagnostics)
        })
    }
    /// Reuse this exact rejected Check's existing diagnostic storage. A native
    /// acceptance join retains no second copy of a whole source failure.
    pub(crate) fn rejection(&self, reference: Reference) -> Option<QueryError> {
        let completed = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        let (product, data) = completed.as_ref()?;
        if OUTPUT.reference(&product.action.0.0)? != reference {
            return None;
        }
        if has_errors(&data.checked.diagnostics) {
            Some(QueryError::Diagnostics(data.checked.diagnostics.clone()))
        } else if data.checked.completion != CheckCompletion::Complete {
            Some(QueryError::NotImplemented)
        } else {
            None
        }
    }
    /// A failed dependency carries an identity token, not an archive demand.
    pub(crate) fn rejection_reference(&self, product: &ProductRef) -> Option<Reference> {
        (self.accepted(product.clone()) == Some(false))
            .then(|| OUTPUT.reference(&product.action.0.0))
            .flatten()
    }
    pub fn failure(&self, reference: Reference) -> Option<(QueryError, Option<QueryKind>)> {
        let failures = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        let (selected, error, phase) = failures.get(&reference.output.0)?;
        (*selected == reference).then(|| (error.clone(), *phase))
    }
}
struct Factory(Arc<FrontCheckQueries>);
impl QueryFactory for Factory {
    fn create(&self, id: QueryId) -> Result<Box<dyn QueryTask>, Reference> {
        if id == self.0.admission_query() {
            return Ok(Box::new(admission::Task::new(self.0.clone())));
        }
        if id != self.0.query {
            return Err(self.0.protocol_refusal());
        }
        self.0.plan.front.compact_retained_reads();
        Ok(Box::new(Task {
            queries: self.0.clone(),
            files: self.0.files.iter().cloned().map(File::new).collect(),
            publication: Publication::Front,
            admitted: false,
            file: 0,
            root: 0,
            selecting_instances: false,
            staged: None,
            instances: Vec::new(),
            reads: ReaderSequence::default(),
            front: Vec::new(),
            lowered: Vec::new(),
            failed_front: Vec::new(),
            failed_lowered: Vec::new(),
            lint_seen: 0,
            lint_complete: true,
            lint_errors: BTreeSet::new(),
            lint_constructed: BTreeSet::new(),
            terminal: None,
        }))
    }
}
struct File {
    input: FileInput,
    diagnostics: Vec<Diagnostic>,
    error: Option<QueryError>,
    phase: Option<QueryKind>,
    comptime: Vec<DefPath>,
    rest: Vec<DefPath>,
    source_rules: bool,
}
impl File {
    fn new(input: FileInput) -> Self {
        Self {
            diagnostics: input.diagnostics.clone(),
            error: input.error.clone(),
            phase: input.phase,
            input,
            comptime: Vec::new(),
            rest: Vec::new(),
            source_rules: false,
        }
    }
    fn clean(&self) -> bool {
        self.error.is_none() && !has_errors(&self.diagnostics)
    }
    fn error(&mut self, error: QueryError, phase: Option<QueryKind>) {
        match error {
            QueryError::Diagnostics(diagnostics) => self.diagnostics.extend(diagnostics),
            other => {
                self.error = Some(other);
                self.phase = phase;
            }
        }
    }
}
// Ordered reduction only. All body work, including selected instances and
// staged evaluation, has already been admitted by the independent stream.
#[derive(Clone, Copy)]
enum Publication {
    Front,
    Growth,
    Storage,
    Comptime,
    Rest,
    RuntimeStaged,
    Done,
}
struct Task {
    queries: Arc<FrontCheckQueries>,
    files: Vec<File>,
    publication: Publication,
    admitted: bool,
    file: usize,
    root: usize,
    selecting_instances: bool,
    instances: Vec<DefPath>,
    staged: Option<Vec<super::scheduled_lower::staged_runtime::Selection>>,
    reads: ReaderSequence,
    front: Vec<(DefPath, FrontObservation)>,
    lowered: Vec<(DefPath, ProductRef)>,
    failed_front: Vec<(DefPath, Reference)>,
    failed_lowered: Vec<(DefPath, Reference)>,
    lint_seen: usize,
    lint_complete: bool,
    lint_errors: BTreeSet<DefPath>,
    lint_constructed: BTreeSet<(DefPath, u32)>,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl QueryTask for Task {
    fn id(&self) -> QueryId {
        self.queries.query
    }
    fn weight(&self) -> MemoryWeight {
        self.queries.weight
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        if let Some(result) = self.terminal.clone() {
            return finish(result);
        }
        // These producers can make progress independently. In particular the
        // body stream must not be a continuation of the aggregate SCC verdict.
        if !self.admitted {
            let mut pending = Vec::new();
            let admission = self.queries.admission_query();
            match cx.read(admission) {
                Poll::Pending => pending.push(admission),
                Poll::Ready(Err(error)) => return QueryPoll::Failed(error),
                Poll::Ready(Ok(_)) => cx.consume(admission),
            }
            if self.files.iter().any(File::clean) {
                let growth = self.queries.plan.growth.query();
                if cx.read(growth).is_pending() {
                    pending.push(growth);
                }
            }
            if !pending.is_empty() {
                return QueryPoll::Waiting(pending);
            }
            self.admitted = true;
        }
        match self.advance(cx) {
            Ok(Some(waiting)) => QueryPoll::Waiting(waiting),
            result => {
                let result = result.and_then(|_| self.complete());
                self.terminal = Some(result.clone());
                finish(result)
            }
        }
    }
}
fn finish(result: Result<ProductRef, Reference>) -> QueryPoll {
    match result {
        Ok(product) => QueryPoll::Complete(product),
        Err(error) => QueryPoll::Failed(error),
    }
}
impl Task {
    fn advance(&mut self, cx: &mut dyn QueryContext) -> Result<Option<Vec<QueryId>>, Reference> {
        loop {
            match self.publication {
                Publication::Front => {
                    // Only this source-order window pins speculative P work.
                    // Consumed roots retain observations and release their edge.
                    while self.file < self.files.len() {
                        let file = &mut self.files[self.file];
                        if file.input.error.is_some()
                            || has_errors(&file.input.diagnostics)
                            || file.error.is_some()
                            || self.root == file.input.paths.len()
                        {
                            self.file += 1;
                            self.root = 0;
                            continue;
                        }
                        let mut pending = Vec::new();
                        for (path, _) in file.input.paths.iter().skip(self.root).take(FRONT_WINDOW)
                        {
                            let id = self
                                .queries
                                .plan
                                .front
                                .query(path)
                                .ok_or_else(|| self.queries.protocol_refusal())?;
                            if cx.read(id).is_pending() {
                                pending.push(id);
                            }
                        }
                        if !pending.is_empty() {
                            return Ok(Some(pending));
                        }
                        let (path, executable) = &file.input.paths[self.root];
                        let id = self
                            .queries
                            .plan
                            .front
                            .query(path)
                            .ok_or_else(|| self.queries.protocol_refusal())?;
                        let selected = match cx.read(id) {
                            Poll::Pending => return Ok(Some(vec![id])),
                            Poll::Ready(selected) => selected,
                        };
                        let reference = selected.clone();
                        let Some(facts) = self.queries.plan.front.check_facts(reference.clone())
                        else {
                            return Err(reference
                                .err()
                                .unwrap_or_else(|| self.queries.protocol_refusal()));
                        };
                        let observed = self
                            .reads
                            .record_receipt(
                                self.queries
                                    .plan
                                    .front
                                    .reader_receipt(
                                        id,
                                        &facts.reads,
                                        &self.queries.store,
                                        self.queries.weight.scratch,
                                    )
                                    .ok_or(self.queries.errors.resource)?,
                                self.queries.weight.retained,
                                Some((DefId::intern(path), false)),
                            )
                            .ok_or(self.queries.errors.resource)?;
                        if self
                            .queries
                            .lint
                            .as_ref()
                            .is_some_and(|lint| lint.bodies.contains(path))
                        {
                            self.lint_seen += 1;
                            if let Some(uses) = facts.dead_arms {
                                let lint = self
                                    .queries
                                    .lint
                                    .as_ref()
                                    .ok_or_else(|| self.queries.protocol_refusal())?;
                                self.lint_errors.extend(
                                    uses.errors
                                        .into_iter()
                                        .filter(|path| lint.declarations.contains_key(path)),
                                );
                                self.lint_constructed
                                    .extend(uses.constructed.into_iter().filter(|(path, arm)| {
                                        lint.declarations
                                            .get(path)
                                            .is_some_and(|(_, arms)| (*arm as usize) < arms.len())
                                    }));
                            } else {
                                self.lint_complete = false;
                            }
                        }
                        file.source_rules |= facts.source_rules;
                        match facts.result {
                            Ok(diagnostics) => {
                                file.diagnostics.extend(diagnostics);
                                if let Ok(product) = selected {
                                    self.front.push((
                                        path.clone(),
                                        FrontObservation {
                                            address: product.address,
                                            action: product.action,
                                            domain: product.domain(),
                                            reads: observed,
                                        },
                                    ));
                                }
                                if *executable {
                                    if facts
                                        .lowering
                                        .as_ref()
                                        .ok_or_else(|| self.queries.protocol_refusal())?
                                        .needed_with(|id| self.queries.constants.contains(&id))
                                    {
                                        file.comptime.push(path.clone());
                                    } else {
                                        file.rest.push(path.clone());
                                    }
                                }
                            }
                            Err(error) => {
                                self.failed_front.push((
                                    path.clone(),
                                    reference
                                        .err()
                                        .unwrap_or_else(|| self.queries.protocol_refusal()),
                                ));
                                file.error(error, facts.phase);
                                if file.error.is_none()
                                    && let Some(phase) = facts.incomplete
                                {
                                    file.error(QueryError::NotImplemented, Some(phase));
                                }
                            }
                        }
                        cx.consume(id);
                        self.root += 1;
                    }
                    self.publication = Publication::Growth;
                    self.file = 0;
                    self.root = 0;
                }
                Publication::Growth => {
                    if self.files.iter().any(File::clean) {
                        let id = self.queries.plan.growth.query();
                        let selected = match cx.read(id) {
                            Poll::Pending => return Ok(Some(vec![id])),
                            Poll::Ready(selected) => selected,
                        };
                        match selected {
                            Ok(product) => {
                                let output = self
                                    .queries
                                    .plan
                                    .growth
                                    .outcome(product)
                                    .ok_or_else(|| self.queries.protocol_refusal())?;
                                self.reads
                                    .record(
                                        &*self.queries.store,
                                        &output.reads,
                                        self.queries.weight.scratch,
                                        self.queries.weight.retained,
                                        None,
                                    )
                                    .ok_or(self.queries.errors.resource)?;
                                for file in self.files.iter_mut().filter(|file| file.clean()) {
                                    file.diagnostics.extend(output.diagnostics.iter().cloned());
                                }
                            }
                            Err(reference) => {
                                let (error, phase) = if let Some(output) =
                                    self.queries.plan.front.check_facts(Err(reference))
                                {
                                    (
                                        output
                                            .result
                                            .err()
                                            .ok_or_else(|| self.queries.protocol_refusal())?,
                                        output.phase,
                                    )
                                } else {
                                    (
                                        self.queries
                                            .plan
                                            .growth
                                            .failure(reference)
                                            .ok_or(reference)?,
                                        Some(QueryKind::Check),
                                    )
                                };
                                for file in self.files.iter_mut().filter(|file| file.clean()) {
                                    file.error(error.clone(), phase);
                                }
                            }
                        }
                    }
                    self.publication = Publication::Storage;
                }
                Publication::Storage => {
                    if self.files.iter().any(File::clean)
                        && self.files.iter().any(|file| file.source_rules)
                    {
                        if let Some(storage) = &self.queries.storage {
                            let id = storage.query();
                            let result = match cx.read(id) {
                                Poll::Pending => return Ok(Some(vec![id])),
                                Poll::Ready(result) => result,
                            };
                            match result {
                                Ok(product) => {
                                    let output = storage
                                        .outcome(product)
                                        .ok_or_else(|| self.queries.protocol_refusal())?;
                                    self.reads
                                        .record(
                                            &*self.queries.store,
                                            &output.reads,
                                            self.queries.weight.scratch,
                                            self.queries.weight.retained,
                                            None,
                                        )
                                        .ok_or(self.queries.errors.resource)?;
                                    for file in self.files.iter_mut().filter(|file| file.clean()) {
                                        file.diagnostics.extend(output.diagnostics.iter().cloned());
                                    }
                                }
                                Err(reference) => {
                                    let error = storage
                                        .failure(reference)
                                        .or_else(|| self.queries.plan.lower.failure(reference))
                                        .or_else(|| {
                                            self.queries
                                                .plan
                                                .front
                                                .outcome(Err(reference))
                                                .and_then(|o| o.outcome.result.err())
                                        })
                                        .ok_or(reference)?;
                                    for file in self.files.iter_mut().filter(|file| file.clean()) {
                                        file.error(error.clone(), Some(QueryKind::Check));
                                    }
                                }
                            }
                        } else if self.files.iter().any(|file| file.source_rules) {
                            for file in self.files.iter_mut().filter(|file| file.clean()) {
                                file.error(QueryError::NotImplemented, Some(QueryKind::Check));
                            }
                        }
                    }
                    self.publication = Publication::Comptime;
                }
                Publication::Comptime | Publication::Rest => {
                    let rest = matches!(self.publication, Publication::Rest);
                    while self.file < self.files.len() {
                        let file = &mut self.files[self.file];
                        if !file.clean()
                            || (rest && file.input.lowering != LoweringPolicy::Reachable)
                        {
                            self.file += 1;
                            self.root = 0;
                            self.selecting_instances = false;
                            self.instances.clear();
                            continue;
                        }
                        let paths = if rest {
                            &file.rest
                        } else if self.selecting_instances {
                            &self.instances
                        } else {
                            &file.comptime
                        };
                        if self.root == paths.len() {
                            if !rest && !self.selecting_instances {
                                let roots = file.input.paths.iter().map(|(path, _)| path.clone());
                                match self
                                    .queries
                                    .plan
                                    .lower
                                    .selected_instances(roots, self.queries.weight.scratch)
                                {
                                    Ok(instances) => {
                                        self.instances = instances.into_iter().collect()
                                    }
                                    Err(error) => file.error(error, Some(QueryKind::Lower)),
                                }
                                self.selecting_instances = true;
                                self.root = 0;
                                continue;
                            }
                            self.file += 1;
                            self.root = 0;
                            self.selecting_instances = false;
                            self.instances.clear();
                            continue;
                        }
                        let path = &paths[self.root];
                        let lower = &self.queries.plan.lower;
                        let id = lower
                            .query(path)
                            .ok_or_else(|| self.queries.protocol_refusal())?;
                        let selected = match cx.read(id) {
                            Poll::Pending => return Ok(Some(vec![id])),
                            Poll::Ready(selected) => selected,
                        };
                        match selected {
                            Ok(product) => {
                                let output = self
                                    .queries
                                    .plan
                                    .lower
                                    .outcome(product.clone())
                                    .ok_or_else(|| self.queries.protocol_refusal())?;
                                self.reads
                                    .record(
                                        &*self.queries.store,
                                        &output.reads,
                                        self.queries.weight.scratch,
                                        self.queries.weight.retained,
                                        None,
                                    )
                                    .ok_or(self.queries.errors.resource)?;
                                file.diagnostics.extend(output.output.diagnostics);
                                self.lowered.push((
                                    path.clone(),
                                    self.queries
                                        .plan
                                        .lower
                                        .observation_product(product)
                                        .ok_or_else(|| self.queries.protocol_refusal())?,
                                ));
                                cx.consume(id);
                            }
                            Err(reference) => {
                                let reads = self
                                    .queries
                                    .plan
                                    .lower
                                    .selected_reads(id, Err(reference))
                                    .ok_or(reference)?;
                                self.reads
                                    .record(
                                        &*self.queries.store,
                                        &reads,
                                        self.queries.weight.scratch,
                                        self.queries.weight.retained,
                                        None,
                                    )
                                    .ok_or(self.queries.errors.resource)?;
                                self.failed_lowered.push((path.clone(), reference));
                                file.error(
                                    self.queries
                                        .plan
                                        .lower
                                        .failure(reference)
                                        .ok_or(reference)?,
                                    Some(QueryKind::Lower),
                                );
                            }
                        }
                        self.root += 1;
                    }
                    self.file = 0;
                    self.root = 0;
                    self.publication = if rest {
                        Publication::RuntimeStaged
                    } else {
                        Publication::Rest
                    };
                }
                Publication::RuntimeStaged => {
                    while self.file < self.files.len() {
                        let file = &mut self.files[self.file];
                        if !file.clean() {
                            self.file += 1;
                            self.root = 0;
                            self.staged = None;
                            continue;
                        }
                        if self.staged.is_none() {
                            let roots = file.input.paths.iter().map(|(path, _)| path.clone());
                            match self.queries.plan.lower.runtime_staged_queries(
                                roots,
                                self.queries.weight.scratch,
                                self.queries.weight.retained,
                            ) {
                                Ok(staged) => self.staged = Some(staged),
                                Err(error) => {
                                    file.error(error, Some(QueryKind::Lower));
                                    continue;
                                }
                            }
                        }
                        let Some((id, site)) = self
                            .staged
                            .as_ref()
                            .and_then(|rows| rows.get(self.root))
                            .copied()
                        else {
                            self.file += 1;
                            self.root = 0;
                            self.staged = None;
                            continue;
                        };
                        let selected = match cx.read(id) {
                            Poll::Pending => return Ok(Some(vec![id])),
                            Poll::Ready(selected) => selected,
                        };
                        let reference = selected
                            .as_ref()
                            .err()
                            .copied()
                            .unwrap_or_else(|| self.queries.protocol_refusal());
                        let reads = self
                            .queries
                            .plan
                            .lower
                            .selected_reads(id, selected.clone())
                            .ok_or(reference)?;
                        self.reads
                            .record(
                                &*self.queries.store,
                                &reads,
                                self.queries.weight.scratch,
                                self.queries.weight.retained,
                                None,
                            )
                            .ok_or(self.queries.errors.resource)?;
                        if selected.is_err() {
                            file.error(
                                self.queries
                                    .plan
                                    .lower
                                    .staged_failure(id, reference, site)
                                    .ok_or(reference)?,
                                Some(QueryKind::Lower),
                            );
                        }
                        self.root += 1;
                    }
                    self.publication = Publication::Done;
                }
                Publication::Done => return Ok(None),
            }
        }
    }
    fn complete(&mut self) -> Result<ProductRef, Reference> {
        let queries = &self.queries;
        queries.plan.front.release_handoff();
        let reads = std::mem::take(&mut self.reads)
            .finish(&*queries.store, queries.weight.scratch, &queries.plan.front)
            .ok_or(queries.errors.resource)?
            .map_err(|error| {
                crate::requests::source_graph::protocol_failure(
                    &*queries.store,
                    queries.errors,
                    Some(queries.query()),
                    &format!("Check observations are inconsistent: {error:?}"),
                )
            })?;
        let stopped = self.files.iter().find_map(|file| match &file.error {
            Some(error) if *error != QueryError::NotImplemented => {
                Some((error.clone(), file.phase))
            }
            _ => None,
        });
        if let Some((error, phase)) = stopped {
            let bytes = queries
                .records
                .encode(
                    &queries.anchors,
                    &(&error, phase.map(QueryKind::phase_name), &reads),
                )
                .ok_or(queries.errors.resource)?;
            let reference = queries
                .store
                .put_content(OUTPUT, &bytes)
                .ok_or(queries.errors.resource)?;
            queries
                .failures
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(reference.output.0, (reference, error, phase));
            return Err(reference);
        }
        let incomplete = self
            .files
            .iter()
            .filter(|file| file.error == Some(QueryError::NotImplemented))
            .filter_map(|file| file.phase)
            .last();
        let completion = incomplete.map_or(CheckCompletion::Complete, |phase| {
            CheckCompletion::Incomplete { phase }
        });
        let mut diagnostics: Vec<_> = self
            .files
            .iter()
            .flat_map(|file| file.diagnostics.iter().cloned())
            .collect();
        // The root package's dead error arms (LANG-8) warn only after a clean
        // complete check; a body without settled facts leaves it unlinted.
        if completion == CheckCompletion::Complete && !has_errors(&diagnostics) {
            if let Some(lint) = &queries.lint
                && self.lint_complete
                && self.lint_seen == lint.bodies.len()
            {
                diagnostics.extend(super::dead_arms::dead_arm_diagnostics(
                    lint.declarations.clone(),
                    &self.lint_errors,
                    &self.lint_constructed,
                ));
            }
        }
        let mut diagnostics = crate::select_diagnostics(diagnostics, Some(&queries.sources));
        let files: Vec<_> = (queries.plan.files.iter())
            .map(|(file, cx, _)| (*file, cx))
            .collect();
        crate::package::diagnostic_guidance::append(
            &files,
            &queries.sources,
            &queries.declarations,
            &mut diagnostics,
        );
        let action = hirsel_artifact_store::Action(ContentHash::of_parts(
            "hirsel.front.check.action.v3",
            std::iter::once(queries.query.address.0.0.as_slice())
                .chain(std::iter::once(
                    incomplete
                        .map_or("complete", QueryKind::phase_name)
                        .as_bytes(),
                ))
                .chain(
                    queries
                        .plan
                        .files
                        .iter()
                        .filter_map(|(file, _, _)| queries.sources.file(*file))
                        .map(|file| file.text().as_bytes()),
                )
                .chain(self.front.iter().map(|(_, p)| p.action.0.0.as_slice()))
                .chain(self.lowered.iter().map(|(_, p)| p.action.0.0.as_slice()))
                .chain(
                    self.failed_front
                        .iter()
                        .map(|(_, p)| p.output.0.0.as_slice()),
                )
                .chain(
                    self.failed_lowered
                        .iter()
                        .map(|(_, p)| p.output.0.0.as_slice()),
                )
                .chain(std::iter::once(
                    hirsel_engine_store::payload_reads_hash(&reads).0.as_slice(),
                )),
        ));
        let checked = CheckOutput {
            completion,
            sources: queries.sources.clone(),
            diagnostics,
            unsafe_ffi: queries.plan.unsafe_ffi.clone(),
        };
        let data = Arc::new(Data {
            checked,
            front: std::mem::take(&mut self.front),
            lowered: std::mem::take(&mut self.lowered),
            failed_front: std::mem::take(&mut self.failed_front),
            failed_lowered: std::mem::take(&mut self.failed_lowered),
            reads: ReaderReceipt::capture(&*queries.store, &reads, queries.weight.scratch)
                .ok_or(queries.errors.resource)?,
            store: queries.store.clone(),
        });
        let product = ProductRef::live(
            queries.query.address,
            action,
            Arc::new(super::live_record::LiveRecord {
                value: data.clone(),
                domain: OUTPUT,
                records: queries.records.clone(),
                source: queries.anchors.clone(),
                serialization_working: data
                    .reads
                    .decoding_bytes()
                    .and_then(|bytes| bytes.checked_add(reads.encoding_working_bytes()?))
                    .ok_or(queries.errors.resource)?,
                _admission: queries._allowance.clone(),
            }),
        );
        *queries.completed.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((product.clone(), data));
        Ok(product)
    }
}
impl QueryEngine {
    /// Size ordered Check rows and LANG-8 facts from existing source metadata.
    /// Fact aliases and lint rows share the original snapshot reservation with
    /// the other producers; they are fixed inputs, not output slack.
    pub fn front_check_input_bytes(
        &self,
        files: &[(FileId, FileContext)],
        selected: &[DefPath],
        lint_package: Option<&hirsel_syntax::PackageId>,
        weight: MemoryWeight,
    ) -> Option<u64> {
        let mut definitions = BTreeSet::new();
        let mut lint = DeadArmLint {
            declarations: BTreeMap::new(),
            bodies: Vec::new(),
        };
        for (file, context) in files {
            for item in self.source_items(*file) {
                if lint_package == Some(&context.package) {
                    lint.insert(&item);
                }
                if item.facts.fact {
                    definitions.insert(hirsel_check_protocol::fact::fact_path(&item.definition));
                }
                definitions.insert(item.definition);
            }
        }
        // Match the instance table captured by front_lower_queries. Generated
        // instance sources are not members of the original package file list.
        let selected: BTreeSet<_> = selected.iter().map(DefId::intern).collect();
        for declaration in &selected {
            if !self.instance_bodied(declaration.path()) {
                continue;
            }
            if let Some(instances) = self.instance_bodies.instances.get(declaration) {
                definitions.extend(
                    instances
                        .values()
                        .filter(|instance| selected.contains(&instance.definition))
                        .map(|instance| instance.definition.path().clone()),
                );
            }
        }
        let definitions: Vec<_> = definitions.into_iter().collect();
        FrontCheckQueries::input_bytes(&definitions, files.len(), weight)?.checked_add(
            if lint.declarations.is_empty() {
                0
            } else {
                lint.bytes()
            },
        )
    }
    /// Capture the ordered file inputs. S0 uses the existing index/association
    /// checks; no body, constant, lower or old phase controller executes here.
    pub fn front_check_queries(
        &mut self,
        plan: FrontCheckPlan,
        weight: MemoryWeight,
        allowance: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
    ) -> Result<Arc<FrontCheckQueries>, QueryError> {
        let capture = plan.front.namespace().clone();
        capture.require_environment(self)?;
        capture.require_consistent(plan.lower.namespace())?;
        let refusal = || {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "ordered check admission".into(),
                message: "source check plan exceeds admission or lacks a required producer".into(),
            })
        };
        let mut files = Vec::new();
        let mut constants = BTreeSet::new();
        let mut lint = DeadArmLint {
            declarations: BTreeMap::new(),
            bodies: Vec::new(),
        };
        for (file, context, lowering) in &plan.files {
            if !capture.file_holds(&self.sources, *file) {
                return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "namespace capture".into(),
                    message: "Check file differs from namespace capture".into(),
                }));
            }

            let mut input = FileInput {
                paths: Vec::new(),
                lowering: *lowering,
                diagnostics: Vec::new(),
                error: None,
                phase: None,
            };
            let indexed = self
                .index_source(*file, context)
                .and_then(|()| crate::tools::association_diagnostics(self, *file, context));
            let phase = self.unavailable.or(self.last_phase);
            match indexed {
                Ok(diagnostics) => {
                    input.diagnostics = diagnostics;
                    if !input.diagnostics.is_empty() {
                        input.phase = phase;
                    }
                }
                Err(QueryError::Diagnostics(diagnostics)) => {
                    input.diagnostics = diagnostics;
                    input.phase = phase;
                }
                Err(error) => {
                    input.error = Some(error);
                    input.phase = phase;
                }
            }
            if input.error.is_none() && !has_errors(&input.diagnostics) {
                let linted = plan.lint_package.as_ref() == Some(&context.package);
                for item in self.source_items(*file) {
                    if linted {
                        lint.insert(&item);
                    }
                    if item.facts.body_kind == BodyKind::Constant {
                        constants.insert(DefId::intern(&item.definition));
                    }
                    if plan.front.query(&item.definition).is_none() {
                        return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                            operation: "ordered check admission".into(),
                            message: format!(
                                "source Check has no front producer for {}",
                                item.definition
                            ),
                        }));
                    }
                    input
                        .paths
                        .push((item.definition.clone(), item.facts.executable));
                    if item.facts.fact {
                        input.paths.push((
                            hirsel_check_protocol::fact::fact_path(&item.definition),
                            true,
                        ));
                    }
                }
            }
            files.push(input);
        }
        let mut instances: Vec<_> = plan.lower.instance_definitions().cloned().collect();
        instances.sort();
        instances.dedup();
        for path in &instances {
            if plan.front.query(path).is_none() || plan.lower.query(path).is_none() {
                return Err(refusal());
            }
        }
        // This also reserves the static instance inventory and the one saved
        // per-file selection cursor. The selection walk uses root scratch.
        let definitions: BTreeSet<_> = files
            .iter()
            .flat_map(|file| file.paths.iter().map(|(path, _)| path.clone()))
            .chain(instances.iter().cloned())
            .collect();
        let definitions: Vec<_> = definitions.into_iter().collect();
        let lint = (!lint.declarations.is_empty()).then_some(lint);
        let required = FrontCheckQueries::input_bytes(&definitions, files.len(), weight)
            .and_then(|required| required.checked_add(lint.as_ref().map_or(0, DeadArmLint::bytes)))
            .ok_or_else(refusal)?;
        if required > allowance.bytes() {
            return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "ordered check admission".into(),
                message: format!(
                    "source Check requires {required} bytes, admitted {}",
                    allowance.bytes()
                ),
            }));
        }
        let mut file_identity = Vec::new();
        if let Some(package) = &plan.lint_package {
            file_identity.extend(
                ContentHash::of_parts(
                    "hirsel.front.check.lint-package.v1",
                    [package.name.as_bytes(), package.version.as_bytes()],
                )
                .0,
            );
        }
        for file in &files {
            file_identity.push(file.lowering as u8);
            for (path, _) in &file.paths {
                file_identity.extend(path.fingerprint().0);
            }
        }
        for instance in &instances {
            file_identity.extend(instance.fingerprint().0);
        }
        let query = QueryId {
            kind: FRONT_CHECK_KIND,
            address: hirsel_artifact_store::Address(ContentHash::of_parts(
                "hirsel.front.check.address.v2",
                [
                    self.environment_value().0.as_slice(),
                    file_identity.as_slice(),
                ],
            )),
        };
        let store = self.result_store.clone().ok_or_else(refusal)?;
        Ok(Arc::new(FrontCheckQueries {
            query,
            files,
            plan,
            storage: None,
            lint,
            constants,
            instances,
            sources: capture.sources.clone(),
            declarations: capture.declarations.clone(),
            records: crate::NativeRecords::new(self.types.clone(), store.clone()),
            anchors: (*capture.anchors).clone(),
            store,
            completed: Mutex::new(None),
            failures: Mutex::new(BTreeMap::new()),
            weight,
            errors,
            _allowance: Arc::new(allowance),
        }))
    }
}
