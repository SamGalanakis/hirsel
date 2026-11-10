//! Immutable selection deltas share their predecessor's inventory. Phase
//! bindings and query identities always come from the consuming capture.
use super::*;

pub(super) struct Inventory {
    previous: Option<Arc<Inventory>>,
    added: Box<[DefId]>,
    removed: Box<[DefId]>,
    _charge: LowerInputCharge,
}
impl Inventory {
    fn contains(&self, id: DefId) -> bool {
        let mut cursor = Some(self);
        while let Some(inventory) = cursor {
            if inventory.added.binary_search(&id).is_ok() {
                return true;
            }
            if inventory.removed.binary_search(&id).is_ok() {
                return false;
            }
            cursor = inventory.previous.as_deref();
        }
        false
    }
}
pub(super) struct Inputs {
    inventory: Arc<Inventory>,
    namespace: Arc<NamespaceCapture>,
}
impl Inputs {
    pub(super) fn inventory_bytes(definitions: usize) -> Option<usize> {
        (size_of::<Inventory>() + 2 * size_of::<usize>())
            .checked_add(definitions.checked_mul(size_of::<DefId>())?)
    }
    pub(super) fn new(
        mut rows: Vec<DefId>,
        namespace: Arc<NamespaceCapture>,
        previous: Option<&Self>,
        partition: &LowerPartition,
    ) -> Option<Self> {
        rows.sort_unstable();
        rows.dedup();
        let removed = previous.map_or_else(Vec::new, |previous| {
            previous
                .keys()
                .map(DefId::intern)
                .filter(|id| rows.binary_search(id).is_err())
                .collect::<Vec<_>>()
        });
        if let Some(previous) = previous {
            rows.retain(|id| !previous.inventory.contains(*id));
            if rows.is_empty() && removed.is_empty() {
                return Some(Self {
                    inventory: previous.inventory.clone(),
                    namespace,
                });
            }
        }
        let bytes = Self::inventory_bytes(rows.len().checked_add(removed.len())?)? as u64;
        let charge = LowerInputCharge::take(partition, bytes)?;
        Some(Self {
            inventory: Arc::new(Inventory {
                previous: previous.map(|previous| previous.inventory.clone()),
                added: rows.into_boxed_slice(),
                removed: removed.into_boxed_slice(),
                _charge: charge,
            }),
            namespace,
        })
    }
    pub(super) fn get(&self, path: &DefPath) -> Option<&Arc<namespace::BodyKeys>> {
        self.get_id(DefId::get(path)?)
    }
    pub(super) fn get_id(&self, id: DefId) -> Option<&Arc<namespace::BodyKeys>> {
        if !self.inventory.contains(id) {
            return None;
        }
        self.namespace.bodies.get(&id)?.queries.as_ref()
    }
    pub(super) fn contains_key(&self, path: &DefPath) -> bool {
        self.get(path).is_some()
    }
    pub(super) fn iter(&self) -> Iter<'_> {
        Iter {
            rows: self.namespace.bodies.iter(),
            inventory: &self.inventory,
        }
    }
    pub(super) fn keys(&self) -> impl Iterator<Item = &DefPath> {
        self.iter().map(|(path, _)| path)
    }
}
impl std::ops::Index<&DefPath> for Inputs {
    type Output = Arc<namespace::BodyKeys>;
    fn index(&self, path: &DefPath) -> &Self::Output {
        self.get(path).expect("captured Lower input")
    }
}
pub(super) struct Iter<'a> {
    rows: Box<dyn Iterator<Item = (&'a DefId, &'a Arc<namespace::BodyInput>)> + 'a>,
    inventory: &'a Inventory,
}
impl<'a> Iterator for Iter<'a> {
    type Item = (&'static DefPath, &'a Arc<namespace::BodyKeys>);
    fn next(&mut self) -> Option<Self::Item> {
        self.rows.find_map(|(id, body)| {
            self.inventory.contains(*id).then(|| {
                (
                    id.path(),
                    body.queries.as_ref().expect("captured Lower phase binding"),
                )
            })
        })
    }
}
impl<'a> IntoIterator for &'a Inputs {
    type Item = (&'static DefPath, &'a Arc<namespace::BodyKeys>);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// An immutable table owns its debit once, independently of capture lifetime.
pub(super) struct Shared<T> {
    pub(super) value: T,
    _charge: LowerInputCharge,
}
impl<T> Shared<T> {
    pub(super) fn new(value: T, bytes: u64, partition: &LowerPartition) -> Option<Arc<Self>> {
        let bytes = bytes.checked_add((size_of::<Self>() + 2 * size_of::<usize>()) as u64)?;
        Some(Arc::new(Self {
            value,
            _charge: LowerInputCharge::take(partition, bytes)?,
        }))
    }
}
impl<T> std::ops::Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

/// Only suspended queries retain a request site. Capacity stays charged until
/// this view drops, including after the last active site has been removed.
pub(super) struct Waiting {
    rows: Vec<((DefId, u32), Span)>,
    charge: LowerInputCharge,
}
impl Waiting {
    const INITIAL: usize = 4;
    pub(super) fn initial_bytes() -> usize {
        Self::INITIAL * size_of::<((DefId, u32), Span)>() + 2 * size_of::<usize>()
    }
    pub(super) fn new(partition: &LowerPartition, initial: bool) -> Self {
        Self {
            rows: if initial {
                Vec::with_capacity(Self::INITIAL)
            } else {
                Vec::new()
            },
            charge: LowerInputCharge::empty(partition.request()),
        }
    }
    pub(super) fn get(&self, key: (DefId, u32)) -> Option<Span> {
        self.rows
            .iter()
            .find(|(id, _)| *id == key)
            .map(|(_, site)| *site)
    }
    pub(super) fn set(&mut self, key: (DefId, u32), site: Option<Span>) -> Option<()> {
        if let Some(at) = self.rows.iter().position(|(id, _)| *id == key) {
            if let Some(site) = site {
                self.rows[at].1 = site;
            } else {
                self.rows.swap_remove(at);
            }
        } else if let Some(site) = site {
            if self.rows.len() == self.rows.capacity() {
                let growth = self.rows.capacity().max(1);
                let extra = growth
                    .checked_mul(size_of::<((DefId, u32), Span)>())?
                    .checked_add(if self.rows.capacity() == 0 {
                        2 * size_of::<usize>()
                    } else {
                        0
                    })? as u64;
                self.charge.grow(extra)?;
                self.rows.reserve_exact(growth);
            }
            self.rows.push((key, site));
        }
        Some(())
    }
}

/// Capture-salted addresses need their own reverse lookup. Queried
/// definitions are remembered by full address, charging actual capacity to the
/// existing partition. The first miss indexes every input once, so later
/// misses are definite without rehashing every binding. When that growth
/// cannot be admitted, the exact binding scan remains.
pub(super) struct Paths {
    rows: Mutex<PathRows>,
}
#[derive(Default)]
struct PathIndex {
    rows: Vec<(ContentHash, DefId)>,
    /// Every input's address is present, each mapped to the definition the
    /// binding scan finds first; a miss is definite.
    complete: bool,
    /// Admission refused the complete index; misses keep the exact scan.
    refused: bool,
}
struct PathRows {
    lower: PathIndex,
    evaluate: PathIndex,
    charge: LowerInputCharge,
}
impl Paths {
    pub(super) fn new(partition: &LowerPartition) -> Self {
        Self {
            rows: Mutex::new(PathRows {
                lower: PathIndex::default(),
                evaluate: PathIndex::default(),
                charge: LowerInputCharge::empty(partition.request()),
            }),
        }
    }
    pub(super) fn clear(&self) {
        let mut index = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        drop(std::mem::take(&mut index.lower));
        drop(std::mem::take(&mut index.evaluate));
        index.charge.release();
    }
    pub(super) fn remember(&self, query: QueryId, id: DefId) {
        let mut index = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let PathRows {
            lower,
            evaluate,
            charge,
        } = &mut *index;
        let index = match query.kind {
            FRONT_LOWER_KIND => lower,
            FRONT_EVALUATE_KIND => evaluate,
            _ => return,
        };
        if index.complete {
            return;
        }
        let rows = &mut index.rows;
        let row = (query.address.0, id);
        let Err(at) = rows.binary_search(&row) else {
            return;
        };
        if rows.len() == rows.capacity() {
            let capacity = rows.capacity().max(4);
            let Some(bytes) = capacity
                .checked_mul(size_of::<(ContentHash, DefId)>())
                .and_then(|bytes| u64::try_from(bytes).ok())
            else {
                return;
            };
            if charge.grow(bytes).is_none() {
                return;
            }
            rows.reserve_exact(capacity);
        }
        rows.insert(at, row);
    }
    pub(super) fn get(
        &self,
        (kind, address): &(u32, ContentHash),
        inputs: &Inputs,
        namespace: &NamespaceCapture,
    ) -> Option<DefId> {
        let identity = |input: &namespace::BodyKeys| {
            let key = if *kind == FRONT_LOWER_KIND {
                input.lower.0
            } else {
                input.evaluate.0
            };
            namespace.query_id(*kind, key).address.0
        };
        let mut guard = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let PathRows {
            lower,
            evaluate,
            charge,
        } = &mut *guard;
        let index = match *kind {
            FRONT_LOWER_KIND => lower,
            FRONT_EVALUATE_KIND => evaluate,
            _ => return None,
        };
        let found = |rows: &[(ContentHash, DefId)]| {
            let start = rows.partition_point(|(candidate, _)| candidate < address);
            rows.get(start)
                .filter(|(candidate, _)| candidate == address)
                .map(|(_, id)| *id)
        };
        if let Some(id) = found(&index.rows) {
            return Some(id);
        }
        if index.complete {
            return None;
        }
        if !index.refused {
            // Inputs and the namespace are immutable for this owner, so each
            // address keeps the definition the binding scan finds first.
            let mut rows = inputs
                .iter()
                .enumerate()
                .map(|(at, (path, input))| (identity(input), at, DefId::intern(path)))
                .collect::<Vec<_>>();
            rows.sort_unstable_by_key(|(address, at, _)| (*address, *at));
            rows.dedup_by_key(|(address, _, _)| *address);
            let mut rows = rows
                .into_iter()
                .map(|(address, _, id)| (address, id))
                .collect::<Vec<_>>();
            rows.shrink_to_fit();
            let growth = rows
                .capacity()
                .saturating_sub(index.rows.capacity())
                .checked_mul(size_of::<(ContentHash, DefId)>())
                .and_then(|bytes| u64::try_from(bytes).ok());
            if growth.and_then(|bytes| charge.grow(bytes)).is_some() {
                index.rows = rows;
                index.complete = true;
                return found(&index.rows);
            }
            index.refused = true;
        }
        drop(guard);
        let id = inputs
            .iter()
            .find_map(|(path, input)| (identity(input) == *address).then(|| DefId::intern(path)))?;
        self.remember(
            QueryId {
                kind: *kind,
                address: hirsel_artifact_store::Address(*address),
            },
            id,
        );
        Some(id)
    }
}
