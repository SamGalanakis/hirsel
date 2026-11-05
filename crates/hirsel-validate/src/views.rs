//! The signature views V validates against, with V's verdict on each.
//!
//! `ViewTable` is the public universe of one snapshot: every signature's
//! public view in DefPath order, its digest, V's verdict on it and a reverse
//! index from each view to the verdicts that read it. `PackageViews` is that
//! universe as one package's bodies see it: the package's opaque types through
//! their private views, and, for compile-time evaluation, a materialized
//! owner's signature in place of its declared one. Only the replaced views and
//! the verdicts that read them are checked again.
use crate::{
    SignatureLookup, UniverseErrors, VRead, ValidatedSignatures, ValidationDigest, ViewRefusal,
    ViewVerdict, Violation,
};
use hirsel_syntax::{ContentHash, DefId, DefMap, PackageId};
use hirsel_types::{HeapRole, SignatureRef, TypeInterner};
use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

/// One signature view and V's digest of it.
#[derive(Clone, Debug)]
pub struct View {
    pub d: DefId,
    input: Arc<crate::IdentifiedSignature>,
}
impl View {
    /// A view of `signature`, keeping `previous`'s digest when it is the same view.
    pub fn new(t: &Arc<TypeInterner>, signature: SignatureRef, previous: Option<&View>) -> Self {
        let input = match previous {
            Some(view) if view.input.belongs_to(t) && view.input.is(&signature) => {
                view.input.clone()
            }
            _ => Arc::new(crate::IdentifiedSignature::new(t.clone(), signature)),
        };
        Self {
            d: input.definition(),
            input,
        }
    }
    /// The identity of `signature`, whose payload `source` reads again on
    /// use: the universe holding this view never pins `source`'s row.
    pub fn sourced(
        t: &Arc<TypeInterner>,
        signature: &SignatureRef,
        source: &Arc<dyn crate::SignatureSource>,
        previous: Option<&View>,
    ) -> Self {
        let input = match previous {
            Some(view) if view.input.belongs_to(t) && view.input.is(signature) => {
                view.input.clone()
            }
            _ => Arc::new(crate::IdentifiedSignature::sourced(
                t.clone(),
                signature,
                source.clone(),
            )),
        };
        Self {
            d: input.definition(),
            input,
        }
    }
    pub fn matches(&self, t: &TypeInterner, signature: &SignatureRef) -> bool {
        self.input.belongs_to(t) && self.input.is(signature)
    }
    pub fn identified(&self) -> &Arc<crate::IdentifiedSignature> {
        &self.input
    }
    /// A scoped view of this view's payload, or the refusal to read it again.
    pub fn signature(&self) -> Result<SignatureRef, ViewRefusal> {
        self.input.signature()
    }
    pub fn digest(&self) -> ValidationDigest {
        self.input.digest()
    }
}

/// Every view V's verdict on `d` reads, by the view it names.
fn read_views(verdict: &ViewVerdict) -> impl Iterator<Item = DefId> + '_ {
    verdict.reads.iter().filter_map(|read| match read {
        VRead::View(d, _) | VRead::Missing(d) => Some(*d),
        VRead::FirstHeap(_) | VRead::UniverseErrors(_) => None,
    })
}
fn reads_heap(verdict: &ViewVerdict) -> bool {
    verdict
        .reads
        .iter()
        .any(|read| matches!(read, VRead::FirstHeap(_)))
}
fn first_heap<'a>(views: impl Iterator<Item = &'a View>) -> Option<(DefId, HeapRole)> {
    views
        .into_iter()
        .find_map(|v| v.input.heap().cloned().map(|role| (v.d, role)))
}
/// The earlier, in DefPath order, of two first heap roles.
fn earlier(
    a: Option<(DefId, HeapRole)>,
    b: Option<&(DefId, HeapRole)>,
) -> Option<(DefId, HeapRole)> {
    match (a, b) {
        (Some(a), Some(b)) if b.0 < a.0 => Some(b.clone()),
        (None, b) => b.cloned(),
        (a, _) => a,
    }
}

