use super::*;

/// # Safety
/// Context and Net remain live through delivery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_udp_open(
    context: *mut Context,
    net: u64,
    v6: bool,
    slots: u32,
    slot_bytes: u32,
    site: u64,
) -> Outcome {
    let endpoint = Address {
        family: u8::from(v6),
        ..Address::default()
    };
    unsafe {
        create(
            context,
            net,
            Creation {
                endpoint,
                slots,
                slot_bytes,
                group: None,
                output: ptr::null_mut(),
            },
            UDP_OPEN,
            site,
        )
    }
    .outcome()
}

/// # Safety
/// Context and Net remain live through delivery.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_udp_bind(
    context: *mut Context,
    net: u64,
    family: u8,
    hi: u64,
    lo: u64,
    port: u16,
    slots: u32,
    slot_bytes: u32,
    site: u64,
) -> AddressOutcome {
    let endpoint = Address {
        family,
        hi,
        lo,
        port,
    };
    unsafe {
        create(
            context,
            net,
            Creation {
                endpoint,
                slots,
                slot_bytes,
                group: None,
                output: ptr::null_mut(),
            },
            UDP_BIND,
            site,
        )
    }
}

/// # Safety
/// Context, Net, and the output list remain live through delivery.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_udp_bind_group(
    context: *mut Context,
    net: u64,
    family: u8,
    hi: u64,
    lo: u64,
    port: u16,
    slots: u32,
    slot_bytes: u32,
    members: u16,
    steer: u32,
    out: *mut Buffer,
    site: u64,
) -> AddressOutcome {
    if members < 2 || steer > 1 || unsafe { (*out).capacity - (*out).len } < usize::from(members) {
        return AddressOutcome::new(DENIED, Address::default());
    }
    let endpoint = Address {
        family,
        hi,
        lo,
        port,
    };
    let members = NonZeroU16::new(members).unwrap();
    let steer = if steer == 0 { Steer::Hash } else { Steer::Quic };
    unsafe {
        create(
            context,
            net,
            Creation {
                endpoint,
                slots,
                slot_bytes,
                group: Some((members, steer)),
                output: out,
            },
            UDP_BIND_GROUP,
            site,
        )
    }
}

/// # Safety
/// Context, socket, and writable output carriers remain live through delivery.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_udp_recv(
    context: *mut Context,
    key: u64,
    bytes: *mut Buffer,
    packets: *mut Buffer,
    until_kind: u8,
    clock: u64,
    at: u64,
    site: u64,
) -> Outcome {
    unsafe {
        existing(
            context, key, UDP_RECV, bytes, packets, until_kind, clock, at, site,
        )
    }
    .outcome()
}

/// # Safety
/// Context, socket, and borrowed input carriers remain live through delivery.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_udp_send(
    context: *mut Context,
    key: u64,
    bytes: *const Buffer,
    packets: *const Buffer,
    until_kind: u8,
    clock: u64,
    at: u64,
    site: u64,
) -> Outcome {
    unsafe {
        existing(
            context,
            key,
            UDP_SEND,
            bytes.cast_mut(),
            packets.cast_mut(),
            until_kind,
            clock,
            at,
            site,
        )
    }
    .outcome()
}

/// # Safety
/// Context and socket remain live through delivery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_udp_local_addr(
    context: *mut Context,
    key: u64,
    site: u64,
) -> AddressOutcome {
    unsafe {
        existing(
            context,
            key,
            UDP_LOCAL_ADDR,
            ptr::null_mut(),
            ptr::null_mut(),
            0,
            0,
            0,
            site,
        )
    }
}
