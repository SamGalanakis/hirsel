//! Snapshot-local sharing of exact immutable read observations.
use super::*;
use crate::runtime::MemoryReservation;
use std::{borrow::Borrow, collections::BTreeSet, sync::Arc};
type Row = (ReadKey, ContentHash);

pub(super) struct ChargedRow {
    pub(super) value: Row,
    _allowance: MemoryReservation,
}
impl std::fmt::Debug for ChargedRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.value.fmt(f)
    }
}

// BTreeSet borrows the immutable observation from its Arc instead of retaining
// a second copy of the wide key in every dictionary node.
#[derive(Clone)]
struct RowRef(Arc<ChargedRow>);
impl Borrow<Row> for RowRef {
    fn borrow(&self) -> &Row {
        &self.0.value
    }
}
impl PartialEq for RowRef {
    fn eq(&self, other: &Self) -> bool {
        self.0.value == other.0.value
    }
}
impl Eq for RowRef {}
impl PartialOrd for RowRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RowRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.value.cmp(&other.0.value)
    }
}

#[derive(Default)]
pub(crate) struct ReadPool {
    rows: BTreeSet<RowRef>,
    charged_bytes: u64,
}
impl ReadPool {
    pub(crate) fn charged_bytes(&self) -> u64 {
        self.charged_bytes
    }
    pub(crate) fn required_bytes(&self, reads: &Reads) -> Option<u64> {
        if reads.is_empty() {
            return Some(0);
        }
        self.admission(reads).map(|(_, charge, _, _)| charge)
    }
    fn admission(&self, reads: &Reads) -> Option<(u64, u64, u64, u64)> {
        let missing = reads.iter().filter(|row| !self.rows.contains(*row)).count();
        let row_bytes = std::alloc::Layout::new::<[usize; 2]>()
            .extend(std::alloc::Layout::new::<ChargedRow>())
            .ok()?
            .0
            .pad_to_align()
            .size() as u64;
        let tree_bytes = |len| {
            let mut weight = crate::NativeMetadataWeight::default();
            if len != 0 {
                weight.tree::<RowRef, ()>(len);
            }
            weight.bytes()
        };
        let shared_bytes = std::alloc::Layout::new::<[usize; 2]>()
            .extend(std::alloc::Layout::new::<SharedRows>())
            .ok()?
            .0
            .pad_to_align()
            .size()
            .checked_add(reads.len().checked_mul(size_of::<Arc<ChargedRow>>())?)?
            as u64;
        let loaned = (missing as u64)
            .checked_mul(row_bytes)?
            .checked_add(shared_bytes)?;
        let charge = loaned.checked_add(
            tree_bytes(self.rows.len().checked_add(missing)?)
                .checked_sub(tree_bytes(self.rows.len()))?,
        )?;
        Some((loaned, charge, row_bytes, shared_bytes))
    }
    pub(crate) fn required_tracked_bytes(&self, reads: &Reads) -> Option<u64> {
        self.admission(reads)?
            .1
            .checked_add(crate::memo::retained_arc_bytes(reads))
    }
    pub(crate) fn intern_tracked(
        &mut self,
        reads: &Reads,
        allowance: &mut MemoryReservation,
        remaining: &mut u64,
        credit: Arc<std::sync::Mutex<u64>>,
    ) -> Option<Reads> {
        self.intern_inner(reads, allowance, remaining, Some(credit))
    }
    pub(crate) fn intern(
        &mut self,
        reads: &Reads,
        allowance: &mut MemoryReservation,
        remaining: &mut u64,
    ) -> Option<Reads> {
        self.intern_inner(reads, allowance, remaining, None)
    }
    fn intern_inner(
        &mut self,
        reads: &Reads,
        allowance: &mut MemoryReservation,
        remaining: &mut u64,
        credit: Option<Arc<std::sync::Mutex<u64>>>,
    ) -> Option<Reads> {
        if reads.is_empty() && credit.is_none() {
            return Some(Reads::empty());
        }
        let (loaned, charge, row_bytes, shared_bytes) = self.admission(reads)?;
        let wrapper = if credit.is_some() {
            crate::memo::retained_arc_bytes(reads)
        } else {
            0
        };
        let loaned = loaned.checked_add(wrapper)?;
        let charge = charge.checked_add(wrapper)?;
        let shared_bytes = shared_bytes.checked_add(wrapper)?;
        let after = remaining.checked_sub(charge)?;
        if allowance.bytes() < loaned {
            return None;
        }
        let mut rows = Vec::with_capacity(reads.len());
        for row in reads.iter() {
            let value = if let Some(value) = self.rows.get(row) {
                value.0.clone()
            } else {
                let value = Arc::new(ChargedRow {
                    value: *row,
                    _allowance: allowance.split(row_bytes).expect("pre-admitted read row"),
                });
                self.rows.insert(RowRef(value.clone()));
                value
            };
            rows.push(value);
        }
        let shared = Arc::new(SharedRows {
            values: rows.into_boxed_slice(),
            _credit: credit.map(|remaining| ReadCredit {
                remaining,
                bytes: shared_bytes,
                owner: std::sync::OnceLock::new(),
            }),
            _allowance: allowance
                .split(shared_bytes)
                .expect("pre-admitted read array"),
        });
        self.charged_bytes += loaned;
        *remaining = after;
        Some(Reads {
            rows: Rows::Shared(shared),
            indices: None,
        })
    }
}