/// Reads the view of one installed definition when V first asks for it.
pub trait ViewSource: Send + Sync {
    fn view(&self, d: DefId) -> Option<View>;
}
/// The views of read-only installed packages, by definition. Each is
/// identified on its first read, so a universe that holds them reads only
/// the rows V's verdicts and lookups name.
pub struct InstalledViews {
    cells: DefMap<OnceLock<Option<View>>>,
    source: Box<dyn ViewSource>,
}
impl InstalledViews {
    pub fn new(members: impl IntoIterator<Item = DefId>, source: Box<dyn ViewSource>) -> Self {
        Self {
            cells: members.into_iter().map(|d| (d, OnceLock::new())).collect(),
            source,
        }
    }
    pub fn contains(&self, d: DefId) -> bool {
        self.cells.contains_key(&d)
    }
    /// `d`'s view, identified on its first read.
    pub fn get(&self, d: DefId) -> Option<&View> {
        self.cells
            .get(&d)?
            .get_or_init(|| self.source.view(d))
            .as_ref()
    }
}
/// The installed part of a universe: views read on demand, and the
/// published summaries that stand for their verdicts. Only error-free
/// summaries are published, so an installed part adds no universe errors.
#[derive(Clone)]
pub struct Installed {
    views: Arc<InstalledViews>,
    parts: Arc<[PackageUniverse]>,
}
impl Installed {
    /// `None` when a summary carries errors: such a package is not installed.
    pub fn new(
        views: Arc<InstalledViews>,
        parts: impl IntoIterator<Item = PackageUniverse>,
    ) -> Option<Self> {
        let mut parts: Vec<_> = parts.into_iter().collect();
        if parts.iter().any(|part| !part.errors.1.is_empty()) {
            return None;
        }
        parts.sort_by_key(|part| part.input);
        Some(Self {
            views,
            parts: parts.into(),
        })
    }
    fn first_heap(&self) -> Option<&(DefId, HeapRole)> {
        self.parts
            .iter()
            .filter_map(|part| part.first_heap.as_ref())
            .min_by_key(|(d, _)| *d)
    }
    /// The identity of a universe of `views` beside these installed parts.
    /// A universe that is exactly one published universe has its identity.
    fn input(&self, views: Option<ContentHash>) -> ContentHash {
        match (views, &*self.parts) {
            (None, [part]) => part.input,
            _ => {
                let mut bytes = Vec::with_capacity(33 + self.parts.len() * 32);
                bytes.push(u8::from(views.is_some()));
                bytes.extend(views.unwrap_or_default().0);
                for part in self.parts.iter() {
                    bytes.extend(part.input.0);
                }
                ContentHash::of_parts(
                    "hirsel.validate.universe-input.installed.v1",
                    [bytes.as_slice()],
                )
            }
        }
    }
}

fn heap_bytes(heap: &HeapRole) -> Option<u64> {
    [&heap.arena, &heap.operation]
        .into_iter()
        .try_fold(0u64, |n, path| {
            let n = n.checked_add(
                (path.0.package.name.capacity()
                    + path.0.package.version.capacity()
                    + path.0.name.capacity()
                    + path.0.modules.capacity() * size_of::<String>()) as u64,
            )?;
            path.0
                .modules
                .iter()
                .try_fold(n, |n, module| n.checked_add(module.capacity() as u64))
        })
}

