//! The per-definition query memos (design §1b). Each holds its value, the
//! adapter-level detail of what it read, and its typed `Reads`. Indexing
//! diffs each definition's inputs; every memo that read a changed input is
//! forgotten and recomputed on demand.
use super::*;
use crate::memo::{Changes, Memo, MemoKey};
use hirsel_query_products::reads::{
    LookupKey, LookupSource, ModuleKey, PackageKey, ReadKey, Reads, ReadsBuilder,
};
use hirsel_syntax::DefId;
use hirsel_types::LookupError;

/// One query's memo.
pub(crate) struct Entry<T> {
    pub memo: Memo<QueryOutput<T>>,
    /// What the adapter read, as it reported it (diagnostic tooling and
    /// the program revision read it; validity never does).
    pub detail: DependencyReads,
    pub products: Arc<PhaseProducts>,
    /// A signature's fingerprint coverage: the adapter's own dependency list,
    /// before the query's reads join it in the output (PS-9).
    pub covered: Vec<SignatureDependency>,
    /// A generator evaluation's depth evidence, or a lowering's transparent
    /// dependency-request trace (design §1c).
    pub depth: Option<hirsel_query_products::depth::DepthEvidence<Span>>,
    pub acceptance: Option<hirsel_validate::ExecutionReceipt>,
    pub verification: std::sync::Mutex<Option<Arc<hirsel_validate::VerifiedDefinition>>>,
    pub selections: Vec<crate::comptime::Selection>,
    /// False when an input changed during the evaluation: the result is
    /// returned but never kept.
    pub consistent: bool,
}
impl<T> Entry<T> {
    pub fn result(&self) -> Result<Arc<QueryOutput<T>>, QueryError> {
        self.memo.value.clone()
    }
    pub fn reads(&self) -> &Reads {
        &self.memo.reads
    }
}
impl<T: Clone> Entry<T> {
    pub fn output(&self) -> Result<QueryOutput<T>, QueryError> {
        self.result().map(|o| (*o).clone())
    }
}

/// A query kind's memos by definition.
pub(crate) type Table<T> = hirsel_syntax::DefMap<Arc<Entry<T>>>;

/// What one evaluation observed beyond the adapter's detail: the private
/// representations it read, and the complete reads it inherits (the query it
/// refines) or merged by executing something.
#[derive(Default)]
pub(crate) struct Observed {
    pub products: Arc<PhaseProducts>,
    pub representations: BTreeMap<DefPath, ContentHash>,
    pub merged: ReadsBuilder,
}
impl Observed {
    pub fn inheriting(reads: Option<&Reads>) -> Self {
        let mut merged = ReadsBuilder::new();
        if let Some(reads) = reads {
            merged.merge(reads);
        }
        Self {
            products: Default::default(),
            representations: BTreeMap::new(),
            merged,
        }
    }
}

/// The inputs of one definition that indexing diffs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DefInputs {
    body: ContentHash,
    header: ContentHash,
    candidates: ContentHash,
    anchors: Vec<hirsel_span::Anchor>,
    names: ContentHash,
}
impl DefInputs {
    pub(super) fn retained_bytes(&self) -> usize {
        self.anchors
            .capacity()
            .saturating_mul(size_of::<hirsel_span::Anchor>())
    }
    pub fn header(&self) -> ContentHash {
        self.header
    }
}

