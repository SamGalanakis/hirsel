//! TLS-9 server handshakes at the consuming provider entry: `tls.hello` reads
//! the ClientHello on an accepted socket, and `tls.accept` completes it with a
//! chain and a `tls_serve` key that only the provider reads.
use crate::{
    Context,
    net::{self, DENIED, NetworkRequest, TcpOrigin, TcpRoute, tls_failure},
    process::View,
    provider::*,
    tls::{bytes, view_bytes},
};
use hirsel_core_net::{SecretUse, tls};
use rustls::pki_types::CertificateDer;
use std::ptr;

/// The runtime's record of a live `Hello`, keyed by its socket's key: the
/// normalised name and the provider's paused handshake (none on replay).
/// Boxed, so `server_name`'s view stays put while other records come and go.
pub(crate) struct Hello {
    name: [u8; 253],
    /// A capacity-0 carrier viewing `name`, which `server_name` hands out.
    view: Buffer,
    accepted: Option<Box<rustls::server::Accepted>>,
}

/// The view of a missing record: a `server_name` result is never null.
struct EmptyName(Buffer);
// SAFETY: the buffer is empty and never written through.
unsafe impl Sync for EmptyName {}
static EMPTY_NAME: EmptyName = EmptyName(Buffer {
    data: ptr::NonNull::dangling().as_ptr(),
    len: 0,
    capacity: 0,
});

#[repr(C)]
pub struct HelloRequest {
    pub(crate) network: NetworkRequest,
    /// The normalised server name, capacity 253.
    pub name_out: *mut Buffer,
    pub(crate) state: *mut std::ffi::c_void,
}

#[derive(Default)]
pub(crate) struct HelloState {
    pub acceptor: Option<rustls::server::Acceptor>,
    pub accepted: Option<Box<rustls::server::Accepted>>,
    pub consumed: bool,
}

#[repr(C)]
pub struct TlsAcceptRequest {
    pub(crate) network: NetworkRequest,
    /// Entries, each a `u32` little-endian length and that many DER bytes.
    pub chain: View,
    /// The key's designation. Its bytes stay in run-owned memory.
    pub key: u64,
    /// As in `TLS_CONNECT`: length-prefixed protocols, in preference order.
    pub alpn: View,
    /// The chosen entry's index in `alpn`, `u64::MAX` for none.
    pub protocol_out: *mut u64,
    pub(crate) state: *mut std::ffi::c_void,
}

pub(crate) struct AcceptState {
    pub accepted: Option<Box<rustls::server::Accepted>>,
    /// The key's designation: its bytes (none on replay), origin and policy.
    pub key: crate::secret::Secret,
    pub chain: Vec<CertificateDer<'static>>,
    pub alpn: Vec<Vec<u8>>,
    pub handshake: Option<crate::os_tls::server::Handshake>,
    pub consumed: bool,
    pub pending_write: bool,
}

impl TlsAcceptRequest {
    #[cfg(feature = "record")]
    pub(crate) unsafe fn chain_entries(&self) -> Vec<&[u8]> {
        unsafe { chain_entries(view_bytes(&self.chain)) }.unwrap_or_default()
    }

    /// How many protocols the server's list names.
    #[cfg(feature = "record")]
    pub(crate) unsafe fn protocol_count(&self) -> u64 {
        let mut encoded = unsafe { view_bytes(&self.alpn) };
        let mut count = 0;
        while let Some((&len, tail)) = encoded.split_first() {
            encoded = tail.get(usize::from(len)..).unwrap_or_default();
            count += 1;
        }
        count
    }

    /// The index of `protocol` in the server's list.
    pub(crate) unsafe fn index_of(&self, protocol: &[u8]) -> Option<u64> {
        let mut encoded = unsafe { view_bytes(&self.alpn) };
        let mut index = 0;
        while let Some((&len, tail)) = encoded.split_first() {
            let (entry, rest) = tail.split_at(usize::from(len));
            if entry == protocol {
                return Some(index);
            }
            encoded = rest;
            index += 1;
        }
        None
    }
}

