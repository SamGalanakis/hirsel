//! Object lifecycle state: store-holder claims (moved here ahead of full GC/capture split).
use super::session::{Child, Group, Launch, Mode, Role};
use rustc_hash::FxHashMap;

/// Per-group store-holder claims (RUNNER-SPEC §14.3): at most one live or pending
/// store-holding child per group, or two during cutover when the second is a serving
/// candidate naming the first's generation.
#[derive(Default)]
pub(super) struct StoreClaims {
    holders: FxHashMap<Group, [Option<Role>; 2]>,
}

impl StoreClaims {
    pub(super) fn try_claim(&mut self, launch: &Launch) -> bool {
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

    pub(super) fn release(&mut self, child: &Child) {
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
