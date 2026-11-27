//! The embedder-helper socket protocol (EMB-3 "Exec in an embedded launch").
//! A frame is a little-endian `u32` length, a tag byte and its fields. The
//! descriptors a frame carries travel as `SCM_RIGHTS` on the `sendmsg` that
//! carries its first byte, so they are queued before the frame is complete.
use std::collections::VecDeque;
use std::io;
use std::mem;
#[cfg(target_vendor = "apple")]
use std::os::fd::AsRawFd;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

// Embedder to helper.
pub(super) const SPAWN: u8 = 1;
pub(super) const SIGNAL: u8 = 2;
pub(super) const RELEASE: u8 = 3;
// Helper to embedder.
pub(super) const READY: u8 = 0x81;
pub(super) const REFUSED: u8 = 0x82;
pub(super) const SPAWNED: u8 = 0x83;
pub(super) const SPAWN_FAILED: u8 = 0x84;
pub(super) const EXITED: u8 = 0x85;
pub(super) const WAIT_FAILED: u8 = 0x86;

const MAX_FDS: usize = 4;

/// The release and protocol identity `READY` carries. The embedder refuses a
/// helper whose identity differs from its own, so a helper from another build
/// never serves this protocol (EMB-3). It hashes the protocol's three source
/// files, so any change to either end changes it.
pub(super) fn identity() -> String {
    const SOURCES: [&[u8]; 3] = [
        include_bytes!("wire.rs"),
        include_bytes!("serve.rs"),
        include_bytes!("client.rs"),
    ];
    const HASH: u64 = {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut file = 0;
        while file < SOURCES.len() {
            let mut at = 0;
            while at < SOURCES[file].len() {
                hash = (hash ^ SOURCES[file][at] as u64).wrapping_mul(0x0100_0000_01b3);
                at += 1;
            }
            file += 1;
        }
        hash
    };
    format!("{}-{HASH:016x}", env!("CARGO_PKG_VERSION"))
}

pub(super) struct Frame(Vec<u8>);

impl Frame {
    pub(super) fn new(tag: u8) -> Self {
        Self(vec![0, 0, 0, 0, tag])
    }

    pub(super) fn u8(mut self, value: u8) -> Self {
        self.0.push(value);
        self
    }

    pub(super) fn u64(mut self, value: u64) -> Self {
        self.0.extend(value.to_le_bytes());
        self
    }

    pub(super) fn bytes(mut self, value: &[u8]) -> Self {
        self.0.extend((value.len() as u32).to_le_bytes());
        self.0.extend(value);
        self
    }

    /// Send the frame and `fds` on `socket`. A peer that closed its end gives
    /// `EPIPE`, never `SIGPIPE`.
    pub(super) fn send(mut self, socket: RawFd, fds: &[RawFd]) -> io::Result<()> {
        let len = (self.0.len() - 4) as u32;
        self.0[..4].copy_from_slice(&len.to_le_bytes());
        let mut sent = 0;
        let mut control = [0u64; 8];
        while sent < self.0.len() {
            let mut iov = libc::iovec {
                iov_base: self.0[sent..].as_ptr().cast_mut().cast(),
                iov_len: self.0.len() - sent,
            };
            // SAFETY: msghdr fields are plain data; the pointers below live
            // through sendmsg.
            let mut message: libc::msghdr = unsafe { mem::zeroed() };
            message.msg_iov = &raw mut iov;
            message.msg_iovlen = 1;
            if sent == 0 && !fds.is_empty() {
                let bytes = mem::size_of_val(fds) as u32;
                // SAFETY: control holds CMSG_SPACE for MAX_FDS descriptors.
                unsafe {
                    message.msg_control = control.as_mut_ptr().cast();
                    message.msg_controllen = libc::CMSG_SPACE(bytes) as _;
                    let header = libc::CMSG_FIRSTHDR(&message);
                    (*header).cmsg_level = libc::SOL_SOCKET;
                    (*header).cmsg_type = libc::SCM_RIGHTS;
                    (*header).cmsg_len = libc::CMSG_LEN(bytes) as _;
                    std::ptr::copy_nonoverlapping(
                        fds.as_ptr(),
                        libc::CMSG_DATA(header).cast::<RawFd>(),
                        fds.len(),
                    );
                }
            }
            // SAFETY: message describes live buffers.
            let n = unsafe { libc::sendmsg(socket, &message, NOSIGNAL) };
            if n < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            sent += n as usize;
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
const NOSIGNAL: libc::c_int = libc::MSG_NOSIGNAL;
// Darwin sockets of this protocol set SO_NOSIGPIPE at creation.
#[cfg(target_vendor = "apple")]
const NOSIGNAL: libc::c_int = 0;

/// The fields of one received frame, read in order.
pub(super) struct Fields<'a>(pub(super) &'a [u8]);

impl<'a> Fields<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    pub(super) fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub(super) fn bytes(&mut self) -> io::Result<&'a [u8]> {
        let len = u32::from_le_bytes(self.take(4)?.try_into().unwrap());
        self.take(len as usize)
    }
}

/// The receiving end: buffered bytes and the descriptors received with them.
pub(super) struct Inbox {
    socket: RawFd,
    bytes: Vec<u8>,
    fds: VecDeque<OwnedFd>,
}

impl Inbox {
    pub(super) fn new(socket: RawFd) -> Self {
        Self {
            socket,
            bytes: Vec::new(),
            fds: VecDeque::new(),
        }
    }