/// The run of `u32` little-endian lengths and DER bytes, in order.
pub(crate) fn chain_entries(mut run: &[u8]) -> Option<Vec<&[u8]>> {
    let mut entries = Vec::new();
    while !run.is_empty() {
        let (len, rest) = run.split_first_chunk::<4>()?;
        let (entry, rest) = rest.split_at_checked(u32::from_le_bytes(*len) as usize)?;
        entries.push(entry);
        run = rest;
    }
    Some(entries)
}

/// The OS descriptor of a socket the run holds through `key`, when the OS
/// provider holds it: none for a replayed or a provider-made socket.
unsafe fn os_fd(context: *mut Context, key: u64) -> Option<i32> {
    unsafe {
        let files = (*context).host_files;
        let provider = (*files).tcp_routes.get(&key)?.provider;
        #[cfg(feature = "record")]
        let (provider, source) = match (*files).net_tape {
            Some(tape) => crate::tape::source(&provider, tape.handle(key))?,
            None => (provider, key),
        };
        #[cfg(not(feature = "record"))]
        let source = key;
        crate::os::owns(&provider).then(|| (*files).sockets.fd(source))
    }
}

/// A dropped `Hello` declines the connection: one nonblocking attempt to
/// send `unrecognized_name`, before the socket's own drop closes it.
pub(crate) unsafe fn decline(context: *mut Context, key: u64) {
    unsafe {
        if (*(*context).host_files).tls_hellos.remove(&key).is_some()
            && let Some(fd) = os_fd(context, key)
        {
            crate::os_tls::server::send_alert(fd, &crate::os_tls::server::UNRECOGNIZED_NAME);
        }
    }
}

/// # Safety
/// The Socket is uniquely owned and is consumed on every return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_tls_hello(
    context: *mut Context,
    key: u64,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let files = (*context).host_files;
        // Only a socket a TCP listener accepted, a host's or a served Net's,
        // or its rejoined halves.
        let Some(&TcpRoute {
            provider,
            origin: TcpOrigin::Accepted,
        }) = (*files).tcp_routes.get(&key)
        else {
            net::hirsel_core_socket_drop(context, key);
            return DENIED;
        };
        // A served socket's handshake is its port's, never a tape's.
        #[cfg(feature = "record")]
        let tape = (*files)
            .net_tape
            .filter(|_| !crate::provide::owns(&provider));
        let mut name = [0u8; 253];
        let mut name_out = Buffer {
            data: name.as_mut_ptr(),
            len: 0,
            capacity: name.len(),
        };
        let mut state = HelloState::default();
        let mut hello = HelloRequest {
            network: request_network(context, files, until_kind, clock, deadline_ns),
            name_out: &raw mut name_out,
            state: (&raw mut state).cast(),
        };
        hello.network.os = &crate::os_net::HELLO_OPERATIONS;
        #[cfg(feature = "record")]
        let handle = tape.map_or(key, |tape| tape.handle(key));
        #[cfg(not(feature = "record"))]
        let handle = key;
        let mut request = Request {
            operation: TLS_HELLO,
            mode: 0,
            handle,
            path: (&raw mut hello).cast(),
            path_len: size_of::<HelloRequest>(),
            buffer: net::empty_buffer(),
            site,
        };
        let mut outcome = hirsel_core_host(context, &provider, &mut request);
        if outcome.status == 0 && outcome.error == IoError::OK {
            let len = name_out.len.min(name.len());
            let mut record = Box::new(Hello {
                name,
                view: net::empty_buffer(),
                accepted: state.accepted.take(),
            });
            record.view = Buffer {
                data: record.name.as_mut_ptr(),
                len,
                capacity: 0,
            };
            (*files).tls_hellos.insert(key, record);
            outcome.value = key;
        } else if state.consumed {
            (*files).tcp_routes.remove(&key);
        } else {
            net::hirsel_core_socket_drop(context, key);
        }
        outcome
    }
}

