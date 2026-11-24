//! Owned media state is pinned independently of the one-word resource carrier.
use crate::Context;

mod packet;
mod schedule;
use crate::{
    limit::Payer,
    secret::{Charge, ChargeCell, HandedOver},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

/// Tracks the charges that still belong to live media resources when a unit ends.
#[derive(Default)]
pub(crate) struct Charges(Mutex<BTreeMap<usize, Weak<ChargeCell>>>);

pub(crate) struct Payment {
    charges: Arc<Charges>,
    cell: Arc<ChargeCell>,
}

impl Charges {
    pub(crate) fn track(self: &Arc<Self>, charge: Charge) -> Payment {
        let cell = ChargeCell::new(charge);
        self.0
            .lock()
            .unwrap()
            .insert(Arc::as_ptr(&cell) as usize, Arc::downgrade(&cell));
        Payment {
            charges: self.clone(),
            cell,
        }
    }

    pub(crate) fn hand_over(&self, payer: Payer) -> (HandedOver, Vec<Charge>) {
        let mut handed = Vec::new();
        let mut refunds = Vec::new();
        for weak in self.0.lock().unwrap().values() {
            if let Some(cell) = weak.upgrade()
                && let Some(charge) = cell.take_charge(payer)
            {
                handed.push(crate::secret::HandedCharge::new(&cell, charge.bytes()));
                refunds.push(charge);
            }
        }
        (handed, refunds)
    }
}

impl Drop for Payment {
    fn drop(&mut self) {
        self.charges
            .0
            .lock()
            .unwrap()
            .remove(&(Arc::as_ptr(&self.cell) as usize));
    }
}

#[repr(C)]
struct Slot<T> {
    drop: unsafe fn(*mut u8),
    value: T,
}

pub(crate) fn hold<T: Send>(value: T) -> u64 {
    unsafe fn drop_slot<T>(slot: *mut u8) {
        // SAFETY: the owned word came from hold::<T> and is consumed once.
        drop(unsafe { Box::from_raw(slot.cast::<Slot<T>>()) });
    }
    Box::into_raw(Box::new(Slot {
        drop: drop_slot::<T>,
        value,
    })) as u64
}

/// # Safety
/// The live handle's slot contains T and stays borrowed through the call.
unsafe fn loan<'a, T>(handle: u64) -> &'a T {
    unsafe { &(*(handle as *const Slot<T>)).value }
}

/// # Safety
/// The live handle's slot contains T and the caller lends it exclusively.
unsafe fn loan_mut<'a, T>(handle: u64) -> &'a mut T {
    unsafe { &mut (*(handle as *mut Slot<T>)).value }
}

/// # Safety
/// The live media handle moves to this call and is never used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_media_drop(_: *mut Context, handle: u64) {
    let handle = handle as *mut u8;
    // SAFETY: the first field of the pinned repr(C) slot is its own destructor.
    let drop = unsafe { *handle.cast::<unsafe fn(*mut u8)>() };
    unsafe { drop(handle) };
}