fn tag(domain: &str, parts: &[&[u8]]) -> ContentHash {
    ContentHash::of_parts(domain, parts.iter().copied())
}
/// The canonical absence of a declaration-candidate row.
pub(crate) fn no_candidates() -> ContentHash {
    tag("hirsel.read.no-candidates", &[])
}
/// A failed signature product's identity: its own text and its deterministic
/// diagnostic result. Its dependencies are validated by the failed product
/// itself; a caller binds only what it consumed, the failure.
///
/// Anchors are engine-local handles, so the result is hashed in its portable
/// form: every span keeps its anchor-relative offsets and drops the handle.
pub(crate) fn failed_signature_identity<T>(
    text: ContentHash,
    value: &Result<T, hirsel_types::QueryError>,
) -> Option<ContentHash> {
    let Err(hirsel_types::QueryError::Diagnostics(diagnostics)) = value else {
        return None;
    };
    let mut portable = diagnostics.clone();
    for diagnostic in &mut portable {
        let unanchor = |span: &mut hirsel_span::Span| span.anchor = hirsel_span::Anchor::default();
        unanchor(&mut diagnostic.primary);
        for label in &mut diagnostic.secondary {
            unanchor(&mut label.span);
        }
        if let Some(fix) = &mut diagnostic.fix {
            unanchor(&mut fix.span);
        }
        for obligation in &mut diagnostic.obligations {
            match obligation {
                hirsel_diag::Obligation::GenericBound { arguments } => {
                    arguments.iter_mut().for_each(unanchor)
                }
                hirsel_diag::Obligation::ParameterMode { argument } => unanchor(argument),
                hirsel_diag::Obligation::Multiplicity | hirsel_diag::Obligation::CopyProof => (),
            }
        }
    }
    let diagnostics = bincode::serialize(&portable).ok()?;
    Some(ContentHash::of_parts(
        "hirsel.failed-signature.v2",
        [text.0.as_slice(), diagnostics.as_slice()],
    ))
}
/// The observed value of an optional hash.
pub(crate) fn optional(hash: Option<ContentHash>) -> ContentHash {
    match hash {
        Some(h) => tag("hirsel.read.some", &[&h.0]),
        None => tag("hirsel.read.none", &[]),
    }
}
/// A declaration read: the signature's own structure, without its dependencies.
pub(crate) fn declared_signature_value(
    types: &hirsel_types::TypeInterner,
    signature: &hirsel_types::Signature,
) -> Option<ContentHash> {
    hirsel_types::encoding::fingerprint(types, signature, &[]).ok()
}
/// The observed value of a signature read: its fingerprint, or failure.
pub(crate) fn signature_value(result: Option<ContentHash>) -> ContentHash {
    result.unwrap_or_else(|| tag("hirsel.read.failed-signature", &[]))
}
fn diagnostics_value(parts: &mut Vec<Vec<u8>>, diagnostics: &[Diagnostic]) {
    for d in diagnostics {
        parts.push(d.rule.as_str().as_bytes().to_vec());
        parts.push(d.message.as_bytes().to_vec());
    }
}
/// The observed value of a provider lookup; a frozen provider records the same.
pub(crate) fn provider_value<P: std::borrow::Borrow<DefPath>>(
    result: &Result<P, QueryError>,
) -> ContentHash {
    let mut parts = vec![];
    match result {
        Ok(path) => parts.push(path.borrow().fingerprint().0.to_vec()),
        Err(QueryError::NotImplemented) => parts.push(b"absent".to_vec()),
        Err(QueryError::Operational(f)) => {
            parts.push(b"operational".to_vec());
            parts.push(f.message.as_bytes().to_vec());
        }
        Err(QueryError::Diagnostics(ds)) => {
            parts.push(b"diagnostics".to_vec());
            diagnostics_value(&mut parts, ds);
        }
    }
    ContentHash::of_parts(
        "hirsel.read.provider-lookup",
        parts.iter().map(Vec::as_slice),
    )
}
fn missing_lookup<P>(result: &Result<P, LookupError>) -> bool {
    matches!(
        result,
        Err(LookupError::Missing | LookupError::MissingMember(_))
    )
}
pub(crate) fn manifest_value<P: std::borrow::Borrow<DefPath>>(
    result: &Result<P, LookupError>,
    namespace: Option<hirsel_resolve::Namespace>,
) -> ContentHash {
    let mut parts: Vec<Vec<u8>> = vec![];
    let paths = |parts: &mut Vec<Vec<u8>>, ps: &[DefPath]| {
        for p in ps {
            parts.push(p.fingerprint().0.to_vec());
        }
    };
    match result {
        Ok(path) => {
            parts.push(b"ok".to_vec());
            parts.push(path.borrow().fingerprint().0.to_vec());
            parts.push(format!("{namespace:?}").into_bytes());
        }
        Err(LookupError::Missing) => parts.push(b"missing".to_vec()),
        Err(LookupError::MissingMember(ps)) => {
            parts.push(b"member".to_vec());
            paths(&mut parts, ps);
        }
        Err(LookupError::Ambiguous(ps)) => {
            parts.push(b"ambiguous".to_vec());
            paths(&mut parts, ps);
        }
        Err(LookupError::Invisible(p)) => {
            parts.push(b"invisible".to_vec());
            paths(&mut parts, std::slice::from_ref(p));
        }
        Err(LookupError::Provider(ds)) => {
            parts.push(b"provider".to_vec());
            diagnostics_value(&mut parts, ds);
        }
    }
    ContentHash::of_parts(
        "hirsel.read.manifest-lookup",
        parts.iter().map(Vec::as_slice),
    )
}

