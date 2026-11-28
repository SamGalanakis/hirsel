//! Loader-owned references carried by guest words. Host RawCode and CauseDataRef
//! own their own counts. A normal guest drop or transfer removes its count here.
use super::ops;
use crate::{causes::CauseData, run_stop};
use std::{collections::BTreeMap, sync::Mutex};

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Owner {
    Code,
    Slot,
    Literal,
}

#[derive(Default)]
pub(crate) struct GuestOwners(Mutex<BTreeMap<(Owner, usize), usize>>);

impl GuestOwners {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }

    pub(crate) fn park(&mut self) {
        let owners = self.0.get_mut().unwrap();
        if owners.is_empty() {
            *owners = BTreeMap::new();
        }
    }

    pub(crate) fn release_stopped(&mut self) {
        for ((owner, address), count) in std::mem::take(self.0.get_mut().unwrap()) {
            let pointer = std::ptr::with_exposed_provenance_mut(address);
            for _ in 0..count {
                // SAFETY: each entry owns a reference retained by the guest.
                // The generation has no worker or admitted ABI call left.
                unsafe {
                    match owner {
                        Owner::Code => (ops::ops().unwrap().code_drop)(pointer),
                        Owner::Slot => (ops::ops().unwrap().slot_drop)(pointer),
                        Owner::Literal => CauseData::release(pointer.cast()),
                    }
                }
            }
        }
    }
}

impl Owner {
    pub(crate) fn acquire<T>(self, p: *const T) {
        if !p.is_null()
            && let Some(generation) = run_stop::current_execution()
        {
            let mut owners = generation.guest_owners.0.lock().unwrap();
            *owners.entry((self, p.expose_provenance())).or_default() += 1;
        }
    }

    pub(crate) fn transfer<T>(self, p: *const T) {
        if let Some(generation) = run_stop::current_execution() {
            let mut owners = generation.guest_owners.0.lock().unwrap();
            let key = (self, p.addr());
            if let Some(count) = owners.get_mut(&key) {
                *count -= 1;
                if *count == 0 {
                    owners.remove(&key);
                }
            }
        }
    }
}
