//! Compact immutable indexes over interned identities. Binary searches keep
//! lookups independent of path length without a second per-definition table.
use super::*;

pub(super) struct Inputs {
    rows: Box<[DefId]>,
    namespace: Arc<NamespaceCapture>,
}
impl Inputs {
    pub(super) fn new(mut rows: Vec<DefId>, namespace: Arc<NamespaceCapture>) -> Self {
        rows.sort_unstable();
        rows.dedup();
        Self {
            rows: rows.into_boxed_slice(),
            namespace,
        }
    }
    pub(super) fn retained_bytes(&self) -> Option<u64> {
        u64::try_from(size_of_val(&*self.rows)).ok()
    }
    pub(super) fn get(&self, path: &DefPath) -> Option<&Arc<namespace::BodyKeys>> {
        self.get_id(DefId::get(path)?)
    }
    pub(super) fn get_id(&self, id: DefId) -> Option<&Arc<namespace::BodyKeys>> {
        self.rows.binary_search(&id).ok()?;
        self.namespace.bodies.get(&id)?.queries.as_ref()
    }
    pub(super) fn contains_key(&self, path: &DefPath) -> bool {
        self.get(path).is_some()
    }
    pub(super) fn iter(&self) -> Iter<'_> {
        Iter {
            rows: self.rows.iter(),
            namespace: &self.namespace,
        }
    }
    pub(super) fn keys(&self) -> impl Iterator<Item = &DefPath> {
        self.rows.iter().map(|id| id.path())
    }
}
impl std::ops::Index<&DefPath> for Inputs {
    type Output = Arc<namespace::BodyKeys>;
    fn index(&self, path: &DefPath) -> &Self::Output {
        self.get(path).expect("captured Lower input")
    }
}
pub(super) struct Iter<'a> {
    rows: std::slice::Iter<'a, DefId>,
    namespace: &'a NamespaceCapture,
}
impl<'a> Iterator for Iter<'a> {
    type Item = (&'static DefPath, &'a Arc<namespace::BodyKeys>);
    fn next(&mut self) -> Option<Self::Item> {
        self.rows.next().map(|id| {
            (
                id.path(),
                self.namespace.bodies[id]
                    .queries
                    .as_ref()
                    .expect("captured Lower phase binding"),
            )
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

/// Only suspended queries retain a request site. Capacity stays charged until
/// this view drops, including after the last active site has been removed.
pub(super) struct Waiting {
    rows: Vec<((DefId, u32), Span)>,
    charge: LowerInputCharge,
}
impl Waiting {
    const INITIAL: usize = 4;
    pub(super) fn input_bytes() -> usize {
        Self::INITIAL * size_of::<((DefId, u32), Span)>() + 2 * size_of::<usize>()
    }
    pub(super) fn new(remaining: Arc<Mutex<u64>>) -> Self {
        Self {
            rows: Vec::with_capacity(Self::INITIAL),
            charge: LowerInputCharge {
                remaining,
                bytes: 0,
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
                let extra =
                    self.rows
                        .capacity()
                        .checked_mul(size_of::<((DefId, u32), Span)>())? as u64;
                let mut remaining = self
                    .charge
                    .remaining
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *remaining = remaining.checked_sub(extra)?;
                self.charge.bytes += extra;
                self.rows.reserve_exact(self.rows.capacity());
            }
            self.rows.push((key, site));
        }
        Some(())
    }
}

/// Query addresses differ across captures. Index their prefixes and recover
/// the complete address from the admitted input before accepting a match.
pub(super) struct Paths {
    lower: Box<[(u32, DefId)]>,
    evaluate: Box<[(u32, DefId)]>,
}
impl Paths {
    pub(super) fn input_bytes(definitions: usize) -> Option<usize> {
        definitions.checked_mul(2 * size_of::<(u32, DefId)>())
    }
    fn prefix(address: ContentHash) -> u32 {
        u32::from_le_bytes([address.0[0], address.0[1], address.0[2], address.0[3]])
    }
    fn find(
        rows: &[(u32, DefId)],
        address: ContentHash,
        mut identity: impl FnMut(DefId) -> Option<ContentHash>,
    ) -> Option<&DefId> {
        let prefix = Self::prefix(address);
        let start = rows.partition_point(|(candidate, _)| *candidate < prefix);
        rows[start..]
            .iter()
            .take_while(|(candidate, _)| *candidate == prefix)
            .find_map(|(_, id)| (identity(*id) == Some(address)).then_some(id))
    }
    pub(super) fn new(inputs: &Inputs, namespace: &NamespaceCapture) -> Self {
        let rows = |kind, key: fn(&namespace::BodyKeys) -> ContentHash| {
            let mut rows = inputs
                .iter()
                .map(|(path, input)| {
                    (
                        Self::prefix(namespace.query_id(kind, key(input)).address.0),
                        DefId::intern(path),
                    )
                })
                .collect::<Vec<_>>();
            rows.sort_unstable_by_key(|(hash, _)| *hash);
            rows.into_boxed_slice()
        };
        Self {
            lower: rows(FRONT_LOWER_KIND, |keys| keys.lower.0),
            evaluate: rows(FRONT_EVALUATE_KIND, |keys| keys.evaluate.0),
        }
    }
    pub(super) fn get(
        &self,
        (kind, address): &(u32, ContentHash),
        inputs: &Inputs,
        namespace: &NamespaceCapture,
    ) -> Option<&DefId> {
        let rows = match *kind {
            FRONT_LOWER_KIND => &self.lower,
            FRONT_EVALUATE_KIND => &self.evaluate,
            _ => return None,
        };
        Self::find(rows, *address, |id| {
            let input = inputs.get_id(id)?;
            let key = if *kind == FRONT_LOWER_KIND {
                input.lower.0
            } else {
                input.evaluate.0
            };
            Some(namespace.query_id(*kind, key).address.0)
        })
    }
    pub(super) fn retained_bytes(&self) -> usize {
        size_of_val(&*self.lower) + size_of_val(&*self.evaluate)
    }
}
