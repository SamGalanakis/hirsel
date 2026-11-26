//! Owned media state is pinned independently of the one-word resource carrier.
mod agreement;
use crate::Context;

#[cfg(feature = "record")]
pub(crate) mod capture;
mod import;
mod packet;
mod schedule;
mod sframe;
#[cfg(feature = "record")]
mod snapshot;
#[cfg(feature = "record")]
pub(crate) use snapshot::snapshot;
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

/// A key object's survivor kind, and an agreement or send-key registry entry's.
pub(crate) const KEY_OBJECT: u8 = 3;
pub(crate) const REGISTRY_ENTRY: u8 = 4;

impl Charges {
    /// Track `charge`, naming it as a survivor of `kind`.
    pub(crate) fn track(self: &Arc<Self>, charge: Charge, kind: u8) -> Payment {
        let cell = ChargeCell::made(charge, kind);
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

pub(crate) trait Resource: Send {
    #[cfg(feature = "record")]
    const TAG: u8;
    #[cfg(feature = "record")]
    fn origin(&mut self) -> &mut Origin;
    #[cfg(feature = "record")]
    fn write_snapshot(&self, body: &mut Vec<u8>);
}

#[cfg(not(feature = "record"))]
impl<T: Send> Resource for T {}

#[repr(C)]
struct Head {
    drop: unsafe fn(*mut u8),
    #[cfg(feature = "record")]
    snapshot: unsafe fn(u64, Option<u64>) -> snapshot::Part,
}

#[repr(C)]
struct Slot<T> {
    head: Head,
    value: T,
}

pub(crate) fn hold<T: Resource>(value: T) -> u64 {
    unsafe fn drop_slot<T>(slot: *mut u8) {
        // SAFETY: the owned word came from hold::<T> and is consumed once.
        drop(unsafe { Box::from_raw(slot.cast::<Slot<T>>()) });
    }
    Box::into_raw(Box::new(Slot {
        head: Head {
            drop: drop_slot::<T>,
            #[cfg(feature = "record")]
            snapshot: snapshot::write::<T>,
        },
        value,
    })) as u64
}

#[derive(Clone, Copy)]
pub(crate) struct Origin {
    #[cfg(feature = "record")]
    birth: Option<(u32, u64, u64)>,
    #[cfg(feature = "record")]
    reference: [u8; 9],
}

impl Origin {
    #[cfg(feature = "record")]
    const ARRIVAL: u8 = 3;
    pub(crate) fn new(delivery: u64, opaque: bool, payment: &Payment) -> Self {
        #[cfg(feature = "record")]
        {
            let birth = payment
                .cell
                .made
                .map(|(_, stream, admission)| (stream, delivery, admission));
            let mut reference = [0; 9];
            if opaque {
                reference[..8].copy_from_slice(&delivery.to_le_bytes());
            }
            Self { birth, reference }
        }
        #[cfg(not(feature = "record"))]
        {
            let _ = (delivery, opaque, payment);
            Self {}
        }
    }

    /// A key object that arrived in a replaying unit as its snapshot: its
    /// birth as written, and its new ordinal there as its operand.
    #[cfg(feature = "record")]
    pub(crate) fn arrived(birth: (u32, u64, u64), ordinal: u64) -> Self {
        let mut reference = [0; 9];
        reference[..8].copy_from_slice(&ordinal.to_le_bytes());
        reference[8] = Self::ARRIVAL;
        Self {
            birth: Some(birth),
            reference,
        }
    }

    #[cfg(feature = "record")]
    fn captured(&self) -> bool {
        self.reference != [0; 9]
    }

    #[cfg(feature = "record")]
    fn operand(&self) -> &[u8] {
        &self.reference[..8 + usize::from(self.reference[8] != 0)]
    }

    #[cfg(feature = "record")]
    pub(crate) fn arrival(&self) -> Option<u64> {
        (self.reference[8] == Self::ARRIVAL)
            .then(|| u64::from_le_bytes(self.reference[..8].try_into().unwrap()))
    }

    fn member(self, member: u8) -> Self {
        #[cfg(feature = "record")]
        {
            let mut origin = self;
            if origin.captured() {
                origin.reference[8] = member;
            }
            origin
        }
        #[cfg(not(feature = "record"))]
        {
            let _ = member;
            self
        }
    }
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
    let drop = unsafe { (*handle.cast::<Head>()).drop };
    unsafe { drop(handle) };
}

/// # Safety
/// A recorded parent belongs to this running context and its synchronous tape.
unsafe fn issue_child(
    context: *mut Context,
    parent: Origin,
    operation: u8,
    fields: &[[u8; 8]],
    site: u64,
) -> Result<u64, crate::provider::Outcome> {
    #[cfg(feature = "record")]
    {
        if !parent.captured() {
            return Ok(0);
        }
        use crate::provider::{Buffer, IoError, Outcome};
        let mut args: Vec<&[u8]> = vec![parent.operand()];
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
        let _ = (context, parent, operation, fields, site);
        Ok(0)
    }
}