/// The public universe of one snapshot, with V's verdict on every view.
pub struct ViewTable {
    views: Vec<View>,
    index: DefMap<u32>,
    first_heap: Option<(DefId, HeapRole)>,
    verdicts: Vec<Arc<ViewVerdict>>,
    /// The views whose verdict read each view (or its absence).
    readers: DefMap<Vec<u32>>,
    /// The views whose verdict read the first heap role.
    heap_readers: Vec<u32>,
    errors: UniverseErrors,
    input_digest: OnceLock<ContentHash>,
    stamp: EnvironmentStamp,
    installed: Option<Installed>,
    _retention: Option<Arc<dyn Send + Sync>>,
}
impl ViewTable {
    /// The table of `signatures` (one per path), reusing the digests of
    /// `previous` for unchanged views and the verdicts of `store`'s previous
    /// revision whose reads still hold. A view V cannot read refuses the table.
    pub fn new(
        t: &Arc<TypeInterner>,
        signatures: impl IntoIterator<Item = SignatureRef>,
        previous: Option<&ViewTable>,
        store: &mut ValidatedSignatures,
        old: Option<&ValidatedSignatures>,
    ) -> Result<Self, ViewRefusal> {
        Self::from_views(
            t,
            signatures.into_iter().map(|signature| {
                let d = signature.definition;
                View::new(t, signature, previous.and_then(|p| p.get(d)))
            }),
            store,
            old,
        )
    }
    /// Build a universe from already identified immutable products. V still
    /// checks each declaration's verdict against this universe's exact reads.
    pub fn from_views(
        t: &Arc<TypeInterner>,
        views: impl IntoIterator<Item = View>,
        store: &mut ValidatedSignatures,
        old: Option<&ValidatedSignatures>,
    ) -> Result<Self, ViewRefusal> {
        Self::with_installed(t, views, None, store, old)
    }
    /// A universe of `views`, which V checks, beside `installed` packages,
    /// which V reads on demand and whose published summaries stand for their
    /// verdicts. An installed package never reads its importers' views.
    pub fn with_installed(
        t: &Arc<TypeInterner>,
        views: impl IntoIterator<Item = View>,
        installed: Option<Installed>,
        store: &mut ValidatedSignatures,
        old: Option<&ValidatedSignatures>,
    ) -> Result<Self, ViewRefusal> {
        let _batch = store.publication_batch();
        let mut views: Vec<View> = views.into_iter().collect();
        views.sort_by_key(|view| view.d);
        let index = views
            .iter()
            .enumerate()
            .map(|(i, v)| (v.d, i as u32))
            .collect();
        assert!(
            views
                .iter()
                .all(|v| installed.as_ref().is_none_or(|i| !i.views.contains(v.d))),
            "a checked view is not also an installed one"
        );
        let mut table = Self {
            first_heap: earlier(
                first_heap(views.iter()),
                installed.as_ref().and_then(Installed::first_heap),
            ),
            installed,
            views,
            index,
            verdicts: vec![],
            readers: DefMap::default(),
            heap_readers: vec![],
            errors: UniverseErrors::default(),
            input_digest: OnceLock::new(),
            stamp: EnvironmentStamp::new(t, store),
            _retention: None,
        };
        let verdicts: Vec<_> = table
            .views
            .iter()
            .map(|v| store.verdict_from(t, v.d, &table, old))
            .collect::<Result<_, _>>()?;
        for (i, verdict) in verdicts.iter().enumerate() {
            for d in read_views(verdict).collect::<BTreeSet<_>>() {
                table.readers.entry(d).or_default().push(i as u32);
            }
            if reads_heap(verdict) {
                table.heap_readers.push(i as u32);
            }
        }
        table.errors =
            UniverseErrors::new(verdicts.iter().filter_map(|v| v.error.clone()).collect());
        table.verdicts = verdicts;
        Ok(table)
    }
    /// Control allocations of a complete error-free table. Signature payloads
    /// held by its identified views are charged separately by the host; sourced
    /// payloads retain their original cache owners. Installed tables and failed
    /// verdict payloads are not retained through this optional memo.
    pub fn retained_control_bytes(&self) -> Option<u64> {
        if self.installed.is_some() || !self.errors.errors.is_empty() {
            return None;
        }
        let mut bytes = (size_of::<Self>() + 2 * size_of::<usize>() + 256) as u64;
        for allocated in [
            self.views.capacity().checked_mul(size_of::<View>())?,
            self.verdicts
                .capacity()
                .checked_mul(size_of::<Arc<ViewVerdict>>())?,
            self.index
                .capacity()
                .checked_mul(3 * size_of::<(DefId, u32)>() + 1)?,
            self.readers
                .capacity()
                .checked_mul(3 * size_of::<(DefId, Vec<u32>)>() + 1)?,
            self.heap_readers.capacity().checked_mul(size_of::<u32>())?,
        ] {
            bytes = bytes.checked_add(allocated as u64)?;
        }
        for readers in self.readers.values() {
            bytes = bytes.checked_add(readers.capacity().checked_mul(size_of::<u32>())? as u64)?;
        }
        for verdict in &self.verdicts {
            bytes =
                bytes.checked_add((size_of::<ViewVerdict>() + 2 * size_of::<usize>()) as u64)?;
            bytes = bytes
                .checked_add(verdict.reads.capacity().checked_mul(size_of::<VRead>())? as u64)?;
        }
        for view in &self.views {
            bytes = bytes.checked_add((size_of::<crate::IdentifiedSignature>() + 128) as u64)?;
            if let Some(heap) = view.input.heap() {
                bytes = bytes.checked_add(heap_bytes(heap)?)?;
            }
        }
        if let Some((_, heap)) = &self.first_heap {
            bytes = bytes.checked_add(heap_bytes(heap)?)?;
        }
        Some(bytes)
    }
    /// Keep the host's reservation with every owner of the shared table.
    pub fn retain_owner(mut self, owner: Arc<dyn Send + Sync>) -> Self {
        self._retention = Some(owner);
        self
    }
    pub fn get(&self, d: DefId) -> Option<&View> {
        match self.index.get(&d) {
            Some(&i) => Some(&self.views[i as usize]),
            None => self.installed.as_ref()?.views.get(d),
        }
    }
    /// Identity of all inputs a private evaluation overlay can observe. This
    /// includes valid rows: an overlay can invalidate a dependent declaration
    /// even when both base universes have no errors. Compute once per universe.
    pub fn input_digest(&self) -> ContentHash {
        *self.input_digest.get_or_init(|| {
            universe_input(
                self.views.iter(),
                self.first_heap.as_ref(),
                self.errors.digest,
                self.installed.as_ref(),
            )
        })
    }
    /// Whether this definition belongs to the immutable installed part.
    /// New private execution owners have no installed verdict to replace.
    pub fn contains_installed(&self, d: DefId) -> bool {
        self.installed
            .as_ref()
            .is_some_and(|installed| installed.views.contains(d))
    }
    /// The views V checks in this universe; installed views are not listed.
    pub fn views(&self) -> &[View] {
        &self.views
    }
    /// What importers read of `package`'s own public views in this universe:
    /// the identity of those views, their verdicts' errors and their first
    /// heap role. An importer's universe holds them beside its own views.
    pub fn exported(&self, package: &PackageId) -> PackageUniverse {
        let own: Vec<usize> = (0..self.views.len())
            .filter(|&i| &self.views[i].d.path().0.package == package)
            .collect();
        let heap = first_heap(own.iter().map(|&i| &self.views[i]));
        let errors = UniverseErrors::new(
            own.iter()
                .filter_map(|&i| self.verdicts[i].error.clone())
                .collect(),
        );
        let input = input_digest(
            own.iter().map(|&i| &self.views[i]),
            heap.as_ref(),
            errors.digest,
        );
        PackageUniverse::of(input, &errors, heap.as_ref())
    }
    /// This universe's summary for importers that do not read its views.
    pub fn universe(&self) -> PackageUniverse {
        PackageUniverse::of(self.input_digest(), &self.errors, self.first_heap.as_ref())
    }
    pub fn verdict(&self, d: DefId) -> Option<&Arc<ViewVerdict>> {
        self.index.get(&d).map(|&i| &self.verdicts[i as usize])
    }
}
impl SignatureLookup for ViewTable {
    fn view(&self, d: DefId) -> Option<crate::ViewInput<'_>> {
        self.get(d).map(|v| v.input.input())
    }
    fn first_heap(&self) -> Option<(DefId, HeapRole)> {
        self.first_heap.clone()
    }
    fn universe_errors(&self) -> (ContentHash, &[Violation]) {
        (self.errors.digest, &self.errors.errors)
    }
}

