//! Guest raw storage with an exact logical refund. A native Block owns its
//! own refund until ownership passes into a guest word.
use super::*;
use std::{collections::BTreeMap, sync::Weak};

type Release = unsafe fn(NonNull<u8>, u64);

#[derive(Default)]
pub(crate) struct RawStorage(Mutex<BTreeMap<usize, (u64, Release)>>);

impl RawStorage {
    pub(crate) fn park(&self) -> bool {
        let mut owners = self.0.lock().unwrap();
        if owners.is_empty() {
            *owners = BTreeMap::new();
            return true;
        }
        false
    }

    pub(super) fn current() -> Option<Weak<Self>> {
        let execution = crate::run_stop::current_execution()?;
        execution
            .is_contained()
            .then(|| Arc::downgrade(execution.raw_storage.get_or_init(Arc::default)))
    }

    /// Transfer a native box from Rust to an execution-local guest handle.
    /// # Safety
    /// The destructor is safe after worker quiescence on the generation owner thread.
    pub(crate) unsafe fn into_guest<T>(value: Box<T>) -> *mut T {
        unsafe fn release_box<T>(p: NonNull<u8>, _: u64) {
            unsafe { drop(Box::from_raw(p.cast::<T>().as_ptr())) };
        }
        let p = Box::into_raw(value);
        if let Some(g) = crate::run_stop::current_execution().filter(|g| g.is_contained()) {
            g.raw_storage
                .get_or_init(Arc::default)
                .0
                .lock()
                .unwrap()
                .insert(p.expose_provenance(), (0, release_box::<T>));
        }
        p
    }

    /// The guest has returned its sole native box to its ordinary destructor.
    pub(crate) unsafe fn from_guest<T>(p: *mut T) -> Box<T> {
        if let Some(ledger) = crate::run_stop::current_execution().and_then(|g| g.raw_storage.get())
        {
            ledger.0.lock().unwrap().remove(&p.addr());
        }
        unsafe { Box::from_raw(p) }
    }

    pub(crate) fn reclaim(&self) {
        loop {
            let entry = self.0.lock().unwrap().pop_first();
            let Some((address, (charge, release))) = entry else {
                break;
            };
            let payload = NonNull::new(std::ptr::with_exposed_provenance_mut(address)).unwrap();
            // SAFETY: all workers have left. Normal release or conversion into
            // native custody removed its entry before publishing the slot.
            unsafe { release(payload, charge) };
        }
        returns::purge();
    }
}

impl Page {
    pub(crate) unsafe fn own_raw(payload: NonNull<u8>, charge: u64, release: Release) {
        // SAFETY: caller owns live slot storage.
        let page = unsafe { page_of(payload).as_ref() };
        if let Some(ledger) = page.heap.raw_storage.as_ref().and_then(Weak::upgrade) {
            ledger
                .0
                .lock()
                .unwrap()
                .insert(payload.as_ptr().expose_provenance(), (charge, release));
        }
    }

    /// A raw heap-backed message now belongs to an independently retained cell.
    pub(crate) unsafe fn take_raw(payload: NonNull<u8>) {
        unsafe { page_of(payload).as_ref().forget_raw(payload) };
    }

    pub(crate) fn forget_raw(&self, payload: NonNull<u8>) {
        if let Some(ledger) = self.heap.raw_storage.as_ref().and_then(Weak::upgrade) {
            ledger.0.lock().unwrap().remove(&payload.addr().get());
        }
    }
}
