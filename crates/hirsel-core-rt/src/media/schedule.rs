use crate::{
    Context, media,
    provider::{Buffer, IoError, Outcome},
    secret::Charge,
};
use hirsel_core_net::{
    packet_cipher::HeaderMask,
    packet_epoch::{CIPHER_CHARGE, DOMAIN_CHARGE, Epoch, IssueError, SCHEDULE_CHARGE},
    packet_nonce::{ReceiverNonce, SenderCounter},
    packet_protection::{self, Algorithm},
    packet_schedule,
    tls_kdf::Side,
};

const INITIAL: usize = 0;
pub(super) type Paid = media::Payment;
type Cipher = packet_protection::Cipher<crate::os_tls::packet_cipher::PacketCipher>;

pub(super) struct Schedule {
    pub(super) state: packet_schedule::Schedule<Paid>,
    pub(super) origin: u64,
    pub(super) _charge: Paid,
}

pub(super) struct Sealer {
    pub(super) counter: SenderCounter,
    pub(super) algorithm: Algorithm,
    pub(super) cipher: Option<Cipher>,
    pub(super) origin: u64,
    #[cfg(feature = "record")]
    pub(super) origin_member: u8,
    _charge: Paid,
}

pub(super) struct Opener {
    pub(super) nonce: ReceiverNonce,
    pub(super) algorithm: Algorithm,
    pub(super) cipher: Option<Cipher>,
    pub(super) origin: u64,
    _charge: Paid,
}

pub(super) struct Mask {
    pub(super) key: Option<HeaderMask>,
    pub(super) origin: u64,
    _charge: Paid,
}

pub(super) fn refusal(error: IssueError) -> Outcome {
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

pub(super) fn before_issue(error: IssueError) -> Outcome {
    let mut outcome = refusal(error);
    outcome.error.code = 1; // Custody returned before issue.
    outcome
}

pub(super) fn admit(context: &Context, requested: u64) -> Result<Charge, u64> {
    let payer = context.payer();
    payer.admit(Some(requested)).map_err(|c| c.available)?;
    Ok(Charge::new(payer, requested))
}

pub(super) fn track(context: &Context, charge: Charge) -> Paid {
    // SAFETY: the caller entered with a running context.
    unsafe {
        super::run_state((*context.host_files).registry.run_grants())
            .charges
            .track(charge)
    }
}

pub(super) fn owned<T: Send>(value: T) -> Outcome {
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
            state: packet_schedule::Schedule::single(INITIAL, initial),
            origin: 0,
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
    site: u64,
) -> Outcome {
    // SAFETY: the checked caller lends this schedule exclusively.
    let schedule = unsafe { media::loan_mut::<Schedule>(schedule) };
    let parent = schedule.origin;
    let epoch = match schedule.state.epoch(space) {
        Ok(epoch) => epoch,
        Err(error) => return refusal(error),
    };
    let admit = |requested| {
        // SAFETY: the context remains live through this synchronous operation.
        let mut charge = admit(unsafe { &*context }, requested)?;
        let ledger = track(unsafe { &*context }, charge.partition(DOMAIN_CHARGE));
        Ok((track(unsafe { &*context }, charge), ledger))
    };
    match epoch.sealer(domain, floor, admit) {
        Ok(issued) => {
            let fields = [
                space.to_le_bytes(),
                domain.to_le_bytes(),
                floor.to_le_bytes(),
            ];
            // SAFETY: the recorded issue uses only pinned public arguments through delivery.
            let origin = match unsafe { media::issue_child(context, parent, 3, &fields, site) } {
                Ok(origin) => origin,
                Err(outcome) => return outcome,
            };
            owned(Sealer {
                counter: issued.counter,
                algorithm: issued.algorithm,
                cipher: issued
                    .key
                    .map(|key| Cipher::new(issued.algorithm, key).unwrap()),
                origin,
                #[cfg(feature = "record")]
                origin_member: 0,
                _charge: issued.charge,
            })
        }
        Err(error) => refusal(error),
    }
}

/// # Safety
/// The schedule is live and borrowed for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_space_info(
    _context: *mut Context,
    schedule: u64,
    space: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: checked lowering supplies the borrowed schedule.
    let schedule = unsafe { media::loan::<Schedule>(schedule) };
    match schedule.state.view_epoch(space).and_then(Epoch::info) {
        Ok((value, written)) => Outcome {
            value,
            written,
            ..Outcome::default()
        },
        Err(error) => refusal(error),
    }
}

