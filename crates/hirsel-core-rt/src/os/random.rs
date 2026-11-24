//! getrandom(2) into a request's spare capacity or a caller's buffer.
use super::Request;
use crate::provider::Outcome;
use std::io;

pub(super) fn request(request: &mut Request, flags: u32) -> io::Result<Outcome> {
    let spare = request.buffer.capacity.saturating_sub(request.buffer.len);
    if spare != 0 {
        // SAFETY: the request pins the writable spare capacity, which may be uninitialized.
        let data = unsafe { request.buffer.data.add(request.buffer.len) };
        fill(data, spare, flags)?;
    }
    request.buffer.len += spare;
    Ok(Outcome {
        written: spare as u64,
        ..Outcome::default()
    })
}

pub(super) fn fill(mut data: *mut u8, mut remaining: usize, flags: u32) -> io::Result<()> {
    while remaining != 0 {
        // SAFETY: both callers provide a live writable range of the given length.
        let n = super::syscall(unsafe { libc::getrandom(data.cast(), remaining, flags) } as isize)?
            as usize;
        if n == 0 {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        data = data.wrapping_add(n);
        remaining -= n;
    }
    Ok(())
}
