//! The driver's Linux host mechanisms.
use std::{fs::File, io, os::fd::AsRawFd, path::Path};

/// Create `to` as a copy of `from`: a reflink (`FICLONE`) where the file
/// system shares extents, else a byte copy into the same new inode.
pub(crate) fn clone_file(from: &mut File, to: &Path) -> io::Result<()> {
    let mut to = File::create_new(to)?;
    // SAFETY: FICLONE borrows both descriptors for the call.
    if unsafe { libc::ioctl(to.as_raw_fd(), libc::FICLONE, from.as_raw_fd()) } != 0 {
        io::copy(from, &mut to)?;
    }
    Ok(())
}
