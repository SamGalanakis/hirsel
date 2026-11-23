//! TLS-5. Secret storage is never a language carrier. Each `Secret` value is one
//! holder of a designation slot; the last holder's drop releases the slot and its
//! charge, and the last designation of sealed bytes refunds the seal.
use crate::{
    Context,
    entry::{SecretOperations, hirsel_core_capability_handle},
    host_file::DYNAMIC_GRANT,
    limit::Payer,
    provider::{Buffer, IoError, Outcome, Provider, Request, STEP_DONE, Step, Wait},
};
use hirsel_core_net::{Name, Pattern, SecretGrant, SecretPolicy, SecretSource, SecretUse};
use std::{
    io::Read,
    sync::{Arc, Mutex, Weak},
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Copy)]
pub(crate) enum Origin {
    Granted(u64),
    Sealed(u64),
    /// Designated by its `SECRET_KDF` delivery position under a tape (0
    /// otherwise); opaque when its parent was.
    Derived {
        ordinal: u64,
        opaque: bool,
    },
    /// The `ordinal`th `secret.unwrap` of the run; opaque when its KEK was.
    Unwrapped {
        ordinal: u64,
        opaque: bool,
    },
    /// The run's nth credential fetch (TLS-7), whose bytes the host sealed.
    Fetched(u64),
}

impl Origin {
    /// Whether the program never held these bytes: a grant, or a secret
    /// derived or unwrapped from one (TLS-5).
    pub(crate) fn opaque(self) -> bool {
        match self {
            Self::Granted(_) | Self::Fetched(_) => true,
            Self::Sealed(_) => false,
            Self::Derived { opaque, .. } | Self::Unwrapped { opaque, .. } => opaque,
        }
    }

