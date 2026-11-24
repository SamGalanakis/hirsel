use super::*;
use hirsel_core_net::{argon2id::Error as Argon2Error, secret_kdf::Derivation};

pub(crate) const HKDF_MODE: u8 = 0x80;
pub(crate) const ARGON2ID_MODE: u8 = 0x81;

pub(crate) struct KdfRequest<'a> {
    pub(crate) secret: &'a Secret,
    pub(crate) salt_or_prefix: &'a [u8],
    pub(crate) info_or_data: &'a [u8],
    pub(crate) derivation: Option<Derivation>,
    #[cfg(feature = "record")]
    pub(crate) child: &'a SecretPolicy,
    output: Option<Arc<SecretBytes>>,
}

pub(crate) const KDF_PROVIDER: Provider = Provider {
    issue: kdf_issue,
    ..MAC_PROVIDER
};

unsafe extern "C" fn kdf_issue(_: *mut u8, request: *mut Request, _: u64) -> Step {
    // SAFETY: the adapter pins the exclusive request through delivery.
    let args = unsafe { &mut *(*request).path.cast_mut().cast::<KdfRequest<'_>>() };
    let key = args.secret.bytes.as_deref().unwrap();
    let derived =
        args.derivation
            .take()
            .unwrap()
            .derive(key, args.salt_or_prefix, args.info_or_data);
    let outcome = match derived {
        Ok(bytes) => {
            args.output = Some(Arc::new(SecretBytes::new(bytes)));
            Outcome::default()
        }
        Err(Argon2Error::Params) => INVALID,
        Err(Argon2Error::HostMemory) => crate::process::stop(b"HostMemory"),
    };
    Step {
        kind: STEP_DONE,
        outcome,
        deadline_ns: 0,
    }
}

/// # Safety
/// The Secret is live and all three input carriers stay pinned through delivery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_derive_hmac_sha256(
    context: *mut Context,
    s: u64,
    prefix: *const Buffer,
    data: *const Buffer,
    uses: *const Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        derive(
            context,
            s,
            Some(Derivation::Hmac),
            crate::tls::bytes(&*prefix),
            crate::tls::bytes(&*data),
            uses,
            site,
        )
    }
}

/// # Safety
/// The Secret is live and all three input carriers stay pinned through delivery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_derive_hkdf_sha256(
    context: *mut Context,
    s: u64,
    salt: *const Buffer,
    info: *const Buffer,
    len: u64,
    uses: *const Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        derive(
            context,
            s,
            Derivation::hkdf(len),
            crate::tls::bytes(&*salt),
            crate::tls::bytes(&*info),
            uses,
            site,
        )
    }
}

/// # Safety
/// The Secret is live and both input carriers stay pinned through delivery.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_derive_argon2id(
    context: *mut Context,
    s: u64,
    salt: *const Buffer,
    memory_kib: u32,
    passes: u32,
    lanes: u32,
    len: u64,
    uses: *const Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        let salt = crate::tls::bytes(&*salt);
        let derivation = Derivation::argon2id([memory_kib, passes, lanes], len, salt.len() as u64);
        derive(context, s, derivation, salt, &[], uses, site)
    }
}

unsafe fn derive(
    context: *mut Context,
    s: u64,
    derivation: Option<Derivation>,
    salt_or_prefix: &[u8],
    info_or_data: &[u8],
    uses: *const Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let secret = resolve(context, s);
        let uses = crate::tls::bytes(&*uses);
        let Some(child) = std::str::from_utf8(uses)
            .ok()
            .and_then(|uses| secret.policy.derived(uses))
        else {
            return INVALID;
        };
        let Some(derivation) = derivation else {
            return INVALID;
        };
        let (len, steps, working) = derivation.charges();
        if steps != 0 && !crate::steps::take_many(context, steps, site) {
            return crate::provider::CANCELLED;
        }
        let child_size = SecretGrant::logical_size(&child.send, len).unwrap();
        let payer = (*context).payer();
        let mut charge = match admit_size(payer, child_size.checked_add(working)) {
            Ok(charge) => charge,
            Err(error) => return error,
        };
        let mode = match derivation {
            Derivation::Hmac => 0,
            Derivation::Hkdf { .. } => HKDF_MODE,
            Derivation::Argon2id { .. } => ARGON2ID_MODE,
        };
        let mut args = KdfRequest {
            secret: &secret,
            salt_or_prefix,
            info_or_data,
            derivation: Some(derivation),
            #[cfg(feature = "record")]
            child: &child,
            output: None,
        };
        let mut request = Request {
            operation: crate::provider::SECRET_KDF,
            mode,
            handle: 0,
            path: (&raw mut args).cast(),
            path_len: std::mem::size_of_val(&args),
            buffer: crate::net::empty_buffer(),
            site,
        };
        let outcome = drive_secret_sink(context, KDF_PROVIDER, &mut request);
        if working != 0 {
            payer.refund(working);
            charge.1 = child_size;
        }
        if outcome.error.tag != 0 {
            return outcome;
        }
        if outcome.status != 0 {
            return INVALID;
        }
        #[cfg(feature = "record")]
        let ordinal = outcome.value;
        #[cfg(not(feature = "record"))]
        let ordinal = 0;
        // A key derived from a grant is as unknown to the program as the
        // grant, so `wrap` and served streams treat it alike.
        let origin = Origin::Derived {
            ordinal,
            opaque: secret.origin.opaque(),
        };
        // A replay leaves bytes absent. The sinks that can read this key
        // are themselves replay deliveries.
        let bytes = args.output;
        store_sealed(context, child, bytes, charge, Some(origin))
    }
}
