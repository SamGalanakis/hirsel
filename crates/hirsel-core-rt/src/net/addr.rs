use super::*;

/// Check an address before issue. A tape records the denial for its inner grant
/// and delivers it at its ordinary provider boundary.
pub(crate) unsafe fn authorize(provider: &Provider, request: &mut Request) -> Option<Outcome> {
    if unix_operation(request.operation) {
        // SAFETY: the adapter pins this extension through the provider call.
        return unsafe { !(*request.path.cast::<crate::unix_net::UnixRequest>()).authorized }
            .then_some(DENIED);
    }
    if request.operation == RESOLVE {
        // SAFETY: the resolve adapter pins its request and the Net grant.
        return unsafe { !crate::resolve::authorize(provider, request) }.then_some(DENIED);
    }
    if !matches!(request.operation, CONNECT | LISTEN) {
        return None;
    }
    // SAFETY: CONNECT and LISTEN carry the Net adapter's pinned extension.
    unsafe {
        #[cfg(feature = "record")]
        if crate::tape::owns(provider) {
            if let Some((_, handle)) = crate::tape::source(provider, request.handle) {
                let denial = request_denial(request, handle);
                (*request.path.cast_mut().cast::<NetworkRequest>()).denial = denial;
            }
            return None;
        }
        #[cfg(not(feature = "record"))]
        let _ = provider;
        request_denial(request, request.handle).map(crate::os_net::signal)
    }
}

unsafe fn request_denial(request: &Request, handle: u64) -> Option<u8> {
    // SAFETY: network adapters pin the scalar address and the grant's Rules.
    unsafe {
        let address = &(*request.path.cast::<NetworkRequest>()).address;
        let rules = (&*(handle as *const Rules)).as_slice();
        hirsel_core_net::address_denial(
            rules.iter().map(|row| row.address),
            u8::from(request.operation == LISTEN),
            address.family,
            address.hi,
            address.lo,
            address.port,
        )
    }
}

/// SAFETY: handle is the live Net grant's immutable rule slice.
pub(crate) unsafe fn permits_address(handle: u64, address: &Address, dir: u8) -> bool {
    unsafe {
        let rules = &*(handle as *const Rules);
        rules.permits(|r| {
            r.address
                .permits(dir, address.family, address.hi, address.lo, address.port)
        })
    }
}

/// # Safety
/// A live TCP socket or listener from this run, borrowed through delivery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_local_addr(
    context: *mut Context,
    key: u64,
    site: u64,
) -> AddressOutcome {
    // SAFETY: the caller pins the carrier for this operation.
    unsafe { query_address(context, key, LOCAL_ADDR, site) }
}

/// # Safety
/// A live socket from this run, borrowed through delivery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_peer_addr(
    context: *mut Context,
    key: u64,
    site: u64,
) -> AddressOutcome {
    // SAFETY: the caller pins the carrier for this operation.
    unsafe { query_address(context, key, PEER_ADDR, site) }
}

unsafe fn query_address(context: *mut Context, key: u64, op: u8, site: u64) -> AddressOutcome {
    // SAFETY: the borrowed carrier belongs to this run. The operation has no
    // buffer or deadline.
    unsafe {
        owned_op(
            context,
            op,
            key,
            ptr::null_mut(),
            ptr::null(),
            0,
            0,
            0,
            0,
            site,
        )
    }
}

#[cfg(feature = "record")]
pub(crate) fn address_bytes(address: Address) -> [u8; 19] {
    let mut bytes = [0; 19];
    bytes[0] = address.family;
    bytes[1..9].copy_from_slice(&address.hi.to_le_bytes());
    bytes[9..17].copy_from_slice(&address.lo.to_le_bytes());
    bytes[17..].copy_from_slice(&address.port.to_le_bytes());
    bytes
}

#[cfg(feature = "record")]
pub(crate) unsafe fn denial(request: &Request) -> Option<u8> {
    if matches!(request.operation, CONNECT | LISTEN) {
        // SAFETY: the network adapter supplies this operand for address operations.
        unsafe { (*request.path.cast::<NetworkRequest>()).denial }
    } else {
        None
    }
}