    /// The tape's origin kind and ordinal (recording.md, Credentials).
    #[cfg(feature = "record")]
    pub(crate) fn wire(self) -> (u8, u64) {
        match self {
            Self::Granted(ordinal) => (0, ordinal),
            Self::Sealed(ordinal) => (1, ordinal),
            Self::Derived { ordinal, .. } => (2, ordinal),
            Self::Unwrapped { ordinal, .. } => (3, ordinal),
            Self::Fetched(ordinal) => (4, ordinal),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Secret {
    pub(crate) origin: Origin,
    pub(crate) policy: SecretPolicy,
    pub(crate) bytes: Option<Arc<Zeroizing<Vec<u8>>>>,
    /// A sealed secret's charge, shared by every designation of its bytes.
    pub(crate) seal: Option<Arc<ChargeCell>>,
}

pub(crate) struct Granted {
    pub(crate) record: SecretGrant,
    pub(crate) secret: Secret,
}

#[unsafe(no_mangle)]
pub static hirsel_core_os_secret_ops: SecretOperations = SecretOperations { read: read_grant };

fn read_grant([name, source, patterns]: [&[u8]; 3], index: u64) -> Option<Granted> {
    let text = |view| std::str::from_utf8(view).ok();
    let record = SecretGrant::new(text(name)?, text(source)?, text(patterns)?)?;
    Granted::read(record, index)
}

impl Granted {
    pub(crate) fn read(record: SecretGrant, index: u64) -> Option<Self> {
        let bytes = match SecretSource::parse(&record.source)? {
            SecretSource::Env(name) => {
                use std::os::unix::ffi::OsStringExt;
                Zeroizing::new(std::env::var_os(name)?.into_vec())
            }
            SecretSource::File(path) => read_file(path)?,
        };
        if bytes.is_empty() {
            return None;
        }
        Some(Self {
            secret: Secret {
                origin: Origin::Granted(index),
                policy: SecretPolicy::parse(&record.policy)?,
                bytes: Some(Arc::new(bytes)),
                seal: None,
            },
            record,
        })
    }
}

fn read_file(path: &str) -> Option<Zeroizing<Vec<u8>>> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut bytes = Zeroizing::new(Vec::new());
    let mut chunk = Zeroizing::new([0; 8192]);
    loop {
        let n = match file.read(&mut *chunk) {
            Ok(0) => return Some(bytes),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        let need = bytes.len().checked_add(n)?;
        if need > bytes.capacity() {
            // Keep ownership of the old allocation until it has been wiped.
            let mut grown = Zeroizing::new(Vec::new());
            grown
                .try_reserve_exact(need.max(bytes.capacity().saturating_mul(2)))
                .ok()?;
            grown.extend_from_slice(&bytes);
            bytes = grown;
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
}

pub(crate) struct TlsWrite {
    pub(crate) name: Name,
    pub(crate) spent: bool,
    pub(crate) binding: hirsel_core_net::scram::Binding,
}

pub(crate) struct Charge(Payer, u64);

impl Drop for Charge {
    fn drop(&mut self) {
        self.0.refund(self.1);
    }
}

/// A charge that the release of its designation, or of the last designation of
/// sealed bytes, refunds. When the unit whose limit paid it ends first, its owner
/// takes the charge over (CONC-15).
pub(crate) struct ChargeCell(Mutex<Option<Charge>>);

// SAFETY: an outstanding charge pins its payer slot, and a refund settles from
// any unit (MEM-20).
unsafe impl Send for ChargeCell {}
// SAFETY: the charge is reached only under its mutex.
unsafe impl Sync for ChargeCell {}

impl ChargeCell {
    fn new(charge: Charge) -> Arc<Self> {
        Arc::new(Self(Mutex::new(Some(charge))))
    }
}

/// A designation still held when the unit that paid for it ended, and its bytes.
pub(crate) type HandedOver = Vec<(Weak<ChargeCell>, u64)>;

/// The owner's end take: each designation still held keeps its charge, now
/// paid by the owner's part of the helper's charge.
pub(crate) fn take_over(handed: &HandedOver, mut payer: impl FnMut(u64) -> Payer) {
    for (cell, bytes) in handed {
        if let Some(cell) = cell.upgrade() {
            let mut charge = cell.0.lock().unwrap();
            if charge.is_none() {
                *charge = Some(Charge(payer(*bytes), *bytes));
            }
        }
    }
}

struct Entry {
    secret: Secret,
    /// `None` for a grant registered by the launcher, which is never released.
    holders: Option<u64>,
    /// What `narrow` or `provide.secret` admitted for this designation.
    charge: Option<Arc<ChargeCell>>,
}

#[derive(Default)]
pub(crate) struct Secrets {
    entries: Vec<Option<Entry>>,
    free: Vec<usize>,
    seals: u64,
    unwraps: u64,
    fetches: u64,
}

impl Secrets {
    pub(crate) fn next_fetch(&mut self) -> u64 {
        self.fetches += 1;
        self.fetches - 1
    }

    pub(crate) fn register(&mut self, secret: Secret) -> u64 {
        self.insert(Entry {
            secret,
            holders: None,
            charge: None,
        })
    }

    fn push(&mut self, secret: Secret, charge: Option<Charge>) -> u64 {
        self.insert(Entry {
            secret,
            holders: Some(1),
            charge: charge.map(ChargeCell::new),
        })
    }

    fn insert(&mut self, entry: Entry) -> u64 {
        // A slot is never recorded, only its designation's origin, so reuse is
        // invisible to replay (recording.md, Credentials).
        let index = match self.free.pop() {
            Some(index) => {
                self.entries[index] = Some(entry);
                index
            }
            None => {
                self.entries.push(Some(entry));
                self.entries.len() - 1
            }
        };
        DYNAMIC_GRANT | index as u64
    }

    fn entry(&mut self, handle: u64) -> &mut Entry {
        self.entries[(handle & !DYNAMIC_GRANT) as usize]
            .as_mut()
            .unwrap_or_else(|| crate::process::stop(b"SecretSlot"))
    }

    /// Remove one holder; the last one's entry comes back to be dropped
    /// outside the table's lock.
    fn release(&mut self, handle: u64) -> Option<Entry> {
        let entry = self.entry(handle);
        let holders = entry.holders.as_mut()?;
        *holders -= 1;
        if *holders != 0 {
            return None;
        }
        let index = (handle & !DYNAMIC_GRANT) as usize;
        self.free.push(index);
        self.entries[index].take()
    }

    /// At a helper's end: each charge that `payer`'s domain paid for a
    /// designation still held. Its old charge comes back to be dropped,
    /// refunding this domain, outside the table's lock.
    pub(crate) fn hand_over(&mut self, payer: Payer) -> (HandedOver, Vec<Charge>) {
        let mut handed = Vec::new();
        let mut refunds = Vec::new();
        for entry in self.entries.iter().flatten() {
            for cell in [&entry.charge, &entry.secret.seal].into_iter().flatten() {
                let mut charge = cell.0.lock().unwrap();
                if charge.as_ref().is_some_and(|c| c.0.same_domain(payer)) {
                    let c = charge.take().unwrap();
                    handed.push((Arc::downgrade(cell), c.1));
                    refunds.push(c);
                }
            }
        }
        (handed, refunds)
    }
}

/// Release one holder of a Secret (TLS-5). Not a provider operation: no
/// replay point, no step, no frame.
/// # Safety
/// Context is running and the handle is a live Secret holder of this run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_drop(context: *mut Context, handle: u64) {
    if handle & DYNAMIC_GRANT == 0 {
        return;
    }
    // SAFETY: the run's grants outlive its units.
    let entry = unsafe {
        (*(*context).host_files)
            .registry
            .run_grants()
            .secrets
            .lock()
            .unwrap()
            .release(handle)
    };
    drop(entry);
}

/// `secret.share`: one more holder of the same designation.
/// # Safety
/// Context is running and the handle is a live Secret holder of this run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_share(context: *mut Context, handle: u64) -> u64 {
    if handle & DYNAMIC_GRANT != 0 {
        // SAFETY: the run's grants outlive its units.
        let mut secrets = unsafe {
            (*(*context).host_files)
                .registry
                .run_grants()
                .secrets
                .lock()
                .unwrap()
        };
        if let Some(holders) = &mut secrets.entry(handle).holders {
            *holders += 1;
        }
    }
    handle
}

pub(crate) unsafe fn resolve(context: *const Context, handle: u64) -> Secret {
    unsafe {
        if handle & DYNAMIC_GRANT != 0 {
            (*(*context).host_files)
                .registry
                .run_grants()
                .secrets
                .lock()
                .unwrap()
                .entry(handle)
                .secret
                .clone()
        } else {
            let grant = hirsel_core_capability_handle(context, handle) as *const Granted;
            (*grant).secret.clone()
        }
    }
}

/// A new designation of `secret`, charged as `narrow` charges, to the
/// current limit; a refusal answers error tag 2 (`provide.secret`).
pub(crate) unsafe fn designate(context: *mut Context, secret: Secret) -> Outcome {
    unsafe {
        let charge = match admit((*context).payer(), &secret.policy.send, 0) {
            Ok(charge) => charge,
            Err(error) => return error,
        };
        let value = (*(*context).host_files)
            .registry
            .run_grants()
            .secrets
            .lock()
            .unwrap()
            .push(secret, Some(charge));
        Outcome {
            value,
            ..Outcome::default()
        }
    }
}

unsafe fn policy(to: *const Buffer) -> Option<SecretPolicy> {
    let bytes = unsafe { crate::tls::bytes(&*to) };
    SecretPolicy::parse(std::str::from_utf8(bytes).ok()?)
}

pub(crate) const INVALID: Outcome = Outcome {
    error: IoError { tag: 1, code: 0 },
    value: 0,
    written: 0,
    status: 0,
    kind: 0,
};

fn admit(payer: Payer, policies: &[Pattern], len: u64) -> Result<Charge, Outcome> {
    let size = SecretGrant::logical_size(policies, len);
    match payer.admit(size) {
        Ok(()) => Ok(Charge(payer, size.unwrap())),
        Err(c) => Err(Outcome {
            error: IoError { tag: 2, code: 0 },
            value: c.requested,
            written: c.available,
            status: 0,
            kind: 0,
        }),
    }
}

unsafe fn store_sealed(
    context: *mut Context,
    policy: SecretPolicy,
    bytes: Option<Arc<Zeroizing<Vec<u8>>>>,
    charge: Charge,
    origin: Option<Origin>,
) -> Outcome {
    let mut secrets = unsafe {
        (*(*context).host_files)
            .registry
            .run_grants()
            .secrets
            .lock()
            .unwrap()
    };
    let origin = origin.unwrap_or_else(|| {
        let ordinal = secrets.seals;
        secrets.seals += 1;
        Origin::Sealed(ordinal)
    });
    let value = secrets.push(
        Secret {
            origin,
            policy,
            bytes,
            seal: Some(ChargeCell::new(charge)),
        },
        None,
    );
    Outcome {
        value,
        ..Outcome::default()
    }
}

/// On success, copies the bytes into a run-owned secret and wipes owned
/// source storage. On refusal, leaves the source and its charge untouched.
/// The caller owns the carrier and is responsible for moving or dropping it.
///
/// # Safety
/// The source is exclusively owned and both initialized carriers are pinned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_seal(
    context: *mut Context,
    source: *const Buffer,
    to: *const Buffer,
    _site: u64,
) -> Outcome {
    unsafe {
        let Some(policy) = policy(to) else {
            return INVALID;
        };
        let bytes = crate::tls::bytes(&*source);
        let charge = match admit((*context).payer(), &policy.send, bytes.len() as u64) {
            Ok(charge) => charge,
            Err(error) => return error,
        };
        let owned = Arc::new(Zeroizing::new(bytes.to_vec()));
        if (*source).capacity != 0 && (*source).len != 0 {
            std::slice::from_raw_parts_mut((*source).data, (*source).len).zeroize();
        }
        store_sealed(context, policy, Some(owned), charge, None)
    }
}

/// # Safety
/// A live Secret from this run and a pinned initialized String carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_secret_narrow(
    context: *mut Context,
    s: u64,
    to: *const Buffer,
    _site: u64,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let Some(requested) = policy(to) else {
            return INVALID;
        };
        let mut secret = resolve(context, s);
        let Some(policy) = secret.policy.intersect(&requested) else {
            return INVALID;
        };
        let charge = match admit((*context).payer(), &policy.send, 0) {
            Ok(charge) => charge,
            Err(error) => return error,
        };
        secret.policy = policy;
        let value = (*(*context).host_files)
            .registry
            .run_grants()
            .secrets
            .lock()
            .unwrap()
            .push(secret, Some(charge));
        Outcome {
            value,
            ..Outcome::default()
        }
    }
}