/// The facts an importer's V reads from one package's universe without
/// reading its views: the universe's complete input identity, its signature
/// errors and its first heap role. A read-only installed package publishes
/// this summary, so a dependent need not identify every signature again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageUniverse {
    pub input: ContentHash,
    pub errors: (ContentHash, Arc<[Violation]>),
    pub first_heap: Option<(DefId, HeapRole)>,
}
/// The archived form: definitions by path, since `DefId`s are process-local.
#[derive(serde::Serialize, serde::Deserialize)]
struct ArchivedUniverse {
    input: ContentHash,
    errors: ContentHash,
    first_heap: Option<(hirsel_syntax::DefPath, HeapRole)>,
}
impl PackageUniverse {
    fn of(
        input: ContentHash,
        errors: &UniverseErrors,
        first_heap: Option<&(DefId, HeapRole)>,
    ) -> Self {
        Self {
            input,
            errors: (errors.digest, errors.errors.iter().cloned().collect()),
            first_heap: first_heap.cloned(),
        }
    }
    /// The archived bytes of this summary. Violations have no durable codec,
    /// so only an error-free universe is published; a universe with errors
    /// stays with the full table its importers rebuild.
    pub fn encode(&self) -> Option<Vec<u8>> {
        if !self.errors.1.is_empty() {
            return None;
        }
        bincode::serialize(&ArchivedUniverse {
            input: self.input,
            errors: self.errors.0,
            first_heap: self
                .first_heap
                .as_ref()
                .map(|(d, role)| (d.path().clone(), role.clone())),
        })
        .ok()
    }
    /// A summary from archived bytes. An archive whose error digest is not
    /// the empty universe's claims violations it does not carry: refused.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let archived: ArchivedUniverse = bincode::deserialize(bytes).ok()?;
        if archived.errors != crate::errors_digest(&[]) {
            return None;
        }
        Some(Self {
            input: archived.input,
            errors: (archived.errors, Arc::from([])),
            first_heap: archived
                .first_heap
                .map(|(path, role)| (DefId::intern(&path), role)),
        })
    }
}

