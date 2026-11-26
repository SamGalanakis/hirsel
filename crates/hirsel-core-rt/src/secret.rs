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
#[doc(hidden)]
pub mod agreement;
mod import_state;
#[cfg(test)]
mod import_tests;
pub(crate) mod kdf;
pub(crate) mod key;
pub(crate) mod key_share;
mod mac;
pub use mac::*;
pub(crate) mod p256_generation;
use hirsel_core_net::{
    ImportSpend, Name, Pattern, SecretGrant, SecretPolicy, SecretSource, SecretUse,
    secret_aead::SecretBytes,
};
pub use kdf::hirsel_core_secret_derive_hmac_sha256;
use std::{
    io::Read,
    sync::{Arc, Mutex, Weak},
};
use zeroize::{Zeroize, Zeroizing};

/// A designation (recording.md, "Designations across units"): what made it,
/// numbered in the order of the unit that made it, and that unit's stream.
#[derive(Clone, Copy)]
pub(crate) struct Origin {
    pub(crate) made: Made,
    pub(crate) stream: u32,
}

#[derive(Clone, Copy)]
pub(crate) enum Made {
    /// The run's grant: the root's.
    Granted(u64),
    /// The `ordinal`th seal of its unit.
    Sealed(u64),
    /// Designated by its `SECRET_KDF` delivery position under a tape (0
    /// otherwise); opaque when its parent was.
    Derived {
        ordinal: u64,
        opaque: bool,
    },
    /// The `ordinal`th `secret.unwrap` of its unit; opaque when its KEK was.
    Unwrapped {
        ordinal: u64,
        opaque: bool,
    },
    /// A credential fetch (TLS-7), whose bytes the host sealed, designated
    /// by its delivery position under a tape (0 otherwise).
    Fetched(u64),
    GeneratedP256(u64),
}

impl Origin {
    /// A designation of the root's stream: a grant, or one made outside a
    /// unit's recording.
    pub(crate) fn root(made: Made) -> Self {
        Self { made, stream: 0 }
    }

    /// A designation the running unit makes: its stream, or the root's
    /// outside a recording.
    pub(crate) fn here(made: Made) -> Self {
        #[cfg(feature = "record")]
        let stream = crate::tape::units::current_stream();
        #[cfg(not(feature = "record"))]
        let stream = 0;
        Self { made, stream }
    }

    /// Whether the program never held these bytes: a grant, or a secret
    /// derived or unwrapped from one (TLS-5).
    pub(crate) fn opaque(self) -> bool {
        match self.made {
            Made::Granted(_) | Made::Fetched(_) | Made::GeneratedP256(_) => true,
            Made::Sealed(_) => false,
            Made::Derived { opaque, .. } | Made::Unwrapped { opaque, .. } => opaque,
        }
    }

    /// The tape's origin kind and ordinal (recording.md, Credentials).
    #[cfg(feature = "record")]
    pub(crate) fn wire(self) -> (u8, u64) {
        match self.made {
            Made::Granted(ordinal) => (0, ordinal),
            Made::Sealed(ordinal) => (1, ordinal),
            Made::Derived { ordinal, .. } => (2, ordinal),
            Made::Unwrapped { ordinal, .. } => (3, ordinal),
            Made::Fetched(ordinal) => (4, ordinal),
            Made::GeneratedP256(ordinal) => (5, ordinal),
        }
    }

    /// The designation as every tape field writes it: origin kind `u8`,
    /// stream `u32`, ordinal `varint`.
    #[cfg(feature = "record")]
    pub(crate) fn put(self, out: &mut Vec<u8>) {
        let (kind, ordinal) = self.wire();
        out.push(kind);
        out.extend_from_slice(&self.stream.to_le_bytes());
        hirsel_tape_format::put_varint(out, ordinal);
    }
}

/// The running unit's stream and its next seal (or unwrap) ordinal, in
/// that unit's own order. Taken before the Secrets table is locked: a
/// unit's recording cell is never locked under it.
fn next_in_unit(unwrap: bool) -> (u32, u64) {
    #[cfg(feature = "record")]
    return crate::tape::units::next_designation(unwrap);
    #[cfg(not(feature = "record"))]
    {
        let _ = unwrap;
        (0, 0)
    }
}

