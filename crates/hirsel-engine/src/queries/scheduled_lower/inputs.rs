//! Compact immutable indexes over interned identities. Binary searches keep
//! lookups independent of path length without a second per-definition table.
use super::*;

pub(super) struct Inputs {
    rows: Box<[(DefId, Input)]>,
}
impl Inputs {
    pub(super) fn new(mut rows: Vec<(DefId, Input)>) -> Self {
        rows.sort_unstable_by_key(|(id, _)| *id);
        rows.dedup_by_key(|(id, _)| *id);
        Self {
            rows: rows.into_boxed_slice(),
        }
    }
    pub(super) fn retained_bytes(&self) -> Option<u64> {
        u64::try_from(size_of_val(&*self.rows)).ok()
    }
    pub(super) fn get(&self, path: &DefPath) -> Option<&Input> {
        self.get_id(DefId::get(path)?)
    }
    pub(super) fn get_id(&self, id: DefId) -> Option<&Input> {
        let at = self.rows.binary_search_by_key(&id, |(id, _)| *id).ok()?;
        Some(&self.rows[at].1)
    }
    pub(super) fn contains_key(&self, path: &DefPath) -> bool {
        self.get(path).is_some()
    }
    pub(super) fn iter(&self) -> Iter<'_> {
        Iter(self.rows.iter())
    }
    pub(super) fn keys(&self) -> impl Iterator<Item = &DefPath> {
        self.rows.iter().map(|(id, _)| id.path())
    }
}
impl std::ops::Index<&DefPath> for Inputs {
    type Output = Input;
    fn index(&self, path: &DefPath) -> &Input {
        self.get(path).expect("captured Lower input")
    }
}
pub(super) struct Iter<'a>(std::slice::Iter<'a, (DefId, Input)>);
impl<'a> Iterator for Iter<'a> {
    type Item = (&'static DefPath, &'a Input);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(id, input)| (id.path(), input))
    }
}
impl<'a> IntoIterator for &'a Inputs {
    type Item = (&'static DefPath, &'a Input);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Query addresses differ across captures. Keep only their compact reverse
/// interface, not a tree node and duplicated kind for every definition.
pub(super) struct Paths {
    lower: Box<[(ContentHash, DefId)]>,
    evaluate: Box<[(ContentHash, DefId)]>,
}
impl Paths {
    pub(super) fn new(inputs: &Inputs, namespace: &NamespaceCapture) -> Self {
        let rows = |kind, key: fn(&namespace::BodyKeys) -> ContentHash| {
            let mut rows = inputs
                .iter()
                .map(|(path, input)| {
                    (
                        namespace.query_id(kind, key(&input.captured)).address.0,
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
    pub(super) fn get(&self, (kind, address): &(u32, ContentHash)) -> Option<&DefId> {
        let rows = match *kind {
            FRONT_LOWER_KIND => &self.lower,
            FRONT_EVALUATE_KIND => &self.evaluate,
            _ => return None,
        };
        let at = rows.binary_search_by_key(address, |(hash, _)| *hash).ok()?;
        Some(&rows[at].1)
    }
    pub(super) fn retained_bytes(&self) -> usize {
        size_of_val(&*self.lower) + size_of_val(&*self.evaluate)
    }
}
