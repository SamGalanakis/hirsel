//! File operations on Darwin: plain syscalls; there is no file ring.
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
};

pub(crate) fn allocate_buffer(
    _: &crate::Context,
    _: Option<usize>,
    _: usize,
) -> Option<std::ptr::NonNull<u8>> {
    None
}

fn checked(rc: isize) -> io::Result<isize> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

/// The longest write a pipe or a stream socket takes whole or not at all:
/// `PIPE_BUF`, which is below a stream socket's low-water mark.
const WHOLE: usize = libc::PIPE_BUF;

/// Write a prefix of `bytes` at `offset` (the file position when `None`).
/// No unit owns a file ring on Darwin, so `ring` is always false.
///
/// Darwin returns no count for a write that it took part of and that then
/// failed: a writer that waits for room with a part taken gets a bare EPIPE
/// when the reader goes away. So a write to a pipe or a stream socket never
/// waits with a part taken: it waits for room first and offers no more than
/// the room there is, which the kernel takes whole.
pub(crate) fn write_part(
    fd: RawFd,
    bytes: &[u8],
    offset: Option<i64>,
    _ring: bool,
    _align: u64,
) -> io::Result<usize> {
    let mut len = bytes.len();
    if offset.is_none()
        && len > WHOLE
        && let Some(room) = room(fd)?
    {
        len = room.clamp(1, len);
    }
    // SAFETY: fd and bytes remain live through the syscall.
    let result = unsafe {
        let data = bytes.as_ptr().cast();
        match offset {
            Some(at) => libc::pwrite(fd, data, len, at),
            None => libc::write(fd, data, len),
        }
    };
    checked(result).map(|result| result as usize)
}

/// Wait until `fd` has room for a write and return that room in bytes;
/// `None` when `fd` is neither a pipe nor a stream socket, or the kernel
/// does not report its room.
fn room(fd: RawFd) -> io::Result<Option<usize>> {
    // SAFETY: an all-zero stat is a valid output slot.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: one writable stat record.
    if unsafe { libc::fstat(fd, &mut stat) } < 0 {
        return Ok(None);
    }
    match stat.st_mode & libc::S_IFMT {
        libc::S_IFIFO => {}
        libc::S_IFSOCK => {
            // A datagram is one write, never a prefix.
            let mut kind: libc::c_int = 0;
            let mut size = size_of::<libc::c_int>() as libc::socklen_t;
            // SAFETY: one writable int and its length.
            let rc = unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_TYPE,
                    (&raw mut kind).cast(),
                    &mut size,
                )
            };
            if rc < 0 || kind != libc::SOCK_STREAM {
                return Ok(None);
            }
        }
        _ => return Ok(None),
    }
    // SAFETY: success returns one fresh descriptor.
    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        return Ok(None);
    }
    // SAFETY: a fresh descriptor, closed on return.
    let kq = unsafe { OwnedFd::from_raw_fd(kq) };
    let change = libc::kevent {
        ident: fd as usize,
        filter: libc::EVFILT_WRITE,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: an all-zero kevent is a valid output slot.
    let mut fired: libc::kevent = unsafe { std::mem::zeroed() };
    // SAFETY: one change record and one writable output record; wait without a timeout.
    let n = unsafe { libc::kevent(kq.as_raw_fd(), &change, 1, &mut fired, 1, std::ptr::null()) };
    if n < 0 {
        let error = io::Error::last_os_error();
        // The caller retries an interrupted wait; any other failure leaves the plain write.
        return if error.kind() == io::ErrorKind::Interrupted {
            Err(error)
        } else {
            Ok(None)
        };
    }
    if n == 0 || fired.flags & libc::EV_ERROR != 0 {
        return Ok(None);
    }
    Ok(Some(usize::try_from(fired.data).unwrap_or(0)))
}

/// Close the sole owner of `fd`. Darwin's `close` releases the descriptor
/// even when interrupted; retrying could close a reused one.
pub(crate) fn close(fd: RawFd) -> io::Result<()> {
    // SAFETY: consumes the sole owned fd.
    checked(unsafe { libc::close(fd) } as isize)?;
    Ok(())
}

/// Read a prefix of `len` bytes into `data` at `offset` (the file position
/// when `None`). There is no file ring on Darwin, so `ring` is always false.
///
/// # Safety
/// `data` is writable for `len` bytes, and the caller holds `fd` and `data`
/// until the call returns.
pub(crate) unsafe fn read_part(
    fd: RawFd,
    data: *mut u8,
    len: usize,
    offset: Option<u64>,
    _ring: bool,
) -> io::Result<isize> {
    // SAFETY: by the contract; the syscall completes before return.
    checked(unsafe {
        match offset {
            Some(at) => libc::pread(fd, data.cast(), len, at as i64),
            None => libc::read(fd, data.cast(), len),
        }
    })
}

/// One sync of `fd`, never retried: `F_FULLFSYNC` for files and directories
/// alike (`host::sync`).
pub(crate) fn sync(fd: RawFd, directory: bool, _ring: bool) -> io::Result<()> {
    let rc = if directory {
        super::sync::sync_directory(fd)
    } else {
        super::sync::sync_data(fd)
    };
    checked(rc as isize).map(drop)
}

/// Write the vector `iov` at `at`, returning the bytes written.
pub(crate) fn zero_part(fd: RawFd, iov: &[libc::iovec], at: i64, _ring: bool) -> io::Result<usize> {
    // SAFETY: fd and the initialized vector are live throughout pwritev.
    checked(unsafe { libc::pwritev(fd, iov.as_ptr(), iov.len() as i32, at) }).map(|n| n as usize)
}
