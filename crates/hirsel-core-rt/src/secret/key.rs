//! Synchronous Secret key operations.

use super::*;
use hirsel_core_net::key_wrap::{SignAlg, SigningRandom};
use hirsel_core_net::secret_aead::{self, AeadAlg, AeadInput, AeadMode, AeadRefusal};

/// Tape modes of SECRET_MAC for the key operations (recording.md).
pub(crate) const KEY_WRAP: u8 = 2;
pub(crate) const KEY_UNWRAP: u8 = 3;
pub(crate) const KEY_SIGN: u8 = 4;
pub(crate) const KEY_AEAD_SEAL: u8 = 5;
pub(crate) const KEY_AEAD_OPEN: u8 = 6;

pub(crate) fn is_key_mode(mode: u8) -> bool {
    (KEY_WRAP..=KEY_AEAD_OPEN).contains(&mode)
}

/// `KeyError` as an outcome's error tag: the case index plus one.
const KEY_DENIED: u8 = 1;
pub(super) const KEY_INVALID: u8 = 2;
const KEY_TAMPERED: u8 = 3;
const KEY_CAPACITY: u8 = 4;

pub(crate) enum KeyOp<'a> {
    Wrap(&'a Secret),
    Unwrap {
        blob: &'a [u8],
        uses: &'a SecretPolicy,
        /// The opened bytes, when the key was read; never on replay.
        plain: std::cell::Cell<Option<Zeroizing<Vec<u8>>>>,
    },
    Sign {
        alg: SignAlg,
        random: u64,
        source: SigningRandom,
        msg: &'a [u8],
    },
    Aead {
        input: AeadInput<'a>,
        len: usize,
    },
}

pub(crate) struct KeyRequest<'a> {
    /// The KEK, or the signing key.
    pub(crate) key: &'a Secret,
    pub(crate) op: KeyOp<'a>,
}

pub(super) fn key_error(tag: u8) -> Outcome {
    Outcome {
        error: IoError { tag, code: 0 },
        ..Outcome::default()
    }
}

pub(super) fn key_failure(failure: hirsel_core_net::key_wrap::KeyFailure) -> Outcome {
    use hirsel_core_net::key_wrap::KeyFailure;
    key_error(match failure {
        KeyFailure::Denied => KEY_DENIED,
        KeyFailure::Invalid => KEY_INVALID,
        KeyFailure::Tampered => KEY_TAMPERED,
        KeyFailure::Provider(reason) => crate::process::stop(reason.as_bytes()),
    })
}

pub(super) fn key_capacity(requested: usize, available: usize) -> Outcome {
    Outcome {
        value: requested as u64,
        written: available as u64,
        ..key_error(KEY_CAPACITY)
    }
}

/// The key operation inside its frame: the only place key bytes are read.
pub(super) unsafe fn key_issue(request: &mut Request) -> Outcome {
    use hirsel_core_net::key_wrap;
    // SAFETY: the key adapter pins KeyRequest and its carrier through delivery.
    unsafe {
        let args = &*request.path.cast::<KeyRequest<'_>>();
        let key = args.key.bytes.as_deref().unwrap();
        let produced = match &args.op {
            KeyOp::Wrap(secret) => key_wrap::wrap_with_salt(
                key,
                &secret.policy.text(),
                secret.bytes.as_deref().unwrap(),
                key_wrap::salt(),
            ),
            KeyOp::Sign {
                alg, source, msg, ..
            } => key_wrap::sign(*alg, key, msg, *source),
            KeyOp::Aead { input, len } => {
                let buffer = &mut request.buffer;
                let spare = if *len == 0 {
                    &mut []
                } else {
                    // The pre-issue check proved room. The adapter holds an exclusive
                    // borrow of the carrier. Its spare may be uninitialized.
                    std::slice::from_raw_parts_mut(buffer.data.add(buffer.len).cast(), *len)
                };
                return match input.apply(key, spare) {
                    Ok(()) => {
                        buffer.len += len;
                        Outcome {
                            value: *len as u64,
                            ..Outcome::default()
                        }
                    }
                    Err(failure) => key_failure(failure),
                };
            }
            KeyOp::Unwrap { blob, uses, plain } => {
                let mut out = Zeroizing::new(Vec::new());
                return match key_wrap::unwrap_into(key, blob, &mut out)
                    .and_then(|bound| key_wrap::within(uses, bound))
                {
                    Ok(()) => {
                        plain.set(Some(out));
                        Outcome::default()
                    }
                    Err(failure) => key_failure(failure),
                };
            }
        };
        let bytes = match produced {
            Ok(bytes) => bytes,
            Err(failure) => return key_failure(failure),
        };
        let spare = request.buffer.capacity - request.buffer.len;
        if bytes.len() > spare {
            return key_capacity(bytes.len(), spare);
        }
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            request.buffer.data.add(request.buffer.len),
            bytes.len(),
        );
        request.buffer.len += bytes.len();
        Outcome {
            value: bytes.len() as u64,
            ..Outcome::default()
        }
    }
}

