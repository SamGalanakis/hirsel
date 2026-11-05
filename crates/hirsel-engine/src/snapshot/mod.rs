//! The frozen signature snapshot of one revision (design §1(b), "Snapshot").
//!
//! A revision computes its snapshot once: every signature by `DefId`, the
//! private views each opaque type's own package sees, and V's verdict on every
//! view with each package's universe errors. It is immutable, `Send + Sync`
//! and stamped with its epoch, so definitions read it without locks.
use crate::memo::Revision;
use crate::*;
use hirsel_syntax::{DefId, DefMap, PackageId};
use hirsel_types::{LookupOutput, SignatureRef, TypeInterner};
use hirsel_validate::{SignatureLookup, ValidatedSignatures, ValidationDigest};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

mod checked_runtime;
mod declarations;
mod provider;
mod sources;
pub use sources::CitationSource;
pub(crate) use sources::{OwnedSourceContext, SourceContext};
mod views;
pub(crate) use declarations::{DeclarationHead, DeclarationSiblings, logical};
pub use declarations::{DeclarationIndex, Declarations, ModuleInputs};
pub use provider::FrozenProvider;

pub use views::{PackageUniverse, PackageViews, View, ViewTable};

/// Scalar snapshots may be cached until an operation admits their inputs.
/// Once admitted, only live readers keep the snapshot and its charges alive.
pub(crate) struct SnapshotCache {
    scalar: Option<Arc<SignatureSnapshot>>,
    readers: std::sync::Weak<SignatureSnapshot>,
}
impl SnapshotCache {
    pub(crate) fn new(snapshot: &Arc<SignatureSnapshot>) -> Self {
        Self {
            scalar: (!snapshot
                .header_products()
                .is_some_and(|headers| headers.operation_owned()))
            .then(|| snapshot.clone()),
            readers: Arc::downgrade(snapshot),
        }
    }
    pub(crate) fn get(&self) -> Option<Arc<SignatureSnapshot>> {
        self.readers.upgrade()
    }
    pub(crate) fn release(&mut self) {
        self.scalar = None;
    }
}

/// V's identity of a declared view, as compile-time evaluation names it.
#[derive(Debug)]
struct Identity(Arc<hirsel_validate::IdentifiedSignature>);
impl ViewIdentity for Identity {
    fn definition(&self) -> DefId {
        self.0.definition()
    }
    fn signature(&self) -> Result<SignatureRef, QueryError> {
        self.0.signature().map_err(QueryError::from)
    }
}
use hirsel_interp::aggregate::ViewIdentity;

/// The rows an evaluation demands from one snapshot, one per type it names.
/// Compile-time rows carry V's digest of each view (reflection records it);
/// run-time rows carry the zero digest ordinary execution reads.
struct SnapshotRows {
    snapshot: Arc<SignatureSnapshot>,
    entry: Option<hirsel_tir::BodyId>,
    validated: bool,
}
impl hirsel_interp::aggregate::TypeRows for SnapshotRows {
    fn view(
        &self,
        path: &DefPath,
    ) -> Result<Option<(hirsel_interp::aggregate::SerializedView, ContentHash)>, QueryError> {
        let d = DefId::intern(path);
        if !self.validated {
            return match self.snapshot.body_view(&path.0.package, d) {
                Some(view) => view.map(|view| Some((view.into(), ContentHash::default()))),
                None => Ok(self
                    .snapshot
                    .lookup
                    .registry
                    .get(path)
                    .map(|signature| (signature.clone().into(), ContentHash::default()))),
            };
        }
        match self.snapshot.representation_identity(d) {
            Some(Ok(view)) => {
                let digest = view.digest().0;
                Ok(Some((
                    (Arc::new(Identity(view)) as Arc<dyn ViewIdentity>).into(),
                    digest,
                )))
            }
            Some(Err(error)) => Err(error),
            None => Ok(self
                .snapshot
                .lookup
                .registry
                .get(path)
                .map(|signature| (signature.clone().into(), ContentHash::default()))),
        }
    }
    fn finalizer(
        &self,
        drop: &DefPath,
    ) -> Result<Option<(DefPath, hirsel_tir::BodyId)>, QueryError> {
        if self
            .snapshot
            .lookup
            .candidates
            .contains_key(&DefId::intern(drop))
        {
            let entry = self.entry.ok_or(QueryError::NotImplemented)?;
            Ok(Some((drop.clone(), entry)))
        } else if self.snapshot.lookup.registry.get(drop).is_some() {
            Ok(None)
        } else {
            Err(QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "runtime types".into(),
                message: format!("missing finalizer {drop:?}"),
            }))
        }
    }
}

/// One revision's signatures, private views and V verdicts.
pub struct SignatureSnapshot {
    epoch: Revision,
    types: Arc<TypeInterner>,
    signatures: DefMap<Result<SignatureRef, QueryError>>,
    membership: Mutex<Option<Arc<SnapshotMembership>>>,
    headers: Option<Arc<crate::queries::header_products::HeaderProducts>>,
    /// Each opaque type's private view, which only its own package's bodies see.
    private: DefMap<SignatureRef>,
    /// Signature inputs are sufficient for P; V builds its universe on demand.
    validation: OnceLock<Arc<ValidationState>>,
    /// Set once this snapshot's source dependencies' summaries are published.
    published: OnceLock<()>,
    validation_inputs: Mutex<Option<ValidationCandidates>>,
    /// Each immutable product is identified once, shared by reflection and V.
    public_views: DefMap<OnceLock<View>>,
    private_views: DefMap<OnceLock<View>>,
    /// What lookups resolve against, frozen with the signatures.
    lookup: LookupInput,
    /// Local verifier authority is never part of a serialized snapshot/product.
    verifier: Option<Arc<hirsel_validate::VerifierCache>>,
    verifier_policy: ContentHash,
}
struct SnapshotMembership {
    definitions: Box<[DefId]>,
    packages: std::collections::BTreeSet<PackageId>,
    _lease: Option<Arc<dyn Send + Sync>>,
}
fn view_products<'a>(
    types: &TypeInterner,
    rows: impl Iterator<Item = (DefId, &'a SignatureRef)>,
    previous: Option<&DefMap<OnceLock<View>>>,
) -> DefMap<OnceLock<View>> {
    rows.map(|(d, signature)| {
        let cell = OnceLock::new();
        if let Some(view) = previous.and_then(|p| p.get(&d)).and_then(OnceLock::get)
            && view.matches(types, signature)
        {
            let _ = cell.set(view.clone());
        }
        (d, cell)
    })
    .collect()
}
/// Receipt candidates are not source authority. Keep them through deferred
/// epochs, and release the old universe once this snapshot initializes V.
#[derive(Clone, Default)]
struct ValidationCandidates {
    previous: Option<Arc<ValidationState>>,
}
pub(crate) struct ValidationState {
    pub(crate) registration: OnceLock<Arc<dyn Send + Sync>>,
    views: Arc<ViewTable>,
    bases: BTreeMap<PackageId, Arc<ViewTable>>,
    packages: BTreeMap<PackageId, PackageViews>,
    /// Each installed package's own universe: its published summary, with
    /// its views (its opaque types' private ones) read on demand.
    installed: BTreeMap<PackageId, Arc<ViewTable>>,
    validated: ValidatedSignatures,
    /// Full installed tables for materialized owners. The snapshot fixes the
    /// source, registration and verifier inputs; each overlay still gets its
    /// own replacement verdicts and every execution receipt is replayed.
    overlay_bases: Mutex<BTreeMap<PackageId, Arc<ViewTable>>>,
    overlays: Mutex<BTreeMap<(PackageId, ValidationDigest), Arc<PackageViews>>>,
    producers: Arc<crate::memo::S1Coordinator<ValidationProducer>>,
}
#[derive(Clone, Eq, PartialEq, Hash)]
enum ValidationProducer {
    Base(PackageId),
    Overlay(PackageId, ValidationDigest),
}
/// A read-only installed dependency's published universe summaries: its own
/// universe, which its bodies are validated against, and what importers'
/// universes hold of its public views.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstalledUniverse {
    pub own: hirsel_validate::PackageUniverse,
    pub exported: hirsel_validate::PackageUniverse,
}
impl InstalledUniverse {
    /// The archived pair, or `None` when either summary carries errors.
    pub(crate) fn encode(&self) -> Option<Vec<u8>> {
        let own = self.own.encode()?;
        let exported = self.exported.encode()?;
        bincode::serialize(&(own, exported)).ok()
    }
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let (own, exported): (Vec<u8>, Vec<u8>) = bincode::deserialize(bytes).ok()?;
        Some(Self {
            own: hirsel_validate::PackageUniverse::decode(&own)?,
            exported: hirsel_validate::PackageUniverse::decode(&exported)?,
        })
    }
}
/// Installed views, read from the snapshot's rows when V first names one.
struct InstalledRows {
    types: Arc<TypeInterner>,
    headers: Option<Arc<crate::queries::header_products::HeaderProducts>>,
    signatures: DefMap<SignatureRef>,
    private: DefMap<SignatureRef>,
    /// The package whose opaque types are seen through their private views.
    owner: Option<PackageId>,
}
impl hirsel_validate::ViewSource for InstalledRows {
    fn view(&self, d: DefId) -> Option<View> {
        if self.owner.as_ref() == Some(&d.path().0.package)
            && let Some(private) =
                private_view(&self.types, &self.private, self.headers.as_ref(), d)
        {
            return Some(View::new(&self.types, private, None));
        }
        if let Some(signature) = self.signatures.get(&d) {
            return Some(View::new(&self.types, signature.clone(), None));
        }
        let headers = self.headers.as_ref()?;
        let signature = headers
            .product(d)?
            .ok()?
            .result
            .as_ref()
            .ok()?
            .value
            .clone();
        Some(View::sourced(
            &self.types,
            &signature,
            &(Arc::new(crate::queries::header_products::RowSource(headers.clone()))
                as Arc<dyn hirsel_validate::SignatureSource>),
            None,
        ))
    }
}
/// `d`'s private view: an opaque type's own representation.
fn private_view(
    types: &TypeInterner,
    private: &DefMap<SignatureRef>,
    headers: Option<&Arc<crate::queries::header_products::HeaderProducts>>,
    d: DefId,
) -> Option<SignatureRef> {
    if let Some(view) = private.get(&d) {
        return Some(view.clone());
    }
    let product = headers?.product(d)?.ok()?;
    let public = &product.result.as_ref().ok()?.value;
    if public.metadata.visibility != hirsel_syntax::Visibility::PublicOpaque {
        return None;
    }
    let owner = product.entry.as_ref()?.products.representation.as_ref()?;
    Some(owner.body_view(types, public).0.clone())
}
/// A snapshot's lookup input: the provider manifest hash every lookup
/// observes, the registry a bare name falls back to, the source builtins and
/// each definition's `DeclarationCandidates`.
#[derive(Clone, Default)]
pub(crate) struct LookupInput {
    pub manifest: ContentHash,
    pub registry: crate::registry::BuiltinRegistry,
    pub builtins: std::collections::BTreeSet<DefId>,
    pub candidates: DefMap<ContentHash>,
    pub failed: DefMap<Option<ContentHash>>,
    pub package_dependencies: BTreeMap<PackageId, Vec<PackageId>>,
    /// The dependencies whose published universe summaries are bound to
    /// exactly this snapshot's sources: V reads their rows on demand.
    pub installed: BTreeMap<PackageId, InstalledUniverse>,
    /// Each package's source binding: what its published summary is keyed by.
    pub sources: BTreeMap<PackageId, ContentHash>,
    /// The engine environment summaries are addressed under.
    pub environment: ContentHash,
    /// The local store this machine's V publishes source dependencies'
    /// summaries to, once it has computed them from their exact sources.
    pub summaries: Option<Arc<dyn crate::store::ResultStore>>,
}
const _: () = {
    const fn shared<T: Send + Sync>() {}
    shared::<SignatureSnapshot>();
};
fn validation_retention_refused() {
    hirsel_intern::work::count(hirsel_intern::work::Counter::ValidationRetentionRefusals);
    hirsel_intern::timings::count(hirsel_intern::timings::Count::ValidationRetentionRefusals);
}

