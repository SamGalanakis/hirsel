//! Immutable operation indexes. A successor owns only changed rows and removals;
//! its parent lease keeps the original rows and their admission alive.
use super::*;
use crate::runtime::MemoryReservation;
use std::sync::OnceLock;

pub(crate) struct InputIndex<K, V>(Arc<Rows<K, V>>);
struct Rows<K, V> {
    parent: Option<InputIndex<K, V>>,
    delta: BTreeMap<K, V>,
    removed: BTreeSet<K>,
    len: usize,
    bytes: u64,
    admission: OnceLock<MemoryReservation>,
}
impl<K, V> Clone for InputIndex<K, V> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<K: Ord + Clone, V: PartialEq> InputIndex<K, V> {
    pub(crate) fn capture(
        mut rows: BTreeMap<K, V>,
        previous: Option<&Self>,
        bytes: impl FnOnce(usize) -> Option<u64>,
    ) -> Result<Self, QueryError> {
        let len = rows.len();
        let removed = previous.map_or_else(BTreeSet::new, |previous| {
            previous
                .keys()
                .filter(|key| !rows.contains_key(*key))
                .cloned()
                .collect()
        });
        if let Some(previous) = previous {
            rows.retain(|key, value| previous.get(key) != Some(value));
            if rows.is_empty() && removed.is_empty() {
                return Ok(previous.clone());
            }
        }
        let bytes = bytes(rows.len())
            .and_then(|bytes| {
                bytes
                    .checked_add((removed.len() * (size_of::<K>() + 6 * size_of::<usize>())) as u64)
            })
            .and_then(|bytes| {
                bytes.checked_add((size_of::<Rows<K, V>>() + 2 * size_of::<usize>()) as u64)
            })
            .ok_or_else(|| header_products::source_refusal("operation index size overflow"))?;
        Ok(Self(Arc::new(Rows {
            parent: previous.cloned(),
            delta: rows,
            removed,
            len,
            bytes,
            admission: OnceLock::new(),
        })))
    }
}
impl<K: Ord, V> InputIndex<K, V> {
    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        if self.0.removed.contains(key) {
            return None;
        }
        self.0
            .delta
            .get(key)
            .or_else(|| self.0.parent.as_ref()?.get(key))
    }
    pub(crate) fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }
    pub(crate) fn len(&self) -> usize {
        self.0.len
    }
    pub(crate) fn iter(&self) -> Box<dyn Iterator<Item = (&K, &V)> + '_> {
        let mut own = self.0.delta.iter().peekable();
        let mut parent = self.0.parent.as_ref().map(|parent| {
            parent
                .iter()
                .filter(|(key, _)| !self.0.removed.contains(*key))
                .peekable()
        });
        Box::new(std::iter::from_fn(move || {
            let Some(parent) = parent.as_mut() else {
                return own.next();
            };
            match (own.peek(), parent.peek()) {
                (None, _) => parent.next(),
                (_, None) => own.next(),
                (Some((key, _)), Some((old, _))) => match key.cmp(old) {
                    std::cmp::Ordering::Less => own.next(),
                    std::cmp::Ordering::Greater => parent.next(),
                    std::cmp::Ordering::Equal => {
                        parent.next();
                        own.next()
                    }
                },
            }
        }))
    }
    pub(crate) fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(key, _)| key)
    }
    pub(crate) fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, value)| value)
    }
    pub(crate) fn unadmitted_bytes(&self) -> u64 {
        self.0
            .parent
            .as_ref()
            .map_or(0, Self::unadmitted_bytes)
            .saturating_add(if self.0.admission.get().is_some() {
                0
            } else {
                self.0.bytes
            })
    }
    pub(crate) fn admit(&self, allowance: &mut MemoryReservation) -> Result<(), QueryError> {
        if let Some(parent) = &self.0.parent {
            parent.admit(allowance)?;
        }
        if self.0.admission.get().is_none() {
            let reservation = allowance
                .split(self.0.bytes)
                .ok_or_else(|| header_products::source_refusal("operation index reservation"))?;
            if let Err(reservation) = self.0.admission.set(reservation) {
                allowance
                    .join(reservation)
                    .map_err(|_| header_products::source_refusal("operation index partition"))?;
            }
        }
        Ok(())
    }
}
impl<'a, K: Ord, V> IntoIterator for &'a InputIndex<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Box<dyn Iterator<Item = Self::Item> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<K: Ord, V> std::ops::Index<&K> for InputIndex<K, V> {
    type Output = V;
    fn index(&self, key: &K) -> &V {
        self.get(key).expect("captured operation input")
    }
}