/// Source-dependent inputs to canonical read finishing. Frozen body tasks
/// supply the same namespaces, citation text, candidate and environment values
/// as the engine, without consulting or mutating a session during execution.
pub(crate) trait ReadSource {
    fn namespace(
        &mut self,
        context: &FileContext,
        result: &Result<hirsel_types::ReadPath, LookupError>,
    ) -> Option<hirsel_resolve::Namespace>;
    fn lookup_candidates(
        &mut self,
        context: &FileContext,
        package: Option<&hirsel_syntax::PackageId>,
        segments: &[String],
    ) -> ContentHash;
    fn cited(&self, path: &DefPath) -> ContentHash;
    fn candidates(&self, owner: &DefPath) -> Option<ContentHash>;
    fn environment(&self) -> ContentHash;
}

/// One canonical encoding for scalar queries and frozen tasks, including
/// inherited reads and all failure receipts. The source only selects input
/// values; the ordering, domains and conflict checks stay here.
pub(crate) fn complete_reads(
    owner: &DefPath,
    detail: &DependencyReads,
    observed: &Observed,
    source: &mut impl ReadSource,
) -> Result<Reads, hirsel_query_products::reads::Inconsistent> {
    let mut reads = observed.merged.clone();
    let def = DefId::intern;
    for s in &detail.signatures {
        reads.record(
            ReadKey::Signature(s.definition),
            signature_value(Some(s.fingerprint)),
        );
    }
    for (path, key) in &detail.failed_signature_keys {
        reads.record(ReadKey::FailedSignature(def(path)), optional(*key));
    }
    // Resolver reads are interned, so a repeated lookup is the same
    // allocation and records the same row; each row depends only on its read.
    let mut seen = std::collections::HashSet::<usize, hirsel_intern::IdHasher>::default();
    let mut contexts = Vec::<(usize, (PackageKey, ModuleKey))>::new();
    let mut keys = |context: &FileContext| {
        let at = std::ptr::from_ref(context) as usize;
        match contexts.iter().find(|(seen, _)| *seen == at) {
            Some((_, keys)) => *keys,
            None => {
                let keys = LookupKey::context_keys(context);
                contexts.push((at, keys));
                keys
            }
        }
    };
    // Only the resolver product that emits a missing-name diagnostic
    // observes spelling repairs. Later phases inherit its typed reads;
    // replaying resolver detail must not add new candidate dependencies to
    // successful reservation probes.
    let diagnostics = observed
        .products
        .resolution
        .as_ref()
        .is_some_and(|resolution| !resolution.output.diagnostics.is_empty());
    for read in detail.manifest_lookups.iter().chain(detail.resolver.iter()) {
        let read: &hirsel_types::DependencyRead = read.as_ref();
        if !seen.insert(std::ptr::from_ref(read) as usize) {
            continue;
        }
        match read {
            hirsel_types::DependencyRead::ProviderLookup {
                context,
                package,
                segments,
                output,
            } => reads.record(
                ReadKey::Lookup(LookupKey::in_context(
                    LookupSource::Provider,
                    keys(context),
                    package.as_ref(),
                    segments,
                )),
                provider_value(&output.result),
            ),
            hirsel_types::DependencyRead::Lookup {
                context,
                package,
                segments,
                result,
                ..
            } => {
                let namespace = source.namespace(context, result);
                let context_keys = keys(context);
                if diagnostics && missing_lookup(result) {
                    reads.record(
                        ReadKey::Lookup(LookupKey::in_context(
                            LookupSource::Candidates,
                            context_keys,
                            package.as_ref(),
                            segments,
                        )),
                        source.lookup_candidates(context, package.as_ref(), segments),
                    );
                }
                reads.record(
                    ReadKey::Lookup(LookupKey::in_context(
                        LookupSource::Manifest,
                        context_keys,
                        package.as_ref(),
                        segments,
                    )),
                    manifest_value(result, namespace),
                )
            }
            hirsel_types::DependencyRead::ModuleHead {
                context,
                head,
                exposed,
                ..
            } => reads.record(
                ReadKey::Lookup(LookupKey::in_context(
                    LookupSource::Head,
                    keys(context),
                    None,
                    std::slice::from_ref(head),
                )),
                tag("hirsel.read.head", &[&[u8::from(*exposed)]]),
            ),
            hirsel_types::DependencyRead::Signature {
                definition,
                fingerprint,
            } => reads.record(
                ReadKey::Signature(*definition),
                signature_value(*fingerprint),
            ),
        }
    }
    for (path, _) in &detail.citations {
        reads.record(ReadKey::CitedText(def(path)), source.cited(path));
    }
    // A recursive group's members are read by their text.
    for (path, revision) in &observed.representations {
        reads.record(ReadKey::ExecutedBody(def(path)), optional(Some(*revision)));
    }
    let owner_id = def(owner);
    if let Some(candidates) = source.candidates(owner) {
        reads.record(ReadKey::DeclarationCandidates(owner_id), candidates);
    }
    reads.record(ReadKey::Environment, source.environment());
    reads.finish()
}