impl ValidationState {
    pub(crate) fn evict_cached(&self) -> bool {
        let mut overlays = self.overlays.lock().unwrap();
        let before = overlays.len();
        overlays.retain(|_, views| Arc::strong_count(views) > 1);
        let mut removed = before - overlays.len();
        drop(overlays);
        let mut bases = self.overlay_bases.lock().unwrap();
        let before = bases.len();
        bases.retain(|_, table| Arc::strong_count(table) > 1);
        removed += before - bases.len();
        hirsel_intern::work::count_by(
            hirsel_intern::work::Counter::VerifierFactEvictions,
            removed as u64,
        );
        hirsel_intern::timings::count_by(
            hirsel_intern::timings::Count::VerifierFactEvictions,
            removed as u64,
        );
        removed != 0
    }
    /// The table `package`'s bodies are validated against, without private views.
    fn base(&self, package: &PackageId) -> &Arc<ViewTable> {
        self.installed
            .get(package)
            .or_else(|| self.bases.get(package))
            .unwrap_or(&self.views)
    }
    /// The summary of `package`'s own V universe.
    fn universe(&self, package: &PackageId) -> hirsel_validate::PackageUniverse {
        match self.packages.get(package) {
            Some(views) => views.universe(),
            None => self.base(package).universe(),
        }
    }
    /// What importers' universes hold of `package`'s public views.
    fn exported(&self, package: &PackageId) -> hirsel_validate::PackageUniverse {
        self.bases
            .get(package)
            .unwrap_or(&self.views)
            .exported(package)
    }
}
impl SignatureSnapshot {
    pub(crate) fn builtin_dictionary_value(
        &self,
        memo: &hirsel_types::TypeMemo,
        path: &DefPath,
    ) -> Result<Option<hirsel_tir::Materialized>, QueryError> {
        self.lookup
            .registry
            .dictionary_value(&self.types, memo, path)
    }

