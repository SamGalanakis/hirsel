use super::{FULL_SLOT_BYTES, Options, socket_error};
use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6},
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    sync::{Arc, atomic::AtomicBool},
};

pub struct Socket {
    fd: Arc<OwnedFd>,
    pub(super) v6: bool,
    pub(super) gso: AtomicBool,
}

impl AsFd for Socket {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl Socket {
    pub fn open(v6: bool, options: Options) -> io::Result<Self> {
        let family = if v6 { libc::AF_INET6 } else { libc::AF_INET };
        // SAFETY: socket has no caller memory operands and returns a fresh descriptor.
        let fd = checked(unsafe {
            libc::socket(
                family,
                libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                libc::IPPROTO_UDP,
            )
        })?;
        // SAFETY: the successful socket call transferred its ownership.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let option = |level, name, value| set_option(fd.as_raw_fd(), level, name, value);
        if v6 {
            option(libc::IPPROTO_IPV6, libc::IPV6_V6ONLY, 1)?;
            option(libc::IPPROTO_IPV6, libc::IPV6_RECVTCLASS, 1)?;
            option(
                libc::IPPROTO_IPV6,
                libc::IPV6_MTU_DISCOVER,
                libc::IPV6_PMTUDISC_PROBE,
            )?;
        } else {
            option(libc::IPPROTO_IP, libc::IP_RECVTOS, 1)?;
            option(
                libc::IPPROTO_IP,
                libc::IP_MTU_DISCOVER,
                libc::IP_PMTUDISC_PROBE,
            )?;
        }
        if options.slot_bytes == FULL_SLOT_BYTES {
            option(libc::SOL_UDP, libc::UDP_GRO, 1)?;
        }
        Ok(Self {
            fd: Arc::new(fd),
            v6,
            gso: AtomicBool::new(true),
        })
    }

    pub fn descriptor(&self) -> Arc<OwnedFd> {
        self.fd.clone()
    }

    pub fn bind(&self, addr: SocketAddr) -> io::Result<SocketAddr> {
        let (raw, len) = address(addr);
        // SAFETY: raw contains an address of exactly len initialized bytes.
        checked(unsafe { libc::bind(self.fd.as_raw_fd(), (&raw const raw).cast(), len) })?;
        self.local_addr()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        // SAFETY: zero is valid for this C address storage.
        let mut raw: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        let mut len = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        // SAFETY: the checked descriptor is live and raw is writable up to len.
        checked(unsafe {
            libc::getsockname(self.fd.as_raw_fd(), (&raw mut raw).cast(), &mut len)
        })?;
        // SAFETY: getsockname initialized the reported address.
        unsafe { decode_address((&raw const raw).cast(), len) }
            .ok_or_else(|| socket_error(libc::EAFNOSUPPORT))
    }
}

fn set_option(fd: i32, level: i32, name: i32, value: i32) -> io::Result<()> {
    // SAFETY: options used by UDP take a single initialized native int.
    checked(unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            (&raw const value).cast(),
            size_of::<i32>() as _,
        )
    })?;
    Ok(())
}

pub(super) fn checked(result: i32) -> io::Result<i32> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

pub(super) fn address(addr: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: both sockaddr types fit this storage and require no greater alignment.
    let mut raw: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let storage = &raw mut raw;
    let len = match addr {
        SocketAddr::V4(addr) => {
            let v4 = libc::sockaddr_in {
                sin_family: libc::AF_INET as _,
                sin_port: addr.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(addr.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            // SAFETY: the storage holds the written C address.
            unsafe { storage.cast::<libc::sockaddr_in>().write(v4) };
            size_of::<libc::sockaddr_in>()
        }
        SocketAddr::V6(addr) => {
            let v6 = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as _,
                sin6_port: addr.port().to_be(),
                sin6_flowinfo: addr.flowinfo().to_be(),
                sin6_addr: libc::in6_addr {
                    s6_addr: addr.ip().octets(),
                },
                sin6_scope_id: addr.scope_id(),
            };
            // SAFETY: the storage holds the written C address.
            unsafe { storage.cast::<libc::sockaddr_in6>().write(v6) };
            size_of::<libc::sockaddr_in6>()
        }
    };
    (raw, len as libc::socklen_t)
}

/// # Safety
/// `raw` points to at least `len` initialized address bytes.
pub(super) unsafe fn decode_address(raw: *const u8, len: libc::socklen_t) -> Option<SocketAddr> {
    let len = len as usize;
    if len < size_of::<libc::sa_family_t>() {
        return None;
    }
    // SAFETY: the length checks cover the family and the selected address type.
    unsafe {
        match i32::from(raw.cast::<libc::sa_family_t>().read_unaligned()) {
            libc::AF_INET if len == size_of::<libc::sockaddr_in>() => {
                let v4 = raw.cast::<libc::sockaddr_in>().read_unaligned();
                let ip = Ipv4Addr::from(v4.sin_addr.s_addr.to_ne_bytes());
                Some(SocketAddr::from((ip, u16::from_be(v4.sin_port))))
            }
            libc::AF_INET6 if len == size_of::<libc::sockaddr_in6>() => {
                let v6 = raw.cast::<libc::sockaddr_in6>().read_unaligned();
                let ip = Ipv6Addr::from(v6.sin6_addr.s6_addr);
                Some(SocketAddrV6::new(ip, u16::from_be(v6.sin6_port), 0, v6.sin6_scope_id).into())
            }
            _ => None,
        }
    }
}
