//! The signature views V validates against, with V's verdict on each.
//!
//! `ViewTable` is the public universe of one snapshot: every signature's
//! public view in DefPath order, its digest, V's verdict on it and a reverse
//! index from each view to the verdicts that read it. `PackageViews` is that
//! universe as one package's bodies see it: the package's opaque types through
//! their private views, and, for compile-time evaluation, a materialized
//! owner's signature in place of its declared one. Only the replaced views and
//! the verdicts that read them are checked again.
mod memory;
use crate::region::{Map, Region, Vector};
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
        VRead::HeapRole(_) | VRead::UniverseErrors(_) => None,
    })
}
fn reads_heap(verdict: &ViewVerdict) -> bool {
    verdict
        .reads
        .iter()
        .any(|read| matches!(read, VRead::HeapRole(_)))
}
fn heap_role<'a>(views: impl Iterator<Item = &'a View>) -> Result<Option<HeapRole>, ViewRefusal> {
    crate::input::select_heap(views.filter_map(|v| v.input.heap()))
}
fn combined(a: Option<HeapRole>, b: Option<HeapRole>) -> Result<Option<HeapRole>, ViewRefusal> {
    crate::input::select_heap(
        a.iter()
            .chain(b.iter())
            .map(|role| (&role.arena, &role.operation)),
    )
}

/// Reads the view of one installed definition when V first asks for it.
pub trait ViewSource: Send + Sync {
    fn view(&self, d: DefId) -> Result<Option<View>, ViewRefusal>;
    fn retain_control(&self, memory: &Arc<dyn crate::ValidationMemory>) -> Result<(), ViewRefusal>;
}
/// The views of read-only installed packages, by definition. Each is
/// identified on its first read, so a universe that holds them reads only
/// the rows V's verdicts and lookups name.
#[derive(Default)]
struct InstalledCell {
    value: OnceLock<View>,
    build: std::sync::Mutex<()>,
    retention: crate::memory::ControlRetention,
}
impl InstalledCell {
    fn retain_control(&self, memory: &dyn crate::ValidationMemory) -> Result<(), ViewRefusal> {
        self.retention
            .retain(memory, || Some(size_of::<Self>() as u64))?;
        if let Some(view) = self.value.get() {
            view.input.retain_control(memory)?;
        }
        Ok(())
    }
}
pub struct InstalledViews {
    cells: Vector<(DefId, OnceLock<Box<InstalledCell>>)>,
    source: Box<dyn ViewSource>,
    memory: std::sync::RwLock<Option<Arc<dyn crate::ValidationMemory>>>,
    retention: crate::memory::ControlRetention,
}
impl InstalledViews {
    pub fn new(
        members: impl IntoIterator<Item = DefId>,
        source: Box<dyn ViewSource>,
    ) -> Result<Self, ViewRefusal> {
        let storage = Region::default();
        let mut cells = storage.collect(members.into_iter().map(|d| (d, OnceLock::new())))?;
        cells.sort_unstable_by_key(|(d, _)| *d);
        cells.dedup_by_key(|(d, _)| *d);
        Ok(Self {
            cells,
            source,
            memory: std::sync::RwLock::new(None),
            retention: Default::default(),
        })
    }
    fn retain_control(&self, memory: &Arc<dyn crate::ValidationMemory>) -> Result<(), ViewRefusal> {
        let mut admitted = self.memory.write().unwrap();
        self.cells.allocator().retain(memory)?;
        self.retention
            .retain(&**memory, || Some(crate::memory::arc::<Self>()))?;
        self.source.retain_control(memory)?;
        *admitted = Some(memory.clone());
        for cell in self.cells.iter().filter_map(|(_, slot)| slot.get()) {
            cell.retain_control(&**memory)?;
        }
        Ok(())
    }
    pub fn contains(&self, d: DefId) -> bool {
        self.cells.binary_search_by_key(&d, |(id, _)| *id).is_ok()
    }
    /// `d`'s view, identified on its first read.
    pub fn get(&self, d: DefId) -> Result<Option<&View>, ViewRefusal> {
        let Ok(index) = self.cells.binary_search_by_key(&d, |(id, _)| *id) else {
            return Ok(None);
        };
        let slot = &self.cells[index].1;
        // Admission and publication share this read lock with retain_control's
        // writer, including cells first demanded before a universe is admitted.
        let memory = self.memory.read().unwrap();
        if slot.get().is_none() {
            let cell = InstalledCell::default();
            if let Some(memory) = &*memory {
                cell.retain_control(&**memory)?;
            }
            let _ = slot.set(Box::new(cell));
        }
        let cell = slot.get().unwrap();
        if cell.value.get().is_none() {
            let _build = cell.build.lock().unwrap();
            if cell.value.get().is_none() {
                let Some(view) = self.source.view(d)? else {
                    return Ok(None);
                };
                if let Some(memory) = &*memory {
                    view.input.retain_control(&**memory)?;
                }
                let _ = cell.value.set(view);
            }
        }
        Ok(cell.value.get())
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
    fn heap_role(&self) -> Result<Option<HeapRole>, ViewRefusal> {
        let roles: Vec<_> = self
            .parts
            .iter()
            .filter_map(|part| part.heap.role())
            .collect();
        crate::input::select_heap(roles.iter().map(|role| (&role.arena, &role.operation)))
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
                    "hirsel.validate.universe-input.installed.v2",
                    [bytes.as_slice()],
                )
            }
        }
    }
}