/// One package's universe: the public table with some views replaced.
pub struct PackageViews {
    base: Arc<ViewTable>,
    replaced: DefMap<View>,
    /// Verdicts checked again under the replacements, by view.
    verdicts: DefMap<Arc<ViewVerdict>>,
    first_heap: Option<(DefId, HeapRole)>,
    errors: UniverseErrors,
    input_digest: OnceLock<ContentHash>,
    stamp: EnvironmentStamp,
    origin_input: ContentHash,
    _retention: Option<Arc<dyn Send + Sync>>,
}
impl PackageViews {
    /// The universe `base` with `replacements` in place of (or beside) its views.
    pub fn new(
        t: &Arc<TypeInterner>,
        base: Arc<ViewTable>,
        replacements: impl IntoIterator<Item = View>,
        store: &mut ValidatedSignatures,
        old: Option<&ValidatedSignatures>,
    ) -> Result<Self, ViewRefusal> {
        let replaced: DefMap<View> = replacements.into_iter().map(|v| (v.d, v)).collect();
        Self::over(t, base, replaced, &DefMap::default(), store, old)
    }
    /// This universe with `signature` in place of its view: compile-time
    /// evaluation's materialized owner.
    pub fn replace(
        &self,
        t: &Arc<TypeInterner>,
        signature: SignatureRef,
        store: &mut ValidatedSignatures,
        old: Option<&ValidatedSignatures>,
    ) -> Result<Self, ViewRefusal> {
        let view = View::new(t, signature, None);
        let mut replaced = self.replaced.clone();
        replaced.insert(view.d, view);
        let mut result = Self::over(t, self.base.clone(), replaced, &self.verdicts, store, old)?;
        result.origin_input = self.input_digest();
        Ok(result)
    }
    /// The independently identified complete universe before this replacement.
    pub fn origin_input(&self) -> ContentHash {
        self.origin_input
    }
    /// Incremental control storage of a successful overlay, excluding its
    /// shared base and signature payloads. Failed verdict payloads stay transient.
    pub fn retained_delta_bytes(&self) -> Option<u64> {
        if !self.base.errors.errors.is_empty()
            || !self.errors.errors.is_empty()
            || self.verdicts.values().any(|v| v.error.is_some())
        {
            return None;
        }
        let mut bytes = (size_of::<Self>() + 2 * size_of::<usize>()) as u64;
        bytes = bytes.checked_add((self.replaced.len() as u64).checked_mul(
            (3 * size_of::<(DefId, View)>() + size_of::<crate::IdentifiedSignature>() + 256) as u64,
        )?)?;
        bytes = bytes.checked_add((self.verdicts.len() as u64).checked_mul(
            (3 * size_of::<(DefId, Arc<ViewVerdict>)>() + size_of::<ViewVerdict>() + 256) as u64,
        )?)?;
        for verdict in self.verdicts.values() {
            bytes = bytes.checked_add((verdict.reads.capacity() * size_of::<VRead>()) as u64)?;
        }
        if let Some((_, heap)) = &self.first_heap {
            bytes = bytes.checked_add(heap_bytes(heap)?)?;
        }
        for view in self.replaced.values() {
            if let Some(heap) = view.input.heap() {
                bytes = bytes.checked_add(heap_bytes(heap)?)?;
            }
        }
        Some(bytes)
    }
    /// Retain the host's existing reservation with every shared overlay owner.
    pub fn retain_owner(mut self, owner: Arc<dyn Send + Sync>) -> Self {
        self._retention = Some(owner);
        self
    }
    /// This universe's summary, including its private views' effects.
    pub fn universe(&self) -> PackageUniverse {
        PackageUniverse::of(self.input_digest(), &self.errors, self.first_heap.as_ref())
    }
    /// The view this universe has in place of `d`'s public one.
    pub fn replacement(&self, d: DefId) -> Option<&View> {
        self.replaced.get(&d)
    }
    /// This package's complete V input identity, including its private views.
    /// Replacement artifacts use their base package's digest as provenance;
    /// their own bytes remain identified by their producing execution reads.
    pub fn input_digest(&self) -> ContentHash {
        *self.input_digest.get_or_init(|| {
            let views = self
                .base
                .views
                .iter()
                .map(|view| self.replaced.get(&view.d).unwrap_or(view))
                .chain(
                    self.replaced
                        .values()
                        .filter(|view| self.base.get(view.d).is_none()),
                );
            universe_input(
                views,
                self.first_heap.as_ref(),
                self.errors.digest,
                self.base.installed.as_ref(),
            )
        })
    }
    fn over(
        t: &Arc<TypeInterner>,
        base: Arc<ViewTable>,
        replaced: DefMap<View>,
        checked: &DefMap<Arc<ViewVerdict>>,
        store: &mut ValidatedSignatures,
        old: Option<&ValidatedSignatures>,
    ) -> Result<Self, ViewRefusal> {
        hirsel_intern::work::count(hirsel_intern::work::Counter::ValidationOverlayConstructions);
        hirsel_intern::timings::count(
            hirsel_intern::timings::Count::ValidationOverlayConstructions,
        );
        // The universe in DefPath order: the base's views and new ones.
        // An installed view's readers are not known, so none is replaced.
        assert!(
            replaced.keys().all(|d| base
                .installed
                .as_ref()
                .is_none_or(|i| !i.views.contains(*d))),
            "an installed view is replaced only in its full table"
        );
        let _batch = store.publication_batch();
        let mut order: Vec<&View> = base
            .views
            .iter()
            .map(|v| replaced.get(&v.d).unwrap_or(v))
            .collect();
        order.extend(replaced.values().filter(|v| base.get(v.d).is_none()));
        order.sort_by(|a, b| a.d.path().cmp(b.d.path()));
        let heap = earlier(
            first_heap(order.iter().copied()),
            base.installed.as_ref().and_then(Installed::first_heap),
        );
        // Verdicts to check again: the replaced views and every view whose
        // verdict, public or already checked here, read one of them.
        let mut again: BTreeSet<DefId> = replaced.keys().copied().collect();
        for d in replaced.keys() {
            for &i in base.readers.get(d).into_iter().flatten() {
                again.insert(base.views[i as usize].d);
            }
        }
        for (d, verdict) in checked {
            if read_views(verdict).any(|read| replaced.contains_key(&read)) {
                again.insert(*d);
            }
        }
        if heap.as_ref().map(|h| h.0) != base.first_heap.as_ref().map(|h| h.0) {
            again.extend(base.heap_readers.iter().map(|&i| base.views[i as usize].d));
        }
        let order: Vec<DefId> = order.iter().map(|v| v.d).collect();
        let mut views = Self {
            origin_input: base.input_digest(),
            _retention: None,
            base,
            replaced,
            verdicts: DefMap::default(),
            first_heap: heap,
            errors: UniverseErrors::default(),
            input_digest: OnceLock::new(),
            stamp: EnvironmentStamp::new(t, store),
        };
        let verdicts: DefMap<_> = again
            .iter()
            .map(|&d| Ok((d, store.verdict_from(t, d, &views, old)?)))
            .collect::<Result<_, ViewRefusal>>()?;
        let errors = order
            .iter()
            .filter_map(|d| match verdicts.get(d) {
                Some(verdict) => verdict.error.clone(),
                None => views.base.verdict(*d).and_then(|v| v.error.clone()),
            })
            .collect();
        views.errors = UniverseErrors::new(errors);
        views.verdicts = verdicts;
        Ok(views)
    }
}
impl SignatureLookup for PackageViews {
    fn view(&self, d: DefId) -> Option<crate::ViewInput<'_>> {
        match self.replaced.get(&d) {
            Some(v) => Some(v.input.input()),
            None => self.base.view(d),
        }
    }
    fn first_heap(&self) -> Option<(DefId, HeapRole)> {
        self.first_heap.clone()
    }
    fn universe_errors(&self) -> (ContentHash, &[Violation]) {
        (self.errors.digest, &self.errors.errors)
    }
}

