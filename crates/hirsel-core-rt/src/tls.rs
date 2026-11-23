//! TLS-1 and TLS-3 checks at the consuming provider entry.
use crate::{
    Context,
    entry::{hirsel_core_capability_handle, hirsel_core_capability_provider},
    net::{self, DENIED, NetworkRequest, Rules, tls_failure},
    process::View,
    provider::*,
};
use hirsel_core_net::{GrantRule, Name, TlsTrust, tls};
use std::{ptr, sync::Arc};

#[repr(C)]
pub struct TlsRequest {
    pub(crate) network: NetworkRequest,
    pub name: View,
    pub trust: u8,
    pub roots: Buffer,
    pub alpn: View,
    pub protocol_out: *mut Buffer,
    pub peer_out: *mut [u8; 32],
    pub binding_out: *mut Buffer,
    pub(crate) state: *mut std::ffi::c_void,
}

const _: () = assert!(size_of::<NetworkRequest>() == 176);

pub(crate) struct HandshakeState {
    pin: TlsTrust,
    pub name: Option<Name>,
    pub roots: Option<Arc<rustls::RootCertStore>>,
    pub alpn: Vec<Vec<u8>>,
    pub cacheable: bool,
    pub stream: Option<crate::os_tls::Stream>,
    pub consumed: bool,
}

pub(crate) unsafe fn bytes(buffer: &Buffer) -> &[u8] {
    if buffer.len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(buffer.data, buffer.len) }
    }
}

pub(crate) unsafe fn view_bytes(view: &View) -> &[u8] {
    if view.len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(view.data, view.len) }
    }
}

impl TlsRequest {
    pub(crate) unsafe fn pin(&self) -> TlsTrust {
        unsafe { (*self.state.cast::<HandshakeState>()).pin }
    }

    pub(crate) unsafe fn binding(&self) -> &[u8] {
        let buffer = unsafe { &*self.binding_out };
        if buffer.len <= buffer.capacity && matches!(buffer.len, 0 | 32 | 48 | 64) {
            unsafe { bytes(buffer) }
        } else {
            &[]
        }
    }

    pub(crate) unsafe fn offers(&self, protocol: &[u8]) -> bool {
        let mut encoded = unsafe { view_bytes(&self.alpn) };
        while let Some((&len, tail)) = encoded.split_first() {
            let (entry, rest) = tail.split_at(usize::from(len));
            if entry == protocol {
                return true;
            }
            encoded = rest;
        }
        false
    }
}

fn protocols(mut encoded: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut offered = Vec::new();
    while let Some((&len, tail)) = encoded.split_first() {
        let (protocol, rest) = tail.split_at_checked(usize::from(len))?;
        if offered.len() == 16 {
            return None;
        }
        offered.push(protocol.to_vec());
        encoded = rest;
    }
    tls::valid_protocols(&offered).then_some(offered)
}

