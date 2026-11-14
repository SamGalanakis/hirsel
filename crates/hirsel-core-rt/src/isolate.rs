//! FORM #101 / FAIL-16 isolate reservation and cause-cell transfer.
use crate::{
    Context,
    causes::{self, Causes, Cell},
};

pub const ISOLATE_CELL: u64 = 32;

pub struct Reservation {
    pub(crate) cell: Option<Box<Cell>>,
    pub(crate) saved: Causes,
    pub(crate) obligation: *mut causes::Resource,
    pub(crate) used: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct IsolateEnter {
    pub entered: bool,
    pub available: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct IsolateCatch {
    pub causes: *mut causes::Cell,
    pub status: u8,
}

/// Reserve the isolate's cause cell. Refusal is a value and leaves the context
/// and cause chain unchanged.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_isolate_enter(context: *mut Context) -> IsolateEnter {
    // SAFETY: the engine owns this task context exclusively at every runtime row.
    unsafe {
        let context = &mut *context;
        context.mark_startup_primary();
        let available = context.available();
        let payer = match context.current.isolate_reserve(ISOLATE_CELL) {
            Ok(payer) => payer,
            Err(_) => {
                return IsolateEnter {
                    entered: false,
                    available,
                };
            }
        };
        let reservation = Reservation {
            cell: Some(Cell::reserved(payer)),
            saved: std::mem::take(&mut context.causes),
            obligation: context.active_resource,
            used: false,
        };
        context.isolates.push(reservation);
        IsolateEnter {
            entered: true,
            available: 0,
        }
    }
}

/// Leave a returned isolate. Its unused reserved cell is refunded before the
/// scope is handed back to its parent and its saved chain is restored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_isolate_leave(context: *mut Context) {
    // SAFETY: the producer closes the top reservation only on a normal body return.
    unsafe {
        let context = &mut *context;
        let reservation = context.isolates.pop().expect("isolate leave without enter");
        drop(reservation.cell);
        context.causes = reservation.saved;
    }
}

/// Catch a failed isolate body and transfer its reservation cell according to
/// FAIL-16. Cancellation is passed through with the saved chain prepended.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_isolate_catch(context: *mut Context) -> IsolateCatch {
    // SAFETY: the handler is entered only after the body's cleanup completed.
    unsafe {
        let context = &mut *context;
        let mut reservation = context.isolates.pop().expect("isolate catch without enter");
        let caught = std::mem::take(&mut context.causes);
        let cancelled = caught
            .iter()
            .next()
            .is_some_and(|cause| cause.kind() == causes::CANCELLED);
        if cancelled {
            drop(reservation.cell);
            let mut restored = reservation.saved;
            restored.append_chain(caught);
            context.causes = restored;
            return IsolateCatch {
                causes: std::ptr::null_mut(),
                status: 1,
            };
        }

        let used = reservation.used;
        if used {
            let cell = reservation
                .cell
                .take()
                .expect("used isolate without reservation cell");
            if context.active_resource.is_null() {
                context.primary = Some(cell);
            } else {
                (*context.active_resource).cell = Some(cell);
            }
        } else {
            drop(reservation.cell);
        }
        context.causes = reservation.saved;
        IsolateCatch {
            causes: caught.into_raw(),
            status: 0,
        }
    }
}
