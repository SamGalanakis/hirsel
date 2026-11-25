//! The issue arguments of the TLS operations (recording.md, Sockets).
use super::*;

/// `TLS_CONNECT`'s name, trust and ALPN list; `TLS_ACCEPT`'s chain, key
/// designation and policy, and ALPN list. Other operations add nothing.
pub(super) unsafe fn args(request: &Request, args: &mut Vec<u8>) {
    // SAFETY: the network adapter pins the request's views and state.
    unsafe {
        if request.operation == TLS_CONNECT {
            let tls = &*request.path.cast::<crate::tls::TlsRequest>();
            let name = crate::tls::view_bytes(&tls.name);
            put_varint(args, name.len() as u64);
            args.extend_from_slice(name);
            match tls.pin() {
                hirsel_core_net::TlsTrust::System => args.push(0),
                hirsel_core_net::TlsTrust::Roots(digest) => {
                    args.push(1);
                    args.extend_from_slice(&digest);
                }
                hirsel_core_net::TlsTrust::Any => args.push(2),
            }
            let alpn = crate::tls::view_bytes(&tls.alpn);
            put_varint(args, alpn.len() as u64);
            args.extend_from_slice(alpn);
        }
        if request.operation == TLS_ACCEPT {
            // The chain in order, the key's designation and policy, and the
            // server's ALPN list; never the key's bytes (TLS-9).
            let tls = &*request.path.cast::<crate::tls_server::TlsAcceptRequest>();
            let entries = tls.chain_entries();
            put_varint(args, entries.len() as u64);
            for entry in entries {
                put_varint(args, entry.len() as u64);
                args.extend_from_slice(entry);
            }
            let key = &(*tls.state.cast::<crate::tls_server::AcceptState>()).key;
            key.origin.put(args);
            let policy = key.policy.text();
            put_varint(args, policy.len() as u64);
            args.extend_from_slice(policy.as_bytes());
            let alpn = crate::tls::view_bytes(&tls.alpn);
            put_varint(args, alpn.len() as u64);
            args.extend_from_slice(alpn);
        }
    }
}
