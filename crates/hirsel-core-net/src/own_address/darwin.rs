//! The macOS address-change source: `getifaddrs` for the snapshot and a
//! `PF_ROUTE` socket for the changes. A route message is a signal, not a delta:
//! any `RTM_NEWADDR` or `RTM_DELADDR`, a reported `ENOBUFS` (lost messages) or a
//! message that does not parse takes a new snapshot, so the table is never
//! built from a parsed `ifa_msghdr`.
use super::{Key, errno};
use std::collections::BTreeSet;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// The subscribed routing socket (omitted on iOS where PF_ROUTE is sandbox-denied).
pub(super) struct Changes {
    #[cfg(not(target_os = "ios"))]
    events: OwnedFd,
}

/// Open the routing socket, which receives every routing message the kernel
/// sends, address changes among them.
pub(super) fn subscribe() -> Result<Changes, i32> {
    #[cfg(target_os = "ios")]
    {
        Ok(Changes {})
    }
    #[cfg(not(target_os = "ios"))]
    {
        // SAFETY: plain socket creation; the descriptor is owned at once.
        let fd = unsafe { libc::socket(libc::PF_ROUTE, libc::SOCK_RAW, libc::AF_UNSPEC) };
        if fd < 0 {
            return Err(errno());
        }
        // SAFETY: `fd` is a fresh descriptor nothing else owns.
        let events = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: `events` is an open descriptor.
        if unsafe { libc::fcntl(events.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(errno());
        }
        Ok(Changes { events })
    }
}

impl Changes {
    /// Read every queued routing message. True when one changed an address or
    /// was lost, and the table must be read again.
    pub(super) fn drain(&mut self, _held: &mut BTreeSet<Key>) -> Result<bool, i32> {
        #[cfg(target_os = "ios")]
        {
            Ok(false)
        }
        #[cfg(not(target_os = "ios"))]
        {
            let mut changed = false;
            let mut buffer = vec![0u8; 1 << 16];
            loop {
                // SAFETY: `buffer` is writable for its length.
                let rc = unsafe {
                    libc::recv(
                        self.events.as_raw_fd(),
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                        libc::MSG_DONTWAIT,
                    )
                };
                if rc >= 0 {
                    // A routing socket delivers one message per read: `rtm_msglen`,
                    // `rtm_version`, `rtm_type` lead every header.
                    let message = &buffer[..rc as usize];
                    changed |= match message {
                        [_, _, version, kind, ..] if i32::from(*version) == libc::RTM_VERSION => {
                            matches!(i32::from(*kind), libc::RTM_NEWADDR | libc::RTM_DELADDR)
                        }
                        _ => true,
                    };
                    continue;
                }
                match errno() {
                    libc::EINTR => {}
                    libc::EAGAIN => return Ok(changed),
                    libc::ENOBUFS => changed = true,
                    e => return Err(e),
                }
            }
        }
    }
}

/// Every IPv4 and IPv6 address assigned now, from `getifaddrs`.
pub(super) fn snapshot() -> Result<BTreeSet<Key>, i32> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `list` receives a list this function frees below.
    if unsafe { libc::getifaddrs(&mut list) } < 0 {
        return Err(errno());
    }
    let mut held = BTreeSet::new();
    let mut entry = list;
    while !entry.is_null() {
        // SAFETY: `entry` is a node of the list `getifaddrs` returned.
        let ifa = unsafe { &*entry };
        entry = ifa.ifa_next;
        // SAFETY: a non-null `ifa_addr` and `ifa_netmask` point at sockaddrs
        // of `sa_len` bytes; `ifa_name` is a C string.
        let Some((family, ip)) = (unsafe { address(ifa.ifa_addr) }) else {
            continue;
        };
        // A netmask may be shortened to its nonzero bytes and carry no
        // family; it is read in the address's family, and a missing one is
        // a host route.
        let mask = unsafe { bytes(ifa.ifa_netmask) };
        let prefix = match family {
            0 => mask.map_or(32, |m| {
                u32::from_ne_bytes(m[4..8].try_into().unwrap()).count_ones()
            }),
            _ => mask.map_or(128, |m| {
                u128::from_ne_bytes(m[8..24].try_into().unwrap()).count_ones()
            }),
        } as u8;
        let index = unsafe { libc::if_nametoindex(ifa.ifa_name) };
        held.insert((family, ip, index, prefix));
    }
    // SAFETY: `list` came from `getifaddrs` and is freed once.
    unsafe { libc::freeifaddrs(list) };
    Ok(held)
}

/// The bytes of a sockaddr, at most `sa_len` of them read and the rest zero.
///
/// # Safety
/// `addr` is null or points at `sa_len` readable bytes.
unsafe fn bytes(addr: *const libc::sockaddr) -> Option<[u8; size_of::<libc::sockaddr_in6>()]> {
    if addr.is_null() {
        return None;
    }
    let mut out = [0u8; size_of::<libc::sockaddr_in6>()];
    // SAFETY: the caller's contract; at most `sa_len` bytes are read.
    let len = usize::from(unsafe { (*addr).sa_len }).min(out.len());
    unsafe { std::ptr::copy_nonoverlapping(addr.cast::<u8>(), out.as_mut_ptr(), len) };
    Some(out)
}

/// An IPv4 or IPv6 sockaddr as (family as in `Key`, its bits). A link-local
/// IPv6 address has its scope written into bits 16 to 31 by the kernel, which
/// is cleared so the key is the address.
///
/// # Safety
/// `addr` is null or points at `sa_len` readable bytes.
unsafe fn address(addr: *const libc::sockaddr) -> Option<(u8, u128)> {
    // SAFETY: the caller's contract.
    let raw = unsafe { bytes(addr) }?;
    // `sin_len`, `sin_family`, the port, then `sin_addr`; `sin6_addr` follows
    // `sin6_flowinfo`.
    match i32::from(raw[1]) {
        libc::AF_INET => Some((
            0,
            u128::from(u32::from_be_bytes(raw[4..8].try_into().unwrap())),
        )),
        libc::AF_INET6 => {
            let mut ip = u128::from_be_bytes(raw[8..24].try_into().unwrap());
            if ip >> 118 == 0x3fa {
                ip &= !(0xffff << 96);
            }
            Some((1, ip))
        }
        _ => None,
    }
}