/// A universe's identity: its checked views' and, beside them, its installed
/// parts' published identities.
fn universe_input<'a>(
    views: impl Iterator<Item = &'a View>,
    heap: Option<&(DefId, HeapRole)>,
    errors: ContentHash,
    installed: Option<&Installed>,
) -> ContentHash {
    let mut views = views.peekable();
    match installed {
        None => input_digest(views, heap, errors),
        Some(installed) => {
            let checked = views
                .peek()
                .is_some()
                .then(|| input_digest(views, heap, errors));
            installed.input(checked)
        }
    }
}

/// Salsa validates a query against its recorded inputs (runtime.rs:316-334).
/// An overlay additionally reads the whole signature universe through its
/// error aggregate; hash that immutable input once rather than inventing an
/// error-only receipt or recording O(N) inputs for every evaluation.
fn input_digest<'a>(
    views: impl Iterator<Item = &'a View>,
    heap: Option<&(DefId, HeapRole)>,
    errors: ContentHash,
) -> ContentHash {
    // GEN-6 specializations are left out: one materializing in a program must
    // not invalidate every validation of its declaring package. A caller of
    // one reads its declaration and implementations, which determine it.
    let mut views: Vec<_> = views
        .filter(|view| hirsel_types::specialization_parts(view.d.path()).is_none())
        .collect();
    views.sort_by(|a, b| a.d.path().cmp(b.d.path()));
    let mut bytes = Vec::with_capacity(views.len() * 96);
    bytes.extend((views.len() as u64).to_le_bytes());
    for view in views {
        encode_def_path(&mut bytes, view.d.path());
        bytes.extend(view.digest().0.0);
    }
    bytes.push(u8::from(heap.is_some()));
    if let Some((d, _)) = heap {
        encode_def_path(&mut bytes, d.path());
    }
    bytes.extend(errors.0);
    ContentHash::of_parts("hirsel.validate.universe-input.v1", [bytes.as_slice()])
}

