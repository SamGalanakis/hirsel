//! Snapshot-local sharing of exact immutable read observations.
use super::*;

use std::{
    borrow::Borrow,
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicPtr, Ordering},
    },
};
type Row = (ReadKey, ContentHash);

// Dictionary order is private. Compare session identities without resolving
// paths; the receipt arrays below retain canonical and observation order.
#[derive(Debug, Eq, PartialEq)]
struct PoolRow(Row);
impl Ord for PoolRow {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let (key, value) = &self.0;
        let (other_key, other_value) = &other.0;
        key.kind()
            .cmp(&other_key.kind())
            .then_with(|| match (key.definition(), other_key.definition()) {
                (Some(a), Some(b)) => a.session_cmp(b),
                _ => key.cmp(other_key),
            })
            .then_with(|| value.cmp(other_value))
    }
}
impl PartialOrd for PoolRow {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

pub struct ChargedRow {
    value: PoolRow,
    _allowance: hirsel_engine_admission::MemoryReservation,
    // Null is idle, self is a queued tail, otherwise one owned Arc::into_raw.
    next: AtomicPtr<ChargedRow>,
}
// Rust a7c37f9f library/alloc/src/rcs/arc.rs:1117-1137 drains shared lists
// iteratively. Rows also belong to the dictionary; their links transfer one
// Arc through into_raw/from_raw (the same file:1668-1733).
struct Released {
    head: Mutex<Option<Arc<ChargedRow>>>,
    _allowance: hirsel_engine_admission::MemoryReservation,
}
impl Drop for Released {
    fn drop(&mut self) {
        let mut next = self
            .head
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        while let Some(row) = next {
            next = row.take_next();
        }
    }
}
impl ChargedRow {
    fn take_next(&self) -> Option<Arc<Self>> {
        let next = self.next.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if next.is_null() || std::ptr::eq(next, self) {
            None
        } else {
            // SAFETY: enqueue stores one Arc::into_raw reference in a unique
            // queued node. Only its owner pops it, and this swap consumes the
            // link exactly once. Null/self never represent an owned reference.
            Some(unsafe { Arc::from_raw(next) })
        }
    }
}
impl std::fmt::Debug for ChargedRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.value.0.fmt(f)
    }
}

