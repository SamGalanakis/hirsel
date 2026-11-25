//! File and stream writes report the committed prefix on a failed syscall.
use crate::provider::{DIRECT_ALIGNMENT, IoError, MODE_DIRECT, Outcome, Request, WRITE_AT};
use std::io;

const ZERO_BYTES: usize = 64 << 10;
#[repr(align(4096))]
struct ZeroRegion([u8; ZERO_BYTES]);
static ZEROES: ZeroRegion = ZeroRegion([0; ZERO_BYTES]);

pub(super) fn zero_at(fd: i32, offset: u64, len: u64, mode: u8) -> Outcome {
    let mut out = zero_range(fd, offset, len, mode == MODE_DIRECT);
    // Durable or Direct zeros complete once they are durable, as a write's bytes.
    crate::host::sync::finish_write(fd, mode, &mut out);
    out
}

fn zero_range(fd: i32, offset: u64, len: u64, direct: bool) -> Outcome {
    let ring = crate::units::uses_file_ring();
    let len = usize::try_from(len).unwrap_or(usize::MAX);
    complete(len, Some(offset), ring, direct, |done, at| {
        let count = (len - done).min(16 * ZERO_BYTES);
        let mut iov = [libc::iovec {
            iov_base: ZEROES.0.as_ptr().cast_mut().cast(),
            iov_len: ZERO_BYTES,
        }; 16];
        let used = count.div_ceil(ZERO_BYTES);
        iov[used - 1].iov_len = count - (used - 1) * ZERO_BYTES;
        let at = at.expect("zero_at is positioned");
        crate::host::file::zero_part(fd, &iov[..used], at, ring)
    })
}

pub(super) fn write_request(fd: i32, request: &Request) -> Outcome {
    let bytes = if request.buffer.len == 0 {
        &[]
    } else {
        // SAFETY: the caller pins the readable input bytes through delivery.
        unsafe { std::slice::from_raw_parts(request.buffer.data, request.buffer.len) }
    };
    let offset = (request.operation == WRITE_AT).then(|| request.offset());
    let ring = crate::units::uses_file_ring();
    let part = |bytes: &[u8], at| write_part(fd, bytes, at, ring);
    let mut out = write_all_from(bytes, offset, ring, request.mode == MODE_DIRECT, part);
    // A Durable or Direct write completes once its bytes are durable.
    crate::host::sync::finish_write(fd, request.mode, &mut out);
    out
}

pub(crate) fn write_all(fd: i32, bytes: &[u8]) -> Outcome {
    write_all_from(bytes, None, false, false, |bytes, at| {
        write_part(fd, bytes, at, false)
    })
}

/// Drives one write to completion over `part`, the source of each piece's
/// completion: the ring or the syscall in production, a script in tests.
pub(crate) fn write_all_from(
    bytes: &[u8],
    offset: Option<u64>,
    ring: bool,
    direct: bool,
    mut part: impl FnMut(&[u8], Option<i64>) -> io::Result<usize>,
) -> Outcome {
    complete(bytes.len(), offset, ring, direct, |done, at| {
        part(&bytes[done..], at)
    })
}

/// The one completion loop for `len` bytes: `part(done, at)` issues the
/// remainder after `done` bytes and returns how many it wrote.
fn complete(
    len: usize,
    offset: Option<u64>,
    ring: bool,
    direct: bool,
    mut part: impl FnMut(usize, Option<i64>) -> io::Result<usize>,
) -> Outcome {
    let mut written = 0;
    let error = loop {
        if written == len {
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
        match part(written, at) {
            Ok(0) => break IoError::from_errno(libc::EIO),
            Ok(count) => {
                written += count;
                if direct && written < len && !(written as u64).is_multiple_of(DIRECT_ALIGNMENT) {
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
    crate::host::file::write_part(fd, bytes, offset, ring, DIRECT_ALIGNMENT)
}