/// Run one key operation in its immediate frame. `out` is appended into its
/// spare capacity; unwrap's carrier is empty.
unsafe fn key_op(
    context: *mut Context,
    args: &KeyRequest<'_>,
    mode: u8,
    out: *mut Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: the entry points pin the Secrets and all carriers.
    unsafe {
        #[cfg(feature = "record")]
        let tape = (*(*context).host_files)
            .secret_mac_tape
            .filter(|_| match args.op {
                KeyOp::Sign { alg, source, .. } => {
                    !matches!(args.key.origin.made, Made::Sealed(_))
                        || (alg == SignAlg::P256 && source == SigningRandom::Host)
                }
                _ => true,
            });
        #[cfg(feature = "record")]
        let (p, h) = tape.map_or((MAC_PROVIDER, 0), |tape| (tape.provider, tape.handle));
        #[cfg(not(feature = "record"))]
        let (p, h) = (MAC_PROVIDER, 0);
        let buffer = if out.is_null() {
            Buffer {
                data: std::ptr::null_mut(),
                len: 0,
                capacity: 0,
            }
        } else {
            *out
        };
        let mut request = Request {
            operation: crate::provider::SECRET_MAC,
            mode,
            handle: h,
            path: (args as *const KeyRequest<'_>).cast(),
            path_len: std::mem::size_of::<KeyRequest<'_>>(),
            buffer,
            site,
        };
        let outcome = crate::provider::drive(context, &p, &mut request, None);
        if !out.is_null() {
            (*out).len = request.buffer.len;
        }
        outcome
    }
}

/// `secret.wrap`: `s` sealed under `kek`, bound to `s`'s canonical policy.
/// # Safety
/// Both Secrets are live and `out` is a pinned exclusive Bytes carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_wrap(
    context: *mut Context,
    kek: u64,
    s: u64,
    out: *mut Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let kek = resolve(context, kek);
        let secret = resolve(context, s);
        // A program that holds the KEK's bytes could open the blob itself,
        // so it never wraps bytes it does not hold under that KEK.
        if !kek.policy.uses.permits(SecretUse::WRAP)
            || secret.policy.uses.imports()
            || (secret.origin.opaque() && !kek.origin.opaque())
        {
            return key_error(KEY_DENIED);
        }
        let args = KeyRequest {
            key: &kek,
            op: KeyOp::Wrap(&secret),
        };
        key_op(context, &args, KEY_WRAP, out, site)
    }
}

/// `secret.unwrap`: a new Secret limited to `uses`, which must be within the
/// policy the blob binds. Charged as `seal` charges, before the frame.
/// # Safety
/// The KEK is live and both carriers are pinned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_unwrap(
    context: *mut Context,
    kek: u64,
    blob: *const Buffer,
    uses: *const Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let kek = resolve(context, kek);
        if !kek.policy.uses.permits(SecretUse::WRAP) {
            return key_error(KEY_DENIED);
        }
        let Some(requested) = policy(uses).filter(|policy| !policy.uses.imports()) else {
            return key_error(KEY_INVALID);
        };
        let blob = crate::tls::bytes(&*blob);
        let len = hirsel_core_net::key_wrap::unwrapped_len(blob).unwrap_or(0);
        let charge = match admit((*context).payer(), &requested.send, len as u64) {
            Ok(charge) => charge,
            Err(mut error) => {
                error.error.tag = KEY_CAPACITY;
                return error;
            }
        };
        let args = KeyRequest {
            key: &kek,
            op: KeyOp::Unwrap {
                blob,
                uses: &requested,
                plain: std::cell::Cell::new(None),
            },
        };
        let outcome = key_op(context, &args, KEY_UNWRAP, std::ptr::null_mut(), site);
        if outcome.error != IoError::OK || outcome.status != 0 {
            return outcome;
        }
        let KeyOp::Unwrap { plain, .. } = args.op else {
            unreachable!()
        };
        let (stream, ordinal) = super::next_in_unit(true);
        let origin = Origin {
            made: Made::Unwrapped {
                ordinal,
                opaque: kek.origin.opaque(),
            },
            stream,
        };
        let mut secrets = (*(*context).host_files)
            .registry
            .run_grants()
            .secrets
            .lock()
            .unwrap();
        let value = secrets.push(
            Secret::new(
                origin,
                requested,
                plain
                    .into_inner()
                    .map(|bytes| Arc::new(SecretBytes::new(bytes))),
                Some(ChargeCell::new(charge)),
            ),
            None,
        );
        Outcome {
            value,
            ..Outcome::default()
        }
    }
}