impl ReadSource for QueryEngine {
    fn namespace(
        &mut self,
        context: &FileContext,
        result: &Result<hirsel_types::ReadPath, LookupError>,
    ) -> Option<hirsel_resolve::Namespace> {
        self.lookup_namespace(context, result)
    }
    fn lookup_candidates(
        &mut self,
        context: &FileContext,
        package: Option<&hirsel_syntax::PackageId>,
        segments: &[String],
    ) -> ContentHash {
        self.declarations
            .frozen()
            .index(context.package.clone())
            .lookup_candidates_revision(context, package, segments)
    }
    fn cited(&self, path: &DefPath) -> ContentHash {
        self.cited_value(path)
    }
    fn candidates(&self, owner: &DefPath) -> Option<ContentHash> {
        self.declared(owner).then(|| self.candidates_value(owner))
    }
    fn environment(&self) -> ContentHash {
        self.environment_value()
    }
}

impl QueryEngine {
    /// The session's compiler and explicit build inputs. Package manifests
    /// describe discovery; their semantic dependencies are reads.
    /// Include the comptime limits and charging version
    /// (CT-4): a budget or charging change re-evaluates every evaluation. One value per session,
    /// so the reads of different packages' memos merge without conflict.
    pub(crate) fn environment_value(&self) -> ContentHash {
        let e = &self.environment;
        tag(
            "hirsel.read.environment.v2",
            &[
                &e.interface_version.to_le_bytes(),
                e.compiler_version.as_bytes(),
                &e.spec_hash.0,
                &e.builtin_signatures_hash.0,
                &hirsel_tir::CHARGING_VERSION.to_le_bytes(),
                &self.comptime_limits.budget.to_le_bytes(),
                &(self.comptime_limits.call_depth as u64).to_le_bytes(),
                &optional(self.build_inputs_revision()).0,
                &[u8::from(self.verify == Verify::On)],
            ],
        )
    }
    /// The resolver's explicit build inputs (exposures, metadata): the
    /// manifest revision when the caller set it rather than indexing. Indexing
    /// republishes it from the declarations, whose own changes are reads.
    fn build_inputs_revision(&self) -> Option<ContentHash> {
        let revision = self.declarations.inputs().manifest_revision;
        (Some(revision) != self.published_manifest).then_some(revision)
    }
    /// H(count, then (file path, header hash) for each candidate in occurrence order).
    /// An indexed definition's is its row's; a generated instance, which is
    /// never indexed, hashes its own items the same way.
    pub(crate) fn candidates_value(&self, path: &DefPath) -> ContentHash {
        if let Some(candidates) = self.declarations.candidates(path) {
            return candidates;
        }
        let Some(items) = self.items.get(path) else {
            return self
                .header_products
                .as_ref()
                .and_then(|headers| headers.declaration_candidates(DefId::intern(path)))
                .unwrap_or_else(no_candidates);
        };
        let mut parts: Vec<Vec<u8>> = vec![(items.len() as u64).to_le_bytes().to_vec()];
        for item in items {
            parts.push(crate::snapshot::logical(&self.sources, item));
            parts.push(self.header_hash(item).0.to_vec());
        }
        ContentHash::of_parts(
            "hirsel.declaration-candidates.v1",
            parts.iter().map(Vec::as_slice),
        )
    }
    /// Every candidate's name site, never its body: which one a citation picks
    /// depends on the citing owner's file, but the observed value may not,
    /// so readers in different files merge.
    pub(crate) fn cited_value(&self, path: &DefPath) -> ContentHash {
        match self.captured_text(path) {
            Some(text) => text.cited,
            None => self.citation_source().cited_text(DefId::intern(path)),
        }
    }
    pub(crate) fn failed_key_value(&self, path: &DefPath) -> ContentHash {
        optional(self.failed_signature_key(path))
    }

