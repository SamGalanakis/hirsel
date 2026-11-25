//! UDP on Linux: batched `sendmmsg`/`recvmmsg`, GSO (`UDP_SEGMENT`) and GRO
//! (`UDP_GRO`), and path-MTU probing that sets DF without trusting the cache.
use std::{
    io,
    os::fd::{FromRawFd, OwnedFd, RawFd},
};

/// One message of a batch, with the byte count the kernel reports.
pub(crate) type Message = libc::mmsghdr;
/// Control messages align to the C `long`.
pub(crate) const CMSG_ALIGN: usize = size_of::<usize>();
/// The send-side IPv4 TOS control carries a C `int`.
pub(crate) type Tos = i32;
/// The control message type that reports a received IPv4 TOS byte.
pub(crate) const TOS_RECEIVED: i32 = libc::IP_TOS;
/// The control message that sends a train of equal segments, when the host has one.
pub(crate) const SEGMENT: Option<(i32, i32)> = Some((libc::SOL_UDP, libc::UDP_SEGMENT));
/// The control message that reports a coalesced receive's segment size.
pub(crate) const COALESCED: Option<(i32, i32)> = Some((libc::SOL_UDP, libc::UDP_GRO));

/// A nonblocking close-on-exec UDP socket with ECN reception and DF set. `coalesce`
/// asks the kernel to coalesce received trains.
pub(crate) fn socket(v6: bool, coalesce: bool) -> io::Result<OwnedFd> {
    let family = if v6 { libc::AF_INET6 } else { libc::AF_INET };
    // SAFETY: socket has no caller memory operands and returns a fresh descriptor.
    let fd = super::super::checked(unsafe {
        libc::socket(
            family,
            libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            libc::IPPROTO_UDP,
        )
    })?;
    // SAFETY: the successful socket call transferred its ownership.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let option = |level, name, value| super::super::set_option(&fd, level, name, value);
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
    if coalesce {
        option(libc::SOL_UDP, libc::UDP_GRO, 1)?;
    }
    Ok(fd)
}

/// Linux addresses carry no length byte.
pub(crate) fn sized(_raw: &mut libc::sockaddr_storage, _len: libc::socklen_t) {}

/// Send a batch without blocking: the count of messages sent, or -1 with errno
/// when the first could not be.
///
/// # Safety
/// Every header names live addresses, vectors and control data.
pub(crate) unsafe fn send(fd: RawFd, messages: &mut [Message]) -> i32 {
    // SAFETY: by the contract.
    unsafe {
        libc::sendmmsg(
            fd,
            messages.as_mut_ptr(),
            messages.len() as u32,
            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
        )
    }
}

/// Receive a batch without blocking. Each `msg_len` is the datagram's whole
/// length, even when the vector truncated it.
///
/// # Safety
/// Every header names live, writable address, control and payload space.
pub(crate) unsafe fn receive(fd: RawFd, messages: &mut [Message]) -> i32 {
    // SAFETY: by the contract.
    unsafe {
        libc::recvmmsg(
            fd,
            messages.as_mut_ptr(),
            messages.len() as u32,
            libc::MSG_DONTWAIT | libc::MSG_TRUNC,
            std::ptr::null_mut(),
        )
    }
}
