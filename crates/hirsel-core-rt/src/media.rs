//! Owned media state is pinned independently of the one-word resource carrier.
mod agreement;
use crate::Context;

#[cfg(feature = "record")]
pub(crate) mod capture;
mod import;
mod packet;
mod schedule;
mod sframe;
mod transitions;
// The tape laws drive SFrame imports and seals through the host ops (TLS-8 PC9).
use crate::{
    limit::Payer,
    secret::{Charge, ChargeCell, HandedOver},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};
#[cfg(all(test, feature = "record"))]
pub(crate) use {
    import::hirsel_core_schedule_import_sframe, packet::hirsel_core_packet_seal_into,
    sframe::hirsel_core_schedule_sframe_sealer,
};

/// A run's media state, built by its first media resource.
#[derive(Default)]
pub(crate) struct RunState {
    pub(crate) charges: Arc<Charges>,
    pub(crate) agreements: Arc<hirsel_core_net::agreement_registry::AgreementRegistry<Payment>>,
    pub(crate) send_keys: hirsel_core_net::packet_sframe::SendKeys<Payment>,
}

impl crate::host_file::RunPart for RunState {
    fn hand_over(&self, payer: Payer) -> HandedOver {
        let (handed, refunds) = self.charges.hand_over(payer);
        drop(refunds);
        handed
    }
}

pub(crate) fn run_state(grants: &crate::host_file::RunGrants) -> &RunState {
    crate::host_file::run_part(&grants.media)
}

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

    fn hand_over(&self, payer: Payer) -> (HandedOver, Vec<Charge>) {
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
/// The uniquely owned handle contains T and moves to this call.
unsafe fn take<T>(handle: u64) -> T {
    unsafe { Box::from_raw(handle as *mut Slot<T>) }.value
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

/// # Safety
/// A recorded parent belongs to this running context and its synchronous tape.
unsafe fn issue_child(
    context: *mut Context,
    parent: u64,
    operation: u8,
    fields: &[[u8; 8]],
    site: u64,
) -> Result<u64, crate::provider::Outcome> {
    if parent == 0 {
        return Ok(0);
    }
    #[cfg(feature = "record")]
    {
        use crate::provider::{Buffer, IoError, Outcome};
        let parent = parent.to_le_bytes();
        let mut args: Vec<&[u8]> = vec![&parent];
        args.extend(fields.iter().map(|field| field.as_slice()));
        let mut empty = Buffer {
            data: std::ptr::null_mut(),
            len: 0,
            capacity: 0,
        };
        // SAFETY: the running context and all operands outlive this synchronous issue.
        let out = unsafe {
            capture::issue(
                context,
                operation,
                capture::arguments(operation, 0, &args),
                capture::Delivery::Child { refusals: &[] },
                &mut empty,
                |_| Outcome::default(),
                site,
            )
        };
        if out.status != 0 || out.error != IoError::OK {
            Err(out)
        } else {
            Ok(out.value)
        }
    }
    #[cfg(not(feature = "record"))]
    {
        let _ = (context, operation, fields, site);
        unreachable!("only recording creates a tape origin")
    }
}
