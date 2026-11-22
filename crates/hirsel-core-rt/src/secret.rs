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
}

impl Secrets {
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
        let mut secrets = (*(*context).host_files)
            .registry
            .run_grants()
            .secrets
            .lock()
            .unwrap();
        let origin = Origin::Sealed(secrets.seals);
        secrets.seals += 1;
        let value = secrets.push(
            Secret {
                origin,
                policy,
                bytes: Some(owned),
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
    unreachable!("Secret MAC never waits")
}

unsafe extern "C" fn mac_abort(_: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
    unreachable!("Secret MAC never waits")
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
        #[cfg(feature = "record")]
        let tape = (*(*context).host_files).secret_mac_tape;
        #[cfg(feature = "record")]
        let (p, h) = tape.map_or((MAC_PROVIDER, 0), |tape| (tape.provider, tape.handle));
        #[cfg(not(feature = "record"))]
        let (p, h) = (MAC_PROVIDER, 0);
        let mut request = Request {
            operation: crate::provider::SECRET_MAC,
            mode: u8::from(tag.is_some()),
            handle: h,
            path: (&raw const args).cast(),
            path_len: std::mem::size_of_val(&args),
            buffer: Buffer {
                data: out,
                len: 0,
                capacity: if tag.is_none() { 32 } else { 0 },
            },
            site,
        };
        crate::provider::drive(context, &p, &mut request, None)
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