/// # Safety
/// The Socket is uniquely owned and is consumed on every return. Views and
/// initialized input buffers are pinned through delivery. protocol_out pins
/// 255 writable bytes, and peer_out pins 32 writable bytes.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_tls_connect(
    context: *mut Context,
    n: u64,
    key: u64,
    name: *const View,
    trust: u8,
    roots: *const Buffer,
    alpn: *const View,
    protocol_out: *mut Buffer,
    peer_out: *mut [u8; 32],
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> Outcome {
    unsafe {
        connect(
            context,
            n,
            key,
            *name,
            trust,
            *roots,
            *alpn,
            protocol_out,
            peer_out,
            until_kind,
            clock,
            deadline_ns,
            site,
            false,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn connect(
    context: *mut Context,
    n: u64,
    key: u64,
    name: View,
    trust: u8,
    roots: Buffer,
    alpn: View,
    protocol_out: *mut Buffer,
    peer_out: *mut [u8; 32],
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
    bad_alpn: bool,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let files = (*context).host_files;
        let prepared = {
            let _commit = crate::sequencer::enter();
            prepare(context, files, n, key, name, trust, &roots, alpn, bad_alpn)
        };
        let (address, provider, mut state) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                net::hirsel_core_socket_drop(context, key);
                return error;
            }
        };
        // A served socket's handshake is its port's, never a tape's carrier.
        let served = crate::provide::owns(&provider);
        #[cfg(feature = "record")]
        let tape = (*files).net_tape.filter(|_| !served);
        #[cfg(feature = "record")]
        let source = match tape {
            Some(tape) => crate::tape::source(&provider, tape.handle(key)).map(|(p, _)| p),
            None => Some(provider),
        };
        #[cfg(not(feature = "record"))]
        let source = Some(provider);
        if state.roots.is_none() && source.as_ref().is_some_and(crate::os::owns) {
            if crate::tasks::check_cancel(context, site) != 0 {
                net::hirsel_core_socket_drop(context, key);
                return CANCELLED;
            }
            let cache = {
                let _commit = crate::sequencer::enter();
                (*files).tls.system.clone()
            };
            // The first System call can read files. Load them on the caller's
            // stack, outside the scheduler's socket callback and registry lock.
            state.roots = Some(cache.get_or_init(crate::os_tls::system_roots).clone());
        }
        let verified = state.name.as_ref().unwrap().clone();
        let canonical = match &verified {
            Name::Dns(name) => name.to_string(),
            Name::Ip(ip) => ip.to_string(),
        };
        let mut binding_bytes = [0; 64];
        let mut binding_out = Buffer {
            data: binding_bytes.as_mut_ptr(),
            len: 0,
            capacity: binding_bytes.len(),
        };
        let mut tls = TlsRequest {
            network: NetworkRequest {
                send_tail: ptr::null(),
                address,
                sockets: &raw mut (*files).sockets,
                until: net::deadline(context, until_kind, clock, deadline_ns),
                backlog: 0,
                connecting: None,
                denied: false,
                direction: 0,
                os: &crate::os_net::TLS_OPERATIONS,
                tls: &raw mut (*files).tls,
                pending_write: false,
                max_receive: 0,
            },
            name: View {
                data: canonical.as_ptr(),
                len: canonical.len(),
            },
            trust,
            roots,
            alpn,
            protocol_out,
            peer_out,
            binding_out: &raw mut binding_out,
            state: (&raw mut state).cast(),
        };
        #[cfg(feature = "record")]
        let handle = tape.map_or(key, |tape| tape.handle(key));
        #[cfg(not(feature = "record"))]
        let handle = key;
        let mut request = Request {
            operation: TLS_CONNECT,
            mode: 0,
            handle,
            path: (&raw mut tls).cast(),
            path_len: size_of::<TlsRequest>(),
            buffer: net::empty_buffer(),
            site,
        };
        #[cfg_attr(not(feature = "record"), allow(unused_mut))]
        let mut outcome = hirsel_core_host(context, &provider, &mut request);
        if outcome.status == 0 && outcome.error == IoError::OK {
            let _commit = crate::sequencer::enter();
            #[cfg(feature = "record")]
            if let Some(tape) = tape {
                outcome = tape.carrier(outcome);
            }
            (*files).connected.remove(&key);
            (*files)
                .connected
                .insert(outcome.value, (address, provider));
            (*files).tls_writes.insert(
                outcome.value,
                crate::secret::TlsWrite {
                    name: verified,
                    spent: false,
                    binding: hirsel_core_net::scram::Binding::from_bytes(tls.binding())
                        .unwrap_or_default(),
                },
            );
        } else if state.consumed && !served {
            let _commit = crate::sequencer::enter();
            (*files).connected.remove(&key);
        } else {
            net::hirsel_core_socket_drop(context, key);
        }
        outcome
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn prepare(
    context: *mut Context,
    files: *mut crate::host_file::Files,
    n: u64,
    key: u64,
    name: View,
    trust: u8,
    roots: &Buffer,
    alpn: View,
    bad_alpn: bool,
) -> Result<(net::Address, Provider, HandshakeState), Outcome> {
    unsafe {
        let (address, provider) = (*files).connected.get(&key).copied().ok_or(DENIED)?;
        // A served Net's sockets handshake through that Net, and only it:
        // never another provider's, which could not answer for them.
        let net_provider = &*hirsel_core_capability_provider(context, n);
        if (crate::provide::owns(&provider) || crate::provide::owns(net_provider))
            && provider.state != net_provider.state
        {
            return Err(DENIED);
        }
        let name = tls::verification_name(view_bytes(&name)).ok_or_else(|| tls_failure(1, 0))?;
        if bad_alpn {
            return Err(tls_failure(2, 0));
        }
        let alpn = protocols(view_bytes(&alpn)).ok_or_else(|| tls_failure(2, 0))?;
        let pem = if trust == 1 { bytes(roots) } else { &[] };
        let cacheable = pem.len() <= crate::os_tls::CACHED_PEM_BYTES;
        let cached = if trust == 1 && cacheable {
            (*files).tls.custom_roots(pem)
        } else {
            None
        };
        let (pin, store, remember) = match cached {
            Some((pin, store)) => (pin, Some(store), false),
            None => {
                let (pin, store) = tls::trust(trust, pem).ok_or_else(|| tls_failure(3, 0))?;
                (pin, store.map(Arc::new), cacheable)
            }
        };
        let grant = hirsel_core_capability_handle(context, n);
        #[cfg(feature = "record")]
        let rules = {
            let grant_provider = &*hirsel_core_capability_provider(context, n);
            if crate::tape::owns(grant_provider) {
                crate::tape::net_rules(grant_provider, grant)
            } else {
                grant
            }
        };
        #[cfg(not(feature = "record"))]
        let rules = grant;
        let rules = &*(rules as *const Rules);
        if !rules.as_slice().iter().filter(|rule| rule.address.dir == 2).any(|rule| {
            matches!(rule.decode(), Some(GrantRule::Tls(rule)) if rule.permits(&name, address.port, pin))
        }) {
            return Err(DENIED);
        }
        if remember && let Some(store) = &store {
            (*files).tls.remember_roots(pem, pin, store.clone());
        }
        Ok((
            address,
            provider,
            HandshakeState {
                pin,
                name: Some(name),
                roots: store,
                alpn,
                cacheable,
                stream: None,
                consumed: false,
            },
        ))
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ConnectOutcome {
    pub outcome: Outcome,
    pub peer: [u64; 4],
}

/// FORM operand bridge. The provider ABI sees only the packed offer, and the
/// native result sees the selected index and four network-order digest words.
/// # Safety
/// The inputs are initialized FORM carriers pinned through delivery.
/// The Socket is consumed on every return.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_tls_connect(
    context: *mut Context,
    n: u64,
    key: u64,
    name: *const Buffer,
    trust: u8,
    roots: *const Buffer,
    alpn: *const Buffer,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> ConnectOutcome {
    unsafe {
        let name = View {
            data: (*name).data,
            len: (*name).len,
        };
        let roots = if trust == 0 {
            net::empty_buffer()
        } else {
            *roots
        };
        let mut encoded = [0; 16 * 256];
        let mut len = 0;
        let mut bad_alpn = (*alpn).len > 16;
        let mut offered: [&[u8]; 16] = [&[]; 16];
        if !bad_alpn {
            for (i, offered) in offered.iter_mut().enumerate().take((*alpn).len) {
                let entry = bytes(&*(*alpn).data.cast::<Buffer>().add(i));
                *offered = entry;
                if entry.is_empty() || entry.len() > 255 {
                    bad_alpn = true;
                    break;
                }
                encoded[len] = entry.len() as u8;
                encoded[len + 1..len + 1 + entry.len()].copy_from_slice(entry);
                len += 1 + entry.len();
            }
        }
        let alpn = View {
            data: encoded.as_ptr(),
            len,
        };
        let mut protocol = [0u8; 255];
        let mut protocol_out = Buffer {
            data: protocol.as_mut_ptr(),
            len: 0,
            capacity: protocol.len(),
        };
        let mut digest = [0; 32];
        let mut outcome = connect(
            context,
            n,
            key,
            name,
            trust,
            roots,
            alpn,
            &mut protocol_out,
            &mut digest,
            until_kind,
            clock,
            deadline_ns,
            site,
            bad_alpn,
        );
        let mut peer = [0; 4];
        if outcome.status == 0 && outcome.error == IoError::OK {
            peer = tls::peer_words(digest);
            if protocol_out.len != 0 {
                let chosen = &protocol[..protocol_out.len];
                outcome.written =
                    offered.iter().position(|entry| *entry == chosen).unwrap() as u64 + 1;
            }
        }
        ConnectOutcome { outcome, peer }
    }
}

#[cfg(test)]
mod tests;
