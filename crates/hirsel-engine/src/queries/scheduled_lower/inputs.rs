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
            charge: LowerInputCharge {
                remaining: partition.remaining.clone(),
                bytes: 0,
                _allowance: partition.allowance.clone(),
            },
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
                let mut remaining = self
                    .charge
                    .remaining
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *remaining = remaining.checked_sub(extra)?;
                self.charge.bytes += extra;
                self.rows.reserve_exact(growth);
            }
            self.rows.push((key, site));
        }
        Some(())
    }
}

/// Capture-salted addresses need their own reverse lookup. Index only queried
/// definitions, charging actual capacity to the existing partition. A full
/// binding scan remains exact when optional index growth cannot be admitted.
pub(super) struct Paths {
    rows: Mutex<PathRows>,
}
struct PathRows {
    lower: Vec<(u32, DefId)>,
    evaluate: Vec<(u32, DefId)>,
    charge: LowerInputCharge,
}
impl Paths {
    pub(super) fn new(partition: &LowerPartition) -> Self {
        Self {
            rows: Mutex::new(PathRows {
                lower: Vec::new(),
                evaluate: Vec::new(),
                charge: LowerInputCharge {
                    remaining: partition.remaining.clone(),
                    bytes: 0,
                    _allowance: partition.allowance.clone(),
                },
            }),
        }
    }
    pub(super) fn clear(&self) {
        let mut index = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        drop(std::mem::take(&mut index.lower));
        drop(std::mem::take(&mut index.evaluate));
        let bytes = std::mem::take(&mut index.charge.bytes);
        *index
            .charge
            .remaining
            .lock()
            .unwrap_or_else(|e| e.into_inner()) += bytes;
    }
    fn prefix(address: ContentHash) -> u32 {
        u32::from_le_bytes([address.0[0], address.0[1], address.0[2], address.0[3]])
    }
    pub(super) fn remember(&self, query: QueryId, id: DefId) {
        let mut index = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let PathRows {
            lower,
            evaluate,
            charge,
        } = &mut *index;
        let rows = match query.kind {
            FRONT_LOWER_KIND => lower,
            FRONT_EVALUATE_KIND => evaluate,
            _ => return,
        };
        let row = (Self::prefix(query.address.0), id);
        let Err(at) = rows.binary_search(&row) else {
            return;
        };
        if rows.len() == rows.capacity() {
            let capacity = rows.capacity().max(4);
            let Some(bytes) = capacity
                .checked_mul(size_of::<(u32, DefId)>())
                .and_then(|bytes| u64::try_from(bytes).ok())
            else {
                return;
            };
            let mut remaining = charge.remaining.lock().unwrap_or_else(|e| e.into_inner());
            let Some(available) = remaining.checked_sub(bytes) else {
                return;
            };
            *remaining = available;
            charge.bytes += bytes;
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
        {
            let index = self.rows.lock().unwrap_or_else(|e| e.into_inner());
            let rows = match *kind {
                FRONT_LOWER_KIND => &index.lower,
                FRONT_EVALUATE_KIND => &index.evaluate,
                _ => return None,
            };
            let prefix = Self::prefix(*address);
            let start = rows.partition_point(|(candidate, _)| *candidate < prefix);
            if let Some(id) = rows[start..]
                .iter()
                .take_while(|(candidate, _)| *candidate == prefix)
                .find_map(|(_, id)| (identity(inputs.get_id(*id)?) == *address).then_some(*id))
            {
                return Some(id);
            }
        }
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