fn heap_bytes(heap: &HeapRole) -> Option<u64> {
    crate::input::path_bytes(&heap.arena)?.checked_add(crate::input::path_bytes(&heap.operation)?)
}

/// The public universe of one snapshot, with V's verdict on every view.
pub struct ViewTable {
    storage: Region,
    views: Vector<View>,
    index: Map<DefId, u32>,
    heap_role: Option<HeapRole>,
    heap_evidence: crate::HeapObservation,
    verdicts: Vector<Arc<ViewVerdict>>,
    /// The views whose verdict read each view (or its absence).
    readers: Map<DefId, Vector<u32>>,
    /// The views whose verdict read the selected heap role.
    heap_readers: Vector<u32>,
    errors: UniverseErrors,
    input_digest: OnceLock<ContentHash>,
    stamp: EnvironmentStamp,
    installed: Option<Installed>,
    _retention: crate::memory::ControlRetention,
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
                View::new(t, signature, previous.and_then(|p| p.get(d).ok().flatten()))
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
        let storage = Region::default();
        if let Some(memory) = store.memory() {
            storage.retain(&memory)?;
        }
        let mut views = storage.collect(views)?;
        views.sort_by_key(|view| view.d);
        let mut index = storage.map();
        index
            .try_reserve(views.len())
            .map_err(|_| storage.refusal())?;
        for (i, view) in views.iter().enumerate() {
            index.insert(view.d, i as u32);
        }
        assert!(
            views
                .iter()
                .all(|v| installed.as_ref().is_none_or(|i| !i.views.contains(v.d))),
            "a checked view is not also an installed one"
        );
        let mut table = Self {
            heap_role: combined(
                heap_role(views.iter())?,
                installed
                    .as_ref()
                    .map(Installed::heap_role)
                    .transpose()?
                    .flatten(),
            )?,
            heap_evidence: crate::HeapObservation::None,
            installed,
            views,
            index,
            verdicts: storage.vector(),
            readers: storage.map(),
            heap_readers: storage.vector(),
            storage,
            errors: UniverseErrors::default(),
            input_digest: OnceLock::new(),
            stamp: EnvironmentStamp::new(t, store),
            _retention: Default::default(),
        };
        table.heap_evidence = crate::input::prepare_heap(&table, store, old)?;
        let mut verdicts = table.storage.vector();
        verdicts
            .try_reserve(table.views.len())
            .map_err(|_| table.storage.refusal())?;
        for view in &table.views {
            verdicts.push(store.verdict_from(t, view.d, &table, old)?);
        }
        for (i, verdict) in verdicts.iter().enumerate() {
            for d in read_views(verdict).collect::<BTreeSet<_>>() {
                table
                    .readers
                    .try_reserve(1)
                    .map_err(|_| table.storage.refusal())?;
                let readers = table
                    .readers
                    .entry(d)
                    .or_insert_with(|| table.storage.vector());
                table.storage.push(readers, i as u32)?;
            }
            if reads_heap(verdict) {
                table.storage.push(&mut table.heap_readers, i as u32)?;
            }
        }
        table.errors =
            UniverseErrors::new(verdicts.iter().filter_map(|v| v.error.clone()).collect());
        table.verdicts = verdicts;
        Ok(table)
    }
    /// Physical chunk allocations and separately owned immutable inputs.
    pub fn allocated_control_bytes(&self) -> Option<u64> {
        let mut bytes = self
            .table_control_bytes()?
            .checked_add(self.storage.bytes()?)?;
        for verdict in &self.verdicts {
            bytes = bytes.checked_add(verdict.retained_verdict_bytes()?)?;
        }
        for view in &self.views {
            bytes = bytes.checked_add(view.input.retained_identity_bytes()?)?;
        }
        Some(bytes)
    }
    pub fn get(&self, d: DefId) -> Result<Option<&View>, ViewRefusal> {
        match self.index.get(&d) {
            Some(&i) => Ok(Some(&self.views[i as usize])),
            None => match &self.installed {
                Some(installed) => installed.views.get(d),
                None => Ok(None),
            },
        }
    }
    fn contains(&self, d: DefId) -> bool {
        self.index.contains_key(&d) || self.installed.as_ref().is_some_and(|i| i.views.contains(d))
    }
    /// Identity of all inputs a private evaluation overlay can observe. This
    /// includes valid rows: an overlay can invalidate a dependent declaration
    /// even when both base universes have no errors. Compute once per universe.
    pub fn input_digest(&self) -> ContentHash {
        *self.input_digest.get_or_init(|| {
            universe_input(
                self.views.iter(),
                &self.heap_evidence,
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
        let heap = self.heap_evidence.clone();
        let errors = UniverseErrors::new(
            own.iter()
                .filter_map(|&i| self.verdicts[i].error.clone())
                .collect(),
        );
        let input = input_digest(own.iter().map(|&i| &self.views[i]), &heap, errors.digest);
        PackageUniverse::of(input, &errors, heap)
    }
    /// This universe's summary for importers that do not read its views.
    pub fn universe(&self) -> PackageUniverse {
        PackageUniverse::of(
            self.input_digest(),
            &self.errors,
            self.heap_evidence.clone(),
        )
    }
    pub fn verdict(&self, d: DefId) -> Option<&Arc<ViewVerdict>> {
        self.index.get(&d).map(|&i| &self.verdicts[i as usize])
    }
}
impl SignatureLookup for ViewTable {
    fn view(&self, d: DefId) -> Result<Option<crate::ViewInput<'_>>, crate::ViewRefusal> {
        Ok(self.get(d)?.map(|v| v.input.input()))
    }
    fn heap_role(&self) -> Result<Option<HeapRole>, ViewRefusal> {
        Ok(self.heap_role.clone())
    }
    fn heap_observation(&self) -> Result<crate::HeapObservation, ViewRefusal> {
        Ok(self.heap_evidence.clone())
    }
    fn universe_errors(&self) -> (ContentHash, &[Violation]) {
        (self.errors.digest, &self.errors.errors)
    }
}

/// The facts an importer's V reads from one package's universe without
/// reading its views: the universe's complete input identity, its signature
/// errors and its selected heap role. A read-only installed package publishes
/// this summary, so a dependent need not identify every signature again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageUniverse {
    pub input: ContentHash,
    pub errors: (ContentHash, Arc<[Violation]>),
    pub heap: crate::HeapObservation,
}
/// The archived form: definitions by path, since `DefId`s are process-local.
#[derive(serde::Serialize, serde::Deserialize)]
struct ArchivedUniverse {
    input: ContentHash,
    errors: ContentHash,
    heap: crate::HeapObservation,
}
impl PackageUniverse {
    fn of(input: ContentHash, errors: &UniverseErrors, heap: crate::HeapObservation) -> Self {
        Self {
            input,
            errors: (errors.digest, errors.errors.iter().cloned().collect()),
            heap,
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
            heap: self.heap.clone(),
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
            heap: archived.heap,
        })
    }
}