#[derive(Clone)]
pub(crate) struct Secret {
    pub(crate) origin: Origin,
    pub(crate) policy: SecretPolicy,
    pub(crate) bytes: Option<Arc<SecretBytes>>,
    /// A sealed secret's charge, shared by every designation of its bytes.
    pub(crate) seal: Option<Arc<ChargeCell>>,
    pub(crate) import: Option<Arc<ImportSpend>>,
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
            secret: Secret::new(
                Origin::root(Made::Granted(index)),
                SecretPolicy::parse(&record.policy)?,
                Some(Arc::new(SecretBytes::new(bytes))),
                None,
            ),
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
    /// The name `tls.connect` verified. A half from `tls.accept` has none
    /// (TLS-9), so no secret is ever sent or proved on it.
    pub(crate) name: Option<Name>,
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
pub(crate) struct ChargeCell {
    charge: Mutex<Option<Charge>>,
    /// A UDP pool's `RETIRED`, reached under `charge`'s lock: the end take
    /// that adopts the charge makes it the owner's.
    #[cfg(feature = "record")]
    retirement: Mutex<Option<crate::tape::Retirement>>,
    #[cfg(feature = "record")]
    retired: std::sync::atomic::AtomicBool,
    /// A key object's or registry entry's survivor name: kind, stream and
    /// its number among the charges that unit tracked.
    #[cfg(feature = "record")]
    pub(crate) made: Option<(u8, u32, u64)>,
}

// SAFETY: an outstanding charge pins its payer slot, and a refund settles from
// any unit (MEM-20).
unsafe impl Send for ChargeCell {}
// SAFETY: the charge is reached only under its mutex.
unsafe impl Sync for ChargeCell {}

impl ChargeCell {
    pub(crate) fn new(charge: Charge) -> Arc<Self> {
        Arc::new(Self {
            charge: Mutex::new(Some(charge)),
            #[cfg(feature = "record")]
            retirement: Mutex::new(None),
            #[cfg(feature = "record")]
            retired: std::sync::atomic::AtomicBool::new(false),
            #[cfg(feature = "record")]
            made: None,
        })
    }

    /// A media charge's cell, named as its helper-end survivor.
    #[cfg_attr(not(feature = "record"), allow(unused_variables))]
    pub(crate) fn made(charge: Charge, kind: u8) -> Arc<Self> {
        #[cfg_attr(not(feature = "record"), allow(unused_mut))]
        let mut cell = Self::new(charge);
        #[cfg(feature = "record")]
        {
            let (stream, ordinal) = crate::tape::units::next_made();
            Arc::get_mut(&mut cell).unwrap().made = Some((kind, stream, ordinal));
        }
        cell
    }

    pub(crate) fn take_charge(&self, payer: Payer) -> Option<Charge> {
        let mut charge = self.charge.lock().unwrap();
        if charge.as_ref().is_some_and(|c| c.0.same_domain(payer)) {
            charge.take()
        } else {
            None
        }
    }

    #[cfg(feature = "record")]
    pub(crate) fn record_retirement(&self, retirement: crate::tape::Retirement) {
        let _charge = self.charge.lock().unwrap();
        assert!(
            self.retirement
                .lock()
                .unwrap()
                .replace(retirement)
                .is_none()
        );
    }

    #[cfg(feature = "record")]
    pub(crate) fn retire(&self) {
        let mut charge = self.charge.lock().unwrap();
        let Some(retirement) = *self.retirement.lock().unwrap() else {
            return;
        };
        self.retired
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let previous = charge.take();
        drop(charge);
        if let Some(previous) = previous {
            previous.retire(retirement);
        }
    }

    #[cfg(feature = "record")]
    pub(crate) fn refund_replayed(&self, payer: Payer) -> bool {
        let mut charge = self.charge.lock().unwrap();
        if !charge.as_ref().is_some_and(|c| c.0.is_current_owner(payer)) {
            return false;
        }
        let previous = charge.take();
        drop(charge);
        drop(previous);
        true
    }
}

impl Charge {
    pub(crate) fn new(payer: Payer, bytes: u64) -> Self {
        Self(payer, bytes)
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.1
    }

    pub(crate) fn partition(&mut self, bytes: u64) -> Self {
        self.1 -= bytes;
        Self(self.0, bytes)
    }

