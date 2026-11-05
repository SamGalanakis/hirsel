use super::*;
use crate::queries::operation_inputs::InputIndex;

/// The written input rows have one operation owner. Each capture binds those
/// rows to its own exact query ids without copying the payloads or their grant.
pub(super) struct Inputs {
    rows: InputIndex<DefId, Input>,
    addresses: Box<[(Key, DefId)]>,
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
        let addresses = rows
            .iter()
            .map(|(address, input)| (*address, input.definition))
            .collect();
        let rows = InputIndex::capture(
            rows.into_values()
                .map(|input| (input.definition, input))
                .collect(),
            previous.map(|previous| &previous.rows),
            |len| {
                let mut weight = crate::NativeMetadataWeight::default();
                weight.tree::<DefId, Input>(len);
                Some(weight.bytes())
            },
        )?;
        Ok(Self { rows, addresses })
    }
    pub(super) fn get(&self, key: &Key) -> Option<&Input> {
        self.rows.get(index_value(&self.addresses, key)?)
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
        self.rows.unadmitted_bytes()
    }
    pub(super) fn admit(&self, allowance: &mut MemoryReservation) -> Result<(), QueryError> {
        self.rows.admit(allowance)
    }
}
impl std::ops::Index<&Key> for Inputs {
    type Output = Input;
    fn index(&self, key: &Key) -> &Input {
        self.get(key).expect("captured Front input")
    }
}