    /// The snapshot of `signatures` (every public signature, by path, or the
    /// error that normalizing it reported) and `private` (each opaque owner's
    /// private view), reusing `previous`'s digests and verdicts that still hold.
    pub fn new(
        epoch: Revision,
        types: Arc<TypeInterner>,
        signatures: impl IntoIterator<Item = (DefId, Result<SignatureRef, QueryError>)>,
        private: impl IntoIterator<Item = SignatureRef>,
        previous: Option<&SignatureSnapshot>,
    ) -> Self {
        let snapshot = Self::build(epoch, types, signatures, private, previous);
        // A refused build publishes nothing; its first reader builds it again.
        let _ = snapshot.validation_state();
        snapshot
    }
    fn build(
        epoch: Revision,
        types: Arc<TypeInterner>,
        signatures: impl IntoIterator<Item = (DefId, Result<SignatureRef, QueryError>)>,
        private: impl IntoIterator<Item = SignatureRef>,
        previous: Option<&SignatureSnapshot>,
    ) -> Self {
        let signatures: DefMap<_> = signatures.into_iter().collect();
        let private: DefMap<SignatureRef> = private
            .into_iter()
            .map(|view| (view.definition, view))
            .collect();
        let public_views = view_products(
            &types,
            signatures
                .iter()
                .filter_map(|(d, s)| s.as_ref().ok().map(|s| (*d, s))),
            previous.map(|p| &p.public_views),
        );
        let private_views = view_products(
            &types,
            private.iter().map(|(d, s)| (*d, s)),
            previous.map(|p| &p.private_views),
        );
        let candidates = previous
            .map(|p| p.validation_candidates())
            .unwrap_or_default();
        Self {
            epoch,
            types,
            signatures,
            membership: Mutex::default(),
            headers: None,
            private,
            validation: OnceLock::new(),
            published: OnceLock::new(),
            validation_inputs: Mutex::new(Some(candidates)),
            public_views,
            private_views,
            verifier: None,
            verifier_policy: ContentHash::default(),
            lookup: LookupInput::default(),
        }
    }
    fn with_headers(
        mut self,
        headers: Arc<crate::queries::header_products::HeaderProducts>,
    ) -> Self {
        for d in headers.definitions() {
            self.public_views.entry(d).or_default();
            self.private_views.entry(d).or_default();
        }
        self.headers = Some(headers);
        self
    }
    /// V's view of `d`'s public signature `s`. A header row is held by
    /// identity and read again from the header cache, which may evict it.
    fn view_of(&self, d: DefId, s: &SignatureRef, previous: Option<&View>) -> View {
        match self.headers.as_ref().filter(|headers| headers.contains(d)) {
            Some(headers) => View::sourced(
                &self.types,
                s,
                &(Arc::new(crate::queries::header_products::RowSource(headers.clone()))
                    as Arc<dyn hirsel_validate::SignatureSource>),
                previous,
            ),
            None => View::new(&self.types, s.clone(), previous),
        }
    }
    pub(crate) fn header_products(
        &self,
    ) -> Option<&Arc<crate::queries::header_products::HeaderProducts>> {
        self.headers.as_ref()
    }
    fn validation_candidates(&self) -> ValidationCandidates {
        if let Some(state) = self.validation.get() {
            return ValidationCandidates {
                previous: Some(state.clone()),
            };
        }
        let pending = self.validation_inputs.lock().unwrap().clone();
        pending.unwrap_or_else(|| {
            // An initializer has taken the inputs but has not yet published.
            // Join that initializer after releasing the short input lock.
            let _ = self.validation_state();
            ValidationCandidates {
                previous: self.validation.get().cloned(),
            }
        })
    }
    /// Every package `package` sees: itself, its dependency closure and the
    /// builtin packages. `None` when the loader graph is incomplete.
    fn visible(&self, package: &PackageId) -> Option<std::collections::BTreeSet<PackageId>> {
        let mut visible: std::collections::BTreeSet<_> = self
            .lookup
            .registry
            .signatures()
            .map(|s| s.definition.path().0.package.clone())
            .collect();
        let mut pending = vec![package.clone()];
        while let Some(next) = pending.pop() {
            if !visible.insert(next.clone()) {
                continue;
            }
            pending.extend(self.lookup.package_dependencies.get(&next)?.iter().cloned());
        }
        Some(visible)
    }
    /// The rows of `members`, read as V names them. `owner`'s opaque types
    /// are seen through their private views.
    fn installed_rows(
        &self,
        members: &[DefId],
        owner: Option<&PackageId>,
    ) -> Arc<hirsel_validate::InstalledViews> {
        let rows = InstalledRows {
            types: self.types.clone(),
            headers: self.headers.clone(),
            signatures: members
                .iter()
                .filter_map(|d| Some((*d, self.signatures.get(d)?.as_ref().ok()?.clone())))
                .collect(),
            private: members
                .iter()
                .filter_map(|d| Some((*d, self.private.get(d)?.clone())))
                .collect(),
            owner: owner.cloned(),
        };
        Arc::new(hirsel_validate::InstalledViews::new(
            members.iter().copied(),
            Box::new(rows),
        ))
    }
    fn validation_state(&self) -> Result<&ValidationState, hirsel_validate::ViewRefusal> {
        let state = self.validation_universe()?;
        self.published
            .get_or_init(|| self.publish_dependency_universes(state));
        Ok(state)
    }
    /// Publish the summary of every source dependency this snapshot computed
    /// from its full table. A dependency is never under check here: its
    /// universe is a function of its exact sources alone, so later snapshots
    /// and engines bound to those sources read the summary instead of every
    /// row. Packages under check are no one's dependency and are not kept.
    fn publish_dependency_universes(&self, state: &ValidationState) {
        let Some(store) = self.lookup.summaries.as_ref() else {
            return;
        };
        let dependencies: std::collections::BTreeSet<&PackageId> = self
            .lookup
            .package_dependencies
            .values()
            .flatten()
            .collect();
        for package in dependencies {
            if state.installed.contains_key(package) {
                continue;
            }
            let Some(sources) = self.lookup.sources.get(package) else {
                continue;
            };
            let universe = InstalledUniverse {
                own: state.universe(package),
                exported: state.exported(package),
            };
            let Some(bytes) = universe.encode() else {
                continue;
            };
            let Some(record) = crate::queries::UNIVERSE.reference(&bytes) else {
                continue;
            };
            let query = crate::queries::universe_query(self.lookup.environment, package, *sources);
            store.cache_product(
                query.address,
                crate::queries::universe_action(query, record),
                crate::queries::UNIVERSE,
                &bytes,
            );
        }
    }
    /// V's universe of this snapshot, built once. A build that meets a
    /// refused view read publishes nothing and keeps its inputs, so the next
    /// demand builds it again.
    fn validation_universe(&self) -> Result<&ValidationState, hirsel_validate::ViewRefusal> {
        if let Some(state) = self.validation.get() {
            return Ok(&**state);
        }
        // The input lock serializes builders; a later one finds the state.
        let mut inputs = self.validation_inputs.lock().unwrap();
        if let Some(state) = self.validation.get() {
            return Ok(&**state);
        }
        let previous = inputs
            .as_ref()
            .expect("validation inputs remain until a universe is published")
            .previous
            .clone();
        let state = self.build_validation(previous.as_deref())?;
        *inputs = None;
        Ok(&**self.validation.get_or_init(|| state))
    }
    fn build_validation(
        &self,
        previous: Option<&ValidationState>,
    ) -> Result<Arc<ValidationState>, hirsel_validate::ViewRefusal> {
        {
            let _timing = hirsel_intern::timings::phase(hirsel_intern::timings::Phase::Verify);
            let t = &self.types;
            let old = previous.map(|p| &p.validated);
            let mut validated =
                ValidatedSignatures::with_cache(self.verifier.clone(), self.verifier_policy);
            // An installed dependency's rows are read only when V names one:
            // its published summary stands for its verdicts. Every other
            // view is identified and checked. The universe holds identities;
            // each read of a payload is a scoped view, so the header cache
            // can evict a row it only names.
            let installed = &self.lookup.installed;
            let (members, checked): (Vec<DefId>, Vec<DefId>) = self
                .membership()
                .definitions
                .iter()
                .copied()
                .partition(|d| installed.contains_key(&d.path().0.package));
            // A failed signature is no view. A row the header cache refuses
            // to produce refuses the build: a universe never omits a view it
            // could not read, and no later row demands the refused proof again.
            let checked: Vec<View> = checked
                .into_iter()
                .filter_map(|d| match self.signature(d)? {
                    Ok(s) => Some(Ok(self.public_views[&d]
                        .get_or_init(|| self.view_of(d, &s, previous.and_then(|p| p.views.get(d))))
                        .clone())),
                    Err(cause @ QueryError::Operational(_)) => {
                        Some(Err(hirsel_validate::ViewRefusal::Unreadable {
                            definition: d,
                            cause,
                        }))
                    }
                    Err(_) => None,
                })
                .collect::<Result<_, _>>()?;
            let public = self.installed_rows(&members, None);
            // One universe over the packages `visible` admits (all: `None`).
            // An installed package's verdicts read the first heap role of its
            // own closure; a universe whose first heap differs checks them.
            let universe = |visible: Option<&std::collections::BTreeSet<PackageId>>,
                            validated: &mut ValidatedSignatures| {
                let admits = |package: &PackageId| visible.is_none_or(|v| v.contains(package));
                let views = checked
                    .iter()
                    .filter(|v| admits(&v.d.path().0.package))
                    .cloned();
                let parts: Vec<_> = installed
                    .iter()
                    .filter(|(package, _)| admits(package))
                    .collect();
                if parts.is_empty() {
                    return ViewTable::from_views(t, views, validated, old);
                }
                let table = ViewTable::with_installed(
                    t,
                    views.clone(),
                    hirsel_validate::Installed::new(
                        public.clone(),
                        parts.iter().map(|(_, u)| u.exported.clone()),
                    ),
                    validated,
                    old,
                )?;
                let heap = |package: &PackageId| {
                    let closure = self.visible(package)?;
                    let views = checked
                        .iter()
                        .filter(|v| closure.contains(&v.d.path().0.package));
                    let own = installed
                        .iter()
                        .filter(|(p, _)| closure.contains(*p))
                        .filter_map(|(_, u)| u.exported.first_heap.clone());
                    Some(
                        views
                            .filter_map(|v| v.identified().heap().map(|_| v.d))
                            .chain(own.map(|(d, _)| d))
                            .min(),
                    )
                };
                let first = table.first_heap().map(|(d, _)| d);
                if parts
                    .iter()
                    .all(|(package, _)| heap(package) == Some(first))
                {
                    return Ok(table);
                }
                ViewTable::from_views(
                    t,
                    views.chain(members.iter().filter_map(|d| {
                        admits(&d.path().0.package)
                            .then(|| public.get(*d).cloned())
                            .flatten()
                    })),
                    validated,
                    old,
                )
            };
            let views = Arc::new(universe(None, &mut validated)?);
            // Dependency packages cannot observe their importers. Keep every
            // selected dependency and registry row in each actual V universe,
            // including valid rows, private replacements and heap/error inputs.
            // Explicit snapshots without a complete loader graph retain the
            // full-world contract rather than guessing package visibility.
            let mut bases = BTreeMap::new();
            for package in self.lookup.package_dependencies.keys() {
                if installed.contains_key(package) {
                    continue;
                }
                let Some(visible) = self.visible(package) else {
                    continue;
                };
                let base = if checked
                    .iter()
                    .map(|v| &v.d.path().0.package)
                    .chain(installed.keys())
                    .all(|p| visible.contains(p))
                {
                    views.clone()
                } else {
                    Arc::new(universe(Some(&visible), &mut validated)?)
                };
                bases.insert(package.clone(), base);
            }
            // An installed package's own universe is its published summary.
            let installed_universes = installed
                .iter()
                .filter_map(|(package, universe)| {
                    let visible = self.visible(package)?;
                    Some((package, universe, visible))
                })
                .map(|(package, universe, visible)| {
                    let members: Vec<DefId> = self
                        .membership()
                        .definitions
                        .iter()
                        .copied()
                        .filter(|d| visible.contains(&d.path().0.package))
                        .collect();
                    let rows = self.installed_rows(&members, Some(package));
                    Ok((
                        package.clone(),
                        Arc::new(ViewTable::with_installed(
                            t,
                            [],
                            hirsel_validate::Installed::new(rows, [universe.own.clone()]),
                            &mut validated,
                            old,
                        )?),
                    ))
                })
                .collect::<Result<_, hirsel_validate::ViewRefusal>>()?;
            let mut owned: BTreeMap<PackageId, Vec<View>> = BTreeMap::new();
            for &d in self.membership().definitions.iter() {
                let package = &d.path().0.package;
                if installed.contains_key(package) {
                    continue;
                }
                let Some(view) = self.private_view(d) else {
                    continue;
                };
                let earlier = previous
                    .and_then(|p| p.packages.get(package))
                    .and_then(|p| p.replacement(d));
                owned.entry(package.clone()).or_default().push(
                    self.private_views[&d]
                        .get_or_init(|| View::new(t, view.clone(), earlier))
                        .clone(),
                );
            }
            let packages = owned
                .into_iter()
                .map(|(package, replaced)| {
                    let base = bases.get(&package).unwrap_or(&views).clone();
                    Ok((
                        package,
                        PackageViews::new(t, base, replaced, &mut validated, old)?,
                    ))
                })
                .collect::<Result<_, hirsel_validate::ViewRefusal>>()?;
            let state = Arc::new(ValidationState {
                registration: OnceLock::new(),
                views,
                bases,
                packages,
                overlay_bases: Mutex::default(),
                overlays: Mutex::default(),
                producers: crate::memo::S1Coordinator::new(),
                installed: installed_universes,
                validated,
            });
            if let Some(headers) = &self.headers {
                headers.retain_validation_cache(&state)?;
            }
            Ok(state)
        }
    }
    pub(crate) fn with_lookup(mut self, lookup: LookupInput) -> Self {
        self.lookup = lookup;
        self
    }
    fn with_verifier(
        mut self,
        verifier: Option<Arc<hirsel_validate::VerifierCache>>,
        policy: ContentHash,
    ) -> Self {
        self.verifier = verifier;
        self.verifier_policy = policy;
        self
    }
    pub(crate) fn source_verification_policy(&self) -> ContentHash {
        ContentHash::of_parts(
            "hirsel.validate.frozen-source.v2",
            [self.verifier_policy.0.as_slice()],
        )
    }
    pub(crate) fn revalidate_execution(
        &self,
        receipt: &hirsel_validate::ExecutionReceipt,
        anchor: impl FnMut(&DefPath, u32) -> Option<hirsel_span::Anchor>,
    ) -> Option<hirsel_validate::ExecutionReceipt> {
        let verifier = self.verifier.as_ref()?;
        verifier.revalidate_execution(receipt, self.types.clone(), anchor, self)
    }
    pub(crate) fn validate_source_artifact<'input>(
        &self,
        definition: impl Into<hirsel_validate::DefinitionInput<'input>>,
        owner: impl FnMut(hirsel_span::Anchor) -> Option<(DefPath, u32)>,
    ) -> Result<
        (
            hirsel_validate::Validation,
            bool,
            Option<hirsel_validate::AcceptedArtifact>,
            Option<hirsel_validate::VerifiedDefinition>,
        ),
        hirsel_validate::ViewRefusal,
    > {
        let definition = definition.into();
        let path = match definition {
            hirsel_validate::DefinitionInput::Live(value) => value.path,
            hirsel_validate::DefinitionInput::Archived(value) => {
                value.header().expect("checked TIR header").path
            }
        };
        hirsel_validate::validate_definition(
            definition,
            self.types.clone(),
            owner,
            self.validation(&path.path().0.package)?,
            self.source_verification_policy(),
            self.verifier.as_deref(),
        )
    }
    pub(crate) fn validate_private_artifact(
        &self,
        definition: &hirsel_tir::Definition,
        signature: SignatureRef,
        origin: Option<&(hirsel_span::Anchor, DefPath, u32)>,
    ) -> Result<
        (
            hirsel_validate::Validation,
            Option<hirsel_validate::AcceptedArtifact>,
        ),
        hirsel_validate::ViewRefusal,
    > {
        let views = self.validation_with(&definition.path.path().0.package, signature.clone())?;
        let artifact = hirsel_validate::Artifact::private(
            definition,
            self.types.clone(),
            signature,
            |anchor| {
                origin
                    .filter(|(expected, _, _)| *expected == anchor)
                    .map(|(_, path, occurrence)| (path.clone(), *occurrence))
            },
        );
        let (report, ran, acceptance) = hirsel_validate::validate_with_receipt(
            &artifact,
            hirsel_validate::SignatureInputs::Checked(&*views),
            self.verifier_policy,
            self.verifier.as_deref(),
            &mut vec![],
        )?;
        if ran {
            hirsel_intern::timings::count(hirsel_intern::timings::Count::VRuns);
        }
        Ok((report, acceptance))
    }
    /// Resolve a provider lookup as the engine's provider does: a path naming
    /// a definition this snapshot holds (or a source builtin), else a bare
    /// registry name. The output carries the manifest hash it was read under.
    pub fn lookup(&self, context: &FileContext, path: &hirsel_syntax::Path) -> LookupOutput {
        let result = (|| {
            if path
                .segments
                .iter()
                .any(|segment| segment.as_str().starts_with('$'))
            {
                return Err(QueryError::NotImplemented);
            }
            let definition =
                crate::queries::lookup_path(context, path).ok_or(QueryError::NotImplemented)?;
            let d = DefId::intern(&definition);
            if self.signatures.contains_key(&d)
                || self.headers.as_ref().is_some_and(|h| h.contains(d))
                || self.lookup.builtins.contains(&d)
            {
                Ok(definition)
            } else {
                self.lookup
                    .registry
                    .lookup_name(path)
                    .ok_or(QueryError::NotImplemented)
            }
        })();
        LookupOutput {
            manifest: self.lookup.manifest,
            result,
        }
    }
    /// `DeclarationCandidates(d)` at this snapshot's revision.
    pub fn declaration_candidates(&self, d: DefId) -> Option<ContentHash> {
        self.lookup.candidates.get(&d).copied()
    }
    /// The exact negative receipt captured from the failed query's complete key,
    /// or the canonical absent-query receipt. A diagnostic's debug spelling is
    /// never an input identity.
    pub fn failed_signature_receipt(&self, d: DefId) -> ContentHash {
        crate::queries::memos::optional(self.failed_signature_key(d))
    }
    pub(crate) fn failed_signature_key(&self, d: DefId) -> Option<ContentHash> {
        if let Some(product) = self.headers.as_ref().and_then(|headers| headers.product(d)) {
            // A refused demand has no failed-signature key of its own.
            return product.ok()?.entry.as_ref().map(|entry| {
                ContentHash::of_parts(
                    "hirsel.failed-signature.v1",
                    [
                        entry.memo.text.0.as_slice(),
                        entry.memo.reads.digest().0.as_slice(),
                    ],
                )
            });
        }
        self.lookup.failed.get(&d).copied().flatten()
    }
    pub fn epoch(&self) -> Revision {
        self.epoch
    }
    /// The session interner every view's rows live in.
    pub fn types(&self) -> &Arc<TypeInterner> {
        &self.types
    }
    /// Whether `d`'s header row has been produced in this snapshot and is
    /// still held: what a demand trace observes.
    pub fn header_ready(&self, d: DefId) -> bool {
        self.headers
            .as_ref()
            .is_some_and(|headers| headers.ready(d).is_some())
    }
    /// The public signature of `d`, or the error normalizing it reported.
    pub fn signature(&self, d: DefId) -> Option<Result<SignatureRef, QueryError>> {
        if let Some(product) = self.headers.as_ref().and_then(|headers| headers.product(d)) {
            return Some(product.and_then(|product| {
                product
                    .result
                    .as_ref()
                    .map(|output| output.value.clone())
                    .map_err(Clone::clone)
            }));
        }
        self.signatures.get(&d).cloned()
    }
    /// The view of `d` a body of `package` sees: an opaque type's private view
    /// inside its own package, its public signature everywhere else.
    pub fn body_view(
        &self,
        package: &PackageId,
        d: DefId,
    ) -> Option<Result<SignatureRef, QueryError>> {
        if &d.path().0.package == package
            && let Some(view) = self.private_view(d)
        {
            return Some(Ok(view));
        }
        self.signature(d)
    }
    fn private_view(&self, d: DefId) -> Option<SignatureRef> {
        private_view(&self.types, &self.private, self.headers.as_ref(), d)
    }
    fn membership(&self) -> Arc<SnapshotMembership> {
        // Naming immutable rows invokes no signature/private producer.
        let mut resident = self.membership.lock().unwrap();
        if let Some(membership) = &*resident {
            return membership.clone();
        }
        hirsel_intern::work::count(hirsel_intern::work::Counter::SnapshotMembershipBuilds);
        hirsel_intern::timings::count(hirsel_intern::timings::Count::SnapshotMembershipBuilds);
        let definitions = self
            .signatures
            .keys()
            .copied()
            .chain(
                self.headers
                    .iter()
                    .flat_map(|headers| headers.definitions()),
            )
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Box<[_]>>();
        let packages = definitions
            .iter()
            .map(|d| d.path().0.package.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(
            size_of::<SnapshotMembership>() + 2 * size_of::<usize>() + size_of_val(&*definitions),
        );
        weight.tree::<PackageId, ()>(packages.len());
        for package in &packages {
            weight.add(package.name.capacity() + package.version.capacity());
        }
        let lease = self
            .headers
            .as_ref()
            .and_then(|headers| headers.validation_lease(weight.bytes()).ok());
        let retained = self.headers.is_none() || lease.is_some();
        let membership = Arc::new(SnapshotMembership {
            definitions,
            packages,
            _lease: lease,
        });
        // A refused optional memo remains an exact transient computation.
        if retained {
            *resident = Some(membership.clone());
        }
        membership
    }
    /// Every definition in this snapshot's signature table, in path order.
    pub fn definitions(&self) -> Vec<DefId> {
        self.membership().definitions.to_vec()
    }
    /// The runtime types compile-time evaluation runs with. Each row is
    /// demanded when an evaluation names its path: `d`'s view in its own
    /// package with V's digest of it, else the registry's declaration.
    /// `entry` is the lowering's entry body, which finalizers name.
    pub(crate) fn comptime_runtime_types(
        self: &Arc<Self>,
        entry: Option<hirsel_tir::BodyId>,
    ) -> hirsel_interp::aggregate::RuntimeTypes {
        // Rows name V's identities, so V settles before the first row. A
        // refused universe publishes nothing; a row that reads it builds it again.
        let _ = self.validation_state();
        crate::queries::runtime_types_over(
            self.types.clone(),
            hirsel_interp::aggregate::Serialization::demanded(
                self.types.clone(),
                Arc::new(SnapshotRows {
                    snapshot: self.clone(),
                    entry,
                    validated: true,
                }),
            ),
        )
    }
    /// The runtime types a lowered program runs with. Each row is demanded
    /// when the run names its type, so a run holds rows only for types its
    /// closure reaches: `d`'s view in its own package, else the registry's
    /// declaration, with the zero digest. Finalizers name `entry`, the
    /// lowering's entry body; their definitions are in the closure because
    /// each type's drop is one of its dependencies.
    pub(crate) fn runtime_types(
        self: &Arc<Self>,
        entry: Option<hirsel_tir::BodyId>,
    ) -> hirsel_interp::aggregate::RuntimeTypes {
        crate::queries::runtime_types_over(
            self.types.clone(),
            hirsel_interp::aggregate::Serialization::demanded(
                self.types.clone(),
                Arc::new(SnapshotRows {
                    snapshot: self.clone(),
                    entry,
                    validated: false,
                }),
            ),
        )
    }
    /// `d`'s view in its own package (an opaque type's private view) with
    /// V's digest of it: what compile-time reflection of `d` reads.
    pub fn representation(
        &self,
        d: DefId,
    ) -> Option<Result<(SignatureRef, ValidationDigest), QueryError>> {
        Some(
            self.representation_identity(d)?
                .and_then(|view| Ok((view.signature().map_err(QueryError::from)?, view.digest()))),
        )
    }
    /// The identity of that view: holding it never pins a header row.
    pub(crate) fn representation_identity(
        &self,
        d: DefId,
    ) -> Option<Result<Arc<hirsel_validate::IdentifiedSignature>, QueryError>> {
        if let Some(state) = self.validation.get() {
            if let Some(own) = state.installed.get(&d.path().0.package) {
                return match self.signature(d)? {
                    Ok(_) => own.get(d).map(|v| Ok(v.identified().clone())),
                    Err(error) => Some(Err(error)),
                };
            }
            if let Some(view) = state
                .packages
                .get(&d.path().0.package)
                .and_then(|p| p.replacement(d))
            {
                return Some(Ok(view.identified().clone()));
            }
            let view = state.views.get(d);
            return match self.signature(d)? {
                Ok(_) => view.map(|v| Ok(v.identified().clone())),
                Err(error) => Some(Err(error)),
            };
        }
        let view = if let Some(private) = self.private_view(d) {
            private
        } else {
            match self.signature(d)? {
                Ok(public) => public,
                Err(error) => return Some(Err(error)),
            }
        };
        let product = if self.private_view(d).is_some() {
            self.private_views
                .get(&d)?
                .get_or_init(|| View::new(&self.types, view.clone(), None))
        } else {
            self.public_views
                .get(&d)?
                .get_or_init(|| self.view_of(d, &view, None))
        };
        Some(Ok(product.identified().clone()))
    }
    /// V's verdicts on this snapshot's views.
    pub fn validated(&self) -> Result<&ValidatedSignatures, hirsel_validate::ViewRefusal> {
        Ok(&self.validation_state()?.validated)
    }
    /// The universe V validates `package`'s bodies against.
    pub fn validation(
        &self,
        package: &PackageId,
    ) -> Result<&dyn hirsel_validate::CheckedEnvironment, hirsel_validate::ViewRefusal> {
        let state = self.validation_state()?;
        Ok(match state.packages.get(package) {
            Some(views) => views,
            None => &**state.base(package),
        })
    }
    /// The complete input of a package's base V universe. Artifact overlays
    /// record this before replacing an owner, preserving their source context.
    pub fn validation_input_digest(
        &self,
        package: &PackageId,
    ) -> Result<ContentHash, hirsel_validate::ViewRefusal> {
        let state = self.validation_state()?;
        Ok(match state.packages.get(package) {
            Some(views) => views.input_digest(),
            None => state.base(package).input_digest(),
        })
    }
    /// The summary of `package`'s V universe: its input identity, errors and
    /// first heap role, as installed packages publish them for importers.
    pub fn package_universe(
        &self,
        package: &PackageId,
    ) -> Result<hirsel_validate::PackageUniverse, hirsel_validate::ViewRefusal> {
        Ok(self.validation_state()?.universe(package))
    }
    /// What importers' universes hold of `package`'s public views.
    pub(crate) fn exported_universe(
        &self,
        package: &PackageId,
    ) -> Result<hirsel_validate::PackageUniverse, hirsel_validate::ViewRefusal> {
        Ok(self.validation_state()?.exported(package))
    }
    /// `package`'s published summaries when it is installed here, else its
    /// summaries as this snapshot's full tables compute them.
    pub(crate) fn installed_universe(
        &self,
        package: &PackageId,
    ) -> Result<InstalledUniverse, hirsel_validate::ViewRefusal> {
        if let Some(installed) = self.lookup.installed.get(package) {
            return Ok(installed.clone());
        }
        Ok(InstalledUniverse {
            own: self.package_universe(package)?,
            exported: self.exported_universe(package)?,
        })
    }
    /// The exact source identity `package`'s summary is published under.
    pub(crate) fn package_sources(&self, package: &PackageId) -> Option<ContentHash> {
        self.lookup.sources.get(package).copied()
    }
    /// `package`'s universe with `signature` in place of its declared view:
    /// compile-time evaluation validates a materialized owner against it.
    pub fn validation_with(
        &self,
        package: &PackageId,
        signature: SignatureRef,
    ) -> Result<Arc<PackageViews>, hirsel_validate::ViewRefusal> {
        let state = self.validation_state()?;
        let key = (
            package.clone(),
            hirsel_validate::digest(&self.types, &signature),
        );
        let mut producer = state.producers.thread_worker::<()>();
        loop {
            if let Some(views) = state.overlays.lock().unwrap().get(&key).cloned() {
                return Ok(views);
            }
            match producer.enter(ValidationProducer::Overlay(key.0.clone(), key.1), ()) {
                crate::memo::Entered::Settled => continue,
                crate::memo::Entered::Run => break,
            }
        }
        let (views, retain) = self.compute_validation_with(package, signature.clone())?;
        if !retain {
            return Ok(Arc::new(views));
        }
        let views = if let Some(delta) = views.retained_delta_bytes() {
            let bytes = delta
                .checked_add(crate::memo::retained::retained_signature_payload_bytes(
                    &signature,
                ))
                .and_then(|bytes| {
                    bytes.checked_add(
                        (3 * size_of::<((PackageId, ValidationDigest), Arc<PackageViews>)>()
                            + package.name.capacity()
                            + package.version.capacity()) as u64,
                    )
                })
                .ok_or_else(|| hirsel_validate::ViewRefusal::Unreadable {
                    definition: signature.definition,
                    cause: QueryError::Operational(hirsel_types::OperationalFailure {
                        operation: "validation overlay retention".into(),
                        message: "overlay size overflow".into(),
                    }),
                })?;
            let lease = match &self.headers {
                Some(headers) => match headers.validation_lease(bytes) {
                    Ok(lease) => lease,
                    Err(_) => {
                        // The exact transient result is already computed. A
                        // declined optional retention publishes no cache entry.
                        validation_retention_refused();
                        return Ok(Arc::new(views));
                    }
                },
                None => Arc::new(()) as Arc<dyn Send + Sync>,
            };
            let views = Arc::new(views.retain_owner(lease));
            state
                .overlays
                .lock()
                .unwrap()
                .entry(key)
                .or_insert_with(|| views.clone())
                .clone()
        } else {
            Arc::new(views)
        };
        Ok(views)
    }
    fn compute_validation_with(
        &self,
        package: &PackageId,
        signature: SignatureRef,
    ) -> Result<(PackageViews, bool), hirsel_validate::ViewRefusal> {
        let t = &self.types;
        let state = self.validation_state()?;
        let mut store =
            ValidatedSignatures::with_cache(self.verifier.clone(), self.verifier_policy);
        if state.installed.contains_key(package) {
            // An installed summary does not retain reverse readers. Materialize
            // its full table once in this exact immutable snapshot, then check
            // each replacement and all verdicts that read it against that table.
            // A refused construction publishes nothing and remains retryable.
            let mut retain = true;
            let mut producer = state.producers.thread_worker::<()>();
            let full = loop {
                if let Some(full) = state.overlay_bases.lock().unwrap().get(package).cloned() {
                    break full;
                }
                match producer.enter(ValidationProducer::Base(package.clone()), ()) {
                    crate::memo::Entered::Settled => continue,
                    crate::memo::Entered::Run => {}
                }
                hirsel_intern::timings::count(
                    hirsel_intern::timings::Count::RegistrationValidationViews,
                );
                hirsel_intern::work::count(
                    hirsel_intern::work::Counter::RegistrationValidationViews,
                );
                let visible = self
                    .visible(package)
                    .expect("installed closures are complete");
                let full = ViewTable::from_views(
                    t,
                    self.membership()
                        .definitions
                        .iter()
                        .copied()
                        .filter(|d| visible.contains(&d.path().0.package))
                        .filter_map(|d| match self.body_view(package, d)? {
                            Ok(view) => Some(Ok(
                                if package == &d.path().0.package && self.private_view(d).is_some()
                                {
                                    View::new(t, view, None)
                                } else {
                                    self.public_views[&d]
                                        .get_or_init(|| self.view_of(d, &view, None))
                                        .clone()
                                },
                            )),
                            Err(cause @ QueryError::Operational(_)) => {
                                Some(Err(hirsel_validate::ViewRefusal::Unreadable {
                                    definition: d,
                                    cause,
                                }))
                            }
                            Err(_) => None,
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    &mut store,
                    Some(&state.validated),
                )?;
                let full = if let Some(mut bytes) = full.retained_control_bytes() {
                    let refusal = || hirsel_validate::ViewRefusal::Unreadable {
                        definition: signature.definition,
                        cause: QueryError::Operational(hirsel_types::OperationalFailure {
                            operation: "validation base retention".into(),
                            message: "full table size overflow".into(),
                        }),
                    };
                    for view in full.views() {
                        if let Some(payload) = view.identified().held_signature() {
                            bytes = bytes
                                .checked_add(
                                    crate::memo::retained::retained_signature_payload_bytes(
                                        payload,
                                    ),
                                )
                                .ok_or_else(refusal)?;
                        }
                    }
                    let mut weight = crate::NativeMetadataWeight::default();
                    weight.tree::<PackageId, Arc<ViewTable>>(1);
                    weight.add(package.name.capacity() + package.version.capacity());
                    bytes = bytes.checked_add(weight.bytes()).ok_or_else(refusal)?;
                    let lease = match &self.headers {
                        Some(headers) => match headers.validation_lease(bytes) {
                            Ok(lease) => lease,
                            Err(_) => {
                                validation_retention_refused();
                                retain = false;
                                break Arc::new(full);
                            }
                        },
                        None => Arc::new(()) as Arc<dyn Send + Sync>,
                    };
                    Arc::new(full.retain_owner(lease))
                } else {
                    // Failed tables remain exact transient inputs. A later
                    // request can retry after its resource/context changes.
                    retain = false;
                    break Arc::new(full);
                };
                state
                    .overlay_bases
                    .lock()
                    .unwrap()
                    .insert(package.clone(), full.clone());
                break full;
            };
            drop(producer);
            return PackageViews::new(
                t,
                full,
                [View::new(t, signature, None)],
                &mut store,
                Some(&state.validated),
            )
            .map(|views| (views, retain));
        }
        let views = match state.packages.get(package) {
            Some(views) => views.replace(t, signature, &mut store, Some(&state.validated)),
            None => PackageViews::new(
                t,
                state.bases.get(package).unwrap_or(&state.views).clone(),
                [View::new(t, signature, None)],
                &mut store,
                Some(&state.validated),
            ),
        };
        views.map(|views| (views, true))
    }
}

impl hirsel_validate::ReplayEnvironment for SignatureSnapshot {
    fn checked(&self, package: &PackageId) -> Option<&dyn hirsel_validate::CheckedEnvironment> {
        self.validation(package)
            .map_err(|error| {
                if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
                    eprintln!("OWNER CHECKED REFUSAL {error:?}");
                }
                error
            })
            .ok()
    }

    fn checked_overlay(
        &self,
        package: &PackageId,
        overlay: SignatureRef,
    ) -> Option<Arc<PackageViews>> {
        self.validation_with(package, overlay)
            .map_err(|error| {
                if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
                    eprintln!("OWNER OVERLAY REFUSAL {error:?}");
                }
                error
            })
            .ok()
    }
    fn base_digest(&self, package: &PackageId) -> Option<ContentHash> {
        self.membership()
            .packages
            .contains(package)
            .then(|| {
                self.validation_input_digest(package)
                    .map_err(|error| {
                        if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
                            eprintln!("OWNER DIGEST REFUSAL {error:?}");
                        }
                        error
                    })
                    .ok()
            })
            .flatten()
    }
    fn views(
        &self,
        package: &PackageId,
        overlay: Option<SignatureRef>,
    ) -> Option<Box<dyn SignatureLookup + '_>> {
        self.base_digest(package)?;
        // A refused universe replays nothing: the execution runs again.
        Some(match overlay {
            Some(signature) => Box::new(self.validation_with(package, signature).ok()?),
            None => Box::new(self.validation(package).ok()?),
        })
    }
}

