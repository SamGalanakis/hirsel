//! Dependency admission and ordered publication of one complete source Check.
//! One bounded stream admits and publishes source roots independently of SCC
//! acceptance. Operational stops fence effectful successors.
use super::read_union::{ReaderSequence, ReaderSource};
use super::*;
use hirsel_engine_runtime::*;
use hirsel_engine_store::{Domain, Reference, ResultStore};
use std::sync::Mutex;
mod admission;
mod products;
pub use products::FrontCheckedInput;
mod scope;
pub use scope::FrontCheckScope;
pub const FRONT_CHECK_KIND: u32 = 0x4600_000a;
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
    bodies: BTreeSet<DefPath>,
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
            self.bodies.insert(item.definition.clone());
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
        weight.tree::<DefPath, ()>(self.bodies.len());
        for path in &self.bodies {
            weight.path(path);
        }
        weight.bytes()
    }
}
#[derive(Clone)]
struct RootInput {
    path: DefPath,
    executable: bool,
    ordered_front: bool,
}
#[derive(Clone)]
struct FileInput {
    part: hirsel_intern::attribution::Node,
    paths: Vec<RootInput>,
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
    reads: Arc<Reads>,
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
    reads: Arc<Reads>,
    observations: ReaderSequence,
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
            &*self.reads,
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
        let path_slot = (std::mem::size_of::<RootInput>()
            + std::mem::size_of::<(DefPath, ProductRef)>()
            + std::mem::size_of::<(DefPath, FrontObservation)>()
            + 2 * std::mem::size_of::<(DefPath, Reference)>()
            + std::mem::size_of::<DefId>()
            + 2 * std::mem::size_of::<DefPath>()
            + 128) as u64;
        const {
            assert!(size_of::<File>() <= size_of::<FileInput>() + 128);
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
                        .then(|| self.plan.lower.failure_phase(*reference))
                        .flatten()
                        .map(|phase| phase.phase_name().to_owned())
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
            reads: data.reads.clone(),
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
    pub(crate) fn accepted_product(&self) -> Option<ProductRef> {
        let product = self
            .completed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()?
            .0
            .clone();
        (self.accepted(product.clone()) == Some(true)).then_some(product)
    }
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
        if id != self.0.query {
            return Err(self.0.protocol_refusal());
        }
        Ok(Box::new(Task {
            queries: self.0.clone(),
            files: self.0.files.iter().cloned().map(File::new).collect(),
            publication: Publication::roots(0),
            reads: ReaderSequence::default(),
            front: Vec::new(),
            failed_front: Vec::new(),
            failed_lowered: Vec::new(),
            lint_uses: BTreeMap::new(),
        }))
    }
}
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum DiagnosticRoot {
    Front(DefId),
    Lower(u32, ContentHash),
}
struct File {
    input: FileInput,
    diagnostics: Vec<Diagnostic>,
    error: Option<QueryError>,
    root_diagnostics: BTreeMap<DiagnosticRoot, Vec<Diagnostic>>,
    lowered: Vec<(DefPath, ProductRef)>,
    source_rules: bool,
}
impl File {
    fn new(input: FileInput) -> Self {
        Self {
            diagnostics: input.diagnostics.clone(),
            error: input.error.clone(),
            input,
            root_diagnostics: BTreeMap::new(),
            lowered: Vec::new(),
            source_rules: false,
        }
    }
    fn clean(&self) -> bool {
        self.error.is_none()
            && !has_errors(&self.diagnostics)
            && !self
                .root_diagnostics
                .values()
                .any(|diagnostics| has_errors(diagnostics))
    }
    fn error(&mut self, error: QueryError, phase: Option<QueryKind>) {
        match error {
            QueryError::Diagnostics(diagnostics) => self.diagnostics.extend(diagnostics),
            other if self.error.is_none() => {
                self.error = Some(other);
                self.input.phase = phase;
            }
            _ => {}
        }
    }
}
// The cursor owns the selected inventory from admission through publication.
// No phase has an optional payload or a separate cross-phase reset protocol.
enum Publication {
    Roots {
        file: usize,
        root: usize,
    },
    SelectStaged {
        file: usize,
    },
    RuntimeStaged {
        file: usize,
        selections: Vec<super::scheduled_lower::staged_runtime::Selection>,
        next: usize,
    },
    Growth,
    Storage,
    Done,
    Finished(Result<ProductRef, Reference>),
}
impl Publication {
    /// The file whose part of the check this step does, if any.
    fn file(&self) -> Option<usize> {
        match *self {
            Self::Roots { file, .. }
            | Self::SelectStaged { file }
            | Self::RuntimeStaged { file, .. } => Some(file),
            _ => None,
        }
    }
    fn roots(file: usize) -> Self {
        Self::Roots { file, root: 0 }
    }
}
struct Task {
    queries: Arc<FrontCheckQueries>,
    files: Vec<File>,
    publication: Publication,
    reads: ReaderSequence,
    front: Vec<(DefPath, FrontObservation)>,
    failed_front: Vec<(DefPath, Reference)>,
    failed_lowered: Vec<(DefPath, Reference)>,
    lint_uses: BTreeMap<DefPath, Option<crate::tools::DeadArmUse>>,
}
impl QueryTask for Task {
    fn id(&self) -> QueryId {
        self.queries.query
    }
    fn weight(&self) -> MemoryWeight {
        self.queries.weight
    }
    fn run(&mut self, cx: &mut dyn QueryContext) -> QueryOutcome {
        if let Publication::Finished(result) = &self.publication {
            return result.clone();
        }
        let result = self.advance(cx).and_then(|()| self.complete());
        self.publication = Publication::Finished(result.clone());
        result
    }
}
impl Task {
    fn advance(&mut self, cx: &mut dyn QueryContext) -> Result<(), Reference> {
        loop {
            // Each file's part of the check is its own traced work, so its
            // store rows are found by reachability from that part.
            let _part = self
                .publication
                .file()
                .and_then(|file| self.files.get(file))
                .map(|file| hirsel_intern::attribution::run_as(check_part(file)));
            match &mut self.publication {
                Publication::Roots { file, root } => {
                    let file_index = *file;
                    let Some(file) = self.files.get_mut(file_index) else {
                        self.publication = Publication::Growth;
                        continue;
                    };
                    if file.input.error.is_some() || has_errors(&file.input.diagnostics) {
                        self.publication = Publication::roots(file_index + 1);
                        continue;
                    }
                    if *root == file.input.paths.len() {
                        self.publication = Publication::SelectStaged { file: file_index };
                        continue;
                    }
                    let read = admission::read_root(
                        &self.queries,
                        cx,
                        &file.input,
                        *root,
                        file.error.is_some(),
                    )?;
                    let path = file.input.paths[*root].path.clone();
                    *root += 1;
                    self.publish_root(cx, file_index, path, read)?;
                }
                Publication::SelectStaged { file } => {
                    let index = *file;
                    if !self.files[index].clean() {
                        self.publication = Publication::roots(index + 1);
                        continue;
                    }
                    let roots = self.files[index]
                        .input
                        .paths
                        .iter()
                        .map(|work| work.path.clone());
                    match self.queries.plan.lower.runtime_staged_queries(
                        roots,
                        self.queries.weight.scratch,
                        self.queries.weight.retained,
                        cx,
                    ) {
                        Ok(selections) => {
                            self.publication = Publication::RuntimeStaged {
                                file: index,
                                selections,
                                next: 0,
                            }
                        }
                        Err(error) => {
                            self.files[index].error(error, Some(QueryKind::Lower));
                            self.publication = Publication::roots(index + 1);
                        }
                    }
                }
                Publication::RuntimeStaged {
                    file,
                    selections,
                    next,
                } => {
                    let index = *file;
                    if !self.files[index].clean() || *next == selections.len() {
                        self.publication = Publication::roots(index + 1);
                        continue;
                    }
                    let (id, site) = selections[*next];
                    let facts = self.queries.plan.lower.poll_check(cx, id)?;
                    self.reads.record(
                        facts.reads.clone(),
                        Some(ReaderSource::StagedLower(facts.query)),
                    );
                    if let Err((reference, _)) = facts.result {
                        self.files[index].error(
                            self.queries
                                .plan
                                .lower
                                .staged_failure(id, reference, site)
                                .ok_or(reference)?,
                            Some(QueryKind::Lower),
                        );
                    }
                    *next += 1;
                }
                Publication::Growth => {
                    // SCC acceptance cannot admit an effectful suffix that the
                    // stream withheld. It never gates ordinary body scheduling.
                    if self.files.iter().any(File::clean)
                        && self.files.iter().all(|file| file.error.is_none())
                    {
                        let id = self.queries.plan.growth.query();
                        let selected = cx.read(id);
                        match selected {
                            Ok(product) => {
                                let output = self
                                    .queries
                                    .plan
                                    .growth
                                    .outcome(product)
                                    .ok_or_else(|| self.queries.protocol_refusal())?;
                                self.reads.record(output.reads.clone(), None);
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
                        && self.files.iter().all(|file| file.error.is_none())
                        && self.files.iter().any(|file| file.source_rules)
                    {
                        if let Some(storage) = &self.queries.storage {
                            let id = storage.query();
                            let result = cx.read(id);
                            match result {
                                Ok(product) => {
                                    let output = storage
                                        .outcome(product)
                                        .ok_or_else(|| self.queries.protocol_refusal())?;
                                    self.reads.record(output.reads.clone(), None);
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
                    self.publication = Publication::Done;
                }
                Publication::Done | Publication::Finished(_) => return Ok(()),
            }
        }
    }
    fn publish_root(
        &mut self,
        cx: &mut dyn QueryContext,
        file_index: usize,
        path: DefPath,
        read: admission::RootRead,
    ) -> Result<(), Reference> {
        let admission::RootRead { front, lower } = read;
        if let Some(selected) = front {
            let path = &path;
            let id = self
                .queries
                .plan
                .front
                .query(path)
                .ok_or(self.queries.errors.protocol)?;
            let reference = selected.clone();
            let Some(facts) = self.queries.plan.front.check_facts(reference.clone()) else {
                return Err(reference
                    .err()
                    .unwrap_or_else(|| self.queries.protocol_refusal()));
            };
            let observed = self.reads.record(
                facts.reads.clone(),
                Some(ReaderSource::Front(DefId::intern(path))),
            );
            self.accept_front(file_index, path.clone(), selected, facts, observed)?;
            cx.consume(id);
        }
        if let Some(facts) = lower
            && self.files[file_index].error.is_none()
        {
            self.publish_lower(file_index, path, facts)?;
        }
        Ok(())
    }
    fn accept_front(
        &mut self,
        file_index: usize,
        path: DefPath,
        selected: Result<ProductRef, Reference>,
        facts: parallel::FrontCheckFacts,
        observed: Arc<Reads>,
    ) -> Result<(), Reference> {
        let file = &mut self.files[file_index];
        let path = &path;
        if self
            .queries
            .lint
            .as_ref()
            .is_some_and(|lint| lint.bodies.contains(path))
        {
            self.lint_uses.insert(path.clone(), facts.dead_arms);
        }
        file.source_rules |= facts.source_rules;
        match facts.result {
            Ok(diagnostics) => {
                file.root_diagnostics
                    .insert(DiagnosticRoot::Front(DefId::intern(path)), diagnostics);
                if let Ok(product) = &selected {
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
            }
            Err(error) => {
                self.failed_front.push((
                    path.clone(),
                    selected
                        .clone()
                        .err()
                        .unwrap_or_else(|| self.queries.protocol_refusal()),
                ));
                match error {
                    QueryError::Diagnostics(diagnostics) => {
                        file.root_diagnostics
                            .insert(DiagnosticRoot::Front(DefId::intern(path)), diagnostics);
                    }
                    other => file.error(other, facts.phase),
                }
                if file.error.is_none()
                    && let Some(phase) = facts.incomplete
                {
                    file.error(QueryError::NotImplemented, Some(phase));
                }
            }
        }
        Ok(())
    }
    fn publish_lower(
        &mut self,
        file: usize,
        path: DefPath,
        facts: super::scheduled_lower::check::CheckFacts,
    ) -> Result<(), Reference> {
        self.reads.record(
            facts.reads.clone(),
            Some(ReaderSource::Lower(facts.query, facts.producer.clone())),
        );
        self.accept_lower(file, path, facts);
        Ok(())
    }
    fn accept_lower(
        &mut self,
        file: usize,
        path: DefPath,
        facts: super::scheduled_lower::check::CheckFacts,
    ) {
        match facts.result {
            Ok((diagnostics, observations)) => {
                self.files[file].root_diagnostics.insert(
                    DiagnosticRoot::Lower(facts.query.kind, facts.query.address.0),
                    diagnostics,
                );
                self.files[file].lowered.push((path, observations));
            }
            Err((reference, QueryError::Diagnostics(diagnostics))) => {
                self.failed_lowered.push((path, reference));
                self.files[file].root_diagnostics.insert(
                    DiagnosticRoot::Lower(facts.query.kind, facts.query.address.0),
                    diagnostics,
                );
            }
            Err((reference, error)) => {
                self.failed_lowered.push((path, reference));
                self.files[file].error(error, facts.phase);
            }
        }
    }
    fn complete(&mut self) -> Result<ProductRef, Reference> {
        let queries = &self.queries;
        let reads = self.reads.finish().map_err(|error| {
            crate::requests::source_graph::protocol_failure(
                &*queries.store,
                queries.errors,
                Some(queries.query()),
                &format!("Check observations are inconsistent: {error:?}"),
            )
        })?;
        // The union holds every Front and Lower reader this Check consumed.
        if let Some(escape) = queries.plan.front.reach_escape(&reads) {
            return Err(crate::requests::source_graph::protocol_failure(
                &*queries.store,
                queries.errors,
                Some(queries.query()),
                &escape,
            ));
        }
        let stopped = self.files.iter().find_map(|file| match &file.error {
            Some(error) if *error != QueryError::NotImplemented => {
                Some((error.clone(), file.input.phase))
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
            .filter_map(|file| file.input.phase)
            .last();
        let completion = incomplete.map_or(CheckCompletion::Complete, |phase| {
            CheckCompletion::Incomplete { phase }
        });
        let mut diagnostics: Vec<_> = self
            .files
            .iter()
            .flat_map(|file| {
                file.diagnostics
                    .iter()
                    .chain(file.root_diagnostics.values().flatten())
                    .cloned()
            })
            .collect();
        // The root package's dead error arms (LANG-8) warn only after a clean
        // complete check; a body without settled facts leaves it unlinted.
        if completion == CheckCompletion::Complete && !has_errors(&diagnostics) {
            if let Some(lint) = &queries.lint
                && self.lint_uses.len() == lint.bodies.len()
                && self.lint_uses.values().all(Option::is_some)
            {
                let errors = self
                    .lint_uses
                    .values()
                    .flatten()
                    .flat_map(|uses| uses.errors.iter())
                    .filter(|path| lint.declarations.contains_key(*path))
                    .cloned()
                    .collect();
                let constructed = self
                    .lint_uses
                    .values()
                    .flatten()
                    .flat_map(|uses| uses.constructed.iter())
                    .filter(|(path, arm)| {
                        lint.declarations
                            .get(path)
                            .is_some_and(|(_, arms)| (*arm as usize) < arms.len())
                    })
                    .cloned()
                    .collect();
                diagnostics.extend(super::dead_arms::dead_arm_diagnostics(
                    lint.declarations.clone(),
                    &errors,
                    &constructed,
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
        let lowered: Vec<_> = self
            .files
            .iter_mut()
            .filter(|file| file.clean())
            .flat_map(|file| std::mem::take(&mut file.lowered))
            .collect();
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
                .chain(lowered.iter().map(|(_, p)| p.action.0.0.as_slice()))
                .chain(
                    self.failed_front
                        .iter()
                        .map(|(_, p)| p.output.0.0.as_slice()),
                )
                .chain(
                    self.failed_lowered
                        .iter()
                        .map(|(_, p)| p.output.0.0.as_slice()),
                ),
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
            lowered,
            failed_front: std::mem::take(&mut self.failed_front),
            failed_lowered: std::mem::take(&mut self.failed_lowered),
            reads: Arc::new(reads),
            observations: std::mem::take(&mut self.reads),
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
                    .encoding_working_bytes()
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
        lint_package: Option<&hirsel_syntax::PackageId>,
        weight: MemoryWeight,
    ) -> Option<u64> {
        let mut definitions = BTreeSet::new();
        let mut lint = DeadArmLint {
            declarations: BTreeMap::new(),
            bodies: BTreeSet::new(),
        };
        for (file, context) in files {
            for item in self.source_items(*file) {
                if lint_package == Some(&context.package) {
                    lint.insert(&item);
                }
                definitions.insert(item.definition);
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
        request: &Arc<hirsel_engine_admission::ProcessBudget>,
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
            bodies: BTreeSet::new(),
        };
        // The modules holding a generated private body, at one declaration
        // revision: their Front roots are ordered.
        for (file, context, lowering) in &plan.files {
            if !capture.file_holds(&self.sources, *file) {
                return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "namespace capture".into(),
                    message: "Check file differs from namespace capture".into(),
                }));
            }

            let mut input = FileInput {
                part: self.file_part(*file, context),
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
                    input.paths.push(RootInput {
                        path: item.definition.clone(),
                        executable: item.facts.executable,
                        ordered_front: item.facts.specializes,
                    });
                }
            }
            files.push(input);
        }
        let definitions: BTreeSet<_> = files
            .iter()
            .flat_map(|file| file.paths.iter().map(|work| work.path.clone()))
            .collect();
        let definitions: Vec<_> = definitions.into_iter().collect();
        let lint = (!lint.declarations.is_empty()).then_some(lint);
        let required = FrontCheckQueries::input_bytes(&definitions, files.len(), weight)
            .and_then(|required| required.checked_add(lint.as_ref().map_or(0, DeadArmLint::bytes)))
            .ok_or_else(refusal)?;
        let allowance = request.reserve(required).map_err(|_| {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "ordered check admission".into(),
                message: format!(
                    "source Check requires {required} bytes, request footprint has {}",
                    request.free()
                ),
            })
        })?;
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
            for work in &file.paths {
                file_identity.extend(work.path.fingerprint().0);
            }
        }
        let query = QueryId {
            kind: FRONT_CHECK_KIND,
            address: hirsel_artifact_store::Address(ContentHash::of_parts(
                "hirsel.front.check.address.v2",
                [
                    capture.lease().identity().0.as_slice(),
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

/// The traced work of one file's part of the check. Its kind is no query's.
fn check_part(file: &File) -> hirsel_intern::attribution::Node {
    file.input.part
}
impl QueryEngine {
    /// The check part of one source file, named by its package and logical
    /// path, so indexing it and checking it are the same part in every order.
    pub(crate) fn file_part(
        &self,
        file: FileId,
        context: &FileContext,
    ) -> hirsel_intern::attribution::Node {
        let mut name = format!("{:?}\0", context.package).into_bytes();
        if let Some(source) = self.sources.file(file) {
            name.extend(source.logical_path().as_os_str().as_encoded_bytes());
        }
        (
            hirsel_intern::attribution::PART,
            hirsel_syntax::ContentHash::of_bytes(&name).0,
        )
    }
}
