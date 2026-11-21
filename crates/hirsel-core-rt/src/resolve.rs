//! NET-11 adapter. Only the provider holding a checked request can issue DNS.
use crate::{
    Context,
    entry::{hirsel_core_capability_handle, hirsel_core_capability_provider},
    net::{Address, NetworkRequest, Rules, deadline},
    provider::*,
};
use hirsel_core_net::{GrantRule, Ip, Name};
use std::{net::IpAddr, ptr};

#[repr(C)]
pub(crate) struct ResolveRequest {
    pub network: NetworkRequest,
    pub raw_name: Buffer,
    pub name: Option<Name>,
    pub job: Option<crate::os_resolve::Job>,
}

/// Called at the provider entry, before the replay point or any host query.
pub(crate) unsafe fn authorize(provider: &Provider, request: &mut Request) -> bool {
    // SAFETY: the source adapter pins the typed name and the Net designation.
    unsafe {
        #[cfg(feature = "record")]
        let handle = if crate::tape::owns(provider) {
            crate::tape::net_rules(provider, request.handle)
        } else {
            request.handle
        };
        #[cfg(not(feature = "record"))]
        let handle = {
            let _ = provider;
            request.handle
        };
        let resolve = &mut *request.path.cast_mut().cast::<ResolveRequest>();
        let Some(name) = std::str::from_utf8(crate::tls::bytes(&resolve.raw_name))
            .ok()
            .and_then(Name::parse)
        else {
            return false;
        };
        let rules = &*(handle as *const Rules);
        if !rules.permits(|rule| {
            rule.address.dir == 3
                && matches!(rule.decode(), Some(GrantRule::Resolve(rule)) if rule.permits(&name))
        }) {
            return false;
        }
        resolve.name = Some(name);
        true
    }
}

pub(crate) fn spare(buffer: &Buffer) -> usize {
    buffer.capacity.saturating_sub(buffer.len)
}

/// # Safety
/// Buffer names the caller's live list with writable spare Ip elements.
pub(crate) unsafe fn append(buffer: &mut Buffer, addresses: &[IpAddr]) -> Outcome {
    assert!(addresses.len() <= spare(buffer));
    for &address in addresses {
        // SAFETY: the checked spare has one typed slot per remaining address.
        unsafe {
            buffer
                .data
                .cast::<Ip>()
                .add(buffer.len)
                .write(address.into())
        };
        buffer.len += 1;
    }
    Outcome {
        value: addresses.len() as u64,
        ..Outcome::default()
    }
}

/// # Safety
/// Live run, typed Net and optional Clock grant, and the source's pinned
/// String and exclusively borrowed List[Ip] carriers.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_resolve(
    context: *mut Context,
    net: u64,
    name: *const Buffer,
    out: *mut Buffer,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> Outcome {
    // SAFETY: the source pins the request and both carriers through delivery.
    unsafe {
        let mut resolve = ResolveRequest {
            network: NetworkRequest {
                send_tail: ptr::null(),
                address: Address::default(),
                sockets: ptr::null_mut(),
                until: deadline(context, until_kind, clock, deadline_ns),
                backlog: 0,
                connecting: None,
                denied: false,
                direction: 0,
                os: &crate::os_resolve::OPERATIONS,
                tls: ptr::null_mut(),
                pending_write: false,
            },
            raw_name: *name,
            name: None,
            job: None,
        };
        let mut request = Request {
            operation: RESOLVE,
            mode: 0,
            handle: hirsel_core_capability_handle(context, net),
            path: (&raw mut resolve).cast(),
            path_len: size_of::<ResolveRequest>(),
            buffer: *out,
            site,
        };
        let provider = hirsel_core_capability_provider(context, net);
        let outcome = hirsel_core_host(context, provider, &mut request);
        if outcome.status == 0 && outcome.error == IoError::OK {
            (*out).len = request.buffer.len;
        }
        outcome
    }
}
