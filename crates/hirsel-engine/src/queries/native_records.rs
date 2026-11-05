//! Typed native descriptor transport through the ordinary product codec. The
//! engine owns anchor rebinding; native producers do not duplicate its catalog.
use crate::{QueryEngine, store::ResultStore};
use hirsel_product_codec::TypeRecords;
use hirsel_span::Anchor;
use hirsel_syntax::{DefId, DefPath};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::BTreeMap, sync::Arc};

/// Current source identity data used by the ordinary type/anchor codec. This
/// grants neither source V nor native C authority.
pub trait NativeRecordSource {
    fn record_types(&self) -> &Arc<hirsel_types::TypeInterner>;
    fn record_owner(&self, anchor: Anchor) -> Option<(DefPath, u32)>;
    fn record_source_anchor(&self, owner: &DefPath, occurrence: u32) -> Option<Anchor>;
}
impl NativeRecordSource for QueryEngine {
    fn record_types(&self) -> &Arc<hirsel_types::TypeInterner> {
        self.types()
    }
    fn record_owner(&self, anchor: Anchor) -> Option<(DefPath, u32)> {
        self.record_anchor_owner(anchor)
    }
    fn record_source_anchor(&self, owner: &DefPath, occurrence: u32) -> Option<Anchor> {
        self.record_anchor(owner, occurrence)
    }
}

