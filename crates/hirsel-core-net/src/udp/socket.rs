use super::{FULL_SLOT_BYTES, Options, socket_error};
pub(super) use crate::host::checked;
use crate::host::udp as host;
use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6},
    os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
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
        let fd = host::socket(v6, options.slot_bytes == FULL_SLOT_BYTES)?;
        Ok(Self {
            fd: Arc::new(fd),
            v6,
            gso: AtomicBool::new(host::SEGMENT.is_some()),
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

pub(super) fn address(addr: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: both sockaddr types fit this storage and require no greater alignment.
    let mut raw: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let storage = &raw mut raw;
    let len = match addr {
        SocketAddr::V4(addr) => {
            // SAFETY: zero is valid for the C address's integer fields.
            let mut v4: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            v4.sin_family = libc::AF_INET as _;
            v4.sin_port = addr.port().to_be();
            v4.sin_addr.s_addr = u32::from_ne_bytes(addr.ip().octets());
            // SAFETY: the storage holds the written C address.
            unsafe { storage.cast::<libc::sockaddr_in>().write(v4) };
            size_of::<libc::sockaddr_in>()
        }
        SocketAddr::V6(addr) => {
            // SAFETY: zero is valid for the C address's integer fields.
            let mut v6: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
            v6.sin6_family = libc::AF_INET6 as _;
            v6.sin6_port = addr.port().to_be();
            v6.sin6_flowinfo = addr.flowinfo().to_be();
            v6.sin6_addr.s6_addr = addr.ip().octets();
            v6.sin6_scope_id = addr.scope_id();
            // SAFETY: the storage holds the written C address.
            unsafe { storage.cast::<libc::sockaddr_in6>().write(v6) };
            size_of::<libc::sockaddr_in6>()
        }
    };
    host::sized(&mut raw, len as libc::socklen_t);
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