impl QueryEngine {
    /// This revision's snapshot, built once after its inputs changed. A failed
    /// signature is a settled verdict in it, so every reader shares it.
    pub fn signature_snapshot(&mut self) -> Arc<SignatureSnapshot> {
        self.prepare_private_summaries();
        let snapshot = self.signature_inputs();
        // A refused build publishes nothing; its first reader builds it again.
        let _ = snapshot.validation_state();
        self.adopt_completed_headers();
        snapshot
    }
    /// P needs immutable signatures and views, without eagerly validating V's universe.
    pub(crate) fn signature_inputs(&mut self) -> Arc<SignatureSnapshot> {
        self.adopt_completed_headers();
        self.prepare_signature_snapshot();
        let stamp = self.snapshot_stamp();
        if let Some((kept, snapshot)) = &self.snapshot
            && *kept == stamp
            && let Some(snapshot) = snapshot.get()
        {
            return snapshot;
        }
        let headers = self
            .declared_header_inputs()
            .unwrap_or_else(|| crate::queries::header_products::HeaderProducts::capture(self));
        self.inherit_demand_header_scope(&headers);
        let row_sources = self
            .source_paths()
            .into_iter()
            .filter_map(|path| {
                let id = DefId::intern(&path);
                self.unmarked_signature(id)?;
                self.signature_memo_source(id)
                    .map(|entry| (id, Arc::downgrade(entry)))
            })
            .collect();
        let mut signatures: Vec<_> = self
            .registry
            .signatures()
            .map(|s| (s.definition, Ok(s.clone())))
            .collect();
        for (path, constant) in &self.staged_constants {
            signatures.push((DefId::intern(path), Ok(constant.signature.clone())));
        }
        self.snapshot_epoch = self.snapshot_epoch.next();
        let previous = self.snapshot.as_ref().and_then(|(_, s)| s.get());
        let sources = self.package_source_bindings();
        let installed = self.installed_universes(&sources);
        let lookup = LookupInput {
            installed,
            sources,
            environment: self.environment_value(),
            summaries: self
                .result_store
                .clone()
                .filter(|store| store.verifier().is_some()),
            package_dependencies: self.declarations.inputs().package_dependencies.clone(),
            manifest: ContentHash::of_parts(
                "hirsel.provider.registry.v1",
                [
                    self.manifest_revision().0.as_slice(),
                    self.registry.revision().0.as_slice(),
                ],
            ),
            registry: self.registry.clone(),
            builtins: self.builtins.keys().map(DefId::intern).collect(),
            failed: signatures
                .iter()
                .filter(|(_, result)| result.is_err())
                .map(|(d, _)| (*d, self.failed_signature_key(d.path())))
                .collect(),
            candidates: self
                .source_paths()
                .iter()
                .map(|path| (DefId::intern(path), self.candidates_value(path)))
                .collect(),
        };
        let snapshot = Arc::new(
            SignatureSnapshot::build(
                self.snapshot_epoch,
                self.types.clone(),
                signatures,
                [],
                previous.as_deref(),
            )
            .with_headers(headers)
            .with_lookup(lookup)
            .with_verifier(
                self.result_store.as_ref().and_then(|s| s.verifier()),
                ContentHash::of_parts(
                    "hirsel.validate.private-evaluation.v1",
                    [
                        self.environment_value().0.as_slice(),
                        self.store_identity.0.as_slice(),
                    ],
                ),
            ),
        );
        self.snapshot = Some((stamp, SnapshotCache::new(&snapshot)));
        self.snapshot_sources = row_sources;
        snapshot
    }
    /// Each package's source binding: the environment, its closure manifest
    /// and the complete sources of every package in its dependency closure.
    /// A package with an incomplete closure or unloaded sources has none.
    fn package_source_bindings(&self) -> BTreeMap<PackageId, ContentHash> {
        let dependencies = &self.declarations.inputs().package_dependencies;
        let environment = self.environment_value();
        let mut bindings = BTreeMap::new();
        'packages: for package in dependencies.keys() {
            let mut closure = std::collections::BTreeSet::new();
            let mut pending = vec![package];
            while let Some(next) = pending.pop() {
                if !closure.insert(next) {
                    continue;
                }
                let Some(more) = dependencies.get(next) else {
                    continue 'packages;
                };
                pending.extend(more);
            }
            let mut bytes = Vec::new();
            bytes.extend(environment.0);
            bytes.extend(self.package_environment(package).package_manifest_hash.0);
            for member in &closure {
                let Some(sources) = self.package_sources.get(*member) else {
                    continue 'packages;
                };
                for text in [member.name.as_str(), member.version.as_str()] {
                    bytes.extend((text.len() as u64).to_le_bytes());
                    bytes.extend(text.as_bytes());
                }
                bytes.extend(sources.0);
            }
            bindings.insert(
                package.clone(),
                ContentHash::of_parts("hirsel.front.universe.sources.v1", [bytes.as_slice()]),
            );
        }
        bindings
    }
    /// The dependencies whose summaries are published for exactly their
    /// current sources. Any other package is checked from its full table.
    fn installed_universes(
        &self,
        sources: &BTreeMap<PackageId, ContentHash>,
    ) -> BTreeMap<PackageId, InstalledUniverse> {
        // A summary stands for V's verdicts: only this machine's V, holding
        // the store's local verifier authority, produced one it may read.
        let Some(store) = self
            .result_store
            .as_ref()
            .filter(|store| store.verifier().is_some())
        else {
            return BTreeMap::new();
        };
        let environment = self.environment_value();
        self.package_manifests
            .keys()
            .filter_map(|package| {
                let query =
                    crate::queries::universe_query(environment, package, *sources.get(package)?);
                let association = store.lookup_product(query.address)?;
                let bytes = store.get_content(association.reference)?;
                Some((package.clone(), InstalledUniverse::decode(&bytes)?))
            })
            .collect()
    }
    /// Observe a published signature without another phase memo query. The
    /// snapshot must describe the current universe and the immutable producer
    /// must still be the exact unmarked source that supplied its row.
    pub(crate) fn current_snapshot_signature(&self, d: DefId) -> Option<SignatureRef> {
        let (stamp, _) = self.snapshot.as_ref()?;
        if *stamp != self.snapshot_stamp() {
            return None;
        }
        self.retained_snapshot_signature(d)?.ok()
    }

    fn retained_snapshot_signature(&self, d: DefId) -> Option<Result<SignatureRef, QueryError>> {
        let source = self.snapshot_sources.get(&d)?.upgrade()?;
        let current = self.signature_memo_source(d)?;
        if !Arc::ptr_eq(&source, current)
            || !current.consistent
            || self.graph.is_dirty(&current.memo.key)
        {
            return None;
        }
        self.snapshot.as_ref()?.1.get()?.signature(d)
    }

    pub(crate) fn unmarked_signature(&self, d: DefId) -> Option<Result<SignatureRef, QueryError>> {
        let current = self.signature_memo_source(d)?;
        if !current.consistent
            || self.graph.is_dirty(&current.memo.key)
            || !self.graph.memos_of(d).any(|key| *key == current.memo.key)
        {
            return None;
        }
        // A source-index carry owns the current header memo and its reverse
        // reads even when it did not retain a frozen signature universe. The
        // unmarked producer is authoritative; rebuilding a view need not query
        // or stamp every unchanged header again.
        Some(current.result().map(|output| output.value.clone()))
    }

    /// The inputs a snapshot built now would be built from.
    pub(crate) fn snapshot_stamp(&self) -> SnapshotStamp {
        SnapshotStamp {
            declarations: self.declarations.revision(),
            revision: self.snapshot_inputs_revision,
            environment: self.environment.clone(),
            limits: self.comptime_limits,
            manifest: Some(self.declarations.inputs().manifest_revision),
            unavailable: self.unavailable,
            verify: self.verify,
        }
    }
    /// The explicit inputs of module resolution, to change.
    pub fn module_inputs_mut(&mut self) -> &mut ModuleInputs {
        self.declarations.inputs_mut()
    }
    /// `DeclarationCandidates(path)`: H(count, then (file path, header hash)
    /// per candidate in source order), or None for a path with no item.
    pub fn declaration_candidates(&self, path: &DefPath) -> Option<ContentHash> {
        self.declarations.candidates(path)
    }
    /// The frozen declaration index, published to the phases that read it.
    pub(crate) fn frozen_declarations(&mut self) -> Arc<Declarations> {
        self.declarations.frozen()
    }
    /// Signatures changed outside a source revision (a staged constant).
    pub(crate) fn retire_snapshot(&mut self) {
        self.snapshot_inputs_revision += 1;
    }
}

