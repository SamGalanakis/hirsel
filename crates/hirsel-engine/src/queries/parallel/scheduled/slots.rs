//! Front residency follows logical access and canonical local query rank.
use super::*;
use hirsel_query_products::local::LocalSlot;

#[derive(Default)]
pub(super) struct Slots {
    rows: BTreeMap<LocalSlot, FrontSlot>,
    recency: Mutex<Recency>,
    unbacked: u64,
    archived: u64,
}
#[derive(Default)]
struct Recency {
    clock: u64,
    ages: BTreeMap<LocalSlot, u64>,
    order: BTreeSet<(u64, LocalSlot)>,
}
impl Recency {
    fn touch(&mut self, key: LocalSlot) {
        if let Some(previous) = self.ages.remove(&key) {
            self.order.remove(&(previous, key));
        }
        // No wrap: an exhausted logical clock preserves canonical rank order.
        self.clock = self.clock.saturating_add(1);
        self.ages.insert(key, self.clock);
        self.order.insert((self.clock, key));
    }
}
impl FrontSlot {
    fn observation_bytes(&self) -> (u64, u64) {
        let observations = match self {
            Self::Archived(archived) => Some(&archived.observations),
            Self::Live { observations, .. } => observations.as_ref(),
        };
        observations.map_or((0, 0), |observations| {
            let bytes = observations.rows.metadata_bytes();
            if observations._backing.is_some() {
                (
                    0,
                    bytes
                        + hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<
                            LiveSourceBacking,
                        >::uninit(
                        )),
                )
            } else {
                (bytes, 0)
            }
        })
    }
}
impl Slots {
    pub(super) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }
    pub(super) fn contains_key(&self, key: &Key) -> bool {
        LocalSlot::decode(*key).is_some_and(|key| self.rows.contains_key(&key))
    }
    pub(super) fn get(&self, key: &Key) -> Option<&FrontSlot> {
        let key = LocalSlot::decode(*key)?;
        let row = self.rows.get(&key)?;
        self.recency
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .touch(key);
        Some(row)
    }
    // Observation owners are immutable. Only insert replaces their metadata.
    pub(super) fn get_mut(&mut self, key: &Key) -> Option<&mut FrontSlot> {
        let key = LocalSlot::decode(*key)?;
        let row = self.rows.get_mut(&key)?;
        self.recency
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .touch(key);
        Some(row)
    }
    pub(super) fn values(&self) -> impl Iterator<Item = &FrontSlot> + Clone {
        self.rows.values()
    }
    pub(super) fn values_mut(&mut self) -> impl Iterator<Item = &mut FrontSlot> {
        self.rows.values_mut()
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = (Key, &FrontSlot)> {
        self.rows.iter().map(|(key, slot)| (key.address(), slot))
    }
    pub(super) fn insert(&mut self, key: Key, slot: FrontSlot) {
        let key = LocalSlot::decode(key).expect("operation-local Front address");
        hirsel_intern::work::count(hirsel_intern::work::Counter::FrontSlotReplacements);
        let (unbacked, archived) = slot.observation_bytes();
        let old = self
            .rows
            .insert(key, slot)
            .map_or((0, 0), |old| old.observation_bytes());
        self.unbacked = self.unbacked - old.0 + unbacked;
        self.archived = self.archived - old.1 + archived;
        self.recency
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .touch(key);
    }
    pub(super) fn victim(&self, mut eligible: impl FnMut(Key, &FrontSlot) -> bool) -> Option<Key> {
        let recency = self.recency.lock().unwrap_or_else(|e| e.into_inner());
        recency.order.iter().find_map(|(_, key)| {
            hirsel_intern::work::count(hirsel_intern::work::Counter::FrontVictimCandidates);
            eligible(key.address(), &self.rows[key]).then_some(key.address())
        })
    }
    pub(super) fn index_bytes(
        weight: &mut hirsel_query_products::retained::MetadataWeight,
        len: usize,
    ) {
        if len != 0 {
            weight.tree::<LocalSlot, FrontSlot>(len);
            weight.tree::<LocalSlot, u64>(len);
            weight.tree::<(u64, LocalSlot), ()>(len);
        }
    }
    pub(super) fn unbacked_bytes(&self) -> u64 {
        self.unbacked
    }
    pub(super) fn archived_bytes(&self) -> u64 {
        self.archived
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incremental_inventory_and_victims_preserve_pins_and_replacements() {
        fn archived(keys: &[crate::memo::MemoKey]) -> FrontSlot {
            FrontSlot::Archived(ArchivedFront {
                facts: hirsel_engine_store::product_domain(QueryKind::Summary)
                    .reference(b"required-completion")
                    .unwrap(),
                observations: Arc::new(FrontObservations {
                    rows: observations::Rows::Retained(
                        keys.iter()
                            .map(|key| (*key, Arc::new(Reads::empty())))
                            .collect(),
                    ),
                    _backing: None,
                }),
            })
        }
        for generation in [17, 91] {
            let keys = [0, 1, 2].map(|rank| {
                hirsel_query_products::local::LocalSlot {
                    rank,
                    generation,
                    body: false,
                }
                .address()
            });
            let definition = DefId::intern(&DefPath::core("residency"));
            let memos = [
                crate::memo::MemoKey::new(QueryKind::Check, definition),
                crate::memo::MemoKey::new(QueryKind::Modes, definition),
            ];
            let row_bytes = size_of::<(crate::memo::MemoKey, Arc<Reads>)>() as u64;
            let wrapper = hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<
                FrontObservations,
            >::uninit());
            let mut slots = Slots::default();
            slots.insert(keys[2], archived(&memos));
            slots.insert(keys[0], archived(&[]));
            slots.insert(keys[1], archived(&memos[..1]));
            assert_eq!(slots.unbacked_bytes(), 3 * wrapper + 3 * row_bytes);
            assert_eq!(slots.archived_bytes(), 0);
            assert_eq!(slots.victim(|_, _| true), Some(keys[2]));
            // Rejection twin: a pin excludes the oldest slot.
            assert_eq!(slots.victim(|key, _| key != keys[2]), Some(keys[0]));
            assert!(slots.get(&keys[2]).is_some());
            assert_eq!(slots.victim(|_, _| true), Some(keys[0]));
            slots.insert(keys[0], archived(&memos));
            assert_eq!(slots.unbacked_bytes(), 3 * wrapper + 5 * row_bytes);
            assert_eq!(slots.victim(|_, _| true), Some(keys[1]));
            assert_eq!(slots.victim(|_, _| false), None);
            let independent: u64 = slots
                .values()
                .map(|slot| match slot {
                    FrontSlot::Archived(archived) => {
                        wrapper
                            + match &archived.observations.rows {
                                observations::Rows::Retained(rows) => rows.len() as u64 * row_bytes,
                                observations::Rows::Archived { .. } => unreachable!(),
                            }
                    }
                    FrontSlot::Live { .. } => unreachable!(),
                })
                .sum();
            assert_eq!(slots.unbacked_bytes(), independent);
        }
    }
}
