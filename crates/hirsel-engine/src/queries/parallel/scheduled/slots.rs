//! Front residency follows logical access and canonical local query rank.
use super::*;
use hirsel_query_products::local::LocalSlot;

pub(super) struct Slots {
    rows: Box<[Option<Box<FrontSlot>>]>,
    generation: u64,
    len: usize,
    recency: Mutex<Recency>,
    unbacked: u64,
    archived: u64,
}
const ABSENT: usize = usize::MAX;
#[derive(Clone, Copy)]
struct Link {
    previous: usize,
    next: usize,
}
struct Recency {
    queues: [Queue; 2],
}
const ALL: usize = 0;
const PENDING: usize = 1;
struct Queue {
    links: Box<[Link]>,
    first: usize,
    last: usize,
}
impl Queue {
    fn new(len: usize) -> Self {
        Self {
            links: vec![
                Link {
                    previous: ABSENT,
                    next: ABSENT
                };
                len
            ]
            .into_boxed_slice(),
            first: ABSENT,
            last: ABSENT,
        }
    }
    fn contains(&self, rank: usize) -> bool {
        self.first == rank || self.links[rank].previous != ABSENT || self.links[rank].next != ABSENT
    }
    // Rust LinkedList, library/alloc/src/collections/linked_list.rs:219-285
    // at a7c37f9f. Admitted rank indices replace pointers and allocations.
    fn remove(&mut self, rank: usize) {
        if !self.contains(rank) {
            return;
        }
        let Link { previous, next } = self.links[rank];
        if previous == ABSENT {
            self.first = next;
        } else {
            self.links[previous].next = next;
        }
        if next == ABSENT {
            self.last = previous;
        } else {
            self.links[next].previous = previous;
        }
        self.links[rank] = Link {
            previous: ABSENT,
            next: ABSENT,
        };
    }
    fn touch(&mut self, rank: usize) {
        if self.last == rank {
            return;
        }
        self.remove(rank);
        self.links[rank] = Link {
            previous: self.last,
            next: ABSENT,
        };
        if self.last == ABSENT {
            self.first = rank;
        } else {
            self.links[self.last].next = rank;
        }
        self.last = rank;
    }
}
impl Recency {
    fn touch(&mut self, rank: usize) {
        for queue in &mut self.queues {
            if queue.contains(rank) {
                queue.touch(rank);
            }
        }
    }
    fn replace(&mut self, rank: usize, slot: &FrontSlot) {
        let pending =
            matches!(slot, FrontSlot::Live { outcome, .. } if outcome.pending_body.is_some());
        for (queue, retain) in self.queues.iter_mut().zip([true, pending]) {
            if retain {
                queue.touch(rank);
            } else {
                queue.remove(rank);
            }
        }
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
    pub(super) fn capture_bytes(len: usize) -> Option<u64> {
        len.checked_mul(size_of::<Option<Box<FrontSlot>>>() + 2 * size_of::<Link>())
            .and_then(|bytes| u64::try_from(bytes).ok())
    }
    pub(super) fn new(len: usize, generation: u64) -> Self {
        Self {
            rows: (0..len).map(|_| None).collect(),
            generation,
            len: 0,
            recency: Mutex::new(Recency {
                queues: std::array::from_fn(|_| Queue::new(len)),
            }),
            unbacked: 0,
            archived: 0,
        }
    }
    fn rank(&self, key: &Key) -> Option<usize> {
        let slot = LocalSlot::decode(*key)?;
        (slot.generation == self.generation && !slot.body && (slot.rank as usize) < self.rows.len())
            .then_some(slot.rank as usize)
    }
    fn key(&self, rank: usize) -> Key {
        LocalSlot {
            rank: rank as u32,
            generation: self.generation,
            body: false,
        }
        .address()
    }
    pub(super) fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub(super) fn len(&self) -> usize {
        self.len
    }
    pub(super) fn contains_key(&self, key: &Key) -> bool {
        self.rank(key).is_some_and(|rank| self.rows[rank].is_some())
    }
    pub(super) fn get(&self, key: &Key) -> Option<&FrontSlot> {
        let rank = self.rank(key)?;
        let row = self.rows[rank].as_deref()?;
        self.recency
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .touch(rank);
        Some(row)
    }
    // Observation owners are immutable. Only insert replaces their metadata.
    pub(super) fn get_mut(&mut self, key: &Key) -> Option<&mut FrontSlot> {
        let rank = self.rank(key)?;
        let row = self.rows[rank].as_deref_mut()?;
        self.recency
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .touch(rank);
        Some(row)
    }
    pub(super) fn values(&self) -> impl Iterator<Item = &FrontSlot> + Clone {
        self.rows.iter().filter_map(|row| row.as_deref())
    }
    pub(super) fn values_mut(&mut self) -> impl Iterator<Item = &mut FrontSlot> {
        self.rows.iter_mut().filter_map(|row| row.as_deref_mut())
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = (Key, &FrontSlot)> {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(rank, row)| Some((self.key(rank), row.as_deref()?)))
    }
    pub(super) fn insert(&mut self, key: Key, slot: FrontSlot) {
        let rank = self.rank(&key).expect("captured Front address");
        hirsel_intern::work::count(hirsel_intern::work::Counter::FrontSlotReplacements);
        let (unbacked, archived) = slot.observation_bytes();
        let old = if let Some(row) = &mut self.rows[rank] {
            let old = row.observation_bytes();
            **row = slot;
            old
        } else {
            self.rows[rank] = Some(Box::new(slot));
            self.len += 1;
            (0, 0)
        };
        self.unbacked = self.unbacked - old.0 + unbacked;
        self.archived = self.archived - old.1 + archived;
        self.recency
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .replace(rank, self.rows[rank].as_deref().expect("inserted slot"));
    }
    pub(super) fn victim(&self, eligible: impl FnMut(Key, &FrontSlot) -> bool) -> Option<Key> {
        self.select(ALL, eligible)
    }
    pub(super) fn pending_victim(
        &self,
        eligible: impl FnMut(Key, &FrontSlot) -> bool,
    ) -> Option<Key> {
        self.select(PENDING, eligible)
    }
    fn select(
        &self,
        list: usize,
        mut eligible: impl FnMut(Key, &FrontSlot) -> bool,
    ) -> Option<Key> {
        let mut recency = self.recency.lock().unwrap_or_else(|e| e.into_inner());
        let queue = &mut recency.queues[list];
        let mut rank = queue.first;
        while rank != ABSENT {
            let next = queue.links[rank].next;
            let slot = self.rows[rank].as_deref().expect("queued Front slot");
            // Mutable payload release can clear a pending body. Remove that
            // stale membership once; external pins remain candidates because
            // their owners can disappear without a callback.
            if list == PENDING
                && !matches!(slot, FrontSlot::Live { outcome, .. } if outcome.pending_body.is_some())
            {
                queue.remove(rank);
            } else {
                hirsel_intern::work::count(hirsel_intern::work::Counter::FrontVictimCandidates);
                let key = self.key(rank);
                if eligible(key, slot) {
                    return Some(key);
                }
            }
            rank = next;
        }
        None
    }
    pub(super) fn index_bytes(
        weight: &mut hirsel_query_products::retained::MetadataWeight,
        len: usize,
    ) {
        // The capture owns the fixed rank/queue arrays. Completion admission
        // owns each separately allocated row, including archived checkpoints.
        weight.add(len * size_of::<FrontSlot>());
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
            let mut slots = Slots::new(5, generation);
            assert_eq!(
                Slots::capture_bytes(5),
                Some((5 * (size_of::<usize>() + 4 * size_of::<usize>())) as u64)
            );
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
            for rank in [0, 1, 2, 2, 0, 1] {
                assert!(slots.get(&keys[rank]).is_some());
            }
            assert_eq!(slots.victim(|_, _| true), Some(keys[2]));
            let foreign = LocalSlot {
                rank: 1,
                generation: generation + 1,
                body: false,
            }
            .address();
            let body = LocalSlot {
                rank: 1,
                generation,
                body: true,
            }
            .address();
            let missing = LocalSlot {
                rank: 4,
                generation,
                body: false,
            }
            .address();
            let outside = LocalSlot {
                rank: u32::MAX,
                generation,
                body: false,
            }
            .address();
            for key in [foreign, body, missing, outside] {
                assert!(slots.get(&key).is_none());
                assert!(!slots.contains_key(&key));
            }
            assert_eq!(slots.len(), 3);
            assert_eq!(slots.victim(|_, _| true), Some(keys[2]));
            let candidates =
                hirsel_intern::work::read(hirsel_intern::work::Counter::FrontVictimCandidates);
            for _ in 0..17 {
                assert_eq!(slots.pending_victim(|_, _| true), None);
            }
            assert_eq!(
                hirsel_intern::work::read(hirsel_intern::work::Counter::FrontVictimCandidates)
                    - candidates,
                0,
                "archived checkpoints must not be revisited while searching for an evictable payload"
            );
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
