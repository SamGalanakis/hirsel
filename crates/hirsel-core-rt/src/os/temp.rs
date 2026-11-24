//! Exclusive temporary names within an already granted directory.
use super::beneath;
use crate::provider::{Buffer, MKSTEMP, Request};
use std::{ffi::CString, io, os::fd::OwnedFd};

pub(crate) const TEMP_ATTEMPTS: usize = 128;

pub(super) fn create(fd: i32, request: &mut Request, sequence: &mut u64) -> io::Result<OwnedFd> {
    let buffer = &mut request.buffer;
    let prefix = if buffer.len == 0 {
        &[]
    } else {
        // SAFETY: issue borrows the carrier's existing initialized bytes.
        unsafe { std::slice::from_raw_parts(buffer.data, buffer.len) }
    };
    // A nonempty suffix also makes dot and dotdot prefixes ordinary names.
    if prefix.contains(&b'/') || prefix.contains(&0) {
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }
    let (opened, suffix) = if request.operation == MKSTEMP {
        create_temp_file(fd, prefix, temp_entropy)?
    } else {
        create_temp_dir(fd, prefix, sequence)?
    };
    append_temp_suffix(buffer, suffix.as_bytes());
    Ok(opened)
}

fn create_temp_dir(fd: i32, prefix: &[u8], sequence: &mut u64) -> io::Result<(OwnedFd, String)> {
    for _ in 0..TEMP_ATTEMPTS {
        let suffix = format!("h{:x}x{:x}", std::process::id(), *sequence);
        *sequence = sequence
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"HostNames"));
        let mut name = prefix.to_vec();
        name.extend_from_slice(suffix.as_bytes());
        let name = CString::new(name)?;
        // SAFETY: the operation uses a single name in the granted directory.
        if unsafe { libc::mkdirat(fd, name.as_ptr(), 0o700) } < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EEXIST) {
                continue;
            }
            return Err(error);
        }
        match beneath(
            fd,
            name.as_bytes(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        ) {
            Ok(opened) => return Ok((opened, suffix)),
            Err(error) => {
                // SAFETY: remove the empty directory just created, without following a link.
                unsafe { libc::unlinkat(fd, name.as_ptr(), libc::AT_REMOVEDIR) };
                return Err(error);
            }
        }
    }
    Err(io::Error::from_raw_os_error(libc::EAGAIN))
}

fn append_temp_suffix(buffer: &mut Buffer, suffix: &[u8]) {
    // SAFETY: entry checked 32 spare bytes, which both generators respect.
    unsafe {
        std::ptr::copy_nonoverlapping(suffix.as_ptr(), buffer.data.add(buffer.len), suffix.len());
    }
    buffer.len += suffix.len();
}

fn temp_entropy() -> io::Result<u128> {
    let mut bytes = [0; 16];
    super::fill_random_bytes(bytes.as_mut_ptr(), bytes.len())?;
    Ok(u128::from_ne_bytes(bytes))
}

pub(crate) fn create_temp_file(
    fd: i32,
    prefix: &[u8],
    mut entropy: impl FnMut() -> io::Result<u128>,
) -> io::Result<(OwnedFd, String)> {
    let mut name = prefix.to_vec();
    for _ in 0..TEMP_ATTEMPTS {
        let suffix = format!("{:032x}", entropy()?);
        name.truncate(prefix.len());
        name.extend_from_slice(suffix.as_bytes());
        match beneath(fd, &name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL) {
            Ok(file) => return Ok((file, suffix)),
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::from_raw_os_error(libc::EAGAIN))
}
