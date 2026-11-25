//! The driver's Darwin host mechanisms.
use std::{
    ffi::CString,
    fs::File,
    io,
    os::{fd::AsRawFd, unix::ffi::OsStrExt},
    path::Path,
};

/// Create `to` as a copy of `from`: an APFS clone (`fclonefileat`), else a
/// byte copy into a new file, as Linux does without reflinks.
pub(crate) fn clone_file(from: &mut File, to: &Path) -> io::Result<()> {
    let (Some(parent), Some(name)) = (to.parent(), to.file_name()) else {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    };
    let directory = File::open(parent)?;
    let name = CString::new(name.as_bytes())?;
    // SAFETY: both descriptors and the name stay live for the call; the clone
    // creates `name` and refuses one that exists.
    let rc =
        unsafe { libc::fclonefileat(from.as_raw_fd(), directory.as_raw_fd(), name.as_ptr(), 0) };
    if rc == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EEXIST) {
        return Err(error);
    }
    let mut to = File::create_new(to)?;
    io::copy(from, &mut to)?;
    Ok(())
}
