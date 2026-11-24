//! File and stream writes report the committed prefix on a failed syscall.
use crate::provider::{DIRECT_ALIGNMENT, IoError, MODE_DIRECT, Outcome, Request, WRITE_AT};

pub(super) fn write_request(fd: i32, request: &Request) -> Outcome {
    let bytes = if request.buffer.len == 0 {
        &[]
    } else {
        // SAFETY: the caller pins the readable input bytes through delivery.
        unsafe { std::slice::from_raw_parts(request.buffer.data, request.buffer.len) }
    };
    let offset = (request.operation == WRITE_AT).then(|| request.offset());
    write_all_from(
        fd,
        bytes,
        offset,
        crate::units::uses_file_ring(),
        request.mode == MODE_DIRECT,
    )
}

pub(crate) fn write_all(fd: i32, bytes: &[u8]) -> Outcome {
    write_all_from(fd, bytes, None, false, false)
}

fn write_all_from(fd: i32, bytes: &[u8], offset: Option<u64>, ring: bool, direct: bool) -> Outcome {
    let mut written = 0;
    let error = loop {
        let remaining = &bytes[written..];
        if remaining.is_empty() {
            break IoError::OK;
        }
        let at = if let Some(at) = offset {
            let Some(at) = at
                .checked_add(written as u64)
                .and_then(|at| i64::try_from(at).ok())
            else {
                break IoError::from_errno(libc::EFBIG);
            };
            Some(at)
        } else {
            None
        };
        match write_part(fd, remaining, at, ring) {
            Ok(0) => break IoError::from_errno(libc::EIO),
            Ok(count) => {
                written += count;
                if direct
                    && written < bytes.len()
                    && !(written as u64).is_multiple_of(DIRECT_ALIGNMENT)
                {
                    break IoError::from_errno(libc::EIO);
                }
            }
            Err(error) => match error.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EAGAIN) if ring || direct => continue,
                errno => break IoError::from_errno(errno.unwrap_or(libc::EIO)),
            },
        }
    };
    Outcome {
        written: written as u64,
        error,
        ..Outcome::default()
    }
}

fn write_part(fd: i32, bytes: &[u8], offset: Option<i64>, ring: bool) -> std::io::Result<usize> {
    if ring {
        let entry = io_uring::opcode::Write::new(
            io_uring::types::Fd(fd),
            bytes.as_ptr(),
            bytes
                .len()
                .min(i32::MAX as usize & !(DIRECT_ALIGNMENT as usize - 1)) as u32,
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
    super::syscall(result).map(|result| result as usize)
}
