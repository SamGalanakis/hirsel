//! Request-wide backend facts extracted on the actual Value miss and carried
//! in the C-bound discovery descriptor. Selection never opens a Value body.
use super::*;
use hirsel_lir::{
    ir::{LirUnit, Terminator},
    validate::NativeEnvironment,
};
use hirsel_syntax::DefPath;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{collections::BTreeSet, sync::OnceLock};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EmissionFacts {
    pub callbacks: Vec<DefPath>,
    pub tracking: bool,
    pub inline: InlineInstances,
}
/// One complete inline selection shared by a callable scope. A small escaped
/// view keeps the original registry grant; portable bytes contain only hashes.
#[derive(Clone, Default)]
pub struct InlineInstances(Option<Arc<InlineOwner>>);
struct InlineOwner {
    values: Box<[ContentHash]>,
    _grant: Option<Arc<hirsel_engine_admission::MemoryReservation>>,
    archive: OnceLock<Result<InlineArchive, String>>,
}
struct InlineArchive {
    reference: Reference,
}
const MAX_INLINE_MEMBERS: usize =
    hirsel_rc::instance::MAX_INSTANCES + hirsel_rc::closure_calls::MAX_SPECIALIZATIONS;
const INLINE_SELECTION: hirsel_engine_store::Domain = hirsel_engine_store::Domain {
    kind: 0x4e00_0034,
    schema: 1,
};
impl InlineInstances {
    pub fn new(
        values: impl IntoIterator<Item = ContentHash>,
        grant: Option<Arc<hirsel_engine_admission::MemoryReservation>>,
    ) -> Self {
        let values = values.into_iter().collect::<Box<[_]>>();
        if values.is_empty() {
            Self::default()
        } else {
            Self(Some(Arc::new(InlineOwner {
                values,
                _grant: grant,
                archive: OnceLock::new(),
            })))
        }
    }
    pub fn with_grant(
        self,
        grant: Option<Arc<hirsel_engine_admission::MemoryReservation>>,
    ) -> Self {
        match &self.0 {
            Some(owner) if owner._grant.is_none() && grant.is_some() => {
                Self::new(owner.values.iter().copied(), grant)
            }
            _ => self,
        }
    }
    pub fn identity(&self) -> Option<*const ()> {
        self.0.as_ref().map(|owner| Arc::as_ptr(owner).cast())
    }
    pub fn retained_bytes(&self) -> u64 {
        self.0.as_ref().map_or(0, |owner| {
            (size_of::<InlineOwner>() + 2 * size_of::<usize>() + size_of_val(&*owner.values)) as u64
        })
    }
    fn archive(&self) -> Result<Option<&InlineArchive>, String> {
        let Some(owner) = &self.0 else {
            return Ok(None);
        };
        owner
            .archive
            .get_or_init(|| {
                if owner.values.len() > MAX_INLINE_MEMBERS
                    || owner.values.windows(2).any(|pair| pair[0] >= pair[1])
                {
                    return Err("noncanonical inline selection".into());
                }
                let bytes = serde_json::to_vec(&owner.values).map_err(|error| error.to_string())?;
                let reference = INLINE_SELECTION
                    .reference(&bytes)
                    .ok_or("inline selection archive exceeds the record limit")?;
                Ok(InlineArchive { reference })
            })
            .as_ref()
            .map(Some)
            .map_err(Clone::clone)
    }
    fn archived_reference(&self) -> Option<Reference> {
        self.0
            .as_ref()?
            .archive
            .get()?
            .as_ref()
            .ok()
            .map(|archive| archive.reference)
    }
    pub fn publish(
        &self,
        store: &dyn hirsel_engine_store::ResultStore,
    ) -> Result<Option<Reference>, String> {
        let Some(archive) = self.archive()? else {
            return Ok(None);
        };
        let bytes = serde_json::to_vec(&self.0.as_ref().unwrap().values)
            .map_err(|error| error.to_string())?;
        if INLINE_SELECTION.reference(&bytes) != Some(archive.reference)
            || store.put_content(INLINE_SELECTION, &bytes) != Some(archive.reference)
        {
            return Err("inline selection publication differs".into());
        }
        Ok(Some(archive.reference))
    }
    fn read(
        reference: Reference,
        store: &dyn hirsel_engine_store::ResultStore,
        grant: Option<Arc<hirsel_engine_admission::MemoryReservation>>,
    ) -> Result<Self, String> {
        // A JSON hash array has at most 129 bytes per member plus its
        // separating comma, with one extra outer bracket byte.
        if reference.domain != INLINE_SELECTION
            || reference.len
                > (MAX_INLINE_MEMBERS as u64)
                    .saturating_mul(130)
                    .saturating_add(1)
        {
            return Err("foreign or oversized inline selection component".into());
        }
        let bytes = store
            .get_content(reference)
            .ok_or("missing inline selection component")?;
        if INLINE_SELECTION.reference(&bytes) != Some(reference) {
            return Err("inline selection component integrity differs".into());
        }
        let values: Box<[ContentHash]> = serde_json::from_slice(&bytes)
            .map_err(|error| format!("inline selection component: {error}"))?;
        if values.is_empty()
            || values.len() > MAX_INLINE_MEMBERS
            || values.windows(2).any(|pair| pair[0] >= pair[1])
            || serde_json::to_vec(&values).map_err(|error| error.to_string())? != bytes.as_ref()
        {
            return Err("noncanonical inline selection component".into());
        }
        Ok(Self(Some(Arc::new(InlineOwner {
            values,
            _grant: grant,
            archive: OnceLock::from(Ok(InlineArchive { reference })),
        }))))
    }
}