    /// The typed reads of one evaluation by `owner`.
    pub(crate) fn typed_reads(
        &mut self,
        owner: &DefPath,
        detail: &DependencyReads,
        observed: &Observed,
    ) -> Result<Reads, hirsel_query_products::reads::Inconsistent> {
        complete_reads(owner, detail, observed, self)
    }

    fn lookup_namespace<P: std::borrow::Borrow<DefPath>>(
        &mut self,
        context: &FileContext,
        result: &Result<P, LookupError>,
    ) -> Option<hirsel_resolve::Namespace> {
        let path = result.as_ref().ok()?;
        self.manifest_index(&context.package)?
            .entry(path.borrow())
            .map(|e| e.namespace)
    }

    pub(super) fn observed_signature(&self, id: DefId) -> Option<hirsel_types::SignatureRef> {
        let (environment, limits, inputs, verify) = self.environment_seen.as_ref()?;
        if environment != &self.environment
            || *limits != self.comptime_limits
            || *inputs != self.build_inputs_revision()
            || *verify != self.verify
        {
            return None;
        }
        self.unmarked_signature(id)?.ok()
    }

    /// The value `owner` observes for `read` now.
    pub(crate) fn observe(&mut self, _owner: &DefPath, read: &ReadKey) -> Option<ContentHash> {
        Some(match *read {
            ReadKey::DeclaredSignature(d) => self
                .current_snapshot_signature(d)
                .or_else(|| self.observed_signature(d))
                .and_then(|signature| declared_signature_value(&self.types, &signature))
                .or_else(|| self.load_declared_signature_value(d.path()))
                .or_else(|| {
                    let signature = self.signature(d).ok()?;
                    declared_signature_value(&self.types, &signature)
                })?,
            ReadKey::Signature(d) | ReadKey::ExecutedSignature(d) => {
                let signature = self
                    .current_snapshot_signature(d)
                    .or_else(|| self.observed_signature(d))
                    .or_else(|| self.signature(d).ok());
                signature_value(signature.map(|s| s.fingerprint))
            }
            ReadKey::FailedSignature(d) => {
                if self.signature(d).is_ok() {
                    tag("hirsel.read.signature-ok", &[])
                } else {
                    self.failed_key_value(d.path())
                }
            }
            ReadKey::Lookup(key) => {
                let context = key.context();
                let qualifier = key.qualifier();
                let segments = key.segments();
                match key.source {
                    LookupSource::Provider => {
                        let path = hirsel_syntax::Path {
                            package: qualifier,
                            segments: segments.iter().map(|s| Symbol::intern(s)).collect(),
                        };
                        provider_value(&self.lookup(&context, &path).result)
                    }
                    LookupSource::Manifest => {
                        let result = self.lookup_result(&context, &qualifier, &segments);
                        let namespace = self.lookup_namespace(&context, &result);
                        manifest_value(&result, namespace)
                    }
                    LookupSource::Candidates => {
                        self.lookup_candidates(&context, qualifier.as_ref(), &segments)
                    }
                    LookupSource::Head => {
                        let exposed = self
                            .manifest_index(&context.package)
                            .is_some_and(|i| i.has_head(&context, &segments[0]));
                        tag("hirsel.read.head", &[&[u8::from(exposed)]])
                    }
                }
            }
            ReadKey::Representation(d) => self.representation_digest(d.path()),
            ReadKey::ExecutedBody(d) | ReadKey::LayoutBody(d) => {
                optional(self.implementation_hash(d.path()))
            }
            ReadKey::CitedText(d) => self.cited_value(d.path()),
            ReadKey::DeclarationCandidates(d) => self.candidates_value(d.path()),
            ReadKey::Environment => self.environment_value(),
            // A refused V universe observes nothing: the read does not hold.
            ReadKey::ValidationView(package, d) => self
                .signature_snapshot()
                .validation(&package.id())
                .ok()?
                .view(d)
                .ok()?
                .map_or_else(ContentHash::default, |input| input.digest().0),
            ReadKey::ValidationUniverse(package) => self
                .signature_snapshot()
                .validation_input_digest(&package.id())
                .ok()?,
            ReadKey::UniverseErrors(package) => {
                self.signature_snapshot()
                    .validation(&package.id())
                    .ok()?
                    .universe_errors()
                    .0
            }
            ReadKey::HeapRole(package) => self
                .signature_snapshot()
                .validation(&package.id())
                .ok()?
                .heap_observation()
                .ok()?
                .fingerprint(),
            ReadKey::NameSet(package, module) => {
                self.declarations.frozen().name_set(package, module)
            }
        })
    }