fn encode_def_path(out: &mut Vec<u8>, path: &hirsel_syntax::DefPath) {
    fn text(out: &mut Vec<u8>, value: &str) {
        out.extend((value.len() as u32).to_le_bytes());
        out.extend(value.as_bytes());
    }
    let item = &path.0;
    text(out, &item.package.name);
    text(out, &item.package.version);
    out.extend((item.modules.len() as u32).to_le_bytes());
    for module in &item.modules {
        text(out, module);
    }
    text(out, &item.name);
}

/// An immutable universe owned by V. Only V's view tables implement this trait;
/// an ordinary lookup cannot claim another snapshot's clean stamp.
pub trait CheckedEnvironment: SignatureLookup + sealed::Environment {
    /// V's complete independently identified view/type/error input.
    fn input_digest(&self) -> ContentHash;
    #[doc(hidden)]
    fn stamp(&self) -> &EnvironmentStamp;
}
mod sealed {
    pub trait Environment {}
}
/// Opaque identity of an exact immutable view/type/error snapshot.
/// This identity is local and supplies no durable acceptance authority.
pub struct EnvironmentStamp {
    pub(crate) types: Arc<TypeInterner>,
    pub(crate) identity: Arc<()>,
    pub(crate) verifier: std::sync::Weak<crate::VerifierCache>,
    pub(crate) facts: Arc<EnvironmentFacts>,
}
#[derive(Default)]
pub(crate) struct EnvironmentFacts {
    pub(crate) execution: crate::FactTable<crate::receipts::ExecutionKey, crate::ExecutionReceipt>,
    pub(crate) reads: crate::FactTable<VRead, bool>,
}
impl EnvironmentStamp {
    fn new(types: &Arc<TypeInterner>, store: &ValidatedSignatures) -> Self {
        let graph = store
            .verifier()
            .map_or_else(Arc::default, |v| v.fact_graph());
        Self {
            types: types.clone(),
            identity: Arc::new(()),
            verifier: store
                .verifier()
                .map_or_else(std::sync::Weak::new, Arc::downgrade),
            facts: Arc::new(EnvironmentFacts {
                execution: crate::FactTable::new(graph.clone()),
                reads: crate::FactTable::new(graph),
            }),
        }
    }
}
impl sealed::Environment for ViewTable {}
impl CheckedEnvironment for ViewTable {
    fn input_digest(&self) -> ContentHash {
        self.input_digest()
    }
    fn stamp(&self) -> &EnvironmentStamp {
        &self.stamp
    }
}
impl sealed::Environment for PackageViews {}
impl CheckedEnvironment for PackageViews {
    fn input_digest(&self) -> ContentHash {
        self.input_digest()
    }
    fn stamp(&self) -> &EnvironmentStamp {
        &self.stamp
    }
}