/// # Safety
/// The context is live and the schedule is exclusively borrowed for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_sealer_pair(
    context: *mut Context,
    schedule: u64,
    first: u64,
    second: u64,
    domain: u64,
    first_floor: u64,
    second_floor: u64,
    site: u64,
) -> Outcome {
    // SAFETY: checked lowering supplies the mutable schedule and running context.
    let schedule = unsafe { media::loan_mut::<Schedule>(schedule) };
    let pair = match schedule.state.check_sealer_pair(first, second, domain) {
        Ok(pair) => pair,
        Err(error) => return refusal(error),
    };
    let requested = 2 * (CIPHER_CHARGE + DOMAIN_CHARGE);
    let mut charge = match admit(unsafe { &*context }, requested) {
        Ok(charge) => charge,
        Err(available) => {
            return refusal(IssueError::Capacity {
                requested,
                available,
            });
        }
    };
    let fields = [first, second, domain, first_floor, second_floor].map(u64::to_le_bytes);
    const SEALER_PAIR: u8 = 18;
    // SAFETY: the running context and pinned fields remain live through delivery.
    let origin =
        match unsafe { media::issue_child(context, schedule.origin, SEALER_PAIR, &fields, site) } {
            Ok(origin) => origin,
            Err(outcome) => return outcome,
        };
    let charges = std::array::from_fn(|_| {
        // SAFETY: the running context owns the admitted charge.
        let context = unsafe { &*context };
        let cipher = track(context, charge.partition(CIPHER_CHARGE));
        let ledger = track(context, charge.partition(DOMAIN_CHARGE));
        (cipher, ledger)
    });
    let [first, second] = pair.issue([first_floor, second_floor], charges);
    let hold = |issued: hirsel_core_net::packet_epoch::Sealer<'_, Paid>, _member| {
        media::hold(Sealer {
            counter: issued.counter,
            algorithm: issued.algorithm,
            cipher: issued
                .key
                .map(|key| Cipher::new(issued.algorithm, key).unwrap()),
            origin,
            #[cfg(feature = "record")]
            origin_member: _member,
            _charge: issued.charge,
        })
    };
    Outcome {
        value: hold(first, 1),
        written: hold(second, 2),
        ..Outcome::default()
    }
}

/// # Safety
/// The schedule is live and borrowed for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_opener(
    context: *mut Context,
    schedule: u64,
    space: u64,
    site: u64,
) -> Outcome {
    // SAFETY: the checked caller lends this schedule, and context remains running.
    let schedule = unsafe { media::loan::<Schedule>(schedule) };
    let parent = schedule.origin;
    let epoch = match schedule.state.view_epoch(space) {
        Ok(epoch) => epoch,
        Err(error) => return refusal(error),
    };
    match epoch.opener(|requested| {
        admit(unsafe { &*context }, requested).map(|charge| track(unsafe { &*context }, charge))
    }) {
        Ok(issued) => {
            // SAFETY: the public space and result remain live through the issue.
            let origin = match unsafe {
                media::issue_child(context, parent, 4, &[space.to_le_bytes()], site)
            } {
                Ok(origin) => origin,
                Err(outcome) => return outcome,
            };
            owned(Opener {
                nonce: issued.nonce,
                algorithm: issued.algorithm,
                cipher: issued
                    .key
                    .map(|key| Cipher::new(issued.algorithm, key).unwrap()),
                origin,
                _charge: issued.charge,
            })
        }
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
    site: u64,
) -> Outcome {
    // SAFETY: the checked caller lends this schedule, and context remains running.
    let schedule = unsafe { media::loan::<Schedule>(schedule) };
    let parent = schedule.origin;
    let epoch = match schedule.state.view_epoch(space) {
        Ok(epoch) => epoch,
        Err(error) => return refusal(error),
    };
    match epoch.header_mask(local, |n| {
        admit(unsafe { &*context }, n).map(|charge| track(unsafe { &*context }, charge))
    }) {
        Ok((key, charge)) => {
            let fields = [space.to_le_bytes(), u64::from(local).to_le_bytes()];
            // SAFETY: the public operands remain pinned through the issue.
            let origin = match unsafe { media::issue_child(context, parent, 5, &fields, site) } {
                Ok(origin) => origin,
                Err(outcome) => return outcome,
            };
            owned(Mask {
                key,
                origin,
                _charge: charge,
            })
        }
        Err(error) => refusal(error),
    }
}