    /// Whether `entry` is valid now for a query whose own content is `text`:
    /// its text is current and no change it observed has forgotten it.
    pub(crate) fn memo_valid<T>(&mut self, entry: &Entry<T>, text: ContentHash) -> bool {
        if entry.memo.text != text {
            hirsel_intern::work::count(hirsel_intern::work::Counter::CacheValidationRows);
            return false;
        }
        self.note_environment();
        !entry.memo.is_stale()
    }

    /// Records a freshly computed query. Inconsistent reads (an input changed
    /// during the evaluation) make the result uncacheable.
    pub(crate) fn record<T>(
        &mut self,
        key: MemoKey,
        text: ContentHash,
        result: Result<Arc<QueryOutput<T>>, QueryError>,
        detail: DependencyReads,
        observed: &Observed,
    ) -> Entry<T> {
        if key.kind == QueryKind::Lower {
            self.front_products.forget(&key);
        }
        if key.kind == QueryKind::Check && result.is_err() {
            self.retire_after_check_rejection(key.def);
        }
        let owner = key.def.path();
        let (reads, consistent) = match self.typed_reads(owner, &detail, observed) {
            Ok(reads) => (reads, true),
            Err(_) => (Reads::empty(), false),
        };
        *self.recorded.entry(key.kind).or_default() += 1;
        let memo = Memo::new(key, text, result, Arc::new(reads));
        Entry {
            products: observed.products.clone(),
            memo,
            detail,
            covered: vec![],
            depth: None,
            acceptance: None,
            verification: Default::default(),
            selections: vec![],
            consistent,
        }
    }

    /// Check rejection stops the pipeline before its descendants can validate their
    /// reads. Their earlier successful products no longer describe this check.
    /// Retire them from the typed/aggregate tables;
    /// unrelated definitions and independently usable predecessors survive.
    pub(super) fn retire_after_check_rejection(&mut self, definition: DefId) {
        for kind in [
            QueryKind::CheckedSummary,
            QueryKind::Modes,
            QueryKind::Summary,
            QueryKind::Lower,
        ] {
            self.forget(&MemoKey::new(kind, definition));
        }
    }

    /// The inputs indexing diffs for `path`, if it is indexed.
    pub(crate) fn definition_inputs(&self, path: &DefPath) -> Option<DefInputs> {
        let items = self.items.get(path)?;
        let first = items.first()?;
        let item = &first.item;
        let public = item.visibility != hirsel_syntax::Visibility::Private;
        let kind = match &item.kind {
            ItemKind::Function { .. } => 0u8,
            ItemKind::Const { .. } => 1,
            ItemKind::Type {
                body: hirsel_syntax::TypeBody::Alias(_),
                ..
            } => 2,
            ItemKind::Type { .. } => 3,
            ItemKind::Signature { .. } => 4,
            ItemKind::Implementation { .. } => 5,
        };
        Some(DefInputs {
            body: first.text_hash,
            header: self.headers_hash(path),
            candidates: self.candidates_value(path),
            anchors: items.iter().map(|c| c.anchor).collect(),
            names: tag("hirsel.read.name", &[&[u8::from(public), kind]]),
        })
    }