/// Durable discovery carries an ordered child reference, not a second copy of
/// the scope's complete inline list. A child is checked before its first use.
#[derive(Serialize, Deserialize)]
pub struct EmissionRecord {
    callbacks: Vec<DefPath>,
    tracking: bool,
    inline: Option<Reference>,
}
impl EmissionRecord {
    pub fn new(facts: &EmissionFacts, inline: Option<Reference>) -> Result<Self, String> {
        if !facts.canonical() || facts.inline.archive()?.map(|archive| archive.reference) != inline
        {
            return Err("emission inline component differs from its complete selection".into());
        }
        Ok(Self {
            callbacks: facts.callbacks.clone(),
            tracking: facts.tracking,
            inline,
        })
    }
    pub fn canonical(&self) -> bool {
        !self.callbacks.windows(2).any(|pair| pair[0] >= pair[1])
            && self
                .inline
                .is_none_or(|reference| reference.domain == INLINE_SELECTION && reference.len > 0)
    }
    pub fn read(
        self,
        store: &dyn hirsel_engine_store::ResultStore,
        cached: &InlineInstances,
        grant: Option<Arc<hirsel_engine_admission::MemoryReservation>>,
    ) -> Result<EmissionFacts, String> {
        if !self.canonical() {
            return Err("noncanonical emission component".into());
        }
        let inline = match self.inline {
            None => InlineInstances::default(),
            Some(reference) if cached.archived_reference() == Some(reference) => cached.clone(),
            Some(reference) => InlineInstances::read(reference, store, grant)?,
        };
        Ok(EmissionFacts {
            callbacks: self.callbacks,
            tracking: self.tracking,
            inline,
        })
    }
}
impl std::ops::Deref for InlineInstances {
    type Target = [ContentHash];
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().map_or(&[], |owner| &owner.values)
    }
}
impl AsRef<[ContentHash]> for InlineInstances {
    fn as_ref(&self) -> &[ContentHash] {
        self
    }
}
impl std::fmt::Debug for InlineInstances {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_ref().fmt(f)
    }
}
impl PartialEq for InlineInstances {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity() || **self == **other
    }
}
impl Eq for InlineInstances {}
impl Serialize for InlineInstances {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_ref().serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for InlineInstances {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Box::<[ContentHash]>::deserialize(deserializer).map(|values| Self::new(values, None))
    }
}

impl EmissionFacts {
    pub fn extract<S>(unit: &LirUnit<S>, inline: impl IntoIterator<Item = ContentHash>) -> Self {
        Self {
            callbacks: hirsel_native::stages::callbacks(unit)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            tracking: unit
                .terminators
                .iter()
                .any(|term| matches!(term, Terminator::Recover { .. })),
            inline: InlineInstances::new(inline.into_iter().collect::<BTreeSet<_>>(), None),
        }
    }
    pub fn canonical(&self) -> bool {
        !self.callbacks.windows(2).any(|pair| pair[0] >= pair[1])
            && !self.inline.windows(2).any(|pair| pair[0] >= pair[1])
    }
}
/// This mirrors native/stages.rs build_units' Value-based callback/recovery
/// selection and the actual specialization inline set. Ordinary stored rows
/// cannot authorize source/native acceptance; each input is re-admitted by C.
pub fn select(
    instances: &super::demand::DemandInstances,
    environment: &NativeEnvironment<'_>,
) -> Result<EmissionFacts, String> {
    let mut callbacks = BTreeSet::new();
    let mut inline = BTreeSet::new();
    let mut owners = BTreeSet::new();
    let mut tracking = false;
    for (owner, _) in instances.completed_inputs()? {
        let (current, _) = instances.admitted(&owner, environment)?;
        if !current.facts.emission.canonical() {
            return Err("noncanonical emission facts".into());
        }
        owners.insert(owner.instance.digest());
        callbacks.extend(current.facts.emission.callbacks.iter().cloned());
        tracking |= current.facts.emission.tracking;
        inline.extend(current.facts.emission.inline.iter().copied());
        if current.facts.source.ready_source.origin() != &owner.instance {
            if !owner
                .instance
                .is_same_or_derived_from(current.facts.source.ready_source.origin())
            {
                return Err("derived emission owner has another source origin".into());
            }
            inline.insert(owner.instance.digest());
        }
    }
    if !inline.is_subset(&owners) {
        return Err("emission inline instance has no admitted output".into());
    }
    Ok(EmissionFacts {
        callbacks: callbacks.into_iter().collect(),
        tracking,
        inline: InlineInstances::new(inline, None),
    })
}

