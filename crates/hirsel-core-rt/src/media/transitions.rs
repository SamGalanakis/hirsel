//! TLS schedule transitions and Finished use already admitted schedule storage.
use super::{
    loan, loan_mut,
    schedule::{Schedule, refusal},
};
#[cfg(feature = "record")]
use crate::provider::IoError;
use crate::{
    Context,
    provider::{Buffer, Outcome},
};
use hirsel_core_net::{packet_epoch::IssueError, tls_kdf::Side};

#[cfg(feature = "record")]
const APPLICATION: u8 = 11;
#[cfg(feature = "record")]
const UPDATE: u8 = 12;
#[cfg(feature = "record")]
const RETIRE: u8 = 13;
#[cfg(feature = "record")]
const FINISHED: u8 = 14;
#[cfg(feature = "record")]
const VERIFY: u8 = 15;

fn sender(of: u64) -> Side {
    if of == 0 { Side::Client } else { Side::Server }
}

#[cfg(feature = "record")]
unsafe fn record_transition(
    context: *mut Context,
    origin: u64,
    operation: u8,
    operand: &[u8],
    site: u64,
) -> Outcome {
    use super::capture::{self, Delivery};
    let mut output = Buffer {
        data: std::ptr::null_mut(),
        len: 0,
        capacity: 0,
    };
    let args = capture::arguments(operation, 0, &[&origin.to_le_bytes(), operand]);
    // SAFETY: the context and frame operands remain live throughout the call.
    unsafe {
        capture::issue(
            context,
            operation,
            args,
            Delivery::Scalar { max: 0 },
            &mut output,
            |_| Outcome::default(),
            site,
        )
    }
}

/// # Safety
/// The checked caller lends its schedule exclusively and pins the hash carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_application(
    context: *mut Context,
    handle: u64,
    hash: *const Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: the checked caller pins both operands through the call.
    let (schedule, hash) = unsafe { (loan_mut::<Schedule>(handle), crate::tls::bytes(&*hash)) };
    if let Err(error) = schedule.state.check_application(hash) {
        return refusal(error);
    }
    #[cfg(feature = "record")]
    if schedule.origin != 0 {
        let answer =
            unsafe { record_transition(context, schedule.origin, APPLICATION, hash, site) };
        if answer.status != 0 || answer.error != IoError::OK {
            return answer;
        }
    }
    let _ = (context, site);
    schedule.state.application(hash);
    Outcome::default()
}

/// # Safety
/// The checked caller lends its schedule exclusively through the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_key_update(
    context: *mut Context,
    handle: u64,
    local: bool,
    site: u64,
) -> Outcome {
    // SAFETY: the caller holds the unique schedule loan.
    let schedule = unsafe { loan_mut::<Schedule>(handle) };
    if let Err(error) = schedule.state.check_update(local) {
        return refusal(error);
    }
    #[cfg(feature = "record")]
    if schedule.origin != 0 {
        let local_wire = [u8::from(local)];
        let answer =
            unsafe { record_transition(context, schedule.origin, UPDATE, &local_wire, site) };
        if answer.status != 0 || answer.error != IoError::OK {
            return answer;
        }
    }
    let _ = (context, site);
    drop(schedule.state.key_update(local));
    Outcome::default()
}

/// # Safety
/// The checked caller lends its schedule exclusively through the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_retire(
    context: *mut Context,
    handle: u64,
    space: u64,
    site: u64,
) -> Outcome {
    // SAFETY: the caller holds the unique schedule loan.
    let schedule = unsafe { loan_mut::<Schedule>(handle) };
    if let Err(error) = schedule.state.check_retire(space) {
        return refusal(error);
    }
    #[cfg(feature = "record")]
    if schedule.origin != 0 {
        let space_wire = space.to_le_bytes();
        let answer =
            unsafe { record_transition(context, schedule.origin, RETIRE, &space_wire, site) };
        if answer.status != 0 || answer.error != IoError::OK {
            return answer;
        }
    }
    let _ = (context, site);
    drop(schedule.state.retire());
    Outcome::default()
}

/// # Safety
/// Operands remain live through the call with an exclusive output spare.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_finished(
    context: *mut Context,
    handle: u64,
    of: u64,
    hash: *const Buffer,
    out: *mut Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: the compiler lends shared inputs and a disjoint exclusive output.
    let (schedule, hash, out) = unsafe {
        (
            loan::<Schedule>(handle),
            crate::tls::bytes(&*hash),
            &mut *out,
        )
    };
    let size = match schedule.state.finished_size(hash) {
        Ok(size) => size,
        Err(error) => return refusal(error),
    };
    let spare = out.capacity - out.len;
    if spare < size {
        return refusal(IssueError::Capacity {
            requested: size as u64,
            available: spare as u64,
        });
    }
    let compute = |out: &mut Buffer| {
        let mut bytes = [0u8; 48];
        let bytes = &mut bytes[..size];
        schedule.state.finished(sender(of), hash, bytes);
        // SAFETY: admission proved the Finished value fits in the exclusive spare.
        unsafe { crate::secret::key_share::append(out, bytes) };
        Outcome {
            value: size as u64,
            ..Outcome::default()
        }
    };
    #[cfg(feature = "record")]
    if schedule.origin != 0 {
        use super::capture::{self, Delivery};
        let args = capture::arguments(
            FINISHED,
            spare as u64,
            &[&schedule.origin.to_le_bytes(), &of.to_le_bytes(), hash],
        );
        // SAFETY: the borrowed output carrier and all inputs stay live through delivery.
        return unsafe {
            capture::issue(
                context,
                FINISHED,
                args,
                Delivery::Packet {
                    value: size as u64,
                    bytes: size,
                    tampered: None,
                },
                out,
                compute,
                site,
            )
        };
    }
    let _ = (context, site);
    compute(out)
}

/// # Safety
/// The context, schedule and public inputs are live for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_verify_finished(
    context: *mut Context,
    handle: u64,
    of: u64,
    hash: *const Buffer,
    received: *const Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: all operands are pinned shared loans.
    let (schedule, hash, received) = unsafe {
        (
            loan::<Schedule>(handle),
            crate::tls::bytes(&*hash),
            crate::tls::bytes(&*received),
        )
    };
    let size = match schedule.state.finished_size(hash) {
        Ok(size) => size,
        Err(error) => return refusal(error),
    };
    let compute = || Outcome {
        value: u64::from(
            schedule
                .state
                .verify_finished(sender(of), hash, received, size),
        ),
        ..Outcome::default()
    };
    #[cfg(feature = "record")]
    if schedule.origin != 0 {
        use super::capture::{self, Delivery};
        let args = capture::arguments(
            VERIFY,
            0,
            &[
                &schedule.origin.to_le_bytes(),
                &of.to_le_bytes(),
                hash,
                received,
            ],
        );
        let mut output = Buffer {
            data: std::ptr::null_mut(),
            len: 0,
            capacity: 0,
        };
        // SAFETY: the adapter pins the public operands through the verdict.
        return unsafe {
            capture::issue(
                context,
                VERIFY,
                args,
                Delivery::Scalar { max: 1 },
                &mut output,
                |_| compute(),
                site,
            )
        };
    }
    let _ = (context, site);
    compute()
}
