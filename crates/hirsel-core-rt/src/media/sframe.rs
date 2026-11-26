//! TLS-8 SFrame schedules: per-KID sealers and openers of an imported base key.
use super::{
    Origin, loan, loan_mut,
    schedule::{Opener, Paid, Schedule, Sealer, admit, owned, refusal, track},
};
use crate::{Context, provider::Outcome};
use hirsel_core_net::{
    packet_epoch::{CIPHER_CHARGE, DOMAIN_CHARGE, IssueError},
    packet_protection::{self, Algorithm, Key},
    packet_sframe::Sframe,
    secret_aead::AeadAlg,
    tls_kdf::Tls13PacketKeys,
};

const SEALER: u8 = 21;
const OPENER: u8 = 22;
const ALGORITHM: Algorithm = Algorithm::Aead(AeadAlg::Aes128Gcm);
type Cipher = packet_protection::Cipher<crate::os_tls::packet_cipher::PacketCipher>;

fn cipher(keys: Option<Tls13PacketKeys>) -> Option<Cipher> {
    keys.map(|keys| Cipher::new(ALGORITHM, Key::Aead(keys.payload())).unwrap())
}

/// # Safety
/// The schedule is live and exclusively borrowed for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_sframe_sealer(
    context: *mut Context,
    schedule: u64,
    steps: u64,
    kid: u64,
    floor: u64,
    site: u64,
) -> Outcome {
    // SAFETY: the checked caller lends this schedule exclusively.
    let schedule = unsafe { loan_mut::<Schedule>(schedule) };
    let parent = schedule.origin;
    let sframe = match schedule.state.sframe_mut() {
        Ok(sframe) => sframe,
        Err(error) => return refusal(error),
    };
    if let Err(error) = sframe.check_sealer(steps, kid) {
        return refusal(error);
    }
    let requested = Sframe::<Paid>::SEALER_CHARGE;
    // SAFETY: the context remains live through this synchronous operation.
    let mut charge = match admit(unsafe { &*context }, requested) {
        Ok(charge) => charge,
        Err(available) => {
            return refusal(IssueError::Capacity {
                requested,
                available,
            });
        }
    };
    let entry = charge.partition(DOMAIN_CHARGE);
    let fields = [steps.to_le_bytes(), kid.to_le_bytes(), floor.to_le_bytes()];
    // SAFETY: the recorded issue uses only pinned public arguments through delivery.
    let origin = match unsafe { super::issue_child(context, parent, SEALER, &fields, site) } {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let paid = (
        track(unsafe { &*context }, charge),
        track(unsafe { &*context }, entry),
    );
    let issued = sframe.sealer(steps, kid, floor, paid);
    drop(issued.retired);
    #[cfg(feature = "record")]
    schedule.advance_generation(true, steps);
    owned(Sealer {
        counter: issued.counter,
        algorithm: ALGORITHM,
        cipher: cipher(issued.keys),
        origin: Origin::new(origin, true, &issued.charge),
        #[cfg(feature = "record")]
        space: 2,
        #[cfg(feature = "record")]
        profile: schedule.profile,
        #[cfg(feature = "record")]
        sframe_kid: Some(kid),
        _charge: issued.charge,
    })
}

/// # Safety
/// The schedule is live and borrowed for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_sframe_opener(
    context: *mut Context,
    schedule: u64,
    ahead: u64,
    kid: u64,
    site: u64,
) -> Outcome {
    // SAFETY: the checked caller lends this schedule, and context remains running.
    let schedule = unsafe { loan::<Schedule>(schedule) };
    let sframe = match schedule.state.sframe() {
        Ok(sframe) => sframe,
        Err(error) => return refusal(error),
    };
    if let Err(error) = sframe.check_opener(ahead, kid) {
        return refusal(error);
    }
    let charge = match admit(unsafe { &*context }, CIPHER_CHARGE) {
        Ok(charge) => charge,
        Err(available) => {
            return refusal(IssueError::Capacity {
                requested: CIPHER_CHARGE,
                available,
            });
        }
    };
    let fields = [ahead.to_le_bytes(), kid.to_le_bytes()];
    // SAFETY: the public operands remain live through the issue.
    let origin =
        match unsafe { super::issue_child(context, schedule.origin, OPENER, &fields, site) } {
            Ok(origin) => origin,
            Err(outcome) => return outcome,
        };
    let issued = sframe.opener(ahead, kid, track(unsafe { &*context }, charge));
    owned(Opener {
        nonce: issued.nonce,
        algorithm: ALGORITHM,
        cipher: cipher(issued.keys),
        origin: Origin::new(origin, true, &issued.charge),
        #[cfg(feature = "record")]
        space: 2,
        #[cfg(feature = "record")]
        profile: schedule.profile,
        _charge: issued.charge,
    })
}