/// The inputs a kept snapshot was built from.
#[derive(PartialEq)]
pub(crate) struct SnapshotStamp {
    declarations: u64,
    revision: u64,
    environment: QueryEnvironment,
    limits: crate::ComptimeLimits,
    manifest: Option<ContentHash>,
    unavailable: Option<QueryKind>,
    verify: crate::Verify,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memo::{ReadKey, ReadsBuilder};
    use hirsel_syntax::ItemKind;
    use hirsel_types::{SignatureKind, SignatureProvider, Type, TypeKind};

    fn constant(t: &TypeInterner, name: &str, primitive: hirsel_types::Primitive) -> SignatureRef {
        let mut path = DefPath::core("snapshot_test");
        path.0.name = name.into();
        Arc::new(hirsel_types::Signature {
            metadata: Default::default(),
            definition: DefId::intern(&path),
            kind: SignatureKind::Const {
                ty: t.intern(Type::with_kind(TypeKind::Primitive(primitive))),
                generics: Default::default(),
            },
            fingerprint: ContentHash::of_bytes(name.as_bytes()),
        })
    }

    #[test]
    fn package_validation_universes_cover_selected_dependencies_without_importers() {
        let types = TypeInterner::new(None);
        let dependency = PackageId {
            name: "dependency".into(),
            version: "1".into(),
        };
        let middle = PackageId {
            name: "middle".into(),
            version: "1".into(),
        };
        let application = PackageId {
            name: "application".into(),
            version: "1".into(),
        };
        let row = |package: &PackageId, name: &str, primitive| {
            let mut row = (*constant(&types, name, primitive)).clone();
            let mut path = row.definition.path().clone();
            path.0.package = package.clone();
            row.definition = DefId::intern(&path);
            Arc::new(row)
        };
        let dep = row(&dependency, "Dep", hirsel_types::Primitive::U64);
        let mid = row(&middle, "Mid", hirsel_types::Primitive::U64);
        let app = row(&application, "App", hirsel_types::Primitive::U64);
        let core = constant(&types, "Core", hirsel_types::Primitive::U64);
        let dependencies = BTreeMap::from([
            (dependency.clone(), vec![]),
            (middle.clone(), vec![dependency.clone()]),
            (application.clone(), vec![middle.clone()]),
        ]);
        let snapshot = |dep: SignatureRef, extra: Option<SignatureRef>, dependencies| {
            let rows = [core.clone(), dep, mid.clone(), app.clone()]
                .into_iter()
                .chain(extra);
            SignatureSnapshot::build(
                Revision::FIRST,
                types.clone(),
                rows.map(|row| (row.definition, Ok(row))),
                [],
                None,
            )
            .with_lookup(LookupInput {
                package_dependencies: dependencies,
                ..Default::default()
            })
        };
        let first = snapshot(dep.clone(), None, dependencies.clone());
        assert!(
            first
                .validation(&dependency)
                .unwrap()
                .view(app.definition)
                .is_none()
        );
        assert!(
            first
                .validation(&middle)
                .unwrap()
                .view(dep.definition)
                .is_some()
        );
        assert!(
            first
                .validation(&dependency)
                .unwrap()
                .view(core.definition)
                .is_some()
        );
        let extra = row(&application, "Extra", hirsel_types::Primitive::U8);
        let added = snapshot(dep.clone(), Some(extra.clone()), dependencies.clone());
        assert_eq!(
            first.validation_input_digest(&dependency),
            added.validation_input_digest(&dependency)
        );
        assert_eq!(
            first.validation_input_digest(&middle),
            added.validation_input_digest(&middle)
        );
        assert_ne!(
            first.validation_input_digest(&application),
            added.validation_input_digest(&application)
        );
        let mut changed = (*dep).clone();
        changed.kind = SignatureKind::Const {
            ty: types.intern(Type::with_kind(TypeKind::Primitive(
                hirsel_types::Primitive::U8,
            ))),
            generics: Default::default(),
        };
        assert_eq!(
            changed.fingerprint, dep.fingerprint,
            "producer fingerprints do not confer V authority"
        );
        let changed = snapshot(Arc::new(changed), Some(extra), dependencies.clone());
        for package in [&dependency, &middle, &application] {
            assert_ne!(
                added.validation_input_digest(package),
                changed.validation_input_digest(package),
                "a valid changed selected row invalidates its complete universe"
            );
        }
        let mut incomplete = dependencies;
        incomplete.remove(&dependency);
        let incomplete = snapshot(dep, None, incomplete);
        assert!(
            incomplete
                .validation(&middle)
                .unwrap()
                .view(app.definition)
                .is_some(),
            "incomplete package inputs retain full-world validation"
        );
    }

