use super::{Secret, drive_secret_sink, key, resolve};
use crate::{
    Context,
    provider::{Buffer, IoError, Outcome, Provider, Request, STEP_DONE, Step},
};
use hirsel_core_net::{HmacAlgorithm, SecretUse};

// SECRET_MAC mode tags in recording.md.
const JWS_MAC_FIRST: u8 = 11;
const JWS_MAC_LAST: u8 = 16;
pub(crate) const SHORT_JWS_KEY: u8 = 2;

pub(crate) fn is_jws_mac_mode(mode: u8) -> bool {
    (JWS_MAC_FIRST..=JWS_MAC_LAST).contains(&mode)
}

enum MacProfile {
    Raw(HmacAlgorithm),
    Jws(HmacAlgorithm),
}

impl MacProfile {
    fn mode(&self, verify: bool) -> u8 {
        let sign = match self {
            Self::Raw(HmacAlgorithm::Sha1) => 17,
            Self::Raw(HmacAlgorithm::Sha256) => 0,
            Self::Raw(HmacAlgorithm::Sha384) => 7,
            Self::Raw(HmacAlgorithm::Sha512) => 9,
            Self::Jws(HmacAlgorithm::Sha1) => unreachable!("JWS does not use SHA-1"),
            Self::Jws(HmacAlgorithm::Sha256) => JWS_MAC_FIRST,
            Self::Jws(HmacAlgorithm::Sha384) => 13,
            Self::Jws(HmacAlgorithm::Sha512) => 15,
        };
        sign + u8::from(verify)
    }

    fn algorithm(&self) -> HmacAlgorithm {
        match *self {
            Self::Raw(algorithm) | Self::Jws(algorithm) => algorithm,
        }
    }
}

pub(crate) struct MacRequest<'a> {
    algorithm: HmacAlgorithm,
    pub(crate) secret: &'a Secret,
    pub(crate) data: &'a [u8],
    pub(crate) tag: Option<&'a [u8]>,
}

pub(crate) const MAC_PROVIDER: Provider = Provider {
    state: std::ptr::null_mut(),
    wait: crate::os::WAIT,
    issue: mac_issue,
    resume: mac_resume,
    abort: mac_abort,
    deliver: crate::provider::DELIVER,
};

unsafe extern "C" fn mac_issue(_: *mut u8, request: *mut Request, _: u64) -> Step {
    // SAFETY: the adapter pins MacRequest and its output through delivery.
    let outcome = unsafe {
        let request = &mut *request;
        if key::is_key_mode(request.mode) {
            return Step {
                kind: STEP_DONE,
                outcome: key::key_issue(request),
                deadline_ns: 0,
            };
        }
        let args = &*request.path.cast::<MacRequest<'_>>();
        let key = args.secret.bytes.as_deref().unwrap();
        let mut outcome = Outcome::default();
        if is_jws_mac_mode(request.mode) && key.len() < args.algorithm.tag_len() {
            outcome.error.tag = SHORT_JWS_KEY;
        } else if let Some(tag) = args.tag {
            outcome.value = u64::from(args.algorithm.verify(key, args.data, tag));
        } else {
            let signed = args.algorithm.sign(key, args.data);
            let tag = signed.as_ref();
            std::ptr::copy_nonoverlapping(tag.as_ptr(), request.buffer.data, tag.len());
            request.buffer.len = tag.len();
        }
        outcome
    };
    Step {
        kind: STEP_DONE,
        outcome,
        deadline_ns: 0,
    }
}

unsafe extern "C" fn mac_resume(_: *mut u8, _: *mut Request, _: u64) -> Step {
    unreachable!("Secret key sinks never wait")
}

unsafe extern "C" fn mac_abort(_: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
    unreachable!("Secret key sinks never wait")
}

unsafe fn mac(
    context: *mut Context,
    handle: u64,
    data: *const Buffer,
    tag: Option<*const Buffer>,
    out: *mut u8,
    site: u64,
    profile: MacProfile,
) -> Outcome {
    // SAFETY: the entry points pin the Secret and all carriers.
    unsafe {
        crate::process::deny_describe_capability(context);
        let secret = resolve(context, handle);
        let needed = if tag.is_some() {
            SecretUse::VERIFY
        } else {
            SecretUse::MAC
        };
        if !secret.policy.uses.permits(needed) {
            return crate::net::DENIED;
        }
        let data = crate::tls::bytes(&*data);
        if !crate::steps::take_many(context, data.len() as u64, site) {
            return crate::provider::CANCELLED;
        }
        let algorithm = profile.algorithm();
        let args = MacRequest {
            algorithm,
            secret: &secret,
            data,
            tag: tag.map(|tag| crate::tls::bytes(&*tag)),
        };
        let mut request = Request {
            operation: crate::provider::SECRET_MAC,
            mode: profile.mode(tag.is_some()),
            handle: 0,
            path: (&raw const args).cast(),
            path_len: std::mem::size_of_val(&args),
            buffer: Buffer {
                data: out,
                len: 0,
                capacity: if tag.is_none() {
                    algorithm.tag_len()
                } else {
                    0
                },
            },
            site,
        };
        drive_secret_sink(context, MAC_PROVIDER, &mut request)
    }
}

