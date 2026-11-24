use super::*;

pub(super) unsafe fn address(a: Address) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: zero is a valid initial representation of these C address structs.
    unsafe {
        let mut storage: libc::sockaddr_storage = zeroed();
        let len = if a.family == 0 {
            let v4 = storage_as_mut::<libc::sockaddr_in>(&mut storage);
            v4.sin_family = libc::AF_INET as _;
            #[cfg(target_os = "macos")]
            {
                v4.sin_len = size_of::<libc::sockaddr_in>() as u8;
            }
            v4.sin_port = a.port.to_be();
            v4.sin_addr.s_addr = (a.lo as u32).to_be();
            size_of::<libc::sockaddr_in>()
        } else {
            let v6 = storage_as_mut::<libc::sockaddr_in6>(&mut storage);
            v6.sin6_family = libc::AF_INET6 as _;
            #[cfg(target_os = "macos")]
            {
                v6.sin6_len = size_of::<libc::sockaddr_in6>() as u8;
            }
            v6.sin6_port = a.port.to_be();
            v6.sin6_addr.s6_addr = ((u128::from(a.hi) << 64) | u128::from(a.lo)).to_be_bytes();
            size_of::<libc::sockaddr_in6>()
        };
        (storage, len as libc::socklen_t)
    }
}
unsafe fn storage_as_mut<T>(storage: &mut libc::sockaddr_storage) -> &mut T {
    // SAFETY: callers select the family-specific C struct that fits storage.
    unsafe { &mut *(storage as *mut libc::sockaddr_storage).cast::<T>() }
}
pub(super) unsafe fn decoded(storage: &mut libc::sockaddr_storage) -> Address {
    unsafe {
        if storage.ss_family as i32 == libc::AF_INET {
            let a = storage_as_mut::<libc::sockaddr_in>(storage);
            Address {
                family: 0,
                hi: 0,
                lo: u64::from(u32::from_be(a.sin_addr.s_addr)),
                port: u16::from_be(a.sin_port),
            }
        } else {
            let a = storage_as_mut::<libc::sockaddr_in6>(storage);
            let ip = u128::from_be_bytes(a.sin6_addr.s6_addr);
            Address {
                family: 1,
                hi: (ip >> 64) as u64,
                lo: ip as u64,
                port: u16::from_be(a.sin6_port),
            }
        }
    }
}
pub(super) fn query_address(fd: i32, op: u8, out: &mut Address) -> Result<Outcome, i32> {
    // SAFETY: the kernel writes one address into our local storage.
    unsafe {
        let mut storage: libc::sockaddr_storage = zeroed();
        let mut len = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        let read = if op == LOCAL_ADDR {
            libc::getsockname
        } else {
            libc::getpeername
        };
        retry(|| read(fd, (&raw mut storage).cast(), &mut len) as isize).and_then(|_| match storage
            .ss_family
            as i32
        {
            libc::AF_INET | libc::AF_INET6 => {
                *out = decoded(&mut storage);
                Ok(Outcome::default())
            }
            _ => Err(libc::EAFNOSUPPORT),
        })
    }
}

pub(crate) fn is_operation(op: u8) -> bool {
    matches!(
        op,
        CONNECT
            | LISTEN
            | ACCEPT
            | LOCAL_ADDR
            | PEER_ADDR
            | RECV_BOUNDED
            | RECV_READY
            | SEND_PAIR
            | TLS_CONNECT
            | SEND_SECRET
            | SCRAM
            | RESOLVE
            | CRED_FETCH
    )
}