    #[test]
    fn installed_registration_overlays_construct_one_table_per_snapshot_and_package() {
        use hirsel_intern::work::{self, Counter};
        let types = TypeInterner::new(None);
        let rows = [
            constant(&types, "first", hirsel_types::Primitive::U64),
            constant(&types, "second", hirsel_types::Primitive::U8),
        ];
        let package = rows[0].definition.path().0.package.clone();
        let build = |rows: &[SignatureRef], installed| {
            SignatureSnapshot::build(
                Revision::FIRST,
                types.clone(),
                rows.iter().map(|row| (row.definition, Ok(row.clone()))),
                [],
                None,
            )
            .with_lookup(LookupInput {
                package_dependencies: BTreeMap::from([(package.clone(), vec![])]),
                installed,
                ..Default::default()
            })
        };
        let (table, allocation) = crate::test_alloc::measured(|| {
            ViewTable::new(
                &types,
                rows.iter().cloned(),
                None,
                &mut ValidatedSignatures::default(),
                None,
            )
            .unwrap()
        });
        assert!(allocation.live > 0);
        assert!(allocation.live as u64 <= table.retained_control_bytes().unwrap());
        drop(table);
        let source = build(&rows, BTreeMap::new());
        let installed = BTreeMap::from([(
            package.clone(),
            source.installed_universe(&package).unwrap(),
        )]);
        let snapshot = build(&rows, installed.clone());
        let before = work::read(Counter::RegistrationValidationViews);
        let overlays_before = work::read(Counter::ValidationOverlayConstructions);
        let memberships_before = work::read(Counter::SnapshotMembershipBuilds);
        let mut identities = std::collections::BTreeSet::new();

        for row in &rows {
            // Distinct registrations and repeated receipt replays see the same
            // base, but each replacement must retain its own actual input.
            for primitive in [hirsel_types::Primitive::U16, hirsel_types::Primitive::U32] {
                let mut replacement = (**row).clone();
                let SignatureKind::Const { ty, .. } = &mut replacement.kind else {
                    unreachable!()
                };
                *ty = types.intern(Type::with_kind(TypeKind::Primitive(primitive)));
                let replacement = Arc::new(replacement);
                let expected = View::new(&types, replacement.clone(), None).digest();
                identities.insert((row.definition, primitive as u8));
                let mut shared = None;
                for _ in 0..2 {
                    let views = snapshot
                        .validation_with(&package, replacement.clone())
                        .unwrap();
                    if let Some(previous) = &shared {
                        assert!(Arc::ptr_eq(previous, &views));
                    }
                    shared = Some(views.clone());
                    assert_eq!(views.view(row.definition).unwrap().digest(), expected);
                    let other = &rows[usize::from(row.definition == rows[0].definition)];
                    assert_eq!(
                        views.view(other.definition).unwrap().digest(),
                        View::new(&types, other.clone(), None).digest()
                    );
                    assert!(views.universe_errors().1.is_empty());
                }
            }
        }
        assert_eq!(
            work::read(Counter::ValidationOverlayConstructions) - overlays_before,
            identities.len() as u64
        );
        assert_eq!(
            work::read(Counter::SnapshotMembershipBuilds) - memberships_before,
            1
        );
        let mut invalid = (*rows[0]).clone();
        invalid.kind = SignatureKind::Const {
            ty: types.intern(Type::with_kind(TypeKind::Error)),
            generics: Default::default(),
        };
        assert!(
            !snapshot
                .validation_with(&package, Arc::new(invalid))
                .unwrap()
                .universe_errors()
                .1
                .is_empty()
        );
        assert!(
            snapshot
                .validation_with(&package, rows[0].clone())
                .unwrap()
                .universe_errors()
                .1
                .is_empty()
        );
        assert_eq!(work::read(Counter::RegistrationValidationViews) - before, 1);

        // The producer fingerprint and revision alone cannot identify a scope.
        let mut changed = (*rows[1]).clone();
        changed.kind = SignatureKind::Const {
            ty: types.intern(Type::with_kind(TypeKind::Error)),
            generics: Default::default(),
        };
        assert_eq!(changed.fingerprint, rows[1].fingerprint);
        let changed = build(&[rows[0].clone(), Arc::new(changed)], installed);
        let views = changed.validation_with(&package, rows[0].clone()).unwrap();
        assert!(!views.universe_errors().1.is_empty());
        assert_eq!(work::read(Counter::RegistrationValidationViews) - before, 2);

        let pinned = snapshot.validation_with(&package, rows[0].clone()).unwrap();
        let state = snapshot.validation_state().unwrap();
        assert!(state.evict_cached());
        let before = work::read(Counter::ValidationOverlayConstructions);
        let reused = snapshot.validation_with(&package, rows[0].clone()).unwrap();
        assert!(Arc::ptr_eq(&pinned, &reused));
        assert_eq!(work::read(Counter::ValidationOverlayConstructions), before);
        drop(reused);
        drop(pinned);
        assert!(state.evict_cached());
        assert!(state.overlay_bases.lock().unwrap().is_empty());
        let fresh = snapshot.validation_with(&package, rows[0].clone()).unwrap();
        assert!(fresh.universe_errors().1.is_empty());
        assert_eq!(
            work::read(Counter::ValidationOverlayConstructions) - before,
            1
        );
    }