/// Signature access for the existing validation/receipt algorithm. A checked
/// input retains V's immutable snapshot; a general lookup checks every read.
#[derive(Clone, Copy)]
pub enum SignatureInputs<'a> {
    Raw(&'a dyn SignatureLookup),
    Checked(&'a dyn CheckedEnvironment),
}
impl<'a> SignatureInputs<'a> {
    pub(crate) fn lookup(self) -> &'a dyn SignatureLookup {
        match self {
            Self::Raw(input) => input,
            Self::Checked(input) => input,
        }
    }
    pub(crate) fn stamp(self) -> Option<&'a EnvironmentStamp> {
        match self {
            Self::Raw(_) => None,
            Self::Checked(input) => Some(input.stamp()),
        }
    }
}
impl<'a, T: SignatureLookup> From<&'a T> for SignatureInputs<'a> {
    fn from(input: &'a T) -> Self {
        Self::Raw(input)
    }
}
impl<'a> From<&'a dyn SignatureLookup> for SignatureInputs<'a> {
    fn from(input: &'a dyn SignatureLookup) -> Self {
        Self::Raw(input)
    }
}
impl<'a> From<&'a dyn CheckedEnvironment> for SignatureInputs<'a> {
    fn from(input: &'a dyn CheckedEnvironment) -> Self {
        Self::Checked(input)
    }
}