#[repr(C)]
pub struct SendRequest {
    pub(crate) network: crate::net::NetworkRequest,
    pub secret: Buffer,
    pub(crate) origin: Origin,
    pub(crate) patterns: Arc<[Pattern]>,
    pub(crate) committed: usize,
    /// On a served stream: the Secret its server receives, narrowed to the
    /// stream's name (`provide.secret`), and `secret` is empty.
    pub(crate) narrowed: Option<Secret>,
}

#[cfg(feature = "record")]
#[derive(Clone, Copy)]
pub(crate) struct MacTape {
    pub(crate) provider: Provider,
    pub(crate) handle: u64,
}

pub(crate) struct KdfRequest<'a> {
    pub(crate) secret: &'a Secret,
    pub(crate) prefix: &'a [u8],
    pub(crate) data: &'a [u8],
    #[cfg(feature = "record")]
    pub(crate) child: &'a SecretPolicy,
    output: *mut Option<Arc<Zeroizing<Vec<u8>>>>,
}

pub(crate) const KDF_PROVIDER: Provider = Provider {
    issue: kdf_issue,
    ..MAC_PROVIDER
};

unsafe extern "C" fn kdf_issue(_: *mut u8, request: *mut Request, _: u64) -> Step {
    // SAFETY: the adapter pins the input and exclusive output slot.
    unsafe {
        let args = &*(*request).path.cast::<KdfRequest<'_>>();
        let key = args.secret.bytes.as_deref().unwrap();
        args.output
            .write(Some(Arc::new(hirsel_core_net::derive_hmac_sha256(
                key,
                args.prefix,
                args.data,
            ))));
    }
    Step {
        kind: STEP_DONE,
        outcome: Outcome::default(),
        deadline_ns: 0,
    }
}