/// `secret.sign`: `msg` signed with `alg`, which the key's uses must name.
/// # Safety
/// The key is live, `msg` is pinned and `out` is a pinned exclusive carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_sign(
    context: *mut Context,
    key: u64,
    alg: u64,
    random: u64,
    msg: *const Buffer,
    out: *mut Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let key = resolve(context, key);
        let Some(alg) = SignAlg::from_case(alg) else {
            crate::process::stop(b"SignAlg")
        };
        if !key.policy.uses.permits(alg.needed()) {
            return key_error(KEY_DENIED);
        }
        let provider = crate::entry::hirsel_core_capability_provider(context, random);
        let source = if crate::provide::owns(&*provider) {
            SigningRandom::Virtual
        } else {
            SigningRandom::Host
        };
        if alg == SignAlg::P256 && source == SigningRandom::Virtual && key.origin.opaque() {
            return key_error(KEY_DENIED);
        }
        let args = KeyRequest {
            key: &key,
            op: KeyOp::Sign {
                alg,
                random,
                source,
                msg: crate::tls::bytes(&*msg),
            },
        };
        key_op(context, &args, KEY_SIGN, out, site)
    }
}

unsafe fn secret_aead(
    context: *mut Context,
    key: u64,
    alg: u64,
    mode: AeadMode,
    inputs: [*const Buffer; 3],
    out: *mut Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: the entry adapters pin the key and all four carriers.
    unsafe {
        crate::process::deny_describe_capability(context);
        let key = resolve(context, key);
        let Some(alg) = AeadAlg::from_case(alg) else {
            crate::process::stop(b"AeadAlg")
        };
        let [nonce, aad, bytes] = inputs;
        let spare = (*out).capacity - (*out).len;
        let len = match secret_aead::output_len(
            key.policy.uses,
            alg,
            mode,
            (*nonce).len,
            (*bytes).len,
            spare,
        ) {
            Ok(len) => len,
            Err(AeadRefusal::Denied) => return key_error(KEY_DENIED),
            Err(AeadRefusal::Invalid) => return key_error(KEY_INVALID),
            Err(AeadRefusal::Capacity {
                requested,
                available,
            }) => return key_capacity(requested, available),
        };
        let input = AeadInput {
            alg,
            mode,
            nonce: crate::tls::bytes(&*nonce),
            aad: crate::tls::bytes(&*aad),
            bytes: crate::tls::bytes(&*bytes),
        };
        let args = KeyRequest {
            key: &key,
            op: KeyOp::Aead { input, len },
        };
        let mode = match mode {
            AeadMode::Seal => KEY_AEAD_SEAL,
            AeadMode::Open => KEY_AEAD_OPEN,
        };
        key_op(context, &args, mode, out, site)
    }
}

/// # Safety
/// `context` and `key` are live. The three input carriers and their initialized
/// bytes are pinned through delivery. `out` and its backing buffer are pinned,
/// exclusive, and disjoint from the inputs, with their full capacity writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_aead_seal(
    context: *mut Context,
    key: u64,
    alg: u64,
    nonce: *const Buffer,
    aad: *const Buffer,
    plain: *const Buffer,
    out: *mut Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: the caller supplies the live, pinned carriers.
    unsafe {
        secret_aead(
            context,
            key,
            alg,
            AeadMode::Seal,
            [nonce, aad, plain],
            out,
            site,
        )
    }
}

/// # Safety
/// `context` and `key` are live. The three input carriers and their initialized
/// bytes are pinned through delivery. `out` and its backing buffer are pinned,
/// exclusive, and disjoint from the inputs, with their full capacity writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_aead_open(
    context: *mut Context,
    key: u64,
    alg: u64,
    nonce: *const Buffer,
    aad: *const Buffer,
    sealed: *const Buffer,
    out: *mut Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: the caller supplies the live, pinned carriers.
    unsafe {
        secret_aead(
            context,
            key,
            alg,
            AeadMode::Open,
            [nonce, aad, sealed],
            out,
            site,
        )
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod rng_tests;