// BTreeSet borrows the immutable observation from its Arc instead of retaining
// a second copy of the wide key in every dictionary node.
#[derive(Clone)]
struct RowRef(Arc<ChargedRow>);
impl Borrow<PoolRow> for RowRef {
    fn borrow(&self) -> &PoolRow {
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
pub struct ReadPool {
    rows: BTreeSet<RowRef>,
    charged_bytes: u64,
    released: Option<Arc<Released>>,
}
/// An exclusive pool borrow keeps the computed admission current until insertion.
pub struct TrackedAdmission<'a> {
    pool: &'a mut ReadPool,
    reads: &'a Reads,
    bytes: (u64, u64, u64, u64, u64),
}
impl TrackedAdmission<'_> {
    pub fn required_bytes(&self) -> u64 {
        self.bytes.1
    }
    pub fn intern(
        self,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
        remaining: &mut u64,
        credit: Arc<std::sync::Mutex<u64>>,
    ) -> Option<Reads> {
        self.pool
            .intern_admitted(self.reads, self.bytes, allowance, remaining, Some(credit))
    }
}
#[cfg(test)]
thread_local! { static RECLAIM_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

impl ReadPool {
    /// The caller owns this dictionary's retention partition. Arrays retain
    /// their refundable leases independently; unused rows can now return the
    /// dictionary's original row and B-tree debits as well.
    pub fn reclaim(&mut self) -> u64 {
        let tree_bytes = |len| {
            let mut weight = crate::retained::MetadataWeight::default();
            if len != 0 {
                weight.tree::<RowRef, ()>(len);
            }
            weight.bytes()
        };
        let before = tree_bytes(self.rows.len());
        let mut reclaimed = 0;
        let mut next = self.released.as_ref().and_then(|released| {
            released
                .head
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
        });
        while let Some(row) = next {
            next = row.take_next();
            #[cfg(test)]
            RECLAIM_VISITS.with(|count| count.set(count.get() + 1));
            // The dictionary and this released reference are the last owners.
            // Another array or queued release keeps the row alive independently.
            if Arc::strong_count(&row) == 2 && self.rows.remove(&row.value) {
                reclaimed += row._allowance.bytes();
            }
        }
        if self.rows.is_empty() {
            self.rows = BTreeSet::new();
            if self
                .released
                .as_ref()
                .is_some_and(|released| Arc::strong_count(released) == 1)
            {
                reclaimed += self.released.take().unwrap()._allowance.bytes();
            }
        }
        reclaimed + before - tree_bytes(self.rows.len())
    }
    pub fn charged_bytes(&self) -> u64 {
        self.charged_bytes
    }
    pub fn required_bytes(&self, reads: &Reads) -> Option<u64> {
        if reads.is_empty() {
            return Some(0);
        }
        self.admission(reads).map(|(_, charge, _, _, _)| charge)
    }
    fn admission(&self, reads: &Reads) -> Option<(u64, u64, u64, u64, u64)> {
        let missing = reads
            .iter()
            .filter(|row| !self.rows.contains(&PoolRow(**row)))
            .count();
        let row_bytes = std::alloc::Layout::new::<[usize; 2]>()
            .extend(std::alloc::Layout::new::<ChargedRow>())
            .ok()?
            .0
            .pad_to_align()
            .size() as u64;
        let tree_bytes = |len| {
            let mut weight = crate::retained::MetadataWeight::default();
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
        let shared_bytes =
            shared_bytes.checked_add(reads.observation.as_ref().map_or(0, |order| {
                hirsel_engine_admission::retained_arc_bytes(&**order)
            }))?;
        let queue_bytes = if self.released.is_none() {
            std::alloc::Layout::new::<[usize; 2]>()
                .extend(std::alloc::Layout::new::<Released>())
                .ok()?
                .0
                .pad_to_align()
                .size() as u64
        } else {
            0
        };
        let loaned = (missing as u64)
            .checked_mul(row_bytes)?
            .checked_add(shared_bytes)?
            .checked_add(queue_bytes)?;
        let charge = loaned.checked_add(
            tree_bytes(self.rows.len().checked_add(missing)?)
                .checked_sub(tree_bytes(self.rows.len()))?,
        )?;
        Some((loaned, charge, row_bytes, shared_bytes, queue_bytes))
    }
    pub fn required_tracked_bytes(&self, reads: &Reads) -> Option<u64> {
        self.admission(reads)?
            .1
            .checked_add(hirsel_engine_admission::retained_arc_bytes(reads))
    }
    pub fn prepare_tracked<'a>(&'a mut self, reads: &'a Reads) -> Option<TrackedAdmission<'a>> {
        let (loaned, charge, row_bytes, shared_bytes, queue_bytes) = self.admission(reads)?;
        let wrapper = hirsel_engine_admission::retained_arc_bytes(reads);
        let bytes = (
            loaned.checked_add(wrapper)?,
            charge.checked_add(wrapper)?,
            row_bytes,
            shared_bytes.checked_add(wrapper)?,
            queue_bytes,
        );
        Some(TrackedAdmission {
            pool: self,
            reads,
            bytes,
        })
    }
    /// Admit one tracked receipt against a shared allowance and its retention
    /// credit. `make_room` may reclaim this pool, so each attempt recomputes
    /// the exact charge under the pool lock and checks both limits before
    /// interning. `Err(Some(bytes))` names the charge `make_room` refused.
    pub fn admit_tracked(
        pool: &Mutex<Self>,
        reads: &Reads,
        allowance: &hirsel_engine_admission::MemoryReservationPool,
        remaining: &Arc<Mutex<u64>>,
        mut make_room: impl FnMut(u64) -> bool,
    ) -> Result<Arc<Reads>, Option<u64>> {
        let mut charge = pool
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .required_tracked_bytes(reads)
            .ok_or(None)?;
        loop {
            if !make_room(charge) {
                return Err(Some(charge));
            }
            let mut pool = pool.lock().unwrap_or_else(|e| e.into_inner());
            let admission = pool.prepare_tracked(reads).ok_or(None)?;
            charge = admission.required_bytes();
            let mut credit = remaining.lock().unwrap_or_else(|e| e.into_inner());
            let available = allowance.available_bytes();
            if *credit < charge || available < charge {
                continue;
            }
            let mut lease = allowance.take(available).ok_or(None)?;
            let retained = Arc::new(
                admission
                    .intern(&mut lease, &mut credit, remaining.clone())
                    .ok_or(None)?,
            );
            Reads::bind_tracked_owner(&retained);
            return Ok(retained);
        }
    }
    pub fn intern(
        &mut self,
        reads: &Reads,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
        remaining: &mut u64,
    ) -> Option<Reads> {
        if reads.is_empty() {
            return Some(Reads::empty());
        }
        let bytes = self.admission(reads)?;
        self.intern_admitted(reads, bytes, allowance, remaining, None)
    }
    fn intern_admitted(
        &mut self,
        reads: &Reads,
        bytes: (u64, u64, u64, u64, u64),
        allowance: &mut hirsel_engine_admission::MemoryReservation,
        remaining: &mut u64,
        credit: Option<Arc<std::sync::Mutex<u64>>>,
    ) -> Option<Reads> {
        let (loaned, charge, row_bytes, shared_bytes, queue_bytes) = bytes;
        let after = remaining.checked_sub(charge)?;
        if allowance.bytes() < loaned {
            return None;
        }
        let released = self
            .released
            .get_or_insert_with(|| {
                Arc::new(Released {
                    head: Mutex::new(None),
                    _allowance: allowance
                        .split(queue_bytes)
                        .expect("pre-admitted release queue"),
                })
            })
            .clone();
        let mut rows = Vec::with_capacity(reads.len());
        for row in reads.iter() {
            let value = if let Some(value) = self.rows.get(&PoolRow(*row)) {
                value.0.clone()
            } else {
                let value = Arc::new(ChargedRow {
                    value: PoolRow(*row),
                    next: AtomicPtr::new(std::ptr::null_mut()),
                    _allowance: allowance.split(row_bytes).expect("pre-admitted read row"),
                });
                self.rows.insert(RowRef(value.clone()));
                value
            };
            rows.push(value);
        }
        let shared = Arc::new(SharedRows {
            values: rows.into_boxed_slice(),
            released,
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
            observation: reads.observation.clone(),
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
pub struct SharedRows {
    values: Box<[Arc<ChargedRow>]>,
    released: Arc<Released>,
    _allowance: hirsel_engine_admission::MemoryReservation,
    _credit: Option<ReadCredit>,
}

impl Drop for SharedRows {
    fn drop(&mut self) {
        let mut head = self.released.head.lock().unwrap_or_else(|e| e.into_inner());
        for row in std::mem::take(&mut self.values).into_vec() {
            let tail = Arc::as_ptr(&row).cast_mut();
            if row
                .next
                .compare_exchange(
                    std::ptr::null_mut(),
                    tail,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                let next = head
                    .take()
                    .map_or(tail, |next| Arc::into_raw(next).cast_mut());
                row.next.store(next, Ordering::Release);
                *head = Some(row);
            }
        }
    }
}

impl Reads {
    pub fn bind_tracked_owner(this: &Arc<Self>) {
        if let Rows::Shared(rows) = &this.rows
            && let Some(credit) = &rows._credit
        {
            let _ = credit.owner.set(Arc::downgrade(this));
        }
    }
    pub fn tracked_by(this: &Arc<Self>, remaining: &Arc<std::sync::Mutex<u64>>) -> bool {
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
pub enum Rows {
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
    pub fn shared(&self) -> bool {
        matches!(self, Self::Shared(_))
    }
    pub fn len(&self) -> usize {
        match self {
            Self::Flat(rows) => rows.len(),
            Self::Shared(rows) => rows.values.len(),
        }
    }
    pub fn as_ptr(&self) -> *const () {
        match self {
            Self::Flat(rows) => rows.as_ptr().cast(),
            Self::Shared(rows) => Arc::as_ptr(rows).cast(),
        }
    }
    pub fn allocation_bytes(&self) -> usize {
        // Shared observations and each receipt's array carry their own
        // original source reservations, including after their source drops.
        match self {
            Self::Flat(rows) => hirsel_engine_admission::retained_arc_bytes(&**rows) as usize,
            Self::Shared(_) => 0,
        }
    }
    pub fn binary_search_by(
        &self,
        mut compare: impl FnMut(&Row) -> std::cmp::Ordering,
    ) -> Result<usize, usize> {
        match self {
            Self::Flat(rows) => rows.binary_search_by(compare),
            Self::Shared(rows) => rows.values.binary_search_by(|row| compare(&row.value.0)),
        }
    }
}
impl std::ops::Index<usize> for Rows {
    type Output = Row;
    fn index(&self, at: usize) -> &Row {
        match self {
            Self::Flat(rows) => &rows[at],
            Self::Shared(rows) => &rows.values[at].value.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooled_observations_keep_exact_hashes_and_charge_physical_storage_until_last_owner() {
        let definitions = (0..127)
            .map(|index| DefId::intern(&DefPath::core(&format!("pool_{index}"))))
            .collect::<Vec<_>>();
        let originals = (0..8)
            .map(|set| {
                let observations = std::iter::once((
                    ReadKey::Environment,
                    ContentHash::of_bytes(b"pool-environment"),
                ))
                .chain((0..127).rev().map(|index| {
                    (
                        ReadKey::Signature(definitions[index]),
                        ContentHash::of_bytes(if index == set {
                            b"changed"
                        } else {
                            b"observed"
                        }),
                    )
                }))
                .collect::<Vec<_>>();
                let reads = Reads::from_vec(observations.clone()).unwrap();
                assert_eq!(reads.observed().copied().collect::<Vec<_>>(), observations);
                reads
            })
            .collect::<Vec<_>>();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let mut allowance = budget.reserve(1 << 20).unwrap();
        let mut remaining = allowance.bytes();
        let ((mut pool, pooled), measured) = crate::test_alloc::measured(|| {
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
            assert_eq!(
                pooled.observed().copied().collect::<Vec<_>>(),
                original.observed().copied().collect::<Vec<_>>()
            );
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
        let before = RECLAIM_VISITS.with(std::cell::Cell::get);
        for _ in 0..128 {
            assert_eq!(pool.reclaim(), 0);
        }
        assert_eq!(
            RECLAIM_VISITS.with(std::cell::Cell::get) - before,
            0,
            "retained receipts do not rescan the dictionary without a released array"
        );
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
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
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
        let budget = hirsel_engine_admission::ProcessBudget::new(4096);
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
    fn tracked_admission_makes_room_for_rows_its_own_reclaim_released() {
        // A reclaim inside make_room drops the released rows this receipt
        // re-reads, so its exact charge grows past the estimate it made room
        // for. Oracle: the evictable owner covers the whole first-time charge,
        // so admission must evict it rather than refuse.
        let reads = Reads::from_vec(
            (0..8)
                .map(|index| {
                    (
                        ReadKey::Signature(DefId::intern(&DefPath::core(&format!(
                            "reclaimed_{index}"
                        )))),
                        ContentHash::of_bytes(b"observed"),
                    )
                })
                .collect(),
        )
        .unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let allowance =
            hirsel_engine_admission::MemoryReservationPool::new(budget.reserve(1 << 20).unwrap())
                .ok()
                .unwrap();
        let credit = Arc::new(Mutex::new(1 << 20));
        let pool = Mutex::new(ReadPool::default());
        let first_time = pool.lock().unwrap().required_tracked_bytes(&reads).unwrap();
        drop(ReadPool::admit_tracked(&pool, &reads, &allowance, &credit, |_| true).unwrap());
        let repeat = pool.lock().unwrap().required_tracked_bytes(&reads).unwrap();
        assert!(
            repeat < first_time,
            "released rows still discount a re-read"
        );
        // Leave one byte less than the discounted estimate; an evictable owner
        // holds the rest of the allowance.
        let mut evictable = Some(
            allowance
                .take(allowance.available_bytes() - (repeat - 1))
                .unwrap(),
        );
        assert!(evictable.as_ref().unwrap().bytes() >= first_time);
        let admitted = ReadPool::admit_tracked(&pool, &reads, &allowance, &credit, |charge| {
            loop {
                if allowance.available_bytes() >= charge && *credit.lock().unwrap() >= charge {
                    return true;
                }
                let reclaimed = pool.lock().unwrap().reclaim();
                if reclaimed != 0 {
                    *credit.lock().unwrap() += reclaimed;
                } else if evictable.take().is_none() {
                    return false;
                }
            }
        })
        .expect("an evictable owner covers the charge");
        assert_eq!(admitted.as_ref(), &reads);
        assert!(admitted.observed().eq(reads.observed()));
        assert!(Reads::tracked_by(&admitted, &credit));
        drop(admitted);
        drop(pool);
        drop(allowance);
        assert_eq!(budget.reserved(), 0);
    }
    #[test]
    fn tracked_receipts_refuse_n_minus_one_and_refund_only_the_last_array_owner() {
        let environment = (
            ReadKey::Environment,
            ContentHash::of_bytes(b"tracked-environment"),
        );
        let signature = (
            ReadKey::Signature(DefId::intern(&DefPath::core("tracked"))),
            ContentHash::of_bytes(b"tracked-signature"),
        );
        let reads = Reads::from_vec(vec![environment, signature]).unwrap();
        let canonical = Reads::from_vec(vec![signature, environment]).unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let mut allowance = budget.reserve(1 << 20).unwrap();
        let mut pool = ReadPool::default();
        let ordered_required = pool.required_bytes(&reads).unwrap();
        let canonical_required = pool.required_bytes(&canonical).unwrap();
        let order_bytes = std::alloc::Layout::new::<[usize; 2]>()
            .extend(std::alloc::Layout::array::<u32>(2).unwrap())
            .unwrap()
            .0
            .pad_to_align()
            .size() as u64;
        assert_eq!(
            ordered_required.checked_sub(canonical_required),
            Some(order_bytes)
        );
        assert_eq!(
            reads.observed().copied().collect::<Vec<_>>(),
            vec![environment, signature]
        );
        let required = pool.required_tracked_bytes(&reads).unwrap();
        let remaining = Arc::new(std::sync::Mutex::new(required - 1));
        let before = allowance.bytes();
        assert!(
            pool.prepare_tracked(&reads)
                .unwrap()
                .intern(
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
            pool.prepare_tracked(&reads)
                .unwrap()
                .intern(
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
        assert_eq!(pool.reclaim(), 0, "a held receipt still owns every row");
        drop(held);
        assert_eq!(*remaining.lock().unwrap(), after + array_bytes);
        let reclaimed = pool.reclaim();
        *remaining.lock().unwrap() += reclaimed;
        assert_eq!(
            *remaining.lock().unwrap(),
            before,
            "the last reader and dictionary eviction return the entire original debit"
        );
        assert_eq!(pool.reclaim(), 0, "reclamation refunds each row only once");
        // Independently owned arrays release the same rows concurrently. A
        // later round must requeue a row that a held receipt kept alive.
        for _ in 0..4 {
            let mut local_remaining = allowance.bytes();
            let held = pool
                .intern(&reads, &mut allowance, &mut local_remaining)
                .unwrap();
            let arrays: Vec<_> = (0..16)
                .map(|_| {
                    pool.intern(&reads, &mut allowance, &mut local_remaining)
                        .unwrap()
                })
                .collect();
            std::thread::scope(|scope| {
                for array in arrays {
                    scope.spawn(move || drop(array));
                    assert_eq!(pool.reclaim(), 0, "held rows survive concurrent drains");
                }
            });
            assert_eq!(pool.reclaim(), 0, "held rows cannot be refunded");
            assert_eq!(held, reads);
            drop(held);
            assert!(pool.reclaim() > 0);
            assert!(pool.rows.is_empty());
            assert_eq!(pool.reclaim(), 0);
        }
        drop(pool);
        drop(allowance);
        assert_eq!(budget.reserved(), 0);
    }
}
