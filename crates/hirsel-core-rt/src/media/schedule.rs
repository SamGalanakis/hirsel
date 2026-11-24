use crate::{
    Context, media,
    provider::{Buffer, IoError, Outcome},
    secret::Charge,
};
use hirsel_core_net::{
    packet_cipher::HeaderMask,
    packet_epoch::{DOMAIN_CHARGE, Epoch, IssueError, SCHEDULE_CHARGE},
    packet_nonce::{ReceiverNonce, SenderCounter},
    secret_aead::AeadAlg,
    tls_kdf::Side,
};

const INITIAL: u64 = 0;
type Paid = media::Payment;

pub(super) struct Schedule {
    initial: Epoch<Paid>,
    _charge: Paid,
}

pub(super) struct Sealer {
    pub(super) counter: SenderCounter,
    pub(super) algorithm: AeadAlg,
    pub(super) cipher: crate::os_tls::packet_cipher::PacketCipher,
    _charge: Paid,
}

pub(super) struct Opener {
    pub(super) nonce: ReceiverNonce,
    pub(super) algorithm: AeadAlg,
    pub(super) cipher: crate::os_tls::packet_cipher::PacketCipher,
    _charge: Paid,
}

pub(super) struct Mask {
    pub(super) key: HeaderMask,
    _charge: Paid,
}

fn refusal(error: IssueError) -> Outcome {
    let (tag, value, written) = match error {
        IssueError::Invalid => (2, 0, 0),
        IssueError::Issued => (3, 0, 0),
        IssueError::Spent => (4, 0, 0),
        IssueError::Capacity {
            requested,
            available,
        } => (5, requested, available),
    };
    Outcome {
        error: IoError { tag, code: 0 },
        value,
        written,
        ..Outcome::default()
    }
}

pub(super) fn admit(context: &Context, requested: u64) -> Result<Charge, u64> {
    let payer = context.payer();
    payer.admit(Some(requested)).map_err(|c| c.available)?;
    Ok(Charge::new(payer, requested))
}

fn track(context: &Context, charge: Charge) -> Paid {
    // SAFETY: the caller entered with a running context.
    unsafe {
        (*context.host_files)
            .registry
            .run_grants()
            .media_charges
            .track(charge)
    }
}

fn owned<T: Send>(value: T) -> Outcome {
    Outcome {
        value: media::hold(value),
        ..Outcome::default()
    }
}

/// # Safety
/// Context is running and the borrowed dcid is pinned through this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_quic_initial(
    context: *mut Context,
    side: u64,
    dcid: *const Buffer,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded from the checked call.
    unsafe {
        crate::process::deny_describe_capability(context);
        let dcid = crate::tls::bytes(&*dcid);
        if dcid.len() > 20 {
            return refusal(IssueError::Invalid);
        }
        let charge = match admit(&*context, SCHEDULE_CHARGE) {
            Ok(charge) => track(&*context, charge),
            Err(available) => {
                return refusal(IssueError::Capacity {
                    requested: SCHEDULE_CHARGE,
                    available,
                });
            }
        };
        let side = if side == 0 {
            Side::Client
        } else {
            Side::Server
        };
        let initial = Epoch::quic_initial(side, dcid).unwrap();
        owned(Schedule {
            initial,
            _charge: charge,
        })
    }
}

/// # Safety
/// The schedule is live and exclusively borrowed for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_sealer(
    context: *mut Context,
    schedule: u64,
    space: u64,
    domain: u64,
    floor: u64,
    _site: u64,
) -> Outcome {
    if space != INITIAL {
        return refusal(IssueError::Invalid);
    }
    // SAFETY: the checked caller lends this schedule exclusively.
    let schedule = unsafe { media::loan_mut::<Schedule>(schedule) };
    let admit = |requested| {
        // SAFETY: the context remains live through this synchronous operation.
        let mut charge = admit(unsafe { &*context }, requested)?;
        let ledger = track(unsafe { &*context }, charge.partition(DOMAIN_CHARGE));
        Ok((track(unsafe { &*context }, charge), ledger))
    };
    match schedule.initial.sealer(domain, floor, admit) {
        Ok(issued) => owned(Sealer {
            counter: issued.counter,
            algorithm: issued.algorithm,
            cipher: crate::os_tls::packet_cipher::PacketCipher::new(
                issued.algorithm,
                issued.key.unwrap(),
            )
            .unwrap(),
            _charge: issued.charge,
        }),
        Err(error) => refusal(error),
    }
}

/// # Safety
/// The schedule is live and borrowed for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_opener(
    context: *mut Context,
    schedule: u64,
    space: u64,
    _site: u64,
) -> Outcome {
    if space != INITIAL {
        return refusal(IssueError::Invalid);
    }
    // SAFETY: the checked caller lends this schedule, and context remains running.
    let schedule = unsafe { media::loan::<Schedule>(schedule) };
    match schedule.initial.opener(|requested| {
        admit(unsafe { &*context }, requested).map(|charge| track(unsafe { &*context }, charge))
    }) {
        Ok(issued) => owned(Opener {
            nonce: issued.nonce,
            algorithm: issued.algorithm,
            cipher: crate::os_tls::packet_cipher::PacketCipher::new(
                issued.algorithm,
                issued.key.unwrap(),
            )
            .unwrap(),
            _charge: issued.charge,
        }),
        Err(error) => refusal(error),
    }
}

/// # Safety
/// The schedule is live and borrowed for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_header_mask(
    context: *mut Context,
    schedule: u64,
    space: u64,
    local: bool,
    _site: u64,
) -> Outcome {
    if space != INITIAL {
        return refusal(IssueError::Invalid);
    }
    // SAFETY: the checked caller lends this schedule, and context remains running.
    let schedule = unsafe { media::loan::<Schedule>(schedule) };
    match schedule.initial.header_mask(local, |n| {
        admit(unsafe { &*context }, n).map(|charge| track(unsafe { &*context }, charge))
    }) {
        Ok((key, charge)) => owned(Mask {
            key: key.unwrap(),
            _charge: charge,
        }),
        Err(error) => refusal(error),
    }
}
