//! Lossless dictionary storage for an ordered signature dependency sequence.
//! BTreeMap assigns first-occurrence slots; serde's SerializeSeq emits the
//! original Vec wire format. Lower descriptors encode the dictionary itself.
use crate::SignatureDependency;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeSeq};
use std::{collections::BTreeMap, mem::size_of_val};

#[derive(Clone)]
pub(super) enum Dependencies {
    Flat(Box<[SignatureDependency]>),
    Indexed {
        values: Box<[SignatureDependency]>,
        order: Box<[u32]>,
    },
}
impl From<Vec<SignatureDependency>> for Dependencies {
    fn from(dependencies: Vec<SignatureDependency>) -> Self {
        let mut slots = BTreeMap::new();
        for dependency in &dependencies {
            let Ok(next) = u32::try_from(slots.len()) else {
                return Self::Flat(dependencies.into_boxed_slice());
            };
            slots
                .entry((dependency.definition, dependency.fingerprint))
                .or_insert(next);
        }
        let indexed_bytes = slots
            .len()
            .checked_mul(size_of::<SignatureDependency>())
            .and_then(|bytes| bytes.checked_add(dependencies.len().checked_mul(size_of::<u32>())?));
        if !indexed_bytes.is_some_and(|bytes| bytes < size_of_val(dependencies.as_slice())) {
            return Self::Flat(dependencies.into_boxed_slice());
        }
        let mut values = Vec::with_capacity(slots.len());
        let mut order = Vec::with_capacity(dependencies.len());
        for dependency in dependencies {
            let index = slots[&(dependency.definition, dependency.fingerprint)];
            // Slots follow first occurrence, so new values arrive in order.
            if index as usize == values.len() {
                values.push(dependency);
            }
            order.push(index);
        }
        Self::Indexed {
            values: values.into_boxed_slice(),
            order: order.into_boxed_slice(),
        }
    }
}
/// The descriptor stores each identity once and keeps repetitions as indices.
/// Its schema is separate from the canonical expanded QueryOutput sequence.
#[derive(Clone)]
pub(super) struct PackedDependencies(Dependencies);
impl PackedDependencies {
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }
    pub(super) fn into_inner(self) -> Dependencies {
        self.0
    }
    pub(super) fn to_vec(&self) -> Vec<SignatureDependency> {
        self.0.to_vec()
    }
}
impl From<Vec<SignatureDependency>> for PackedDependencies {
    fn from(values: Vec<SignatureDependency>) -> Self {
        Self(values.into())
    }
}
impl Serialize for PackedDependencies {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.packed().serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for PackedDependencies {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (values, order): (Box<[SignatureDependency]>, Box<[u32]>) =
            Deserialize::deserialize(deserializer)?;
        let dependencies = if order.is_empty() {
            Dependencies::Flat(values)
        } else {
            if order.iter().any(|index| *index as usize >= values.len()) {
                return Err(de::Error::custom("Lower dependency index is out of range"));
            }
            Dependencies::Indexed { values, order }
        };
        Ok(Self(dependencies))
    }
}
impl Dependencies {
    pub(super) fn packed(&self) -> (&[SignatureDependency], &[u32]) {
        match self {
            Self::Flat(values) => (values, &[]),
            Self::Indexed { values, order } => (values, order),
        }
    }
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Flat(values) => values.len(),
            Self::Indexed { order, .. } => order.len(),
        }
    }
    pub(super) fn retained_bytes(&self) -> usize {
        match self {
            Self::Flat(values) => size_of_val(&**values),
            Self::Indexed { values, order } => size_of_val(&**values) + size_of_val(&**order),
        }
    }
    fn get(&self, index: usize) -> &SignatureDependency {
        match self {
            Self::Flat(values) => &values[index],
            Self::Indexed { values, order } => &values[order[index] as usize],
        }
    }
    pub(super) fn iter(&self) -> impl ExactSizeIterator<Item = &SignatureDependency> {
        (0..self.len()).map(|index| self.get(index))
    }
    pub(super) fn to_vec(&self) -> Vec<SignatureDependency> {
        self.iter().cloned().collect()
    }
}
impl Serialize for Dependencies {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.len()))?;
        for dependency in self.iter() {
            sequence.serialize_element(dependency)?;
        }
        sequence.end()
    }
}

impl hirsel_product_codec::decode_schema::Walk for PackedDependencies {
    fn walk(scan: &mut hirsel_product_codec::decode_schema::Scan<'_>) -> Option<()> {
        <(Box<[SignatureDependency]>, Box<[u32]>)>::walk(scan)
    }
}
