//! File operations on Linux. Inside a unit whose pool owns a file ring, each
//! operation goes through the ring (`crate::units::file_request`); otherwise
//! it is the plain syscall.
use std::{io, os::fd::RawFd};

fn checked(rc: isize) -> io::Result<isize> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

/// Write a prefix of `bytes` at `offset` (the file position when `None`).
/// `ring` routes the write through the unit's file ring, which caps one write
/// at the largest `align`-multiple an i32 holds.
pub(crate) fn write_part(
    fd: RawFd,
    bytes: &[u8],
    offset: Option<i64>,
    ring: bool,
    align: u64,
) -> io::Result<usize> {
    if ring {
        let entry = io_uring::opcode::Write::new(
            io_uring::types::Fd(fd),
            bytes.as_ptr(),
            bytes.len().min(i32::MAX as usize & !(align as usize - 1)) as u32,
        )
        .offset(offset.map_or(u64::MAX, |at| at as u64))
        .build();
        // SAFETY: the provider keeps fd and bytes live until the call's CQE.
        let result = unsafe { crate::units::file_request(entry) }?;
        return Ok(result as usize);
    }
    // SAFETY: fd and bytes remain live through the syscall.
    let result = unsafe {
        let data = bytes.as_ptr().cast();
        match offset {
            Some(at) => libc::pwrite(fd, data, bytes.len(), at),
            None => libc::write(fd, data, bytes.len()),
        }
    };
    checked(result).map(|result| result as usize)
}

/// Close the sole owner of `fd`.
pub(crate) fn close(fd: RawFd) -> io::Result<()> {
    if crate::units::uses_file_ring() {
        let entry = io_uring::opcode::Close::new(io_uring::types::Fd(fd)).build();
        // SAFETY: the entry consumes the sole owned descriptor.
        unsafe { crate::units::file_request(entry) }?;
    } else {
        // SAFETY: consumes the sole owned fd. Linux closes even on EINTR;
        // retrying could close a newly reused descriptor.
        checked(unsafe { libc::close(fd) } as isize)?;
    }
    Ok(())
}

/// Read a prefix of `len` bytes into `data` at `offset` (the file position
/// when `None`). `ring` issues one buffered ring READ (§3.3b), which completes
/// from the page cache or through task_work.
///
/// # Safety
/// `data` is writable for `len` bytes, and the caller holds `fd` and `data`
/// until the call returns.
pub(crate) unsafe fn read_part(
    fd: RawFd,
    data: *mut u8,
    len: usize,
    offset: Option<u64>,
    ring: bool,
) -> io::Result<isize> {
    if ring {
        let entry = io_uring::opcode::Read::new(
            io_uring::types::Fd(fd),
            data,
            len.min(i32::MAX as usize) as u32,
        )
        .offset(offset.unwrap_or(u64::MAX))
        .build();
        // SAFETY: the issuing unit stays parked, with fd and the buffer
        // pinned, until the call's CQE.
        return Ok(unsafe { crate::units::file_request(entry) }? as isize);
    }
    // SAFETY: by the contract; the syscall completes before return.
    checked(unsafe {
        match offset {
            Some(at) => libc::pread(fd, data.cast(), len, at as i64),
            None => libc::read(fd, data.cast(), len),
        }
    })
}

/// One sync of `fd`, never retried (§3.3b). `ring` issues one ring FSYNC from
/// the unit's slot (data-only for a file, as `fdatasync`; full for a
/// directory, as `fsync`); otherwise the host's durable sync.
pub(crate) fn sync(fd: RawFd, directory: bool, ring: bool) -> io::Result<()> {
    if ring {
        let flags = if directory {
            io_uring::types::FsyncFlags::empty()
        } else {
            io_uring::types::FsyncFlags::DATASYNC
        };
        let entry = io_uring::opcode::Fsync::new(io_uring::types::Fd(fd))
            .flags(flags)
            .build();
        // SAFETY: the issuing unit stays parked, with fd held by the
        // provider, until the call's CQE.
        unsafe { crate::units::file_request(entry) }?;
        return Ok(());
    }
    let rc = if directory {
        super::sync::sync_directory(fd)
    } else {
        super::sync::sync_data(fd)
    };
    checked(rc as isize).map(drop)
}

/// Write the vector `iov` at `at`, returning the bytes written.
pub(crate) fn zero_part(fd: RawFd, iov: &[libc::iovec], at: i64, ring: bool) -> io::Result<usize> {
    if ring {
        let entry =
            io_uring::opcode::Writev::new(io_uring::types::Fd(fd), iov.as_ptr(), iov.len() as u32)
                .offset(at as u64)
                .build();
        // SAFETY: the provider holds fd and the caller pins the iovecs and
        // what they name until the call's CQE.
        return unsafe { crate::units::file_request(entry) }.map(|n| n as usize);
    }
    // SAFETY: fd and the initialized vector are live throughout pwritev.
    checked(unsafe { libc::pwritev(fd, iov.as_ptr(), iov.len() as i32, at) }).map(|n| n as usize)
}
