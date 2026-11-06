use super::*;
use crate::queries::operation_inputs::InputIndex;

/// The written input rows have one operation owner. Each capture binds those
/// rows to its own exact query ids without copying the payloads or their grant.
#[derive(Clone)]
pub(super) struct Inputs {
    rows: InputIndex<DefId, Input>,
    namespace: Arc<NamespaceCapture>,
    addresses: Arc<Addresses>,
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
        namespace: Arc<NamespaceCapture>,
        previous: Option<&Self>,
    ) -> Result<Self, QueryError> {
        let rows = InputIndex::capture(rows, previous.map(|previous| &previous.rows), |_| Some(0))?;
        Ok(Self {
            rows,
            namespace,
            addresses: Arc::new(Addresses::default()),
        })
    }
    fn key(&self, input: &Input, body: bool) -> Key {
        let key = self
            .namespace
            .query_id(FRONT_BODY_KIND, input.key.0)
            .address
            .0;
        if body { Self::body_key(key) } else { key }
    }
    fn body_key(key: Key) -> Key {
        ContentHash::of_parts("hirsel.front.body-demand.v1", [key.0.as_slice()])
    }
    pub(super) fn address(&self, id: DefId, body: bool) -> Option<Key> {
        let key = self.key(self.rows.get(&id)?, body);
        self.addresses.remember(key, id, body);
        Some(key)
    }
    fn lookup(&self, key: &Key, body: bool) -> Option<&Input> {
        {
            let index = self
                .addresses
                .rows
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let rows = if body { &index.body } else { &index.front };
            let prefix = Addresses::prefix(*key);
            let start = rows.partition_point(|(candidate, _)| *candidate < prefix);
            for (_, id) in rows[start..]
                .iter()
                .take_while(|(candidate, _)| *candidate == prefix)
            {
                let input = self.rows.get(id)?;
                if self.key(input, body) == *key {
                    return Some(input);
                }
            }
        }
        let input = self
            .rows
            .values()
            .find(|input| self.key(input, body) == *key)?;
        self.addresses.remember(*key, input.definition, body);
        Some(input)
    }
    pub(super) fn demand(&self, key: &Key) -> Option<Key> {
        Some(self.key(self.lookup(key, true)?, false))
    }
    pub(super) fn get(&self, key: &Key) -> Option<&Input> {
        self.lookup(key, false)
    }
    pub(super) fn definition(&self, id: DefId) -> Option<&Input> {
        self.rows.get(&id)
    }
    pub(super) fn contains_key(&self, key: &Key) -> bool {
        self.get(key).is_some()
    }
    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }
    pub(super) fn clear_addresses(&self) {
        self.addresses.clear();
    }
    pub(super) fn values(&self) -> impl Iterator<Item = &Input> {
        self.rows.values()
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = (Key, &Input)> {
        self.rows
            .values()
            .map(|input| (self.key(input, false), input))
    }
    pub(super) fn unadmitted_bytes(&self) -> u64 {
        self.rows
            .unadmitted_bytes()
            .saturating_add(self.addresses.unadmitted_bytes())
    }
    pub(super) fn admit(
        &self,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
        pool: Arc<hirsel_engine_admission::MemoryReservationPool>,
        retention: Arc<Mutex<u64>>,
    ) -> Result<(), QueryError> {
        self.rows.admit(allowance)?;
        if self.addresses.admission.get().is_none() {
            let metadata = allowance
                .split(self.addresses.unadmitted_bytes())
                .ok_or_else(|| {
                    header_products::source_refusal("Front address metadata admission")
                })?;
            let _ = self.addresses.admission.set(AddressAdmission {
                _metadata: metadata,
                pool,
                retention,
            });
        }
        Ok(())
    }
}

/// Reverse addresses are hints for demanded definitions. Every prefix hit is
/// checked against the consuming namespace's complete address. If growth cannot
/// fit, the immutable rows still provide an exact lookup without an index.
#[derive(Default)]
struct Addresses {
    rows: Mutex<AddressRows>,
    admission: OnceLock<AddressAdmission>,
}
#[derive(Default)]
struct AddressRows {
    front: Vec<(u32, DefId)>,
    body: Vec<(u32, DefId)>,
    charge: Option<hirsel_engine_admission::MemoryReservation>,
}
struct AddressAdmission {
    _metadata: hirsel_engine_admission::MemoryReservation,
    pool: Arc<hirsel_engine_admission::MemoryReservationPool>,
    retention: Arc<Mutex<u64>>,
}
impl Addresses {
    fn prefix(key: Key) -> u32 {
        u32::from_le_bytes(key.0[..4].try_into().unwrap())
    }
    fn unadmitted_bytes(&self) -> u64 {
        if self.admission.get().is_some() {
            0
        } else {
            hirsel_engine_admission::retained_arc_bytes(self)
        }
    }
    fn remember(&self, key: Key, id: DefId, body: bool) {
        let Some(admission) = self.admission.get() else {
            return;
        };
        let mut index = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let AddressRows {
            front,
            body: bodies,
            charge,
        } = &mut *index;
        let rows = if body { bodies } else { front };
        let row = (Self::prefix(key), id);
        let Err(at) = rows.binary_search(&row) else {
            return;
        };
        if rows.len() == rows.capacity() {
            let growth = rows.capacity().max(4);
            let Some(bytes) = growth
                .checked_mul(size_of::<(u32, DefId)>())
                .and_then(|bytes| {
                    bytes.checked_add(if rows.capacity() == 0 {
                        2 * size_of::<usize>()
                    } else {
                        0
                    })
                })
                .and_then(|bytes| u64::try_from(bytes).ok())
            else {
                return;
            };
            let mut remaining = admission
                .retention
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let Some(next) = remaining.checked_sub(bytes) else {
                return;
            };
            let Some(extra) = admission.pool.take(bytes) else {
                return;
            };
            if let Some(charge) = charge {
                if charge.join(extra).is_err() {
                    return;
                }
            } else {
                *charge = Some(extra);
            }
            *remaining = next;
            rows.reserve_exact(growth);
        }
        rows.insert(at, row);
    }
    fn clear(&self) {
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        rows.front = Vec::new();
        rows.body = Vec::new();
        if let Some(charge) = rows.charge.take() {
            if let Some(admission) = self.admission.get() {
                *admission
                    .retention
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) += charge.bytes();
            }
        }
    }
}
impl Drop for Addresses {
    fn drop(&mut self) {
        if let Some(admission) = self.admission.get()
            && let Some(charge) = self
                .rows
                .get_mut()
                .unwrap_or_else(|e| e.into_inner())
                .charge
                .take()
        {
            *admission
                .retention
                .lock()
                .unwrap_or_else(|e| e.into_inner()) += charge.bytes();
        }
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
        pool: Arc<hirsel_engine_admission::MemoryReservationPool>,
        retention: Arc<Mutex<u64>>,
    ) -> Result<(), QueryError> {
        self.inputs.admit(allowance, pool, retention)?;
        self.cited.admit(allowance)?;
        self.candidates.admit(allowance)?;
        self.arm_fixes.admit(allowance)
    }
}
