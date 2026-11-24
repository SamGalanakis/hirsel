//! A KeyShare carries its registry ticket until drop or the constructor's verdict.
use super::*;
use crate::provider::{CANCELLED, MEDIA_KEY_SHARE};
use hirsel_core_net::{
    agreement_registry::ShareTicket,
    key_agreement::{Group, KeyShare},
};

struct Share {
    _key: Option<KeyShare>,
    _ticket: ShareTicket<Arc<ChargeCell>>,
    _origin: (u8, u64),
    _charge: Arc<ChargeCell>,
}

pub(crate) struct Generation {
    pub(crate) random: u64,
    pub(crate) group: Group,
    key: Option<KeyShare>,
}

pub(crate) const PROVIDER: Provider = Provider {
    issue,
    ..MAC_PROVIDER
};

unsafe extern "C" fn issue(_: *mut u8, request: *mut Request, _: u64) -> Step {
    // SAFETY: the entry pins the exclusive request and output through delivery.
    let request = unsafe { &mut *request };
    let call = unsafe { &mut *request.path.cast_mut().cast::<Generation>() };
    let mut point = [0; 65];
    let point = &mut point[..call.group.public_len()];
    call.key = match KeyShare::host(call.group, point) {
        Ok(key) => Some(key),
        Err(_) => crate::process::stop(b"secret.key_share: OS key generation: ring Unspecified"),
    };
    // SAFETY: admission proved the point fits in the exclusive spare.
    unsafe { append(&mut request.buffer, point) };
    Step {
        kind: STEP_DONE,
        outcome: Outcome::default(),
        deadline_ns: 0,
    }
}

/// # Safety
/// The caller pins an exclusive carrier with enough spare for the exact point.
pub(crate) unsafe fn append(public: &mut Buffer, point: &[u8]) {
    unsafe {
        point
            .as_ptr()
            .copy_to_nonoverlapping(public.data.add(public.len), point.len())
    };
    public.len += point.len();
}

/// # Safety
/// Context is running and public lends its pinned spare exclusively.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_key_share(
    context: *mut Context,
    random: u64,
    group: u64,
    public: *mut Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded from the checked call.
    unsafe {
        crate::process::deny_describe_capability(context);
        let group = Group::from_case(group).expect("a checked Group");
        let spare = (*public).capacity - (*public).len;
        if spare < group.public_len() {
            return key::key_capacity(group.public_len(), spare);
        }
        let payer = (*context).payer();
        const BYTES: u64 = 64;
        if let Err(refused) = payer.admit(Some(BYTES)) {
            return key::key_capacity(BYTES as usize, refused.available as usize);
        }
        let charge = ChargeCell::new(Charge(payer, BYTES));
        let mut call = Generation {
            random,
            group,
            key: None,
        };
        let provider = &*crate::entry::hirsel_core_capability_provider(context, random);
        let (origin, buffer) = if crate::provide::owns(provider) {
            let mut point = [0; 65];
            let point = &mut point[..group.public_len()];
            call.key = match KeyShare::from_draws(
                group,
                |draw| crate::provide::random::fill_virtual(provider, draw),
                point,
            ) {
                Ok(key) => Some(key),
                Err(()) => {
                    drop(charge);
                    use crate::causes::{self, Cause};
                    causes::record(&mut *context, Cause::new(causes::BOUNDS, site));
                    return CANCELLED;
                }
            };
            let mut buffer = *public;
            append(&mut buffer, point);
            (None, buffer)
        } else {
            let mut request = Request {
                operation: MEDIA_KEY_SHARE,
                mode: 0,
                handle: 0,
                path: (&raw mut call).cast(),
                path_len: size_of::<Generation>(),
                buffer: *public,
                site,
            };
            let outcome = drive_secret_sink(context, PROVIDER, &mut request);
            if outcome.status != 0 || outcome.error != IoError::OK {
                return outcome;
            }
            (Some(outcome.value), request.buffer)
        };
        let registry = &(*(*context).host_files).registry.run_grants().agreements;
        let ticket = registry
            .new_share()
            .unwrap_or_else(|| crate::process::stop(b"key share count overflow"));
        let origin = origin.map_or_else(|| (0, ticket.ordinal()), |ordinal| (1, ordinal));
        (*public).len = buffer.len;
        let value = crate::media::hold(Share {
            _key: call.key,
            _ticket: ticket,
            _origin: origin,
            _charge: charge,
        });
        Outcome {
            value,
            ..Outcome::default()
        }
    }
}
