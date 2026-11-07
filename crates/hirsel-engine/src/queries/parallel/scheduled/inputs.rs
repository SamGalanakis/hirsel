use super::*;
use crate::queries::operation_inputs::InputIndex;
use hirsel_query_products::local::LocalSlot;

#[derive(Clone)]
pub(super) struct Inputs {
    rows: InputIndex<u32, Input>,
    index: Arc<Definitions>,
    generation: u64,
}
struct Definitions {
    ranks: hirsel_syntax::DefMap<u32>,
    bytes: u64,
    admission: OnceLock<hirsel_engine_admission::MemoryReservation>,
}
impl PartialEq for Input {
    fn eq(&self, other: &Self) -> bool {
        self.definition == other.definition
            && self.text == other.text
            && self.implementation == other.implementation
            && self.key == other.key
            && self.weight == other.weight
            && match (&self.registration, &other.registration) {
                (None, None) => true,
                (Some(previous), Some(current)) => Arc::ptr_eq(previous, current),
                _ => false,
            }
    }
}
impl Inputs {
    pub(super) fn new(
        rows: BTreeMap<DefId, Input>,
        previous: Option<&Self>,
    ) -> Result<Self, QueryError> {
        let generation = LocalSlot::generation()
            .ok_or_else(|| header_products::source_refusal("Front input generation overflow"))?;
        let mut ranks = hirsel_syntax::DefMap::default();
        let mut ranked = BTreeMap::new();
        for (rank, (definition, input)) in rows.into_iter().enumerate() {
            let rank = u32::try_from(rank)
                .map_err(|_| header_products::source_refusal("Front input rank overflow"))?;
            ranks.insert(definition, rank);
            ranked.insert(rank, input);
        }
        ranks.shrink_to_fit();
        // hashbrown raw/mod.rs capacity_to_buckets and calculate_layout_for,
        // as in retained::MetadataWeight::instance_bodies. This map never grows.
        let buckets = if ranks.is_empty() {
            0
        } else {
            ranks
                .len()
                .saturating_mul(8)
                .div_ceil(7)
                .checked_next_power_of_two()
                .ok_or_else(|| header_products::source_refusal("Front rank index overflow"))?
                .max(4)
        };
        let bytes = (buckets * (size_of::<(DefId, u32)>() + 1)
            + usize::from(buckets != 0) * (16 + align_of::<(DefId, u32)>() - 1))
            as u64
            + hirsel_engine_admission::retained_arc_bytes(
                &std::mem::MaybeUninit::<Definitions>::uninit(),
            );
        let rows =
            InputIndex::capture(ranked, previous.map(|previous| &previous.rows), |_| Some(0))?;
        let index =
            if let Some(previous) = previous.filter(|previous| previous.index.ranks == ranks) {
                previous.index.clone()
            } else {
                Arc::new(Definitions {
                    ranks,
                    bytes,
                    admission: OnceLock::new(),
                })
            };
        Ok(Self {
            rows,
            index,
            generation,
        })
    }
    fn key(&self, rank: u32, body: bool) -> Key {
        LocalSlot {
            rank,
            generation: self.generation,
            body,
        }
        .address()
    }
    pub(super) fn address(&self, id: DefId, body: bool) -> Option<Key> {
        Some(self.key(*self.index.ranks.get(&id)?, body))
    }
    fn lookup(&self, key: &Key, body: bool) -> Option<&Input> {
        hirsel_intern::work::count(hirsel_intern::work::Counter::FrontLocalLookups);
        let slot = LocalSlot::decode(*key)?;
        (slot.generation == self.generation && slot.body == body).then_some(())?;
        self.rows.get(&slot.rank)
    }
    pub(super) fn demand(&self, key: &Key) -> Option<Key> {
        let slot = LocalSlot::decode(*key)?;
        self.lookup(key, true)?;
        Some(self.key(slot.rank, false))
    }
    pub(super) fn get(&self, key: &Key) -> Option<&Input> {
        self.lookup(key, false)
    }
    pub(super) fn definition(&self, id: DefId) -> Option<&Input> {
        self.rows.get(self.index.ranks.get(&id)?)
    }
    pub(super) fn contains_key(&self, key: &Key) -> bool {
        self.get(key).is_some()
    }
    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }
    pub(super) fn key_for(&self, id: DefId) -> Option<Key> {
        self.address(id, false)
    }
    pub(super) fn values(&self) -> impl Iterator<Item = &Input> {
        self.rows.values()
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = (Key, &Input)> {
        self.rows
            .iter()
            .map(|(rank, input)| (self.key(*rank, false), input))
    }
    pub(super) fn unadmitted_bytes(&self) -> u64 {
        self.rows
            .unadmitted_bytes()
            .saturating_add(if self.index.admission.get().is_some() {
                0
            } else {
                self.index.bytes
            })
    }
    pub(super) fn admit(
        &self,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Result<(), QueryError> {
        self.rows.admit(allowance)?;
        if self.index.admission.get().is_none() {
            let reservation = allowance
                .split(self.index.bytes)
                .ok_or_else(|| header_products::source_refusal("Front rank index admission"))?;
            if let Err(reservation) = self.index.admission.set(reservation) {
                allowance
                    .join(reservation)
                    .map_err(|_| header_products::source_refusal("Front rank index partition"))?;
            }
        }
        Ok(())
    }
}
impl std::ops::Index<&Key> for Inputs {
    type Output = Input;
    fn index(&self, key: &Key) -> &Input {
        self.get(key).expect("captured Front input")
    }
}

pub(super) fn capture_map<K: Ord + Clone, V: PartialEq>(
    rows: BTreeMap<K, V>,
    previous: Option<&InputIndex<K, V>>,
) -> Result<InputIndex<K, V>, QueryError> {
    InputIndex::capture(rows, previous, |_| Some(0))
}
#[derive(Clone)]
pub(in crate::queries) struct OperationFrontInputs {
    pub(super) inputs: Inputs,
    pub(super) cited: InputIndex<DefId, ContentHash>,
    pub(super) candidates: InputIndex<DefId, ContentHash>,
    pub(super) arm_fixes: InputIndex<DefPath, (Span, FileId, ContentHash)>,
}
impl OperationFrontInputs {
    pub(super) fn unadmitted_bytes(&self) -> u64 {
        self.inputs
            .unadmitted_bytes()
            .saturating_add(self.cited.unadmitted_bytes())
            .saturating_add(self.candidates.unadmitted_bytes())
            .saturating_add(self.arm_fixes.unadmitted_bytes())
    }
    pub(super) fn admit(
        &self,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Result<(), QueryError> {
        self.inputs.admit(allowance)?;
        self.cited.admit(allowance)?;
        self.candidates.admit(allowance)?;
        self.arm_fixes.admit(allowance)
    }
}
