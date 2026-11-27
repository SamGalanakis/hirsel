//! File I/O on Linux. A tied issuer waits on its own file ring. Pool units
//! park while their worker continues to issue and reap other requests.
use hirsel_core_net::{FileRequest, file_ring::FileRing};
use std::cell::Cell;
use std::{io, os::fd::RawFd};

thread_local! {
    static TIED: io::Result<Option<FileRing>> = hirsel_core_net::reactor().and_then(|backend| {
        if backend.completes_files() {
            FileRing::new(32).map(Some)
        } else {
            Ok(None)
        }
    });
}

pub(crate) fn write_ring() -> io::Result<bool> {
    if crate::units::untied() {
        return Ok(crate::units::uses_file_ring());
    }
    TIED.with(|r| r.as_ref().map(Option::is_some).map_err(copy_error))
}

fn copy_error(e: &io::Error) -> io::Error {
    e.raw_os_error().map_or_else(
        || io::Error::new(e.kind(), e.to_string()),
        io::Error::from_raw_os_error,
    )
}

fn with_ring<T>(f: impl FnOnce(&FileRing) -> io::Result<T>) -> io::Result<T> {
    if crate::units::untied() {
        let poller = crate::units::file_poller().ok_or(io::ErrorKind::Unsupported)?;
        // SAFETY: only the running pool unit can obtain its slot poller here.
        return f(unsafe { poller.file_ring() }?);
    }
    TIED.with(|r| {
        f(r.as_ref()
            .map_err(copy_error)?
            .as_ref()
            .ok_or(io::ErrorKind::Unsupported)?)
    })
}

/// Select the writer's pair on the current file issuer. The existing
/// provider write path recognizes slices of either buffer, including suffixes.
pub fn register_file_buffers(buffers: &hirsel_core_net::file_ring::FileBuffers) -> io::Result<()> {
    with_ring(|ring| ring.register_buffers(buffers))
}

unsafe fn request(ring: &FileRing, entry: io_uring::squeue::Entry) -> io::Result<i32> {
    if crate::units::uses_file_ring() {
        // SAFETY: caller holds operands until completion on the unit's slot.
        return unsafe { crate::units::file_request(entry) };
    }
    unsafe fn finish(context: *const (), result: i32) {
        // SAFETY: the issuer retains its stack cell until this callback.
        unsafe {
            (*context.cast::<Cell<Option<i32>>>()).set(Some(result));
        }
    }
    let result = Cell::<Option<i32>>::new(None);
    let pending = FileRequest::new(entry, (&raw const result).cast(), finish);
    // SAFETY: the tied issuer waits on this thread with all operands pinned.
    unsafe { ring.queue(&pending) };
    while result.get().is_none() {
        // An enter failure is not the operation's CQE. Returning would let
        // the caller release memory the kernel may still be using.
        if ring.drive(true).is_err() {
            std::process::abort();
        }
    }
    let result = result.get().unwrap();
    if result < 0 {
        Err(io::Error::from_raw_os_error(-result))
    } else {
        Ok(result)
    }
}

#[cfg(test)]
thread_local! {
    static SYSCALL_WRITES: Cell<u32> = const { Cell::new(0) };
    static FIXED_WRITES: Cell<u32> = const { Cell::new(0) };
    static RING_WRITES: Cell<u32> = const { Cell::new(0) };
}

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
        return with_ring(|issuer| {
            let len = bytes.len().min(i32::MAX as usize & !(align as usize - 1));
            let entry =
                issuer.write_entry(fd, &bytes[..len], offset.map_or(u64::MAX, |at| at as u64));
            #[cfg(test)]
            let fixed = entry.get_opcode() == u32::from(io_uring::opcode::WriteFixed::CODE);
            // SAFETY: the provider pins fd and bytes until the CQE.
            let count = unsafe { request(issuer, entry) }?;
            #[cfg(test)]
            if fixed {
                FIXED_WRITES.set(FIXED_WRITES.get() + 1);
            }
            #[cfg(test)]
            RING_WRITES.set(RING_WRITES.get() + 1);
            Ok(count as usize)
        });
    }
    #[cfg(test)]
    SYSCALL_WRITES.set(SYSCALL_WRITES.get() + 1);
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
    if let Some(poller) = crate::units::file_poller()
        // SAFETY: only the current slot owner has this poller. If setup fails
        // before submission, the caller still owns the fd and closes it below.
        && unsafe { poller.file_ring() }.is_ok()
    {
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
        return with_ring(|issuer| unsafe { request(issuer, entry) }).map(|n| n as usize);
    }
    // SAFETY: fd and the initialized vector are live throughout pwritev.
    checked(unsafe { libc::pwritev(fd, iov.as_ptr(), iov.len() as i32, at) }).map(|n| n as usize)
}

#[cfg(test)]
#[path = "file_tests.rs"]
mod tests;