unsafe fn request_network(
    context: *mut Context,
    files: *mut crate::host_file::Files,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
) -> NetworkRequest {
    unsafe {
        NetworkRequest {
            send_tail: ptr::null(),
            address: net::Address::default(),
            sockets: &raw mut (*files).sockets,
            until: net::deadline(context, until_kind, clock, deadline_ns),
            backlog: 0,
            connecting: None,
            denial: None,
            public: false,
            direction: 0,
            os: &crate::os_net::HELLO_OPERATIONS,
            tls: &raw mut (*files).tls_sessions,
            tls_send_pending: false,
            max_receive: 0,
        }
    }
}

/// `server_name(h)`: `value` points to a carrier viewing the name in the
/// `Hello`'s record, as `env.name` does.
/// # Safety
/// `hello` is a live `Hello`; the view borrows it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_tls_server_name(
    context: *mut Context,
    hello: u64,
    _site: u64,
) -> Outcome {
    let view = unsafe { (*(*context).host_files).tls_hellos.get(&hello) }
        .map_or(&EMPTY_NAME.0, |record| &record.view);
    Outcome {
        value: ptr::from_ref(view) as u64,
        ..Outcome::default()
    }
}

/// A refusal before issue: one `internal_error` alert, then the close.
unsafe fn refuse(context: *mut Context, key: u64, outcome: Outcome) -> Outcome {
    unsafe {
        if let Some(fd) = os_fd(context, key) {
            crate::os_tls::server::send_alert(fd, &crate::os_tls::server::INTERNAL_ERROR);
        }
        net::hirsel_core_socket_drop(context, key);
        outcome
    }
}

