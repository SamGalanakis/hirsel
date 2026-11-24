//! Registry verdicts are host inputs even if the agreement uses virtual keys.
use super::{MAC_PROVIDER, drive_secret_sink};
use crate::{Context, provider::*};
use hirsel_core_net::{
    agreement_registry::{RegistryVerdict, ShareTicket},
    tls_kdf::Side,
};
use zeroize::Zeroizing;

const INVALID: u64 = 0;
const ACCEPTED: u64 = 1;
const ISSUED: u64 = 2;

pub(crate) struct Call<'a> {
    pub(crate) canonical: &'a [u8],
    live: &'a mut dyn FnMut() -> u64,
}

pub(crate) const AGREEMENT_PROVIDER: Provider = Provider {
    issue,
    ..MAC_PROVIDER
};

unsafe extern "C" fn issue(_: *mut u8, request: *mut Request, _: u64) -> Step {
    // SAFETY: decide pins the exclusive Call through the synchronous issue.
    let call = unsafe { &mut *(*request).path.cast_mut().cast::<Call<'_>>() };
    Step {
        kind: STEP_DONE,
        outcome: Outcome {
            value: (call.live)(),
            ..Outcome::default()
        },
        deadline_ns: 0,
    }
}

/// The registry charge and ticket remain in pending until issue. A replay
/// consumes a recorded verdict and applies its charge without invoking agree.
///
/// # Safety
/// Context is the running task. The caller pins the public constructor
/// operands encoded by canonical and the share's admission through delivery.
pub unsafe fn decide<C>(
    context: *mut Context,
    canonical: &[u8],
    side: Side,
    pending: &mut Option<(ShareTicket<C>, C)>,
    mut agree: impl FnMut() -> Option<Zeroizing<[u8; 32]>>,
    site: u64,
) -> Outcome {
    let mut live = || {
        let (ticket, charge) = pending.take().unwrap();
        let Some(shared) = agree() else {
            return INVALID;
        };
        match ticket.resolve(side, &shared, charge) {
            RegistryVerdict::Accepted => ACCEPTED,
            RegistryVerdict::Issued => ISSUED,
        }
    };
    let mut call = Call {
        canonical,
        live: &mut live,
    };
    let mut request = Request {
        operation: MEDIA_AGREEMENT,
        mode: 0,
        handle: 0,
        path: (&raw mut call).cast(),
        path_len: std::mem::size_of::<Call<'_>>(),
        buffer: Buffer {
            data: std::ptr::null_mut(),
            len: 0,
            capacity: 0,
        },
        site,
    };
    // SAFETY: the program pins this call and no provider may retain it.
    let result = unsafe { drive_secret_sink(context, AGREEMENT_PROVIDER, &mut request) };
    if result.status == 0
        && let Some((ticket, charge)) = pending.take()
    {
        let verdict = match result.value {
            INVALID => return result,
            ACCEPTED => RegistryVerdict::Accepted,
            ISSUED => RegistryVerdict::Issued,
            _ => unreachable!("the tape decoder validates the verdict"),
        };
        ticket.replay(side, verdict, charge);
    }
    result
}