/// Immutable overlay rows retain their admitted parent allocations.
struct PreparedRows<V> {
    storage: Region,
    rows: Map<DefId, V>,
    removed: Map<DefId, ()>,
    parent: Option<Arc<Self>>,
    retention: OnceLock<Arc<dyn Send + Sync>>,
}
impl<V> PreparedRows<V> {
    fn new(
        rows: DefMap<V>,
        parent: Option<Arc<Self>>,
        removed: DefMap<()>,
    ) -> Result<Arc<Self>, ViewRefusal> {
        let storage = Region::default();
        let mut stored_rows = storage.map();
        let mut stored_removed = storage.map();
        for (key, value) in rows {
            storage.insert(&mut stored_rows, key, value)?;
        }
        for (key, value) in removed {
            storage.insert(&mut stored_removed, key, value)?;
        }
        Ok(Arc::new(Self {
            storage,
            rows: stored_rows,
            removed: stored_removed,
            parent,
            retention: OnceLock::new(),
        }))
    }
    fn get(&self, definition: &DefId) -> Option<&V> {
        let mut layer = self;
        loop {
            if let Some(value) = layer.rows.get(definition) {
                return Some(value);
            }
            if layer.removed.contains_key(definition) {
                return None;
            }
            layer = layer.parent.as_deref()?;
        }
    }
    fn iter(&self) -> PreparedRowsIter<'_, V> {
        PreparedRowsIter {
            root: self,
            layer: self,
            rows: self.rows.iter(),
        }
    }
    fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, value)| value)
    }
}
impl<V> Drop for PreparedRows<V> {
    fn drop(&mut self) {
        let mut parent = self.parent.take();
        while let Some(mut rows) = parent.and_then(Arc::into_inner) {
            parent = rows.parent.take();
        }
    }
}
struct PreparedRowsIter<'a, V> {
    root: &'a PreparedRows<V>,
    layer: &'a PreparedRows<V>,
    rows: hashbrown::hash_map::Iter<'a, DefId, V>,
}
impl<'a, V> Iterator for PreparedRowsIter<'a, V> {
    type Item = (&'a DefId, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            for (definition, value) in self.rows.by_ref() {
                if self
                    .root
                    .get(definition)
                    .is_some_and(|current| std::ptr::eq(current, value))
                {
                    return Some((definition, value));
                }
            }
            self.layer = self.layer.parent.as_deref()?;
            self.rows = self.layer.rows.iter();
        }
    }
}
/// One package's universe: the public table with some views replaced.
pub struct PackageViews {
    base: Arc<ViewTable>,
    replaced: Arc<PreparedRows<View>>,
    /// Verdicts checked again under the replacements, by view.
    verdicts: Arc<PreparedRows<Arc<ViewVerdict>>>,
    heap_role: Option<HeapRole>,
    heap_evidence: crate::HeapObservation,
    errors: UniverseErrors,
    input_digest: OnceLock<ContentHash>,
    stamp: EnvironmentStamp,
    origin_input: ContentHash,
    _retention: crate::memory::ControlRetention,
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
        Self::over(
            t,
            base,
            PreparedRows::new(replaced, None, DefMap::default())?,
            None,
            store,
            old,
        )
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
        let replaced = PreparedRows::new(
            [(view.d, view)].into_iter().collect(),
            Some(self.replaced.clone()),
            DefMap::default(),
        )?;
        let mut result = Self::over(t, self.base.clone(), replaced, Some(self), store, old)?;
        result.origin_input = self.input_digest();
        Ok(result)
    }
    /// The independently identified complete universe before this replacement.
    pub fn origin_input(&self) -> ContentHash {
        self.origin_input
    }
    /// This universe's summary, including its private views' effects.
    pub fn universe(&self) -> PackageUniverse {
        PackageUniverse::of(
            self.input_digest(),
            &self.errors,
            self.heap_evidence.clone(),
        )
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
                        .filter(|view| !self.base.contains(view.d)),
                );
            universe_input(
                views,
                &self.heap_evidence,
                self.errors.digest,
                self.base.installed.as_ref(),
            )
        })
    }
    fn over(
        t: &Arc<TypeInterner>,
        base: Arc<ViewTable>,
        replaced: Arc<PreparedRows<View>>,
        previous: Option<&Self>,
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
            replaced.iter().all(|(d, _)| base
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
        order.extend(replaced.values().filter(|v| !base.contains(v.d)));
        order.sort_by(|a, b| a.d.path().cmp(b.d.path()));
        let heap = combined(
            heap_role(order.iter().copied())?,
            base.installed
                .as_ref()
                .map(Installed::heap_role)
                .transpose()?
                .flatten(),
        )?;
        // Verdicts to check again: the replaced views and every view whose
        // verdict, public or already checked here, read one of them.
        let mut again: BTreeSet<DefId> = replaced.iter().map(|(d, _)| *d).collect();
        for (d, _) in replaced.iter() {
            for &i in base.readers.get(d).into_iter().flatten() {
                again.insert(base.views[i as usize].d);
            }
        }
        for (d, verdict) in previous.into_iter().flat_map(|views| views.verdicts.iter()) {
            if read_views(verdict).any(|read| replaced.get(&read).is_some()) {
                again.insert(*d);
            }
        }
        if heap != base.heap_role
            || replaced.iter().any(|(d, _)| {
                heap.as_ref().is_some_and(|h| {
                    *d == DefId::intern(&h.arena) || *d == DefId::intern(&h.operation)
                })
            })
        {
            again.extend(base.heap_readers.iter().map(|&i| base.views[i as usize].d));
            again.extend(
                previous
                    .into_iter()
                    .flat_map(|views| views.verdicts.iter())
                    .filter(|(_, verdict)| reads_heap(verdict))
                    .map(|(d, _)| *d),
            );
        }
        let order: Vec<DefId> = order.iter().map(|v| v.d).collect();
        let mut views = Self {
            origin_input: base.input_digest(),
            _retention: Default::default(),
            base,
            replaced,
            verdicts: PreparedRows::new(
                DefMap::default(),
                previous.map(|views| views.verdicts.clone()),
                DefMap::default(),
            )?,
            heap_role: heap,
            heap_evidence: crate::HeapObservation::None,
            errors: UniverseErrors::default(),
            input_digest: OnceLock::new(),
            stamp: EnvironmentStamp::new(t, store),
        };
        views.heap_evidence = crate::input::prepare_heap(&views, store, old)?;
        let verdicts: DefMap<_> = again
            .iter()
            .map(|&d| Ok((d, store.verdict_from(t, d, &views, old)?)))
            .filter(|result| match result {
                Ok((d, verdict)) => previous
                    .and_then(|views| views.verdicts.get(d))
                    .is_none_or(|checked| !Arc::ptr_eq(checked, verdict)),
                Err(_) => true,
            })
            .collect::<Result<_, ViewRefusal>>()?;
        let removed = previous
            .into_iter()
            .flat_map(|views| views.verdicts.iter())
            .filter(|(definition, _)| !again.contains(definition))
            .map(|(definition, _)| (*definition, ()))
            .collect();
        views.verdicts = PreparedRows::new(
            verdicts,
            previous.map(|views| views.verdicts.clone()),
            removed,
        )?;
        let errors = order
            .iter()
            .filter_map(|d| match views.verdicts.get(d) {
                Some(verdict) => verdict.error.clone(),
                None => views.base.verdict(*d).and_then(|v| v.error.clone()),
            })
            .collect();
        views.errors = UniverseErrors::new(errors);
        Ok(views)
    }
}
impl SignatureLookup for PackageViews {
    fn view(&self, d: DefId) -> Result<Option<crate::ViewInput<'_>>, crate::ViewRefusal> {
        match self.replaced.get(&d) {
            Some(v) => Ok(Some(v.input.input())),
            None => self.base.view(d),
        }
    }
    fn heap_role(&self) -> Result<Option<HeapRole>, ViewRefusal> {
        Ok(self.heap_role.clone())
    }
    fn heap_observation(&self) -> Result<crate::HeapObservation, ViewRefusal> {
        Ok(self.heap_evidence.clone())
    }
    fn universe_errors(&self) -> (ContentHash, &[Violation]) {
        (self.errors.digest, &self.errors.errors)
    }
}