/// # Safety
/// The Hello is uniquely owned and is consumed on every return. Views are
/// pinned through delivery and `protocol_out` is writable.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_tls_accept(
    context: *mut Context,
    hello: u64,
    chain: *const View,
    secret: u64,
    alpn: *const View,
    protocol_out: *mut u64,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> Outcome {
    unsafe {
        accept(
            context,
            hello,
            *chain,
            secret,
            *alpn,
            protocol_out,
            [until_kind as u64, clock, deadline_ns, site],
            false,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn accept(
    context: *mut Context,
    key: u64,
    chain: View,
    secret: u64,
    alpn: View,
    protocol_out: *mut u64,
    [until_kind, clock, deadline_ns, site]: [u64; 4],
    bad_alpn: bool,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let files = (*context).host_files;
        // The record leaves first, so no path below declines the name.
        let record = (*files).tls_hellos.remove(&key);
        let Some(&TcpRoute { provider, .. }) = (*files).tcp_routes.get(&key) else {
            net::hirsel_core_socket_drop(context, key);
            return DENIED;
        };
        let key_secret = crate::secret::resolve(context, secret);
        if !key_secret.policy.uses().permits(SecretUse::TLS_SERVE) {
            return refuse(context, key, DENIED);
        }
        let entries = chain_entries(view_bytes(&chain)).unwrap_or_default();
        if !tls::valid_chain(&entries) {
            return refuse(context, key, tls_failure(7, 0));
        }
        let Some(protocols) = protocols(view_bytes(&alpn)).filter(|_| !bad_alpn) else {
            return refuse(context, key, tls_failure(2, 0));
        };
        #[cfg(feature = "record")]
        let tape = (*files)
            .net_tape
            .filter(|_| !crate::provide::owns(&provider));
        let mut state = AcceptState {
            accepted: record.and_then(|record| record.accepted),
            key: key_secret,
            chain: entries
                .iter()
                .map(|entry| CertificateDer::from(entry.to_vec()))
                .collect(),
            alpn: protocols,
            handshake: None,
            consumed: false,
            pending_write: false,
        };
        *protocol_out = u64::MAX;
        let mut request_body = TlsAcceptRequest {
            network: request_network(context, files, until_kind as u8, clock, deadline_ns),
            chain,
            key: secret,
            alpn,
            protocol_out,
            state: (&raw mut state).cast(),
        };
        request_body.network.os = &crate::os_net::ACCEPT_OPERATIONS;
        #[cfg(feature = "record")]
        let handle = tape.map_or(key, |tape| tape.handle(key));
        #[cfg(not(feature = "record"))]
        let handle = key;
        let mut request = Request {
            operation: TLS_ACCEPT,
            mode: 0,
            handle,
            path: (&raw mut request_body).cast(),
            path_len: size_of::<TlsAcceptRequest>(),
            buffer: net::empty_buffer(),
            site,
        };
        #[cfg_attr(not(feature = "record"), allow(unused_mut))]
        let mut outcome = hirsel_core_host(context, &provider, &mut request);
        drop(state.handshake.take());
        drop(state.key.bytes.take());
        if outcome.status == 0 && outcome.error == IoError::OK {
            #[cfg(feature = "record")]
            if let Some(tape) = tape {
                outcome = tape.carrier(outcome);
            }
            (*files).tcp_routes.remove(&key);
            (*files).tcp_routes.insert(
                outcome.value,
                TcpRoute {
                    provider,
                    origin: TcpOrigin::Accepted,
                },
            );
            // A server half has no verified name: it authorizes no secret.
            (*files).tls_writes.insert(
                outcome.value,
                crate::secret::TlsWrite {
                    name: None,
                    spent: false,
                    binding: Default::default(),
                },
            );
        } else if state.consumed {
            (*files).tcp_routes.remove(&key);
        } else {
            net::hirsel_core_socket_drop(context, key);
        }
        outcome
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

/// FORM operand bridge: `chain` is a `List[Bytes]` and `alpn` a
/// `List[String]`. On success `written` is the chosen index plus one, or 0.
/// # Safety
/// The inputs are initialized FORM carriers pinned through delivery.
/// The Hello is consumed on every return.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_tls_accept(
    context: *mut Context,
    hello: u64,
    chain: *const Buffer,
    secret: u64,
    alpn: *const Buffer,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> Outcome {
    unsafe {
        let entry = |list: *const Buffer, i: usize| bytes(&*(*list).data.cast::<Buffer>().add(i));
        // An entry over u32 or a list over 8 cannot be a valid chain; the
        // check before issue refuses the run as packed.
        let mut packed = Vec::new();
        for i in 0..(*chain).len {
            let der = entry(chain, i);
            let Ok(len) = u32::try_from(der.len()) else {
                packed.clear();
                packed.extend_from_slice(&[0; 4]);
                break;
            };
            packed.extend_from_slice(&len.to_le_bytes());
            packed.extend_from_slice(der);
        }
        let mut encoded = [0; 16 * 256];
        let mut len = 0;
        let mut bad_alpn = (*alpn).len > 16;
        if !bad_alpn {
            for i in 0..(*alpn).len {
                let protocol = entry(alpn, i);
                if protocol.is_empty() || protocol.len() > 255 {
                    bad_alpn = true;
                    break;
                }
                encoded[len] = protocol.len() as u8;
                encoded[len + 1..len + 1 + protocol.len()].copy_from_slice(protocol);
                len += 1 + protocol.len();
            }
        }
        let mut chosen = u64::MAX;
        let mut outcome = accept(
            context,
            hello,
            View {
                data: packed.as_ptr(),
                len: packed.len(),
            },
            secret,
            View {
                data: encoded.as_ptr(),
                len,
            },
            &mut chosen,
            [until_kind as u64, clock, deadline_ns, site],
            bad_alpn,
        );
        if outcome.status == 0 && outcome.error == IoError::OK {
            outcome.written = chosen.wrapping_add(1);
        }
        outcome
    }
}