    /// Diffs the inputs of `paths` against what was last indexed and forgets
    /// every memo that read a changed input (S0's change set). Removed
    /// definitions forget their memos.
    pub(crate) fn note_inputs(&mut self, paths: impl IntoIterator<Item = DefPath>) {
        self.adopt_completed_headers();
        let mut changes = Changes::default();
        let mut any_names_changed = false;
        for path in paths {
            let mut names_changed = false;
            let id = DefId::intern(&path);
            let now = self.definition_inputs(&path);
            // A definition that vanished for its readers and now has items
            // again appears to them as new.
            let vanished = self.vanished.remove(&id);
            let before = if vanished && now.is_some() {
                None
            } else {
                self.inputs.get(&id).cloned()
            };
            if now == before {
                continue;
            }
            self.source_growth.changed(id, now.is_some());
            self.front_changes.changed(id, now.is_some());
            let mut read = |k| changes.read(k);
            match (&before, &now) {
                (Some(b), Some(n)) => {
                    if b.body != n.body {
                        read(ReadKey::ExecutedBody(id));
                        read(ReadKey::LayoutBody(id));
                        read(ReadKey::Representation(id));
                        read(ReadKey::CitedText(id));
                    }
                    if b.header != n.header {
                        if hirsel_types::specialization_parts(&path).is_none() {
                            read(ReadKey::NameSet(
                                PackageKey::of(&path.0.package),
                                ModuleKey::of(&path.0.modules),
                            ));
                        }
                        read(ReadKey::Signature(id));
                        read(ReadKey::DeclaredSignature(id));
                        read(ReadKey::FailedSignature(id));
                        read(ReadKey::ExecutedSignature(id));
                        read(ReadKey::Representation(id));
                        read(ReadKey::CitedText(id));
                    }
                    if b.candidates != n.candidates {
                        read(ReadKey::DeclarationCandidates(id));
                        read(ReadKey::CitedText(id));
                        read(ReadKey::Signature(id));
                        read(ReadKey::DeclaredSignature(id));
                        read(ReadKey::FailedSignature(id));
                    }
                    if b.names != n.names {
                        names_changed = true;
                    }
                    // A replaced item has a new anchor, which no text covers, and
                    // a header edit changes the signature a fast hit skips the
                    // text compare for: the definition's memos are recomputed. A
                    // body-only edit marks nothing here; each body memo compares
                    // its own text when read.
                    if b.anchors != n.anchors || b.header != n.header {
                        changes.text(id);
                    }
                }
                _ => {
                    // Added or removed: everything that names it.
                    names_changed = true;
                    for k in [
                        ReadKey::Signature(id),
                        ReadKey::DeclaredSignature(id),
                        ReadKey::FailedSignature(id),
                        ReadKey::ExecutedSignature(id),
                        ReadKey::ExecutedBody(id),
                        ReadKey::LayoutBody(id),
                        ReadKey::Representation(id),
                        ReadKey::CitedText(id),
                        ReadKey::DeclarationCandidates(id),
                    ] {
                        read(k);
                    }
                    changes.text(id);
                }
            }
            match now {
                Some(n) => {
                    self.inputs.insert(id, n);
                }
                None => {
                    self.inputs.remove(&id);
                    self.specialization_dependencies.remove(&id);
                    self.forget_definition(id);
                }
            }
            // A GEN-6 specialization's name is never written in source, so no
            // lookup can observe it appearing.
            if names_changed && hirsel_types::specialization_parts(&path).is_none() {
                any_names_changed = true;
                changes.read(ReadKey::NameSet(
                    PackageKey::of(&path.0.package),
                    ModuleKey::of(&path.0.modules),
                ));
            }
        }
        changes.lookups |= any_names_changed;
        self.apply_changes(&changes);
    }

    /// S0's signature step: recomputes every forgotten signature in DefPath
    /// order (each reads its own dependencies first) and, where a signature's
    /// observed value changed, forgets its readers, until none is pending. A
    /// recomputed signature whose value did not change forgets nothing, so
    /// its readers stay valid.
    pub(crate) fn settle_signatures(&mut self) {
        // Settling is S0 bookkeeping, not a phase the caller asked for.
        let (last_phase, unavailable) = (self.last_phase, self.unavailable);
        while !self.pending_signatures.is_empty() {
            let pending = std::mem::take(&mut self.pending_signatures);
            let mut changes = Changes::default();
            for (id, before) in pending {
                let path = id.path();
                let _ = self.signature_query(path);
                if self.signature_value_of(path) != before {
                    for k in [
                        ReadKey::Signature(id),
                        ReadKey::DeclaredSignature(id),
                        ReadKey::FailedSignature(id),
                        ReadKey::ExecutedSignature(id),
                    ] {
                        let value = self.observe(path, &k);
                        changes.read_value(k, value);
                    }
                }
            }
            self.apply_changes(&changes);
        }
        self.last_phase = last_phase;
        self.unavailable = unavailable;
    }
    /// A signature memo's observed value without computing it: its
    /// fingerprint, or its failed key.
    pub(super) fn signature_value_of(&self, path: &DefPath) -> Option<ContentHash> {
        let entry = self.signatures.get(&DefId::intern(path))?;
        Some(match &entry.memo.value {
            Ok(output) => output.value.fingerprint,
            Err(_) => optional(self.failed_signature_key(path)),
        })
    }