/// Immutable source bindings for native workers, captured through the engine's
/// sole existing owner/anchor selection. Contains no QueryEngine, Definition or
/// signature collection. Unknown source generations remain unrepresentable.
#[derive(Clone)]
pub struct NativeRecordContext {
    types: Arc<hirsel_types::TypeInterner>,
    bindings: Arc<SourceBindings>,
}
struct SourceBindings {
    owners: Box<[(Anchor, (RecordOwner, u32))]>,
    anchors: Box<[((RecordOwner, u32), Anchor)]>,
    source_definitions: Box<[(Anchor, DefId)]>,
    parent: Option<Arc<SourceBindings>>,
    admission: std::sync::OnceLock<hirsel_engine_admission::MemoryReservation>,
    cache: std::sync::Weak<SourceOwnership>,
}
impl SourceBindings {
    fn owner(&self, anchor: Anchor) -> Option<&(RecordOwner, u32)> {
        self.owners
            .binary_search_by_key(&anchor, |(anchor, _)| *anchor)
            .ok()
            .map(|at| &self.owners[at].1)
            .or_else(|| self.parent.as_ref()?.owner(anchor))
    }
    fn anchor(&self, owner: &DefPath, occurrence: u32) -> Option<Anchor> {
        self.anchors
            .binary_search_by(|((selected, ordinal), _)| {
                (selected.path(), *ordinal).cmp(&(owner, occurrence))
            })
            .ok()
            .map(|at| self.anchors[at].1)
            .or_else(|| self.parent.as_ref()?.anchor(owner, occurrence))
    }
    fn definition(&self, anchor: Anchor) -> Option<DefId> {
        self.source_definitions
            .binary_search_by_key(&anchor, |(anchor, _)| *anchor)
            .ok()
            .map(|at| self.source_definitions[at].1)
            .or_else(|| self.parent.as_ref()?.definition(anchor))
    }
    fn owners(&self) -> Box<dyn Iterator<Item = &(Anchor, (RecordOwner, u32))> + '_> {
        Box::new(
            self.owners.iter().chain(
                self.parent
                    .iter()
                    .flat_map(|parent| parent.owners())
                    .filter(|(anchor, _)| {
                        self.owners
                            .binary_search_by_key(anchor, |(anchor, _)| *anchor)
                            .is_err()
                    }),
            ),
        )
    }
    fn anchors(&self) -> Box<dyn Iterator<Item = &((RecordOwner, u32), Anchor)> + '_> {
        Box::new(
            self.anchors.iter().chain(
                self.parent
                    .iter()
                    .flat_map(|parent| parent.anchors())
                    .filter(|(owner, _)| {
                        self.anchors
                            .binary_search_by(|(selected, _)| selected.cmp(owner))
                            .is_err()
                    }),
            ),
        )
    }
    fn definitions(&self) -> Box<dyn Iterator<Item = &(Anchor, DefId)> + '_> {
        Box::new(
            self.source_definitions.iter().chain(
                self.parent
                    .iter()
                    .flat_map(|parent| parent.definitions())
                    .filter(|(anchor, _)| {
                        self.source_definitions
                            .binary_search_by_key(anchor, |(anchor, _)| *anchor)
                            .is_err()
                    }),
            ),
        )
    }
    fn bytes(&self) -> Option<u64> {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(size_of::<NativeRecordContext>() + size_of::<Self>() + 4 * size_of::<usize>());
        weight.add(
            size_of_val(&*self.owners)
                + size_of_val(&*self.anchors)
                + size_of_val(&*self.source_definitions),
        );
        for (owner, _) in self
            .owners
            .iter()
            .map(|(_, owner)| owner)
            .chain(self.anchors.iter().map(|(owner, _)| owner))
        {
            if let RecordOwner::Written(path) = owner {
                weight.arc(path);
                weight.path(path);
            }
        }
        Some(weight.bytes())
    }
    fn unadmitted_bytes(&self) -> Option<u64> {
        let own = if self.admission.get().is_some() {
            0
        } else {
            self.bytes()?
        };
        own.checked_add(
            self.parent
                .as_ref()
                .map_or(Some(0), |parent| parent.unadmitted_bytes())?,
        )
    }
    fn admitted(&self) -> bool {
        self.admission.get().is_some()
            || self.parent.as_ref().is_some_and(|parent| parent.admitted())
    }
    fn admit(&self, allowance: &mut hirsel_engine_admission::MemoryReservation) -> Option<()> {
        if let Some(parent) = &self.parent {
            parent.admit(allowance)?;
        }
        if self.admission.get().is_none() {
            let reservation = allowance.split(self.bytes()?)?;
            if let Err(reservation) = self.admission.set(reservation) {
                allowance.join(reservation).ok()?;
            }
            if let Some(cache) = self.cache.upgrade()
                && let Some(current) = cache
                    .current
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_mut()
            {
                current.scalar = None;
            }
        }
        Some(())
    }
}
#[derive(Clone)]
enum RecordOwner {
    Interned(DefId),
    Written(Arc<DefPath>),
}
impl RecordOwner {
    fn capture(path: &DefPath) -> Self {
        DefId::get(path).map_or_else(|| Self::Written(Arc::new(path.clone())), Self::Interned)
    }
    fn path(&self) -> &DefPath {
        match self {
            Self::Interned(id) => id.path(),
            Self::Written(path) => path,
        }
    }
}
impl PartialEq for RecordOwner {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Interned(left), Self::Interned(right)) => left == right,
            _ => self.path() == other.path(),
        }
    }
}
impl Eq for RecordOwner {}
impl PartialOrd for RecordOwner {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RecordOwner {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if self == other {
            std::cmp::Ordering::Equal
        } else {
            self.path().cmp(other.path())
        }
    }
}
/// Recomputable projections of current source products, never semantic read
/// observations. Mutating a source table drops this one product; detached
/// workers retain their immutable generation through NativeRecordContext.
struct SourceProjection {
    placement: u64,
    types: std::sync::Weak<hirsel_types::TypeInterner>,
    bindings: std::sync::Weak<SourceBindings>,
    scalar: Option<NativeRecordContext>,
}
#[derive(Default)]
pub(crate) struct SourceOwnership {
    current: std::sync::Mutex<Option<SourceProjection>>,
    // Execution assemblies retain the complete admitted source base while
    // registrations add bindings. Ordinary edited engines have no inherited base.
    inherited: std::sync::Mutex<Option<NativeRecordContext>>,
}
impl SourceOwnership {
    fn changed(&self) {
        self.current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}
/// Source tables invalidate their own ownership projection on mutable access.
/// This also covers removals and rollback, without a separate host change log.
#[derive(Clone)]
pub(crate) struct SourceTable<K, V> {
    values: Arc<BTreeMap<K, V>>,
    ownership: Arc<SourceOwnership>,
}
impl<K, V> Default for SourceTable<K, V> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}
impl<K, V> SourceTable<K, V> {
    pub(super) fn owner(&self) -> std::sync::Weak<BTreeMap<K, V>> {
        Arc::downgrade(&self.values)
    }
    pub(super) fn observations(&self) -> Arc<BTreeMap<K, V>> {
        self.values.clone()
    }
    pub(super) fn new(ownership: Arc<SourceOwnership>) -> Self {
        Self {
            values: Arc::new(BTreeMap::new()),
            ownership,
        }
    }
    pub(super) fn bind(&mut self, ownership: Arc<SourceOwnership>) {
        ownership.changed();
        self.ownership = ownership;
    }
}
impl<K, V> std::ops::Deref for SourceTable<K, V> {
    type Target = BTreeMap<K, V>;
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}
impl<K: Clone, V: Clone> std::ops::DerefMut for SourceTable<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ownership.changed();
        Arc::make_mut(&mut self.values)
    }
}
impl<K: Clone, V: Clone> IntoIterator for SourceTable<K, V> {
    type Item = (K, V);
    type IntoIter = std::collections::btree_map::IntoIter<K, V>;
    fn into_iter(self) -> Self::IntoIter {
        Arc::try_unwrap(self.values)
            .unwrap_or_else(|shared| (*shared).clone())
            .into_iter()
    }
}
impl<'a, K, V> IntoIterator for &'a SourceTable<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::collections::btree_map::Iter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}
impl<K: Ord, V> FromIterator<(K, V)> for SourceTable<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        Self {
            values: Arc::new(iter.into_iter().collect()),
            ownership: Default::default(),
        }
    }
}
impl QueryEngine {
    pub(crate) fn inherit_source_records(&self, context: NativeRecordContext) {
        // Both reconstruction paths install the capture's exact source tables
        // before binding its records. A later source mutation invalidates this
        // projection and builds a successor against the inherited bindings.
        *self
            .source_ownership
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(SourceProjection {
            placement: self.sources.placement_revision(),
            types: Arc::downgrade(&context.types),
            bindings: Arc::downgrade(&context.bindings),
            scalar: None,
        });
        *self
            .source_ownership
            .inherited
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(context);
    }
    pub(crate) fn source_records(&self) -> NativeRecordContext {
        let mut current = self
            .source_ownership
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(projection) = &*current
            && projection.placement == self.sources.placement_revision()
            && projection.types.ptr_eq(&Arc::downgrade(self.types()))
            && let Some(bindings) = projection.bindings.upgrade()
        {
            return NativeRecordContext {
                types: self.types.clone(),
                bindings,
            };
        }
        let mut context = NativeRecordContext::build(self);
        if let Some(inherited) = self
            .source_ownership
            .inherited
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            context = context.extend(inherited, &self.sources);
        }
        *current = Some(SourceProjection {
            placement: self.sources.placement_revision(),
            types: Arc::downgrade(&context.types),
            bindings: Arc::downgrade(&context.bindings),
            scalar: (!context.bindings.admitted()).then(|| context.clone()),
        });
        context
    }
}
impl NativeRecordContext {
    fn extend(&self, inherited: &Self, sources: &hirsel_span::SourceMap) -> Self {
        let mut owners = self.bindings.owners().cloned().collect::<BTreeMap<_, _>>();
        let mut anchors = self.bindings.anchors().cloned().collect::<BTreeMap<_, _>>();
        let mut definitions = self
            .bindings
            .definitions()
            .copied()
            .collect::<BTreeMap<_, _>>();
        for (anchor, owner) in inherited.bindings.owners() {
            owners.entry(*anchor).or_insert_with(|| owner.clone());
        }
        for (owner, anchor) in inherited.bindings.anchors() {
            let same_source = |other| {
                let ((old_file, old_base), (new_file, new_base)) =
                    (sources.anchor(*anchor)?, sources.anchor(other)?);
                let (old, new) = (sources.file(old_file)?, sources.file(new_file)?);
                Some(
                    old_base == new_base
                        && old.logical_path() == new.logical_path()
                        && (std::ptr::eq(old, new) || old.text() == new.text()),
                )
            };
            match anchors.entry(owner.clone()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(*anchor);
                }
                std::collections::btree_map::Entry::Occupied(mut entry)
                    if same_source(*entry.get()) == Some(true) =>
                {
                    entry.insert(*anchor);
                }
                _ => {}
            }
        }
        for (anchor, definition) in inherited.bindings.definitions() {
            definitions.entry(*anchor).or_insert(*definition);
        }
        owners.retain(|anchor, owner| inherited.bindings.owner(*anchor) != Some(owner));
        anchors.retain(|(owner, occurrence), anchor| {
            inherited.bindings.anchor(owner.path(), *occurrence) != Some(*anchor)
        });
        definitions.retain(|anchor, definition| {
            inherited.bindings.definition(*anchor) != Some(*definition)
        });
        if owners.is_empty() && anchors.is_empty() && definitions.is_empty() {
            return Self {
                types: self.types.clone(),
                bindings: inherited.bindings.clone(),
            };
        }
        Self {
            types: self.types.clone(),
            bindings: Arc::new(SourceBindings {
                parent: Some(inherited.bindings.clone()),
                admission: Default::default(),
                cache: self.bindings.cache.clone(),
                owners: owners.into_iter().collect(),
                anchors: anchors.into_iter().collect(),
                source_definitions: definitions.into_iter().collect(),
            }),
        }
    }
    pub(crate) fn bindings_hold(&self, current: &Self) -> bool {
        Arc::ptr_eq(&self.types, &current.types)
            && (Arc::ptr_eq(&self.bindings, &current.bindings)
                || self
                    .bindings
                    .anchors()
                    .all(|((owner, occurrence), anchor)| {
                        current.record_source_anchor(owner.path(), *occurrence) == Some(*anchor)
                    }))
    }

    pub(crate) fn input_bytes(&self) -> Option<u64> {
        self.bindings
            .bytes()?
            .checked_add(self.bindings.parent.as_ref().map_or(Some(0), |parent| {
                NativeRecordContext {
                    types: self.types.clone(),
                    bindings: parent.clone(),
                }
                .input_bytes()
            })?)
    }
    pub(crate) fn unadmitted_bytes(&self) -> Option<u64> {
        self.bindings.unadmitted_bytes()
    }
    pub(crate) fn admit(
        &self,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Option<()> {
        self.bindings.admit(allowance)
    }
    pub(crate) fn planning_bytes(&self) -> Option<u64> {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(size_of::<Self>() + 3 * size_of::<Box<[usize]>>() + 4 * size_of::<usize>());
        weight.add(
            self.bindings
                .owners()
                .count()
                .checked_mul(size_of::<(Anchor, (RecordOwner, u32))>())?,
        );
        weight.add(
            self.bindings
                .anchors()
                .count()
                .checked_mul(size_of::<((RecordOwner, u32), Anchor)>())?,
        );
        weight.add(
            self.bindings
                .definitions()
                .count()
                .checked_mul(size_of::<(Anchor, DefId)>())?,
        );
        for (owner, _) in self
            .bindings
            .owners()
            .map(|(_, owner)| owner)
            .chain(self.bindings.anchors().map(|(owner, _)| owner))
        {
            if let RecordOwner::Written(path) = owner {
                weight.arc(path);
                weight.path(path);
            }
        }
        Some(weight.bytes())
    }

    /// The declaration owning this current source generation. A generated
    /// codec owner is content-bound and can differ from this declaration;
    /// selector ordinals are neither codec occurrences nor source bases.
    pub fn source_definition(&self, anchor: Anchor) -> Option<&DefPath> {
        Some(self.bindings.definition(anchor)?.path())
    }

    pub fn capture(engine: &QueryEngine) -> Self {
        if let Some(headers) = &engine.header_products {
            return (**headers.owners()).clone();
        }
        engine.source_records()
    }

    fn build(engine: &QueryEngine) -> Self {
        let mut owners = BTreeMap::new();
        let mut source_definitions = BTreeMap::new();
        // Scalar owner selection takes the first item in DefPath/occurrence
        // order. Index that same selection once, instead of scanning every
        // declaration for every source anchor.
        for (path, items) in &engine.items {
            for item in items {
                if engine.sources.anchor(item.anchor).is_some() {
                    owners
                        .entry(item.anchor)
                        .or_insert_with(|| (path.clone(), item.occurrence));
                }
            }
        }
        let catalog_owners: BTreeMap<_, _> =
            engine
                .catalog
                .bindings()
                .fold(BTreeMap::new(), |mut owners, (anchor, owner)| {
                    owners.entry(anchor).or_insert(owner);
                    owners
                });
        let mut ordinary_anchors = BTreeMap::new();
        for (path, items) in &engine.items {
            for item in items {
                if engine.sources.anchor(item.anchor).is_some() {
                    ordinary_anchors
                        .entry((path.clone(), item.occurrence))
                        .or_insert(item.anchor);
                }
            }
        }
        for (anchor, owner) in engine.catalog.bindings() {
            if engine.sources.anchor(anchor).is_some() {
                ordinary_anchors.entry(owner).or_insert(anchor);
            }
        }
        let mut generated = BTreeMap::new();
        for (path, source) in &engine.generated {
            generated.entry(source.file).or_insert_with(|| {
                (
                    super::persistent::generated_codec_owner(path, &source.text),
                    DefId::intern(path),
                )
            });
        }
        // Registration can repeat identical text while inferred headers still
        // cite its earlier placed source. Only the same logical source and
        // exact bytes may share its content-bound codec owner.
        let mut generations = BTreeMap::new();
        for (file, binding) in &generated {
            if let Some(identity) =
                super::persistent::generated_source_identity(&engine.sources, *file)
            {
                generations
                    .entry(identity)
                    .or_insert_with(|| binding.clone());
            }
        }
        let mut current_anchors = BTreeMap::new();
        for (anchor, file, base) in engine.sources.anchors() {
            if let Some((owner, _)) = generated.get(&file) {
                current_anchors
                    .entry((owner.clone(), base))
                    .or_insert(anchor);
            }
        }
        let mut anchors = ordinary_anchors;
        for (owner, anchor) in &current_anchors {
            anchors.entry(owner.clone()).or_insert(*anchor);
        }
        let mut seen = std::collections::BTreeSet::new();
        for (_, file, _) in engine.sources.anchors() {
            if generated.contains_key(&file) || !seen.insert(file) {
                continue;
            }
            if let Some(identity) =
                super::persistent::generated_source_identity(&engine.sources, file)
                && let Some(binding) = generations.get(&identity)
            {
                generated.insert(file, binding.clone());
            }
        }
        for (anchor, file, base) in engine.sources.anchors() {
            let owner = owners.get(&anchor).cloned().or_else(|| {
                generated
                    .get(&file)
                    .map(|(path, _)| (path.clone(), base))
                    .or_else(|| catalog_owners.get(&anchor).cloned())
            });
            if let Some(owner) = owner {
                anchors.entry(owner.clone()).or_insert(anchor);
                let declaration = generated
                    .get(&file)
                    .map(|(_, declaration)| *declaration)
                    .unwrap_or_else(|| DefId::intern(&owner.0));
                source_definitions.insert(anchor, declaration);
                owners.insert(anchor, owner);
            }
        }
        Self {
            types: engine.types().clone(),
            bindings: Arc::new(SourceBindings {
                parent: None,
                admission: Default::default(),
                cache: Arc::downgrade(&engine.source_ownership),
                owners: owners
                    .into_iter()
                    .map(|(anchor, (owner, occurrence))| {
                        (anchor, (RecordOwner::capture(&owner), occurrence))
                    })
                    .collect(),
                anchors: anchors
                    .into_iter()
                    .map(|((owner, occurrence), anchor)| {
                        ((RecordOwner::capture(&owner), occurrence), anchor)
                    })
                    .collect(),
                source_definitions: source_definitions.into_iter().collect(),
            }),
        }
    }

    pub(crate) fn record_owner_ref(&self, anchor: Anchor) -> Option<(&DefPath, u32)> {
        let (owner, occurrence) = self.bindings.owner(anchor)?;
        Some((owner.path(), *occurrence))
    }
}
impl NativeRecordSource for NativeRecordContext {
    fn record_types(&self) -> &Arc<hirsel_types::TypeInterner> {
        &self.types
    }
    fn record_owner(&self, anchor: Anchor) -> Option<(DefPath, u32)> {
        self.record_owner_ref(anchor)
            .map(|(owner, occurrence)| (owner.clone(), occurrence))
    }
    fn record_source_anchor(&self, owner: &DefPath, occurrence: u32) -> Option<Anchor> {
        self.bindings.anchor(owner, occurrence)
    }
}