#[cfg(test)]
mod component_tests {
    use super::*;
    use hirsel_engine_store::{Bytes, EntryKey, MemStore, ResultStore};

    struct ChangedBytes(Vec<u8>);
    impl ResultStore for ChangedBytes {
        fn admit_product_memory(&self, _bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
            None
        }

        fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
            None
        }

        fn capability(&self) -> hirsel_engine_store::ProviderCapability {
            hirsel_engine_store::ProviderCapability::ReadOnly
        }
        fn get(&self, _: &EntryKey) -> Option<Bytes> {
            None
        }
        fn put(&self, _: &EntryKey, _: &[u8]) -> hirsel_engine_store::WriteOutcome {
            hirsel_engine_store::WriteOutcome::Refused
        }
        fn get_content(&self, _: Reference) -> Option<Bytes> {
            Some(self.0.clone().into())
        }
        fn name(&self) -> &'static str {
            "changed-inline-child"
        }
    }

    #[test]
    fn inline_child_is_archived_once_and_fresh_readers_refuse_corruption_and_noncanonical_members()
    {
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let grant = Arc::new(budget.reserve(1 << 20).unwrap());
        let values = [
            ContentHash::of_bytes(b"first"),
            ContentHash::of_bytes(b"second"),
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        let facts = EmissionFacts {
            callbacks: Vec::new(),
            tracking: true,
            inline: InlineInstances::new(values.clone(), Some(grant.clone())),
        };
        let before = facts.inline.retained_bytes();
        let store = MemStore::default();
        let reference = facts.inline.publish(&store).unwrap().unwrap();
        let archive = facts.inline.archive().unwrap().unwrap();
        assert_eq!(archive.reference, reference);
        let archived = serde_json::to_vec(&values).unwrap();
        assert_eq!(
            store.get_content(reference).unwrap().as_ref(),
            archived.as_slice()
        );
        assert_eq!(
            facts.inline.retained_bytes(),
            before,
            "completed publication retains no encoded buffer"
        );
        assert_eq!(
            facts.inline.archive().unwrap().unwrap().reference,
            reference
        );
        let record = || EmissionRecord::new(&facts, Some(reference)).unwrap();
        let loaded = record()
            .read(&store, &Default::default(), Some(grant.clone()))
            .unwrap();
        assert_eq!(loaded, facts);
        let mut corrupted = archived.clone();
        corrupted[1] ^= 1;
        let changed = ChangedBytes(corrupted);
        assert!(
            record()
                .read(&changed, &Default::default(), Some(grant.clone()))
                .is_err()
        );
        let retained = record()
            .read(&changed, &loaded.inline, Some(grant.clone()))
            .unwrap();
        assert_eq!(retained.inline.identity(), loaded.inline.identity());
        let mut crossed = record();
        crossed.inline.as_mut().unwrap().output.0.0[0] ^= 1;
        assert!(
            crossed
                .read(&store, &Default::default(), Some(grant.clone()))
                .is_err()
        );
        let mut foreign = record();
        foreign.inline.as_mut().unwrap().domain.schema += 1;
        assert!(
            foreign
                .read(&store, &Default::default(), Some(grant.clone()))
                .is_err()
        );
        for bytes in [
            serde_json::to_vec(&[values.first().unwrap(); 2]).unwrap(),
            [b" ".as_slice(), archived.as_slice()].concat(),
        ] {
            let reference = store.put_content(INLINE_SELECTION, &bytes).unwrap();
            assert!(InlineInstances::read(reference, &store, Some(grant.clone())).is_err());
        }
        let escaped = loaded.inline.clone();
        drop(grant);
        drop(facts);
        drop(loaded);
        drop(retained);
        assert_eq!(budget.reserved(), 1 << 20);
        drop(escaped);
        assert_eq!(budget.reserved(), 0);
    }
}