struct ReadCredit {
    remaining: Arc<std::sync::Mutex<u64>>,
    bytes: u64,
    owner: std::sync::OnceLock<std::sync::Weak<Reads>>,
}
impl Drop for ReadCredit {
    fn drop(&mut self) {
        *self.remaining.lock().unwrap_or_else(|e| e.into_inner()) += self.bytes;
    }
}
pub(super) struct SharedRows {
    values: Box<[Arc<ChargedRow>]>,
    _allowance: MemoryReservation,
    _credit: Option<ReadCredit>,
}

impl Reads {
    pub(crate) fn bind_tracked_owner(this: &Arc<Self>) {
        if let Rows::Shared(rows) = &this.rows
            && let Some(credit) = &rows._credit
        {
            let _ = credit.owner.set(Arc::downgrade(this));
        }
    }
    pub(crate) fn tracked_by(this: &Arc<Self>, remaining: &Arc<std::sync::Mutex<u64>>) -> bool {
        this.indices.is_none()
            && matches!(&this.rows, Rows::Shared(rows) if rows._credit.as_ref().is_some_and(|credit| {
                Arc::ptr_eq(&credit.remaining, remaining) && credit.owner.get().is_some_and(|owner| std::ptr::eq(owner.as_ptr(), Arc::as_ptr(this)))
            }))
    }
}
impl std::fmt::Debug for SharedRows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.values.fmt(f)
    }
}

