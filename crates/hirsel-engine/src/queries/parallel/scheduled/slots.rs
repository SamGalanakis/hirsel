use super::*;

/// Slot replacement is the only way to change the completion index's owners.
/// Mutable access exposes outcomes, whose payload admission is independent.
#[derive(Default)]
pub(super) struct FrontSlots {
    rows: BTreeMap<Key, FrontSlot>,
    shared_observations: u64,
    owned_observations: u64,
}
impl std::ops::Deref for FrontSlots {
    type Target = BTreeMap<Key, FrontSlot>;
    fn deref(&self) -> &Self::Target {
        &self.rows
    }
}
impl FrontSlot {
    fn observation_metadata(&self) -> (u64, u64) {
        let observations = match self {
            Self::Archived(archived) => Some(&archived.observations),
            Self::Live { observations, .. } => observations.as_ref(),
        };
        match observations {
            None => (0, 0),
            Some(observations) if observations._backing.is_none() => {
                (observations.rows.metadata_bytes(), 0)
            }
            Some(observations) => (
                0,
                observations.rows.metadata_bytes()
                    + hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<
                        LiveSourceBacking,
                    >::uninit()),
            ),
        }
    }
}

impl FrontSlots {
    pub(super) fn insert(&mut self, key: Key, slot: FrontSlot) {
        let (shared, owned) = slot.observation_metadata();
        let (old_shared, old_owned) = self
            .rows
            .insert(key, slot)
            .as_ref()
            .map_or((0, 0), FrontSlot::observation_metadata);
        self.shared_observations = self.shared_observations - old_shared + shared;
        self.owned_observations = self.owned_observations - old_owned + owned;
    }
    pub(super) fn shared_observations(&self) -> u64 {
        self.shared_observations
    }
    pub(super) fn owned_observations(&self) -> u64 {
        self.owned_observations
    }
    pub(super) fn outcome_mut(&mut self, key: &Key) -> Option<&mut FrontOutcome> {
        match self.rows.get_mut(key)? {
            FrontSlot::Live { outcome, .. } => Some(outcome),
            FrontSlot::Archived(_) => None,
        }
    }
    pub(super) fn outcomes_mut(&mut self) -> impl Iterator<Item = (&Key, &mut FrontOutcome)> {
        self.rows.iter_mut().filter_map(|(key, slot)| match slot {
            FrontSlot::Live { outcome, .. } => Some((key, outcome)),
            FrontSlot::Archived(_) => None,
        })
    }
}

#[cfg(test)]
impl FrontBodyQueries {
    pub(in crate::queries::parallel) fn assert_slot_accounting_transitions(&self) {
        let source = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let FrontSlot::Live { outcome, .. } = source.values().next().unwrap() else {
            panic!("fixture needs completed facts");
        };
        let facts = outcome.facts.clone();
        drop(source);
        let reference = self.errors.resource;
        let definition = facts.definition;
        let memo = crate::memo::MemoKey::new(QueryKind::Check, definition);
        let observations = |owned: bool, count: usize| {
            Arc::new(FrontObservations {
                rows: observations::Rows::Retained(
                    vec![(memo, Arc::new(Reads::default())); count].into_boxed_slice(),
                ),
                _backing: owned.then(|| facts._backing.clone()),
            })
        };
        let live = |observations| FrontSlot::Live {
            outcome: FrontOutcome::failed(reference, QueryError::NotImplemented, facts.clone()),
            observations,
        };
        let archive = |observations| {
            FrontSlot::Archived(ArchivedFront {
                facts: reference,
                observations,
            })
        };
        let mut slots = FrontSlots::default();
        let mut pending = BTreeMap::new();
        let first = ContentHash::of_bytes(b"first slot");
        let second = ContentHash::of_bytes(b"second slot");
        let check =
            |slots: &FrontSlots, pending: &BTreeMap<DefId, usize>| {
                // Independent oracle: walk the actual owners and reconstruct the
                // former charge, without using the transition-accounting helpers.
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
                let mut owned = 0;
                let mut shared = 0;
                for slot in slots.values() {
                    let observations = match slot {
                        FrontSlot::Live { observations, .. } => observations.as_ref(),
                        FrontSlot::Archived(archived) => Some(&archived.observations),
                    };
                    if let Some(observations) = observations {
                        let bytes = observations.rows.metadata_bytes();
                        if observations._backing.is_some() {
                            owned += bytes
                                + hirsel_engine_admission::retained_arc_bytes(
                                    &std::mem::MaybeUninit::<LiveSourceBacking>::uninit(),
                                );
                        } else {
                            shared += bytes;
                            weight.add(bytes as usize);
                        }
                    }
                }
                assert_eq!(slots.shared_observations(), shared);
                assert_eq!(slots.owned_observations(), owned);
                if weight.bytes() != 0 {
                    weight.add(hirsel_engine_admission::retained_arc_bytes(
                        &std::mem::MaybeUninit::<LiveSourceBacking>::uninit(),
                    ) as usize);
                }
                let mut expected = weight.bytes();
                if !pending.is_empty() {
                    expected = expected.max(Self::slot_inventory(
                        slots.len() + pending.len(),
                        pending.len(),
                    ));
                }
                drop(clean);
                drop(work);
                let actual = self.slot_admission_bytes(pending, slots);
                assert_eq!(actual + slots.owned_observations(), expected + owned);
                // A pressure decision at either side of the exact boundary must
                // agree, including when some of the grant is already held.
                for held in [0, expected / 2, expected] {
                    let needed = expected.saturating_sub(held);
                    for available in [needed.saturating_sub(1), needed, needed + 1] {
                        assert_eq!(
                            actual.saturating_sub(held) <= available,
                            needed <= available
                        );
                    }
                }
            };
        check(&slots, &pending);
        pending.insert(definition, 1);
        check(&slots, &pending);
        slots.insert(first, live(None));
        check(&slots, &pending);
        slots.insert(first, live(Some(observations(false, 2))));
        check(&slots, &pending);
        slots.insert(first, archive(observations(false, 3)));
        check(&slots, &pending);
        slots.insert(second, archive(observations(true, 2)));
        check(&slots, &pending);
        slots.insert(
            second,
            archive(Arc::new(FrontObservations {
                rows: observations::Rows::Archived {
                    keys: vec![memo; 4].into_boxed_slice(),
                    reference,
                },
                _backing: Some(facts._backing.clone()),
            })),
        );
        check(&slots, &pending);
        slots.insert(first, live(Some(observations(true, 1))));
        check(&slots, &pending);
        slots.insert(second, live(None));
        check(&slots, &pending);
        pending.clear();
        check(&slots, &pending);
        slots.insert(first, live(None));
        check(&slots, &pending);
    }
}