/// A universe's identity: its checked views' and, beside them, its installed
/// parts' published identities.
fn universe_input<'a>(
    views: impl Iterator<Item = &'a View>,
    heap: &crate::HeapObservation,
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
    heap: &crate::HeapObservation,
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
    bytes.extend(bincode::serialize(heap).expect("heap evidence encodes"));
    bytes.extend(errors.0);
    ContentHash::of_parts("hirsel.validate.universe-input.v2", [bytes.as_slice()])
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
/// A fixed immutable checked environment with an additional allocation owner.
/// Keeping the owner alive preserves the cache's admitted metadata while read.
pub struct OwnedEnvironment {
    views: Arc<dyn CheckedEnvironment + Send + Sync>,
    _owner: Arc<dyn Send + Sync>,
}
impl OwnedEnvironment {
    pub fn new(
        views: Arc<dyn CheckedEnvironment + Send + Sync>,
        owner: Arc<dyn Send + Sync>,
    ) -> Self {
        Self {
            views,
            _owner: owner,
        }
    }
}
impl std::ops::Deref for OwnedEnvironment {
    type Target = dyn CheckedEnvironment;
    fn deref(&self) -> &Self::Target {
        &*self.views
    }
}
impl SignatureLookup for OwnedEnvironment {
    fn view(&self, d: DefId) -> Result<Option<crate::ViewInput<'_>>, crate::ViewRefusal> {
        self.views.view(d)
    }
    fn heap_role(&self) -> Result<Option<HeapRole>, ViewRefusal> {
        self.views.heap_role()
    }
    fn heap_observation(&self) -> Result<crate::HeapObservation, ViewRefusal> {
        self.views.heap_observation()
    }
    fn universe_errors(&self) -> (ContentHash, &[Violation]) {
        self.views.universe_errors()
    }
}
impl sealed::Environment for OwnedEnvironment {}
impl CheckedEnvironment for OwnedEnvironment {
    fn input_digest(&self) -> ContentHash {
        self.views.input_digest()
    }
    fn stamp(&self) -> &EnvironmentStamp {
        self.views.stamp()
    }
}
impl<T: CheckedEnvironment + ?Sized> sealed::Environment for &T {}
impl<T: CheckedEnvironment + ?Sized> CheckedEnvironment for &T {
    fn input_digest(&self) -> ContentHash {
        (**self).input_digest()
    }
    fn stamp(&self) -> &EnvironmentStamp {
        (**self).stamp()
    }
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