#[derive(Clone, Debug)]
pub(super) enum Rows {
    Flat(Arc<[Row]>),
    Shared(Arc<SharedRows>),
}
impl Default for Rows {
    fn default() -> Self {
        Self::Flat(Arc::default())
    }
}
impl From<Vec<Row>> for Rows {
    fn from(rows: Vec<Row>) -> Self {
        Self::Flat(rows.into())
    }
}
impl Rows {
    pub(super) fn shared(&self) -> bool {
        matches!(self, Self::Shared(_))
    }
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Flat(rows) => rows.len(),
            Self::Shared(rows) => rows.values.len(),
        }
    }
    pub(super) fn as_ptr(&self) -> *const () {
        match self {
            Self::Flat(rows) => rows.as_ptr().cast(),
            Self::Shared(rows) => Arc::as_ptr(rows).cast(),
        }
    }
    pub(super) fn allocation_bytes(&self) -> usize {
        // Shared observations and each receipt's array carry their own
        // original source reservations, including after their source drops.
        match self {
            Self::Flat(rows) => super::super::retained::retained_arc_bytes(&**rows) as usize,
            Self::Shared(_) => 0,
        }
    }
    pub(super) fn binary_search_by(
        &self,
        mut compare: impl FnMut(&Row) -> std::cmp::Ordering,
    ) -> Result<usize, usize> {
        match self {
            Self::Flat(rows) => rows.binary_search_by(compare),
            Self::Shared(rows) => rows.values.binary_search_by(|row| compare(&row.value)),
        }
    }
}
impl std::ops::Index<usize> for Rows {
    type Output = Row;
    fn index(&self, at: usize) -> &Row {
        match self {
            Self::Flat(rows) => &rows[at],
            Self::Shared(rows) => &rows.values[at].value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ProcessBudget;

    #[test]
    fn pooled_observations_keep_exact_hashes_and_charge_physical_storage_until_last_owner() {
        let originals = (0..8)
            .map(|set| {
                Reads::from_vec(
                    (0..128)
                        .map(|index| {
                            (
                                ReadKey::Signature(DefId::intern(&DefPath::core(&format!(
                                    "pool_{index}"
                                )))),
                                ContentHash::of_bytes(if index == set {
                                    b"changed"
                                } else {
                                    b"observed"
                                }),
                            )
                        })
                        .collect(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let budget = ProcessBudget::new(1 << 20);
        let mut allowance = budget.reserve(1 << 20).unwrap();
        let mut remaining = allowance.bytes();
        let ((pool, pooled), measured) = crate::test_alloc::measured(|| {
            let mut pool = ReadPool::default();
            let pooled = originals
                .iter()
                .map(|reads| pool.intern(reads, &mut allowance, &mut remaining).unwrap())
                .collect::<Vec<_>>();
            (pool, pooled)
        });
        let retained = (1 << 20) - remaining
            + pooled
                .iter()
                .flat_map(Reads::allocations)
                .map(|(_, bytes)| bytes as u64)
                .sum::<u64>()
            + (pooled.capacity() * size_of::<Reads>()) as u64;
        assert!(measured.live > 0);
        assert!(
            retained >= measured.live as u64,
            "charged {retained}, actual {}",
            measured.live
        );
        let flat = originals
            .iter()
            .flat_map(Reads::allocations)
            .map(|(_, bytes)| bytes as u64)
            .sum::<u64>();
        assert!(
            retained < flat,
            "shared storage {retained}, flat storage {flat}"
        );
        for (original, pooled) in originals.iter().zip(&pooled) {
            assert_eq!(original, pooled);
            assert_eq!(original.digest(), pooled.digest());
            let mut expected = vec![];
            original.encode(&mut expected);
            let mut actual = vec![];
            pooled.encode(&mut actual);
            assert_eq!(expected, actual);
            assert_eq!(
                Reads::decode(&mut actual.as_slice()),
                Some(original.clone())
            );
            for (key, value) in original.iter() {
                assert_eq!(pooled.get(key), Some(*value));
            }
        }
        assert_eq!(pool.rows.len(), 136);
        let held = pooled[0].clone();
        drop(pool);
        drop(pooled);
        drop(allowance);
        assert!(
            budget.reserved() > 0,
            "escaped rows must retain their original admission"
        );
        assert_eq!(held, originals[0]);
        drop(held);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn independently_held_receipt_arrays_keep_their_original_admission() {
        let reads = Reads::from_vec(
            (0..128)
                .map(|index| {
                    (
                        ReadKey::Signature(DefId::intern(&DefPath::core(&format!(
                            "held_pool_{index}"
                        )))),
                        ContentHash::of_bytes(b"observed"),
                    )
                })
                .collect(),
        )
        .unwrap();
        let budget = ProcessBudget::new(1 << 20);
        let mut held = Vec::with_capacity(32);
        let (_, actual) = crate::test_alloc::measured(|| {
            let mut allowance = budget.reserve(1 << 20).unwrap();
            let mut remaining = allowance.bytes();
            let mut pool = ReadPool::default();
            for _ in 0..32 {
                held.push(pool.intern(&reads, &mut allowance, &mut remaining).unwrap());
            }
            drop(pool);
            drop(allowance);
        });
        assert!(actual.live > 0);
        assert!(
            budget.reserved() >= actual.live as u64,
            "reserved={}, live={}",
            budget.reserved(),
            actual.live
        );
        for receipt in &held {
            assert_eq!(receipt, &reads);
        }
        drop(held);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn pooled_refusal_does_not_allocate_or_replace_a_previous_fingerprint() {
        let budget = ProcessBudget::new(4096);
        let mut allowance = budget.reserve(4096).unwrap();
        let key = ReadKey::Environment;
        let first = Reads::from_vec(vec![(key, ContentHash::of_bytes(b"first"))]).unwrap();
        let second = Reads::from_vec(vec![(key, ContentHash::of_bytes(b"second"))]).unwrap();
        let mut pool = ReadPool::default();
        let mut remaining = 4096;
        let accepted = pool.intern(&first, &mut allowance, &mut remaining).unwrap();
        let reserved = allowance.bytes();
        let mut exhausted = 0;
        let (refused, allocations) =
            crate::test_alloc::measured(|| pool.intern(&second, &mut allowance, &mut exhausted));
        assert!(refused.is_none());
        assert_eq!(allocations.allocations, 0);
        assert_eq!(allowance.bytes(), reserved);
        assert_eq!(pool.rows.len(), 1);
        assert_eq!(accepted, first);
        let changed = pool
            .intern(&second, &mut allowance, &mut remaining)
            .unwrap();
        assert_eq!(pool.rows.len(), 2);
        assert_eq!(changed, second);
        assert_ne!(accepted.get(&key), changed.get(&key));
    }
    #[test]
    fn tracked_receipts_refuse_n_minus_one_and_refund_only_the_last_array_owner() {
        let reads = Reads::from_vec(vec![(
            ReadKey::Environment,
            ContentHash::of_bytes(b"tracked"),
        )])
        .unwrap();
        let budget = ProcessBudget::new(1 << 20);
        let mut allowance = budget.reserve(1 << 20).unwrap();
        let mut pool = ReadPool::default();
        let required = pool.required_tracked_bytes(&reads).unwrap();
        let remaining = Arc::new(std::sync::Mutex::new(required - 1));
        let before = allowance.bytes();
        assert!(
            pool.intern_tracked(
                &reads,
                &mut allowance,
                &mut remaining.lock().unwrap(),
                remaining.clone()
            )
            .is_none()
        );
        assert_eq!(*remaining.lock().unwrap(), required - 1);
        assert_eq!(allowance.bytes(), before);
        assert!(pool.rows.is_empty());
        *remaining.lock().unwrap() = before;
        let receipt = Arc::new(
            pool.intern_tracked(
                &reads,
                &mut allowance,
                &mut remaining.lock().unwrap(),
                remaining.clone(),
            )
            .unwrap(),
        );
        Reads::bind_tracked_owner(&receipt);
        assert!(Reads::tracked_by(&receipt, &remaining));
        assert!(Reads::tracked_by(&receipt.clone(), &remaining));
        assert!(!Reads::tracked_by(
            &receipt,
            &Arc::new(std::sync::Mutex::new(0))
        ));
        let copied_wrapper = Arc::new((*receipt).clone());
        assert!(!Reads::tracked_by(&copied_wrapper, &remaining));
        assert_eq!(*copied_wrapper, reads);
        drop(copied_wrapper);
        let after = *remaining.lock().unwrap();
        assert_eq!(before - after, required);
        let array_bytes = pool.required_tracked_bytes(&reads).unwrap();
        let held = receipt.as_ref().clone();
        drop(receipt);
        assert_eq!(*remaining.lock().unwrap(), after);
        assert_eq!(held, reads);
        drop(held);
        assert_eq!(*remaining.lock().unwrap(), after + array_bytes);
        drop(pool);
        drop(allowance);
        assert_eq!(budget.reserved(), 0);
    }
}
