//! Body-local indices with bounded copy-on-write pages. No flow operations live here.
//! Word packing follows rustc_index/src/bit_set.rs:16-30,70-94 at a7c37f9f.
//! Unlike its flat allocation, large sets use sparse persistent word rows.
use crate::rows::Rows;
use std::rc::Rc;

const SMALL: usize = 8;
#[derive(Clone, Debug)]
pub struct IndexSet {
    domain: usize,
    len: usize,
    data: Option<Rc<SetData>>,
}
#[derive(Clone, Debug)]
enum SetData {
    Small(Vec<usize>),
    Words(Rows<u64>),
}
impl IndexSet {
    pub fn new(domain: usize) -> Self {
        Self {
            domain,
            len: 0,
            data: None,
        }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn contains(&self, index: &usize) -> bool {
        assert!(*index < self.domain);
        match self.data.as_deref() {
            None => false,
            Some(SetData::Small(rows)) => rows.binary_search(index).is_ok(),
            Some(SetData::Words(words)) => words[*index / 64] & (1 << (*index % 64)) != 0,
        }
    }
    pub fn insert(&mut self, index: usize) -> bool {
        assert!(index < self.domain);
        if self.contains(&index) {
            return false;
        }
        match Rc::make_mut(
            self.data
                .get_or_insert_with(|| Rc::new(SetData::Small(Vec::new()))),
        ) {
            SetData::Small(rows) => {
                let at = rows.binary_search(&index).unwrap_err();
                if rows.len() < SMALL {
                    rows.insert(at, index);
                } else {
                    let mut words = Rows::new(0, self.domain.div_ceil(64));
                    for i in rows.iter().copied().chain([index]) {
                        words.set(i / 64, words[i / 64] | (1 << (i % 64)));
                    }
                    self.data = Some(Rc::new(SetData::Words(words)));
                }
            }
            SetData::Words(words) => {
                let word = words[index / 64];
                let next = word | (1 << (index % 64));
                words.set(index / 64, next);
            }
        }
        self.len += 1;
        true
    }
    pub fn remove(&mut self, index: &usize) -> bool {
        assert!(*index < self.domain);
        if !self.contains(index) {
            return false;
        }
        match Rc::make_mut(self.data.as_mut().unwrap()) {
            SetData::Small(rows) => {
                let at = rows.binary_search(index).unwrap();
                rows.remove(at);
            }
            SetData::Words(words) => {
                let word = words[*index / 64];
                let next = word & !(1 << (*index % 64));
                words.set(*index / 64, next);
            }
        }
        self.len -= 1;
        true
    }
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        let (small, words): (&[usize], _) = match self.data.as_deref() {
            None => (&[][..], Vec::new()),
            Some(SetData::Small(rows)) => (rows.as_slice(), Vec::new()),
            Some(SetData::Words(words)) => (&[][..], words.entries()),
        };
        small
            .iter()
            .copied()
            .chain(words.into_iter().flat_map(|(index, word)| {
                let mut word = *word;
                std::iter::from_fn(move || {
                    if word == 0 {
                        return None;
                    }
                    let bit = word.trailing_zeros() as usize;
                    word &= word - 1;
                    Some(index * 64 + bit)
                })
            }))
    }
    pub fn extend(&mut self, other: &Self) {
        assert_eq!(self.domain, other.domain);
        if other.is_empty() || self == other {
            return;
        }
        if self.is_empty() {
            *self = other.clone();
            return;
        }
        if let (SetData::Words(own), SetData::Words(incoming)) = (
            Rc::make_mut(self.data.as_mut().unwrap()),
            other.data.as_deref().unwrap(),
        ) {
            let added: usize = own
                .differences(incoming)
                .into_iter()
                .map(|i| (incoming[i] & !own[i]).count_ones() as usize)
                .sum();
            own.union(incoming, |a, b| *a |= b);
            self.len += added;
        } else {
            for index in other.iter() {
                self.insert(index);
            }
        }
    }
}
impl PartialEq for IndexSet {
    fn eq(&self, other: &Self) -> bool {
        if self.domain != other.domain || self.len != other.len {
            return false;
        }
        if self.is_empty() {
            return true;
        }
        if self
            .data
            .as_ref()
            .zip(other.data.as_ref())
            .is_some_and(|(a, b)| Rc::ptr_eq(a, b))
        {
            return true;
        }
        match (self.data.as_deref(), other.data.as_deref()) {
            (Some(SetData::Small(a)), Some(SetData::Small(b))) => a == b,
            (Some(SetData::Words(a)), Some(SetData::Words(b))) => a == b,
            _ => self.iter().eq(other.iter()),
        }
    }
}
impl Eq for IndexSet {}