    #[test]
    fn signature_inputs_demand_only_the_observed_header_until_a_universe_consumer_reads_all() {
        struct Header(Arc<Mutex<Vec<DefPath>>>);
        impl SignaturePhase for Header {
            fn signature(
                &self,
                types: &TypeInterner,
                _: &hirsel_types::TypeMemo,
                syntax: Arc<ItemSyntax>,
                _: &Declarations,
                _: &mut PhaseProducts,
                _: &mut dyn SignatureProvider,
            ) -> Result<QueryOutput<Signature>, QueryError> {
                self.0.lock().unwrap().push(syntax.item.definition.clone());
                Ok(crate::queries::output(Signature {
                    metadata: Default::default(),
                    definition: DefId::intern(&syntax.item.definition),
                    kind: SignatureKind::Const {
                        ty: types.intern(Type::unit()),
                        generics: Default::default(),
                    },
                    fingerprint: syntax.text_hash,
                }))
            }
        }
        crate::with_compiler_stack(|| {
            let trace = Arc::new(Mutex::new(Vec::new()));
            let mut adapters = crate::phases::engine_only_adapters();
            adapters.signature = Some(Arc::new(Header(trace.clone())));
            let mut engine = QueryEngine::new(default_environment(), adapters);
            let context = default_context();
            let file = engine
                .sources
                .add(
                    "demand.hir",
                    r#"extern "used" fn used()
extern "unused" fn unused()"#
                        .into(),
                )
                .unwrap();
            let parsed = engine.parse_file(file, &context).unwrap();
            let used = parsed.value.items[0].item.definition.clone();
            let unused = parsed.value.items[1].item.definition.clone();
            let snapshot = engine.signature_inputs();
            assert!(trace.lock().unwrap().is_empty());
            let first = snapshot.signature(DefId::intern(&used)).unwrap().unwrap();
            let second = snapshot.signature(DefId::intern(&used)).unwrap().unwrap();
            assert!(Arc::ptr_eq(&first, &second));
            assert_eq!(*trace.lock().unwrap(), vec![used]);
            snapshot.validation(&context.package);
            assert!(trace.lock().unwrap().contains(&unused));
        });
    }

    #[test]
    fn live_header_demand_retains_the_shared_written_owner() {
        struct Header;
        impl SignaturePhase for Header {
            fn signature(
                &self,
                types: &TypeInterner,
                _: &hirsel_types::TypeMemo,
                syntax: Arc<ItemSyntax>,
                _: &Declarations,
                _: &mut PhaseProducts,
                _: &mut dyn SignatureProvider,
            ) -> Result<QueryOutput<Signature>, QueryError> {
                Ok(crate::queries::output(Signature {
                    metadata: Default::default(),
                    definition: DefId::intern(&syntax.item.definition),
                    kind: SignatureKind::Const {
                        ty: types.intern(Type::unit()),
                        generics: Default::default(),
                    },
                    fingerprint: syntax.text_hash,
                }))
            }
        }
        crate::with_compiler_stack(|| {
            let mut adapters = crate::phases::engine_only_adapters();
            adapters.signature = Some(Arc::new(Header));
            let store = Arc::new(crate::store::MemStore::default());
            let mut engine =
                QueryEngine::new(default_environment(), adapters).with_result_store(store.clone());
            let context = default_context();
            let file = engine
                .sources
                .add(
                    "archived.hir",
                    "fn used() { () }\nfn unused() { () }".into(),
                )
                .unwrap();
            let parsed = engine.parse_file(file, &context).unwrap();
            let path = parsed.value.items[0].item.definition.clone();
            let written = Arc::downgrade(&parsed.value.items[0]);
            engine.adapters.lexer = None;
            engine.adapters.parser = None;
            let snapshot = engine.signature_inputs();
            drop(parsed);
            drop(engine);
            assert!(
                written.upgrade().is_some(),
                "a live demand source shares the immutable written owner"
            );
            assert!(snapshot.signature(DefId::intern(&path)).unwrap().is_ok());
        });
    }

    /// Receipt observation uses a settled immutable row without touching its
    /// phase memo, but a changed header must still be queried before reuse.
    #[test]
    fn live_observation_reuses_a_settled_snapshot_and_requeries_an_edited_header() {
        struct Header;
        impl SignaturePhase for Header {
            fn signature(
                &self,
                t: &TypeInterner,
                _: &hirsel_types::TypeMemo,
                syntax: Arc<ItemSyntax>,
                _: &Declarations,
                _: &mut PhaseProducts,
                _: &mut dyn SignatureProvider,
            ) -> Result<QueryOutput<Signature>, QueryError> {
                let ItemKind::Function { abi: Some(abi), .. } = &syntax.item.kind else {
                    panic!("header fixture")
                };
                Ok(QueryOutput {
                    value: Signature {
                        metadata: Default::default(),
                        definition: DefId::intern(&syntax.item.definition),
                        kind: SignatureKind::Const {
                            ty: t.intern(Type::unit()),
                            generics: Default::default(),
                        },
                        fingerprint: ContentHash::of_bytes(abi.literal.as_bytes()),
                    },
                    dependencies: vec![],
                    diagnostics: vec![],
                })
            }
        }
        let mut adapters = crate::phases::engine_only_adapters();
        adapters.signature = Some(Arc::new(Header));
        let mut engine = QueryEngine::new(default_environment(), adapters);
        let context = default_context();
        let file = engine
            .sources
            .add("snapshot.hir", "extern \"first\" fn probe()".into())
            .unwrap();
        let parsed = engine.parse_file(file, &context).unwrap();
        let owner = parsed.value.items[0].item.definition.clone();
        let key = ReadKey::Signature(DefId::intern(&owner));
        let snapshot = engine.signature_snapshot();
        let expected = crate::queries::memos::signature_value(Some(
            snapshot
                .signature(DefId::intern(&owner))
                .unwrap()
                .unwrap()
                .fingerprint,
        ));
        let before = engine.memo_stats().touched;
        assert_eq!(engine.observe(&owner, &key), Some(expected));
        let executed = ReadKey::ExecutedSignature(DefId::intern(&owner));
        assert_eq!(engine.observe(&owner, &executed), Some(expected));
        assert_eq!(
            engine.memo_stats().touched,
            before,
            "settled observation reads the snapshot, not phase memos"
        );
        let budget = crate::runtime::ProcessBudget::new(64 << 20);
        let bytes = engine.source_index_bytes().unwrap();
        let state = engine
            .into_source_index(budget.reserve(bytes).unwrap())
            .unwrap();
        let mut adapters = crate::phases::engine_only_adapters();
        adapters.signature = Some(Arc::new(Header));
        let mut engine = QueryEngine::new(default_environment(), adapters);
        assert!(state.restore(&mut engine).is_ok());
        assert_eq!(
            engine.memo_stats().touched,
            0,
            "a fresh engine counts only new work"
        );
        let before = engine.memo_stats().touched;
        let rebuilt = engine.signature_snapshot();
        assert_eq!(
            rebuilt
                .signature(DefId::intern(&owner))
                .unwrap()
                .unwrap()
                .fingerprint,
            snapshot
                .signature(DefId::intern(&owner))
                .unwrap()
                .unwrap()
                .fingerprint,
        );
        assert_eq!(
            engine.memo_stats().touched,
            before,
            "source-only carry rebuilds the universe without touching unchanged headers"
        );

        let file = engine
            .sources
            .add("snapshot.hir", "extern \"second\" fn probe()".into())
            .unwrap();
        engine.parse_file(file, &context).unwrap();
        let changed = engine.observe(&owner, &key).unwrap();
        assert_ne!(
            changed, expected,
            "an old snapshot cannot authorize a changed producer"
        );
        let current = engine.signature_snapshot();
        assert_eq!(
            changed,
            crate::queries::memos::signature_value(Some(
                current
                    .signature(DefId::intern(&owner))
                    .unwrap()
                    .unwrap()
                    .fingerprint
            ))
        );
    }

    /// Eight workers read one snapshot through their own `FrozenProvider`s
    /// with no lock, each recording the same typed reads.
    #[test]
    fn eight_workers_read_one_snapshot_and_record_the_same_reads() {
        let types = TypeInterner::new(None);
        let signatures: Vec<SignatureRef> = (0..64)
            .map(|i| constant(&types, &format!("c{i}"), hirsel_types::Primitive::U64))
            .collect();
        let snapshot = Arc::new(SignatureSnapshot::new(
            Revision::from_u64(1).unwrap(),
            types.clone(),
            signatures.iter().map(|s| (s.definition, Ok(s.clone()))),
            [],
            None,
        ));
        let owner = signatures[0].definition.path().0.package.clone();
        let reads = hirsel_intern::jobs::map(
            hirsel_intern::jobs::Jobs::new(8),
            (0..8).collect::<Vec<usize>>(),
            |_| 1,
            |_| {
                let mut reads = ReadsBuilder::new();
                let mut provider = FrozenProvider::new(&snapshot, owner.clone(), &mut reads);
                for signature in &signatures {
                    let read = provider.signature(signature.definition).unwrap();
                    assert!(Arc::ptr_eq(&read, signature));
                    provider.validation_view(signature.definition).unwrap();
                }
                provider.universe_errors();
                reads.finish().unwrap()
            },
        );
        assert_eq!(reads.len(), 8);
        assert!(reads.iter().all(|r| r == &reads[0]));
        assert!(reads[0].iter().any(|(key, value)| {
            *key == ReadKey::Signature(signatures[3].definition)
                && *value == signatures[3].fingerprint
        }));
    }

    fn cached_views(
        types: Arc<TypeInterner>,
        public: Vec<SignatureRef>,
        private: Vec<SignatureRef>,
        cache: Arc<hirsel_validate::VerifierCache>,
    ) -> SignatureSnapshot {
        let snapshot = SignatureSnapshot::build(
            Revision::FIRST,
            types,
            public
                .into_iter()
                .map(|signature| (signature.definition, Ok(signature))),
            private,
            None,
        )
        .with_verifier(
            Some(cache),
            ContentHash::of_bytes(b"signature-view-test-policy"),
        );
        snapshot.validation_state();
        snapshot
    }
    #[test]
    fn reflection_and_validation_share_only_the_exact_immutable_view_product() {
        let types = TypeInterner::new(None);
        let signature = constant(&types, "shared", hirsel_types::Primitive::U64);
        let d = signature.definition;
        let build = |signature: SignatureRef, previous: Option<&SignatureSnapshot>| {
            SignatureSnapshot::build(
                Revision::FIRST,
                types.clone(),
                [(d, Ok(signature))],
                [],
                previous,
            )
        };
        let first = build(signature.clone(), None);
        let reflected = first.representation(d).unwrap().unwrap().1;
        let product = first.public_views[&d].get().unwrap().identified().clone();
        let state = first.validation_state().unwrap();
        assert!(Arc::ptr_eq(
            &product,
            state.views.get(d).unwrap().identified()
        ));
        assert_eq!(reflected, state.views.get(d).unwrap().digest());
        let next = build(signature.clone(), Some(&first));
        let next_state = next.validation_state();
        assert!(Arc::ptr_eq(
            &product,
            next_state.unwrap().views.get(d).unwrap().identified()
        ));

        let mut changed = (*signature).clone();
        changed.kind = SignatureKind::Const {
            ty: types.intern(Type::with_kind(TypeKind::Error)),
            generics: Default::default(),
        };
        let changed = build(Arc::new(changed), Some(&next));
        let changed_state = changed.validation_state().unwrap();
        assert!(!Arc::ptr_eq(
            &product,
            changed_state.views.get(d).unwrap().identified()
        ));
        assert_ne!(reflected, changed_state.views.get(d).unwrap().digest());
        assert!(!changed_state.views.universe_errors().1.is_empty());
    }

