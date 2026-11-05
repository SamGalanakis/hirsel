use super::*;
use crate::queries::operation_inputs::InputIndex;

/// The written input rows have one operation owner. Each capture binds those
/// rows to its own exact query ids without copying the payloads or their grant.
#[derive(Clone)]
pub(super) struct Inputs {
    rows: InputIndex<DefId, Input>,
    addresses: InputIndex<Key, DefId>,
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
        rows: BTreeMap<Key, Input>,
        previous: Option<&Self>,
    ) -> Result<Self, QueryError> {
        let addresses = capture_map(
            rows.iter()
                .map(|(address, input)| (*address, input.definition))
                .collect(),
            previous.map(|previous| &previous.addresses),
        )?;
        let rows = InputIndex::capture(
            rows.into_values()
                .map(|input| (input.definition, input))
                .collect(),
            previous.map(|previous| &previous.rows),
            |_| Some(0),
        )?;
        Ok(Self { rows, addresses })
    }
    pub(super) fn get(&self, key: &Key) -> Option<&Input> {
        self.rows.get(self.addresses.get(key)?)
    }
    pub(super) fn contains_key(&self, key: &Key) -> bool {
        self.get(key).is_some()
    }
    pub(super) fn values(&self) -> impl Iterator<Item = &Input> {
        self.rows.values()
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = (&Key, &Input)> {
        self.addresses.iter().map(|(key, id)| (key, &self.rows[id]))
    }
    pub(super) fn keys(&self) -> impl Iterator<Item = &Key> {
        self.addresses.iter().map(|(key, _)| key)
    }
    pub(super) fn unadmitted_bytes(&self) -> u64 {
        self.rows
            .unadmitted_bytes()
            .saturating_add(self.addresses.unadmitted_bytes())
    }
    pub(super) fn admit(
        &self,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Result<(), QueryError> {
        self.rows.admit(allowance)?;
        self.addresses.admit(allowance)
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
    pub(super) by_definition: InputIndex<DefId, Key>,
    pub(super) body_demands: InputIndex<Key, Key>,
    pub(super) cited: InputIndex<DefId, ContentHash>,
    pub(super) candidates: InputIndex<DefId, ContentHash>,
    pub(super) arm_fixes: InputIndex<DefPath, (Span, FileId, ContentHash)>,
}
impl OperationFrontInputs {
    pub(super) fn unadmitted_bytes(&self) -> u64 {
        self.inputs
            .unadmitted_bytes()
            .saturating_add(self.by_definition.unadmitted_bytes())
            .saturating_add(self.body_demands.unadmitted_bytes())
            .saturating_add(self.cited.unadmitted_bytes())
            .saturating_add(self.candidates.unadmitted_bytes())
            .saturating_add(self.arm_fixes.unadmitted_bytes())
    }
    pub(super) fn admit(
        &self,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Result<(), QueryError> {
        self.inputs.admit(allowance)?;
        self.by_definition.admit(allowance)?;
        self.body_demands.admit(allowance)?;
        self.cited.admit(allowance)?;
        self.candidates.admit(allowance)?;
        self.arm_fixes.admit(allowance)
    }
}
