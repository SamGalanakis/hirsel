//! Typed native descriptor transport through the ordinary product codec. The
//! engine owns anchor rebinding; native producers do not duplicate its catalog.
use crate::QueryEngine;
use hirsel_engine_store::ResultStore;
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
#[derive(Clone)]
pub(crate) struct NativeRecordIdentity {
    types: std::sync::Weak<hirsel_types::TypeInterner>,
    bindings: std::sync::Weak<SourceBindings>,
}
impl NativeRecordIdentity {
    pub(crate) fn matches(&self, engine: &QueryEngine) -> bool {
        if !self.types.ptr_eq(&Arc::downgrade(engine.types())) {
            return false;
        }
        // The caller also proves unchanged source inputs and engine revision.
        // Read the existing projection without reconstructing an expired payload.
        engine
            .source_ownership
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|current| {
                current.placement == engine.sources.placement_revision()
                    && self.types.ptr_eq(&current.types)
                    && self.bindings.ptr_eq(&current.bindings)
            })
    }
    pub(crate) fn control_bytes() -> u64 {
        (size_of::<hirsel_types::TypeInterner>()
            + size_of::<SourceBindings>()
            + 4 * size_of::<usize>()) as u64
    }
}
impl NativeRecordContext {
    pub(crate) fn identity(&self) -> NativeRecordIdentity {
        NativeRecordIdentity {
            types: Arc::downgrade(&self.types),
            bindings: Arc::downgrade(&self.bindings),
        }
    }
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
    /// This generation and its ancestors, root first. Alias deltas make long
    /// chains of one-binding generations, so whole views merge the chain once
    /// instead of filtering every inherited row through every descendant.
    fn generations(&self) -> Vec<&Self> {
        let mut chain = vec![self];
        while let Some(parent) = &chain[chain.len() - 1].parent {
            chain.push(parent);
        }
        chain.reverse();
        chain
    }
    /// The effective view: a key's nearest generation wins, in key order.
    fn effective<'a, K: Ord + 'a, T: 'a>(
        &'a self,
        rows: impl Fn(&'a Self) -> &'a [T],
        key: impl Fn(&'a T) -> K,
    ) -> Box<dyn Iterator<Item = &'a T> + 'a> {
        if self.parent.is_none() {
            return Box::new(rows(self).iter());
        }
        let mut view = BTreeMap::new();
        for generation in self.generations() {
            for row in rows(generation) {
                view.insert(key(row), row);
            }
        }
        Box::new(view.into_values())
    }
    fn owners(&self) -> Box<dyn Iterator<Item = &(Anchor, (RecordOwner, u32))> + '_> {
        self.effective(|bindings| &bindings.owners, |(anchor, _)| *anchor)
    }
    fn anchors(&self) -> Box<dyn Iterator<Item = &((RecordOwner, u32), Anchor)> + '_> {
        self.effective(|bindings| &bindings.anchors, |(owner, _)| owner)
    }
    fn definitions(&self) -> Box<dyn Iterator<Item = &(Anchor, DefId)> + '_> {
        self.effective(
            |bindings| &bindings.source_definitions,
            |(anchor, _)| *anchor,
        )
    }
    fn bytes(&self) -> Option<u64> {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
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
                && current.scalar.as_ref().is_some_and(|scalar| {
                    scalar
                        .bindings
                        .generations()
                        .into_iter()
                        .any(|generation| std::ptr::eq(generation, self))
                })
            {
                // Release only a hold that would pin this charge.
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
    /// An inherited capture is not what a build over this engine's tables
    /// gives: a build prefers this engine's items, then its catalog, and the
    /// capture fills only what both leave empty. Placement keeps only a
    /// projection this engine built.
    inherited: bool,
}
/// The engine releases its hold on bindings once a partition admits them, so
/// the partition's charge ends with its last holder. When that holder drops
/// the projected generation, the engine takes back an unadmitted, flattened
/// copy of the same view, as it held before admission, instead of rebuilding
/// the whole context on its next read. The reservation and every parent
/// generation are released here.
impl Drop for SourceBindings {
    fn drop(&mut self) {
        if !self.admitted() {
            return;
        }
        let Some(cache) = self.cache.upgrade() else {
            return;
        };
        // A projection replaced under this lock is being retired anyway.
        let Ok(mut current) = cache.current.try_lock() else {
            return;
        };
        let Some(projection) = current.as_mut() else {
            return;
        };
        if projection.scalar.is_some() || !std::ptr::eq(projection.bindings.as_ptr(), self) {
            return;
        }
        let Some(types) = projection.types.upgrade() else {
            return;
        };
        let (owners, anchors, source_definitions) = if self.parent.is_none() {
            (
                std::mem::take(&mut self.owners),
                std::mem::take(&mut self.anchors),
                std::mem::take(&mut self.source_definitions),
            )
        } else {
            (
                self.owners().cloned().collect(),
                self.anchors().cloned().collect(),
                self.definitions().copied().collect(),
            )
        };
        let bindings = Arc::new(SourceBindings {
            owners,
            anchors,
            source_definitions,
            parent: None,
            admission: Default::default(),
            cache: self.cache.clone(),
        });
        projection.bindings = Arc::downgrade(&bindings);
        projection.scalar = Some(NativeRecordContext { types, bindings });
    }
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
    /// Places one new declaration. The current source projection takes the
    /// declaration's own bindings as a delta, and is emptied (rebuilt on its
    /// next read) only when the declaration changes an answer the projection
    /// already gives: which owner an existing anchor resolves to, that owner's
    /// declaration, or the anchor an existing reverse binding names.
    pub(crate) fn materialize_syntax(
        &mut self,
        path: &DefPath,
        syntax: Arc<hirsel_syntax::ItemSyntax>,
    ) {
        let placement = self.placement(path, &syntax);
        let Some(delta) = placement else {
            self.items.insert(path.clone(), vec![syntax]);
            return;
        };
        Arc::make_mut(&mut self.items.values).insert(path.clone(), vec![syntax]);
        if let Some(context) = delta {
            *self
                .source_ownership
                .current
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(SourceProjection {
                placement: self.sources.placement_revision(),
                types: Arc::downgrade(&context.types),
                bindings: Arc::downgrade(&context.bindings),
                // Hold the new delta until its next capture admits it.
                scalar: Some(context),
                inherited: false,
            });
        }
        // Each recheck rebuilds the whole projection, so the sample is far
        // sparser than a carried row's: a program places tens of thousands
        // of declarations.
        #[cfg(debug_assertions)]
        if super::namespace::Recheck::of_process().selects(path, 0, 1024) {
            self.recheck_source_projection();
        }
    }
    /// How placing `syntax` at `path` changes the current projection: `None`
    /// when it must be rebuilt, `Some(None)` when it already answers for the
    /// declaration, and otherwise the delta generation that does.
    ///
    /// A build takes an anchor's owner from the first item in path order
    /// that cites it, ahead of catalog and generated fallbacks, and keys its
    /// reverse binding by the item's own path and occurrence; extending with
    /// an inherited base only fills keys the build leaves empty.
    fn placement(
        &self,
        path: &DefPath,
        syntax: &hirsel_syntax::ItemSyntax,
    ) -> Option<Option<NativeRecordContext>> {
        if syntax.item.definition != *path || self.items.contains_key(path) {
            return None;
        }
        let bindings = {
            let current = self
                .source_ownership
                .current
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let projection = current.as_ref()?;
            if projection.inherited
                || projection.placement != self.sources.placement_revision()
                || !projection.types.ptr_eq(&Arc::downgrade(self.types()))
            {
                return None;
            }
            projection.bindings.upgrade()?
        };
        let live = NativeRecordContext {
            types: self.types.clone(),
            bindings,
        };
        let (anchor, occurrence) = (syntax.anchor, syntax.occurrence);
        // A build indexes only placed anchors.
        if self.sources.anchor(anchor).is_none() {
            return Some(None);
        }
        let mut owners = Vec::new();
        let mut source_definitions = Vec::new();
        match live.record_owner_ref(anchor) {
            Some((owner, at)) if owner == path => {
                if at != occurrence || live.bindings.definition(anchor) != DefId::get(path) {
                    return None;
                }
            }
            // An earlier item keeps owner selection.
            Some((owner, at))
                if owner < path
                    && self.items.get(owner).is_some_and(|items| {
                        items
                            .iter()
                            .any(|item| item.anchor == anchor && item.occurrence == at)
                    }) => {}
            // The declaration takes an anchor some other owner resolves to.
            Some(_) => return None,
            None => {
                owners.push((anchor, (RecordOwner::capture(path), occurrence)));
                source_definitions.push((anchor, DefId::intern(path)));
            }
        }
        let mut anchors = Vec::new();
        match live.record_source_anchor(path, occurrence) {
            Some(bound) if bound == anchor => {}
            Some(_) => return None,
            None => anchors.push(((RecordOwner::capture(path), occurrence), anchor)),
        }
        if owners.is_empty() && anchors.is_empty() {
            return Some(None);
        }
        Some(Some(NativeRecordContext {
            types: live.types,
            bindings: Arc::new(SourceBindings {
                owners: owners.into_boxed_slice(),
                anchors: anchors.into_boxed_slice(),
                source_definitions: source_definitions.into_boxed_slice(),
                parent: Some(live.bindings),
                admission: Default::default(),
                cache: Arc::downgrade(&self.source_ownership),
            }),
        }))
    }
    /// A checked build compares the kept projection with a fresh rebuild; any
    /// difference is a wrong anchor answer, so it fails naming the anchor.
    #[cfg(debug_assertions)]
    fn recheck_source_projection(&self) {
        let (kept, fresh) = (self.source_records(), self.fresh_source_records());
        let owners = |context: &NativeRecordContext| {
            context
                .bindings
                .owners()
                .map(|(anchor, (owner, at))| (*anchor, (owner.path().clone(), *at)))
                .collect::<BTreeMap<_, _>>()
        };
        let anchors = |context: &NativeRecordContext| {
            context
                .bindings
                .anchors()
                .map(|((owner, at), anchor)| ((owner.path().clone(), *at), *anchor))
                .collect::<BTreeMap<_, _>>()
        };
        let definitions = |context: &NativeRecordContext| {
            context
                .bindings
                .definitions()
                .map(|(anchor, definition)| (*anchor, definition.path().clone()))
                .collect::<BTreeMap<_, _>>()
        };
        fn first_difference<K: Ord + Clone, V: PartialEq>(
            kept: &BTreeMap<K, V>,
            fresh: &BTreeMap<K, V>,
        ) -> Option<K> {
            kept.keys()
                .chain(fresh.keys())
                .find(|key| kept.get(key) != fresh.get(key))
                .cloned()
        }
        let (kept_owners, fresh_owners) = (owners(&kept), owners(&fresh));
        if let Some(anchor) = first_difference(&kept_owners, &fresh_owners) {
            panic!(
                "kept source projection gives anchor {anchor:?} owner {:?}, a rebuild {:?}",
                kept_owners.get(&anchor),
                fresh_owners.get(&anchor)
            );
        }
        let (kept_anchors, fresh_anchors) = (anchors(&kept), anchors(&fresh));
        if let Some(owner) = first_difference(&kept_anchors, &fresh_anchors) {
            panic!(
                "kept source projection binds {owner:?} to anchor {:?}, a rebuild to {:?}",
                kept_anchors.get(&owner),
                fresh_anchors.get(&owner)
            );
        }
        let (kept_definitions, fresh_definitions) = (definitions(&kept), definitions(&fresh));
        if let Some(anchor) = first_difference(&kept_definitions, &fresh_definitions) {
            panic!(
                "kept source projection gives anchor {anchor:?} declaration {:?}, a rebuild {:?}",
                kept_definitions.get(&anchor),
                fresh_definitions.get(&anchor)
            );
        }
    }
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
            inherited: true,
        });
        *self
            .source_ownership
            .inherited
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(context);
    }
    /// The projection a build gives now, ignoring the kept one.
    fn fresh_source_records(&self) -> NativeRecordContext {
        let context = NativeRecordContext::build(self);
        match self
            .source_ownership
            .inherited
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            Some(inherited) => context.extend(inherited, &self.sources),
            None => context,
        }
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
        hirsel_intern::work::count(hirsel_intern::work::Counter::SourceProjectionBuilds);
        let context = self.fresh_source_records();
        *current = Some(SourceProjection {
            placement: self.sources.placement_revision(),
            types: Arc::downgrade(&context.types),
            bindings: Arc::downgrade(&context.bindings),
            scalar: (!context.bindings.admitted()).then(|| context.clone()),
            inherited: false,
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
        // The inherited effective view, merged once; every check below is a
        // lookup in it rather than a walk of its generation chain.
        let inherited_owners = inherited
            .bindings
            .owners()
            .map(|(anchor, owner)| (*anchor, owner))
            .collect::<BTreeMap<_, _>>();
        let inherited_anchors = inherited
            .bindings
            .anchors()
            .map(|(owner, anchor)| (owner, *anchor))
            .collect::<BTreeMap<_, _>>();
        let inherited_definitions = inherited
            .bindings
            .definitions()
            .copied()
            .collect::<BTreeMap<_, _>>();
        for (anchor, owner) in &inherited_owners {
            owners.entry(*anchor).or_insert_with(|| (*owner).clone());
        }
        // Many inherited anchors cite the same pair of files; compare each
        // pair's bytes once.
        let mut same_text = BTreeMap::new();
        for (owner, anchor) in &inherited_anchors {
            let mut same_source = |other| {
                let ((old_file, old_base), (new_file, new_base)) =
                    (sources.anchor(*anchor)?, sources.anchor(other)?);
                let (old, new) = (sources.file(old_file)?, sources.file(new_file)?);
                Some(
                    old_base == new_base
                        && old.logical_path() == new.logical_path()
                        && *same_text
                            .entry((old_file, new_file))
                            .or_insert_with(|| std::ptr::eq(old, new) || old.text() == new.text()),
                )
            };
            match anchors.entry((*owner).clone()) {
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
        for (anchor, definition) in &inherited_definitions {
            definitions.entry(*anchor).or_insert(*definition);
        }
        owners.retain(|anchor, owner| inherited_owners.get(anchor) != Some(&&*owner));
        anchors.retain(|owner, anchor| inherited_anchors.get(owner) != Some(anchor));
        definitions
            .retain(|anchor, definition| inherited_definitions.get(anchor) != Some(definition));
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
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
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
        // Persistent alias deltas also retain their parent headers and arrays.
        Some(weight.bytes().max(self.input_bytes()?))
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
                        .or_insert_with(|| (RecordOwner::capture(path), item.occurrence));
                }
            }
        }
        let catalog_owners: BTreeMap<_, _> =
            engine
                .catalog
                .bindings()
                .fold(BTreeMap::new(), |mut owners, (anchor, owner)| {
                    owners
                        .entry(anchor)
                        .or_insert_with(|| (RecordOwner::capture(owner.0), owner.1));
                    owners
                });
        let mut ordinary_anchors = Vec::new();
        for (path, items) in &engine.items {
            for item in items {
                if engine.sources.anchor(item.anchor).is_some() {
                    ordinary_anchors
                        .push(((RecordOwner::capture(path), item.occurrence), item.anchor));
                }
            }
        }
        for (anchor, owner) in engine.catalog.bindings() {
            if engine.sources.anchor(anchor).is_some() {
                ordinary_anchors.push(((RecordOwner::capture(owner.0), owner.1), anchor));
            }
        }
        let mut generated = BTreeMap::new();
        for (path, source) in &engine.generated {
            generated.entry(source.file).or_insert_with(|| {
                (
                    RecordOwner::capture(&super::persistent::generated_codec_owner(
                        path,
                        &source.text,
                    )),
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
            anchors.push((owner.clone(), *anchor));
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
                anchors.push((owner.clone(), anchor));
                let declaration = generated
                    .get(&file)
                    .map(|(_, declaration)| *declaration)
                    .unwrap_or_else(|| DefId::intern(owner.0.path()));
                source_definitions.insert(anchor, declaration);
                owners.insert(anchor, owner);
            }
        }
        // Stable sorting retains the first insertion for every equal key,
        // exactly as entry().or_insert() did. The item and catalog runs already
        // arrive in path order, so no tree is needed while building this array.
        anchors.sort_by(|(left, _), (right, _)| left.cmp(right));
        anchors.dedup_by(|(left, _), (right, _)| left == right);
        Self {
            types: engine.types().clone(),
            bindings: Arc::new(SourceBindings {
                parent: None,
                admission: Default::default(),
                cache: Arc::downgrade(&engine.source_ownership),
                owners: owners.into_iter().collect(),
                anchors: anchors.into_boxed_slice(),
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
    boundary: hirsel_engine_store::ArchiveBoundary,
    store: Arc<dyn ResultStore>,
    records: Arc<TypeRecords>,
    types: Arc<hirsel_types::TypeInterner>,
}

pub(crate) struct NativeRecordEncoder<'a> {
    encoding: hirsel_product_codec::RecordEncoding<'a>,
    _admission: &'a hirsel_engine_admission::MemoryReservation,
    store: &'a dyn ResultStore,
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
        self.store
            .archive_encoded(hirsel_engine_store::ArchiveBoundary::Export);
        self.encoding.encode(value)
    }
}

impl NativeRecords {
    pub(crate) fn for_boundary(&self, boundary: hirsel_engine_store::ArchiveBoundary) -> Self {
        Self {
            boundary,
            ..self.clone()
        }
    }
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
            store: &*self.store,
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
            boundary: hirsel_engine_store::ArchiveBoundary::Ordinary,
            records: TypeRecords::new(
                types.clone(),
                Arc::new(super::persistent::Contents(store.clone())),
            ),
            store,
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
        self.encode_at(source, value, self.boundary)
    }
    pub(crate) fn encode_at<T: Serialize>(
        &self,
        source: &(impl NativeRecordSource + ?Sized),
        value: &T,
        boundary: hirsel_engine_store::ArchiveBoundary,
    ) -> Option<Vec<u8>> {
        if !Arc::ptr_eq(&self.types, source.record_types()) {
            return None;
        }
        self.store.archive_encoded(boundary);
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
        self.store.archive_encoded(self.boundary);
        self.records
            .encode_tir(definition, |anchor| source.record_owner(anchor))
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
        self.store.archive_encoded(self.boundary);
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

pub(crate) struct InstalledDecode<'a, S> {
    requirement: hirsel_product_codec::DecodeRequirement<S>,
    source: &'a dyn NativeRecordSource,
}
impl<S> InstalledDecode<'_, S> {
    pub(crate) fn required_bytes(&self) -> u64 {
        self.requirement.required_bytes()
    }
}
impl InstalledDecode<'_, hirsel_product_codec::InstalledHeader> {
    pub(crate) fn decode(
        self,
        reservation: &hirsel_engine_admission::MemoryReservation,
    ) -> Option<(hirsel_types::SignatureRef, hirsel_types::SignatureRef)> {
        self.requirement.decode(reservation, |owner, occurrence| {
            self.source.record_source_anchor(owner, occurrence)
        })
    }
}
impl InstalledDecode<'_, hirsel_product_codec::InstalledSyntax> {
    pub(crate) fn decode(
        self,
        reservation: &hirsel_engine_admission::MemoryReservation,
    ) -> Option<Arc<hirsel_syntax::ItemSyntax>> {
        self.requirement.decode(reservation, |owner, occurrence| {
            self.source.record_source_anchor(owner, occurrence)
        })
    }
}
impl NativeRecords {
    pub(crate) fn installed_header<'a>(
        &self,
        source: &'a dyn NativeRecordSource,
        bytes: hirsel_artifact_store::Bytes,
    ) -> Option<InstalledDecode<'a, hirsel_product_codec::InstalledHeader>> {
        if !Arc::ptr_eq(&self.types, source.record_types()) {
            return None;
        }
        Some(InstalledDecode {
            requirement: self.records.installed_header(bytes)?,
            source,
        })
    }
    pub(crate) fn installed_syntax<'a>(
        &self,
        source: &'a dyn NativeRecordSource,
        bytes: hirsel_artifact_store::Bytes,
    ) -> Option<InstalledDecode<'a, hirsel_product_codec::InstalledSyntax>> {
        if !Arc::ptr_eq(&self.types, source.record_types()) {
            return None;
        }
        Some(InstalledDecode {
            requirement: self.records.installed_syntax(bytes)?,
            source,
        })
    }
}

impl NativeRecords {
    pub(crate) fn decode_admitted<
        T: hirsel_product_codec::decode_schema::Walk + DeserializeOwned,
    >(
        &self,
        source: &(impl NativeRecordSource + ?Sized),
        bytes: hirsel_artifact_store::Bytes,
        scratch: u64,
    ) -> Option<(T, u64)> {
        if !Arc::ptr_eq(&self.types, source.record_types()) {
            return None;
        }
        let requirement = self.records.record_requirement::<T>(bytes)?;
        let required = requirement.required_bytes();
        if required > scratch {
            return None;
        }
        let value = requirement.decode_reserved(
            |bytes| self.store.try_admit_product_memory(bytes),
            |owner, occurrence| source.record_source_anchor(owner, occurrence),
        )?;
        Some((value, required))
    }
}

impl NativeRecords {
    pub(crate) fn required_record<T: hirsel_product_codec::decode_schema::Walk>(
        &self,
        bytes: hirsel_artifact_store::Bytes,
    ) -> Option<u64> {
        Some(
            self.records
                .record_requirement::<T>(bytes)?
                .required_bytes(),
        )
    }
}
