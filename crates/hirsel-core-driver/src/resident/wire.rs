//! The resident compiler's messages: a little-endian length, then the bytes,
//! with the descriptors a request lends on its first bytes.
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

pub(super) fn write_message(
    stream: &mut UnixStream,
    bytes: &[u8],
    fds: &[RawFd],
) -> io::Result<()> {
    let mut framed = (bytes.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(bytes);
    if fds.is_empty() {
        return stream.write_all(&framed);
    }
    let space = unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as u32) } as usize;
    let mut control = vec![0u8; space];
    let mut iov = libc::iovec {
        iov_base: framed.as_mut_ptr().cast(),
        iov_len: framed.len(),
    };
    // SAFETY: the header points at `iov` and `control`, which outlive the
    // call; the control buffer has CMSG_SPACE for the descriptors written.
    let sent = unsafe {
        let mut header: libc::msghdr = std::mem::zeroed();
        header.msg_iov = &mut iov;
        header.msg_iovlen = 1;
        header.msg_control = control.as_mut_ptr().cast();
        header.msg_controllen = space as _;
        let cmsg = libc::CMSG_FIRSTHDR(&header);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as _;
        std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(cmsg).cast(), fds.len());
        libc::sendmsg(stream.as_raw_fd(), &header, libc::MSG_NOSIGNAL)
    };
    if sent < 0 {
        return Err(io::Error::last_os_error());
    }
    stream.write_all(&framed[sent as usize..])
}

/// One message, with the descriptors that came with its first bytes.
pub(super) fn read_message(stream: &mut UnixStream) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut length = [0u8; 4];
    let mut control = vec![0u8; unsafe { libc::CMSG_SPACE(4 * 8) } as usize];
    let mut iov = libc::iovec {
        iov_base: length.as_mut_ptr().cast(),
        iov_len: length.len(),
    };
    let mut fds = Vec::new();
    // SAFETY: as in write_message; each descriptor received is owned once.
    let got = unsafe {
        let mut header: libc::msghdr = std::mem::zeroed();
        header.msg_iov = &mut iov;
        header.msg_iovlen = 1;
        header.msg_control = control.as_mut_ptr().cast();
        header.msg_controllen = control.len() as _;
        let got = crate::host::receive(stream.as_raw_fd(), &mut header);
        if got > 0 {
            let mut cmsg = libc::CMSG_FIRSTHDR(&header);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                    let n = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize)
                        / std::mem::size_of::<RawFd>();
                    for i in 0..n {
                        fds.push(OwnedFd::from_raw_fd(data.add(i).read_unaligned()));
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&header, cmsg);
            }
        }
        got
    };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    if got == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    stream.read_exact(&mut length[got as usize..])?;
    let mut bytes = vec![0; u32::from_le_bytes(length) as usize];
    stream.read_exact(&mut bytes)?;
    Ok((bytes, fds))
}
