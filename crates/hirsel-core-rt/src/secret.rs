//! TLS-5. Secret storage is never a language carrier.
use crate::{
    Context,
    entry::hirsel_core_capability_handle,
    host_file::DYNAMIC_GRANT,
    limit::Payer,
    provider::{Buffer, IoError, Outcome},
};
use hirsel_core_net::{Name, Pattern, SecretGrant, SecretSource};
use std::{io::Read, sync::Arc};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Copy)]
pub(crate) enum Origin {
    Granted(u64),
    Sealed(u64),
}

#[derive(Clone)]
pub(crate) struct Secret {
    pub(crate) origin: Origin,
    pub(crate) patterns: Arc<[Pattern]>,
    pub(crate) bytes: Option<Arc<Zeroizing<Vec<u8>>>>,
}

pub(crate) struct Granted {
    pub(crate) record: SecretGrant,
    pub(crate) secret: Secret,
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
                patterns: Pattern::parse_list(&record.patterns)?.into(),
                bytes: Some(Arc::new(bytes)),
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
}

struct Charge(Payer, u64);

impl Drop for Charge {
    fn drop(&mut self) {
        self.0.refund(self.1);
    }
}

#[derive(Default)]
pub(crate) struct Secrets {
    entries: Vec<(Secret, Option<Charge>)>,
    seals: u64,
}

impl Secrets {
    pub(crate) fn register(&mut self, secret: Secret) -> u64 {
        self.push(secret, None)
    }

    fn push(&mut self, secret: Secret, charge: Option<Charge>) -> u64 {
        let index = self.entries.len() as u64;
        self.entries.push((secret, charge));
        DYNAMIC_GRANT | index
    }
}

pub(crate) unsafe fn resolve(context: *const Context, handle: u64) -> Secret {
    unsafe {
        if handle & DYNAMIC_GRANT != 0 {
            (*(*context).host_files).secrets.lock().unwrap().entries
                [(handle & !DYNAMIC_GRANT) as usize]
                .0
                .clone()
        } else {
            let grant = hirsel_core_capability_handle(context, handle) as *const Granted;
            (*grant).secret.clone()
        }
    }
}

unsafe fn patterns(to: *const Buffer) -> Option<Box<[Pattern]>> {
    let bytes = unsafe { crate::tls::bytes(&*to) };
    Pattern::parse_list(std::str::from_utf8(bytes).ok()?)
}

pub(crate) const INVALID: Outcome = Outcome {
    error: IoError { tag: 1, code: 0 },
    value: 0,
    written: 0,
    status: 0,
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
        }),
    }
}

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
        let Some(patterns) = patterns(to) else {
            return INVALID;
        };
        let bytes = crate::tls::bytes(&*source);
        let charge = match admit((*context).payer(), &patterns, bytes.len() as u64) {
            Ok(charge) => charge,
            Err(error) => return error,
        };
        let owned = Zeroizing::new(bytes.to_vec());
        if (*source).capacity != 0 && (*source).len != 0 {
            std::slice::from_raw_parts_mut((*source).data, (*source).len).zeroize();
        }
        let mut secrets = (*(*context).host_files).secrets.lock().unwrap();
        let origin = Origin::Sealed(secrets.seals);
        secrets.seals += 1;
        let value = secrets.push(
            Secret {
                origin,
                patterns: patterns.into(),
                bytes: Some(Arc::new(owned)),
            },
            Some(charge),
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
        let Some(requested) = patterns(to) else {
            return INVALID;
        };
        let mut secret = resolve(context, s);
        let patterns = Pattern::intersection(&secret.patterns, &requested);
        if patterns.is_empty() {
            return INVALID;
        }
        let charge = match admit((*context).payer(), &patterns, 0) {
            Ok(charge) => charge,
            Err(error) => return error,
        };
        secret.patterns = patterns.into();
        let value = (*(*context).host_files)
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
}
