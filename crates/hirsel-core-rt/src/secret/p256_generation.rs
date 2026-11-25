//! The host provider publishes a designation and point. Only the Secret table
//! retains the private document, and the replay provider never creates one.
use super::{key::*, *};
use crate::provider::{CANCELLED, MEDIA_P256_KEY};
use hirsel_core_net::p256_key::{self, DOCUMENT_LEN, PUBLIC_LEN};

pub(crate) struct Generation {
    pub(crate) random: u64,
    /// The canonical policy text, which the tape records as the call's uses.
    pub(crate) policy: String,
    pub(in crate::secret) document: Option<Zeroizing<[u8; DOCUMENT_LEN]>>,
}

pub(crate) const PROVIDER: Provider = Provider {
    issue,
    ..MAC_PROVIDER
};

unsafe extern "C" fn issue(_: *mut u8, request: *mut Request, _: u64) -> Step {
    // SAFETY: the entry pins the exclusive request and output through delivery.
    let request = unsafe { &mut *request };
    let call = unsafe { &mut *request.path.cast_mut().cast::<Generation>() };
    let mut point = [0; PUBLIC_LEN];
    let outcome = match p256_key::generate_host(&mut point) {
        Ok(document) => {
            call.document = Some(document);
            // SAFETY: admission proved a full point fits in the pinned exclusive spare.
            unsafe { append_point(&mut request.buffer, &point) };
            Outcome::default()
        }
        Err(failure) => key_failure(failure),
    };
    Step {
        kind: STEP_DONE,
        outcome,
        deadline_ns: 0,
    }
}

/// # Safety
/// The caller holds PUBLIC_LEN writable bytes starting at len.
pub(crate) unsafe fn append_point(public: &mut Buffer, point: &[u8; PUBLIC_LEN]) {
    unsafe {
        point
            .as_ptr()
            .copy_to_nonoverlapping(public.data.add(public.len), PUBLIC_LEN)
    };
    public.len += PUBLIC_LEN;
}

fn secret_bytes(document: Zeroizing<[u8; DOCUMENT_LEN]>) -> Arc<SecretBytes> {
    Arc::new(SecretBytes::new(Zeroizing::new(document.to_vec())))
}

/// # Safety
/// Context is running, Random is live, and public lends its pinned spare.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_p256_key(
    context: *mut Context,
    random: u64,
    uses: *const Buffer,
    public: *mut Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded from the checked call.
    unsafe {
        crate::process::deny_describe_capability(context);
        let Some(policy) = SecretPolicy::p256_generation(crate::tls::bytes(&*uses)) else {
            return key_error(KEY_INVALID);
        };
        let spare = (*public).capacity - (*public).len;
        if spare < PUBLIC_LEN {
            return key_capacity(PUBLIC_LEN, spare);
        }
        let charge = match admit((*context).payer(), &[], DOCUMENT_LEN as u64) {
            Ok(value) => value,
            Err(error) => return key_capacity(error.value as usize, error.written as usize),
        };
        let provider = &*crate::entry::hirsel_core_capability_provider(context, random);
        if crate::provide::owns(provider) {
            let mut point = [0; PUBLIC_LEN];
            let document = p256_key::generate_virtual(
                |draw| crate::provide::random::fill_virtual(provider, draw),
                &mut point,
            );
            let Ok(document) = document else {
                drop(charge);
                use crate::causes::{self, Cause};
                causes::record(&mut *context, Cause::new(causes::BOUNDS, site));
                return CANCELLED;
            };
            let result = store_sealed(context, policy, Some(secret_bytes(document)), charge, None);
            append_point(&mut *public, &point);
            return result;
        }

        let mut call = Generation {
            random,
            policy: policy.text(),
            document: None,
        };
        let mut request = Request {
            operation: MEDIA_P256_KEY,
            mode: 0,
            handle: 0,
            path: (&raw mut call).cast(),
            path_len: std::mem::size_of::<Generation>(),
            buffer: *public,
            site,
        };
        let answer = drive_secret_sink(context, PROVIDER, &mut request);
        if answer.status != 0 || answer.error != IoError::OK {
            return answer;
        }
        let result = store_sealed(
            context,
            policy,
            call.document.map(secret_bytes),
            charge,
            Some(Origin::GeneratedP256(answer.value)),
        );
        (*public).len = request.buffer.len;
        result
    }
}