    #[test]
    fn durable_verdicts_recompute_independent_digests_and_reject_changed_views() {
        use hirsel_intern::work::{self, Counter};
        let cache = Arc::new(hirsel_validate::VerifierCache::default());
        let types = TypeInterner::new(None);
        let signature = constant(&types, "durable", hirsel_types::Primitive::U64);
        let snapshot = cached_views(
            types.clone(),
            vec![signature.clone()],
            vec![],
            cache.clone(),
        );
        assert!(!snapshot.validated().unwrap().is_empty());
        // Ordinary view bytes cross interners; only the shared local verifier
        // capability retains proof. No producer seed carries success claims.
        let views = vec![signature.clone()];
        let (_, roots) =
            hirsel_types::with_table_collection(|| serde_json::to_vec(&views).unwrap());
        let (table, _) = hirsel_types::encode_table(&types, &roots);
        let bytes = hirsel_types::with_table_encoding(&types, &roots, || {
            serde_json::to_vec(&views).unwrap()
        });
        let other = TypeInterner::new(None);
        other.intern(Type::with_kind(TypeKind::Primitive(
            hirsel_types::Primitive::U8,
        )));
        let ids = hirsel_types::decode_table(&other, &table);
        let mut decoded: Vec<SignatureRef> =
            hirsel_types::with_table_decoding(&ids, || serde_json::from_slice(&bytes).unwrap());
        let before = work::read(Counter::ValidateSignatureRows);
        let imported = cached_views(other.clone(), decoded.clone(), vec![], cache.clone());
        assert_eq!(work::read(Counter::ValidateSignatureRows), before);
        assert!(
            imported
                .validation(&signature.definition.path().0.package)
                .unwrap()
                .universe_errors()
                .1
                .is_empty()
        );
        let changed = Arc::make_mut(&mut decoded[0]);
        let fingerprint = changed.fingerprint;
        let SignatureKind::Const { ty, .. } = &mut changed.kind else {
            unreachable!()
        };
        *ty = other.intern(Type::with_kind(TypeKind::Error));
        assert_eq!(changed.fingerprint, fingerprint);
        let changed = cached_views(other, decoded, vec![], cache);
        assert_eq!(work::read(Counter::ValidateSignatureRows), before + 1);
        assert!(
            !changed
                .validation(&signature.definition.path().0.package)
                .unwrap()
                .universe_errors()
                .1
                .is_empty()
        );
        assert_eq!(changed.validated().unwrap().successful().count(), 0);
    }

    #[test]
    fn durable_private_views_reject_shape_changes_without_public_fingerprint_changes() {
        use hirsel_intern::work::{self, Counter};
        use hirsel_types::{Allocation, AuthoritySummary, Bound, Layout, TypeDefinition};
        let types = TypeInterner::new(None);
        let mut public = (*constant(&types, "Opaque", hirsel_types::Primitive::U64)).clone();
        public.metadata.visibility = hirsel_syntax::Visibility::PublicOpaque;
        public.kind = SignatureKind::Type(Box::new(TypeDefinition {
            loop_source: None,
            format: None,
            destructor_allocation: Allocation::NoAlloc,
            generics: vec![],
            representation: None,
            bounds: vec![Bound::Plain],
            authority: AuthoritySummary::inert(),
            capability: None,
            implementation_authority: None,
            serializable: false,
            serializable_check: None,
            layout: Layout::default(),
            distinct: false,
            resource: false,
            drop: None,
            fact: None,
            captures: None,
        }));
        let mut private = public.clone();
        let SignatureKind::Type(declaration) = &mut private.kind else {
            unreachable!()
        };
        declaration.representation = Some(types.intern(Type::with_kind(TypeKind::Primitive(
            hirsel_types::Primitive::U64,
        ))));
        let public = Arc::new(public);
        let cache = Arc::new(hirsel_validate::VerifierCache::default());
        let mut private = Arc::new(private);
        let snapshot = cached_views(
            types.clone(),
            vec![public.clone()],
            vec![private.clone()],
            cache.clone(),
        );
        assert_eq!(snapshot.private.len(), 1);
        let before = work::read(Counter::ValidateSignatureRows);
        let imported = cached_views(
            types.clone(),
            vec![public.clone()],
            vec![private.clone()],
            cache.clone(),
        );
        assert_eq!(work::read(Counter::ValidateSignatureRows), before);
        let package = &public.definition.path().0.package;
        assert!(
            imported
                .validation(package)
                .unwrap()
                .universe_errors()
                .1
                .is_empty()
        );
        let changed_private = Arc::make_mut(&mut private);
        let SignatureKind::Type(declaration) = &mut changed_private.kind else {
            unreachable!()
        };
        // String fails the declared plain bound; the unchanged public view
        // still has a reusable pass, while this private view must be checked.
        declaration.representation = Some(types.intern(Type::with_kind(TypeKind::Primitive(
            hirsel_types::Primitive::String,
        ))));
        assert_eq!(changed_private.fingerprint, public.fingerprint);
        let changed = cached_views(types, vec![public.clone()], vec![private], cache);
        assert_eq!(work::read(Counter::ValidateSignatureRows), before + 1);
        assert!(
            !changed
                .validation(package)
                .unwrap()
                .universe_errors()
                .1
                .is_empty()
        );
        assert_eq!(changed.validated().unwrap().successful().count(), 1);
    }
    #[test]
    fn local_verdicts_reuse_without_granting_declaration_authority() {
        use hirsel_intern::work::{self, Counter};
        let store = Arc::new(crate::store::MemStore::default());
        let mut original =
            QueryEngine::new(default_environment(), crate::phases::engine_only_adapters())
                .with_result_store(store.clone());
        original.signature_snapshot();
        let injected = constant(
            original.types(),
            "not_declared",
            hirsel_types::Primitive::U64,
        );
        let mut imported =
            QueryEngine::new(default_environment(), crate::phases::engine_only_adapters())
                .with_result_store(store);
        let before = work::read(Counter::ValidateSignatureRows);
        let snapshot = imported.signature_snapshot();
        assert_eq!(work::read(Counter::ValidateSignatureRows), before);
        assert!(snapshot.signature(injected.definition).is_none());
    }
    #[test]
    fn frozen_negative_and_validation_receipts_match_live_observations() {
        struct Reject;
        impl SignaturePhase for Reject {
            fn signature(
                &self,
                _: &TypeInterner,
                _: &hirsel_types::TypeMemo,
                syntax: Arc<ItemSyntax>,
                _: &Declarations,
                _: &mut PhaseProducts,
                _: &mut dyn SignatureProvider,
            ) -> Result<QueryOutput<Signature>, QueryError> {
                Err(QueryError::Diagnostics(vec![Diagnostic::error(
                    const { hirsel_diag::RuleId::of("LANG-7") },
                    syntax.item.name_span,
                    "fixture rejection",
                )]))
            }
        }
        let mut adapters = crate::phases::engine_only_adapters();
        adapters.signature = Some(Arc::new(Reject));
        let mut engine = QueryEngine::new(default_environment(), adapters);
        let file = engine
            .sources
            .add("failed.hir", "fn failed() -> u64 { 1 }".into())
            .unwrap();
        let parsed = engine.parse_file(file, &default_context()).unwrap();
        let owner = parsed.value.items[0].item.definition.clone();
        let failed = DefId::intern(&owner);
        let mut missing = owner.clone();
        missing.0.name = "absent".into();
        let absent = DefId::intern(&missing);
        let snapshot = engine.signature_snapshot();
        let mut reads = ReadsBuilder::new();
        let mut provider = FrozenProvider::new(&snapshot, owner.0.package.clone(), &mut reads);
        assert!(provider.signature(failed).is_err());
        assert!(provider.signature(absent).is_err());
        provider.declaration_candidates(failed);
        provider.declaration_candidates(absent);
        provider.validation_view(failed);
        provider.universe_errors();
        provider.first_heap();
        let reads = reads.finish().unwrap();
        for (key, value) in reads.iter() {
            assert_eq!(engine.observe(&owner, key), Some(*value), "{key:?}");
        }
        // A failed signature is a settled verdict, not a retry: later reads
        // (storage, then Lower) share the snapshot that holds it.
        let storage = engine.signature_inputs();
        assert!(Arc::ptr_eq(&storage, &engine.signature_inputs()));
        assert!(Arc::ptr_eq(&snapshot, &storage));
        // A failed header's exact complete query key changes even when the
        // phase returns byte-identical diagnostic prose.
        let old = snapshot.failed_signature_receipt(failed);
        let file = engine
            .sources
            .add("failed.hir", "fn failed(x: u8) -> u64 { 1 }".into())
            .unwrap();
        engine.parse_file(file, &default_context()).unwrap();
        let changed = engine.signature_snapshot();
        assert_ne!(old, changed.failed_signature_receipt(failed));
        assert_eq!(
            engine.observe(&owner, &ReadKey::FailedSignature(failed)),
            Some(changed.failed_signature_receipt(failed))
        );
    }
    #[test]
    fn frozen_citations_select_occurrences_and_observe_current_body_text() {
        let mut engine =
            QueryEngine::new(default_environment(), crate::phases::engine_only_adapters());
        let context = default_context();
        for (file, source) in [
            ("left.hir", "fn target() {}\nfn left() {}\n"),
            ("right.hir", "fn target() {}\nfn right() {}\n"),
        ] {
            let file = engine.sources.add(file, source.into()).unwrap();
            engine.parse_file(file, &context).unwrap();
        }
        let path = |name: &str| {
            let mut p = crate::entry_path(&context);
            p.0.name = name.into();
            DefId::intern(&p)
        };
        let target = path("target");
        let snapshot = engine.signature_snapshot();
        let source = engine.citation_source();
        let mut receipts = vec![];
        let mut spans = vec![];
        for owner in [path("left"), path("right")] {
            let mut reads = ReadsBuilder::new();
            let mut provider =
                FrozenProvider::for_definition(&snapshot, owner, &source, &mut reads);
            let span = provider.cite_name(target.path()).unwrap();
            assert_eq!(Some(span), source.cited_name(owner, target));
            spans.push(span);
            let reads = reads.finish().unwrap();
            receipts.push(
                reads
                    .iter()
                    .find_map(|(key, value)| (*key == ReadKey::CitedText(target)).then_some(*value))
                    .unwrap(),
            );
        }
        assert_ne!(spans[0], spans[1]);
        assert_ne!(
            engine.sources.resolve(spans[0]).file,
            engine.sources.resolve(spans[1]).file
        );
        assert_eq!(receipts[0], receipts[1]);
        let before = receipts[0];
        drop(source);
        let file = engine
            .sources
            .add("right.hir", "fn target() { 1 }\nfn right() {}\n".into())
            .unwrap();
        engine.parse_file(file, &context).unwrap();
        let source = engine.citation_source();
        let mut reads = ReadsBuilder::new();
        // The retained snapshot is intentionally the earlier signature view.
        let mut provider =
            FrozenProvider::for_definition(&snapshot, path("right"), &source, &mut reads);
        assert!(provider.cite_name(target.path()).is_some());
        let reads = reads.finish().unwrap();
        let now = reads
            .iter()
            .find_map(|(key, value)| (*key == ReadKey::CitedText(target)).then_some(*value))
            .unwrap();
        assert_ne!(before, now);
        assert_eq!(now, source.cited_text(target));
    }
}
