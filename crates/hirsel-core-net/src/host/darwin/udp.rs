//! UDP on Darwin: no batched syscalls, so a batch is a loop of `sendmsg`/`recvmsg`
//! that stops at the first refusal; no GSO or GRO; DF through `IP_DONTFRAG`;
//! `SO_NOSIGPIPE` set at creation; addresses carry their length byte.
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
};

/// One message of a batch, with the byte count the loop reports.
#[repr(C)]
pub(crate) struct Message {
    pub(crate) msg_hdr: libc::msghdr,
    pub(crate) msg_len: u32,
}
/// Control messages align to 32 bits (`__DARWIN_ALIGN32`).
pub(crate) const CMSG_ALIGN: usize = 4;
/// The send-side IPv4 TOS control carries an int: macOS 26 accepts a
/// one-byte IP_TOS control and sends the datagram unmarked.
pub(crate) type Tos = i32;
/// The control message type that reports a received IPv4 TOS byte.
pub(crate) const TOS_RECEIVED: i32 = libc::IP_RECVTOS;
pub(crate) const SEGMENT: Option<(i32, i32)> = None;
pub(crate) const COALESCED: Option<(i32, i32)> = None;
/// The largest UDP payload; a smaller receive vector asks `SO_NREAD` first.
const MAX_DATAGRAM: usize = 65_535;

/// A nonblocking close-on-exec UDP socket with ECN reception and DF set. Darwin
/// has no receive coalescing, so `coalesce` is moot.
pub(crate) fn socket(v6: bool, _coalesce: bool) -> io::Result<OwnedFd> {
    let family = if v6 { libc::AF_INET6 } else { libc::AF_INET };
    // SAFETY: socket has no caller memory operands and returns a fresh descriptor.
    let fd = super::super::checked(unsafe {
        libc::socket(family, libc::SOCK_DGRAM, libc::IPPROTO_UDP)
    })?;
    // SAFETY: the successful socket call transferred its ownership.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: integer fcntl requests on the owned descriptor.
    unsafe {
        super::super::checked(libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC))?;
        super::super::checked(libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK))?;
    }
    let option = |level, name, value| super::super::set_option(&fd, level, name, value);
    option(libc::SOL_SOCKET, libc::SO_NOSIGPIPE, 1)?;
    if v6 {
        option(libc::IPPROTO_IPV6, libc::IPV6_V6ONLY, 1)?;
        option(libc::IPPROTO_IPV6, libc::IPV6_RECVTCLASS, 1)?;
        option(libc::IPPROTO_IPV6, libc::IPV6_DONTFRAG, 1)?;
    } else {
        option(libc::IPPROTO_IP, libc::IP_RECVTOS, 1)?;
        option(libc::IPPROTO_IP, libc::IP_DONTFRAG, 1)?;
    }
    Ok(fd)
}

/// BSD addresses lead with their length.
pub(crate) fn sized(raw: &mut libc::sockaddr_storage, len: libc::socklen_t) {
    raw.ss_len = len as u8;
}

/// Send a batch without blocking: the count of messages sent, or -1 with errno
/// when the first could not be.
///
/// # Safety
/// Every header names live addresses, vectors and control data.
pub(crate) unsafe fn send(fd: RawFd, messages: &mut [Message]) -> i32 {
    for (sent, message) in messages.iter_mut().enumerate() {
        // SAFETY: by the contract.
        let n = unsafe { libc::sendmsg(fd, &message.msg_hdr, libc::MSG_DONTWAIT) };
        if n < 0 {
            return if sent == 0 { -1 } else { sent as i32 };
        }
        message.msg_len = n as u32;
    }
    messages.len() as i32
}

/// Receive a batch without blocking. Each `msg_len` is the datagram's whole
/// length: when the vector may truncate, `SO_NREAD` reports it before the read.
///
/// # Safety
/// Every header names live, writable address, control and payload space, with
/// exactly one vector.
pub(crate) unsafe fn receive(fd: RawFd, messages: &mut [Message]) -> i32 {
    for (received, message) in messages.iter_mut().enumerate() {
        // SAFETY: by the contract each header has one live vector.
        let room = unsafe { (*message.msg_hdr.msg_iov).iov_len };
        let mut pending: libc::c_int = 0;
        if room < MAX_DATAGRAM {
            let mut len = size_of::<libc::c_int>() as libc::socklen_t;
            // SAFETY: SO_NREAD writes one int.
            let rc = unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_NREAD,
                    (&raw mut pending).cast(),
                    &mut len,
                )
            };
            if rc < 0 {
                return if received == 0 { -1 } else { received as i32 };
            }
        }
        // SAFETY: by the contract.
        let n = unsafe { libc::recvmsg(fd, &mut message.msg_hdr, libc::MSG_DONTWAIT) };
        if n < 0 {
            return if received == 0 { -1 } else { received as i32 };
        }
        let truncated = message.msg_hdr.msg_flags & libc::MSG_TRUNC != 0;
        message.msg_len = if truncated {
            (pending as isize).max(n)
        } else {
            n
        } as u32;
    }
    messages.len() as i32
}
