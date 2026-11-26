//! Object lifecycle: the launcher's retained objects, the store-holder claims, the
//! reservations admissions hold, and the collection fence that defers their release
//! (launch.md "Collect", RUNNER-SPEC §14.3-§14.5).
use super::admit::Admission;
use super::session::{Attempt, Child, Group, Launch, Mode, Role};
use super::{Image, Images};
use rustc_hash::FxHashMap;
use std::collections::{BTreeMap, BTreeSet};

/// The objects the launcher keeps and what holds them.
pub(super) struct ObjectLifecycle<I> {
    pub(super) images: I,
    claims: StoreClaims,
    reservations: BTreeMap<Attempt, BTreeSet<[u8; 32]>>,
    /// Between `CollectBegin` and its `Collect`: releases wait for the collection.
    fenced: bool,
    deferred: Vec<Attempt>,
}

impl<I: Images> ObjectLifecycle<I> {
    pub(super) fn new(images: I) -> Self {
        Self {
            images,
            claims: StoreClaims::default(),
            reservations: BTreeMap::new(),
            fenced: false,
            deferred: Vec::new(),
        }
    }

    /// Claim `launch`'s store hold and resolve its image; a failed resolve gives the
    /// claim back. `None` is a held store.
    pub(super) fn claim_image(&mut self, launch: &Launch) -> Option<Result<Image, String>> {
        if !self.claims.try_claim(launch) {
            return None;
        }
        let image = self
            .images
            .resolve(launch.assembly.as_slice().try_into().unwrap());
        if image.is_err() {
            self.claims.release(&launch.child());
        }
        Some(image)
    }

    pub(super) fn release_claim(&mut self, child: &Child) {
        self.claims.release(child);
    }

    /// Keep `objects` until the host releases `attempt`.
    pub(super) fn reserve(&mut self, attempt: Attempt, objects: BTreeSet<[u8; 32]>) {
        self.reservations
            .entry(attempt)
            .or_default()
            .extend(objects);
    }

    /// End `attempt`'s reservation, after the running collection when one is fenced.
    pub(super) fn release(&mut self, attempt: Attempt) {
        if self.fenced {
            self.deferred.push(attempt);
        } else {
            self.reservations.remove(&attempt);
        }
    }

    pub(super) fn fence(&mut self) {
        self.fenced = true;
    }

    pub(super) fn is_fenced(&self) -> bool {
        self.fenced
    }

    /// End the fence and apply the releases it deferred.
    pub(super) fn unfence(&mut self) {
        self.fenced = false;
        for attempt in std::mem::take(&mut self.deferred) {
            self.reservations.remove(&attempt);
        }
    }

    /// A new session: its fence ends without removing anything and every reservation of
    /// the old session ends (§14.5).
    pub(super) fn new_session(&mut self) {
        self.unfence();
        self.reservations.clear();
    }

    /// Remove every object neither `roots` nor a reservation keeps, then end the fence.
    /// The caller holds `admission`'s publish lock and has applied every written note.
    pub(super) fn collect(&mut self, roots: &[Vec<u8>], admission: Option<&Admission>) -> u64 {
        let mut keep = BTreeSet::new();
        let mut whole = true;
        for root in roots {
            match <[u8; 32]>::try_from(root.as_slice()) {
                Ok(root) => drop(keep.insert(root)),
                Err(_) => whole = false,
            }
        }
        for objects in self.reservations.values() {
            keep.extend(objects);
        }
        // A launcher without a whole root set removes nothing.
        // Code objects an admission has reserved but not yet published stay, under the
        // admission store's collection lock.
        let removed = match (admission, whole) {
            (_, false) => Ok(0),
            (Some(a), true) => a.under_collection(|reserved| self.images.collect(&keep, reserved)),
            (None, true) => self.images.collect(&keep, &|_| false),
        };
        self.unfence();
        removed.unwrap_or(0)
    }
}

/// Per-group store-holder claims (RUNNER-SPEC §14.3): at most one live or pending
/// store-holding child per group, or two during cutover when the second is a serving
/// candidate naming the first's generation.
#[derive(Default)]
struct StoreClaims {
    holders: FxHashMap<Group, [Option<Role>; 2]>,
}

impl StoreClaims {
    fn try_claim(&mut self, launch: &Launch) -> bool {
        let role = launch.mode.role();
        if !role.holds_store() {
            return true;
        }
        let candidate = match &launch.mode {
            Mode::Serving(_, serve) => serve.candidate,
            _ => None,
        };
        let entry = self.holders.entry(launch.group.clone()).or_default();
        match *entry {
            [None, None] => {
                entry[0] = Some(role);
                true
            }
            [Some(only), None] | [None, Some(only)] => {
                if candidate.is_some_and(|old| only == Role::Serving(old)) {
                    if entry[0].is_none() {
                        entry[0] = Some(role);
                    } else {
                        entry[1] = Some(role);
                    }
                    true
                } else {
                    false
                }
            }
            [Some(_), Some(_)] => false,
        }
    }

    fn release(&mut self, child: &Child) {
        if !child.role.holds_store() {
            return;
        }
        if let Some(entry) = self.holders.get_mut(&child.group) {
            if entry[0] == Some(child.role) {
                entry[0] = None;
            } else if entry[1] == Some(child.role) {
                entry[1] = None;
            }
            if entry[0].is_none() && entry[1].is_none() {
                self.holders.remove(&child.group);
            }
        }
    }
}
