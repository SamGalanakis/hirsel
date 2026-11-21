//! Run-owned environment data and CAP-7 attenuation.
use crate::{
    Context, entry,
    provider::{Buffer, IoError, Outcome},
};
use hirsel_core_net::{EnvPatterns, EnvSnapshot};
use std::{os::unix::ffi::OsStringExt, ptr, sync::Arc};

struct Entry {
    name: String,
    _value: Vec<u8>,
    name_view: Buffer,
    value_view: Option<Buffer>,
}

// The carriers point into this entry's immutable heap allocations. A language
// view borrows the entry and never acquires write or drop authority.
unsafe impl Send for Entry {}
unsafe impl Sync for Entry {}

pub(crate) struct Env {
    patterns: EnvPatterns,
    entries: Arc<[Entry]>,
}

impl Env {
    pub(crate) fn snapshot(patterns: EnvPatterns) -> Self {
        let mut entries: Vec<_> = std::env::vars_os()
            .filter_map(|(name, value)| {
                let name = name.into_string().ok()?;
                patterns.covers(&name).then(|| (name, value.into_vec()))
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|a, b| a.0 == b.0);
        Self::new(EnvSnapshot { patterns, entries })
    }

    pub(crate) fn new(snapshot: EnvSnapshot) -> Self {
        let view = |bytes: &[u8]| Buffer {
            data: bytes.as_ptr().cast_mut(),
            len: bytes.len(),
            capacity: 0,
        };
        Self {
            patterns: snapshot.patterns,
            entries: snapshot
                .entries
                .into_iter()
                .map(|(name, value)| Entry {
                    name_view: view(name.as_bytes()),
                    value_view: std::str::from_utf8(&value).ok().map(|s| view(s.as_bytes())),
                    name,
                    _value: value,
                })
                .collect(),
        }
    }

    #[cfg(feature = "record")]
    pub(crate) fn record(&self) -> EnvSnapshot {
        EnvSnapshot {
            patterns: self.patterns.clone(),
            entries: self
                .entries
                .iter()
                .map(|row| (row.name.clone(), row._value.clone()))
                .collect(),
        }
    }

    pub(crate) fn handle(&self) -> u64 {
        ptr::from_ref(self) as u64
    }

    fn covered(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|row| self.patterns.covers(&row.name))
    }
}

pub(crate) struct Grant {
    _env: Box<Env>,
    payer: crate::limit::Payer,
    charge: u64,
}

impl Drop for Grant {
    fn drop(&mut self) {
        self.payer.refund(self.charge);
    }
}

/// # Safety
/// `handle` is the pinned Env designation in this run's grant registry.
pub(crate) unsafe fn from_handle<'a>(handle: u64) -> &'a Env {
    unsafe { &*(handle as *const Env) }
}

unsafe fn env<'a>(context: *mut Context, grant: u64) -> &'a Env {
    unsafe {
        crate::process::deny_describe_capability(context);
        from_handle(entry::hirsel_core_capability_handle(context, grant))
    }
}

/// # Safety
/// `grant` is a live Env. `name` borrows a live String carrier for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_env_get(
    context: *mut Context,
    grant: u64,
    name: *const Buffer,
    _site: u64,
) -> Outcome {
    unsafe {
        let env = env(context, grant);
        let name = std::str::from_utf8_unchecked(crate::tls::bytes(&*name));
        if !env.patterns.covers(name) {
            return crate::os_net::signal(1);
        }
        let Ok(i) = env
            .entries
            .binary_search_by(|row| row.name.as_str().cmp(name))
        else {
            return Outcome::default();
        };
        let Some(view) = &env.entries[i].value_view else {
            return crate::os_net::signal(2);
        };
        Outcome {
            value: ptr::from_ref(view) as u64,
            ..Outcome::default()
        }
    }
}

/// # Safety
/// `grant` is a live Env in the context's run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_env_len(
    context: *mut Context,
    grant: u64,
    _site: u64,
) -> Outcome {
    Outcome {
        value: unsafe { env(context, grant) }.covered().count() as u64,
        ..Outcome::default()
    }
}

/// # Safety
/// `grant` is a live Env in the context's run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_env_name(
    context: *mut Context,
    grant: u64,
    index: u64,
    _site: u64,
) -> Outcome {
    let value = unsafe { env(context, grant) }
        .covered()
        .nth(index as usize)
        .map_or(0, |row| ptr::from_ref(&row.name_view) as u64);
    Outcome {
        value,
        ..Outcome::default()
    }
}

/// # Safety
/// `grant` is a live Env. `names` borrows a live String carrier for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_env_narrow(
    context: *mut Context,
    grant: u64,
    names: *const Buffer,
    _site: u64,
) -> Outcome {
    unsafe {
        let env = env(context, grant);
        let names = std::str::from_utf8_unchecked(crate::tls::bytes(&*names));
        let Some(filter) = EnvPatterns::parse(names) else {
            return crate::os_net::signal(1);
        };
        let patterns = env.patterns.intersect(&filter);
        let charge = patterns.charge();
        let payer = (*context).payer();
        if let Err(capacity) = payer.admit(charge) {
            return Outcome {
                value: capacity.requested,
                error: IoError { tag: 2, code: 0 },
                written: capacity.available,
                ..Outcome::default()
            };
        }
        let narrowed = Box::new(Env {
            patterns,
            entries: Arc::clone(&env.entries),
        });
        let handle = narrowed.handle();
        let provider = *entry::hirsel_core_capability_provider(context, grant);
        let files = &*(*context).host_files;
        files.registry.run.env_grants.lock().unwrap().push(Grant {
            _env: narrowed,
            payer,
            charge: charge.unwrap(),
        });
        Outcome {
            value: files.registry.grant(provider, handle),
            ..Outcome::default()
        }
    }
}