unsafe fn drive_secret_sink(
    context: *mut Context,
    live: Provider,
    request: &mut Request,
) -> Outcome {
    #[cfg(feature = "record")]
    if let Some(tape) = unsafe { (*(*context).host_files).secret_mac_tape } {
        request.handle = tape.handle;
        return unsafe { crate::provider::drive(context, &tape.provider, request, None) };
    }
    unsafe { crate::provider::drive(context, &live, request, None) }
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
        crate::process::deny_describe_capability(context);
        let secret = resolve(context, s);
        let uses = crate::tls::bytes(&*uses);
        let Some(child) = std::str::from_utf8(uses)
            .ok()
            .and_then(|uses| secret.policy.derived(uses))
        else {
            return INVALID;
        };
        let charge = match admit((*context).payer(), &child.send, 32) {
            Ok(charge) => charge,
            Err(error) => return error,
        };
        let mut bytes = None;
        let args = KdfRequest {
            secret: &secret,
            prefix: crate::tls::bytes(&*prefix),
            data: crate::tls::bytes(&*data),
            #[cfg(feature = "record")]
            child: &child,
            output: &raw mut bytes,
        };
        let mut request = Request {
            operation: crate::provider::SECRET_KDF,
            mode: 0,
            handle: 0,
            path: (&raw const args).cast(),
            path_len: std::mem::size_of_val(&args),
            buffer: crate::net::empty_buffer(),
            site,
        };
        let outcome = drive_secret_sink(context, KDF_PROVIDER, &mut request);
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
        store_sealed(context, child, bytes, charge, Some(origin))
    }
}