/// One type/anchor codec over the selected provider and the owning interner.
/// Encoded recipes are ordinary data, never source V or native C authority.
#[derive(Clone)]
pub struct NativeRecords {
    records: Arc<TypeRecords>,
    types: Arc<hirsel_types::TypeInterner>,
}

pub(crate) struct NativeRecordEncoder<'a> {
    encoding: hirsel_product_codec::RecordEncoding<'a>,
    _admission: &'a hirsel_engine_admission::MemoryReservation,
}
impl NativeRecordEncoder<'_> {
    pub(crate) fn capture<T>(
        &mut self,
        bytes: u64,
        types: impl IntoIterator<Item = hirsel_types::TypeId>,
        capture: impl FnOnce(&hirsel_product_codec::RecordEncoding<'_>) -> T,
    ) -> Option<T> {
        self.encoding.capture(bytes, types, capture)
    }

    pub(crate) fn encode<T: Serialize>(self, value: &T) -> Option<Vec<u8>> {
        self.encoding.encode(value)
    }
}

impl NativeRecords {
    pub(crate) fn prepare_encoding<'a, T: Serialize>(
        &'a self,
        source: &NativeRecordContext,
        value: &T,
        admission: &'a hirsel_engine_admission::MemoryReservation,
        serialization_working: u64,
    ) -> Option<NativeRecordEncoder<'a>> {
        if !Arc::ptr_eq(&self.types, source.record_types()) {
            return None;
        }
        Some(NativeRecordEncoder {
            encoding: self.records.prepare_record_encoding(
                value,
                |anchor| source.record_owner_ref(anchor),
                admission.bytes(),
                serialization_working,
            )?,
            _admission: admission,
        })
    }

    pub(crate) fn encode_with_admission<T: Serialize>(
        &self,
        source: &NativeRecordContext,
        value: &T,
        admission: &hirsel_engine_admission::MemoryReservation,
        serialization_working: u64,
    ) -> Option<Vec<u8>> {
        self.prepare_encoding(source, value, admission, serialization_working)?
            .encode(value)
    }

    pub(crate) fn uses_types(&self, types: &Arc<hirsel_types::TypeInterner>) -> bool {
        Arc::ptr_eq(&self.types, types)
    }
    /// Initial inline allocations of a fresh record codec. Its type interner
    /// and store are shared. Valid only before any record encode or decode;
    /// consumers that populate its caches must admit that work separately.
    pub fn fresh_metadata_bytes() -> u64 {
        (size_of::<TypeRecords>()
            + size_of::<super::persistent::Contents>()
            + 4 * size_of::<usize>()) as u64
    }
    pub fn new(types: Arc<hirsel_types::TypeInterner>, store: Arc<dyn ResultStore>) -> Self {
        Self {
            records: TypeRecords::new(types.clone(), Arc::new(super::persistent::Contents(store))),
            types,
        }
    }
    pub fn encode<T: Serialize>(
        &self,
        source: &(impl NativeRecordSource + ?Sized),
        value: &T,
    ) -> Option<Vec<u8>> {
        if !Arc::ptr_eq(&self.types, source.record_types()) {
            return None;
        }
        self.records
            .encode_record(value, |anchor| source.record_owner(anchor))
    }
    /// Indexed source TIR, with complete type closure materialization. The
    /// store publishes its association only after this succeeds.
    pub fn encode_tir(
        &self,
        source: &(impl NativeRecordSource + ?Sized),
        definition: &hirsel_tir::Definition,
    ) -> Option<Vec<u8>> {
        if !Arc::ptr_eq(&self.types, source.record_types()) {
            return None;
        }
        self.records
            .encode_tir(definition, |anchor| source.record_owner(anchor))
    }
    pub fn encode_tir_with_decode_bytes(
        &self,
        source: &(impl NativeRecordSource + ?Sized),
        definition: &hirsel_tir::Definition,
    ) -> Option<(Vec<u8>, u64)> {
        if !Arc::ptr_eq(&self.types, source.record_types()) {
            return None;
        }
        self.records
            .encode_tir_with_decode_bytes(definition, |anchor| source.record_owner(anchor))
    }
    /// Bind independently checked rows to this consumer's current sources and
    /// type owner. Content and structural validation do not grant V authority.
    pub fn open_tir(
        &self,
        source: Arc<NativeRecordContext>,
        bytes: hirsel_artifact_store::Bytes,
        expected: hirsel_artifact_store::Reference,
        owner: Arc<dyn Send + Sync>,
    ) -> Option<hirsel_product_codec::CheckedTir> {
        if !Arc::ptr_eq(&self.types, source.record_types()) {
            return None;
        }
        hirsel_product_codec::CheckedTir::open(
            bytes.retain_owner(Arc::new((source.clone(), owner))),
            expected,
            self.records.clone(),
            |owner, occurrence| source.record_source_anchor(owner, occurrence),
        )
    }

    /// Canonical ordinary bytes and pre-admission for outer record decoding.
    /// The caller must add its typed payload's physical layout bound.
    pub fn encode_with_decode_bytes<T: Serialize>(
        &self,
        source: &(impl NativeRecordSource + ?Sized),
        value: &T,
    ) -> Option<(Vec<u8>, u64)> {
        if !Arc::ptr_eq(&self.types, source.record_types()) {
            return None;
        }
        self.records
            .encode_record_with_decode_bytes(value, |anchor| source.record_owner(anchor))
    }
    pub fn decode<T: DeserializeOwned>(
        &self,
        source: &(impl NativeRecordSource + ?Sized),
        bytes: &[u8],
    ) -> Option<T> {
        if !Arc::ptr_eq(&self.types, source.record_types()) {
            return None;
        }
        self.records.decode_record(bytes, |owner, occurrence| {
            source.record_source_anchor(owner, occurrence)
        })
    }

    pub(crate) fn visit_record_types(
        &self,
        bytes: &[u8],
        visit: impl FnMut(hirsel_artifact_store::Reference) -> Option<bool>,
    ) -> Option<()> {
        self.records.visit_record_types(bytes, visit)
    }
}

#[cfg(test)]
mod tests;
