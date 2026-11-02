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

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_syntax::{ContentHash, DefId, DefPath};

    fn dependency(name: &str, fingerprint: &[u8]) -> SignatureDependency {
        SignatureDependency {
            definition: DefId::intern(&DefPath::core(name)),
            fingerprint: ContentHash::of_bytes(fingerprint),
        }
    }
    #[test]
    fn indexed_dependencies_preserve_repeated_order_versions_and_canonical_bytes() {
        let a = dependency("dependency", b"first");
        let changed = dependency("dependency", b"changed");
        let b = dependency("another_dependency_with_a_long_owned_name", b"other");
        let original = vec![
            a.clone(),
            b.clone(),
            a.clone(),
            changed.clone(),
            a.clone(),
            b.clone(),
            changed,
        ];
        let sequence = Dependencies::from(original.clone());
        assert!(matches!(sequence, Dependencies::Indexed { .. }));
        assert_eq!(sequence.to_vec(), original);
        assert!(sequence.retained_bytes() < size_of_val(original.as_slice()));
        let expected = bincode::serialize(&original).unwrap();
        let encoded = bincode::serialize(&sequence).unwrap();
        assert_eq!(encoded, expected);
        assert_eq!(
            bincode::deserialize::<Vec<SignatureDependency>>(&encoded).unwrap(),
            original
        );
    }
    #[test]
    fn packed_descriptor_keeps_order_and_rejects_invalid_indices() {
        let a = dependency("a_very_long_dependency_name", b"first");
        let b = dependency("another_long_dependency_name", b"second");
        let original: Vec<_> = (0..104)
            .map(|i| if i % 3 == 0 { b.clone() } else { a.clone() })
            .collect();
        let packed = PackedDependencies::from(original.clone());
        let bytes = bincode::serialize(&packed).unwrap();
        assert!(bytes.len() < bincode::serialized_size(&original).unwrap() as usize / 2);
        let decoded: PackedDependencies = bincode::deserialize(&bytes).unwrap();
        assert_eq!(decoded.to_vec(), original);
        assert_eq!(bincode::serialize(&decoded).unwrap(), bytes);
        for order in [vec![1u32], vec![u32::MAX]] {
            let invalid = bincode::serialize(&(vec![a.clone()], order)).unwrap();
            assert!(bincode::deserialize::<PackedDependencies>(&invalid).is_err());
        }
        for original in [vec![], vec![a, b]] {
            let bytes = bincode::serialize(&PackedDependencies::from(original.clone())).unwrap();
            assert_eq!(
                bincode::deserialize::<PackedDependencies>(&bytes)
                    .unwrap()
                    .to_vec(),
                original
            );
        }
    }
    #[test]
    fn sparse_and_empty_dependency_sequences_keep_flat_bounded_storage() {
        for len in [0, 1, 2, 31, 843] {
            for distinct in [1, 2, 192, 843] {
                let original: Vec<_> = (0..len)
                    .map(|index| {
                        dependency(&format!("dependency_{}", index % distinct), b"version")
                    })
                    .collect();
                let sequence = Dependencies::from(original.clone());
                assert_eq!(sequence.to_vec(), original);
                assert!(sequence.retained_bytes() <= size_of_val(original.as_slice()));
                assert_eq!(
                    bincode::serialize(&sequence).unwrap(),
                    bincode::serialize(&original).unwrap()
                );
                if len <= distinct {
                    assert!(matches!(sequence, Dependencies::Flat(_)));
                }
            }
        }
    }
}