    #[cfg(feature = "record")]
    fn retire(self, retirement: crate::tape::Retirement) {
        let charge = std::mem::ManuallyDrop::new(self);
        charge.0.post_retirement(charge.1, retirement);
    }
}

/// A designation still held when the unit that paid for it ended, and its bytes.
pub(crate) type HandedOver = Vec<HandedCharge>;

pub(crate) struct HandedCharge {
    weak: Weak<ChargeCell>,
    bytes: u64,
    /// Its survivor name: kind, origin kind for a designation, stream, ordinal.
    #[cfg(feature = "record")]
    pub(crate) name: Option<(u8, Option<u8>, u32, u64)>,
    // A pool can physically retire between the hand-over and the end take.
    // Keep its pending tape identity until the new payer can receive the input.
    #[cfg(feature = "record")]
    retiring: Option<Arc<ChargeCell>>,
}

impl HandedCharge {
    pub(crate) fn new(cell: &Arc<ChargeCell>, bytes: u64) -> Self {
        #[cfg(feature = "record")]
        let retirement = *cell.retirement.lock().unwrap();
        Self {
            weak: Arc::downgrade(cell),
            bytes,
            #[cfg(feature = "record")]
            name: retirement
                .map(|r| r.survivor())
                .or(cell.made)
                .map(|(kind, stream, ordinal)| (kind, None, stream, ordinal)),
            #[cfg(feature = "record")]
            retiring: retirement.is_some().then(|| cell.clone()),
        }
    }
}

impl HandedCharge {
    #[cfg(feature = "record")]
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// The owner's end take moves each still-held charge to its part of the
/// helper's reservation, and gives back the indices of the charges it took:
/// the survivors its `TAKE` lists. A pool's `RETIRED` becomes the owner's.
pub(crate) fn take_over(handed: &HandedOver, mut payer: impl FnMut(u64) -> Payer) -> Vec<usize> {
    let mut taken = Vec::new();
    for (index, handed) in handed.iter().enumerate() {
        if let Some(cell) = handed.weak.upgrade() {
            let mut charge = cell.charge.lock().unwrap();
            if charge.is_none() {
                taken.push(index);
                let adopted = Charge(payer(handed.bytes), handed.bytes);
                #[cfg(feature = "record")]
                if handed.retiring.is_some() {
                    let mut retirement = cell.retirement.lock().unwrap();
                    let retirement = retirement.as_mut().unwrap();
                    retirement.adopt();
                    if cell.retired.load(std::sync::atomic::Ordering::Relaxed) {
                        let retirement = *retirement;
                        drop(charge);
                        adopted.retire(retirement);
                        continue;
                    }
                }
                *charge = Some(adopted);
            }
        }
    }
    taken
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
            for (_kind, cell) in [(2, &entry.charge), (7, &entry.secret.seal)] {
                let Some(cell) = cell else { continue };
                if let Some(c) = cell.take_charge(payer) {
                    #[cfg_attr(not(feature = "record"), allow(unused_mut))]
                    let mut charge = HandedCharge::new(cell, c.1);
                    // Every designation of sealed bytes shares their origin,
                    // so the seal's name is the same from any of them.
                    #[cfg(feature = "record")]
                    {
                        let (origin, ordinal) = entry.secret.origin.wire();
                        charge.name =
                            Some((_kind, Some(origin), entry.secret.origin.stream, ordinal));
                    }
                    handed.push(charge);
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
    admit_size(payer, SecretGrant::logical_size(policies, len))
}

fn admit_size(payer: Payer, size: Option<u64>) -> Result<Charge, Outcome> {
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
    bytes: Option<Arc<SecretBytes>>,
    charge: Charge,
    origin: Option<Origin>,
) -> Outcome {
    let origin = origin.unwrap_or_else(|| {
        let (stream, ordinal) = next_in_unit(false);
        Origin {
            made: Made::Sealed(ordinal),
            stream,
        }
    });
    let mut secrets = unsafe {
        (*(*context).host_files)
            .registry
            .run_grants()
            .secrets
            .lock()
            .unwrap()
    };
    let value = secrets.push(
        Secret::new(origin, policy, bytes, Some(ChargeCell::new(charge))),
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
        let owned = Arc::new(SecretBytes::new(Zeroizing::new(bytes.to_vec())));
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
    /// SEND_SECRET: the whole message, built from the request's `wire`
    /// buffer and the marks in `send_tail`. SCRAM: the proof, sent between
    /// the request's buffer and its tail.
    pub secret: Buffer,
    /// Each secret's origin and `tls_send` patterns, in `secrets` order.
    pub(crate) designations: Vec<(Origin, Arc<[Pattern]>)>,
    pub(crate) committed: usize,
    /// On a served stream: the Secrets its server receives, narrowed to the
    /// stream's name (`provide.secret`), and `secret` is empty.
    pub(crate) narrowed: Option<Vec<Secret>>,
}

#[cfg(feature = "record")]
#[derive(Clone, Copy)]
pub(crate) struct MacTape {
    pub(crate) provider: Provider,
    pub(crate) handle: u64,
}

pub(crate) unsafe fn drive_secret_sink(
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