    /// The next frame's tag and fields, or `None` once the peer has closed.
    pub(super) fn next(&mut self) -> io::Result<Option<(u8, Vec<u8>)>> {
        loop {
            if self.bytes.len() >= 5 {
                let len = u32::from_le_bytes(self.bytes[..4].try_into().unwrap()) as usize;
                if self.bytes.len() >= 4 + len {
                    let frame: Vec<u8> = self.bytes.drain(..4 + len).skip(4).collect();
                    return Ok(Some((frame[0], frame[1..].to_vec())));
                }
            }
            if !self.receive()? {
                return Ok(None);
            }
        }
    }

    pub(super) fn fd(&mut self) -> io::Result<OwnedFd> {
        self.fds
            .pop_front()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))
    }

    fn receive(&mut self) -> io::Result<bool> {
        let mut buffer = [0u8; 16384];
        let mut control = [0u64; 8];
        loop {
            let mut iov = libc::iovec {
                iov_base: buffer.as_mut_ptr().cast(),
                iov_len: buffer.len(),
            };
            // SAFETY: plain data; the pointers below live through recvmsg.
            let mut message: libc::msghdr = unsafe { mem::zeroed() };
            message.msg_iov = &raw mut iov;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = mem::size_of_val(&control) as _;
            // SAFETY: message describes live buffers.
            let n = unsafe { libc::recvmsg(self.socket, &mut message, CMSG_CLOEXEC) };
            if n < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            // SAFETY: recvmsg filled message.msg_control with valid headers.
            unsafe {
                let mut header = libc::CMSG_FIRSTHDR(&message);
                while !header.is_null() {
                    if (*header).cmsg_level == libc::SOL_SOCKET
                        && (*header).cmsg_type == libc::SCM_RIGHTS
                    {
                        let data = libc::CMSG_DATA(header).cast::<RawFd>();
                        let len = ((*header).cmsg_len as usize - (data as usize - header as usize))
                            / mem::size_of::<RawFd>();
                        for at in 0..len.min(MAX_FDS) {
                            let fd = OwnedFd::from_raw_fd(data.add(at).read_unaligned());
                            #[cfg(target_vendor = "apple")]
                            libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
                            self.fds.push_back(fd);
                        }
                    }
                    header = libc::CMSG_NXTHDR(&message, header);
                }
            }
            if message.msg_flags & libc::MSG_CTRUNC != 0 {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            self.bytes.extend_from_slice(&buffer[..n as usize]);
            return Ok(n != 0);
        }
    }
}

#[cfg(target_os = "linux")]
const CMSG_CLOEXEC: libc::c_int = libc::MSG_CMSG_CLOEXEC;
#[cfg(target_vendor = "apple")]
const CMSG_CLOEXEC: libc::c_int = 0;

/// A connected close-on-exec stream pair: the embedder's end and the helper's.
pub(super) fn pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut ends = [-1; 2];
    #[cfg(target_os = "linux")]
    let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    #[cfg(target_vendor = "apple")]
    let kind = libc::SOCK_STREAM;
    // SAFETY: socketpair writes two descriptors on success.
    if unsafe { libc::socketpair(libc::AF_UNIX, kind, 0, ends.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socketpair returned two fresh descriptors.
    let ends = unsafe { [OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])] };
    #[cfg(target_vendor = "apple")]
    for end in &ends {
        quiet(end.as_raw_fd())?;
        // SAFETY: a live descriptor.
        if unsafe { libc::fcntl(end.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let [ours, theirs] = ends;
    Ok((ours, theirs))
}

/// Darwin: a send on a closed socket answers `EPIPE` instead of raising
/// `SIGPIPE`. Each process sets it on its own end.
#[cfg(target_vendor = "apple")]
pub(super) fn quiet(fd: RawFd) -> io::Result<()> {
    let on: libc::c_int = 1;
    // SAFETY: SO_NOSIGPIPE reads one int.
    let set = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            (&raw const on).cast(),
            mem::size_of_val(&on) as libc::socklen_t,
        )
    };
    if set < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