    /// The environment is a public input: a change since the last query
    /// forgets every memo that read it.
    pub(crate) fn note_environment(&mut self) {
        let now = (
            self.environment.clone(),
            self.comptime_limits,
            self.build_inputs_revision(),
            self.verify,
        );
        if self.environment_seen.as_ref() != Some(&now) {
            if self.environment_seen.is_some() {
                let mut changes = Changes::default();
                changes.read(ReadKey::Environment);
                self.apply_changes(&changes);
            }
            self.environment_seen = Some(now);
        }
    }

    /// Forgets the readers of definitions whose signatures appeared or changed
    /// outside indexing (staged constants, generated instances).
    pub(crate) fn note_signatures(&mut self, paths: &[DefPath]) {
        let mut changes = self.signature_value_changes(paths);
        changes.lookups = true;
        self.apply_changes(&changes);
        self.settle_signatures();
    }

    fn signature_value_changes(&mut self, paths: &[DefPath]) -> Changes {
        let mut changes = Changes::default();
        for path in paths {
            let id = DefId::intern(path);
            self.source_growth.invalidate(id);
            self.front_changes.changed(
                id,
                self.declared(path) || self.staged_constants.contains_key(path),
            );
            for k in [
                ReadKey::Signature(id),
                ReadKey::DeclaredSignature(id),
                ReadKey::FailedSignature(id),
                ReadKey::ExecutedSignature(id),
                ReadKey::DeclarationCandidates(id),
            ] {
                changes.read(k);
            }
        }
        changes
    }

    /// Definitions whose only items were dropped and wait to be adopted by
    /// another file: they are gone for every reader, so their readers and all
    /// lookups are forgotten as for a removal, but their own memos and recorded
    /// inputs stay until the next index adopts or expires them.
    pub(crate) fn note_detached(&mut self, paths: impl IntoIterator<Item = DefPath>) {
        let mut changes = Changes::default();
        let mut any = false;
        for path in paths {
            let id = DefId::intern(&path);
            if !self.inputs.contains_key(&id) || !self.vanished.insert(id) {
                continue;
            }
            self.source_growth.changed(id, false);
            self.front_changes.changed(id, false);
            any = true;
            for k in [
                ReadKey::Signature(id),
                ReadKey::DeclaredSignature(id),
                ReadKey::FailedSignature(id),
                ReadKey::ExecutedSignature(id),
                ReadKey::ExecutedBody(id),
                ReadKey::LayoutBody(id),
                ReadKey::Representation(id),
                ReadKey::CitedText(id),
                ReadKey::DeclarationCandidates(id),
                ReadKey::NameSet(
                    PackageKey::of(&path.0.package),
                    ModuleKey::of(&path.0.modules),
                ),
            ] {
                changes.read(k);
            }
        }
        if any {
            changes.lookups = true;
            self.apply_changes(&changes);
        }
    }

    /// Forgets every memo of `definition`.
    pub(crate) fn forget_definition(&mut self, definition: DefId) {
        for kind in [
            QueryKind::Signature,
            QueryKind::Resolve,
            QueryKind::Check,
            QueryKind::Summary,
            QueryKind::CheckedSummary,
            QueryKind::Modes,
            QueryKind::Lower,
        ] {
            self.forget(&MemoKey::new(kind, definition));
        }
    }

    /// Forgets one memo from its table.
    pub(crate) fn forget(&mut self, key: &MemoKey) {
        self.front_products.forget(key);
        match key.kind {
            QueryKind::Signature => {
                self.signatures.remove(&key.def);
                self.adopted_headers = None;
            }
            QueryKind::Resolve => {
                self.resolutions.remove(&key.def);
            }
            QueryKind::Check => {
                self.checked.remove(&key.def);
            }
            QueryKind::Summary => {
                self.summaries.remove(&key.def);
                self.settled_summaries.remove(&key.def);
            }
            QueryKind::CheckedSummary => {
                self.checked_summaries.remove(&key.def);
            }
            QueryKind::Modes => {
                self.modes.remove(&key.def);
            }
            QueryKind::Lower => {
                self.lowered.remove(&key.def);
                self.lower_receipts.remove(&key.def);
                self.lower_holds.forget(key);
            }
            _ => {}
        }
    }

    /// Execution closure by union: merges `reads` into every evaluation frame.
    pub(crate) fn merge_execution(&mut self, reads: &Reads) {
        for frame in &mut self.execution_stack {
            frame.merge(reads);
        }
    }
    /// Whether every read of an executed closure still observes its value.
    pub(crate) fn execution_valid(&mut self, owner: &DefPath, reads: &Reads) -> bool {
        reads
            .first_changed(|read| self.observe(owner, read))
            .is_none()
    }
}