/// # Safety
/// The Secret is live, `data` is pinned, and `out` is writable for 20 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_hmac_sha1(
    context: *mut Context,
    s: u64,
    data: *const Buffer,
    out: *mut u8,
    site: u64,
) -> Outcome {
    unsafe {
        mac(
            context,
            s,
            data,
            None,
            out,
            site,
            MacProfile::Raw(HmacAlgorithm::Sha1),
        )
    }
}

/// # Safety
/// The Secret is live and both input carriers are pinned through delivery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_verify_hmac_sha1(
    context: *mut Context,
    s: u64,
    data: *const Buffer,
    tag: *const Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        mac(
            context,
            s,
            data,
            Some(tag),
            std::ptr::null_mut(),
            site,
            MacProfile::Raw(HmacAlgorithm::Sha1),
        )
    }
}

/// # Safety
/// The Secret is live, `data` is pinned, and `out` is writable for 32 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_hmac_sha256(
    context: *mut Context,
    s: u64,
    data: *const Buffer,
    out: *mut u8,
    site: u64,
) -> Outcome {
    unsafe {
        mac(
            context,
            s,
            data,
            None,
            out,
            site,
            MacProfile::Raw(HmacAlgorithm::Sha256),
        )
    }
}

/// # Safety
/// The Secret is live and both input carriers are pinned through delivery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_verify_hmac_sha256(
    context: *mut Context,
    s: u64,
    data: *const Buffer,
    tag: *const Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        mac(
            context,
            s,
            data,
            Some(tag),
            std::ptr::null_mut(),
            site,
            MacProfile::Raw(HmacAlgorithm::Sha256),
        )
    }
}

/// # Safety
/// The Secret is live, `data` is pinned, and `out` is writable for 48 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_hmac_sha384(
    context: *mut Context,
    s: u64,
    data: *const Buffer,
    out: *mut u8,
    site: u64,
) -> Outcome {
    unsafe {
        mac(
            context,
            s,
            data,
            None,
            out,
            site,
            MacProfile::Raw(HmacAlgorithm::Sha384),
        )
    }
}

/// # Safety
/// The Secret is live and both input carriers are pinned through delivery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_verify_hmac_sha384(
    context: *mut Context,
    s: u64,
    data: *const Buffer,
    tag: *const Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        mac(
            context,
            s,
            data,
            Some(tag),
            std::ptr::null_mut(),
            site,
            MacProfile::Raw(HmacAlgorithm::Sha384),
        )
    }
}

/// # Safety
/// The Secret is live, `data` is pinned, and `out` is writable for 64 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_hmac_sha512(
    context: *mut Context,
    s: u64,
    data: *const Buffer,
    out: *mut u8,
    site: u64,
) -> Outcome {
    unsafe {
        mac(
            context,
            s,
            data,
            None,
            out,
            site,
            MacProfile::Raw(HmacAlgorithm::Sha512),
        )
    }
}

/// # Safety
/// The Secret is live and both input carriers are pinned through delivery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_verify_hmac_sha512(
    context: *mut Context,
    s: u64,
    data: *const Buffer,
    tag: *const Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        mac(
            context,
            s,
            data,
            Some(tag),
            std::ptr::null_mut(),
            site,
            MacProfile::Raw(HmacAlgorithm::Sha512),
        )
    }
}

/// # Safety
/// The Secret is live, `data` is pinned, and `out` is writable for 64 bytes.
/// `alg` is a JwsMacAlg case.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_jws_hmac_sign(
    context: *mut Context,
    s: u64,
    data: *const Buffer,
    out: *mut u8,
    alg: u64,
    site: u64,
) -> Outcome {
    let algorithm = HmacAlgorithm::from_jws_case(alg);
    unsafe {
        let outcome = mac(
            context,
            s,
            data,
            None,
            out,
            site,
            MacProfile::Jws(algorithm),
        );
        if outcome.status == 0 && outcome.error == IoError::OK {
            let len = algorithm.tag_len();
            out.add(len).write_bytes(0, 64 - len);
        }
        outcome
    }
}

/// # Safety
/// The Secret is live, both input carriers are pinned, and `alg` is a JwsMacAlg case.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_jws_hmac_verify(
    context: *mut Context,
    s: u64,
    data: *const Buffer,
    tag: *const Buffer,
    alg: u64,
    site: u64,
) -> Outcome {
    unsafe {
        mac(
            context,
            s,
            data,
            Some(tag),
            std::ptr::null_mut(),
            site,
            MacProfile::Jws(HmacAlgorithm::from_jws_case(alg)),
        )
    }
}