pub(crate) struct MacRequest<'a> {
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
        if request.mode >= KEY_WRAP {
            return Step {
                kind: STEP_DONE,
                outcome: key_issue(request),
                deadline_ns: 0,
            };
        }
        let args = &*request.path.cast::<MacRequest<'_>>();
        let key = args.secret.bytes.as_deref().unwrap();
        let mut outcome = Outcome::default();
        if let Some(tag) = args.tag {
            outcome.value = u64::from(hirsel_core_net::verify_hmac_sha256(key, args.data, tag));
        } else {
            let tag = hirsel_core_net::hmac_sha256(key, args.data);
            request.buffer.data.cast::<[u8; 32]>().write(tag);
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
) -> Outcome {
    // SAFETY: the two entry points pin the Secret and all carriers.
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
        let args = MacRequest {
            secret: &secret,
            data: crate::tls::bytes(&*data),
            tag: tag.map(|tag| crate::tls::bytes(&*tag)),
        };
        let mut request = Request {
            operation: crate::provider::SECRET_MAC,
            mode: u8::from(tag.is_some()),
            handle: 0,
            path: (&raw const args).cast(),
            path_len: std::mem::size_of_val(&args),
            buffer: Buffer {
                data: out,
                len: 0,
                capacity: if tag.is_none() { 32 } else { 0 },
            },
            site,
        };
        drive_secret_sink(context, MAC_PROVIDER, &mut request)
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
    unsafe { mac(context, s, data, None, out, site) }
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
    unsafe { mac(context, s, data, Some(tag), std::ptr::null_mut(), site) }
}

/// Tape modes of SECRET_MAC for the key operations (recording.md).
pub(crate) const KEY_WRAP: u8 = 2;
pub(crate) const KEY_UNWRAP: u8 = 3;
pub(crate) const KEY_SIGN: u8 = 4;

/// `KeyError` as an outcome's error tag: the case index plus one.
const KEY_DENIED: u8 = 1;
const KEY_INVALID: u8 = 2;
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
        alg: hirsel_core_net::key_wrap::SignAlg,
        msg: &'a [u8],
    },
}

pub(crate) struct KeyRequest<'a> {
    /// The KEK, or the signing key.
    pub(crate) key: &'a Secret,
    pub(crate) op: KeyOp<'a>,
}

fn key_error(tag: u8) -> Outcome {
    Outcome {
        error: IoError { tag, code: 0 },
        ..Outcome::default()
    }
}

fn key_failure(failure: hirsel_core_net::key_wrap::KeyFailure) -> Outcome {
    use hirsel_core_net::key_wrap::KeyFailure;
    key_error(match failure {
        KeyFailure::Denied => KEY_DENIED,
        KeyFailure::Invalid => KEY_INVALID,
        KeyFailure::Tampered => KEY_TAMPERED,
    })
}

/// The key operation inside its frame: the only place key bytes are read.
unsafe fn key_issue(request: &mut Request) -> Outcome {
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
            KeyOp::Sign { alg, msg } => key_wrap::sign(*alg, key, msg),
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
            return Outcome {
                error: IoError {
                    tag: KEY_CAPACITY,
                    code: 0,
                },
                value: bytes.len() as u64,
                written: spare as u64,
                ..Outcome::default()
            };
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
        let tape = (*(*context).host_files).secret_mac_tape;
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
        let Some(requested) = policy(uses) else {
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
        let mut secrets = (*(*context).host_files)
            .registry
            .run_grants()
            .secrets
            .lock()
            .unwrap();
        let origin = Origin::Unwrapped {
            ordinal: secrets.unwraps,
            opaque: kek.origin.opaque(),
        };
        secrets.unwraps += 1;
        let value = secrets.push(
            Secret {
                origin,
                policy: requested,
                bytes: plain.into_inner().map(Arc::new),
                seal: Some(ChargeCell::new(charge)),
            },
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
    msg: *const Buffer,
    out: *mut Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let key = resolve(context, key);
        let Some(alg) = hirsel_core_net::key_wrap::SignAlg::from_case(alg) else {
            crate::process::stop(b"SignAlg")
        };
        if !key.policy.uses.permits(alg.needed()) {
            return key_error(KEY_DENIED);
        }
        let args = KeyRequest {
            key: &key,
            op: KeyOp::Sign {
                alg,
                msg: crate::tls::bytes(&*msg),
            },
        };
        key_op(context, &args, KEY_SIGN, out, site)
    }
}
