//! The evaluator's role check (crate docs, "Roles").

use crate::roles::Role;
use std::collections::{BTreeMap, HashMap};

/// Per running activation: its roles, and by block the byte spans each role
/// touched (`lo -> (hi, role)`, disjoint, a role's adjacent spans merged), apart
/// from `Role::ViewBuffers`, whose read spans and every other role's written
/// spans are kept on their own.
pub(super) struct Footprint {
    pub roles: std::sync::Arc<[Option<Role>]>,
    spans: HashMap<usize, BTreeMap<usize, (usize, Role)>>,
    /// By block, the bytes `Role::ViewBuffers` read and the bytes any other role
    /// wrote (`lo -> hi`, disjoint, adjacent spans merged).
    viewed: HashMap<usize, BTreeMap<usize, usize>>,
    written: HashMap<usize, BTreeMap<usize, usize>>,
}

impl Footprint {
    pub fn new(roles: std::sync::Arc<[Option<Role>]>) -> Self {
        Footprint {
            roles,
            spans: HashMap::new(),
            viewed: HashMap::new(),
            written: HashMap::new(),
        }
    }

    /// Note that `role` read or wrote (`write`) bytes `lo..hi` of block `b`, and
    /// stop the run when another role touched any of them, unless both are reads
    /// and one is `Role::ViewBuffers`, or when anything writes a view buffer.
    pub fn touch(&mut self, role: Role, b: usize, lo: usize, hi: usize, write: bool) {
        if role == Role::ViewBuffers {
            assert!(!write, "a view buffer is written at +{lo}..+{hi}");
            if let Some((l, h)) = self.written.get(&b).and_then(|w| overlap(w, lo, hi)) {
                panic!(
                    "{role:?} reads +{}..+{} of a block another role wrote",
                    lo.max(l),
                    hi.min(h)
                );
            }
            cover(self.viewed.entry(b).or_default(), lo, hi);
            return;
        }
        if write {
            if let Some((l, h)) = self.viewed.get(&b).and_then(|v| overlap(v, lo, hi)) {
                panic!(
                    "{role:?} writes +{}..+{} of a block {:?} read",
                    lo.max(l),
                    hi.min(h),
                    Role::ViewBuffers
                );
            }
            cover(self.written.entry(b).or_default(), lo, hi);
        }
        let spans = self.spans.entry(b).or_default();
        let near: Vec<(usize, usize, Role)> = spans
            .range(..=hi)
            .rev()
            .take_while(|(_, (h, _))| *h >= lo)
            .map(|(&l, &(h, r))| (l, h, r))
            .collect();
        let (mut start, mut end) = (lo, hi);
        for (l, h, r) in near {
            if r == role {
                spans.remove(&l);
                (start, end) = (start.min(l), end.max(h));
            } else {
                assert!(
                    h <= lo || hi <= l,
                    "{role:?} touches +{}..+{} of a block that {r:?} touched",
                    lo.max(l),
                    hi.min(h)
                );
            }
        }
        spans.insert(start, (end, role));
    }
}

/// The last span of `set` that overlaps `lo..hi`.
fn overlap(set: &BTreeMap<usize, usize>, lo: usize, hi: usize) -> Option<(usize, usize)> {
    set.range(..hi)
        .next_back()
        .map(|(&l, &h)| (l, h))
        .filter(|&(_, h)| h > lo)
}

/// Add `lo..hi` to `set`, merging the spans it overlaps or abuts.
fn cover(set: &mut BTreeMap<usize, usize>, lo: usize, hi: usize) {
    let near: Vec<(usize, usize)> = set
        .range(..=hi)
        .rev()
        .take_while(|(_, h)| **h >= lo)
        .map(|(&l, &h)| (l, h))
        .collect();
    let (mut start, mut end) = (lo, hi);
    for (l, h) in near {
        set.remove(&l);
        (start, end) = (start.min(l), end.max(h));
    }
    set.insert(start, end);
}