/// Sparse payload rows. Snapshots and neighboring writes never clone a payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexMap<T> {
    rows: Rows<Option<Rc<T>>>,
    active: IndexSet,
}
impl<T: Clone + PartialEq> IndexMap<T> {
    pub fn new(domain: usize) -> Self {
        Self {
            rows: Rows::new(None, domain),
            active: IndexSet::new(domain),
        }
    }
    pub fn get(&self, index: usize) -> Option<&T> {
        self.rows[index].as_deref()
    }
    pub fn insert(&mut self, index: usize, value: T) -> bool {
        if self.get(index) == Some(&value) {
            return false;
        }
        self.active.insert(index);
        self.rows.set(index, Some(Rc::new(value)));
        true
    }
    pub fn remove(&mut self, index: usize) {
        if self.active.remove(&index) {
            self.rows.set(index, None);
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = (usize, &T)> {
        self.active
            .iter()
            .map(|index| (index, self.get(index).unwrap()))
    }
    /// Replace only present incoming rows; absent rows retain their old values.
    pub fn overwrite(&mut self, other: &Self) -> bool {
        let previous = self.rows.clone();
        self.rows.union(&other.rows, |a, b| {
            if b.is_some() {
                *a = b.clone();
            }
        });
        self.active.extend(&other.active);
        self.rows != previous
    }

    pub fn retain(&mut self, mut keep: impl FnMut(usize, &T) -> bool) {
        let removed: Vec<_> = self
            .iter()
            .filter_map(|(i, v)| (!keep(i, v)).then_some(i))
            .collect();
        for index in removed {
            self.remove(index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    #[test]
    fn memberships_snapshots_and_union_match_independent_sets() {
        for domain in [0, 1, 8, 9, 63, 64, 65, 513, 4097] {
            let mut a = IndexSet::new(domain);
            let mut oracle = BTreeSet::new();
            for i in (0..domain).rev().step_by(3) {
                assert_eq!(a.insert(i), oracle.insert(i));
            }
            let saved = a.clone();
            let before = oracle.clone();
            for i in (0..domain).step_by(5) {
                assert_eq!(a.remove(&i), oracle.remove(&i));
                assert_eq!(a.remove(&i), oracle.remove(&i));
            }
            assert_eq!(
                saved.iter().collect::<Vec<_>>(),
                before.iter().copied().collect::<Vec<_>>()
            );
            assert_eq!(
                a.iter().collect::<Vec<_>>(),
                oracle.iter().copied().collect::<Vec<_>>()
            );
            a.extend(&saved);
            oracle.extend(&before);
            assert_eq!(a.len(), oracle.len());
            for i in 0..domain {
                assert_eq!(a.contains(&i), oracle.contains(&i));
            }
            assert_eq!(
                a.iter().collect::<Vec<_>>(),
                oracle.iter().copied().collect::<Vec<_>>()
            );
            // Equality depends on membership, even after a paged set becomes small.
            let mut small = IndexSet::new(domain);
            for i in a.iter() {
                small.insert(i);
            }
            assert_eq!(a, small);
            let mut shrunk = a.clone();
            let first: Vec<_> = oracle.iter().take(3).copied().collect();
            for i in oracle.iter().skip(3) {
                shrunk.remove(i);
            }
            let mut sparse = IndexSet::new(domain);
            for i in first {
                sparse.insert(i);
            }
            assert_eq!(shrunk, sparse);
        }
    }
    #[test]
    fn payload_rows_copy_no_unchanged_payload_and_retain_original_indices() {
        #[derive(Debug, PartialEq)]
        struct Payload(usize);
        impl Clone for Payload {
            fn clone(&self) -> Self {
                panic!("payload clone")
            }
        }
        let mut base = IndexMap::new(4097);
        for i in (0..4097).step_by(7) {
            base.insert(i, Payload(i * 2));
        }
        let mut changed = base.clone();
        changed.insert(1, Payload(3));
        changed.remove(7);
        assert_eq!(base.get(7), Some(&Payload(14)));
        assert_eq!(base.get(1), None);
        assert_eq!(changed.get(7), None);
        assert_eq!(changed.get(1), Some(&Payload(3)));
        assert_eq!(
            base.iter().map(|(i, v)| (i, v.0)).collect::<Vec<_>>(),
            (0..4097).step_by(7).map(|i| (i, i * 2)).collect::<Vec<_>>()
        );
        changed.retain(|i, _| i % 2 == 0);
        assert!(changed.iter().all(|(i, _)| i % 2 == 0));
        for left in 0..16usize {
            for right in 0..16usize {
                let mut a = IndexMap::new(4);
                let mut b = IndexMap::new(4);
                let mut oracle = std::collections::BTreeMap::new();
                for i in 0..4 {
                    if left & (1 << i) != 0 {
                        a.insert(i, i);
                        oracle.insert(i, i);
                    }
                    if right & (1 << i) != 0 {
                        b.insert(i, 10 + i);
                    }
                }
                let before = oracle.clone();
                for i in 0..4 {
                    if right & (1 << i) != 0 {
                        oracle.insert(i, 10 + i);
                    }
                }
                assert_eq!(a.overwrite(&b), before != oracle);
                assert_eq!(
                    a.iter().map(|(i, v)| (i, *v)).collect::<Vec<_>>(),
                    oracle.into_iter().collect::<Vec<_>>()
                );
                assert!(!a.overwrite(&b));
            }
        }
    }
}
